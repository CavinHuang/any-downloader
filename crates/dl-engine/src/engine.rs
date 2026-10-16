use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::driver::DownloadOptions;
use dl_error::{DownloadError, ErrorKind, Res};
use dl_model::{
    new_task_id, now_ms, CreateContentTask, CreateTask, Task, TaskKind, TaskStatus,
};
use dl_persist::Db;

/// 引擎事件（100ms 聚合节流见 http.rs 的 emit_progress）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EngineEvent {
    Progress {
        task_id: String,
        downloaded: u64,
        total: Option<u64>,
        bps: u64,
        /// 分段实时快照（HTTP 多线程；HLS/单连接为 None）
        segments: Option<Vec<dl_segment::Segment>>,
    },
    StateChanged {
        task_id: String,
        status: TaskStatus,
    },
}

/// 同进程运行控制：pause/cancel 都经 token 协作取消，reason 区分落库去向。
/// done 用于「旧实例收尾中 → 新调用等它退出后再开始」。
#[derive(Clone)]
struct RunControl {
    token: CancellationToken,
    cancel_requested: Arc<std::sync::atomic::AtomicBool>,
    done: Arc<tokio::sync::Notify>,
}

/// 引擎句柄：任务生命周期 + 下载驱动 + 事件流。
/// 状态迁移全部经 transition()（迁移表校验 + 落库 + 事件）。
#[derive(Clone)]
pub struct Engine {
    db: Db,
    events: broadcast::Sender<EngineEvent>,
    runs: Arc<Mutex<HashMap<String, RunControl>>>,
    /// 数据根目录（db 所在目录）；journal 等工作文件放在其下。
    root: std::path::PathBuf,
}

impl Engine {
    pub async fn open(db_path: &Path) -> Res<Engine> {
        let (events, _) = broadcast::channel(1024);
        let root = db_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        Ok(Engine {
            db: Db::open(db_path)?,
            events,
            runs: Arc::new(Mutex::new(HashMap::new())),
            root,
        })
    }

    pub(crate) fn journal_dir(&self) -> std::path::PathBuf {
        self.root.join("dl-journals")
    }

    /// 当前运行中任务数（并发上限用）。
    pub fn running_count(&self) -> usize {
        self.runs.lock().unwrap().len()
    }

    /// 暂停全部运行中任务（协作 checkpoint），返回数量。退出前调用。
    pub fn request_pause_all(&self) -> usize {
        let runs = self.runs.lock().unwrap();
        let mut n = 0;
        for c in runs.values() {
            c.cancel_requested
                .store(false, std::sync::atomic::Ordering::Relaxed);
            c.token.cancel();
            n += 1;
        }
        n
    }

