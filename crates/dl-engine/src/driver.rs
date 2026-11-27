//! HTTP 直链下载驱动（DESIGN §5.3/§5.5/§5.6）。
//!
//! 核心机制（参照 MisterDownloadManager / Ghost-Downloader-3）：
//! - 每个 worker 持有独立 HTTP/1.1 TCP 连接池（避免 HTTP/2 单连接多路复用瓶颈）；
//! - 零断连动态分段窃取（SegmentPlanner + AtomicU64 stop_at）：空闲 worker 从最大
//!   活跃段切走后半段，victim 仅原子收紧 stop_at，不中断连接即可无缝让出区间；
//! - 单写者线程 + 有界异步背压 + 强制完整性校验。

use std::collections::{HashMap, VecDeque};
use std::io::ErrorKind as IoErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, Notify};
use tokio_util::sync::CancellationToken;

use crate::engine::{Engine, EngineEvent};
use dl_error::{DownloadError, ErrorKind, Res};
use dl_filename::{pick_name, resolve_conflict, ConflictPolicy};
use dl_io::{Mode, Msg, SpawnParams};
use dl_journal::{self, Journal};
use dl_model::{Task, TaskStatus};
use dl_net::probe::{self, ProbeResult};
use dl_net::{
    build_client, build_pooled_worker_client, classify_status, identity_headers, map_reqwest_err,
    retry_after_secs,
};
use dl_segment::{self, Segment};
use dl_throttle::Limiter;

pub const MAX_SEGMENTS: usize = 32;
/// Planner 运行期内动态拆分分段上限（留足安全余量，严守 dl_journal::MAX_SEGMENTS = 168）。
const MAX_PLANNER_SEGMENTS: usize = 128;
/// 每段连续无进展失败上限；收到有效字节即重置为 0。
const MAX_ATTEMPTS: u32 = 8;
const STALL_TIMEOUT: Duration = Duration::from_secs(15);
const BACKOFF_MAX: Duration = Duration::from_secs(20);
/// 空闲窃取半段最小阈值（256 KiB，精细切分尾部剩余区间）。
const STEAL_MIN_HALF: u64 = 256 * 1024;
/// 尾部慢连接停滞抢占阈值：单个活跃段超过此时长无新增落盘字节时，由空闲连接接管剩余区间。
const STRAGGLER_STALL: Duration = Duration::from_secs(4);
/// 每连接写入聚合块上限（256 KiB，参照 MDM Writer BATCH，大幅减少异步通道与落盘调用开销）。
const WRITE_BATCH: usize = 256 * 1024;
/// 聚合块最大滞留刷新时间，保证低速连接也能及时更新进度与落盘。
const BATCH_FLUSH_EVERY: Duration = Duration::from_millis(250);
/// MDM 慢连接淘汰（shed_slow）参数。
const BEST_AFTER: Duration = Duration::from_millis(100);
const LINK_WINDOW: Duration = Duration::from_secs(1);
const SHED_GRACE: Duration = Duration::from_secs(2);
const SHED_RATIO: u64 = 4;
const SHED_LEFT: Duration = Duration::from_secs(2);
const PREEMPT_MIN: Duration = Duration::from_millis(200);
/// 无强 ETag/Last-Modified 时断点续传的重叠校验字节数（4 KiB，参照 MDM verify_overlap）。
const RESUME_OVERLAP_BYTES: u64 = 4096;

#[derive(Debug, Clone)]
pub struct DownloadOptions {
    pub dir: PathBuf,
    pub segments: usize,
    pub task_bps: Option<u64>,
    pub global_bps: Option<u64>,
    pub proxy: Option<String>,
    pub conflict: ConflictPolicy,
    pub spot_checks: usize,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        DownloadOptions {
            dir: PathBuf::from("."),
            segments: 8,
            task_bps: None,
            global_bps: None,
            proxy: None,
            conflict: ConflictPolicy::Rename,
            spot_checks: 2,
        }
    }
}

pub(crate) fn drive_from(e: DownloadError) -> DriveErr {
    DriveErr::Fatal(e.kind, e.message)
}

#[derive(Debug)]
pub(crate) enum DriveErr {
    Fatal(ErrorKind, String),
    Paused,
    /// 服务器内容在下载中途变化（If-Range 失配）：清掉 journal/半成品整体重下。
    Restart,
}

/// 任务运行上下文（owned，worker 线程可持有）。
#[derive(Clone)]
pub(crate) struct Ctx {
    pub engine: Engine,
    pub opts: DownloadOptions,
    pub client: reqwest::Client,
    pub limiter: Limiter,
    pub cancel: CancellationToken,
    pub task_id: String,
    pub url: String,
    /// 任务透传头（已过白名单，键小写）。
    pub task_headers: HashMap<String, String>,
    /// 完成后哈希校验（已规范化）。
    pub checksum: Option<(String, String)>,
    /// 下载后处理（视频号解密等；完成时按序执行）。
    pub postprocess: Vec<dl_model::PostProcess>,
}

/// 把任务透传头附加到请求（键小写、仅白名单，DESIGN §9）。
pub(crate) fn with_task_headers(
    req: reqwest::RequestBuilder,
    headers: &HashMap<String, String>,
) -> reqwest::RequestBuilder {
    let mut req = req;
    for (k, v) in headers {
        if dl_model::HEADER_ALLOWLIST.contains(&k.as_str()) {
            req = req.header(k, v);
        }
    }
    req
}

fn parse_task_headers(task: &dl_model::Task) -> HashMap<String, String> {
    task.headers_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<HashMap<String, String>>(s).ok())
        .map(|m| {
            m.into_iter()
                .map(|(k, v)| (k.to_ascii_lowercase(), v))
                .collect()
        })
        .unwrap_or_default()
}

struct ValidatorState {
    etag: Option<String>,
    last_modified: Option<String>,
}

impl ValidatorState {
    fn if_range(&self) -> Option<String> {
        self.etag.clone().or_else(|| self.last_modified.clone())
    }
}

#[derive(Default)]
struct LinkMeter {
    since: Option<(Instant, u64)>,
    best: u64,
}

struct Shared {
    no_range_fallback: AtomicBool,
    fatal: Mutex<Option<ErrorKind>>,
    task_downloaded: AtomicU64,
    aggregate_bps: AtomicU64,
    active_workers: AtomicUsize,
    live_workers: AtomicUsize,
    workers_done: Notify,
    mirror_cursor: AtomicU64,
    bad_mirrors: Mutex<std::collections::HashSet<usize>>,
    mirror_speeds: Mutex<Vec<u64>>,
    mirror_failures: Mutex<Vec<u32>>,
    validators: Mutex<ValidatorState>,
    best_speed: AtomicU64,
    link_meter: Mutex<LinkMeter>,
    received_bytes: AtomicU64,
}

impl Shared {
    fn new(etag: Option<String>, last_modified: Option<String>) -> Self {
        Shared {
            no_range_fallback: AtomicBool::new(false),
            fatal: Mutex::new(None),
            task_downloaded: AtomicU64::new(0),
            aggregate_bps: AtomicU64::new(0),
            active_workers: AtomicUsize::new(0),
            live_workers: AtomicUsize::new(0),
            workers_done: Notify::new(),
            mirror_cursor: AtomicU64::new(0),
            bad_mirrors: Mutex::new(std::collections::HashSet::new()),
            mirror_speeds: Mutex::new(Vec::new()),
            mirror_failures: Mutex::new(Vec::new()),
            validators: Mutex::new(ValidatorState {
                etag,
                last_modified,
            }),
            best_speed: AtomicU64::new(0),
            link_meter: Mutex::new(LinkMeter::default()),
            received_bytes: AtomicU64::new(0),
        }
    }

    fn has_fatal(&self) -> bool {
        self.fatal.lock().unwrap().is_some()
    }

    fn record_fatal(&self, kind: ErrorKind) {
        let mut f = self.fatal.lock().unwrap();
        if f.is_none() {
            *f = Some(kind);
        }
    }

    fn take_fatal(&self) -> Option<ErrorKind> {
        self.fatal.lock().unwrap().take()
    }
}

struct ActiveSeg {
    start: u64,
    stop_at: Arc<AtomicU64>,
    cursor: Arc<AtomicU64>,
    flushed_cursor: Arc<AtomicU64>,
    receiving: Arc<AtomicBool>,
    preempted: Arc<AtomicBool>,
    assigned_at: Instant,
    last_progress_ms: Arc<AtomicU64>,
}

struct PlannerInner {
    pending: VecDeque<Segment>,
    active: HashMap<u64, ActiveSeg>,
    next_id: u64,
    total_segments: usize,
}

/// 零断连动态分段规划器（MDM / Ghost-Downloader-3 模式 + 尾部停滞抢占）。
struct SegmentPlanner {
    inner: Mutex<PlannerInner>,
    notify: Notify,
    started_at: Instant,
}

enum WorkItem {
    Assigned {
        id: u64,
        seg: Segment,
        stop_at: Arc<AtomicU64>,
        cursor: Arc<AtomicU64>,
        flushed_cursor: Arc<AtomicU64>,
        receiving: Arc<AtomicBool>,
        preempted: Arc<AtomicBool>,
        last_progress_ms: Arc<AtomicU64>,
        split: Option<(u64, u64)>,
    },
    Wait,
    Done,
}

impl SegmentPlanner {
    fn new(ranges: Vec<Segment>, total_segments: usize) -> Self {
        SegmentPlanner {
            inner: Mutex::new(PlannerInner {
                pending: VecDeque::from(ranges),
                active: HashMap::new(),
                next_id: 1,
                total_segments,
            }),
            notify: Notify::new(),
            started_at: Instant::now(),
        }
    }

    fn now_ms(&self) -> u64 {
        self.started_at.elapsed().as_millis() as u64
    }

    fn receiving_count(&self) -> usize {
        self.inner
            .lock()
            .unwrap()
            .active
            .values()
            .filter(|a| a.receiving.load(Ordering::Acquire))
            .count()
    }

    fn active_count(&self) -> usize {
        self.inner.lock().unwrap().active.len()
    }

    fn has_splittable(&self, min_half: u64) -> bool {
        let g = self.inner.lock().unwrap();
        if g.total_segments >= MAX_PLANNER_SEGMENTS {
            return false;
        }
        g.active.values().any(|a| {
            let end = a.stop_at.load(Ordering::Acquire);
            let pos = a.cursor.load(Ordering::Acquire);
            end.saturating_sub(pos) >= 2 * min_half
        })
    }

