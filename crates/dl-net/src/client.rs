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

pub const DIRECT_PROXY: &str = "direct";

pub fn is_direct_proxy(proxy: Option<&str>) -> bool {
    proxy
        .map(str::trim)
        .is_some_and(|p| p.eq_ignore_ascii_case(DIRECT_PROXY))
}

pub fn apply_proxy(
    builder: reqwest::ClientBuilder,
    proxy: Option<&str>,
) -> Res<reqwest::ClientBuilder> {
    let Some(p) = proxy.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(builder);
    };
    if p.eq_ignore_ascii_case(DIRECT_PROXY) {
        return Ok(builder.no_proxy());
    }
    // 手动代理；支持 http://、https://、socks5://、socks5h://（远端 DNS，推荐默认）
    let proxy = reqwest::Proxy::all(p).map_err(|e| {
        DownloadError::new(ErrorKind::ProxyConnectFailed, format!("bad proxy url: {e}"))
    })?;
    Ok(builder.proxy(proxy))
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

#[allow(dead_code)]
fn parse_scutil_proxy_output(out: &str) -> Option<String> {
    let mut map = std::collections::HashMap::<&str, &str>::new();
    for line in out.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        map.insert(k.trim(), v.trim());
    }
    if map.get("HTTPSEnable").copied() == Some("1") {
        if let (Some(h), Some(p)) = (map.get("HTTPSProxy"), map.get("HTTPSPort")) {
            if !h.is_empty() && !p.is_empty() {
                return Some(format!("http://{h}:{p}"));
            }
        }
    }
    if map.get("HTTPEnable").copied() == Some("1") {
        if let (Some(h), Some(p)) = (map.get("HTTPProxy"), map.get("HTTPPort")) {
            if !h.is_empty() && !p.is_empty() {
                return Some(format!("http://{h}:{p}"));
            }
        }
    }
    if map.get("SOCKSEnable").copied() == Some("1") {
        if let (Some(h), Some(p)) = (map.get("SOCKSProxy"), map.get("SOCKSPort")) {
            if !h.is_empty() && !p.is_empty() {
                return Some(format!("socks5h://{h}:{p}"));
            }
        }
    }
    None
}

#[allow(dead_code)]
fn normalize_win_proxy_server(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if !trimmed.contains('=') {
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("http://")
            || lower.starts_with("https://")
            || lower.starts_with("socks5://")
            || lower.starts_with("socks5h://")
        {
            return Some(trimmed.to_string());
        }
        return Some(format!("http://{trimmed}"));
    }
    let mut http = None;
    let mut https = None;
    let mut socks = None;
    for part in trimmed.split(';') {
        let Some((scheme, addr)) = part.split_once('=') else {
            continue;
        };
        let addr = addr.trim();
        if addr.is_empty() {
            continue;
        }
        match scheme.trim().to_ascii_lowercase().as_str() {
            "https" => https = Some(format!("http://{addr}")),
            "http" => http = Some(format!("http://{addr}")),
            "socks" => socks = Some(format!("socks5h://{addr}")),
            _ => {}
        }
    }
    https.or(http).or(socks)
}

/// 探测当前系统激活的代理地址（环境变量 → macOS `scutil --proxy` / Windows 注册表）。
pub fn detect_system_proxy() -> Option<String> {
    for key in [
        "ALL_PROXY",
        "all_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ] {
        if let Ok(val) = std::env::var(key) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = std::process::Command::new("scutil").arg("--proxy").output() {
            if out.status.success() {
                let text = String::from_utf8_lossy(&out.stdout);
                if let Some(p) = parse_scutil_proxy_output(&text) {
                    return Some(p);
                }
            }
        }
    }
    #[cfg(windows)]
    {
        use winreg::enums::HKEY_CURRENT_USER;
        use winreg::RegKey;
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        if let Ok(key) =
            hkcu.open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings")
        {
            let enabled: u32 = key.get_value("ProxyEnable").unwrap_or(0);
            if enabled == 1 {
                if let Ok(server) = key.get_value::<String, _>("ProxyServer") {
                    if let Some(p) = normalize_win_proxy_server(&server) {
                        return Some(p);
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_modes_build_clients() {
        assert!(is_direct_proxy(Some("direct")));
        assert!(is_direct_proxy(Some(" DIRECT ")));
        assert!(!is_direct_proxy(Some("http://127.0.0.1:7890")));
        assert!(!is_direct_proxy(None));

        assert!(build_client(None).is_ok());
        assert!(build_client(Some("direct")).is_ok());
        assert!(build_client(Some("http://127.0.0.1:7890")).is_ok());
        assert!(build_client(Some("socks5h://127.0.0.1:1080")).is_ok());
    }

    #[test]
    fn system_proxy_parsers_handle_common_formats() {
        let scutil = r#"
<dictionary> {
  HTTPEnable : 1
  HTTPPort : 7890
  HTTPProxy : 127.0.0.1
  HTTPSEnable : 1
  HTTPSPort : 7890
  HTTPSProxy : 127.0.0.1
  SOCKSEnable : 0
}
"#;
        assert_eq!(
            parse_scutil_proxy_output(scutil),
            Some("http://127.0.0.1:7890".to_string())
        );

        let scutil_socks = "SOCKSEnable : 1\nSOCKSProxy : 127.0.0.1\nSOCKSPort : 1080\n";
        assert_eq!(
            parse_scutil_proxy_output(scutil_socks),
            Some("socks5h://127.0.0.1:1080".to_string())
        );
        assert_eq!(parse_scutil_proxy_output("HTTPEnable : 0"), None);

        assert_eq!(
            normalize_win_proxy_server("127.0.0.1:7890"),
            Some("http://127.0.0.1:7890".to_string())
        );
        assert_eq!(
            normalize_win_proxy_server("http=127.0.0.1:8080;https=127.0.0.1:8443"),
            Some("http://127.0.0.1:8443".to_string())
        );
        assert_eq!(
            normalize_win_proxy_server("socks=127.0.0.1:1080"),
            Some("socks5h://127.0.0.1:1080".to_string())
        );
    }
}
