use serde::{Deserialize, Serialize};

pub fn new_task_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Http,
    Hls,
    /// yt-dlp 托管模式（YouTube 等；docs/MEDIA-ADAPTERS.md §5.2）。
    Ytdlp,
    /// 内容任务（站点适配层文章导出，docs/MEDIA-ADAPTERS.md §11）：
    /// 内容由扩展生成经 task.add_content 送达，建任务即落盘完成，无下载阶段。
    Content,
}

impl TaskKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskKind::Http => "http",
            TaskKind::Hls => "hls",
            TaskKind::Ytdlp => "ytdlp",
            TaskKind::Content => "content",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "http" => Some(TaskKind::Http),
            "hls" => Some(TaskKind::Hls),
            "ytdlp" => Some(TaskKind::Ytdlp),
            "content" => Some(TaskKind::Content),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Queued,
    Probing,
    Downloading,
    Paused,
    WaitingRetry,
    Merging,
    Verifying,
    /// 下载完成后的后处理阶段（如视频号 Isaac64-XOR 解密）。
    Decrypting,
    Done,
    Failed,
    Canceled,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Queued => "queued",
            TaskStatus::Probing => "probing",
            TaskStatus::Downloading => "downloading",
            TaskStatus::Paused => "paused",
            TaskStatus::WaitingRetry => "waiting_retry",
            TaskStatus::Merging => "merging",
            TaskStatus::Verifying => "verifying",
            TaskStatus::Decrypting => "decrypting",
            TaskStatus::Done => "done",
            TaskStatus::Failed => "failed",
            TaskStatus::Canceled => "canceled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "queued" => TaskStatus::Queued,
            "probing" => TaskStatus::Probing,
            "downloading" => TaskStatus::Downloading,
            "paused" => TaskStatus::Paused,
            "waiting_retry" => TaskStatus::WaitingRetry,
            "merging" => TaskStatus::Merging,
            "verifying" => TaskStatus::Verifying,
            "decrypting" => TaskStatus::Decrypting,
            "done" => TaskStatus::Done,
            "failed" => TaskStatus::Failed,
            "canceled" => TaskStatus::Canceled,
            _ => return None,
        })
    }

    /// 引擎正在执行中的状态；这些状态下不允许直接删除任务。
    pub fn is_active(self) -> bool {
        matches!(
            self,
            TaskStatus::Probing
                | TaskStatus::Downloading
                | TaskStatus::Merging
                | TaskStatus::Verifying
                | TaskStatus::Decrypting
        )
    }
}

/// 下载完成后的后处理步骤（docs/MEDIA-ADAPTERS.md §5.1）。
/// 目前只有视频号的 Isaac64-XOR：密钥流 = Isaac64(seed=randrsl[0]=decode_key)
/// 生成 16384 个 u64 大端拼接（131072 字节）后整体 reverse，与文件前 128KB XOR。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PostProcess {
    pub algo: PostProcessAlgo,
    pub seed: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PostProcessAlgo {
    Isaac64Xor,
}

impl PostProcess {
    pub fn as_json(list: &[PostProcess]) -> String {
        serde_json::to_string(list).unwrap_or_else(|_| "[]".into())
    }

    pub fn parse_list(json: Option<&str>) -> Vec<PostProcess> {
        json.and_then(|j| serde_json::from_str(j).ok())
            .unwrap_or_default()
    }
}