    fn next_work(&self, after: Option<u64>, min_half: u64, preempt_after: Duration) -> WorkItem {
        let now_ms = self.now_ms();
        let mut g = self.inner.lock().unwrap();
        // 1. 优先取与刚完成分段紧邻的待处理分段，其次 FIFO
        let pending_idx = after
            .and_then(|a| g.pending.iter().position(|s| s.start == a))
            .or_else(|| (!g.pending.is_empty()).then_some(0));
        if let Some(idx) = pending_idx {
            let seg = g.pending.remove(idx).unwrap();
            let id = g.next_id;
            g.next_id += 1;
            let stop_at = Arc::new(AtomicU64::new(seg.end));
            let cursor = Arc::new(AtomicU64::new(seg.start));
            let flushed_cursor = Arc::new(AtomicU64::new(seg.start));
            let receiving = Arc::new(AtomicBool::new(false));
            let preempted = Arc::new(AtomicBool::new(false));
            let last_progress_ms = Arc::new(AtomicU64::new(now_ms));
            g.active.insert(
                id,
                ActiveSeg {
                    start: seg.start,
                    stop_at: stop_at.clone(),
                    cursor: cursor.clone(),
                    flushed_cursor: flushed_cursor.clone(),
                    receiving: receiving.clone(),
                    preempted: preempted.clone(),
                    assigned_at: Instant::now(),
                    last_progress_ms: last_progress_ms.clone(),
                },
            );
            return WorkItem::Assigned {
                id,
                seg,
                stop_at,
                cursor,
                flushed_cursor,
                receiving,
                preempted,
                last_progress_ms,
                split: None,
            };
        }

        // 2. MDM 抢占规则：下一段的连接建连过久仍未收到首字节，由空闲连接接管
        if let Some(a) = after {
            let victim_id = g
                .active
                .iter()
                .find(|(_, s)| {
                    s.start == a
                        && !s.receiving.load(Ordering::Acquire)
                        && s.assigned_at.elapsed() >= preempt_after
                        && s.stop_at.load(Ordering::Acquire) > a
                })
                .map(|(id, _)| *id);
            if let Some(vid) = victim_id {
                if let Some(old) = g.active.remove(&vid) {
                    let old_end = old.stop_at.load(Ordering::Acquire);
                    old.preempted.store(true, Ordering::Release);
                    old.stop_at.store(a, Ordering::Release);
                    let id = g.next_id;
                    g.next_id += 1;
                    let seg = Segment {
                        start: a,
                        end: old_end,
                        written: 0,
                    };
                    let stop_at = Arc::new(AtomicU64::new(old_end));
                    let cursor = Arc::new(AtomicU64::new(a));
                    let flushed_cursor = Arc::new(AtomicU64::new(a));
                    let receiving = Arc::new(AtomicBool::new(false));
                    let preempted = Arc::new(AtomicBool::new(false));
                    let last_progress_ms = Arc::new(AtomicU64::new(now_ms));
                    g.active.insert(
                        id,
                        ActiveSeg {
                            start: a,
                            stop_at: stop_at.clone(),
                            cursor: cursor.clone(),
                            flushed_cursor: flushed_cursor.clone(),
                            receiving: receiving.clone(),
                            preempted: preempted.clone(),
                            assigned_at: Instant::now(),
                            last_progress_ms: last_progress_ms.clone(),
                        },
                    );
                    return WorkItem::Assigned {
                        id,
                        seg,
                        stop_at,
                        cursor,
                        flushed_cursor,
                        receiving,
                        preempted,
                        last_progress_ms,
                        split: None,
                    };
                }
            }
        }

        // 3. 动态二分窃取最大剩余活跃段
        if g.total_segments < MAX_PLANNER_SEGMENTS {
            let best = g
                .active
                .iter()
                .filter_map(|(aid, a)| {
                    let end = a.stop_at.load(Ordering::Acquire);
                    let pos = a.cursor.load(Ordering::Acquire);
                    let rem = end.saturating_sub(pos);
                    (rem >= 2 * min_half).then_some((*aid, pos, end, rem))
                })
                .max_by_key(|(_, _, _, rem)| *rem);

            if let Some((victim_id, pos, old_end, rem)) = best {
                let raw_mid = pos + rem / 2;
                let aligned = (raw_mid / 4096) * 4096;
                let mid = if aligned > pos && aligned < old_end {
                    aligned
                } else {
                    raw_mid
                };
                if mid > pos && mid < old_end {
                    if let Some(victim) = g.active.get(&victim_id) {
                        victim.stop_at.store(mid, Ordering::Release);
                    }
                    g.total_segments += 1;
                    let id = g.next_id;
                    g.next_id += 1;
                    let new_seg = Segment {
                        start: mid,
                        end: old_end,
                        written: 0,
                    };
                    let stop_at = Arc::new(AtomicU64::new(old_end));
                    let cursor = Arc::new(AtomicU64::new(mid));
                    let flushed_cursor = Arc::new(AtomicU64::new(mid));
                    let receiving = Arc::new(AtomicBool::new(false));
                    let preempted = Arc::new(AtomicBool::new(false));
                    let last_progress_ms = Arc::new(AtomicU64::new(now_ms));
                    g.active.insert(
                        id,
                        ActiveSeg {
                            start: mid,
                            stop_at: stop_at.clone(),
                            cursor: cursor.clone(),
                            flushed_cursor: flushed_cursor.clone(),
                            receiving: receiving.clone(),
                            preempted: preempted.clone(),
                            assigned_at: Instant::now(),
                            last_progress_ms: last_progress_ms.clone(),
                        },
                    );
                    return WorkItem::Assigned {
                        id,
                        seg: new_seg,
                        stop_at,
                        cursor,
                        flushed_cursor,
                        receiving,
                        preempted,
                        last_progress_ms,
                        split: Some((old_end, mid)),
                    };
                }
            }
        }

        // 4. 尾部停滞连接抢占：剩余段 < 2 * min_half 或仅剩少数尾段卡死超过 4s 无新增落盘时，
        //    从已安全落盘的 flushed_cursor 处由空闲连接接管剩余区间
        let stall_ms = STRAGGLER_STALL.as_millis() as u64;
        let straggler = g
            .active
            .iter()
            .filter_map(|(aid, a)| {
                let end = a.stop_at.load(Ordering::Acquire);
                let fpos = a.flushed_cursor.load(Ordering::Acquire);
                let lp = a.last_progress_ms.load(Ordering::Acquire);
                let stalled = a.assigned_at.elapsed() >= STRAGGLER_STALL
                    && now_ms.saturating_sub(lp) >= stall_ms;
                let need_split = fpos > a.start;
                if stalled && fpos < end && (!need_split || g.total_segments < MAX_PLANNER_SEGMENTS)
                {
                    Some((*aid, a.start, fpos, end))
                } else {
                    None
                }
            })
            .max_by_key(|(_, _, fpos, end)| end.saturating_sub(*fpos));

        if let Some((victim_id, victim_start, fpos, old_end)) = straggler {
            if let Some(old) = g.active.remove(&victim_id) {
                let split = if fpos > victim_start {
                    g.total_segments += 1;
                    Some((old_end, fpos))
                } else {
                    None
                };
                old.preempted.store(true, Ordering::Release);
                old.stop_at.store(fpos, Ordering::Release);
                let id = g.next_id;
                g.next_id += 1;
                let seg = Segment {
                    start: fpos,
                    end: old_end,
                    written: 0,
                };
                let stop_at = Arc::new(AtomicU64::new(old_end));
                let cursor = Arc::new(AtomicU64::new(fpos));
                let flushed_cursor = Arc::new(AtomicU64::new(fpos));
                let receiving = Arc::new(AtomicBool::new(false));
                let preempted = Arc::new(AtomicBool::new(false));
                let last_progress_ms = Arc::new(AtomicU64::new(now_ms));
                g.active.insert(
                    id,
                    ActiveSeg {
                        start: fpos,
                        stop_at: stop_at.clone(),
                        cursor: cursor.clone(),
                        flushed_cursor: flushed_cursor.clone(),
                        receiving: receiving.clone(),
                        preempted: preempted.clone(),
                        assigned_at: Instant::now(),
                        last_progress_ms: last_progress_ms.clone(),
                    },
                );
                return WorkItem::Assigned {
                    id,
                    seg,
                    stop_at,
                    cursor,
                    flushed_cursor,
                    receiving,
                    preempted,
                    last_progress_ms,
                    split,
                };
            }
        }

        if g.active.is_empty() && g.pending.is_empty() {
            WorkItem::Done
        } else {
            WorkItem::Wait
        }
    }

    fn finish_active(&self, id: u64) {
        if self.inner.lock().unwrap().active.remove(&id).is_some() {
            self.notify.notify_waiters();
        }
    }

    fn requeue_and_finish(&self, id: u64, remaining: Segment) {
        let mut g = self.inner.lock().unwrap();
        g.active.remove(&id);
        if remaining.end > remaining.start {
            g.pending.push_back(remaining);
        }
        drop(g);
        self.notify.notify_waiters();
    }

    fn notify_all(&self) {
        self.notify.notify_waiters();
    }
}

struct ActiveGuard<'a> {
    planner: &'a SegmentPlanner,
    id: u64,
    armed: bool,
}

impl<'a> ActiveGuard<'a> {
    fn new(planner: &'a SegmentPlanner, id: u64) -> Self {
        Self {
            planner,
            id,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.planner.finish_active(self.id);
        }
    }
}

enum StreamOutcome {
    Done(u64),
    Preempted,
    Aborted,
    Retired,
}

pub(crate) async fn run(
    engine: &Engine,
    task_id: &str,
    opts: DownloadOptions,
    cancel: CancellationToken,
    cancel_requested: Arc<std::sync::atomic::AtomicBool>,
) -> Res<Task> {
    let task = engine.require(task_id).await?;
    if !matches!(
        task.status,
        TaskStatus::Queued
            | TaskStatus::Paused
            | TaskStatus::WaitingRetry
            | TaskStatus::Downloading
    ) {
        return Err(DownloadError::new(
            ErrorKind::InvalidTransition {
                from: task.status.as_str().into(),
                to: "downloading".into(),
            },
            "task must be queued, paused, or crash-interrupted to start a download",
        ));
    }
    let client = build_client(opts.proxy.as_deref())?;
    let limiter = engine.task_limiter(opts.global_bps, opts.task_bps);
    let ctx = Arc::new(Ctx {
        engine: engine.clone(),
        opts,
        client,
        limiter,
        cancel,
        task_id: task_id.to_string(),
        url: task.url.clone(),
        task_headers: parse_task_headers(&task),
        checksum: task
            .checksum
            .as_deref()
            .and_then(crate::checksum::parse_checksum),
        postprocess: dl_model::PostProcess::parse_list(task.postprocess_json.as_deref()),
    });

    let mirrors_raw: Vec<String> = task
        .mirrors_json
        .as_deref()
        .and_then(|j| serde_json::from_str(j).ok())
        .unwrap_or_default();

    let mut restarts: u32 = 0;
    let result = loop {
        let step = if task.kind == dl_model::TaskKind::Ytdlp {
            crate::ytdlp_task::run_ytdlp(ctx.clone(), &task)
                .await
                .map(|_| ())
        } else {
            phase(ctx.clone(), &mirrors_raw).await
        };
        match step {
            Ok(()) => break Ok(()),
            Err(e @ DriveErr::Fatal(..)) => break Err(e),
            Err(DriveErr::Paused) => break Err(DriveErr::Paused),
            Err(DriveErr::Restart) => {
                restarts += 1;
                if restarts > 2 {
                    break Err(DriveErr::Fatal(
                        ErrorKind::Internal,
                        "server content keeps changing during download".into(),
                    ));
                }
            }
        }
    };

    match result {
        Ok(()) => engine.require(task_id).await,
        Err(DriveErr::Paused) => {
            let cur = engine.require(task_id).await?;
            let is_cancel = cancel_requested.load(Ordering::Relaxed);
            if matches!(cur.status, TaskStatus::Probing | TaskStatus::Downloading) {
                if is_cancel {
                    engine.transition(task_id, TaskStatus::Canceled, None).await
                } else {
                    engine.transition(task_id, TaskStatus::Paused, None).await
                }
            } else if cur.status == TaskStatus::Paused && is_cancel {
                engine.transition(task_id, TaskStatus::Canceled, None).await
            } else {
                Ok(cur)
            }
        }
        Err(DriveErr::Fatal(kind, msg)) => {
            let cur = engine.require(task_id).await?;
            if !matches!(cur.status, TaskStatus::Failed | TaskStatus::Done) {
                engine
                    .transition(task_id, TaskStatus::Failed, Some(kind.clone()))
                    .await?;
            }
            Err(DownloadError::new(kind, msg))
        }
        Err(DriveErr::Restart) => unreachable!("restart loop handles Restart"),
    }
}