    pub async fn open_default() -> Res<Engine> {
        let dir = crate::data_dir();
        Self::open(&dir.join("data.db")).await
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EngineEvent> {
        self.events.subscribe()
    }

    pub(crate) fn emit(&self, ev: EngineEvent) {
        let _ = self.events.send(ev);
    }

    /// 请求协作暂停正在运行的任务（同进程内有效；跨进程控制在 M3 经 IPC 接入）。
    pub fn request_pause(&self, id: &str) -> bool {
        match self.runs.lock().unwrap().get(id) {
            Some(c) => {
                c.cancel_requested
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                c.token.cancel();
                true
            }
            None => false,
        }
    }

    /// 请求协作取消正在运行的任务。
    pub fn request_cancel(&self, id: &str) -> bool {
        match self.runs.lock().unwrap().get(id) {
            Some(c) => {
                c.cancel_requested
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                c.token.cancel();
                true
            }
            None => false,
        }
    }

    /// 驱动一个任务走完 下载 → 验证 → 收尾 全流程，返回最终任务。
    /// 入口状态：Queued（新任务）、Paused（续传）或 Downloading（崩溃残留）。
    pub async fn download(&self, id: &str, opts: DownloadOptions) -> Res<Task> {
        {
            // 已有实例在跑：若它正在收尾（token 已取消，如刚暂停），等它退出后
            // 重新开始；否则才是真正的重复启动。锁不跨 await。
            loop {
                let wait_on = {
                    let runs = self.runs.lock().unwrap();
                    match runs.get(id) {
                        None => None,
                        Some(c) if c.token.is_cancelled() => Some(c.done.clone()),
                        Some(_) => {
                            return Err(DownloadError::new(
                                ErrorKind::Internal,
                                format!("task {id} is already running"),
                            ));
                        }
                    }
                };
                match wait_on {
                    None => break,
                    Some(done) => {
                        let _ = tokio::time::timeout(
                            std::time::Duration::from_secs(30),
                            done.notified(),
                        )
                        .await;
                    }
                }
            }
            let mut runs = self.runs.lock().unwrap();
            runs.insert(
                id.to_string(),
                RunControl {
                    token: CancellationToken::new(),
                    cancel_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    done: Arc::new(tokio::sync::Notify::new()),
                },
            );
        }
        let control = self
            .runs
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .expect("just inserted");
        let done = control.done.clone();
        let result =
            crate::driver::run(self, id, opts, control.token, control.cancel_requested).await;
        self.runs.lock().unwrap().remove(id);
        done.notify_waiters();
        result
    }

    /// 按优先级处理队列中的任务（DESIGN §5.2：任务级并发）。
    pub async fn run_queued(
        &self,
        opts: DownloadOptions,
        concurrency: usize,
    ) -> Res<Vec<Res<Task>>> {
        let tasks = self.list_tasks(Some(TaskStatus::Queued)).await?;
        let sem = Arc::new(tokio::sync::Semaphore::new(concurrency.max(1)));
        let mut handles = Vec::new();
        for t in tasks {
            let permit = sem.clone().acquire_owned().await.unwrap();
            let engine = self.clone();
            let opts = opts.clone();
            handles.push(tokio::spawn(async move {
                let r = engine.download(&t.id, opts).await;
                drop(permit);
                (t.id, r)
            }));
        }
        let mut out = Vec::new();
        for h in handles {
            let (id, r) = match h.await {
                Ok(pair) => pair,
                Err(e) => {
                    return Err(DownloadError::new(
                        ErrorKind::Internal,
                        format!("a queued download task panicked: {e}"),
                    ));
                }
            };
            tracing::info!(
                task = %id,
                status = r.as_ref().map(|t| t.status.as_str()).unwrap_or("error"),
                "queued task finished"
            );
            out.push(r);
        }
        Ok(out)
    }

    pub async fn create_task(&self, mut c: CreateTask) -> Res<Task> {
        // scheme 白名单是安全不变量（DESIGN §9）：重定向跟随层同样只允许 http/https。
        let parsed = url::Url::parse(&c.url)
            .map_err(|e| DownloadError::new(ErrorKind::InvalidUrl, format!("invalid url: {e}")))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(DownloadError::new(
                ErrorKind::InvalidUrl,
                format!("unsupported scheme {:?}, only http/https", parsed.scheme()),
            ));
        }
        // 仅白名单头允许透传（DESIGN §9）；序列化后 ≤16KB
        // 去重：同一 URL 已存在未完结任务 → 拒绝重复创建（GUI 提示已有任务）
        let dup = self.db.find_active_by_url(&c.url).await?;
        if let Some(existing) = dup {
            return Err(DownloadError::new(
                ErrorKind::TaskExists {
                    id: existing.id,
                    status: existing.status.as_str().into(),
                },
                "an active task for this URL already exists".to_string(),
            ));
        }
        // 自动分类目录：未指定目录 + 设置开启 → 默认目录/分类子目录
        if c.dir
            .as_deref()
            .map(|d| d.trim().is_empty())
            .unwrap_or(true)
        {
            let auto = self
                .get_setting("auto_categorize")
                .await
                .ok()
                .flatten()
                .map(|v| v == "on")
                .unwrap_or(false);
            if auto {
                let base = self
                    .get_setting("default_dir")
                    .await
                    .ok()
                    .flatten()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| {
                        dirs::download_dir()
                            .map(|d| d.display().to_string())
                            .unwrap_or_else(|| crate::data_dir().display().to_string())
                    });
                c.dir = Some(match Self::category_for(&c.url) {
                    Some(cat) => {
                        format!("{base}{sep}{cat}", sep = std::path::MAIN_SEPARATOR)
                    }
                    None => base,
                });
            }
        }

