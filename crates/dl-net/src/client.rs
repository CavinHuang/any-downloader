//! HTTP 客户端构建（DESIGN §5.3/§5.8）。
//!
//! 关键约束：所有请求强制 `Accept-Encoding: identity`——Range 语义与传输层压缩不能共存
//! （Content-Range 描述压缩后字节，各段是无法独立解压的 gzip 流，透明解压会静默错位）。
//! reqwest 不启用 gzip/brotli/zstd feature，天然不自动解压。

use std::time::Duration;

use dl_error::{DownloadError, ErrorKind, Res};

pub const USER_AGENT: &str = concat!("AnythingDownloader/", env!("CARGO_PKG_VERSION"));

pub fn build_client(proxy: Option<&str>) -> Res<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(10));
    if let Some(p) = proxy {
        // 手动代理；支持 http://、https://、socks5://、socks5h://（远端 DNS，推荐默认）
        let proxy = reqwest::Proxy::all(p).map_err(|e| {
            DownloadError::new(ErrorKind::ProxyConnectFailed, format!("bad proxy url: {e}"))
        })?;
        builder = builder.proxy(proxy);
    }
    // 未配置时 reqwest 默认跟随环境变量与系统代理设置。
    builder
        .build()
        .map_err(|e| DownloadError::new(ErrorKind::ProxyConnectFailed, e.to_string()))
}

/// 每个请求都要带的头：关闭传输层压缩。
pub fn identity_headers(req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    req.header("Accept-Encoding", "identity")
}

/// 将 reqwest 错误映射到错误分类。
pub fn map_reqwest_err(e: &reqwest::Error) -> ErrorKind {
    use std::error::Error as _;
    if e.is_timeout() {
        ErrorKind::NetworkConnectTimeout
    } else if e.is_connect() {
        // 区分 DNS 解析失败与连接被拒/中断（错误链末端是 std::io::Error）
        let io_kind = e
            .source()
            .and_then(|s| s.downcast_ref::<std::io::Error>())
            .map(|io| io.kind());
        match io_kind {
            Some(
                std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::UnexpectedEof,
            ) => ErrorKind::NetworkConnectionReset,
            _ => ErrorKind::NetworkDns,
        }
    } else {
        // body/decode 失败与其它传输层错误同义：连接中断
        ErrorKind::NetworkConnectionReset
    }
}

/// HTTP 状态码 → 错误分类（除 206/200/416 外）。
pub fn classify_status(status: u16) -> Option<ErrorKind> {
    match status {
        401 => Some(ErrorKind::AuthExpired),
        403 | 404 | 410 => Some(ErrorKind::UrlExpired),
        407 => Some(ErrorKind::ProxyAuthRequired),
        429 | 500..=599 => Some(ErrorKind::Http {
            status,
            retry_after_secs: None,
        }),
        400..=499 => Some(ErrorKind::Http {
            status,
            retry_after_secs: None,
        }),
        _ => None,
    }
}

/// 解析 Retry-After（秒）；HTTP-date 形式不支持时返回 None。
pub fn retry_after_secs(header: Option<&reqwest::header::HeaderValue>) -> Option<u64> {
    let v = header?.to_str().ok()?;
    v.trim().parse::<u64>().ok()
}
