//! HTTP 直链下载驱动（DESIGN §5.3/§5.5/§5.6）。
//!
//! 流程：探测决策表 → journal 恢复/分段规划 → 单写者线程 + N 个分段 worker
//! → 强制验证（覆盖/长度/抽检）→ `.dlpart` 原子 rename 收尾。
//! 暂停/取消经 CancellationToken 协作；kill -9 靠 journal 恢复。

use std::collections::{HashMap, VecDeque};
use std::io::ErrorKind as IoErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use crate::engine::{Engine, EngineEvent};
use dl_error::{DownloadError, ErrorKind, Res};
use dl_filename::{pick_name, resolve_conflict, ConflictPolicy};
use dl_io::{Mode, Msg, SpawnParams};
use dl_journal::{self, Journal};
use dl_model::{Task, TaskStatus};
use dl_net::probe::{self, ProbeResult};
use dl_net::{build_client, classify_status, identity_headers, map_reqwest_err, retry_after_secs};
use dl_segment::{self, Segment};
use dl_throttle::Limiter;

pub const MAX_SEGMENTS: usize = 32;
/// 每段连续失败上限；超过则任务失败（错误分类保留最后一次）。
const MAX_ATTEMPTS: u32 = 8;
const STALL_TIMEOUT: Duration = Duration::from_secs(30);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// 慢段分裂护栏（DESIGN §5.3）：冷启动不评估、最小半段 4MB、单段最多分裂 3 次。
/// 空闲窃取门槛：剩余 ≥ 16MB 的活跃段才值得窃取（避免小段反复交还）。
const STEAL_MIN_REMAINING: u64 = 16 * 1024 * 1024;
const SPLIT_MIN_TASK_BYTES: u64 = 32 * 1024 * 1024;
const SPLIT_MIN_TASK_SECS: u64 = 10;
const SPLIT_MIN_HALF: u64 = 4 * 1024 * 1024;
const SPLIT_MAX_PER_RANGE: u32 = 3;
const SPLIT_WINDOW: Duration = Duration::from_secs(5);

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

struct Shared {
    no_range_fallback: AtomicBool,
    fatal: Mutex<Option<ErrorKind>>,
    task_downloaded: AtomicU64,
    aggregate_bps: AtomicU64,
    active_workers: AtomicUsize,
    started: Instant,
    /// 活跃段注册表：claim 起点 -> (段终点, 当前 cursor)。IDM 式动态分段：
    /// worker 提前完成时，空闲 worker 据此窃取最大活跃段的一半。
    active: Mutex<HashMap<u64, (u64, u64)>>,
    /// 镜像轮转游标（分段按轮转分派到各镜像）
    mirror_cursor: AtomicU64,
    /// 失效镜像下标集合（200=不支持 Range 的镜像）
    bad_mirrors: Mutex<std::collections::HashSet<usize>>,
    /// 强制交还标记：key = 段 claim 起点。被窃取段的 owner 在下一块前中止并交还剩余区间。
    force_requeue: Mutex<std::collections::HashSet<u64>>,
}

impl Shared {
    fn new() -> Self {
        Shared {
            no_range_fallback: AtomicBool::new(false),
            fatal: Mutex::new(None),
            task_downloaded: AtomicU64::new(0),
            aggregate_bps: AtomicU64::new(0),
            active_workers: AtomicUsize::new(0),
            started: Instant::now(),
            active: Mutex::new(HashMap::new()),
            force_requeue: Mutex::new(std::collections::HashSet::new()),
            mirror_cursor: AtomicU64::new(0),
            bad_mirrors: Mutex::new(std::collections::HashSet::new()),
        }
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

    fn active_insert(&self, start: u64, end: u64, cursor: u64) {
        self.active.lock().unwrap().insert(start, (end, cursor));
    }

    fn active_remove(&self, start: u64) {
        self.active.lock().unwrap().remove(&start);
    }

    fn active_update_cursor(&self, start: u64, cursor: u64) {
        if let Some(e) = self.active.lock().unwrap().get_mut(&start) {
            e.1 = cursor;
        }
    }

    /// 剩余最大的活跃段（≥ min_remaining 才值得窃取）。
    fn largest_active(&self, min_remaining: u64) -> Option<(u64, u64)> {
        self.active
            .lock()
            .unwrap()
            .iter()
            .map(|(s, (e, c))| (*s, *e, e.saturating_sub(*c)))
            .filter(|(_, _, rem)| *rem >= min_remaining)
            .max_by_key(|(_, _, rem)| *rem)
            .map(|(s, e, _)| (s, e))
    }

    /// 标记强制交还；返回是否成功标记（避免重复标记）。
    fn mark_force_requeue(&self, start: u64) -> bool {
        self.force_requeue.lock().unwrap().insert(start)
    }

    fn take_force_requeue(&self, start: u64) -> bool {
        self.force_requeue.lock().unwrap().remove(&start)
    }
}

/// 领取条目：seg 为待下载区间，splits_done 是该区间已经历的分裂代数
/// （护栏「单段分裂 ≤3 次」须跨分裂代传递）。
#[derive(Clone, Copy)]
struct Claim {
    seg: Segment,
    splits_done: u32,
}

#[derive(Default)]
struct Queue(Mutex<VecDeque<Claim>>);

impl Queue {
    fn new(ranges: Vec<Segment>) -> Self {
        Queue(Mutex::new(
            ranges
                .into_iter()
                .map(|seg| Claim {
                    seg,
                    splits_done: 0,
                })
                .collect(),
        ))
    }

