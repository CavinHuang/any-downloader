//! URL 探测（DESIGN §5.3 决策表）。
//! `GET + Range: bytes=0-0`；响应体最多读 1 个 chunk 即断连，绝不整读进内存。

use crate::client::{classify_status, identity_headers, map_reqwest_err};
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
}

pub async fn probe(
    client: &reqwest::Client,
    url: &str,
    headers: &std::collections::HashMap<String, String>,
) -> Res<ProbeResult> {
    let mut req = identity_headers(client.get(url)).header("Range", "bytes=0-0");
    for (k, v) in headers {
        if dl_model::HEADER_ALLOWLIST.contains(&k.as_str()) {
            req = req.header(k, v);
        }
    }
    let resp = req
        .send()
        .await
        .map_err(|e| DownloadError::new(map_reqwest_err(&e), e.to_string()))?;

    let status = resp.status().as_u16();
    let headers = resp.headers().clone();

    if let Some(kind) = classify_status(status) {
        return Err(DownloadError::new(kind, format!("probe got HTTP {status}")));
    }
    let segmented_by_status = match status {
        206 => true,
        200 => false, // 服务器忽略 Range：单连接，不可续传
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

    // 传输层压缩与 Range 不共存（DESIGN §5.3）
    if let Some(ce) = headers
        .get(reqwest::header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
    {
        if !ce.eq_ignore_ascii_case("identity") {
            return Err(DownloadError::new(
                ErrorKind::UnsupportedEncoding,
                format!("content-encoding = {ce}, only identity is supported"),
            ));
        }
    }

    // 206 + Content-Range: bytes 0-0/<total>；`*` = 总长未知 → 单连接
    let total_from_range = headers
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_total_from_content_range);
    let total_from_len = headers
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());

    let accept_ranges_header = headers
        .get(reqwest::header::ACCEPT_RANGES)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("bytes"))
        .unwrap_or(false);
    let (total_bytes, segmented) = if segmented_by_status {
        match total_from_range {
            Some(t) => (Some(t), true),
            None => (None, false), // bytes 0-0/* → Range 可用但总长未知
        }
    } else {
        // 200：服务器忽略了探测请求的 Range。社区标准（aria2）以 `Accept-Ranges:
        // bytes` 响应头为分段信号——部分 CDN/网关只对特定请求忽略 Range，
        // 声明支持时仍可多线程；段请求若再遇 200 会自动降级单连接兜底。
        (
            total_from_len,
            accept_ranges_header && total_from_len.is_some(),
        )
    };

    let etag = headers
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .filter(|s| !s.starts_with("W/")); // 弱 ETag 不能用于 If-Range
    let last_modified = headers
        .get(reqwest::header::LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let validator = etag.clone().or_else(|| last_modified.clone());

    let mime = headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let filename = headers
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .and_then(dl_filename::disposition_filename);

    let final_url_real = resp.url().to_string();
    // 主动断连：探测响应体（那 1 字节）绝不使用
    drop(resp);
    tracing::info!(
        status,
        accept_ranges = headers
            .get(reqwest::header::ACCEPT_RANGES)
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        content_length = ?total_from_len,
        segmented,
        "probe decision"
    );

    Ok(ProbeResult {
        // 重定向后的真实地址（分段请求直接用它，省去每次重定向开销）
        final_url: final_url_real,
        total_bytes,
        segmented,
        validator,
        etag,
        last_modified,
        mime,
        filename,
    })
}

/// "bytes 0-0/1234" → Some(1234)；"bytes 0-0/*" → None。
fn parse_total_from_content_range(v: &str) -> Option<u64> {
    let total = v.rsplit('/').next()?;
    if total.trim() == "*" {
        return None;
    }
    total.trim().parse::<u64>().ok()
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
}
