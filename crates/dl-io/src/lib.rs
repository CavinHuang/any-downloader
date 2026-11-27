//! 单写者线程（DESIGN §5.3）。
//!
//! 所有落盘经由一个专用 OS 线程顺序定位写，使用有界异步通道（tokio::sync::mpsc）
//! 提供背压，防止高速下载时内存无限膨胀。
//!
//! 检查点顺序（崩溃安全不变量「journal ⊆ 已落盘数据」）：
//! `sync_data(数据文件) → 覆盖写 journal → sync_data(journal)`。

#[cfg(unix)]
use std::io;
#[cfg(not(unix))]
use std::io::{self, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot, watch};

use dl_journal::{Data as JournalData, Entry as JournalEntry, Journal};
use dl_segment::Segment;

/// 有界写队列深度（256 * ~64KB..256KB ≈ 16–64MB 内存上限）。
const WRITER_QUEUE_CAP: usize = 256;
/// 数据文件强制落盘周期：时间或字节任一达到即检查点。
const CHECKPOINT_SECS: u64 = 5;
const CHECKPOINT_BYTES: u64 = 64 * 1024 * 1024;
/// 进度快照推送节流间隔（避免每个 chunk 都 clone 整个 Vec<Segment>）。
const SNAP_INTERVAL: Duration = Duration::from_millis(80);
/// 超过此大小的文件跳过 set_len 预分配（exFAT 等不支持稀疏的盘会真实分配并清零）。
const PREALLOC_LIMIT: u64 = 64 * 1024 * 1024 * 1024;

pub enum Msg {
    /// 定位写一块。offset 为绝对文件偏移；同段内严格递增（worker 顺序流式）。
    Chunk { offset: u64, data: Vec<u8> },
    /// 动态分段窃取：把 end == old_end 的活跃段在 split_at 处缩段，
    /// 拆分为 [start, split_at) 与 [split_at, old_end)。必须先于新段的 Chunk 入队。
    SplitAt { old_end: u64, split_at: u64 },
    /// Sequential 模式：响应体正常结束。
    Eof,
    /// 立即检查点（暂停/取消路径）。
    Flush,
    /// 最终检查点 + 关闭。ack 返回最终分段表。
    Finish {
        ack: oneshot::Sender<io::Result<Vec<Segment>>>,
    },
}

#[derive(Debug, Clone, Copy)]
pub enum Mode {
    Segmented { total: u64, validator_hash: u64 },
    Sequential,
}

pub struct SpawnParams {
    pub path: PathBuf,
    pub mode: Mode,
    pub segments: Vec<Segment>,
    pub journal: Option<Journal>,
}

/// 进度快照：writer 线程的分段表实时镜像（UI 分段进度条数据源）。
#[derive(Debug, Clone, Default)]
pub struct WriterProgress {
    pub downloaded: u64,
    /// Segmented 模式为全部分段；Sequential 模式为空。
    pub segs: Vec<Segment>,
}

#[derive(Clone)]
pub struct Writer {
    tx: mpsc::Sender<Msg>,
    progress: watch::Receiver<WriterProgress>,
}

impl Writer {
    /// 进度快照（writer 线程推进）。
    pub fn progress(&self) -> watch::Receiver<WriterProgress> {
        self.progress.clone()
    }

    pub fn tx(&self) -> mpsc::Sender<Msg> {
        self.tx.clone()
    }

    pub async fn send(&self, msg: Msg) -> io::Result<()> {
        self.tx
            .send(msg)
            .await
            .map_err(|_| io::Error::other("writer thread died"))
    }

    /// 最终检查点 + 关闭，返回最终分段表。
    pub async fn finish(self) -> io::Result<Vec<Segment>> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.send(Msg::Finish { ack: ack_tx }).await?;
        match ack_rx.await {
            Ok(r) => r,
            Err(_) => Err(io::Error::other("writer thread died before finish")),
        }
    }
}

