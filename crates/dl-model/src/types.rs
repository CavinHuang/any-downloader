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
}

impl TaskKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskKind::Http => "http",
            TaskKind::Hls => "hls",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "http" => Some(TaskKind::Http),
            "hls" => Some(TaskKind::Hls),
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
        )
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
        }
    }
}
