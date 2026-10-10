use serde::{Deserialize, Serialize};

/// 错误分类（DESIGN §5.6）。每类对应稳定错误码 + i18n key + 处置动作：
/// 403 鉴权失败、签名过期、磁盘满对用户是完全不同的处置，不能都显示「下载失败」。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    NetworkDns,
    NetworkConnectTimeout,
    NetworkTls,
    NetworkConnectionReset,
    /// HTTP 状态码错误；429/503 依 Retry-After 退避。
    Http {
        status: u16,
        retry_after_secs: Option<u64>,
    },
    /// 401/403 且带凭据：引导「从浏览器刷新 Cookie 后重试」。
    AuthExpired,
    /// 签名 URL 过期：引导「重新嗅探」。
    UrlExpired,
    InvalidUrl,
    RangeUnsupported,
    LengthUnknown,
    DiskFull,
    FileAccessDenied,
    FileNameInvalid,
    /// 目标文件已存在且冲突策略为 Ask：交由用户决策。
    NameConflict {
        path: String,
    },
    PathTooLong,
    /// 响应带非 identity 的 Content-Encoding：拒绝拼装（DESIGN §5.3）。
    UnsupportedEncoding,
    /// 直播型 HLS（无 ENDLIST），首版明确不支持。
    LiveUnsupported,
    DrmUnsupported,
    DashUnsupported,
    ProxyAuthRequired,
    ProxyConnectFailed,
    InvalidTransition {
        from: String,
        to: String,
    },
    NotFound,
    /// 入站参数不合法（头过大/格式错误等）。
    InvalidParams,
    /// 同 URL 已存在未完结任务。
    TaskExists {
        id: String,
        status: String,
    },
    /// 完成后哈希校验不符。
    ChecksumMismatch,
    /// 下载后处理失败（如视频号解密：算法可能已更新）。
    DecryptFailed,
    Internal,
}

impl ErrorKind {
    /// 稳定错误码，用于 IPC、日志与错误码→文案映射。
    pub fn code(&self) -> &'static str {
        match self {
            ErrorKind::NetworkDns => "network_dns",
            ErrorKind::NetworkConnectTimeout => "network_connect_timeout",
            ErrorKind::NetworkTls => "network_tls",
            ErrorKind::NetworkConnectionReset => "network_connection_reset",
            ErrorKind::Http { .. } => "http",
            ErrorKind::AuthExpired => "auth_expired",
            ErrorKind::UrlExpired => "url_expired",
            ErrorKind::InvalidUrl => "invalid_url",
            ErrorKind::RangeUnsupported => "range_unsupported",
            ErrorKind::LengthUnknown => "length_unknown",
            ErrorKind::DiskFull => "disk_full",
            ErrorKind::FileAccessDenied => "file_access_denied",
            ErrorKind::FileNameInvalid => "file_name_invalid",
            ErrorKind::NameConflict { .. } => "name_conflict",
            ErrorKind::PathTooLong => "path_too_long",
            ErrorKind::UnsupportedEncoding => "unsupported_encoding",
            ErrorKind::LiveUnsupported => "live_unsupported",
            ErrorKind::DrmUnsupported => "drm_unsupported",
            ErrorKind::DashUnsupported => "dash_unsupported",
            ErrorKind::ProxyAuthRequired => "proxy_auth_required",
            ErrorKind::ProxyConnectFailed => "proxy_connect_failed",
            ErrorKind::InvalidTransition { .. } => "invalid_transition",
            ErrorKind::NotFound => "not_found",
            ErrorKind::InvalidParams => "invalid_params",
            ErrorKind::TaskExists { .. } => "task_exists",
            ErrorKind::ChecksumMismatch => "checksum_mismatch",
            ErrorKind::DecryptFailed => "decrypt_failed",
            ErrorKind::Internal => "internal",
        }
    }

    pub fn i18n_key(&self) -> String {
        format!("error.{}", self.code())
    }

    /// 网络类/限流类可指数退避重试；4xx 类不盲目重试，升级为重新探测。
    pub fn is_retryable(&self) -> bool {
        match self {
            ErrorKind::NetworkDns
            | ErrorKind::NetworkConnectTimeout
            | ErrorKind::NetworkTls
            | ErrorKind::NetworkConnectionReset => true,
            ErrorKind::Http { status, .. } => *status == 429 || (500..=599).contains(status),
            _ => false,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{kind:?}: {message}")]
pub struct DownloadError {
    pub kind: ErrorKind,
    pub message: String,
}

impl DownloadError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        DownloadError {
            kind,
            message: message.into(),
        }
    }
}

pub type Res<T> = std::result::Result<T, DownloadError>;