async fn phase(ctx: Arc<Ctx>, mirrors_raw: &[String]) -> Result<(), DriveErr> {
    let task = ctx.engine.require(&ctx.task_id).await.map_err(drive_from)?;
    match task.status {
        TaskStatus::Queued | TaskStatus::WaitingRetry | TaskStatus::Paused => {
            ctx.engine
                .transition(&ctx.task_id, TaskStatus::Probing, None)
                .await
                .map_err(drive_from)?;
        }
        TaskStatus::Downloading => {}
        s => {
            return Err(DriveErr::Fatal(
                ErrorKind::InvalidTransition {
                    from: s.as_str().into(),
                    to: "downloading".into(),
                },
                format!("cannot download from state {}", s.as_str()),
            ))
        }
    }

    if task.kind == dl_model::TaskKind::Hls {
        return crate::hls::phase(ctx).await;
    }

    let probe = tokio::select! {
        _ = ctx.cancel.cancelled() => return Err(DriveErr::Paused),
        result = probe::probe(&ctx.client, &task.url, &ctx.task_headers) => result.map_err(drive_from)?,
    };

    // 合并中转页穿透带出的 Referer / Cookie（任务显式头优先）
    let mut run_headers = ctx.task_headers.clone();
    for (k, v) in &probe.extra_headers {
        run_headers.entry(k.clone()).or_insert_with(|| v.clone());
    }
    let ctx = if run_headers.len() != ctx.task_headers.len() {
        let mut next = (*ctx).clone();
        next.task_headers = run_headers;
        Arc::new(next)
    } else {
        ctx
    };

    // 并发探测多镜像：Range 可用且总长与主源一致才启用（超时 5s/个，失败跳过）
    let mut mirrors: Vec<String> = Vec::new();
    let mut mirror_set = tokio::task::JoinSet::new();
    for m in mirrors_raw.iter().take(8) {
        if m == &task.url {
            continue;
        }
        let client = ctx.client.clone();
        let url = m.clone();
        let hdrs = ctx.task_headers.clone();
        let expected_total = probe.total_bytes;
        mirror_set.spawn(async move {
            let fut = probe::probe(&client, &url, &hdrs);
            match tokio::time::timeout(Duration::from_secs(5), fut).await {
                Ok(Ok(mp)) if mp.segmented && mp.total_bytes == expected_total => {
                    Some(mp.final_url)
                }
                _ => None,
            }
        });
    }
    while let Some(res) = mirror_set.join_next().await {
        if let Ok(Some(u)) = res {
            mirrors.push(u);
        }
    }
    if !mirrors.is_empty() {
        tracing::info!(count = mirrors.len(), "mirrors enabled");
    }
    // 主源使用探测解析后的 final_url（避免每个分段重新走重定向链/中转页）
    mirrors.insert(0, probe.final_url.clone());

    if ctx.cancel.is_cancelled() {
        return Err(DriveErr::Paused);
    }
    ctx.engine
        .record_probe(
            &ctx.task_id,
            probe.total_bytes,
            probe.etag.clone(),
            probe.last_modified.clone(),
            probe.mime.clone(),
            Some(probe.final_url.clone()),
        )
        .await
        .map_err(drive_from)?;

    let paths = target_paths(&ctx, &task, &probe)?;
    ctx.engine
        .record_output(&ctx.task_id, &paths.dir, &paths.name, None)
        .await
        .map_err(drive_from)?;
    let total = probe.total_bytes;

    let cur = ctx.engine.require(&ctx.task_id).await.map_err(drive_from)?;
    if matches!(cur.status, TaskStatus::Probing | TaskStatus::Paused) {
        ctx.engine
            .transition(&ctx.task_id, TaskStatus::Downloading, None)
            .await
            .map_err(drive_from)?;
    }

    let segmented = probe.segmented && probe.total_bytes.is_some();
    let mut prep = if segmented {
        prepare_segmented(&ctx, &probe, &paths)?
    } else {
        Journal::remove(&paths.journal);
        let _ = std::fs::remove_file(&paths.part);
        PreparedSegments {
            mode: Mode::Sequential,
            segments: vec![Segment {
                start: 0,
                end: u64::MAX,
                written: 0,
            }],
            journal: None,
            resumed: false,
            needs_overlap: false,
        }
    };
    if segmented
        && prep.resumed
        && prep.needs_overlap
        && !verify_resume_overlap(&ctx, &paths.part, &probe.final_url, &prep.segments).await
    {
        tracing::warn!(
            task = %ctx.task_id,
            "resume overlap bytes mismatch; restarting fresh"
        );
        cleanup_partial(&paths);
        prep = prepare_segmented(&ctx, &probe, &paths)?;
    }

    // 磁盘空间预检：续传时 `.dlpart` 已占空间不重复索取全量，仅校验首次新建/未分配任务所需空间
    if !prep.resumed {
        if let Some(need) = total {
            check_disk_space(&paths.dir, need).await?;
        }
    }

    let shared = Arc::new(Shared::new(probe.etag.clone(), probe.last_modified.clone()));
    let outcome = if segmented {
        download_segmented(
            ctx.clone(),
            shared.clone(),
            prep.mode,
            prep.segments,
            prep.journal,
            &paths,
            mirrors.clone(),
            &probe,
        )
        .await?
    } else {
        download_sequential(ctx.clone(), shared, &paths, &probe.final_url, total).await?
    };

    match outcome {
        PhaseOutcome::Completed(final_segs) => {
            finish_completed(
                &ctx,
                &paths,
                &final_segs,
                &probe.final_url,
                total,
                segmented,
                probe.server_checksum.as_ref(),
            )
            .await
        }
        PhaseOutcome::Fatal(kind, msg) => Err(DriveErr::Fatal(kind, msg)),
        PhaseOutcome::Paused => Err(DriveErr::Paused),
        PhaseOutcome::FallbackSequential => {
            cleanup_partial(&paths);
            let outcome = download_sequential(
                ctx.clone(),
                Arc::new(Shared::new(probe.etag.clone(), probe.last_modified.clone())),
                &paths,
                &probe.final_url,
                total,
            )
            .await?;
            match outcome {
                PhaseOutcome::Completed(final_segs) => {
                    finish_completed(
                        &ctx,
                        &paths,
                        &final_segs,
                        &probe.final_url,
                        total,
                        false,
                        probe.server_checksum.as_ref(),
                    )
                    .await
                }
                PhaseOutcome::Paused => Err(DriveErr::Paused),
                PhaseOutcome::Fatal(kind, msg) => Err(DriveErr::Fatal(kind, msg)),
                _ => Err(DriveErr::Fatal(
                    ErrorKind::Internal,
                    "unexpected phase outcome in fallback".into(),
                )),
            }
        }
    }
}

async fn finish_completed(
    ctx: &Ctx,
    paths: &TargetPaths,
    final_segs: &[Segment],
    final_url: &str,
    total: Option<u64>,
    segmented: bool,
    server_checksum: Option<&(String, String)>,
) -> Result<(), DriveErr> {
    ctx.engine
        .transition(&ctx.task_id, TaskStatus::Verifying, None)
        .await
        .map_err(drive_from)?;
    verify(ctx, paths, final_segs, final_url, total, segmented).await?;
    if !ctx.postprocess.is_empty() {
        ctx.engine
            .transition(&ctx.task_id, TaskStatus::Decrypting, None)
            .await
            .map_err(drive_from)?;
        crate::postprocess::apply_all(&paths.part, &ctx.postprocess)
            .map_err(|e| DriveErr::Fatal(e.kind, e.message))?;
        ctx.engine
            .transition(&ctx.task_id, TaskStatus::Verifying, None)
            .await
            .map_err(drive_from)?;
    }
    let effective_checksum = ctx.checksum.clone().or_else(|| server_checksum.cloned());
    if let Some((algo, expect)) = effective_checksum {
        let part_path = paths.part.clone();
        let algo_c = algo.clone();
        let ok = tokio::task::spawn_blocking(move || {
            crate::checksum::verify_file(&part_path, &algo_c, &expect)
        })
        .await
        .map_err(|e| DriveErr::Fatal(ErrorKind::Internal, format!("checksum task: {e}")))?
        .map_err(|e| DriveErr::Fatal(e.kind, e.message))?;
        if !ok {
            return Err(DriveErr::Fatal(
                ErrorKind::ChecksumMismatch,
                format!("checksum {algo} mismatch"),
            ));
        }
    }
    finalize(ctx, paths).await?;
    ctx.engine
        .transition(&ctx.task_id, TaskStatus::Done, None)
        .await
        .map_err(drive_from)?;
    Ok(())
}

pub(crate) struct TargetPaths {
    pub(crate) dir: PathBuf,
    pub(crate) name: String,
    pub(crate) part: PathBuf,
    pub(crate) journal: PathBuf,
}

fn target_paths(
    ctx: &Ctx,
    task: &dl_model::Task,
    probe: &ProbeResult,
) -> Result<TargetPaths, DriveErr> {
    let dir = PathBuf::from(
        task.dir
            .clone()
            .unwrap_or_else(|| ctx.opts.dir.display().to_string()),
    );
    std::fs::create_dir_all(&dir).map_err(|e| {
        DriveErr::Fatal(
            ErrorKind::FileAccessDenied,
            format!("create download dir {}: {e}", dir.display()),
        )
    })?;
    let url_path = url::Url::parse(&probe.final_url)
        .or_else(|_| url::Url::parse(&task.url))
        .map(|u| u.path().to_string())
        .unwrap_or_default();
    let name = pick_name(
        task.filename.as_deref(),
        probe.filename.as_deref(),
        &url_path,
        probe.mime.as_deref(),
    );
    let journal_dir = ctx.engine.journal_dir();
    std::fs::create_dir_all(&journal_dir).map_err(|e| {
        DriveErr::Fatal(
            ErrorKind::FileAccessDenied,
            format!("create journal dir: {e}"),
        )
    })?;
    let part = dir.join(format!("{name}.dlpart"));
    Ok(TargetPaths {
        journal: journal_dir.join(format!("{}.dlctrl", ctx.task_id)),
        name,
        part,
        dir,
    })
}

struct PreparedSegments {
    mode: Mode,
    segments: Vec<Segment>,
    journal: Option<Journal>,
    resumed: bool,
    needs_overlap: bool,
}

fn prepare_segmented(
    ctx: &Ctx,
    probe: &ProbeResult,
    paths: &TargetPaths,
) -> Result<PreparedSegments, DriveErr> {
    let total = probe.total_bytes.unwrap_or(0);
    let vh = dl_journal::validator_hash(probe.etag.as_deref(), probe.last_modified.as_deref());
    let has_validator = probe.etag.is_some() || probe.last_modified.is_some();
    let (j, recovered) = Journal::open(&paths.journal)
        .map_err(|e| DriveErr::Fatal(classify_io(&e), format!("open journal: {e}")))?;

    let (usable, validator_changed) = match &recovered {
        Some(d) => {
            let base_ok = (has_validator || total > 0)
                && d.total_len == total
                && paths.part.exists()
                && journal_entries_sane(&d.entries, total);
            let vchanged = d.validator_hash != vh;
            (base_ok, vchanged)
        }
        None => (false, false),
    };

    match usable.then(|| recovered.clone().unwrap()) {
        Some(d) => Ok(PreparedSegments {
            mode: Mode::Segmented {
                total,
                validator_hash: vh,
            },
            segments: d
                .entries
                .into_iter()
                .map(|e| Segment {
                    start: e.start,
                    end: e.end,
                    written: e.written,
                })
                .collect(),
            journal: Some(j),
            resumed: true,
            needs_overlap: !has_validator || validator_changed,
        }),
        None => {
            Journal::remove(&paths.journal);
            let _ = std::fs::remove_file(&paths.part);
            let (j, _) = Journal::open(&paths.journal)
                .map_err(|e| DriveErr::Fatal(classify_io(&e), format!("open journal: {e}")))?;
            let planned = dl_segment::plan(total, ctx.opts.segments.min(MAX_SEGMENTS));
            Ok(PreparedSegments {
                mode: Mode::Segmented {
                    total,
                    validator_hash: vh,
                },
                segments: planned,
                journal: Some(j),
                resumed: false,
                needs_overlap: false,
            })
        }
    }
}

