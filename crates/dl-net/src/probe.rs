//! URL 探测（DESIGN §5.3 决策表 + 下载中转页自动穿透）。
//! `GET + Range: bytes=0-0`；常规下载响应体绝不整读进内存，遇到 HTML 中转页时
//! 最多读 256KB 解析直链或拦截报错。

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::client::{classify_status, identity_headers, map_reqwest_err, retry_after_secs};
use crate::interstitial;
use dl_error::{DownloadError, ErrorKind, Res};

#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub final_url: String,
    pub total_bytes: Option<u64>,
    /// true = 可分段多线程 + 可续传。
    pub segmented: bool,
    /// If-Range 校验器：优先强 ETag，其次 Last-Modified。
    pub validator: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub mime: Option<String>,
    pub filename: Option<String>,
    /// true = 从网页中转页自动穿透解析得到真实直链。
    pub resolved_from_interstitial: bool,
    /// 穿透中转页时附带的 Cookie / Referer（后续分段请求合并携带）。
    pub extra_headers: HashMap<String, String>,
    /// 探测请求首字节耗时（毫秒），供后续慢连接抢占（preempt）计时使用。
    pub ttfb_ms: u64,
    /// 服务端响应头声明的文件校验和（Repr-Digest / Digest / Content-MD5 / x-goog-hash）。
    pub server_checksum: Option<(String, String)>,
}

const MAX_INTERSTITIAL_HOPS: usize = 3;
const MAX_HTML_PEEK_BYTES: usize = 256 * 1024;