    fn claim(&self) -> Option<Claim> {
        self.0.lock().unwrap().pop_front()
    }

    fn push(&self, c: Claim) {
        self.0.lock().unwrap().push_back(c);
    }
}

enum StreamOutcome {
    Done,
    Aborted,
}

pub(crate) async fn run(
    engine: &Engine,
    task_id: &str,
    opts: DownloadOptions,
    cancel: CancellationToken,
    cancel_requested: Arc<std::sync::atomic::AtomicBool>,
) -> Res<Task> {
    let task = engine.require(task_id).await?;
    // Downloading 入口 = 进程崩溃（kill -9）后的残留状态：journal 恢复续传（DESIGN §5.5）；
    // WaitingRetry 入口 = 自动重试退避期间应用重启的残留：等价重新入队（→探测）
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
    let limiter = Limiter::new(opts.global_bps, opts.task_bps);
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

    // 多镜像原始清单解析（探测校验在 phase 内、主源 total 已知之后）
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
        // Queued：新任务；WaitingRetry：退避残留；Paused：恢复前重新探测
        //（迁移表放行 Paused→Probing，探测期间状态如实显示「探测中」，
        //  探测失败才能可见地落 Failed 而不是卡死在暂停）
        TaskStatus::Queued | TaskStatus::WaitingRetry | TaskStatus::Paused => {
            ctx.engine
                .transition(&ctx.task_id, TaskStatus::Probing, None)
                .await
                .map_err(drive_from)?;
        }
        // Downloading：崩溃残留，journal 恢复
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
        // HLS 管线自带探测（清单拉取/解析），不接受 HTTP 探测
        return crate::hls::phase(ctx).await;
    }

