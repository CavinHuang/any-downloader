//! yt-dlp sidecar 驱动（docs/MEDIA-ADAPTERS.md §5.2）。
//!
//! YouTube 自 2025 起强制 PO Token + SABR，自研解析不可行；托管模式把整条
//! 下载交给 yt-dlp（含音视频合并），本 crate 只负责：拉起进程、解析进度行、
//! 取消时杀进程、定位产出文件。引擎侧（dl-engine::ytdlp_task）把进度桥接成
//! EngineEvent::Progress 并接管完成后的改名/校验。

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

/// 进度行模板前缀（--progress-template 自定义，避免与普通日志混淆）。
const PROGRESS_PREFIX: &str = "adl|";
/// 标题打印前缀（--print before_dl:adl_title|%(title)s，下载前实时回填真实标题）。
const TITLE_PREFIX: &str = "adl_title|";

/// 进度模板字段。total_bytes 与 total_bytes_estimate 必须都带上：yt-dlp 二者
/// 互斥（Content-Length 已知 → 只有 total_bytes；未知（HLS/SABR）→ 只有
/// estimate），只取 estimate 会让绝大多数普通下载拿到 NA、进度条不可判读。
/// _percent_str/_speed_str 是右对齐带前导空格的预格式化串（'  N/A%'、
/// ' Unknown B/s'，见 yt-dlp downloader/common.py format_percent/format_speed），
/// 解析端按字段 trim。
const PROGRESS_FIELDS: &str = "%(progress._percent_str)s|%(progress.downloaded_bytes)s|%(progress.total_bytes)s|%(progress.total_bytes_estimate)s|%(progress._speed_str)s";

/// 单行读取上限。超出即截断并丢弃该行剩余字节（异常插件/二进制输出不能吃爆内存）。
const MAX_LINE_BYTES: usize = 256 * 1024;
const TRUNCATED_MARK: &str = "…[adl: line truncated]";

/// 错误摘要保留的最后日志行数（退出码本身不携带原因，摘要让用户看到
/// "Sign in to confirm…"、"Video unavailable" 等真实失败原因）。
const TAIL_LINES: usize = 8;
/// 摘要单行截断宽度（chars，非 bytes）。
const TAIL_LINE_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyPolicy {
    /// 跟随系统代理（传入探测到的系统/环境代理以便 yt-dlp 与子进程 ffmpeg/deno 保持一致）。
    System(Option<String>),
    /// 显式禁用代理（`--proxy ""` 并清除代理环境变量）。
    Direct,
    /// 使用指定自定义代理（`--proxy <url>` 并同步代理环境变量）。
    Custom(String),
}

impl Default for ProxyPolicy {
    fn default() -> Self {
        Self::System(None)
    }
}

const PROXY_ENV_KEYS: [&str; 6] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
];

fn apply_proxy_policy(args: &mut Vec<String>, cmd: &mut Command, policy: &ProxyPolicy) {
    match policy {
        ProxyPolicy::Direct => {
            args.push("--proxy".into());
            args.push(String::new());
            for k in PROXY_ENV_KEYS {
                cmd.env_remove(k);
            }
        }
        ProxyPolicy::Custom(url) | ProxyPolicy::System(Some(url)) => {
            let trimmed = url.trim();
            if !trimmed.is_empty() {
                args.push("--proxy".into());
                args.push(trimmed.to_string());
                for k in PROXY_ENV_KEYS {
                    cmd.env(k, trimmed);
                }
            }
        }
        ProxyPolicy::System(None) => {}
    }
}

#[derive(Debug, Clone)]
pub struct YtdlpSpec {
    pub url: String,
    /// 可执行文件路径或 PATH 上的名字（"yt-dlp"）。
    pub exe: String,
    /// 输出基路径（不含扩展名；yt-dlp 合并后自动加 .mp4/.mkv 等）。
    pub out_path: PathBuf,
    /// ffmpeg 所在目录（合并 TS/fMP4 需要；None = 依赖 PATH）。
    pub ffmpeg_dir: Option<PathBuf>,
    /// 透传附加参数（如 --cookies、清晰度选择）。
    pub extra_args: Vec<String>,
    /// 追加到子进程 PATH 前部的目录（内置 yt-dlp/deno/ffmpeg 的安装目录）。
    pub path_prepend: Vec<PathBuf>,
    /// 代理策略（系统代理 / 手动代理 / 直连）。
    pub proxy: ProxyPolicy,
}