fn journal_entries_sane(entries: &[dl_journal::Entry], total: u64) -> bool {
    let mut segs: Vec<Segment> = entries
        .iter()
        .map(|e| Segment {
            start: e.start,
            end: e.end,
            written: e.written,
        })
        .collect();
    segs.sort_by_key(|s| s.start);
    let mut cursor = 0u64;
    for s in &segs {
        if s.start != cursor || s.end > total || s.written > s.len() {
            return false;
        }
        cursor = s.end;
    }
    cursor == total
}

enum PhaseOutcome {
    Completed(Vec<Segment>),
    Fatal(ErrorKind, String),
    Paused,
    FallbackSequential,
}

struct WorkerCountGuard(Arc<Shared>);

impl Drop for WorkerCountGuard {
    fn drop(&mut self) {
        if self.0.live_workers.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.workers_done.notify_waiters();
        }
    }
}

async fn wait_all_workers_done(shared: &Shared) {
    loop {
        if shared.live_workers.load(Ordering::Acquire) == 0 {
            return;
        }
        shared.workers_done.notified().await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn download_segmented(
    ctx: Arc<Ctx>,
    shared: Arc<Shared>,
    mode: Mode,
    initial_segs: Vec<Segment>,
    journal: Option<Journal>,
    paths: &TargetPaths,
    mirrors: Vec<String>,
    primary: &ProbeResult,
) -> Result<PhaseOutcome, DriveErr> {
    let total_seg_count = initial_segs.len();
    let (writer, all_rx) = dl_io::spawn(SpawnParams {
        path: paths.part.clone(),
        mode,
        segments: initial_segs.clone(),
        journal,
    })
    .await
    .map_err(|e| DriveErr::Fatal(classify_io(&e), format!("open part file: {e}")))?;

    {
        let ectx = ctx.clone();
        let eshared = shared.clone();
        let progress = writer.progress();
        let etotal = primary.total_bytes;
        tokio::spawn(emit_progress(ectx, eshared, progress, etotal));
    }

    let ranges: Vec<Segment> = initial_segs
        .iter()
        .filter(|s| !s.done())
        .map(|s| Segment {
            start: s.cursor(),
            end: s.end,
            written: 0,
        })
        .collect();
    let planner = Arc::new(SegmentPlanner::new(ranges, total_seg_count));
    let host = url::Url::parse(&primary.final_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .unwrap_or_default();
    let learned_cap = ctx.engine.host_limits().cap(&host);
    let mut worker_count = ctx.opts.segments.clamp(1, MAX_SEGMENTS);
    if let Some(cap) = learned_cap {
        worker_count = worker_count.min(cap.max(1));
    }
    shared.active_workers.store(worker_count, Ordering::Relaxed);
    shared.live_workers.store(worker_count, Ordering::Release);

    // 共享一个带较大连接池的 HTTP/1.1 客户端（参照 MDM：keep-alive 复用 + TLS 会话恢复）
    let worker_client = build_pooled_worker_client(ctx.opts.proxy.as_deref(), MAX_SEGMENTS)
        .unwrap_or_else(|_| ctx.client.clone());
    let preempt_after = PREEMPT_MIN.max(Duration::from_millis(primary.ttfb_ms.saturating_mul(2)));
    let primary_total = primary.total_bytes;

    for _ in 0..worker_count {
        tokio::spawn(worker_main(
            ctx.clone(),
            worker_client.clone(),
            shared.clone(),
            planner.clone(),
            writer.tx(),
            mirrors.clone(),
            preempt_after,
            primary_total,
        ));
    }

    // Ghost-Downloader-3 自适应提速监督：网速稳定且未被限速/未触及主机上限时，逐步扩增 worker
    let sup_cancel = ctx.cancel.child_token();
    let auto_speed_up = ctx
        .engine
        .get_setting("auto_speed_up")
        .await
        .ok()
        .flatten()
        .map(|v| v != "off")
        .unwrap_or(true);
    let rate_limited = ctx.opts.task_bps.unwrap_or(0) > 0
        || ctx.opts.global_bps.unwrap_or(0) > 0
        || ctx.engine.has_global_rate_override();
    if auto_speed_up
        && primary.total_bytes.unwrap_or(0) >= 8 * 1024 * 1024
        && !rate_limited
        && worker_count < MAX_SEGMENTS
        && learned_cap.is_none()
        && !mirrors.is_empty()
    {
        let s_ctx = ctx.clone();
        let s_client = worker_client.clone();
        let s_shared = shared.clone();
        let s_planner = planner.clone();
        let s_wtx = writer.tx();
        let s_mirrors = mirrors.clone();
        let s_host = host.clone();
        let s_cancel = sup_cancel.clone();
        tokio::spawn(async move {
            let mut history: Vec<u64> = Vec::with_capacity(5);
            let mut last_bytes = s_shared.received_bytes.load(Ordering::Relaxed);
            let mut baseline: Option<(usize, f64, Instant)> = None;
            loop {
                tokio::select! {
                    _ = s_cancel.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                }
                if s_shared.has_fatal()
                    || s_shared.no_range_fallback.load(Ordering::Relaxed)
                    || s_ctx.engine.host_limits().cap(&s_host).is_some()
                {
                    break;
                }
                let cur_bytes = s_shared.received_bytes.load(Ordering::Relaxed);
                let delta = cur_bytes.saturating_sub(last_bytes);
                last_bytes = cur_bytes;
                history.push(delta);
                if history.len() > 5 {
                    history.remove(0);
                }
                if history.len() < 5 {
                    continue;
                }
                let avg = history.iter().map(|&v| v as f64).sum::<f64>() / 5.0;
                if avg <= 0.0 {
                    continue;
                }
                let max_dev = history
                    .iter()
                    .map(|&v| (v as f64 - avg).abs() / avg)
                    .fold(0.0f64, f64::max);
                if max_dev > 0.20 {
                    continue;
                }
                if let Some((init_workers, init_speed, since)) = baseline {
                    if since.elapsed() <= Duration::from_secs(5) {
                        continue;
                    }
                    let cur_live = s_shared.live_workers.load(Ordering::Acquire);
                    let worker_ratio =
                        (cur_live.saturating_sub(init_workers)) as f64 / init_workers.max(1) as f64;
                    let speed_ratio = (avg - init_speed) / init_speed.max(1.0);
                    if worker_ratio <= 0.0 || speed_ratio < 0.5 * worker_ratio {
                        tracing::debug!(worker_ratio, speed_ratio, "auto speed-up plateau reached");
                        break;
                    }
                    baseline = None;
                    history.clear();
                    continue;
                }
                let cur_live = s_shared.live_workers.load(Ordering::Acquire);
                if cur_live >= MAX_SEGMENTS || !s_planner.has_splittable(STEAL_MIN_HALF) {
                    continue;
                }
                let add = (MAX_SEGMENTS - cur_live).min(4);
                if add == 0 {
                    break;
                }
                baseline = Some((cur_live, avg, Instant::now()));
                s_shared.live_workers.fetch_add(add, Ordering::AcqRel);
                s_shared.active_workers.fetch_add(add, Ordering::Relaxed);
                for _ in 0..add {
                    tokio::spawn(worker_main(
                        s_ctx.clone(),
                        s_client.clone(),
                        s_shared.clone(),
                        s_planner.clone(),
                        s_wtx.clone(),
                        s_mirrors.clone(),
                        preempt_after,
                        primary_total,
                    ));
                }
                tracing::debug!(added = add, "auto speed-up spawned extra workers");
            }
        });
    }

    let mut all_rx = all_rx;
    let all_result = tokio::select! {
        biased;
        r = &mut all_rx => Some(r),
        _ = wait_all_workers_done(&shared) => {
            tokio::time::timeout(Duration::from_millis(500), &mut all_rx)
                .await
                .ok()
        }
    };
    sup_cancel.cancel();

    if let Some(kind) = shared.take_fatal() {
        let _ = writer.finish().await;
        return Ok(PhaseOutcome::Fatal(kind, "worker failure".into()));
    }
    if ctx.cancel.is_cancelled() {
        let _ = writer.send(Msg::Flush).await;
        let _ = writer.finish().await;
        return Ok(PhaseOutcome::Paused);
    }
    if shared.no_range_fallback.load(Ordering::Relaxed) {
        let _ = writer.finish().await;
        return Ok(PhaseOutcome::FallbackSequential);
    }
    match all_result {
        Some(Ok(Ok(final_segs))) => {
            let segs = writer
                .finish()
                .await
                .map_err(|e| DriveErr::Fatal(classify_io(&e), format!("finalize write: {e}")))?;
            Ok(PhaseOutcome::Completed(if segs.is_empty() {
                final_segs
            } else {
                segs
            }))
        }
        Some(Ok(Err(e))) => Err(DriveErr::Fatal(
            classify_io(&e),
            format!("write failure: {e}"),
        )),
        Some(Err(_)) | None => Err(DriveErr::Fatal(
            ErrorKind::Internal,
            "workers exited without completing all segments".into(),
        )),
    }
}

async fn emit_progress(
    ctx: Arc<Ctx>,
    shared: Arc<Shared>,
    mut progress: tokio::sync::watch::Receiver<dl_io::WriterProgress>,
    total: Option<u64>,
) {
    let mut last = 0u64;
    let mut last_t = Instant::now();
    let mut bps = 0f64;
    let mut last_persisted = Instant::now();
    let mut has_sample = false;
    loop {
        if progress.changed().await.is_err() {
            break;
        }
        let snap = progress.borrow_and_update().clone();
        let now = snap.downloaded;
        let dt = last_t.elapsed().as_secs_f64();
        if dt >= 0.1 {
            if !has_sample {
                has_sample = true;
                last = now;
                last_t = Instant::now();
                continue;
            }
            let inst = (now.saturating_sub(last)) as f64 / dt;
            bps = if bps == 0.0 {
                inst
            } else {
                bps * 0.7 + inst * 0.3
            };
            last = now;
            last_t = Instant::now();
            shared.task_downloaded.store(now, Ordering::Relaxed);
            shared.aggregate_bps.store(bps as u64, Ordering::Relaxed);
            let segments = if snap.segs.is_empty() {
                None
            } else {
                Some(snap.segs.clone())
            };
            ctx.engine.emit(EngineEvent::Progress {
                task_id: ctx.task_id.clone(),
                downloaded: now,
                total,
                bps: bps as u64,
                segments,
            });
            if last_persisted.elapsed() >= Duration::from_secs(2) {
                let _ = ctx.engine.record_downloaded(&ctx.task_id, now).await;
                last_persisted = Instant::now();
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn worker_main(
    ctx: Arc<Ctx>,
    worker_client: reqwest::Client,
    shared: Arc<Shared>,
    planner: Arc<SegmentPlanner>,
    wtx: mpsc::Sender<Msg>,
    mirrors: Vec<String>,
    preempt_after: Duration,
    primary_total: Option<u64>,
) {
    let _live_guard = WorkerCountGuard(shared.clone());
    let mut after: Option<u64> = None;
    loop {
        if ctx.cancel.is_cancelled()
            || shared.no_range_fallback.load(Ordering::Relaxed)
            || shared.has_fatal()
        {
            break;
        }
        match planner.next_work(after.take(), STEAL_MIN_HALF, preempt_after) {
            WorkItem::Done => break,
            WorkItem::Wait => {
                tokio::select! {
                    _ = ctx.cancel.cancelled() => break,
                    _ = planner.notify.notified() => {},
                    _ = tokio::time::sleep(Duration::from_millis(150)) => {},
                }
            }
            WorkItem::Assigned {
                id,
                seg,
                stop_at,
                cursor,
                flushed_cursor,
                receiving,
                preempted,
                last_progress_ms,
                split,
            } => {
                let mut guard = ActiveGuard::new(&planner, id);
                if let Some((old_end, split_at)) = split {
                    if wtx.send(Msg::SplitAt { old_end, split_at }).await.is_err() {
                        shared.record_fatal(ErrorKind::Internal);
                        planner.notify_all();
                        break;
                    }
                    tracing::debug!(old_end, split_at, "dynamic segment steal");
                }

                let midx = pick_mirror(&shared, &mirrors);
                match stream_range(
                    &ctx,
                    &worker_client,
                    &shared,
                    &planner,
                    &mut guard,
                    &wtx,
                    seg,
                    stop_at,
                    cursor,
                    flushed_cursor,
                    receiving,
                    preempted,
                    last_progress_ms,
                    &mirrors,
                    midx,
                    primary_total,
                )
                .await
                {
                    Ok(StreamOutcome::Done(end)) => {
                        after = Some(end);
                        drop(guard);
                    }
                    Ok(StreamOutcome::Preempted) => {
                        drop(guard);
                    }
                    Ok(StreamOutcome::Retired) => {
                        drop(guard);
                        break;
                    }
                    Ok(StreamOutcome::Aborted) => {
                        drop(guard);
                    }
                    Err(kind) => {
                        drop(guard);
                        shared.record_fatal(kind);
                        planner.notify_all();
                        break;
                    }
                }
            }
        }
    }
    shared.active_workers.fetch_sub(1, Ordering::Relaxed);
}

struct Pace {
    started: Instant,
    received: u64,
}

impl Pace {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            received: 0,
        }
    }

    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    fn speed(&self) -> u64 {
        rate_bps(self.received, self.elapsed())
    }
}

fn rate_bps(bytes: u64, d: Duration) -> u64 {
    let secs = d.as_secs_f64();
    if secs > 0.0 {
        (bytes as f64 / secs) as u64
    } else {
        u64::MAX
    }
}

fn note_best(shared: &Shared, pace: &Pace) {
    let elapsed = pace.elapsed();
    if pace.received >= 256 * 1024 && elapsed >= BEST_AFTER {
        shared
            .best_speed
            .fetch_max(rate_bps(pace.received, elapsed), Ordering::Relaxed);
    }
    note_link(shared);
}

fn note_link(shared: &Shared) {
    let now = Instant::now();
    let total = shared.received_bytes.load(Ordering::Relaxed);
    let mut m = shared.link_meter.lock().unwrap();
    let Some((since, then)) = m.since else {
        m.since = Some((now, total));
        return;
    };
    let took = now.saturating_duration_since(since);
    if took < LINK_WINDOW {
        return;
    }
    m.best = m.best.max(rate_bps(total.saturating_sub(then), took));
    m.since = Some((now, total));
}

fn shed_bar(one: u64, link: u64, receiving: usize) -> u64 {
    if link == 0 {
        return one;
    }
    one.min(link.max(one) / receiving.max(1) as u64)
}

fn too_slow(speed: u64, on_net: Duration, best: u64, left: u64) -> bool {
    if on_net < SHED_GRACE || best == 0 {
        return false;
    }
    speed.saturating_mul(SHED_RATIO) < best
        && (left as f64 / speed.max(1) as f64) > SHED_LEFT.as_secs_f64()
}

async fn flush_batch(
    batch_buf: &mut Vec<u8>,
    batch_start: &mut u64,
    batch_opened: &mut Instant,
    wtx: &mpsc::Sender<Msg>,
) -> Result<(), ErrorKind> {
    if batch_buf.is_empty() {
        return Ok(());
    }
    let next_start = *batch_start + batch_buf.len() as u64;
    let data = std::mem::replace(batch_buf, Vec::with_capacity(WRITE_BATCH));
    if wtx
        .send(Msg::Chunk {
            offset: *batch_start,
            data,
        })
        .await
        .is_err()
    {
        return Err(ErrorKind::Internal);
    }
    *batch_start = next_start;
    *batch_opened = Instant::now();
    Ok(())
}

fn record_mirror_speed(shared: &Shared, midx: usize, bps: u64) {
    if bps == 0 {
        return;
    }
    let mut speeds = shared.mirror_speeds.lock().unwrap();
    if speeds.len() <= midx {
        speeds.resize(midx + 1, 0);
    }
    let cur = speeds[midx];
    speeds[midx] = if cur == 0 {
        bps
    } else {
        (cur * 6 + bps * 4) / 10
    };
}

fn record_mirror_failure(shared: &Shared, midx: usize) {
    let mut fails = shared.mirror_failures.lock().unwrap();
    if fails.len() <= midx {
        fails.resize(midx + 1, 0);
    }
    fails[midx] = fails[midx].saturating_add(1);
}

fn pick_mirror(shared: &Shared, mirrors: &[String]) -> usize {
    pick_mirror_excluding(shared, mirrors, None)
}

fn pick_mirror_excluding(shared: &Shared, mirrors: &[String], exclude: Option<usize>) -> usize {
    if mirrors.len() <= 1 {
        return 0;
    }
    let bad = shared.bad_mirrors.lock().unwrap();
    let mut speeds = shared.mirror_speeds.lock().unwrap();
    if speeds.len() < mirrors.len() {
        speeds.resize(mirrors.len(), 0);
    }
    let mut fails = shared.mirror_failures.lock().unwrap();
    if fails.len() < mirrors.len() {
        fails.resize(mirrors.len(), 0);
    }

    // 1. 优先轮询未探测速度且未标记坏源的镜像，让所有镜像都得到真实测速
    let base = (shared.mirror_cursor.fetch_add(1, Ordering::Relaxed) as usize) % mirrors.len();
    for off in 0..mirrors.len() {
        let idx = (base + off) % mirrors.len();
        if Some(idx) == exclude || bad.contains(&idx) {
            continue;
        }
        if speeds[idx] == 0 {
            return idx;
        }
    }

    // 2. 均有样本时，按「EWMA 速度 / (1 + 失败次数)」选最快健康镜像
    let best = (0..mirrors.len())
        .filter(|i| Some(*i) != exclude && !bad.contains(i))
        .max_by_key(|&i| speeds[i] / (1 + fails[i] as u64));
    best.unwrap_or_else(|| exclude.unwrap_or(0))
}

#[allow(clippy::too_many_arguments)]
async fn stream_range(
    ctx: &Ctx,
    client: &reqwest::Client,
    shared: &Shared,
    planner: &SegmentPlanner,
    guard: &mut ActiveGuard<'_>,
    wtx: &mpsc::Sender<Msg>,
    seg: Segment,
    stop_at: Arc<AtomicU64>,
    active_cursor: Arc<AtomicU64>,
    flushed_cursor: Arc<AtomicU64>,
    receiving: Arc<AtomicBool>,
    preempted: Arc<AtomicBool>,
    last_progress_ms: Arc<AtomicU64>,
    mirrors: &[String],
    mut midx: usize,
    primary_total: Option<u64>,
) -> Result<StreamOutcome, ErrorKind> {
    let mut cursor = seg.start;
    let mut attempts: u32 = 0;
    let mut batch_buf: Vec<u8> = Vec::with_capacity(WRITE_BATCH);
    let mut batch_start = cursor;
    let mut batch_opened = Instant::now();
    let rate_limited = ctx.opts.task_bps.unwrap_or(0) > 0
        || ctx.opts.global_bps.unwrap_or(0) > 0
        || ctx.engine.has_global_rate_override();

    let others_receiving = || {
        let self_recv = receiving.load(Ordering::Acquire);
        let total_recv = planner.receiving_count();
        if self_recv {
            total_recv.saturating_sub(1)
        } else {
            total_recv
        }
    };

    'requests: loop {
        if preempted.load(Ordering::Acquire) {
            return Ok(StreamOutcome::Preempted);
        }
        let effective_end = stop_at.load(Ordering::Acquire);
        if cursor >= effective_end {
            flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, wtx).await?;
            flushed_cursor.store(cursor, Ordering::Release);
            return Ok(StreamOutcome::Done(effective_end));
        }
        if ctx.cancel.is_cancelled() || shared.no_range_fallback.load(Ordering::Relaxed) {
            flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, wtx).await?;
            flushed_cursor.store(cursor, Ordering::Release);
            return Ok(StreamOutcome::Aborted);
        }

        let mirror_url = mirrors
            .get(midx)
            .cloned()
            .unwrap_or_else(|| ctx.url.clone());
        let host = url::Url::parse(&mirror_url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
            .unwrap_or_default();

        if attempts >= MAX_ATTEMPTS {
            flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, wtx).await?;
            if others_receiving() > 0 {
                let remaining_end = stop_at.load(Ordering::Acquire);
                guard.disarm();
                planner.requeue_and_finish(
                    guard.id,
                    Segment {
                        start: cursor,
                        end: remaining_end,
                        written: 0,
                    },
                );
                return Ok(StreamOutcome::Retired);
            }
            return Err(ErrorKind::NetworkConnectionReset);
        }

        let sent_if_range = shared.validators.lock().unwrap().if_range();
        let mut req = with_task_headers(
            identity_headers(client.get(&mirror_url))
                .header("Range", format!("bytes={}-{}", cursor, effective_end - 1)),
            &ctx.task_headers,
        );
        if let Some(ref v) = sent_if_range {
            req = req.header("If-Range", v);
        }
        let response = tokio::select! {
            _ = ctx.cancel.cancelled() => {
                flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, wtx).await?;
                return Ok(StreamOutcome::Aborted);
            }
            result = req.send() => result,
        };
        if preempted.load(Ordering::Acquire) {
            return Ok(StreamOutcome::Preempted);
        }
        let resp = match response {
            Ok(r) => r,
            Err(e) => {
                let other_recv = others_receiving();
                if !receiving.load(Ordering::Acquire) && other_recv > 0 && mirrors.len() == 1 {
                    let kind = map_reqwest_err(&e);
                    if matches!(
                        kind,
                        ErrorKind::NetworkConnectionReset | ErrorKind::NetworkConnectTimeout
                    ) {
                        if let Some(cap) = ctx.engine.host_limits().refused(&host, other_recv) {
                            if planner.active_count() > cap {
                                flush_batch(
                                    &mut batch_buf,
                                    &mut batch_start,
                                    &mut batch_opened,
                                    wtx,
                                )
                                .await?;
                                flushed_cursor.store(cursor, Ordering::Release);
                                let remaining_end = stop_at.load(Ordering::Acquire);
                                guard.disarm();
                                planner.requeue_and_finish(
                                    guard.id,
                                    Segment {
                                        start: cursor,
                                        end: remaining_end,
                                        written: 0,
                                    },
                                );
                                return Ok(StreamOutcome::Retired);
                            }
                        }
                    }
                }
                attempts += 1;
                if mirrors.len() > 1 {
                    record_mirror_failure(shared, midx);
                    midx = pick_mirror_excluding(shared, mirrors, Some(midx));
                }
                tracing::debug!(error = %e, attempt = attempts, "segment request failed, retrying");
                if !sleep_or_cancel(&ctx.cancel, backoff(attempts)).await {
                    flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, wtx).await?;
                    return Ok(StreamOutcome::Aborted);
                }
                continue 'requests;
            }
        };
        let status = resp.status().as_u16();
        match status {
            206 => {
                if let Some((start, cr_total)) = resp
                    .headers()
                    .get(reqwest::header::CONTENT_RANGE)
                    .and_then(|v| v.to_str().ok())
                    .and_then(parse_content_range)
                {
                    let total_mismatch =
                        primary_total.is_some() && cr_total.is_some() && cr_total != primary_total;
                    if start != cursor || total_mismatch {
                        attempts += 1;
                        drop(resp);
                        tracing::debug!(
                            expected = cursor,
                            got = start,
                            expected_total = ?primary_total,
                            got_total = ?cr_total,
                            "content-range mismatch, retrying"
                        );
                        if !sleep_or_cancel(&ctx.cancel, backoff(attempts)).await {
                            flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, wtx)
                                .await?;
                            return Ok(StreamOutcome::Aborted);
                        }
                        continue 'requests;
                    }
                }
            }
            200 => {
                // 节点级 ETag 差异（CDN 各节点 ETag 不同但 Last-Modified 与总长一致）：
                // 放弃 ETag 改用 Last-Modified 重试，不误降级为单连接（参照 MDM per_node_etag）
                let resp_len = resp
                    .headers()
                    .get(reqwest::header::CONTENT_LENGTH)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok());
                let resp_lm = resp
                    .headers()
                    .get(reqwest::header::LAST_MODIFIED)
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string());
                let sent_strong_etag = sent_if_range.as_deref().is_some_and(|v| v.starts_with('"'));
                let per_node_ok = {
                    let mut vs = shared.validators.lock().unwrap();
                    if sent_strong_etag
                        && primary_total.is_some()
                        && resp_len == primary_total
                        && vs.last_modified.is_some()
                        && resp_lm == vs.last_modified
                    {
                        vs.etag = None;
                        true
                    } else {
                        false
                    }
                };
                if per_node_ok {
                    drop(resp);
                    tracing::debug!("per-node ETag mismatch on CDN; falling back to Last-Modified");
                    continue 'requests;
                }

                flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, wtx).await?;
                if mirrors.len() > 1 {
                    shared.bad_mirrors.lock().unwrap().insert(midx);
                    let remaining_end = stop_at.load(Ordering::Acquire);
                    let all_bad = shared.bad_mirrors.lock().unwrap().len() >= mirrors.len();
                    if all_bad {
                        shared.no_range_fallback.store(true, Ordering::Relaxed);
                        planner.notify_all();
                        return Ok(StreamOutcome::Aborted);
                    }
                    if remaining_end > cursor {
                        guard.disarm();
                        planner.requeue_and_finish(
                            guard.id,
                            Segment {
                                start: cursor,
                                end: remaining_end,
                                written: 0,
                            },
                        );
                    }
                    return Ok(StreamOutcome::Aborted);
                }
                shared.no_range_fallback.store(true, Ordering::Relaxed);
                planner.notify_all();
                return Ok(StreamOutcome::Aborted);
            }
            416 => {
                flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, wtx).await?;
                if cursor >= stop_at.load(Ordering::Acquire) {
                    return Ok(StreamOutcome::Done(stop_at.load(Ordering::Acquire)));
                }
                return Err(ErrorKind::RangeUnsupported);
            }
            s => {
                let other_recv = others_receiving();
                if matches!(s, 403 | 429 | 503)
                    && !receiving.load(Ordering::Acquire)
                    && other_recv > 0
                    && mirrors.len() == 1
                {
                    if let Some(cap) = ctx.engine.host_limits().refused(&host, other_recv) {
                        if planner.active_count() > cap {
                            flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, wtx)
                                .await?;
                            let remaining_end = stop_at.load(Ordering::Acquire);
                            guard.disarm();
                            planner.requeue_and_finish(
                                guard.id,
                                Segment {
                                    start: cursor,
                                    end: remaining_end,
                                    written: 0,
                                },
                            );
                            return Ok(StreamOutcome::Retired);
                        }
                    }
                }
                match classify_status(s) {
                    Some(kind) if kind.is_retryable() => {
                        let ra = retry_after_secs(resp.headers().get(reqwest::header::RETRY_AFTER));
                        attempts += 1;
                        drop(resp);
                        let wait =
                            Duration::from_secs(ra.unwrap_or(0)).saturating_add(backoff(attempts));
                        if !sleep_or_cancel(&ctx.cancel, wait).await {
                            flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, wtx)
                                .await?;
                            return Ok(StreamOutcome::Aborted);
                        }
                        continue 'requests;
                    }
                    Some(kind) => {
                        flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, wtx)
                            .await?;
                        if other_recv > 0 {
                            let remaining_end = stop_at.load(Ordering::Acquire);
                            guard.disarm();
                            planner.requeue_and_finish(
                                guard.id,
                                Segment {
                                    start: cursor,
                                    end: remaining_end,
                                    written: 0,
                                },
                            );
                            return Ok(StreamOutcome::Retired);
                        }
                        return Err(kind);
                    }
                    None => {
                        attempts += 1;
                        drop(resp);
                        if !sleep_or_cancel(&ctx.cancel, backoff(attempts)).await {
                            flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, wtx)
                                .await?;
                            return Ok(StreamOutcome::Aborted);
                        }
                        continue 'requests;
                    }
                }
            }
        }

        let mut resp = resp;
        if let Some(ce) = resp
            .headers()
            .get(reqwest::header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok())
        {
            if !ce.eq_ignore_ascii_case("identity") {
                flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, wtx).await?;
                return Err(ErrorKind::UnsupportedEncoding);
            }
        }

        let mut pace = Pace::new();
        let mut last_byte_at = Instant::now();
        loop {
            if preempted.load(Ordering::Acquire) {
                return Ok(StreamOutcome::Preempted);
            }
            let effective_end = stop_at.load(Ordering::Acquire);
            if cursor >= effective_end {
                flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, wtx).await?;
                flushed_cursor.store(cursor, Ordering::Release);
                return Ok(StreamOutcome::Done(effective_end));
            }
            let wait_cap = if !batch_buf.is_empty() {
                BATCH_FLUSH_EVERY
            } else {
                STALL_TIMEOUT
            };
            let chunk = tokio::select! {
                _ = ctx.cancel.cancelled() => {
                    flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, wtx).await?;
                    flushed_cursor.store(cursor, Ordering::Release);
                    return Ok(StreamOutcome::Aborted);
                }
                r = tokio::time::timeout(wait_cap, resp.chunk()) => match r {
                    Err(_) => {
                        if !batch_buf.is_empty() {
                            if !preempted.load(Ordering::Acquire) {
                                flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, wtx)
                                    .await?;
                                flushed_cursor.store(cursor, Ordering::Release);
                            }
                            if last_byte_at.elapsed() < STALL_TIMEOUT {
                                continue;
                            }
                        }
                        None
                    }
                    Ok(Err(_)) => None,
                    Ok(Ok(None)) => {
                        let end_now = stop_at.load(Ordering::Acquire);
                        if cursor >= end_now {
                            flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, wtx).await?;
                            flushed_cursor.store(cursor, Ordering::Release);
                            return Ok(StreamOutcome::Done(end_now));
                        }
                        None
                    }
                    Ok(Ok(Some(b))) => Some(b),
                }
            };
            if preempted.load(Ordering::Acquire) {
                return Ok(StreamOutcome::Preempted);
            }
            let Some(bytes) = chunk else {
                flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, wtx).await?;
                flushed_cursor.store(cursor, Ordering::Release);
                attempts += 1;
                if mirrors.len() > 1 {
                    record_mirror_failure(shared, midx);
                    midx = pick_mirror_excluding(shared, mirrors, Some(midx));
                }
                continue 'requests;
            };
            if !bytes.is_empty() {
                attempts = 0;
                last_byte_at = Instant::now();
                last_progress_ms.store(planner.now_ms(), Ordering::Release);
                if !receiving.swap(true, Ordering::AcqRel) && mirrors.len() == 1 {
                    ctx.engine
                        .host_limits()
                        .working(&host, planner.receiving_count());
                }
                shared
                    .received_bytes
                    .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                pace.received += bytes.len() as u64;
                note_best(shared, &pace);
                if mirrors.len() > 1 && pace.elapsed() >= BEST_AFTER {
                    record_mirror_speed(shared, midx, pace.speed());
                }
            }
            let effective_end = stop_at.load(Ordering::Acquire);
            if cursor >= effective_end {
                flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, wtx).await?;
                flushed_cursor.store(cursor, Ordering::Release);
                return Ok(StreamOutcome::Done(effective_end));
            }
            let usable = bytes.len().min((effective_end - cursor) as usize);
            let mut rest: &[u8] = &bytes[..usable];
            while !rest.is_empty() {
                if preempted.load(Ordering::Acquire) {
                    return Ok(StreamOutcome::Preempted);
                }
                if ctx.cancel.is_cancelled() {
                    flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, wtx).await?;
                    flushed_cursor.store(cursor, Ordering::Release);
                    return Ok(StreamOutcome::Aborted);
                }
                let effective_end = stop_at.load(Ordering::Acquire);
                if cursor >= effective_end {
                    flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, wtx).await?;
                    flushed_cursor.store(cursor, Ordering::Release);
                    return Ok(StreamOutcome::Done(effective_end));
                }
                let n = rest
                    .len()
                    .min(Limiter::ACQUIRE_CAP as usize)
                    .min((effective_end - cursor) as usize);
                if n == 0 {
                    flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, wtx).await?;
                    flushed_cursor.store(cursor, Ordering::Release);
                    return Ok(StreamOutcome::Done(effective_end));
                }
                ctx.limiter.acquire(n as u64).await;
                let mut slice = &rest[..n];
                while !slice.is_empty() {
                    if batch_buf.is_empty() {
                        batch_start = cursor;
                        batch_opened = Instant::now();
                    }
                    let take = slice.len().min(WRITE_BATCH - batch_buf.len());
                    batch_buf.extend_from_slice(&slice[..take]);
                    cursor += take as u64;
                    active_cursor.store(cursor, Ordering::Release);
                    slice = &slice[take..];
                    if batch_buf.len() >= WRITE_BATCH || batch_opened.elapsed() >= BATCH_FLUSH_EVERY
                    {
                        if preempted.load(Ordering::Acquire) {
                            return Ok(StreamOutcome::Preempted);
                        }
                        flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, wtx)
                            .await?;
                        flushed_cursor.store(cursor, Ordering::Release);
                    }
                }
                rest = &rest[n..];
            }
            let end_now = stop_at.load(Ordering::Acquire);
            if cursor >= end_now {
                flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, wtx).await?;
                flushed_cursor.store(cursor, Ordering::Release);
                return Ok(StreamOutcome::Done(end_now));
            }

            // MDM 慢连接淘汰：连接稳定运行 >= 2s 且远低于公平带宽份额、且剩余耗时 > 2s 时主动重连/切最快镜像
            if !rate_limited {
                let best_one = shared.best_speed.load(Ordering::Relaxed);
                let link_best = shared.link_meter.lock().unwrap().best;
                let recv_cnt = planner.receiving_count();
                let bar = shed_bar(best_one, link_best, recv_cnt);
                let left = end_now.saturating_sub(cursor);
                if too_slow(pace.speed(), pace.elapsed(), bar, left) {
                    tracing::debug!(
                        speed = pace.speed(),
                        bar,
                        left,
                        "shedding slow connection to reconnect"
                    );
                    flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, wtx).await?;
                    flushed_cursor.store(cursor, Ordering::Release);
                    drop(resp);
                    if mirrors.len() > 1 {
                        midx = pick_mirror_excluding(shared, mirrors, Some(midx));
                    }
                    continue 'requests;
                }
            }
        }
    }
}