/// 请求头透传白名单（DESIGN §9）：仅这些头允许从任务带到下载请求。
pub const HEADER_ALLOWLIST: &[&str] = &[
    "referer",
    "user-agent",
    "cookie",
    "authorization",
    "accept",
    "accept-language",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    /// 用户给定的原始 URL。续传一律从此处重走重定向链，不用 final_url
    /// （签名 URL 过期后 final_url 不可复活，见 DESIGN §5.3）。
    pub url: String,
    pub final_url: Option<String>,
    pub filename: Option<String>,
    pub dir: Option<String>,
    pub total_bytes: Option<u64>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub mime: Option<String>,
    pub kind: TaskKind,
    pub status: TaskStatus,
    pub priority: i32,
    /// 敏感请求头（Cookie/Authorization）经 OS keychain 存储，库里只存引用（M5 加固）。
    pub headers_ref: Option<String>,
    /// 透传请求头 JSON（仅白名单头；M1.5 以明文列过渡，keychain 迁移见 DESIGN §9）。
    pub headers_json: Option<String>,
    /// 已下载字节（暂停/失败后 UI 恢复显示；定期节流落库）。
    #[serde(default)]
    pub downloaded_bytes: u64,
    /// HLS master 的变体列表 JSON（[{bandwidth,url,resolution?}]，GUI 换清晰度用）。
    #[serde(default)]
    pub variants_json: Option<String>,
    /// 完成后哈希校验（"algo:hex"，规范化存储；None = 不校验）。
    #[serde(default)]
    pub checksum: Option<String>,
    /// 多镜像 URL 列表 JSON（不含主 URL；探测校验后启用）。
    #[serde(default)]
    pub mirrors_json: Option<String>,
    /// 后处理步骤 JSON（[{algo,seed}]；完成下载后按序执行）。
    #[serde(default)]
    pub postprocess_json: Option<String>,
    pub error: Option<dl_error::ErrorKind>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateTask {
    pub url: String,
    /// None 时由引擎探测决定；缺省视为 Http。
    pub kind: Option<TaskKind>,
    pub dir: Option<String>,
    pub filename: Option<String>,
    pub priority: Option<i32>,
    /// 嗅探带出的请求头（扩展透传）；非白名单头在入队时被剔除。
    #[serde(default)]
    pub headers: Option<std::collections::HashMap<String, String>>,
    /// 完成后哈希校验（"algo:hex" 或裸 hex，引擎规范化）。
    #[serde(default)]
    pub checksum: Option<String>,
    /// 附加镜像 URL（http/https，去重后 ≤8 个；主 URL 之外）。
    #[serde(default)]
    pub mirrors: Option<Vec<String>>,
    /// 下载完成后的后处理（如视频号解密；站点适配器透传）。
    #[serde(default)]
    pub postprocess: Option<Vec<PostProcess>>,
}

impl CreateTask {
    pub fn of(url: impl Into<String>) -> Self {
        CreateTask {
            url: url.into(),
            kind: None,
            dir: None,
            filename: None,
            priority: None,
            headers: None,
            checksum: None,
            mirrors: None,
            postprocess: None,
        }
    }
}

/// 内容任务入参（IPC task.add_content）：扩展侧生成的文章 HTML/Markdown，
/// 建任务即落盘（直接 Done），不经过下载管线。
#[derive(Debug, Clone, Deserialize)]
pub struct CreateContentTask {
    /// 已由扩展按 dl-filename 规则预清洗；引擎侧再过一遍 sanitize 兜底。
    pub filename: String,
    /// 如 "text/html;charset=utf-8"；仅记录展示，不影响落盘。
    #[serde(default)]
    pub mime: Option<String>,
    /// 文档正文（UTF-8 文本；帧上限 32 MiB 兜底，见 dl-ipc MAX_REQUEST_BYTES）。
    pub content: String,
    /// 目标目录（绝对路径）。缺省 = default_dir 设置（auto_categorize → 「文档」）。
    /// 文章合集模式：扩展把同批文档+图片统一指到 <default_dir>/<标题>/。
    #[serde(default)]
    pub dir: Option<String>,
    /// 原文页面链接（https/https）：记录进 Task.url 供 UI 展示/跳转；
    /// 缺省落盘后记 content://<路径>。内容任务不参与续传/去重，url 仅展示。
    #[serde(default)]
    pub source_url: Option<String>,
}