pub async fn probe(
    client: &reqwest::Client,
    url: &str,
    headers: &HashMap<String, String>,
) -> Res<ProbeResult> {
    let mut cur_url = url.to_string();
    let mut resolved_from_interstitial = false;
    let mut extra_headers: HashMap<String, String> = HashMap::new();
    let mut visited = HashSet::new();

    if let Some(unwrapped) = interstitial::unwrap_wrapped_link(&cur_url) {
        visited.insert(cur_url.clone());
        cur_url = unwrapped;
        resolved_from_interstitial = true;
    }
    let original_url = cur_url.clone();

    if let Some(norm) = interstitial::normalize_download_url(&cur_url) {
        visited.insert(cur_url.clone());
        cur_url = norm;
        resolved_from_interstitial = true;
    }

    for _hop in 0..=MAX_INTERSTITIAL_HOPS {
        visited.insert(cur_url.clone());
        if !headers.contains_key("user-agent") && crate::client::prefers_own_ua(&cur_url) {
            extra_headers
                .entry("user-agent".to_string())
                .or_insert_with(|| crate::client::OWN_USER_AGENT.to_string());
        }
        let mut merged_headers = headers.clone();
        for (k, v) in &extra_headers {
            merged_headers.entry(k.clone()).or_insert_with(|| v.clone());
        }

        let probe_t0 = Instant::now();
        let mut resp =
            send_probe_request(client, &cur_url, &merged_headers, Some("bytes=0-0")).await?;
        let mut status = resp.status().as_u16();
        // 部分服务器拒绝 Range: bytes=0-0（416/405/501），回退无 Range 请求
        if matches!(status, 405 | 416 | 501) {
            drop(resp);
            resp = send_probe_request(client, &cur_url, &merged_headers, None).await?;
            status = resp.status().as_u16();
        }
        // 反爬虫 403 智能回退：当浏览器 UA 被风控拦截时，尝试显式下载器 UA（参照 MDM agent.rs）
        if status == 403
            && !resp.headers().contains_key("cf-mitigated")
            && !merged_headers.contains_key("user-agent")
        {
            drop(resp);
            let mut own_headers = merged_headers.clone();
            own_headers.insert(
                "user-agent".to_string(),
                crate::client::OWN_USER_AGENT.to_string(),
            );
            let mut retry_resp =
                send_probe_request(client, &cur_url, &own_headers, Some("bytes=0-0")).await?;
            let mut retry_status = retry_resp.status().as_u16();
            if matches!(retry_status, 405 | 416 | 501) {
                drop(retry_resp);
                retry_resp = send_probe_request(client, &cur_url, &own_headers, None).await?;
                retry_status = retry_resp.status().as_u16();
            }
            if matches!(retry_status, 200 | 206) {
                crate::client::remember_own_ua(&cur_url);
                extra_headers.insert(
                    "user-agent".to_string(),
                    crate::client::OWN_USER_AGENT.to_string(),
                );
                merged_headers = own_headers;
            }
            resp = retry_resp;
            status = retry_status;
        }
        let ttfb_ms = probe_t0.elapsed().as_millis().max(1) as u64;

        let resp_headers = resp.headers().clone();
        let final_url_real = resp.url().to_string();
        if resp_headers.contains_key("cf-mitigated") {
            return Err(DownloadError::new(
                ErrorKind::InterstitialPage {
                    url: final_url_real,
                    title: Some("Cloudflare Challenge".into()),
                },
                "目标链接触发 Cloudflare 人机验证",
            ));
        }
        if let Some(kind) = classify_status(status) {
            return Err(DownloadError::new(kind, format!("probe got HTTP {status}")));
        }

        let server_checksum = extract_server_checksum(&resp_headers, status);

        let segmented_by_status = match status {
            206 => true,
            200 => false,
            416 => {
                return Err(DownloadError::new(
                    ErrorKind::RangeUnsupported,
                    "probe got 416 (range not satisfiable)",
                ))
            }
            s => {
                return Err(DownloadError::new(
                    ErrorKind::Http {
                        status: s,
                        retry_after_secs: None,
                    },
                    format!("unexpected probe status {s}"),
                ))
            }
        };

        let mime = resp_headers
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let filename = resp_headers
            .get(reqwest::header::CONTENT_DISPOSITION)
            .and_then(|v| v.to_str().ok())
            .and_then(dl_filename::disposition_filename);
        let has_attachment = resp_headers
            .get(reqwest::header::CONTENT_DISPOSITION)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.to_ascii_lowercase().contains("attachment"));

        let total_from_len_peek = resp_headers
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());

        let is_html_mime = interstitial::is_html_mime(mime.as_deref());
        let might_be_untyped_html = status == 200
            && mime.is_none()
            && total_from_len_peek.is_some_and(|n| n <= MAX_HTML_PEEK_BYTES as u64)
            && !url_has_explicit_text_ext(&original_url);

        // 中转页识别与穿透：收到 HTML（或无类型小响应体嗅探到 HTML 魔数）且非显式 attachment 且原链接并非请求 .html
        if (is_html_mime || might_be_untyped_html)
            && !has_attachment
            && !interstitial::url_expects_html(&original_url)
        {
            // 若是 206（只给了 1 字节），重新发无 Range GET 才能读完整中转页 HTML
            let html_resp = if status == 206 {
                drop(resp);
                let mut req = client.get(&final_url_real);
                for (k, v) in &merged_headers {
                    if dl_model::HEADER_ALLOWLIST.contains(&k.as_str()) {
                        req = req.header(k, v);
                    }
                }
                req.send()
                    .await
                    .map_err(|e| DownloadError::new(map_reqwest_err(&e), e.to_string()))?
            } else {
                resp
            };
            let html_enc = html_resp
                .headers()
                .get(reqwest::header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            let set_cookies = collect_cookies(html_resp.headers());
            let raw = read_capped_body(html_resp, MAX_HTML_PEEK_BYTES).await;

            if is_html_mime || interstitial::looks_like_html_body(&raw) {
                let html = interstitial::decode_body(&raw, html_enc.as_deref());
                let title = interstitial::extract_title(&html);

                if let Some(next_url) = interstitial::extract_direct_url(&final_url_real, &html) {
                    if !visited.contains(&next_url) {
                        extra_headers.insert("referer".to_string(), final_url_real.clone());
                        if let Some(ck) = set_cookies {
                            extra_headers.insert("cookie".to_string(), ck);
                        }
                        cur_url = next_url;
                        resolved_from_interstitial = true;
                        continue;
                    }
                }
                return Err(DownloadError::new(
                    ErrorKind::InterstitialPage {
                        url: final_url_real,
                        title,
                    },
                    "目标链接返回了 HTML 下载中转页或登录墙，未能解析出直链",
                ));
            }
            // 未触发 HTML 魔数的无类型小文件：基于已获取的响应头直接返回探测结果
            let etag = resp_headers
                .get(reqwest::header::ETAG)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
                .filter(|s| !s.starts_with("W/"));
            let last_modified = resp_headers
                .get(reqwest::header::LAST_MODIFIED)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            let validator = etag.clone().or_else(|| last_modified.clone());
            return Ok(ProbeResult {
                final_url: final_url_real,
                total_bytes: total_from_len_peek,
                segmented: false,
                validator,
                etag,
                last_modified,
                mime,
                filename,
                resolved_from_interstitial,
                extra_headers,
                ttfb_ms,
                server_checksum,
            });
        }

        let ce = resp_headers
            .get(reqwest::header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok());
        let non_identity = ce.is_some_and(|v| !v.eq_ignore_ascii_case("identity"));
        if segmented_by_status && non_identity {
            return Err(DownloadError::new(
                ErrorKind::UnsupportedEncoding,
                format!(
                    "content-encoding = {}, only identity is supported",
                    ce.unwrap_or("")
                ),
            ));
        }

        // 206 + Content-Range: bytes 0-0/<total>；`*` = 总长未知 → 单连接
        let total_from_range = resp_headers
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_total_from_content_range);
        let total_from_len = resp_headers
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());

        let accept_ranges_header = resp_headers
            .get(reqwest::header::ACCEPT_RANGES)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.eq_ignore_ascii_case("bytes"))
            .unwrap_or(false);
        let (total_bytes, segmented) = if segmented_by_status {
            match total_from_range {
                Some(t) => (Some(t), true),
                None => (None, false),
            }
        } else {
            (
                total_from_len,
                !non_identity && accept_ranges_header && total_from_len.is_some(),
            )
        };

        let etag = resp_headers
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .filter(|s| !s.starts_with("W/"));
        let last_modified = resp_headers
            .get(reqwest::header::LAST_MODIFIED)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let validator = etag.clone().or_else(|| last_modified.clone());

        // 主动断连：探测响应体（那 1 字节）绝不使用
        drop(resp);
        tracing::info!(
            status,
            accept_ranges = resp_headers
                .get(reqwest::header::ACCEPT_RANGES)
                .and_then(|v| v.to_str().ok())
                .unwrap_or(""),
            content_length = ?total_from_len,
            segmented,
            resolved_from_interstitial,
            "probe decision"
        );

        return Ok(ProbeResult {
            final_url: final_url_real,
            total_bytes,
            segmented,
            validator,
            etag,
            last_modified,
            mime,
            filename,
            resolved_from_interstitial,
            extra_headers,
            ttfb_ms,
            server_checksum,
        });
    }

    Err(DownloadError::new(
        ErrorKind::InterstitialPage {
            url: cur_url,
            title: None,
        },
        "中转页跳转次数过多，未能到达真实直链",
    ))
}