/// 启动写线程。返回 (writer, all_written)；
/// all_written 在「全部段写满 / Eof」或「致命 IO 错误」时恰好触发一次。
pub async fn spawn(
    params: SpawnParams,
) -> io::Result<(Writer, oneshot::Receiver<io::Result<Vec<Segment>>>)> {
    let (ready_tx, ready_rx) = oneshot::channel();
    let (all_tx, all_rx) = oneshot::channel();
    let (tx, rx) = mpsc::channel::<Msg>(WRITER_QUEUE_CAP);
    let (progress_tx, progress_rx) = watch::channel(WriterProgress::default());

    std::thread::Builder::new()
        .name("dl-writer".into())
        .spawn(move || {
            let result = writer_loop(params, rx, progress_tx, all_tx, ready_tx);
            if let Err(e) = result {
                tracing::warn!(error = %e, "writer thread exited with error");
            }
        })
        .map_err(|e| io::Error::other(format!("spawn writer: {e}")))?;

    match ready_rx.await {
        Ok(Ok(())) => Ok((
            Writer {
                tx,
                progress: progress_rx,
            },
            all_rx,
        )),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(io::Error::other("writer thread died during setup")),
    }
}

fn writer_loop(
    params: SpawnParams,
    mut rx: mpsc::Receiver<Msg>,
    progress_tx: watch::Sender<WriterProgress>,
    all_tx: oneshot::Sender<io::Result<Vec<Segment>>>,
    ready_tx: oneshot::Sender<io::Result<()>>,
) -> io::Result<()> {
    let setup = (|| -> io::Result<(std::fs::File, Option<u64>, Option<u64>)> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&params.path)?;
        let (total, validator_hash) = match params.mode {
            Mode::Segmented {
                total,
                validator_hash,
            } => {
                if total > 0 && total <= PREALLOC_LIMIT {
                    file.set_len(total)?;
                }
                (Some(total), Some(validator_hash))
            }
            Mode::Sequential => (None, None),
        };
        Ok((file, total, validator_hash))
    })();

    let (mut file, total, validator_hash) = match setup {
        Ok(v) => v,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return Ok(());
        }
    };
    let _ = ready_tx.send(Ok(()));

    let mut journal = params.journal;
    let mut segs = params.segments;
    let mut all_fired = false;
    let mut all_tx = Some(all_tx);
    let mut downloaded: u64 = segs.iter().map(|s| s.written).sum();
    let sequential = matches!(params.mode, Mode::Sequential);
    let mut last_snap = Instant::now();
    let send_snap = |downloaded: u64, segs: &[Segment], last_snap: &mut Instant, force: bool| {
        if force || last_snap.elapsed() >= SNAP_INTERVAL {
            let _ = progress_tx.send(WriterProgress {
                downloaded,
                segs: if sequential {
                    Vec::new()
                } else {
                    segs.to_vec()
                },
            });
            *last_snap = Instant::now();
        }
    };
    send_snap(downloaded, &segs, &mut last_snap, true);

    let mut last_ckpt = Instant::now();
    let mut bytes_since_ckpt: u64 = 0;
    #[cfg(not(unix))]
    let mut last_pos: Option<u64> = None;

    macro_rules! checkpoint {
        () => {
            if let Some(j) = journal.as_mut() {
                if let Err(e) = do_checkpoint(&mut file, j, &segs, total, validator_hash) {
                    tracing::warn!(error = %e, "journal checkpoint failed");
                }
            }
        };
    }

    loop {
        if !all_fired && total.is_some() && segs.iter().all(|s| s.done()) {
            all_fired = true;
            send_snap(downloaded, &segs, &mut last_snap, true);
            if let Some(tx) = all_tx.take() {
                let _ = tx.send(Ok(segs.clone()));
            }
        }

        if last_ckpt.elapsed() >= Duration::from_secs(CHECKPOINT_SECS)
            || bytes_since_ckpt >= CHECKPOINT_BYTES
        {
            checkpoint!();
            last_ckpt = Instant::now();
            bytes_since_ckpt = 0;
        }

        let msg = match rx.blocking_recv() {
            Some(m) => m,
            None => {
                checkpoint!();
                break;
            }
        };

        match msg {
            Msg::Chunk { offset, data } => {
                let Some(idx) = segs
                    .iter()
                    .position(|s| offset >= s.start && offset < s.end)
                else {
                    continue;
                };
                // 只接受与段连续光标精确匹配的顺序块；跳过窃取/缩段产生的陈旧重叠块
                let seg_cursor = segs[idx].cursor();
                if offset != seg_cursor {
                    continue;
                }
                let take = data.len().min((segs[idx].end - offset) as usize);
                if take == 0 {
                    continue;
                }
                #[cfg(unix)]
                let write_res = file.write_all_at(&data[..take], offset);
                #[cfg(not(unix))]
                let write_res = {
                    let seek_res = if last_pos != Some(offset) {
                        file.seek(SeekFrom::Start(offset)).map(|_| ())
                    } else {
                        Ok(())
                    };
                    let res = seek_res.and_then(|_| file.write_all(&data[..take]));
                    if res.is_ok() {
                        last_pos = Some(offset + take as u64);
                    } else {
                        last_pos = None;
                    }
                    res
                };
                if let Err(e) = write_res {
                    if let Some(tx) = all_tx.take() {
                        let _ = tx.send(Err(e));
                    }
                    break;
                }
                let seg = &mut segs[idx];
                seg.written += take as u64;
                downloaded += take as u64;
                bytes_since_ckpt += take as u64;
                let seg_done = seg.done();
                send_snap(downloaded, &segs, &mut last_snap, seg_done);
            }
            Msg::SplitAt { old_end, split_at } => {
                if let Some(idx) = segs
                    .iter()
                    .position(|s| s.end == old_end && split_at > s.start && split_at < s.end)
                {
                    let old_cursor = segs[idx].cursor();
                    if old_cursor > split_at {
                        // 原 worker 的最后一块已越过 split_at：回退多计的字节，新段将从 split_at 覆盖写入
                        let excess = old_cursor - split_at;
                        downloaded = downloaded.saturating_sub(excess);
                        segs[idx].written = split_at - segs[idx].start;
                    }
                    segs[idx].end = split_at;
                    segs.push(Segment {
                        start: split_at,
                        end: old_end,
                        written: 0,
                    });
                    send_snap(downloaded, &segs, &mut last_snap, true);
                }
            }
            Msg::Eof => {
                send_snap(downloaded, &segs, &mut last_snap, true);
                if let Some(tx) = all_tx.take() {
                    let _ = tx.send(Ok(segs.clone()));
                }
            }
            Msg::Flush => {
                checkpoint!();
            }
            Msg::Finish { ack } => {
                let r = (|| -> io::Result<Vec<Segment>> {
                    if let Some(j) = journal.as_mut() {
                        do_checkpoint(&mut file, j, &segs, total, validator_hash)?;
                    } else {
                        file.sync_data()?;
                    }
                    Ok(segs.clone())
                })();
                let _ = ack.send(r);
                return Ok(());
            }
        }
    }
    Ok(())
}

fn do_checkpoint(
    file: &mut std::fs::File,
    journal: &mut Journal,
    segs: &[Segment],
    total: Option<u64>,
    validator_hash: Option<u64>,
) -> io::Result<()> {
    file.sync_data()?;
    let data = JournalData {
        total_len: total.unwrap_or(0),
        validator_hash: validator_hash.unwrap_or(0),
        entries: segs
            .iter()
            .map(|s| JournalEntry {
                start: s.start,
                end: s.end,
                written: s.written,
            })
            .collect(),
    };
    journal.checkpoint(&data)
}