async fn download_sequential(
    ctx: Arc<Ctx>,
    shared: Arc<Shared>,
    paths: &TargetPaths,
    target_url: &str,
    total: Option<u64>,
) -> Result<PhaseOutcome, DriveErr> {
    let mut restarts: u32 = 0;
    loop {
        let (writer, all_rx) = dl_io::spawn(SpawnParams {
            path: paths.part.clone(),
            mode: Mode::Sequential,
            segments: vec![Segment {
                start: 0,
                end: u64::MAX,
                written: 0,
            }],
            journal: None,
        })
        .await
        .map_err(|e| DriveErr::Fatal(classify_io(&e), format!("open part file: {e}")))?;

        {
            let ectx = ctx.clone();
            let eshared = shared.clone();
            let progress = writer.progress();
            tokio::spawn(emit_progress(ectx, eshared, progress, total));
        }

        let result = stream_whole(&ctx, target_url, total, writer.tx()).await;
        match result {
            Err(kind) => {
                let _ = writer.finish().await;
                return Ok(PhaseOutcome::Fatal(
                    kind,
                    "sequential download failed".into(),
                ));
            }
            Ok(StreamOutcome::Aborted | StreamOutcome::Preempted | StreamOutcome::Retired) => {
                let _ = writer.finish().await;
                restarts += 1;
                if restarts > 2 {
                    return Ok(PhaseOutcome::Fatal(
                        ErrorKind::Internal,
                        "server stopped honoring ranges during sequential download".into(),
                    ));
                }
                let _ = std::fs::remove_file(&paths.part);
                continue;
            }
            Ok(StreamOutcome::Done(_)) => {
                if ctx.cancel.is_cancelled() {
                    let _ = writer.finish().await;
                    return Ok(PhaseOutcome::Paused);
                }
                return match all_rx.await {
                    Ok(Ok(_)) => {
                        let segs = writer.finish().await.map_err(|e| {
                            DriveErr::Fatal(classify_io(&e), format!("finalize write: {e}"))
                        })?;
                        Ok(PhaseOutcome::Completed(segs))
                    }
                    Ok(Err(e)) => Err(DriveErr::Fatal(
                        classify_io(&e),
                        format!("write failure: {e}"),
                    )),
                    Err(_) => Err(DriveErr::Fatal(
                        ErrorKind::Internal,
                        "sequential download ended without completion".into(),
                    )),
                };
            }
        }
    }
}