fn url_has_explicit_text_ext(raw: &str) -> bool {
    let Ok(u) = url::Url::parse(raw) else {
        return false;
    };
    let Some(last) = u.path_segments().and_then(|mut s| s.next_back()) else {
        return false;
    };
    let lower = last.to_ascii_lowercase();
    lower.ends_with(".txt")
        || lower.ends_with(".csv")
        || lower.ends_with(".json")
        || lower.ends_with(".xml")
}

async fn send_probe_request(
    client: &reqwest::Client,
    url: &str,
    headers: &HashMap<String, String>,
    range: Option<&str>,
) -> Res<reqwest::Response> {
    const RETRY_DELAYS_MS: [u64; 2] = [400, 1200];
    for attempt in 0..=RETRY_DELAYS_MS.len() {
        let mut req = identity_headers(client.get(url));
        if let Some(r) = range {
            req = req.header("Range", r);
        }
        for (k, v) in headers {
            if dl_model::HEADER_ALLOWLIST.contains(&k.as_str()) {
                req = req.header(k, v);
            }
        }
        match req.send().await {
            Ok(resp) => {
                let s = resp.status().as_u16();
                if let Some(&default_ms) = RETRY_DELAYS_MS
                    .get(attempt)
                    .filter(|_| matches!(s, 429 | 502 | 503 | 504))
                {
                    let wait_ms =
                        retry_after_secs(resp.headers().get(reqwest::header::RETRY_AFTER))
                            .map(|sec| sec.clamp(1, 3) * 1000)
                            .unwrap_or(default_ms);
                    drop(resp);
                    tracing::debug!(
                        url,
                        status = s,
                        wait_ms,
                        "probe transient HTTP error, retrying"
                    );
                    tokio::time::sleep(Duration::from_millis(wait_ms)).await;
                    continue;
                }
                return Ok(resp);
            }
            Err(e) => {
                let kind = map_reqwest_err(&e);
                if let Some(&wait_ms) = RETRY_DELAYS_MS.get(attempt).filter(|_| {
                    matches!(
                        kind,
                        ErrorKind::NetworkConnectionReset | ErrorKind::NetworkConnectTimeout
                    )
                }) {
                    tracing::debug!(url, error = %e, wait_ms, "probe transient transport error, retrying");
                    tokio::time::sleep(Duration::from_millis(wait_ms)).await;
                    continue;
                }
                return Err(DownloadError::new(kind, e.to_string()));
            }
        }
    }
    unreachable!()
}