#[derive(Debug, Clone)]
pub enum YtdlpEvent {
    Progress {
        downloaded: u64,
        total: Option<u64>,
        bps: u64,
    },
    /// 下载前捕获到的真实视频标题（来自 --print before_dl:adl_title|%(title)s）。
    Title(String),
    /// 原始日志行（stderr 与非进度 stdout）。
    Log(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum YtdlpError {
    /// 用户取消（token 触发、进程已杀）。
    Canceled,
    /// 失败，消息含退出状态与 yt-dlp 日志尾部摘要。
    Failed(String),
}

impl std::fmt::Display for YtdlpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            YtdlpError::Canceled => write!(f, "canceled"),
            YtdlpError::Failed(m) => write!(f, "{m}"),
        }
    }
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn hide_window(cmd: &mut Command) {
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    #[cfg(not(windows))]
    let _ = cmd;
}

/// 快速可用性探测：`yt-dlp --version` 成功退出即视为可用（支持附带 extra PATH）。
/// 挂 10s 超时——配置成 GUI 程序/坏路径导致 spawn 挂死时任务不能永远停在 Probing。
pub async fn available_in_path(exe: &str, path_prepend: &[PathBuf]) -> bool {
    let mut cmd = Command::new(exe);
    hide_window(&mut cmd);
    cmd.arg("--version");
    if !path_prepend.is_empty() {
        let current = std::env::var_os("PATH").unwrap_or_default();
        let dirs = path_prepend
            .iter()
            .cloned()
            .chain(std::env::split_paths(&current));
        if let Ok(joined) = std::env::join_paths(dirs) {
            cmd.env("PATH", joined);
        }
    }
    tokio::time::timeout(Duration::from_secs(10), cmd.output())
        .await
        .map(|res| matches!(res, Ok(out) if out.status.success()))
        .unwrap_or(false)
}

pub async fn available(exe: &str) -> bool {
    available_in_path(exe, &[]).await
}