async fn stream_whole(
    ctx: &Ctx,
    target_url: &str,
    total: Option<u64>,
    wtx: mpsc::Sender<Msg>,
) -> Result<StreamOutcome, ErrorKind> {
    let mut attempts: u32 = 0;
    let mut cursor = 0u64;
    let mut batch_buf: Vec<u8> = Vec::with_capacity(WRITE_BATCH);
    let mut batch_start = 0u64;
    let mut batch_opened = Instant::now();
    'requests: loop {
        if ctx.cancel.is_cancelled() {
            flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, &wtx).await?;
            return Ok(StreamOutcome::Done(cursor));
        }
        if attempts >= MAX_ATTEMPTS {
            flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, &wtx).await?;
            return Err(ErrorKind::NetworkConnectionReset);
        }
        let mut req = with_task_headers(
            identity_headers(ctx.client.get(target_url)),
            &ctx.task_headers,
        );
        if cursor > 0 {
            req = req.header("Range", format!("bytes={cursor}-"));
        }
        let response = tokio::select! {
            _ = ctx.cancel.cancelled() => {
                flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, &wtx).await?;
                return Ok(StreamOutcome::Done(cursor));
            }
            result = req.send() => result,
        };
        let resp = match response {
            Ok(r) => r,
            Err(_) => {
                attempts += 1;
                if !sleep_or_cancel(&ctx.cancel, backoff(attempts)).await {
                    flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, &wtx).await?;
                    return Ok(StreamOutcome::Done(cursor));
                }
                continue 'requests;
            }
        };
        match resp.status().as_u16() {
            200 => {
                if cursor > 0 {
                    flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, &wtx).await?;
                    return Ok(StreamOutcome::Aborted);
                }
            }
            206 => {}
            s => match classify_status(s) {
                Some(kind) if kind.is_retryable() => {
                    attempts += 1;
                    if !sleep_or_cancel(&ctx.cancel, backoff(attempts)).await {
                        flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, &wtx)
                            .await?;
                        return Ok(StreamOutcome::Done(cursor));
                    }
                    continue 'requests;
                }
                Some(kind) => {
                    flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, &wtx).await?;
                    return Err(kind);
                }
                None => {
                    attempts += 1;
                    if !sleep_or_cancel(&ctx.cancel, backoff(attempts)).await {
                        flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, &wtx)
                            .await?;
                        return Ok(StreamOutcome::Done(cursor));
                    }
                    continue 'requests;
                }
            },
        }
        let mut resp = resp;
        if let Some(ce) = resp
            .headers()
            .get(reqwest::header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok())
        {
            if !ce.eq_ignore_ascii_case("identity") {
                flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, &wtx).await?;
                return Err(ErrorKind::UnsupportedEncoding);
            }
        }
        loop {
            let chunk = tokio::select! {
                _ = ctx.cancel.cancelled() => {
                    flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, &wtx).await?;
                    return Ok(StreamOutcome::Done(cursor));
                }
                r = tokio::time::timeout(STALL_TIMEOUT, resp.chunk()) => match r {
                    Err(_) => None,
                    Ok(Err(_)) => None,
                    Ok(Ok(None)) => {
                        flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, &wtx).await?;
                        if total.is_some_and(|t| cursor < t) {
                            attempts += 1;
                            if !sleep_or_cancel(&ctx.cancel, backoff(attempts)).await {
                                return Ok(StreamOutcome::Done(cursor));
                            }
                            continue 'requests;
                        }
                        if wtx.send(Msg::Eof).await.is_err() {
                            return Err(ErrorKind::Internal);
                        }
                        return Ok(StreamOutcome::Done(cursor));
                    }
                    Ok(Ok(Some(b))) => Some(b),
                }
            };
            let Some(bytes) = chunk else {
                flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, &wtx).await?;
                attempts += 1;
                continue 'requests;
            };
            if !bytes.is_empty() {
                attempts = 0;
            }
            let mut rest: &[u8] = &bytes;
            while !rest.is_empty() {
                if ctx.cancel.is_cancelled() {
                    flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, &wtx).await?;
                    return Ok(StreamOutcome::Done(cursor));
                }
                let n = rest.len().min(Limiter::ACQUIRE_CAP as usize);
                ctx.limiter.acquire(n as u64).await;
                let mut slice = &rest[..n];
                while !slice.is_empty() {
                    if batch_buf.is_empty() {
                        batch_start = cursor;
                        batch_opened = Instant::now();
                    }
                    let take = slice.len().min(WRITE_BATCH - batch_buf.len());
                    batch_buf.extend_from_slice(&slice[..take]);
                    cursor += take as u64;
                    slice = &slice[take..];
                    if batch_buf.len() >= WRITE_BATCH || batch_opened.elapsed() >= BATCH_FLUSH_EVERY
                    {
                        flush_batch(&mut batch_buf, &mut batch_start, &mut batch_opened, &wtx)
                            .await?;
                    }
                }
                rest = &rest[n..];
            }
        }
    }
}

