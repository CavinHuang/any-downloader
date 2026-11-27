//! HTTP 客户端构建（DESIGN §5.3/§5.8）。
//!
//! 关键约束：所有请求强制 `Accept-Encoding: identity`——Range 语义与传输层压缩不能共存
//! （Content-Range 描述压缩后字节，各段是无法独立解压的 gzip 流，透明解压会静默错位）。
//! reqwest 不启用 gzip/brotli/zstd feature，天然不自动解压。

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

use dl_error::{DownloadError, ErrorKind, Res};

pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
/// 反爬虫站回退使用的真实程序标识（当站点拦截伪装浏览器 UA 但允许显式下载器标识时，参照 MDM agent.rs）。
pub const OWN_USER_AGENT: &str = concat!("AnythingDownloader/", env!("CARGO_PKG_VERSION"));

const OWN_UA_HOSTS_MAX: usize = 512;
static OWN_UA_HOSTS: Mutex<Option<HashSet<String>>> = Mutex::new(None);

fn host_key(url: &str) -> Option<String> {
    let u = url::Url::parse(url).ok()?;
    let h = u.host_str()?.to_ascii_lowercase();
    Some(match u.port_or_known_default() {
        Some(p) => format!("{h}:{p}"),
        None => h,
    })
}

pub fn prefers_own_ua(url: &str) -> bool {
    let Some(k) = host_key(url) else {
        return false;
    };
    OWN_UA_HOSTS
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|set| set.contains(&k))
}

pub fn remember_own_ua(url: &str) {
    let Some(k) = host_key(url) else {
        return;
    };
    let mut guard = OWN_UA_HOSTS.lock().unwrap();
    let set = guard.get_or_insert_with(HashSet::new);
    if set.len() >= OWN_UA_HOSTS_MAX {
        set.clear();
    }
    set.insert(k);
}

fn apply_proxy(
    builder: reqwest::ClientBuilder,
    proxy: Option<&str>,
) -> Res<reqwest::ClientBuilder> {
    if let Some(p) = proxy {
        // 手动代理；支持 http://、https://、socks5://、socks5h://（远端 DNS，推荐默认）
        let proxy = reqwest::Proxy::all(p).map_err(|e| {
            DownloadError::new(ErrorKind::ProxyConnectFailed, format!("bad proxy url: {e}"))
        })?;
        Ok(builder.proxy(proxy))
    } else {
        Ok(builder)
    }
}

pub fn build_client(proxy: Option<&str>) -> Res<reqwest::Client> {
    let builder = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .tcp_nodelay(true)
        .tcp_keepalive(Some(Duration::from_secs(30)))
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(10));
    // 未配置时 reqwest 默认跟随环境变量与系统代理设置。
    apply_proxy(builder, proxy)?
        .build()
        .map_err(|e| DownloadError::new(ErrorKind::ProxyConnectFailed, e.to_string()))
}

/// 分段 worker 专用客户端：强制 HTTP/1.1 + 独立连接池。
///
/// 每个 worker 持有自己的客户端实例，避免 HTTP/2 ALPN 把 8–32 个分段请求
/// 多路复用到同一条 TCP 连接上共享拥塞窗口（参照 MDM / Ghost-Downloader-3）。
pub fn build_worker_client(proxy: Option<&str>) -> Res<reqwest::Client> {
    build_pooled_worker_client(proxy, 2)
}

/// 可配置连接池容量的 HTTP/1.1 并发客户端（参照 MDM）：
/// 同一下载任务内的多个 worker 共享该客户端与连接池，支持跨分段的 keep-alive 复用与 TLS 会话恢复。
pub fn build_pooled_worker_client(proxy: Option<&str>, pool_size: usize) -> Res<reqwest::Client> {
    let builder = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .http1_only()
        .tcp_nodelay(true)
        .tcp_keepalive(Some(Duration::from_secs(30)))
        .pool_max_idle_per_host(pool_size.clamp(2, 64))
        .pool_idle_timeout(Some(Duration::from_secs(60)))
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(10));
    apply_proxy(builder, proxy)?
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