        // 多镜像归一化：http/https 校验、去重、剔除主 URL、上限 8
        let mirrors_json = match &c.mirrors {
            Some(list) if !list.is_empty() => {
                let mut out: Vec<String> = Vec::new();
                for m in list {
                    let m = m.trim();
                    if !matches!(
                        url::Url::parse(m)
                            .map(|u| u.scheme().to_string())
                            .as_deref(),
                        Ok("http") | Ok("https")
                    ) {
                        return Err(DownloadError::new(
                            ErrorKind::InvalidUrl,
                            format!("invalid mirror url: {m}"),
                        ));
                    }
                    if m != c.url && !out.iter().any(|x| x == m) && out.len() < 8 {
                        out.push(m.to_string());
                    }
                }
                if out.is_empty() {
                    None
                } else {
                    serde_json::to_string(&out).ok()
                }
            }
            _ => None,
        };
        let headers_json = match &c.headers {
            Some(h) if !h.is_empty() => {
                let filtered: std::collections::HashMap<_, _> = h
                    .iter()
                    .filter(|(k, _)| {
                        dl_model::HEADER_ALLOWLIST.contains(&k.to_ascii_lowercase().as_str())
                    })
                    .collect();
                if filtered.is_empty() {
                    None
                } else {
                    let json = serde_json::to_string(&filtered).map_err(|e| {
                        DownloadError::new(
                            ErrorKind::InvalidParams,
                            format!("serialize headers: {e}"),
                        )
                    })?;
                    if json.len() > 16 * 1024 {
                        return Err(DownloadError::new(
                            ErrorKind::InvalidParams,
                            "headers too large (max 16KiB)",
                        ));
                    }
                    Some(json)
                }
            }
            _ => None,
        };
        // URL 特征启发：.m3u8/.m3u 直链自动归为 HLS（驱动内清单探测确认）；
        // YouTube/X/Bilibili/TikTok 归为 yt-dlp 托管（风控/签名/动态清单由
        // yt-dlp 应对，识别域名清单见 ytdlp_task::is_ytdlp_host）
        let kind = c.kind.or_else(|| {
            let host_ok = url::Url::parse(&c.url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
                .map(|h| crate::ytdlp_task::is_ytdlp_host(&h))
                .unwrap_or(false);
            if host_ok {
                return Some(TaskKind::Ytdlp);
            }
            if url::Url::parse(&c.url)
                .ok()
                .and_then(|u| {
                    u.path_segments().map(|mut it| {
                        it.next_back().map(|s| {
                            s.to_ascii_lowercase().ends_with(".m3u8")
                                || s.to_ascii_lowercase().ends_with(".m3u")
                        })
                    })
                })
                .flatten()
                .unwrap_or(false)
            {
                Some(TaskKind::Hls)
            } else {
                None
            }
        });
        let now = now_ms();
        let task = Task {
            id: new_task_id(),
            url: c.url,
            final_url: None,
            filename: c.filename,
            dir: c.dir.take(),
            total_bytes: None,
            etag: None,
            last_modified: None,
            mime: None,
            kind: kind.unwrap_or(TaskKind::Http),
            status: TaskStatus::Queued,
            priority: c.priority.unwrap_or(0),
            headers_ref: None,
            headers_json,
            downloaded_bytes: 0,
            variants_json: None,
            checksum: c
                .checksum
                .as_deref()
                .and_then(crate::checksum::parse_checksum)
                .map(|(algo, hex)| format!("{algo}:{hex}")),
            mirrors_json,
            postprocess_json: Self::normalize_postprocess(&c.postprocess),
            error: None,
            created_at: now,
            updated_at: now,
        };
        self.db.insert_task(task.clone()).await?;
        Ok(task)
    }

    /// 内容任务上限（与 dl-ipc 请求帧上限一致；防御直连 IPC 的超大载荷）。
    pub const MAX_CONTENT_BYTES: usize = 32 * 1024 * 1024;

    /// 内容任务（站点适配层文章导出，docs/MEDIA-ADAPTERS.md §11）：
    /// 扩展生成的 HTML/Markdown 直接落盘为已完成任务——无下载阶段，
    /// 状态直达 Done（状态机不变量由 insert 直写保证，不经 transition）。
    pub async fn create_content_task(&self, c: CreateContentTask) -> Res<Task> {
        if c.content.len() > Self::MAX_CONTENT_BYTES {
            return Err(DownloadError::new(
                ErrorKind::InvalidParams,
                format!("content too large (max {} bytes)", Self::MAX_CONTENT_BYTES),
            ));
        }
        if c.filename.trim().is_empty() {
            return Err(DownloadError::new(
                ErrorKind::InvalidParams,
                "filename is required".to_string(),
            ));
        }
        if let Some(src) = c.source_url.as_deref() {
            if !src.starts_with("http://") && !src.starts_with("https://") {
                return Err(DownloadError::new(
                    ErrorKind::InvalidParams,
                    "source_url must be http(s)".to_string(),
                ));
            }
        }
        if let Some(d) = c.dir.as_deref() {
            // 目录穿越防护：合集目录由扩展拼接，段级 ".." 一律拒绝
            if d.split(['/', '\\']).any(|seg| seg == "..") {
                return Err(DownloadError::new(
                    ErrorKind::InvalidParams,
                    "dir must not contain '..' segments".to_string(),
                ));
            }
        }
        // 目录解析：显式 dir（文章合集模式）> default_dir 设置 > 系统下载目录；
        // 无显式 dir 时保留 auto_categorize → 「文档」语义
        let base = match c.dir.as_deref() {
            Some(d) if !d.trim().is_empty() => d.to_string(),
            _ => {
                let base = self
                    .get_setting("default_dir")
                    .await?
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| {
                        dirs::download_dir()
                            .map(|d| d.display().to_string())
                            .unwrap_or_else(|| crate::data_dir().display().to_string())
                    });
                let auto = self
                    .get_setting("auto_categorize")
                    .await
                    .ok()
                    .flatten()
                    .map(|v| v == "on")
                    .unwrap_or(false);
                if auto {
                    format!("{base}{sep}文档", sep = std::path::MAIN_SEPARATOR)
                } else {
                    base
                }
            }
        };
        let dir_path = std::path::PathBuf::from(&base);
        tokio::fs::create_dir_all(&dir_path).await.map_err(|e| {
            DownloadError::new(
                ErrorKind::FileAccessDenied,
                format!("create dir {}: {e}", dir_path.display()),
            )
        })?;