/// 无强校验器或热换链重连时，断点续传前比对盘首与盘尾最多 4KiB，防止静默文件变更（MDM verify_overlap）。
async fn compare_remote_range(
    ctx: &Ctx,
    part: &Path,
    final_url: &str,
    start: u64,
    len: u64,
) -> bool {
    if len == 0 {
        return true;
    }
    let Ok(resp) = with_task_headers(
        identity_headers(ctx.client.get(final_url))
            .header("Range", format!("bytes={start}-{}", start + len - 1)),
        &ctx.task_headers,
    )
    .send()
    .await
    else {
        return true;
    };
    if resp.status().as_u16() != 206 {
        return true;
    }
    let Ok(remote) = resp.bytes().await else {
        return true;
    };
    if remote.len() != len as usize {
        return false;
    }
    let mut local = vec![0u8; len as usize];
    let Ok(mut f) = std::fs::File::open(part) else {
        return false;
    };
    use std::io::{Read, Seek, SeekFrom};
    if f.seek(SeekFrom::Start(start)).is_err() || f.read_exact(&mut local).is_err() {
        return false;
    }
    local == remote.as_ref()
}

async fn verify_resume_overlap(ctx: &Ctx, part: &Path, final_url: &str, segs: &[Segment]) -> bool {
    let Some(seg) = segs.iter().find(|s| s.written > 0) else {
        return true;
    };
    let mut windows: Vec<(u64, u64)> = Vec::with_capacity(2);
    if segs[0].written > 0 {
        windows.push((0, segs[0].written.min(RESUME_OVERLAP_BYTES)));
    }
    let tail_len = seg.written.min(RESUME_OVERLAP_BYTES);
    let tail_start = seg.cursor() - tail_len;
    if !windows.contains(&(tail_start, tail_len)) {
        windows.push((tail_start, tail_len));
    }
    for (start, len) in windows {
        if !compare_remote_range(ctx, part, final_url, start, len).await {
            return false;
        }
    }
    true
}

fn looks_like_disguised_html(part: &Path, url: &str, final_url: &str, filename: &str) -> bool {
    let lower_name = filename.to_ascii_lowercase();
    if lower_name.ends_with(".html")
        || lower_name.ends_with(".htm")
        || dl_net::interstitial::url_expects_html(url)
        || dl_net::interstitial::url_expects_html(final_url)
    {
        return false;
    }
    let Ok(mut f) = std::fs::File::open(part) else {
        return false;
    };
    use std::io::Read;
    let mut buf = [0u8; 512];
    let n = f.read(&mut buf).unwrap_or(0);
    if n == 0 {
        return false;
    }
    let head = String::from_utf8_lossy(&buf[..n]);
    let trimmed = head.trim_start_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("<!doctype html")
        || lower.starts_with("<html")
        || lower.starts_with("<head")
        || lower.starts_with("<script")
        || lower.starts_with("<body")
}

/// 完成前强制校验集（DESIGN §5.3）：长度、伪装 HTML 检测、覆盖完整性、头尾与随机抽检。
async fn verify(
    ctx: &Ctx,
    paths: &TargetPaths,
    final_segs: &[Segment],
    final_url: &str,
    total: Option<u64>,
    segmented: bool,
) -> Result<(), DriveErr> {
    let meta = std::fs::metadata(&paths.part)
        .map_err(|e| DriveErr::Fatal(classify_io(&e), format!("verify: stat part file: {e}")))?;
    if let Some(t) = total {
        if meta.len() != t {
            return Err(DriveErr::Fatal(
                ErrorKind::Internal,
                format!("length mismatch: file {} bytes, expected {t}", meta.len()),
            ));
        }
    }
    if looks_like_disguised_html(&paths.part, &ctx.url, final_url, &paths.name) {
        return Err(DriveErr::Fatal(
            ErrorKind::InterstitialPage {
                url: final_url.to_string(),
                title: None,
            },
            "downloaded content is an HTML error or interstitial page".into(),
        ));
    }
    if segmented {
        let t = total.unwrap_or(0);
        if !dl_segment::full_coverage(final_segs, t) {
            return Err(DriveErr::Fatal(
                ErrorKind::Internal,
                "coverage check failed: segments do not exactly cover the file".into(),
            ));
        }
        if ctx.opts.spot_checks > 0 && t > 0 {
            let len = 4096u64.min(t);
            let mut offsets: Vec<u64> = vec![0];
            if t > len {
                offsets.push(t - len);
            }
            for i in 0..ctx.opts.spot_checks.saturating_sub(2) {
                let seed = xxhash_rust::xxh64::xxh64(
                    &std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos()
                        .to_le_bytes(),
                    i as u64,
                );
                let off = seed % (t - len + 1);
                if !offsets.contains(&off) {
                    offsets.push(off);
                }
            }
            for off in offsets {
                match spot_check(ctx, &paths.part, final_url, off, len).await {
                    Ok(()) => {}
                    Err(e) if e.kind == ErrorKind::Internal => {
                        tracing::warn!(
                            task = %ctx.task_id,
                            offset = off,
                            error = %e,
                            "spot check byte mismatch; restarting download"
                        );
                        let _ = ctx
                            .engine
                            .transition(
                                &ctx.task_id,
                                TaskStatus::WaitingRetry,
                                Some(ErrorKind::Internal),
                            )
                            .await;
                        cleanup_partial(paths);
                        return Err(DriveErr::Restart);
                    }
                    Err(e) => {
                        tracing::warn!(offset = off, error = %e, "spot check network error ignored");
                    }
                }
            }
        }
    }
    Ok(())
}

