//! 单写者线程（DESIGN §5.3）。
//!
//! 所有落盘经由一个专用 OS 线程顺序定位写。为什么不做多 worker 并发定位写：
//! Windows 的 `seek_write` 移动的是每 handle 共享的文件指针（官方文档明示），并发写
//! 同一 handle 会互相覆盖偏移——静默损坏；且内核对缓冲写按 inode 串行化，并发定位写
//! 本无并行收益。单写者还让 journal 推进天然串行化。
//!
//! 检查点顺序（崩溃安全不变量「journal ⊆ 已落盘数据」）：
//! `sync_data(数据文件) → 覆盖写 journal → sync_data(journal)`。
//! 绝不分块 fsync（机械盘 fsync 上限 ~200 次/s 会打死吞吐）；数据 sync 周期 2s/256MB。

use std::io::{self, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use tokio::sync::{oneshot, watch};

use dl_journal::{Data as JournalData, Entry as JournalEntry, Journal};
use dl_segment::Segment;

/// 数据文件强制落盘周期：时间或字节任一达到即检查点。
const CHECKPOINT_SECS: u64 = 2;
const CHECKPOINT_BYTES: u64 = 256 * 1024 * 1024;
/// 超过此大小的文件跳过 set_len 预分配（exFAT 等不支持稀疏的盘会真实分配并清零）。
const PREALLOC_LIMIT: u64 = 64 * 1024 * 1024 * 1024;

pub enum Msg {
    /// 定位写一块。offset 为绝对文件偏移；同段内严格递增（worker 顺序流式）。
    Chunk { offset: u64, data: Vec<u8> },
    /// 慢段分裂：包含 at 的段在其 at 处被中止，剩余部分切为
    /// [at, mid) 与 [mid, old_end)。必须先于任何新段的 Chunk 入队（mpsc 全序保证）。
    /// 段以「包含 at」定位而非起点匹配：续传恢复后 claimed 区间起点 ≠ journal 段起点。
    Split { old_end: u64, at: u64, mid: u64 },
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
    tx: Sender<Msg>,
    progress: watch::Receiver<WriterProgress>,
}

impl Writer {
    /// 进度快照（writer 线程推进）。
    pub fn progress(&self) -> watch::Receiver<WriterProgress> {
        self.progress.clone()
    }

    pub fn tx(&self) -> Sender<Msg> {
        self.tx.clone()
    }

    pub async fn send(&self, msg: Msg) -> io::Result<()> {
        self.tx
            .send(msg)
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
    let (tx, rx) = std::sync::mpsc::channel::<Msg>();
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
    rx: Receiver<Msg>,
    progress_tx: watch::Sender<WriterProgress>,
    all_tx: oneshot::Sender<io::Result<Vec<Segment>>>,
    ready_tx: oneshot::Sender<io::Result<()>>,
) -> io::Result<()> {
    // 初始化
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
                // exFAT 等非稀疏盘上 set_len 会真实分配；超大文件跳过预分配。
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
    let send_snap = |downloaded: u64, segs: &[Segment]| {
        let _ = progress_tx.send(WriterProgress {
            downloaded,
            // Sequential 模式的伪段（0..u64::MAX）不外发
            segs: if sequential {
                Vec::new()
            } else {
                segs.to_vec()
            },
        });
    };
    send_snap(downloaded, &segs);

    let mut last_ckpt = Instant::now();
    let mut bytes_since_ckpt: u64 = 0;

    macro_rules! checkpoint {
        () => {
            if let Some(j) = journal.as_mut() {
                if let Err(e) = do_checkpoint(&mut file, j, &segs, total, validator_hash) {
                    // journal 写失败不致命：保守下界保持旧值，下次检查点重试
                    tracing::warn!(error = %e, "journal checkpoint failed");
                }
            }
        };
    }

    loop {
        // Segmented 全部写满 → 触发完成（只一次），随后仍等 Finish 做最终检查点
        if !all_fired && total.is_some() && segs.iter().all(|s| s.done()) {
            all_fired = true;
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

        let msg = match rx.recv() {
            Ok(m) => m,
            Err(_) => {
                // sender 全部关闭且无 Finish：尽力做一次检查点
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
                    // 被分裂替换掉的旧段残余块：内容与续传重写结果一致，忽略
                    continue;
                };
                if let Err(e) = file
                    .seek(SeekFrom::Start(offset))
                    .and_then(|_| file.write_all(&data))
                {
                    if let Some(tx) = all_tx.take() {
                        let _ = tx.send(Err(e));
                    }
                    break;
                }
                let seg = &mut segs[idx];
                if offset == seg.cursor() {
                    seg.written += data.len() as u64;
                    downloaded += data.len() as u64;
                    bytes_since_ckpt += data.len() as u64;
                    send_snap(downloaded, &segs);
                }
                // 非连续块（理论上不发生）：数据已写入但位图不推进；续传会重写该区段，安全。
            }
            Msg::Split { old_end, at, mid } => {
                if let Some(idx) = segs.iter().position(|s| at >= s.start && at < s.end) {
                    let old_start = segs[idx].start;
                    segs[idx] = Segment {
                        start: old_start,
                        end: at,
                        written: at - old_start,
                    };
                    segs.push(Segment {
                        start: at,
                        end: mid,
                        written: 0,
                    });
                    segs.push(Segment {
                        start: mid,
                        end: old_end,
                        written: 0,
                    });
                    send_snap(downloaded, &segs);
                }
            }
            Msg::Eof => {
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
    // 顺序即崩溃安全：先让数据落盘，journal 才允许推进。
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