        let clean = dl_filename::sanitize(&c.filename);
        let target = dl_filename::resolve_conflict(
            &dir_path,
            &clean,
            dl_filename::ConflictPolicy::Rename,
        )?
        .ok_or_else(|| {
            DownloadError::new(
                ErrorKind::NameConflict {
                    path: dir_path.join(&clean).display().to_string(),
                },
                "target already exists".to_string(),
            )
        })?;
        let bytes = c.content.as_bytes();
        tokio::fs::write(&target, bytes).await.map_err(|e| {
            DownloadError::new(
                ErrorKind::FileAccessDenied,
                format!("write {}: {e}", target.display()),
            )
        })?;

        let now = now_ms();
        let task = Task {
            id: new_task_id(),
            // 有原文链接记原文（UI 展示/跳转真实来源页）；否则落盘路径占位。
            // 内容任务不参与续传/去重，url 仅展示语义。
            url: c.source_url.unwrap_or_else(|| format!("content://{}", target.display())),
            final_url: None,
            filename: Some(
                target
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(&clean)
                    .to_string(),
            ),
            dir: Some(base),
            total_bytes: Some(bytes.len() as u64),
            etag: None,
            last_modified: None,
            mime: c.mime,
            kind: TaskKind::Content,
            status: TaskStatus::Done,
            priority: 0,
            headers_ref: None,
            headers_json: None,
            downloaded_bytes: bytes.len() as u64,
            variants_json: None,
            checksum: None,
            mirrors_json: None,
            postprocess_json: None,
            error: None,
            created_at: now,
            updated_at: now,
        };
        self.db.insert_task(task.clone()).await?;
        // GUI 列表靠事件刷新；内容任务无状态迁移，直接广播终态
        self.emit(EngineEvent::StateChanged {
            task_id: task.id.clone(),
            status: TaskStatus::Done,
        });
        Ok(task)
    }

    /// 从设置构建下载选项（GUI 设置页 / IPC task.start 共用）。
    pub async fn download_options_from_settings(&self) -> Res<crate::driver::DownloadOptions> {
        let dir = self
            .get_setting("default_dir")
            .await?
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                dirs::download_dir()
                    .map(|d| d.display().to_string())
                    .unwrap_or_else(|| crate::data_dir().display().to_string())
            });
        let segments = self
            .get_setting("segments")
            .await?
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(8)
            .clamp(1, crate::driver::MAX_SEGMENTS);
        let task_bps = self
            .get_setting("task_bps")
            .await?
            .and_then(|v| v.parse::<u64>().ok());
        let global_bps = self
            .get_setting("global_bps")
            .await?
            .and_then(|v| v.parse::<u64>().ok());
        let proxy = self.get_setting("proxy").await?.filter(|s| !s.is_empty());
        let conflict = self
            .get_setting("conflict")
            .await?
            .and_then(|v| dl_filename::ConflictPolicy::parse(&v))
            .unwrap_or_default();
        Ok(crate::driver::DownloadOptions {
            dir: std::path::PathBuf::from(dir),
            segments,
            task_bps,
            global_bps,
            proxy,
            conflict,
            spot_checks: 2,
        })
    }

    /// 设置 KV（GUI/CLI 共享；值为 JSON 字符串）。
    pub async fn set_setting(&self, key: &str, value: &str) -> Res<()> {
        self.db.set_setting(key, value).await
    }

    pub async fn get_setting(&self, key: &str) -> Res<Option<String>> {
        self.db.get_setting(key).await
    }

    pub async fn get_task(&self, id: &str) -> Res<Option<Task>> {
        self.db.get_task(id).await
    }

    pub async fn list_tasks(&self, status: Option<TaskStatus>) -> Res<Vec<Task>> {
        self.db.list_tasks(status).await
    }

    /// 增量轮询：返回 updated_at > since_ms 的任务。
    pub async fn list_updated_since(&self, since_ms: i64) -> Res<Vec<Task>> {
        self.db.list_updated_since(since_ms).await
    }

    /// 暂停：运行中 → 协作取消（driver checkpoint 后自行落 Paused）；
    /// 非运行 → 直接状态迁移。
    pub async fn pause(&self, id: &str) -> Res<Task> {
        if self.request_pause(id) {
            return self.require(id).await;
        }
        self.transition(id, TaskStatus::Paused, None).await
    }

    pub async fn resume(&self, id: &str) -> Res<Task> {
        self.transition(id, TaskStatus::Downloading, None).await
    }

    /// 取消：运行中 → 协作取消；非运行 → 直接迁移。
    pub async fn cancel(&self, id: &str) -> Res<Task> {
        if self.request_cancel(id) {
            return self.require(id).await;
        }
        self.transition(id, TaskStatus::Canceled, None).await
    }

    /// 失败/取消后重新入队，并清除错误。
    pub async fn retry(&self, id: &str) -> Res<Task> {
        self.transition(id, TaskStatus::Queued, None).await
    }

    /// 设置队列优先级（run_queued / 未来调度器按此排序；数值越大越先）。
    pub async fn set_priority(&self, id: &str, priority: i32) -> Res<Task> {
        self.db.set_priority(id, priority).await?;
        self.emit(EngineEvent::StateChanged {
            task_id: id.to_string(),
            status: self.require(id).await?.status,
        });
        self.require(id).await
    }

    /// 重命名任务的显示/输出文件名。仅非活跃态允许（下载中改名会造成
    /// .dlpart 与成品名错位）。
    pub async fn rename_task(&self, id: &str, filename: &str) -> Res<Task> {
        let task = self.require(id).await?;
        if task.status.is_active() {
            return Err(DownloadError::new(
                ErrorKind::InvalidTransition {
                    from: task.status.as_str().into(),
                    to: "renamed".into(),
                },
                "pause the task before renaming it",
            ));
        }
        if filename.trim().is_empty() {
            return Err(DownloadError::new(
                ErrorKind::FileNameInvalid,
                "filename is empty",
            ));
        }
        let clean = dl_filename::sanitize(filename);
        self.db.rename_task(id, &clean).await?;
        self.emit(EngineEvent::StateChanged {
            task_id: id.to_string(),
            status: task.status,
        });
        self.require(id).await
    }

    pub async fn remove(&self, id: &str) -> Res<bool> {
        let task = self.require(id).await?;
        if task.status.is_active() {
            return Err(DownloadError::new(
                ErrorKind::InvalidTransition {
                    from: task.status.as_str().into(),
                    to: "removed".into(),
                },
                "cancel the task before removing it",
            ));
        }
        let deleted = self.db.delete_task(id).await?;
        if deleted {
            // 尽力清理产物残留：journal / HLS 临时目录 / .dlpart（绝不触碰成品）
            let _ = std::fs::remove_file(self.journal_dir().join(format!("{id}.dlctrl")));
            let _ = std::fs::remove_dir_all(self.root.join("dl-tmp").join(id));
            if let (Some(dir), Some(name)) = (&task.dir, &task.filename) {
                let _ =
                    std::fs::remove_file(std::path::Path::new(dir).join(format!("{name}.dlpart")));
            }
        }
        Ok(deleted)
    }

    pub(crate) async fn transition(
        &self,
        id: &str,
        to: TaskStatus,
        error: Option<ErrorKind>,
    ) -> Res<Task> {
        let task = self.require(id).await?;
        if !dl_model::can_transition(task.status, to) {
            return Err(DownloadError::new(
                ErrorKind::InvalidTransition {
                    from: task.status.as_str().into(),
                    to: to.as_str().into(),
                },
                format!(
                    "cannot transition {} -> {}",
                    task.status.as_str(),
                    to.as_str()
                ),
            ));
        }
        self.db.update_status(id, to, error).await?;
        self.emit(EngineEvent::StateChanged {
            task_id: id.to_string(),
            status: to,
        });
        self.require(id).await
    }

    pub(crate) async fn record_downloaded(&self, id: &str, bytes: u64) -> Res<()> {
        self.db.update_downloaded(id, bytes).await
    }

    pub(crate) async fn record_variants(&self, id: &str, variants_json: &str) -> Res<()> {
        self.db.update_variants(id, variants_json).await
    }

    /// 自动分类目录：按 URL 扩展名归类（设置 auto_categorize=on 时生效）。
    fn category_for(url: &str) -> Option<&'static str> {
        let path = url::Url::parse(url).ok()?.path().to_ascii_lowercase();
        let ext = path.rsplit('.').next()?;
        Some(match ext {
            "mp4" | "mkv" | "webm" | "avi" | "mov" | "wmv" | "flv" | "m4v" | "ts" | "m3u8" => {
                "视频"
            }
            "mp3" | "flac" | "m4a" | "aac" | "ogg" | "wav" | "ape" => "音乐",
            "zip" | "rar" | "7z" | "tar" | "gz" => "压缩包",
            "pdf" | "epub" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" => "文档",
            "exe" | "msi" | "apk" | "dmg" => "软件",
            "jpg" | "jpeg" | "png" | "gif" | "webp" => "图片",
            _ => return None,
        })
    }

    /// 后处理归一化：只接受 isaac64-xor 且 seed 非空；空列表 → None。
    fn normalize_postprocess(steps: &Option<Vec<dl_model::PostProcess>>) -> Option<String> {
        let list = steps.as_deref().unwrap_or_default();
        let mut out: Vec<dl_model::PostProcess> = Vec::new();
        for st in list {
            if st.seed.trim().is_empty() {
                continue;
            }
            out.push(dl_model::PostProcess {
                algo: match st.algo {
                    a @ dl_model::PostProcessAlgo::Isaac64Xor => a,
                },
                seed: st.seed.trim().to_string(),
            });
        }
        (!out.is_empty()).then(|| dl_model::PostProcess::as_json(&out))
    }

    pub(crate) async fn record_probe(
        &self,
        id: &str,
        total_bytes: Option<u64>,
        etag: Option<String>,
        last_modified: Option<String>,
        mime: Option<String>,
        final_url: Option<String>,
    ) -> Res<()> {
        self.db
            .update_probe(id, total_bytes, etag, last_modified, mime, final_url)
            .await
    }

    pub(crate) async fn require(&self, id: &str) -> Res<Task> {
        self.get_task(id)
            .await?
            .ok_or_else(|| DownloadError::new(ErrorKind::NotFound, format!("task {id} not found")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestEnv {
        _dir: tempfile::TempDir,
        engine: Engine,
    }

    async fn test_engine() -> TestEnv {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(&dir.path().join("test.db")).await.unwrap();
        TestEnv { _dir: dir, engine }
    }

    #[tokio::test]
    async fn create_validates_url_and_scheme() {
        let env = test_engine().await;

        let err = env
            .engine
            .create_task(CreateTask::of("not a url"))
            .await
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidUrl);

        let err = env
            .engine
            .create_task(CreateTask::of("ftp://example.com/a.zip"))
            .await
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidUrl);

        let t = env
            .engine
            .create_task(CreateTask::of("https://example.com/a.zip"))
            .await
            .unwrap();
        assert_eq!(t.status, TaskStatus::Queued);
        assert_eq!(t.kind, TaskKind::Http);
    }

    #[tokio::test]
    async fn create_content_task_writes_file_and_marks_done() {
        let env = test_engine().await;
        // 必须显式指定目录：缺省目录解析会落到系统下载目录（真实用户目录）
        let dir = env._dir.path().join("out");
        env.engine
            .set_setting("default_dir", dir.display().to_string().as_str())
            .await
            .unwrap();
        let t = env
            .engine
            .create_content_task(CreateContentTask {
                filename: "测试文章.html".into(),
                mime: Some("text/html;charset=utf-8".into()),
                content: "<!DOCTYPE html><html><body>正文</body></html>".into(),
                dir: None,
                source_url: Some("https://blog.csdn.net/u/a/1".into()),
            })
            .await
            .unwrap();
        // 内容任务直达 Done，无下载阶段
        assert_eq!(t.status, TaskStatus::Done);
        assert_eq!(t.kind, TaskKind::Content);
        assert_eq!(t.filename.as_deref(), Some("测试文章.html"));
        assert_eq!(t.total_bytes, Some(t.downloaded_bytes));
        assert_eq!(t.mime.as_deref(), Some("text/html;charset=utf-8"));
        // 原文链接记录进 url（UI 展示真实来源页）
        assert_eq!(t.url, "https://blog.csdn.net/u/a/1");
        // 文件确实落盘且内容一致
        let path = std::path::Path::new(t.dir.as_ref().unwrap()).join(t.filename.as_ref().unwrap());
        let written = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(written.contains("正文"));
        // 列表可见（GUI 任务面板来源）
        let list = env.engine.list_tasks(Some(TaskStatus::Done)).await.unwrap();
        assert!(list.iter().any(|x| x.id == t.id));
    }

    #[tokio::test]
    async fn create_content_task_collection_dir_and_source_url() {
        // 文章合集模式：显式 dir = <default_dir>/<标题>/，文档与图片同目录
        let env = test_engine().await;
        let base = env._dir.path().join("dl");
        let coll = base.join("我的文章");
        let t = env
            .engine
            .create_content_task(CreateContentTask {
                filename: "我的文章.html".into(),
                mime: None,
                content: "<p>x</p>".into(),
                dir: Some(coll.display().to_string()),
                source_url: None,
            })
            .await
            .unwrap();
        assert_eq!(t.dir.as_deref(), Some(coll.display().to_string().as_str()));
        // 无原文链接：url 记 content://<落盘路径> 占位
        assert!(t.url.starts_with("content://"));
        let path = std::path::Path::new(t.dir.as_ref().unwrap()).join("我的文章.html");
        assert!(path.exists());

        // 防护：dir 含 ".." 段、source_url 非 http(s) → 拒绝
        let err = env
            .engine
            .create_content_task(CreateContentTask {
                filename: "x.html".into(),
                mime: None,
                content: "x".into(),
                dir: Some(base.join("..\\evil").display().to_string()),
                source_url: None,
            })
            .await
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidParams);
        let err = env
            .engine
            .create_content_task(CreateContentTask {
                filename: "x.html".into(),
                mime: None,
                content: "x".into(),
                dir: None,
                source_url: Some("javascript:alert(1)".into()),
            })
            .await
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidParams);
    }

    #[tokio::test]
    async fn create_content_task_renames_on_conflict_and_sanitizes() {
        let env = test_engine().await;
        let dir = env._dir.path().join("out");
        env.engine
            .set_setting("default_dir", dir.display().to_string().as_str())
            .await
            .unwrap();
        let mk = |name: &str| CreateContentTask {
            filename: name.into(),
            mime: None,
            content: "x".into(),
            dir: None,
            source_url: None,
        };
        let a = env.engine.create_content_task(mk("a.md")).await.unwrap();
        let b = env
            .engine
            .create_content_task(mk("a.md"))
            .await
            .unwrap();
        assert_eq!(b.filename.as_deref(), Some("a (1).md"), "冲突自动重命名");
        assert_ne!(a.id, b.id);
        // 文件名清洗：路径成分与非法字符
        let c = env
            .engine
            .create_content_task(mk("..\\evil<name>.md"))
            .await
            .unwrap();
        assert!(!c.filename.as_ref().unwrap().contains("..\\"));
        assert!(!c.filename.as_ref().unwrap().contains('<'));
    }

    #[tokio::test]
    async fn create_content_task_rejects_oversize_and_blank_name() {
        let env = test_engine().await;
        let err = env
            .engine
            .create_content_task(CreateContentTask {
                filename: "big.html".into(),
                mime: None,
                content: "x".repeat(Engine::MAX_CONTENT_BYTES + 1),
                dir: None,
                source_url: None,
            })
            .await
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidParams);

        let err = env
            .engine
            .create_content_task(CreateContentTask {
                filename: "   ".into(),
                mime: None,
                content: "x".into(),
                dir: None,
                source_url: None,
            })
            .await
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidParams);
    }

    #[tokio::test]
    async fn m3u8_url_autodetects_hls_kind() {
        let env = test_engine().await;
        let t = env
            .engine
            .create_task(CreateTask::of("https://example.com/video/index.m3u8"))
            .await
            .unwrap();
        assert_eq!(t.kind, TaskKind::Hls);
    }

    #[tokio::test]
    async fn youtube_url_autodetects_ytdlp_kind() {
        let env = test_engine().await;
        for url in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ",
            "https://m.youtube.com/shorts/abcdefghijk",
        ] {
            let t = env.engine.create_task(CreateTask::of(url)).await.unwrap();
            assert_eq!(t.kind, TaskKind::Ytdlp, "{url}");
        }
        // 非 YouTube 不受影响
        let t = env
            .engine
            .create_task(CreateTask::of("https://vimeo.com/12345"))
            .await
            .unwrap();
        assert_eq!(t.kind, TaskKind::Http);
    }

    #[tokio::test]
    async fn other_platform_urls_autodetect_ytdlp_kind() {
        let env = test_engine().await;
        for url in [
            "https://x.com/testuser/status/1700000000000000001",
            "https://mobile.twitter.com/i/web/status/1700000000000000001",
            "https://www.bilibili.com/video/BV1xx411c7mD",
            "https://b23.tv/abc123",
            "https://www.tiktok.com/@someuser/video/7301234567890123456",
            "https://www.douyin.com/video/7301234567890123456",
            "https://v.douyin.com/abcXYZ/",
        ] {
            let t = env.engine.create_task(CreateTask::of(url)).await.unwrap();
            assert_eq!(t.kind, TaskKind::Ytdlp, "{url}");
        }
        // 清单外域名仍是普通 HTTP（快手同类站点未纳入，需要时再扩）
        let t = env
            .engine
            .create_task(CreateTask::of(
                "https://www.kuaishou.com/short-video/3xabc123",
            ))
            .await
            .unwrap();
        assert_eq!(t.kind, TaskKind::Http);
    }

    #[tokio::test]
    async fn lifecycle_and_transition_guards() {
        let env = test_engine().await;
        let t = env
            .engine
            .create_task(CreateTask::of("https://example.com/a.zip"))
            .await
            .unwrap();

        let err = env.engine.pause(&t.id).await.unwrap_err();
        assert!(matches!(err.kind, ErrorKind::InvalidTransition { .. }));

        let canceled = env.engine.cancel(&t.id).await.unwrap();
        assert_eq!(canceled.status, TaskStatus::Canceled);

        let retried = env.engine.retry(&t.id).await.unwrap();
        assert_eq!(retried.status, TaskStatus::Queued);
        assert!(retried.error.is_none());

        assert!(env.engine.remove(&t.id).await.unwrap());
        assert!(env.engine.get_task(&t.id).await.unwrap().is_none());

        let err = env.engine.remove(&t.id).await.unwrap_err();
        assert_eq!(err.kind, ErrorKind::NotFound);
    }

    #[tokio::test]
    async fn rename_sanitizes_and_guards_active_states() {
        let env = test_engine().await;
        let t = env
            .engine
            .create_task(CreateTask::of("https://example.com/a.zip"))
            .await
            .unwrap();

        let renamed = env
            .engine
            .rename_task(&t.id, "my<new>:file.zip")
            .await
            .unwrap();
        assert_eq!(renamed.filename.as_deref(), Some("my-new--file.zip"));

        let err = env.engine.rename_task(&t.id, "   ").await.unwrap_err();
        assert_eq!(err.kind, ErrorKind::FileNameInvalid);

        env.engine.cancel(&t.id).await.unwrap();
        let renamed2 = env.engine.rename_task(&t.id, "final.zip").await.unwrap();
        assert_eq!(renamed2.filename.as_deref(), Some("final.zip"));
    }

    #[tokio::test]
    async fn remove_cleans_artifacts() {
        let env = test_engine().await;
        let out = tempfile::tempdir().unwrap();
        let t = env
            .engine
            .create_task(CreateTask {
                url: "https://example.com/a.zip".into(),
                kind: None,
                dir: Some(out.path().display().to_string()),
                filename: Some("a.zip".into()),
                priority: None,
                headers: None,
                checksum: None,
                mirrors: None,
                postprocess: None,
            })
            .await
            .unwrap();

        // 模拟产物残留：journal + .dlpart + hls tmp 目录
        let journal = env.engine.journal_dir().join(format!("{}.dlctrl", t.id));
        std::fs::create_dir_all(env.engine.journal_dir()).unwrap();
        std::fs::write(&journal, b"x").unwrap();
        let part = out.path().join("a.zip.dlpart");
        std::fs::write(&part, b"partial").unwrap();
        let hls_tmp = env.engine.root.join("dl-tmp").join(&t.id);
        std::fs::create_dir_all(&hls_tmp).unwrap();
        std::fs::write(hls_tmp.join("0"), b"seg").unwrap();

        env.engine.cancel(&t.id).await.unwrap();
        assert!(env.engine.remove(&t.id).await.unwrap());

        assert!(!journal.exists(), "journal 必须清理");
        assert!(!part.exists(), ".dlpart 必须清理");
        assert!(!hls_tmp.exists(), "hls 临时目录必须清理");
    }

    #[tokio::test]
    async fn duplicate_active_url_rejected() {
        let env = test_engine().await;
        let t = env
            .engine
            .create_task(CreateTask::of("https://example.com/dup.zip"))
            .await
            .unwrap();
        let err = env
            .engine
            .create_task(CreateTask::of("https://example.com/dup.zip"))
            .await
            .unwrap_err();
        assert_eq!(err.kind.code(), "task_exists");
        // 完结后可重新创建
        env.engine.cancel(&t.id).await.unwrap();
        env.engine
            .create_task(CreateTask::of("https://example.com/dup.zip"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn updated_since_returns_changed_rows() {
        let env = test_engine().await;
        let t = env
            .engine
            .create_task(CreateTask::of("https://example.com/a.zip"))
            .await
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(3)); // 保证毫秒时钟前进
        let after = now_ms();
        let rows = env.engine.list_updated_since(after).await.unwrap();
        assert!(rows.is_empty(), "创建后的增量查询（since 更晚）应为空");
        std::thread::sleep(std::time::Duration::from_millis(2)); // 保证 cancel 的 updated_at 严格晚于 after
        env.engine.cancel(&t.id).await.unwrap();
        let rows = env.engine.list_updated_since(after).await.unwrap();
        assert_eq!(rows.len(), 1, "取消（updated_at 变更）应出现在增量里");
    }

    #[tokio::test]
    async fn auto_categorize_sets_subdir() {
        let env = test_engine().await;
        env.engine
            .set_setting("auto_categorize", "on")
            .await
            .unwrap();
        env.engine
            .set_setting("default_dir", "D:/DL")
            .await
            .unwrap();

        let t = env
            .engine
            .create_task(CreateTask::of("https://example.com/movie.mp4"))
            .await
            .unwrap();
        assert!(
            t.dir
                .as_deref()
                .unwrap()
                .replace(std::path::MAIN_SEPARATOR, "/")
                .ends_with("DL/视频"),
            "dir = {:?}",
            t.dir
        );

        // 未分类类型：落在默认目录
        let t2 = env
            .engine
            .create_task(CreateTask::of("https://example.com/file.xyz"))
            .await
            .unwrap();
        assert_eq!(t2.dir.as_deref(), Some("D:/DL"));

        // 显式目录优先于分类
        let t3 = env
            .engine
            .create_task(CreateTask {
                dir: Some("E:/Custom".into()),
                ..CreateTask::of("https://example.com/other/movie.mp4")
            })
            .await
            .unwrap();
        assert_eq!(t3.dir.as_deref(), Some("E:/Custom"));
    }

    #[tokio::test]
    async fn settings_roundtrip() {
        let env = test_engine().await;
        assert_eq!(env.engine.get_setting("segments").await.unwrap(), None);
        env.engine.set_setting("segments", "12").await.unwrap();
        assert_eq!(
            env.engine.get_setting("segments").await.unwrap(),
            Some("12".to_string())
        );
        env.engine.set_setting("segments", "4").await.unwrap();
        assert_eq!(
            env.engine.get_setting("segments").await.unwrap(),
            Some("4".to_string())
        );
    }
}