async fn read_capped_body(mut resp: reqwest::Response, cap: usize) -> Vec<u8> {
    let mut out = Vec::new();
    while let Ok(Some(chunk)) = resp.chunk().await {
        let take = chunk.len().min(cap.saturating_sub(out.len()));
        out.extend_from_slice(&chunk[..take]);
        if out.len() >= cap {
            break;
        }
    }
    out
}

fn collect_cookies(headers: &reqwest::header::HeaderMap) -> Option<String> {
    let parts: Vec<String> = headers
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|s| s.split(';').next())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("; "))
    }
}

/// "bytes 0-0/1234" → Some(1234)；"bytes 0-0/*" → None。
fn parse_total_from_content_range(v: &str) -> Option<u64> {
    let total = v.rsplit('/').next()?;
    if total.trim() == "*" {
        return None;
    }
    total.trim().parse::<u64>().ok()
}

/// 解析服务端声明的完整性摘要（Repr-Digest / Digest / Content-MD5 / x-goog-hash），
/// 返回最强的 `(algo, lowercase_hex)`（参照 MDM verify.rs）。
fn extract_server_checksum(
    headers: &reqwest::header::HeaderMap,
    status: u16,
) -> Option<(String, String)> {
    let to_hex = |bytes: &[u8]| -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() };
    let strength = |algo: &str| -> u8 {
        match algo {
            "sha512" => 4,
            "sha256" => 3,
            "sha1" => 2,
            "md5" => 1,
            _ => 0,
        }
    };
    let expect_len = |algo: &str| -> usize {
        match algo {
            "sha512" => 64,
            "sha256" => 32,
            "sha1" => 20,
            "md5" => 16,
            _ => 0,
        }
    };

    let mut best: Option<(u8, String, String)> = None;
    let mut consider = |algo: &str, bytes: Vec<u8>| {
        if bytes.len() != expect_len(algo) {
            return;
        }
        let s = strength(algo);
        if best.as_ref().is_none_or(|(cur, _, _)| s > *cur) {
            best = Some((s, algo.to_string(), to_hex(&bytes)));
        }
    };

    // RFC 9530 Repr-Digest（200 与 206 均描述完整资源表示）与 RFC 3230 Digest（仅 200 全量响应有效）
    let digest_headers: &[&str] = if status == 200 {
        &["repr-digest", "digest"]
    } else {
        &["repr-digest"]
    };
    for name in digest_headers {
        for v in headers
            .get_all(*name)
            .iter()
            .filter_map(|v| v.to_str().ok())
        {
            for part in v.split(',') {
                let Some((algo_raw, val_raw)) = part.trim().split_once('=') else {
                    continue;
                };
                let algo = match algo_raw.trim().to_ascii_lowercase().as_str() {
                    "sha-512" | "sha512" => "sha512",
                    "sha-256" | "sha256" => "sha256",
                    "sha" | "sha-1" | "sha1" => "sha1",
                    "md5" => "md5",
                    _ => continue,
                };
                let cleaned = val_raw.trim().trim_matches(':');
                if let Some(bytes) = interstitial::decode_base64(cleaned) {
                    consider(algo, bytes);
                }
            }
        }
    }

    if status == 200 {
        if let Some(val) = headers
            .get("content-md5")
            .and_then(|v| v.to_str().ok())
            .and_then(interstitial::decode_base64)
        {
            consider("md5", val);
        }
    }

    for v in headers
        .get_all("x-goog-hash")
        .iter()
        .filter_map(|v| v.to_str().ok())
    {
        for part in v.split(',') {
            if let Some(b64) = part.trim().strip_prefix("md5=") {
                if let Some(bytes) = interstitial::decode_base64(b64.trim()) {
                    consider("md5", bytes);
                }
            }
        }
    }

    best.map(|(_, algo, hex)| (algo, hex))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_range_total_parsing() {
        assert_eq!(parse_total_from_content_range("bytes 0-0/1234"), Some(1234));
        assert_eq!(parse_total_from_content_range("bytes 0-0/*"), None);
        assert_eq!(parse_total_from_content_range("bytes 0-0/0"), Some(0));
    }

    #[test]
    fn extracts_server_checksum_with_strength_priority() {
        use reqwest::header::{HeaderMap, HeaderValue};
        let mut h = HeaderMap::new();
        // md5 of "hello" -> XrY7u+Ae7tCTyyK7j1rNww==
        h.insert(
            "content-md5",
            HeaderValue::from_static("XrY7u+Ae7tCTyyK7j1rNww=="),
        );
        // sha256 -> LPJNul+wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ=
        h.insert(
            "repr-digest",
            HeaderValue::from_static("sha-256=:LPJNul+wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ=:"),
        );
        // On 200, both Content-MD5 and Repr-Digest present -> sha256 wins
        let res200 = extract_server_checksum(&h, 200).unwrap();
        assert_eq!(res200.0, "sha256");
        assert_eq!(
            res200.1,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );

        // Digest/Content-MD5 must be ignored on 206, while Repr-Digest and x-goog-hash still work
        let mut h206 = HeaderMap::new();
        h206.insert(
            "content-md5",
            HeaderValue::from_static("XrY7u+Ae7tCTyyK7j1rNww=="),
        );
        assert_eq!(extract_server_checksum(&h206, 206), None);
        h206.insert(
            "x-goog-hash",
            HeaderValue::from_static("crc32c=AAAAAA==, md5=XrY7u+Ae7tCTyyK7j1rNww=="),
        );
        assert_eq!(
            extract_server_checksum(&h206, 206),
            Some(("md5".into(), "5eb63bbbe01eeed093cb22bb8f5acdc3".into()))
        );
    }
}