    let probe = probe::probe(&ctx.client, &task.url, &ctx.task_headers)
        .await
        .map_err(drive_from)?;
    // 多镜像探测：Range 可用且总长与主源一致才启用（超时 10s/个，失败跳过）
    let mut mirrors: Vec<String> = Vec::new();
    for m in mirrors_raw.iter().take(8) {
        if m == &task.url {
            continue;
        }
        let fut = probe::probe(&ctx.client, m, &ctx.task_headers);
        if let Ok(Ok(mp)) = tokio::time::timeout(Duration::from_secs(10), fut).await {
            if mp.segmented && mp.total_bytes == probe.total_bytes {
                mirrors.push(mp.final_url);
            }
        }
    }
    if !mirrors.is_empty() {
        tracing::info!(count = mirrors.len(), "mirrors enabled");
        mirrors.insert(0, probe.final_url.clone()); // 主源放首位
    }
    if ctx.cancel.is_cancelled() {
        // run() 的 Paused 分支会按当前状态（Probing/Paused）落库
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
    let total = probe.total_bytes;

    // 空文件直接完成（plan(0) 的单段天然 done，走通用路径即可，无需特判）

    // 磁盘空间预检（HTTP 分段；HLS 总大小未知，落盘阶段由 ENOSPC 兜底）
    if let Some(total) = probe.total_bytes {
        const MARGIN: u64 = 64 * 1024 * 1024;
        match fs4::available_space(&paths.dir) {
            Ok(avail) if avail < total + MARGIN => {
                return Err(DriveErr::Fatal(
                    ErrorKind::DiskFull,
                    format!(
                        "磁盘空间不足：需要 ≥ {}（含余量），可用 {}",
                        humansize(total + MARGIN),
                        humansize(avail)
                    ),
                ));
            }
            _ => {}
        }
    }

    // 统一进入 Downloading（入口 Queued 已在探测前迁到 Probing；入口 Paused 在此迁移）
    let cur = ctx.engine.require(&ctx.task_id).await.map_err(drive_from)?;
    if matches!(cur.status, TaskStatus::Probing | TaskStatus::Paused) {
        ctx.engine
            .transition(&ctx.task_id, TaskStatus::Downloading, None)
            .await
            .map_err(drive_from)?;
    }

    let segmented = probe.segmented && probe.total_bytes.is_some();
    let (mode, initial_segs, journal, resumed) = if segmented {
        prepare_segmented(&ctx, &probe, &paths)?
    } else {
        // 单连接不可续传：清理残留，从零开始
        Journal::remove(&paths.journal);
        let _ = std::fs::remove_file(&paths.part);
        (
            Mode::Sequential,
            vec![Segment {
                start: 0,
                end: u64::MAX,
                written: 0,
            }],
            None,
            false,
        )
    };
    let _ = resumed;

    // —— 下载
    let shared = Arc::new(Shared::new());
    let outcome = if segmented {
        download_segmented(
            ctx.clone(),
            shared.clone(),
            mode,
            initial_segs,
            journal,
            &paths,
            mirrors,
            &probe,
        )
        .await?
    } else {
        download_sequential(ctx.clone(), shared, &paths, total).await?
    };

    match outcome {
        PhaseOutcome::Completed(final_segs) => {
            finish_completed(&ctx, &paths, &final_segs, total, segmented).await
        }
        PhaseOutcome::Fatal(kind, msg) => Err(DriveErr::Fatal(kind, msg)),
        PhaseOutcome::Paused => Err(DriveErr::Paused),
        PhaseOutcome::FallbackSequential => {
            // 服务器中途不再支持 Range：清理后整体走单连接
            cleanup_partial(&paths);
            let outcome =
                download_sequential(ctx.clone(), Arc::new(Shared::new()), &paths, total).await?;
            match outcome {
                PhaseOutcome::Completed(final_segs) => {
                    finish_completed(&ctx, &paths, &final_segs, total, false).await
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

/// Completed 结果的统一收尾：Verifying → 校验 → finalize → Done。
async fn finish_completed(
    ctx: &Ctx,
    paths: &TargetPaths,
    final_segs: &[Segment],
    total: Option<u64>,
    segmented: bool,
) -> Result<(), DriveErr> {
    ctx.engine
        .transition(&ctx.task_id, TaskStatus::Verifying, None)
        .await
        .map_err(drive_from)?;
    verify(ctx, &paths.part, final_segs, total, segmented).await?;
    // 后处理（视频号 Isaac64-XOR 解密）：在哈希校验之前——校验和针对明文
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
    // 哈希校验：不符 → Failed(checksum_mismatch)，.dlpart 保留供排查
    if let Some((algo, expect)) = &ctx.checksum {
        let ok = crate::checksum::verify_file(&paths.part, algo, expect)
            .map_err(|e| DriveErr::Fatal(e.kind, e.message))?;
        if !ok {
            return Err(DriveErr::Fatal(
                ErrorKind::ChecksumMismatch,
                format!("checksum {algo} mismatch"),
            ));
        }
    }
    finalize(ctx, paths)?;
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
    let url_path = url::Url::parse(&task.url)
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

/// journal 恢复 + 分段规划（DESIGN §5.5 续传语义）。
/// 双验证器缺失时保守放弃进度重下（指纹探测留后续版本）。
fn prepare_segmented(
    ctx: &Ctx,
    probe: &ProbeResult,
    paths: &TargetPaths,
) -> Result<(Mode, Vec<Segment>, Option<Journal>, bool), DriveErr> {
    let total = probe.total_bytes.unwrap_or(0);
    let vh = dl_journal::validator_hash(probe.etag.as_deref(), probe.last_modified.as_deref());
    let has_validator = probe.etag.is_some() || probe.last_modified.is_some();
    let (j, recovered) = Journal::open(&paths.journal)
        .map_err(|e| DriveErr::Fatal(classify_io(&e), format!("open journal: {e}")))?;

    let usable = has_validator
        && match &recovered {
            Some(d) => {
                d.total_len == total
                    && d.validator_hash == vh
                    && paths.part.exists()
                    && journal_entries_sane(&d.entries, total)
            }
            None => false,
        };

    match usable.then(|| recovered.clone().unwrap()) {
        Some(d) => Ok((
            Mode::Segmented {
                total,
                validator_hash: vh,
            },
            d.entries
                .into_iter()
                .map(|e| Segment {
                    start: e.start,
                    end: e.end,
                    written: e.written,
                })
                .collect(),
            Some(j),
            true,
        )),
        None => {
            Journal::remove(&paths.journal);
            let _ = std::fs::remove_file(&paths.part);
            let (j, _) = Journal::open(&paths.journal)
                .map_err(|e| DriveErr::Fatal(classify_io(&e), format!("open journal: {e}")))?;
            let planned = dl_segment::plan(total, ctx.opts.segments.min(MAX_SEGMENTS));
            Ok((
                Mode::Segmented {
                    total,
                    validator_hash: vh,
                },
                planned,
                Some(j),
                false,
            ))
        }
    }
}

/// journal 分段表必须仍是 [0,total) 的不交覆盖（written ≤ len）。
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

/// 分段下载主编排：writer + N workers + 进度上报，返回段完成/中止结果。
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
    let (writer, all_rx) = dl_io::spawn(SpawnParams {
        path: paths.part.clone(),
        mode,
        segments: initial_segs.clone(),
        journal,
    })
    .await
    .map_err(|e| DriveErr::Fatal(classify_io(&e), format!("open part file: {e}")))?;

    // 进度上报：writer 进度 → 引擎事件（100ms 节流）+ 分裂判据数据
    {
        let ectx = ctx.clone();
        let eshared = shared.clone();
        let progress = writer.progress();
        let etotal = primary.total_bytes;
        tokio::spawn(emit_progress(ectx, eshared, progress, etotal));
    }

    // 待领取区间：恢复任务的区间从 journal 光标起
    let ranges: Vec<Segment> = initial_segs
        .iter()
        .filter(|s| !s.done())
        .map(|s| Segment {
            start: s.cursor(),
            end: s.end,
            written: 0,
        })
        .collect();
    let queue = Arc::new(Queue::new(ranges));
    let worker_count = ctx.opts.segments.clamp(1, MAX_SEGMENTS);
    shared.active_workers.store(worker_count, Ordering::Relaxed);
    let validator = primary.validator.clone();

    let mut handles = Vec::new();
    for _ in 0..worker_count {
        let h = tokio::spawn(worker_main(
            ctx.clone(),
            shared.clone(),
            queue.clone(),
            writer.tx(),
            validator.clone(),
            mirrors.clone(),
        ));
        handles.push(h);
    }

    // 等待完成信号或 worker 全部退出（fatal/中止时后者先发生）。
    // biased：完成信号优先于 worker 退出判定，避免正常完成被误判。
    let mut joiner = tokio::spawn(async move {
        for h in handles {
            let _ = h.await;
        }
    });
    let mut all_rx = all_rx;
    let all_result = tokio::select! {
        biased;
        r = &mut all_rx => Some(r),
        _ = &mut joiner => {
            // worker 全退而 writer 未发完成信号：给它 500ms 缓冲（刚完成的竞态）。
            // 注意：胜出分支里 joiner 已完成，绝不能再次 await（JoinHandle 二次 poll 会 panic）。
            tokio::time::timeout(Duration::from_millis(500), &mut all_rx)
                .await
                .ok()
        }
    };
    // joiner 无需返回值：完成即 drop（drop 只 detach，不 poll）
    drop(joiner);

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
    let mut has_sample = false; // 首帧只建基线：恢复/刚启动时避免把历史进度当瞬时速度
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
            // 已下字节持久化（暂停/失败后 UI 恢复显示）：≥2s 一次，防写放大
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
    shared: Arc<Shared>,
    queue: Arc<Queue>,
    wtx: std::sync::mpsc::Sender<Msg>,
    validator: Option<String>,
    mirrors: Vec<String>,
) {
    loop {
        if shared.no_range_fallback.load(Ordering::Relaxed) {
            break;
        }
        let Some(claim) = queue.claim() else {
            // 队列空：若仍有大段在流式（本 worker 提前完成/空闲），
            // 窃取最大活跃段（IDM 动态分段），让空闲连接产出吞吐
            if let Some((start, _end)) = shared.largest_active(STEAL_MIN_REMAINING) {
                if shared.mark_force_requeue(start) {
                    // 等待 owner 交还剩余区间（owner 每块前检查标记，通常毫秒级）
                    for _ in 0..50 {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        if ctx.cancel.is_cancelled()
                            || shared.no_range_fallback.load(Ordering::Relaxed)
                        {
                            break;
                        }
                        if let Some(seg) = queue.claim() {
                            let smidx = (shared.mirror_cursor.fetch_add(1, Ordering::Relaxed)
                                as usize)
                                % mirrors.len().max(1);
                            match stream_range(
                                &ctx,
                                &shared,
                                &queue,
                                &wtx,
                                validator.as_deref(),
                                seg,
                                mirrors.clone(),
                                smidx,
                            )
                            .await
                            {
                                Ok(_) => {}
                                Err(kind) => {
                                    shared.record_fatal(kind);
                                }
                            }
                            break;
                        }
                    }
                }
            }
            break;
        };
        match stream_range(
            &ctx,
            &shared,
            &queue,
            &wtx,
            validator.as_deref(),
            claim,
            mirrors.clone(),
            (shared.mirror_cursor.fetch_add(1, Ordering::Relaxed) as usize) % mirrors.len().max(1),
        )
        .await
        {
            Ok(_) => {}
            Err(kind) => {
                shared.record_fatal(kind);
                break;
            }
        }
    }
}

/// 单个区间的流式下载：Range + If-Range 请求，失败从连续写位置重试。
#[allow(clippy::too_many_arguments)]
async fn stream_range(
    ctx: &Ctx,
    shared: &Shared,
    queue: &Queue,
    wtx: &std::sync::mpsc::Sender<Msg>,
    validator: Option<&str>,
    claim: Claim,
    mirrors: Vec<String>,
    mut midx: usize,
) -> Result<StreamOutcome, ErrorKind> {
    let seg = claim.seg;
    let mut cursor = seg.start;
    let mut attempts: u32 = 0;
    let mut window_bytes: u64 = 0;
    let mut window_start = Instant::now();
    shared.active_insert(seg.start, seg.end, cursor);

    'requests: loop {
        if ctx.cancel.is_cancelled() || shared.no_range_fallback.load(Ordering::Relaxed) {
            shared.active_remove(seg.start);
            return Ok(StreamOutcome::Aborted);
        }
        if attempts >= MAX_ATTEMPTS {
            shared.active_remove(seg.start);
            return Err(ErrorKind::NetworkConnectionReset);
        }
        // 段被其他空闲 worker 窃取：交还剩余区间后退出（不重复下载）
        if shared.take_force_requeue(seg.start) {
            shared.active_remove(seg.start);
            let remaining = seg.end.saturating_sub(cursor);
            if remaining > 0 {
                queue.push(Claim {
                    seg: Segment {
                        start: cursor,
                        end: seg.end,
                        written: 0,
                    },
                    splits_done: claim.splits_done,
                });
            }
            return Ok(StreamOutcome::Aborted);
        }

        // —— 慢段分裂（DESIGN §5.3：护栏防分裂风暴）
        if claim.splits_done < SPLIT_MAX_PER_RANGE
            && window_start.elapsed() >= SPLIT_WINDOW
            && shared.task_downloaded.load(Ordering::Relaxed) >= SPLIT_MIN_TASK_BYTES
            && shared.started.elapsed().as_secs() >= SPLIT_MIN_TASK_SECS
        {
            let win_dt = window_start.elapsed().as_secs_f64().max(0.001);
            let own_bps = window_bytes as f64 / win_dt;
            let fair = shared.aggregate_bps.load(Ordering::Relaxed) as f64
                / shared.active_workers.load(Ordering::Relaxed).max(1) as f64;
            let remaining = seg.end - cursor;
            if own_bps * 2.0 < fair && remaining >= 2 * SPLIT_MIN_HALF {
                let mid = cursor + remaining / 2;
                if wtx
                    .send(Msg::Split {
                        at: cursor,
                        mid,
                        old_end: seg.end,
                    })
                    .is_err()
                {
                    return Err(ErrorKind::Internal);
                }
                queue.push(Claim {
                    seg: Segment {
                        start: cursor,
                        end: mid,
                        written: 0,
                    },
                    splits_done: claim.splits_done + 1,
                });
                queue.push(Claim {
                    seg: Segment {
                        start: mid,
                        end: seg.end,
                        written: 0,
                    },
                    splits_done: claim.splits_done + 1,
                });
                tracing::info!(from = cursor, to = seg.end, mid, "slow segment split");
                return Ok(StreamOutcome::Aborted);
            }
            window_bytes = 0;
            window_start = Instant::now();
        }

        let mirror_url = mirrors
            .get(midx)
            .cloned()
            .unwrap_or_else(|| ctx.url.clone());
        let mut req = with_task_headers(
            identity_headers(ctx.client.get(&mirror_url))
                .header("Range", format!("bytes={}-{}", cursor, seg.end - 1)),
            &ctx.task_headers,
        );
        if let Some(v) = validator {
            req = req.header("If-Range", v);
        }
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                attempts += 1;
                if mirrors.len() > 1 {
                    midx = (midx + 1) % mirrors.len();
                }
                tracing::debug!(error = %e, attempt = attempts, "segment request failed, retrying");
                sleep(backoff(attempts)).await;
                continue 'requests;
            }
        };
        let status = resp.status().as_u16();
        match status {
            206 => {
                // Content-Range 起点校验（aria2 同款）：CDN 边缘不一致时重试，
                // 绝不能把错位的响应拼进本地偏移
                if let Some(start) = resp
                    .headers()
                    .get(reqwest::header::CONTENT_RANGE)
                    .and_then(|v| v.to_str().ok())
                    .and_then(parse_content_range_start)
                {
                    if start != cursor {
                        attempts += 1;
                        drop(resp);
                        tracing::debug!(
                            expected = cursor,
                            got = start,
                            "content-range mismatch, retrying"
                        );
                        sleep(backoff(attempts)).await;
                        continue 'requests;
                    }
                }
            }
            200 => {
                // 200 = 该镜像忽略了 Range。多镜像：标记失效，剩余区间重新入队
                // （其他正常镜像接手）；全部失效：任务级降级单连接（从 0 重新
                // 下载，内容变更亦被覆盖）
                if mirrors.len() > 1 {
                    shared.bad_mirrors.lock().unwrap().insert(midx);
                    shared.active_remove(seg.start);
                    let remaining = seg.end.saturating_sub(cursor);
                    if remaining > 0 {
                        queue.push(Claim {
                            seg: Segment {
                                start: cursor,
                                end: seg.end,
                                written: 0,
                            },
                            splits_done: claim.splits_done,
                        });
                    }
                    return Ok(StreamOutcome::Aborted);
                }
                shared.no_range_fallback.store(true, Ordering::Relaxed);
                return Ok(StreamOutcome::Aborted);
            }
            416 => {
                if cursor >= seg.end {
                    return Ok(StreamOutcome::Done);
                }
                return Err(ErrorKind::RangeUnsupported);
            }
            s => match classify_status(s) {
                Some(kind) if kind.is_retryable() => {
                    let ra = retry_after_secs(resp.headers().get(reqwest::header::RETRY_AFTER));
                    attempts += 1;
                    drop(resp);
                    sleep(Duration::from_secs(ra.unwrap_or(0)).saturating_add(backoff(attempts)))
                        .await;
                    continue 'requests;
                }
                Some(kind) => return Err(kind),
                None => {
                    attempts += 1;
                    drop(resp);
                    sleep(backoff(attempts)).await;
                    continue 'requests;
                }
            },
        }

        // 206：流式读取；30s 无数据视为断流，从当前光标重试
        let mut resp = resp;
        if let Some(ce) = resp
            .headers()
            .get(reqwest::header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok())
        {
            if !ce.eq_ignore_ascii_case("identity") {
                return Err(ErrorKind::UnsupportedEncoding);
            }
        }
        loop {
            let chunk = tokio::select! {
                _ = ctx.cancel.cancelled() => return Ok(StreamOutcome::Aborted),
                r = tokio::time::timeout(STALL_TIMEOUT, resp.chunk()) => match r {
                    Err(_) => None,
                    Ok(Err(_)) => None,
                    Ok(Ok(None)) => {
                        if cursor >= seg.end {
                            return Ok(StreamOutcome::Done);
                        }
                        None // 提前 EOF → 从光标重试
                    }
                    Ok(Ok(Some(b))) => Some(b),
                }
            };
            let Some(bytes) = chunk else {
                attempts += 1;
                continue 'requests;
            };
            // 大 chunk 切片发送：限速器单次额度上限 256KB（见 Limiter::ACQUIRE_CAP）
            let mut rest: &[u8] = &bytes;
            while !rest.is_empty() {
                if ctx.cancel.is_cancelled() {
                    shared.active_remove(seg.start);
                    return Ok(StreamOutcome::Aborted);
                }
                let n = rest.len().min(Limiter::ACQUIRE_CAP as usize);
                ctx.limiter.acquire(n as u64).await;
                if wtx
                    .send(Msg::Chunk {
                        offset: cursor,
                        data: rest[..n].to_vec(),
                    })
                    .is_err()
                {
                    shared.active_remove(seg.start);
                    return Err(ErrorKind::Internal);
                }
                cursor += n as u64;
                window_bytes += n as u64;
                shared.active_update_cursor(seg.start, cursor);
                rest = &rest[n..];
            }
        }
    }
}

/// 单连接顺序下载（服务器不支持 Range / 总长未知路径）。
/// 断流重试带 Range 续传尝试；服务器对续传请求返回 200 时整体从零重来（有限次）。
async fn download_sequential(
    ctx: Arc<Ctx>,
    shared: Arc<Shared>,
    paths: &TargetPaths,
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

        let result = stream_whole(&ctx, writer.tx()).await;
        match result {
            Err(kind) => {
                let _ = writer.finish().await;
                return Ok(PhaseOutcome::Fatal(
                    kind,
                    "sequential download failed".into(),
                ));
            }
            Ok(StreamOutcome::Aborted) => {
                // 服务器对续传请求返回 200：内容可能已变，从零重来
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
            Ok(StreamOutcome::Done) => {
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
    wtx: std::sync::mpsc::Sender<Msg>,
) -> Result<StreamOutcome, ErrorKind> {
    let mut attempts: u32 = 0;
    let mut cursor = 0u64;
    'requests: loop {
        if ctx.cancel.is_cancelled() {
            return Ok(StreamOutcome::Done);
        }
        if attempts >= MAX_ATTEMPTS {
            return Err(ErrorKind::NetworkConnectionReset);
        }
        let mut req = with_task_headers(
            identity_headers(ctx.client.get(&ctx.url)),
            &ctx.task_headers,
        );
        if cursor > 0 {
            req = req.header("Range", format!("bytes={cursor}-"));
        }
        let resp = match req.send().await {
            Ok(r) => r,
            Err(_) => {
                attempts += 1;
                sleep(backoff(attempts)).await;
                continue 'requests;
            }
        };
        match resp.status().as_u16() {
            200 => {
                if cursor > 0 {
                    // 续传请求被无视：内容可能已变，放弃已下部分
                    return Ok(StreamOutcome::Aborted);
                }
            }
            206 => {}
            s => match classify_status(s) {
                Some(kind) if kind.is_retryable() => {
                    attempts += 1;
                    sleep(backoff(attempts)).await;
                    continue 'requests;
                }
                Some(kind) => return Err(kind),
                None => {
                    attempts += 1;
                    sleep(backoff(attempts)).await;
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
                return Err(ErrorKind::UnsupportedEncoding);
            }
        }
        loop {
            let chunk = tokio::select! {
                _ = ctx.cancel.cancelled() => return Ok(StreamOutcome::Done),
                r = tokio::time::timeout(STALL_TIMEOUT, resp.chunk()) => match r {
                    Err(_) => None,
                    Ok(Err(_)) => None,
                    Ok(Ok(None)) => {
                        // body 正常结束
                        if wtx.send(Msg::Eof).is_err() {
                            return Err(ErrorKind::Internal);
                        }
                        return Ok(StreamOutcome::Done);
                    }
                    Ok(Ok(Some(b))) => Some(b),
                }
            };
            let Some(bytes) = chunk else {
                attempts += 1;
                continue 'requests; // 断流：带 Range 从光标续传重试
            };
            let mut rest: &[u8] = &bytes;
            while !rest.is_empty() {
                if ctx.cancel.is_cancelled() {
                    return Ok(StreamOutcome::Done);
                }
                let n = rest.len().min(Limiter::ACQUIRE_CAP as usize);
                ctx.limiter.acquire(n as u64).await;
                if wtx
                    .send(Msg::Chunk {
                        offset: cursor,
                        data: rest[..n].to_vec(),
                    })
                    .is_err()
                {
                    return Err(ErrorKind::Internal);
                }
                cursor += n as u64;
                rest = &rest[n..];
            }
        }
    }
}

/// 完成前强制校验集（DESIGN §5.3）：覆盖完整性、长度、随机抽检。
async fn verify(
    ctx: &Ctx,
    part: &Path,
    final_segs: &[Segment],
    total: Option<u64>,
    segmented: bool,
) -> Result<(), DriveErr> {
    let meta = std::fs::metadata(part)
        .map_err(|e| DriveErr::Fatal(classify_io(&e), format!("verify: stat part file: {e}")))?;
    if let Some(t) = total {
        if meta.len() != t {
            return Err(DriveErr::Fatal(
                ErrorKind::Internal,
                format!("length mismatch: file {} bytes, expected {t}", meta.len()),
            ));
        }
    }
    if segmented {
        let t = total.unwrap_or(0);
        if !dl_segment::full_coverage(final_segs, t) {
            return Err(DriveErr::Fatal(
                ErrorKind::Internal,
                "coverage check failed: segments do not exactly cover the file".into(),
            ));
        }
        // 抽检：随机 4KB 范围重下比对，挡住 CDN 静默截断
        for i in 0..ctx.opts.spot_checks {
            let t = match total {
                Some(t) if t > 0 => t,
                _ => break,
            };
            let len = 4096u64.min(t);
            let seed = xxhash_rust::xxh64::xxh64(
                &std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
                    .to_le_bytes(),
                i as u64,
            );
            let off = seed % (t - len + 1);
            if let Err(e) = spot_check(ctx, part, off, len).await {
                return Err(DriveErr::Fatal(
                    ErrorKind::Internal,
                    format!("spot check at offset {off} failed: {e}"),
                ));
            }
        }
    }
    Ok(())
}

async fn spot_check(ctx: &Ctx, part: &Path, off: u64, len: u64) -> Res<()> {
    let resp = with_task_headers(
        identity_headers(ctx.client.get(&ctx.url))
            .header("Range", format!("bytes={off}-{}", off + len - 1)),
        &ctx.task_headers,
    )
    .send()
    .await
    .map_err(|e| DownloadError::new(map_reqwest_err(&e), e.to_string()))?;
    if resp.status().as_u16() != 206 {
        return Ok(()); // 服务器此刻不给 Range：跳过抽检
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

/// 收尾：冲突策略 → `.dlpart` 原子 rename → journal 清理（DESIGN §5.5）。
pub(crate) fn finalize(ctx: &Ctx, paths: &TargetPaths) -> Result<(), DriveErr> {
    let target = resolve_conflict(&paths.dir, &paths.name, ctx.opts.conflict)
        .map_err(|e| DriveErr::Fatal(e.kind, e.message))?;
    let Some(target) = target else {
        // Skip：丢弃成品
        let _ = std::fs::remove_file(&paths.part);
        Journal::remove(&paths.journal);
        return Ok(());
    };
    if target.exists() {
        // Overwrite：先移除旧目标（Windows 上被占用时短暂重试）
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

/** "bytes 200-999/1000" → Some(200)。 */
fn parse_content_range_start(v: &str) -> Option<u64> {
    let spec = v.trim().strip_prefix("bytes ")?;
    let mut it = spec.split('-');
    let start = it.next()?;
    start.trim().parse::<u64>().ok()
}

/// 指数退避：1s/2s/4s/8s/16s/30s…（DESIGN §5.6）。
fn backoff(attempts: u32) -> Duration {
    let exp = attempts.saturating_sub(1).min(5);
    Duration::from_secs(1u64 << exp).min(BACKOFF_MAX)
}

async fn sleep(d: Duration) {
    if d > Duration::ZERO {
        tokio::time::sleep(d).await;
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