async fn spot_check(ctx: &Ctx, part: &Path, final_url: &str, off: u64, len: u64) -> Res<()> {
    let resp = with_task_headers(
        identity_headers(ctx.client.get(final_url))
            .header("Range", format!("bytes={off}-{}", off + len - 1)),
        &ctx.task_headers,
    )
    .send()
    .await
    .map_err(|e| DownloadError::new(map_reqwest_err(&e), e.to_string()))?;
    if resp.status().as_u16() != 206 {
        return Ok(());
    }
    let remote = resp
        .bytes()
        .await
        .map_err(|e| DownloadError::new(map_reqwest_err(&e), e.to_string()))?;
    let mut local = vec![0u8; remote.len()];
    let mut f = std::fs::File::open(part).map_err(io_dl)?;
    use std::io::{Read, Seek, SeekFrom};
    f.seek(SeekFrom::Start(off)).map_err(io_dl)?;
    f.read_exact(&mut local).map_err(io_dl)?;
    if local != remote.as_ref() {
        return Err(DownloadError::new(
            ErrorKind::Internal,
            "on-disk bytes differ from server bytes",
        ));
    }
    Ok(())
}

pub(crate) async fn finalize(ctx: &Ctx, paths: &TargetPaths) -> Result<(), DriveErr> {
    let target = resolve_conflict(&paths.dir, &paths.name, ctx.opts.conflict)
        .map_err(|e| DriveErr::Fatal(e.kind, e.message))?;
    let Some(target) = target else {
        let _ = std::fs::remove_file(&paths.part);
        Journal::remove(&paths.journal);
        return Ok(());
    };
    if target.exists() {
        let mut removed = false;
        for _ in 0..3 {
            if std::fs::remove_file(&target).is_ok() {
                removed = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        if !removed {
            return Err(DriveErr::Fatal(
                ErrorKind::FileAccessDenied,
                format!("cannot replace existing file {}", target.display()),
            ));
        }
    }
    let mut renamed = false;
    let mut last_err = None;
    for _ in 0..3 {
        match std::fs::rename(&paths.part, &target) {
            Ok(()) => {
                renamed = true;
                break;
            }
            Err(e) => {
                last_err = Some(e);
                std::thread::sleep(Duration::from_millis(150));
            }
        }
    }
    if !renamed {
        let e = last_err.unwrap();
        return Err(DriveErr::Fatal(
            classify_io(&e),
            format!(
                "rename {} -> {}: {e}",
                paths.part.display(),
                target.display()
            ),
        ));
    }
    let size = std::fs::metadata(&target).ok().map(|m| m.len());
    let name = target.file_name().unwrap().to_string_lossy();
    ctx.engine
        .record_output(&ctx.task_id, &paths.dir, &name, size)
        .await
        .map_err(drive_from)?;
    Journal::remove(&paths.journal);
    Ok(())
}

fn cleanup_partial(paths: &TargetPaths) {
    Journal::remove(&paths.journal);
    let _ = std::fs::remove_file(&paths.part);
}

fn io_dl(e: std::io::Error) -> DownloadError {
    let kind = classify_io(&e);
    DownloadError::new(kind, e.to_string())
}

fn classify_io(e: &std::io::Error) -> ErrorKind {
    match e.kind() {
        IoErrorKind::StorageFull | IoErrorKind::FileTooLarge => ErrorKind::DiskFull,
        IoErrorKind::PermissionDenied => ErrorKind::FileAccessDenied,
        IoErrorKind::NotFound => ErrorKind::FileAccessDenied,
        IoErrorKind::InvalidFilename | IoErrorKind::InvalidInput => ErrorKind::FileNameInvalid,
        IoErrorKind::InvalidData => ErrorKind::Internal,
        _ => ErrorKind::Internal,
    }
}

fn parse_content_range(v: &str) -> Option<(u64, Option<u64>)> {
    let spec = v.trim().strip_prefix("bytes ")?;
    let (range_part, total_part) = spec.split_once('/')?;
    let (start_part, _) = range_part.split_once('-')?;
    let start = start_part.trim().parse::<u64>().ok()?;
    let total = if total_part.trim() == "*" {
        None
    } else {
        Some(total_part.trim().parse::<u64>().ok()?)
    };
    Some((start, total))
}

fn backoff(attempts: u32) -> Duration {
    let exp = attempts.saturating_sub(1).min(5);
    Duration::from_secs(1u64 << exp).min(BACKOFF_MAX)
}

pub(crate) async fn check_disk_space(dir: &Path, need_bytes: u64) -> Result<(), DriveErr> {
    const MARGIN: u64 = 64 * 1024 * 1024;
    let check_dir = dir.to_path_buf();
    if let Ok(Ok(Ok(avail))) = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::task::spawn_blocking(move || fs4::available_space(&check_dir)),
    )
    .await
    {
        if avail < need_bytes.saturating_add(MARGIN) {
            return Err(DriveErr::Fatal(
                ErrorKind::DiskFull,
                format!(
                    "磁盘空间不足：需要 ≥ {}（含余量），可用 {}",
                    humansize(need_bytes.saturating_add(MARGIN)),
                    humansize(avail)
                ),
            ));
        }
    }
    Ok(())
}

pub(crate) fn move_file_or_copy(src: &Path, dst: &Path) -> std::io::Result<()> {
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(rename_err) => {
            // 跨磁盘/跨挂载点（如 dl-tmp 与下载目录不在同一分区）回退为缓冲拷贝 + 落盘
            let is_exdev = rename_err.kind() == IoErrorKind::CrossesDevices
                || matches!(rename_err.raw_os_error(), Some(17) | Some(18));
            if !is_exdev {
                return Err(rename_err);
            }
            let tmp_dst = dst.with_extension("dlmove");
            {
                let mut reader =
                    std::io::BufReader::with_capacity(256 * 1024, std::fs::File::open(src)?);
                let mut writer =
                    std::io::BufWriter::with_capacity(256 * 1024, std::fs::File::create(&tmp_dst)?);
                std::io::copy(&mut reader, &mut writer)?;
                use std::io::Write;
                writer.flush()?;
                writer.get_ref().sync_data()?;
            }
            std::fs::rename(&tmp_dst, dst)?;
            let _ = std::fs::remove_file(src);
            Ok(())
        }
    }
}

pub(crate) async fn sleep_or_cancel(cancel: &CancellationToken, d: Duration) -> bool {
    if d <= Duration::ZERO {
        return !cancel.is_cancelled();
    }
    tokio::select! {
        _ = cancel.cancelled() => false,
        _ = tokio::time::sleep(d) => true,
    }
}

fn humansize(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.1}{}", U[i])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shed_bar_shares_link_fairly_across_receiving_workers() {
        // 1 connection measured 8 MB/s (`one`), 8 connections share an 8 MB/s link (`link`)
        let bar = shed_bar(8 * 1024 * 1024, 8 * 1024 * 1024, 8);
        assert_eq!(bar, 1024 * 1024);
        // Fair share ~1 MB/s must NOT shed against bar 1 MB/s
        assert!(!too_slow(
            1024 * 1024,
            Duration::from_secs(3),
            bar,
            10 * 1024 * 1024
        ));
        // Straggler < 1/4 of bar (< 256 KB/s) with > 2s left MUST shed
        assert!(too_slow(
            200 * 1024,
            Duration::from_secs(3),
            bar,
            10 * 1024 * 1024
        ));
        // Short tail (< 2s remaining) must NOT shed
        assert!(!too_slow(
            200 * 1024,
            Duration::from_secs(3),
            bar,
            100 * 1024
        ));
        // Before grace period (< 2s) must NOT shed
        assert!(!too_slow(
            50 * 1024,
            Duration::from_secs(1),
            bar,
            10 * 1024 * 1024
        ));
    }

    #[test]
    fn planner_preempts_stalled_straggler_from_flushed_cursor() {
        let mut planner = SegmentPlanner::new(
            vec![Segment {
                start: 0,
                end: 100_000,
                written: 0,
            }],
            1,
        );
        let WorkItem::Assigned {
            id: first_id,
            flushed_cursor,
            last_progress_ms,
            preempted,
            stop_at,
            ..
        } = planner.next_work(None, 256 * 1024, Duration::from_millis(200))
        else {
            panic!("expected first assignment");
        };
        // 模拟前 40 KiB 已安全 flush，随后卡住 5 秒（相对 planner 启动时钟推进 5s）
        flushed_cursor.store(40_960, Ordering::Release);
        planner.started_at = Instant::now() - Duration::from_secs(5);
        {
            let mut g = planner.inner.lock().unwrap();
            let entry = g.active.get_mut(&first_id).unwrap();
            entry.assigned_at = Instant::now() - Duration::from_secs(5);
        }
        last_progress_ms.store(0, Ordering::Release);

        // 第 2 个空闲 worker 进场：因总剩余仅 100KB < 2 * 256KB 无法二分窃取，应触发 Step 4 抢占
        match planner.next_work(None, 256 * 1024, Duration::from_millis(200)) {
            WorkItem::Assigned { seg, split, .. } => {
                assert_eq!(seg.start, 40_960);
                assert_eq!(seg.end, 100_000);
                assert_eq!(split, Some((100_000, 40_960)));
                assert!(preempted.load(Ordering::Acquire));
                assert_eq!(stop_at.load(Ordering::Acquire), 40_960);
            }
            _ => panic!("expected straggler preemption"),
        }
    }

    #[test]
    fn pick_mirror_prefers_faster_healthy_mirror() {
        let shared = Shared::new(None, None);
        let mirrors = vec![
            "https://m0.example/f".into(),
            "https://m1.example/f".into(),
            "https://m2.example/f".into(),
        ];
        // 采样每个镜像测速
        assert_eq!(pick_mirror(&shared, &mirrors), 0);
        record_mirror_speed(&shared, 0, 500_000);
        assert_eq!(pick_mirror(&shared, &mirrors), 1);
        record_mirror_speed(&shared, 1, 5_000_000);
        assert_eq!(pick_mirror(&shared, &mirrors), 2);
        record_mirror_speed(&shared, 2, 1_200_000);

        // 所有镜像都有测速后应优先选中最快健康源 1
        assert_eq!(pick_mirror(&shared, &mirrors), 1);
        // 排除 1 时次优应选 2
        assert_eq!(pick_mirror_excluding(&shared, &mirrors, Some(1)), 2);
    }
}