/// yt-dlp 链接探测输入规格（单视频格式列表 / 播放列表展开）。
#[derive(Debug, Clone)]
pub struct InspectSpec {
    pub url: String,
    pub exe: String,
    pub extra_args: Vec<String>,
    pub path_prepend: Vec<PathBuf>,
    pub proxy: ProxyPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormatOption {
    pub format_id: String,
    pub label: String,
    pub ext: String,
    pub resolution: Option<String>,
    pub fps: Option<u32>,
    pub filesize: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaylistEntry {
    pub id: String,
    pub title: String,
    pub url: String,
    pub duration_sec: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InspectResult {
    Video {
        title: String,
        duration_sec: Option<f64>,
        thumbnail: Option<String>,
        formats: Vec<FormatOption>,
    },
    Playlist {
        title: String,
        total_count: usize,
        entries: Vec<PlaylistEntry>,
    },
}

/// 快速探测 yt-dlp 视频信息或播放列表（30s 超时）。
pub async fn inspect(spec: InspectSpec) -> Result<InspectResult, YtdlpError> {
    let mut args = vec![
        spec.url.clone(),
        "--flat-playlist".into(),
        "--dump-single-json".into(),
        "--no-warnings".into(),
        "--skip-download".into(),
    ];
    args.extend(spec.extra_args.iter().cloned());

    let mut cmd = Command::new(&spec.exe);
    hide_window(&mut cmd);
    apply_proxy_policy(&mut args, &mut cmd, &spec.proxy);
    cmd.args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if !spec.path_prepend.is_empty() {
        let current = std::env::var_os("PATH").unwrap_or_default();
        let dirs = spec
            .path_prepend
            .iter()
            .cloned()
            .chain(std::env::split_paths(&current));
        if let Ok(joined) = std::env::join_paths(dirs) {
            cmd.env("PATH", joined);
        }
    }

    let output = tokio::time::timeout(Duration::from_secs(30), cmd.output())
        .await
        .map_err(|_| YtdlpError::Failed("yt-dlp inspect timed out (30s)".into()))?
        .map_err(|e| YtdlpError::Failed(format!("spawn {}: {e}", spec.exe)))?;

    if !output.status.success() {
        let err_tail = String::from_utf8_lossy(&output.stderr)
            .lines()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join(" | ");
        return Err(YtdlpError::Failed(format!(
            "yt-dlp inspect exited with {}: {}",
            output.status, err_tail
        )));
    }
    let json_text = String::from_utf8_lossy(&output.stdout);
    parse_inspect_json(&json_text, &spec.url)
}

pub fn parse_inspect_json(json_text: &str, input_url: &str) -> Result<InspectResult, YtdlpError> {
    let v: serde_json::Value = serde_json::from_str(json_text.trim())
        .map_err(|e| YtdlpError::Failed(format!("invalid yt-dlp json: {e}")))?;

    if let Some(entries_arr) = v.get("entries").and_then(|e| e.as_array()) {
        if !entries_arr.is_empty() || v.get("_type").and_then(|t| t.as_str()) == Some("playlist") {
            let title = v
                .get("title")
                .and_then(|s| s.as_str())
                .unwrap_or("Playlist")
                .trim()
                .to_string();
            let total_count = entries_arr.len();
            let mut entries = Vec::with_capacity(entries_arr.len().min(200));
            for item in entries_arr.iter().take(200) {
                let id = item
                    .get("id")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                let etitle = item
                    .get("title")
                    .and_then(|s| s.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or(if id.is_empty() { "Video" } else { &id })
                    .to_string();
                let entry_url = resolve_entry_url(item, &id, input_url);
                let Some(entry_url) = entry_url else {
                    continue;
                };
                let duration_sec = item.get("duration").and_then(|d| d.as_f64());
                entries.push(PlaylistEntry {
                    id,
                    title: etitle,
                    url: entry_url,
                    duration_sec,
                });
            }
            return Ok(InspectResult::Playlist {
                title,
                total_count,
                entries,
            });
        }
    }

    let title = v
        .get("title")
        .and_then(|s| s.as_str())
        .unwrap_or("Video")
        .trim()
        .to_string();
    let duration_sec = v.get("duration").and_then(|d| d.as_f64());
    let thumbnail = v
        .get("thumbnail")
        .and_then(|s| s.as_str())
        .map(str::to_string);

    let mut formats = Vec::new();
    let mut seen_heights = std::collections::BTreeSet::new();
    if let Some(fmts) = v.get("formats").and_then(|f| f.as_array()) {
        struct FormatCandidate {
            height: u32,
            fps: Option<u32>,
            filesize: Option<u64>,
            ext: String,
            res: String,
        }
        // 按分辨率高度降序整理主流清晰度档位（选择器用 bv*[height<=H]+ba/b 兼容分离流与直出流）
        let mut candidates: Vec<FormatCandidate> = Vec::new();
        for f in fmts {
            let vcodec = f.get("vcodec").and_then(|s| s.as_str()).unwrap_or("none");
            if vcodec == "none" {
                continue;
            }
            let Some(h) = f.get("height").and_then(|n| n.as_u64()).map(|n| n as u32) else {
                continue;
            };
            if h < 144 {
                continue;
            }
            let w = f.get("width").and_then(|n| n.as_u64()).map(|n| n as u32);
            let res = w
                .map(|ww| format!("{ww}x{h}"))
                .unwrap_or_else(|| format!("{h}p"));
            let fps = f
                .get("fps")
                .and_then(|n| n.as_f64())
                .map(|x| x.round() as u32);
            let ext = f
                .get("ext")
                .and_then(|s| s.as_str())
                .unwrap_or("mp4")
                .to_string();
            let fs = f
                .get("filesize")
                .or_else(|| f.get("filesize_approx"))
                .and_then(|n| n.as_u64());
            candidates.push(FormatCandidate {
                height: h,
                fps,
                filesize: fs,
                ext,
                res,
            });
        }
        candidates.sort_by(|a, b| b.height.cmp(&a.height).then(b.filesize.cmp(&a.filesize)));
        for c in candidates {
            if !seen_heights.insert(c.height) {
                continue;
            }
            let fps_tag = c
                .fps
                .filter(|&f| f > 30)
                .map(|f| format!("{f}fps"))
                .unwrap_or_default();
            let label = if fps_tag.is_empty() {
                format!("{}p", c.height)
            } else {
                format!("{}p {fps_tag}", c.height)
            };
            formats.push(FormatOption {
                format_id: format!("bv*[height<={}]+ba/b[height<={}]/b", c.height, c.height),
                label,
                ext: c.ext,
                resolution: Some(c.res),
                fps: c.fps,
                filesize: c.filesize,
            });
        }
    }
    formats.push(FormatOption {
        format_id: "bestaudio[ext=m4a]/bestaudio/best".into(),
        label: "仅音频 (M4A/最佳音频)".into(),
        ext: "m4a".into(),
        resolution: None,
        fps: None,
        filesize: None,
    });

    Ok(InspectResult::Video {
        title,
        duration_sec,
        thumbnail,
        formats,
    })
}

fn resolve_entry_url(item: &serde_json::Value, id: &str, input_url: &str) -> Option<String> {
    for key in ["webpage_url", "original_url", "url"] {
        if let Some(u) = item.get(key).and_then(|s| s.as_str()) {
            if u.starts_with("http://") || u.starts_with("https://") {
                return Some(u.to_string());
            }
        }
    }
    if id.is_empty() {
        return None;
    }
    let ie = item
        .get("ie_key")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let lower_in = input_url.to_ascii_lowercase();
    if ie.contains("youtube") || lower_in.contains("youtube.com") || lower_in.contains("youtu.be") {
        return Some(format!("https://www.youtube.com/watch?v={id}"));
    }
    if (ie.contains("bilibili") || lower_in.contains("bilibili.com"))
        && (id.starts_with("BV") || id.starts_with("av"))
    {
        return Some(format!("https://www.bilibili.com/video/{id}"));
    }
    None
}

/// 运行到结束。取消 = 杀进程并返回 `YtdlpError::Canceled`；
/// 失败时错误消息附带 yt-dlp 日志尾部摘要。
pub async fn run(
    spec: YtdlpSpec,
    cancel: CancellationToken,
    mut on_event: impl FnMut(YtdlpEvent) + Send,
) -> Result<PathBuf, YtdlpError> {
    let mut args: Vec<String> = vec![
        spec.url.clone(),
        "-o".into(),
        // -o 的值是模板：字面 % 会被当成 %(field)s 引出，必须转义成 %%；追加 .%(ext)s 保证真实容器扩展名
        format!(
            "{}.%(ext)s",
            escape_percent(&spec.out_path.display().to_string())
        ),
        "--newline".into(),
        "--no-part".into(),
        // 不写视频原始 mtime：produced() 依赖文件 mtime 区分本次产出与残留
        "--no-mtime".into(),
        "--no-playlist".into(),
        "--no-simulate".into(),
        "--no-quiet".into(),
        "--progress".into(),
        "--print".into(),
        format!("before_dl:{TITLE_PREFIX}%(title)s"),
        "--progress-template".into(),
        format!("{PROGRESS_PREFIX}{PROGRESS_FIELDS}"),
    ];
    if let Some(ff) = &spec.ffmpeg_dir {
        args.push("--ffmpeg-location".into());
        args.push(ff.display().to_string());
    }
    args.extend(spec.extra_args.iter().cloned());

    let mut cmd = Command::new(&spec.exe);
    hide_window(&mut cmd);
    apply_proxy_policy(&mut args, &mut cmd, &spec.proxy);
    cmd.args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    // 内置 sidecar 目录注入 PATH：yt-dlp 据此找到同目录的 deno（nsig 解签）
    if !spec.path_prepend.is_empty() {
        // join_paths/split_paths 用平台正确的分隔符，不硬编码 ";"
        let current = std::env::var_os("PATH").unwrap_or_default();
        let dirs = spec
            .path_prepend
            .iter()
            .cloned()
            .chain(std::env::split_paths(&current));
        if let Ok(joined) = std::env::join_paths(dirs) {
            cmd.env("PATH", joined);
        }
    }
    // mtime 基准取 spawn 之前（时钟粗糙时宁可偏松，produced() 端还有 2s 容差）
    let started_at = SystemTime::now();
    let mut child = cmd
        .spawn()
        .map_err(|e| YtdlpError::Failed(format!("spawn {}: {e}", spec.exe)))?;

    let mut stdout = tokio::io::BufReader::new(child.stdout.take().unwrap());
    let mut stderr = tokio::io::BufReader::new(child.stderr.take().unwrap());
    let mut out_buf: Vec<u8> = Vec::new();
    let mut err_buf: Vec<u8> = Vec::new();
    // stderr 先于 stdout 关闭时置 false，否则 select! 每轮都拿到 Ok(None) 空转
    let mut stderr_open = true;
    let mut tail: VecDeque<String> = VecDeque::new();
    loop {
        tokio::select! {
            // biased：取消优先于就绪的 I/O，杀进程不等缓冲排空
            biased;
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(YtdlpError::Canceled);
            }
            line = async {
                if stderr_open {
                    next_line_capped(&mut stderr, &mut err_buf).await
                } else {
                    std::future::pending::<()>().await;
                    unreachable!()
                }
            } => {
                match line {
                    Ok(Some(l)) => handle_line(&l, &mut tail, &mut on_event),
                    // stderr 关闭（少见，通常与进程同寿命）：停轮询，继续等 stdout
                    Ok(None) => stderr_open = false,
                    Err(e) => {
                        let _ = child.kill().await;
                        return Err(YtdlpError::Failed(format!("stderr: {e}")));
                    }
                }
            }
            line = next_line_capped(&mut stdout, &mut out_buf) => {
                match line {
                    Ok(Some(l)) => handle_line(&l, &mut tail, &mut on_event),
                    Ok(None) => break,
                    Err(e) => {
                        let _ = child.kill().await;
                        return Err(YtdlpError::Failed(format!("stdout: {e}")));
                    }
                }
            }
        }
    }

    let status = child
        .wait()
        .await
        .map_err(|e| YtdlpError::Failed(format!("wait: {e}")))?;
    if !status.success() {
        // 只报退出码用户看不到原因（风控/版权/网络错误都在日志里），附上尾部摘要
        return Err(YtdlpError::Failed(format!(
            "yt-dlp exited with {status}: {}",
            summarize_tail(&tail)
        )));
    }
    produced(&spec.out_path, started_at).ok_or_else(|| {
        YtdlpError::Failed(format!(
            "yt-dlp produced no file: {}",
            summarize_tail(&tail)
        ))
    })
}

/// 一行日志的去重处理：adl_title|... → Title；adl|... 进度行 → Progress；
/// 普通日志 → 尾部摘要缓冲 + Log 事件。
fn handle_line(l: &str, tail: &mut VecDeque<String>, on_event: &mut impl FnMut(YtdlpEvent)) {
    if let Some(raw_title) = l.trim().strip_prefix(TITLE_PREFIX) {
        let t = raw_title.trim();
        if !t.is_empty() && !t.eq_ignore_ascii_case("NA") && !t.eq_ignore_ascii_case("None") {
            on_event(YtdlpEvent::Title(t.to_string()));
        }
        return;
    }
    if let Some(p) = parse_progress(l) {
        on_event(p);
    } else if !l.trim().is_empty() {
        record_tail(tail, l);
        on_event(YtdlpEvent::Log(l.to_string()));
    }
}

fn record_tail(tail: &mut VecDeque<String>, line: &str) {
    let trimmed = line.trim();
    let mut s: String = trimmed.chars().take(TAIL_LINE_CHARS).collect();
    if trimmed.chars().count() > TAIL_LINE_CHARS {
        s.push('…');
    }
    if tail.len() >= TAIL_LINES {
        tail.pop_front();
    }
    tail.push_back(s);
}

fn summarize_tail(tail: &VecDeque<String>) -> String {
    if tail.is_empty() {
        return "no output".into();
    }
    tail.iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(" | ")
}

/// 读一行（\n 结尾）。超过 MAX_LINE_BYTES 的行：保留前缀 + 截断标记，并丢弃
/// 该行剩余字节直到下一个换行。EOF 前的残余（无换行）也作为一行返回。
async fn next_line_capped<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
) -> std::io::Result<Option<String>> {
    buf.clear();
    let mut truncated = false;
    loop {
        let (done, consumed) = {
            let available = reader.fill_buf().await?;
            if available.is_empty() {
                break; // EOF
            }
            match available.iter().position(|&b| b == b'\n') {
                Some(i) => {
                    if !truncated {
                        let take = i.min(MAX_LINE_BYTES.saturating_sub(buf.len()));
                        buf.extend_from_slice(&available[..take]);
                        truncated = take < i;
                    }
                    (true, i + 1)
                }
                None => {
                    if !truncated {
                        let take = available
                            .len()
                            .min(MAX_LINE_BYTES.saturating_sub(buf.len()));
                        buf.extend_from_slice(&available[..take]);
                        truncated = take < available.len();
                    }
                    (false, available.len())
                }
            }
        };
        reader.consume(consumed);
        if done {
            break;
        }
    }
    if buf.is_empty() && !truncated {
        return Ok(None);
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    let mut s = String::from_utf8_lossy(buf).into_owned();
    if truncated {
        s.push_str(TRUNCATED_MARK);
    }
    Ok(Some(s))
}

/// `adl| 52.3%|10485760|20000000|NA|   1.2MiB/s` → Progress
/// （_percent_str/_speed_str 带前导空格；downloaded 解析失败的行按日志处理）
fn parse_progress(line: &str) -> Option<YtdlpEvent> {
    let rest = line.strip_prefix(PROGRESS_PREFIX)?;
    let mut it = rest.split('|');
    let _pct = it.next()?;
    let downloaded = it.next()?.trim().parse::<u64>().ok()?;
    let total_field = it.next();
    let estimate_field = it.next();
    let total = total_field
        .and_then(opt_bytes)
        .or_else(|| estimate_field.and_then(opt_bytes));
    let bps = parse_speed(it.next().unwrap_or("")).unwrap_or(0);
    Some(YtdlpEvent::Progress {
        downloaded,
        total,
        bps,
    })
}

/// yt-dlp 裸数值字段："NA"/"None"/空 = 无值 → None。
fn opt_bytes(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty()
        || s.eq_ignore_ascii_case("na")
        || s.eq_ignore_ascii_case("none")
        || s.eq_ignore_ascii_case("unknown")
    {
        return None;
    }
    s.parse::<u64>().ok()
}

/// yt-dlp 速度字符串 → B/s（"1.2MiB/s"、"350.5KiB/s"、"Unknown B/s"、"N/A"；
/// 均可能带右对齐前导空格）
fn parse_speed(s: &str) -> Option<u64> {
    let s = s.trim().trim_end_matches("/s").trim();
    let (num, unit) = s.split_at(
        s.find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(s.len()),
    );
    let v: f64 = num.trim().parse().ok()?;
    let mult = match unit.trim().to_ascii_uppercase().as_str() {
        "KIB" => 1024.0,
        "MIB" => 1024.0 * 1024.0,
        "GIB" => 1024.0 * 1024.0 * 1024.0,
        "B" | "" => 1.0,
        _ => return None,
    };
    Some((v * mult) as u64)
}

/// yt-dlp 把 -o 的值当输出模板求值，`%` 引出 %(field)s；路径里的字面 `%`
/// （如 `C:\Users\100%\…`）必须写成 `%%`，否则模板解析直接报错。
fn escape_percent(s: &str) -> String {
    s.replace('%', "%%")
}

/// 定位产出文件：精确路径 + `{基名}.{任意}` 前缀扫描（yt-dlp 自动追加扩展名，
/// 我们无法预知最终容器格式）。同目录可能残留早前任务的同名成品——`--no-part`
/// 下中断也会留下截断的最终名文件——因此以「mtime ≥ 进程启动（2s 时钟容差）」
/// 为首选、mtime 最新为兜底，否则音频任务会扫到旧任务留下的 .mp4 并把它当成
/// 自己的产出改名走。
fn produced(out: &Path, started_at: SystemTime) -> Option<PathBuf> {
    let name = out.file_name()?.to_string_lossy().into_owned();
    let prefix = format!("{name}.");
    let dir = out.parent().unwrap_or_else(|| Path::new("."));
    let entries = std::fs::read_dir(dir).ok()?;
    let slack = Duration::from_secs(2);
    let mut best_fresh: Option<(SystemTime, PathBuf)> = None;
    let mut best_any: Option<(SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let p = entry.path();
        if !p.is_file() {
            continue; // 同名目录不算产出
        }
        let fname = entry.file_name().to_string_lossy().into_owned();
        if fname != name && !fname.starts_with(&prefix) {
            continue;
        }
        let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if best_any.as_ref().is_none_or(|(t, _)| mtime > *t) {
            best_any = Some((mtime, p.clone()));
        }
        if mtime + slack >= started_at && best_fresh.as_ref().is_none_or(|(t, _)| mtime > *t) {
            best_fresh = Some((mtime, p));
        }
    }
    best_fresh.or(best_any).map(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_line_parses_with_total_bytes() {
        // total_bytes 已知（Content-Length）、estimate=NA：二者互斥
        let ev = parse_progress("adl|52.3%|10485760|20000000|NA|1.2MiB/s");
        match ev {
            Some(YtdlpEvent::Progress {
                downloaded,
                total,
                bps,
            }) => {
                assert_eq!(downloaded, 10485760);
                assert_eq!(total, Some(20000000));
                assert_eq!(bps, 1258291);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn progress_padding_fields_parse() {
        // _percent_str/_speed_str 右对齐带前导空格
        let ev = parse_progress("adl|  42.7%|1024|4096|NA|   1.2MiB/s");
        match ev {
            Some(YtdlpEvent::Progress { downloaded, .. }) => assert_eq!(downloaded, 1024),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn progress_estimate_fallback_when_total_na() {
        // 未知大小（HLS/SABR）：total_bytes=NA、estimate 有值
        let ev = parse_progress("adl|  3.0%|512|NA|4096| Unknown B/s");
        match ev {
            Some(YtdlpEvent::Progress { total, bps, .. }) => {
                assert_eq!(total, Some(4096));
                assert_eq!(bps, 0);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn progress_na_speed_and_unknown_total() {
        let ev = parse_progress("adl|  N/A%|1000|NA|NA| Unknown B/s");
        match ev {
            Some(YtdlpEvent::Progress {
                downloaded,
                total,
                bps,
            }) => {
                assert_eq!(downloaded, 1000);
                assert_eq!(total, None);
                assert_eq!(bps, 0);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn non_progress_lines_ignored() {
        assert!(parse_progress("[youtube] Extracting URL").is_none());
        assert!(parse_progress("").is_none());
        // downloaded=NA（finished 事件等）：不当作进度
        assert!(parse_progress("adl|100.0%|NA|12345|NA| Unknown B/s").is_none());
    }

    #[test]
    fn speed_units() {
        assert_eq!(parse_speed("350.5KiB/s"), Some(358912));
        assert_eq!(parse_speed("2GiB/s"), Some(2147483648));
        assert_eq!(parse_speed("Unknown B/s"), None);
        assert_eq!(parse_speed("N/A"), None);
        assert_eq!(parse_speed("   1.2MiB/s"), Some(1258291));
    }

    #[test]
    fn percent_in_out_path_is_escaped() {
        assert_eq!(escape_percent("C:\\Users\\100%\\v"), "C:\\Users\\100%%\\v");
        assert_eq!(escape_percent("no percent"), "no percent");
    }

    #[tokio::test]
    async fn capped_reader_truncates_overlong_line() {
        let mut data = vec![b'x'; MAX_LINE_BYTES + 1000];
        data.push(b'\n');
        data.extend_from_slice(b"ok\n");
        let mut reader: &[u8] = &data;
        let mut buf = Vec::new();
        let first = next_line_capped(&mut reader, &mut buf)
            .await
            .unwrap()
            .unwrap();
        assert!(first.ends_with(TRUNCATED_MARK), "{first:?}");
        // 截断不吞掉行内剩余字节，后续行照常
        let second = next_line_capped(&mut reader, &mut buf)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second, "ok");
        assert!(next_line_capped(&mut reader, &mut buf)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn capped_reader_handles_crlf_and_eof_tail() {
        let mut reader: &[u8] = b"a\r\nlast-line-no-newline";
        let mut buf = Vec::new();
        assert_eq!(
            next_line_capped(&mut reader, &mut buf)
                .await
                .unwrap()
                .unwrap(),
            "a"
        );
        assert_eq!(
            next_line_capped(&mut reader, &mut buf)
                .await
                .unwrap()
                .unwrap(),
            "last-line-no-newline"
        );
        assert!(next_line_capped(&mut reader, &mut buf)
            .await
            .unwrap()
            .is_none());
    }

    #[test]
    fn produced_prefers_file_created_after_start() {
        let dir = tempfile::tempdir().unwrap();
        let stale = dir.path().join("v.mp4");
        std::fs::write(&stale, b"old video from a previous task").unwrap();
        std::thread::sleep(Duration::from_millis(80));
        let started = SystemTime::now();
        let fresh = dir.path().join("v.m4a");
        std::fs::write(&fresh, b"fresh audio").unwrap();
        assert_eq!(produced(&dir.path().join("v"), started), Some(fresh));
    }

    #[test]
    fn produced_picks_newest_when_nothing_fresh() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("v.mp4"), b"older").unwrap();
        std::thread::sleep(Duration::from_millis(80));
        let newest = dir.path().join("v.mkv");
        std::fs::write(&newest, b"newest but stale").unwrap();
        std::thread::sleep(Duration::from_millis(80));
        // 两个候选都在启动之前 → 无 fresh，取 mtime 最新的
        let started = SystemTime::now();
        assert_eq!(produced(&dir.path().join("v"), started), Some(newest));
    }

    #[test]
    fn produced_scans_prefixed_intermediate_names() {
        let dir = tempfile::tempdir().unwrap();
        let started = SystemTime::now();
        let f = dir.path().join("v.f137.mp4");
        std::fs::write(&f, b"").unwrap();
        assert_eq!(produced(&dir.path().join("v"), started), Some(f));
    }

    #[test]
    fn produced_none_for_missing_or_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(produced(&dir.path().join("v"), SystemTime::now()), None);
        std::fs::create_dir(dir.path().join("v.mp4")).unwrap();
        assert_eq!(produced(&dir.path().join("v"), SystemTime::now()), None);
    }

    #[test]
    fn tail_summary_truncates_and_joins() {
        let mut tail = VecDeque::new();
        for i in 0..(TAIL_LINES + 2) {
            record_tail(&mut tail, &format!("line{i}"));
        }
        // 只留最后 TAIL_LINES 条
        assert_eq!(tail.front().unwrap(), "line2");
        let s = summarize_tail(&tail);
        assert!(s.starts_with("line2 | "));
        // 超长行按 chars 截断
        let mut one = VecDeque::new();
        record_tail(&mut one, &"很".repeat(400));
        assert_eq!(one.front().unwrap().chars().count(), TAIL_LINE_CHARS + 1);
    }

    #[tokio::test]
    async fn available_rejects_missing_exe() {
        assert!(!available("adl-definitely-missing-ytdlp-xyz").await);
    }

    #[test]
    fn parse_inspect_json_handles_video_and_playlist() {
        let video_json = r#"{
            "title": "Sample Video",
            "duration": 125.0,
            "thumbnail": "https://img.example/1.jpg",
            "formats": [
                {"vcodec": "none", "acodec": "mp4a", "ext": "m4a"},
                {"vcodec": "avc1", "height": 720, "width": 1280, "fps": 30, "ext": "mp4", "filesize": 1000},
                {"vcodec": "avc1", "height": 1080, "width": 1920, "fps": 60, "ext": "mp4", "filesize": 2000}
            ]
        }"#;
        let v = parse_inspect_json(video_json, "https://youtu.be/abc").unwrap();
        match v {
            InspectResult::Video {
                title,
                duration_sec,
                formats,
                ..
            } => {
                assert_eq!(title, "Sample Video");
                assert_eq!(duration_sec, Some(125.0));
                assert_eq!(formats[0].label, "1080p 60fps");
                assert_eq!(
                    formats[0].format_id,
                    "bv*[height<=1080]+ba/b[height<=1080]/b"
                );
                assert_eq!(formats[1].label, "720p");
                assert_eq!(
                    formats.last().unwrap().format_id,
                    "bestaudio[ext=m4a]/bestaudio/best"
                );
            }
            other => panic!("unexpected {other:?}"),
        }

        let pl_json = r#"{
            "_type": "playlist",
            "title": "Course List",
            "entries": [
                {"id": "vid1", "title": "Part 1", "duration": 60.0},
                {"id": "vid2", "title": "Part 2", "url": "https://www.youtube.com/watch?v=vid2"}
            ]
        }"#;
        let pl =
            parse_inspect_json(pl_json, "https://www.youtube.com/playlist?list=PL123").unwrap();
        match pl {
            InspectResult::Playlist {
                title,
                total_count,
                entries,
            } => {
                assert_eq!(title, "Course List");
                assert_eq!(total_count, 2);
                assert_eq!(entries[0].url, "https://www.youtube.com/watch?v=vid1");
                assert_eq!(entries[1].url, "https://www.youtube.com/watch?v=vid2");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn apply_proxy_policy_sets_args_cleanly() {
        let mut args = Vec::new();
        let mut cmd = Command::new("yt-dlp");
        apply_proxy_policy(&mut args, &mut cmd, &ProxyPolicy::Direct);
        assert_eq!(args, vec!["--proxy", ""]);

        let mut args2 = Vec::new();
        let mut cmd2 = Command::new("yt-dlp");
        apply_proxy_policy(
            &mut args2,
            &mut cmd2,
            &ProxyPolicy::Custom("socks5h://127.0.0.1:1080".into()),
        );
        assert_eq!(args2, vec!["--proxy", "socks5h://127.0.0.1:1080"]);

        let mut args3 = Vec::new();
        let mut cmd3 = Command::new("yt-dlp");
        apply_proxy_policy(
            &mut args3,
            &mut cmd3,
            &ProxyPolicy::System(Some("http://127.0.0.1:7890".into())),
        );
        assert_eq!(args3, vec!["--proxy", "http://127.0.0.1:7890"]);
    }

    #[test]
    fn handle_line_extracts_title_without_polluting_tail() {
        let mut tail = VecDeque::new();
        let mut events = Vec::new();
        handle_line("adl_title|Me at the zoo", &mut tail, &mut |ev| {
            events.push(ev);
        });
        handle_line("adl_title|NA", &mut tail, &mut |ev| {
            events.push(ev);
        });
        assert!(tail.is_empty());
        assert_eq!(events.len(), 1);
        match &events[0] {
            YtdlpEvent::Title(t) => assert_eq!(t, "Me at the zoo"),
            other => panic!("unexpected {other:?}"),
        }
    }
}
