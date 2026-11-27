//! 跨平台内置网页媒体与资源嗅探（无需安装证书或系统代理）。
//! 给定任意网页 URL，抓取页面 HTML 并提取 <video>/<audio>/<source>、OpenGraph 媒体标签、
//! 可下载链接与内联脚本中的直链（.mp4/.m3u8/.mpd/.mp3/.zip/.dmg 等）。

use std::collections::HashSet;

use serde::Serialize;

use crate::client::map_reqwest_err;
use crate::interstitial;
use dl_error::{DownloadError, ErrorKind, Res};

const MAX_PAGE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct PageSniffItem {
    pub url: String,
    pub filename: String,
    pub kind: String,
    pub mime: Option<String>,
    pub bytes: Option<u64>,
    pub source: String,
}

pub async fn sniff_page(client: &reqwest::Client, page_url: &str) -> Res<Vec<PageSniffItem>> {
    let parsed = url::Url::parse(page_url)
        .map_err(|e| DownloadError::new(ErrorKind::InvalidUrl, format!("invalid url: {e}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(DownloadError::new(
            ErrorKind::InvalidUrl,
            "only http/https URLs are supported",
        ));
    }

    let mut resp = client
        .get(page_url)
        .header("Accept", "text/html,application/xhtml+xml,*/*;q=0.9")
        .header("Accept-Encoding", "gzip, deflate, br, identity")
        .send()
        .await
        .map_err(|e| DownloadError::new(map_reqwest_err(&e), e.to_string()))?;

    let status = resp.status().as_u16();
    if let Some(kind) = crate::classify_status(status) {
        return Err(DownloadError::new(kind, format!("HTTP {status}")));
    }
    let final_url = resp.url().to_string();
    let mime = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let ce = resp
        .headers()
        .get(reqwest::header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let cd_name = resp
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .and_then(dl_filename::disposition_filename);
    let cl = resp.content_length();

    // 若本身就是直链媒体/文件，直接返回单条
    if !interstitial::is_html_mime(mime.as_deref()) {
        if let Some(kind) = classify_kind(&final_url, mime.as_deref()) {
            let filename = cd_name.unwrap_or_else(|| filename_of(&final_url, mime.as_deref()));
            return Ok(vec![PageSniffItem {
                url: final_url,
                filename,
                kind: kind.to_string(),
                mime,
                bytes: cl,
                source: "direct".to_string(),
            }]);
        }
    }

    let mut raw = Vec::new();
    while let Ok(Some(chunk)) = resp.chunk().await {
        let take = chunk.len().min(MAX_PAGE_BYTES.saturating_sub(raw.len()));
        raw.extend_from_slice(&chunk[..take]);
        if raw.len() >= MAX_PAGE_BYTES {
            break;
        }
    }
    let html = interstitial::decode_body(&raw, ce.as_deref());
    Ok(extract_page_items(&final_url, &html))
}

pub fn extract_page_items(base_url: &str, html: &str) -> Vec<PageSniffItem> {
    let Ok(raw_base) = url::Url::parse(base_url) else {
        return Vec::new();
    };
    let base = iter_tags(html, "base")
        .into_iter()
        .find_map(|tag| extract_attr(tag, "href"))
        .and_then(|href| {
            raw_base
                .join(interstitial::decode_html_entities(&href).trim())
                .ok()
        })
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .unwrap_or(raw_base);
    let mut seen = HashSet::new();
    let mut items = Vec::new();

    let mut push = |raw_url: &str, source: &str, forced_kind: Option<&str>| {
        let decoded = interstitial::decode_html_entities(raw_url.trim());
        let clean = decoded
            .trim()
            .replace("\\u002F", "/")
            .replace("\\u002f", "/")
            .replace("\\/", "/");
        if clean.is_empty()
            || clean.starts_with('#')
            || clean.starts_with("javascript:")
            || clean.starts_with("data:")
            || clean.starts_with("blob:")
        {
            return;
        }
        let Ok(resolved) = base.join(&clean) else {
            return;
        };
        if !matches!(resolved.scheme(), "http" | "https") {
            return;
        }
        let url_str = resolved.to_string();
        if !seen.insert(url_str.clone()) {
            return;
        }
        let Some(kind) = forced_kind.or_else(|| classify_kind(&url_str, None)) else {
            return;
        };
        items.push(PageSniffItem {
            filename: filename_of(&url_str, None),
            url: url_str,
            kind: kind.to_string(),
            mime: None,
            bytes: None,
            source: source.to_string(),
        });
    };

    // 1. <video> / <audio> / <source> 的 src / data-src
    for tag_name in ["video", "audio", "source"] {
        for tag in iter_tags(html, tag_name) {
            let forced = if tag_name == "audio" {
                Some("audio")
            } else if tag_name == "video" {
                Some("video")
            } else {
                None
            };
            for attr in ["src", "data-src"] {
                if let Some(v) = extract_attr(tag, attr) {
                    push(&v, tag_name, forced);
                }
            }
        }
    }

    // 2. OpenGraph / Twitter 媒体 meta
    for tag in iter_tags(html, "meta") {
        let prop = extract_attr(tag, "property")
            .or_else(|| extract_attr(tag, "name"))
            .unwrap_or_default()
            .to_ascii_lowercase();
        let forced = match prop.as_str() {
            "og:video" | "og:video:url" | "og:video:secure_url" | "twitter:player:stream" => {
                Some("video")
            }
            "og:audio" | "og:audio:url" => Some("audio"),
            _ => None,
        };
        if let (Some(kind), Some(content)) = (forced, extract_attr(tag, "content")) {
            push(&content, "meta", Some(kind));
        }
    }

    // 2.5 JSON-LD <script type="application/ld+json"> 中的 VideoObject
    for url in extract_json_ld_videos(html) {
        push(&url, "json-ld", Some("video"));
    }

    // 3. <a href="..."> 可下载链接
    for tag in iter_tags(html, "a") {
        if let Some(href) = extract_attr(tag, "href") {
            push(&href, "link", None);
        }
    }

    // 4. 脚本/JSON 字符串中内嵌的媒体直链
    for candidate in scan_embedded_urls(html) {
        push(&candidate, "script", None);
    }

    items.sort_by_key(|it| kind_rank(&it.kind));
    items.truncate(100);
    items
}

fn kind_rank(kind: &str) -> u8 {
    match kind {
        "hls" | "video" => 0,
        "audio" => 1,
        "archive" | "software" => 2,
        "doc" => 3,
        "ytdlp" => 4,
        _ => 5,
    }
}

fn classify_kind(url_str: &str, mime: Option<&str>) -> Option<&'static str> {
    if let Some(m) = mime {
        let base = m
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if base == "application/vnd.apple.mpegurl" || base == "application/x-mpegurl" {
            return Some("hls");
        }
        if base.starts_with("video/") {
            return Some("video");
        }
        if base.starts_with("audio/") {
            return Some("audio");
        }
        if matches!(
            base.as_str(),
            "application/zip"
                | "application/x-7z-compressed"
                | "application/x-rar-compressed"
                | "application/gzip"
                | "application/x-tar"
        ) {
            return Some("archive");
        }
        if base == "application/pdf" {
            return Some("doc");
        }
        if matches!(
            base.as_str(),
            "application/octet-stream"
                | "application/x-msdownload"
                | "application/x-apple-diskimage"
                | "application/vnd.android.package-archive"
        ) {
            return Some("software");
        }
    }

    let path = url::Url::parse(url_str).ok()?.path().to_ascii_lowercase();
    let ext = path.rsplit('.').next()?;
    match ext {
        "m3u8" | "m3u" => Some("hls"),
        "mp4" | "mkv" | "webm" | "mov" | "flv" | "avi" | "m4v" => Some("video"),
        "mp3" | "flac" | "m4a" | "aac" | "ogg" | "wav" | "opus" => Some("audio"),
        "zip" | "rar" | "7z" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" => Some("archive"),
        "exe" | "msi" | "dmg" | "pkg" | "apk" | "ipa" | "iso" | "deb" | "rpm" | "appimage" => {
            Some("software")
        }
        "pdf" | "epub" | "mobi" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" => Some("doc"),
        _ => None,
    }
}

fn filename_of(url_str: &str, mime: Option<&str>) -> String {
    let path = url::Url::parse(url_str)
        .map(|u| u.path().to_string())
        .unwrap_or_default();
    dl_filename::pick_name(None, None, &path, mime)
}

fn extract_json_ld_videos(html: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(found) = lower[pos..].find("<script") {
        let start = pos + found;
        let Some(open_end_rel) = html[start..].find('>') else {
            break;
        };
        let tag = &html[start..=start + open_end_rel];
        let content_start = start + open_end_rel + 1;
        let Some(close_rel) = lower[content_start..].find("</script>") else {
            break;
        };
        let content_end = content_start + close_rel;
        pos = content_end + "</script>".len();

        let script_type = extract_attr(tag, "type")
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !script_type.contains("ld+json") {
            continue;
        }
        let raw_json = html[content_start..content_end].trim();
        if raw_json.is_empty() {
            continue;
        }
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(raw_json) {
            collect_json_ld_videos(&val, &mut out);
        }
    }
    out
}

fn collect_json_ld_videos(val: &serde_json::Value, out: &mut Vec<String>) {
    match val {
        serde_json::Value::Array(arr) => {
            for item in arr {
                collect_json_ld_videos(item, out);
            }
        }
        serde_json::Value::Object(map) => {
            let is_video = map.get("@type").is_some_and(|t| match t {
                serde_json::Value::String(s) => s.eq_ignore_ascii_case("VideoObject"),
                serde_json::Value::Array(arr) => arr.iter().any(|x| {
                    x.as_str()
                        .is_some_and(|s| s.eq_ignore_ascii_case("VideoObject"))
                }),
                _ => false,
            });
            if is_video {
                if let Some(u) = map
                    .get("contentUrl")
                    .and_then(|v| v.as_str())
                    .or_else(|| map.get("embedUrl").and_then(|v| v.as_str()))
                {
                    out.push(u.to_string());
                }
            }
            for (k, v) in map {
                if k == "@graph" || k == "video" || k == "mainEntity" || k == "associatedMedia" {
                    collect_json_ld_videos(v, out);
                }
            }
        }
        _ => {}
    }
}

fn scan_embedded_urls(html: &str) -> Vec<String> {
    let normalized = html
        .replace("\\u002F", "/")
        .replace("\\u002f", "/")
        .replace("\\/", "/");
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(idx) = normalized[pos..].find("http") {
        let start = pos + idx;
        let rest = &normalized[start..];
        if !(rest.starts_with("http://") || rest.starts_with("https://")) {
            pos = start + 4;
            continue;
        }
        let end = rest
            .find(|c: char| {
                c.is_ascii_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | ')' | '`' | '\\')
            })
            .unwrap_or(rest.len());
        let candidate = &rest[..end];
        if candidate.len() <= 2048 && classify_kind(candidate, None).is_some() {
            out.push(candidate.to_string());
        }
        pos = start + end.max(4);
    }
    out
}

fn iter_tags<'a>(html: &'a str, tag_name: &str) -> Vec<&'a str> {
    let lower = html.to_ascii_lowercase();
    let needle = format!("<{tag_name}");
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(found) = lower[pos..].find(&needle) {
        let start = pos + found;
        let next_ch = lower.as_bytes().get(start + needle.len()).copied();
        if !matches!(next_ch, Some(b' ' | b'\t' | b'\r' | b'\n' | b'>' | b'/')) {
            pos = start + needle.len();
            continue;
        }
        if let Some(end) = html[start..].find('>') {
            out.push(&html[start..=start + end]);
            pos = start + end + 1;
        } else {
            break;
        }
    }
    out
}

fn extract_attr(tag: &str, attr: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut search_from = 0;
    while let Some(pos) = lower[search_from..].find(attr) {
        let idx = search_from + pos;
        let before_ok = idx == 0
            || lower.as_bytes()[idx - 1].is_ascii_whitespace()
            || lower.as_bytes()[idx - 1] == b'<';
        let after = idx + attr.len();
        if !before_ok {
            search_from = after;
            continue;
        }
        let val = tag[after..].trim_start();
        if let Some(eq_rest) = val.strip_prefix('=') {
            let vs = eq_rest.trim_start();
            if let Some(q) = vs.chars().next().filter(|c| *c == '"' || *c == '\'') {
                if let Some(end) = vs[1..].find(q) {
                    return Some(vs[1..1 + end].to_string());
                }
            } else {
                let end = vs
                    .find(|c: char| c.is_ascii_whitespace() || c == '>')
                    .unwrap_or(vs.len());
                return Some(vs[..end].to_string());
            }
        }
        search_from = after;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_video_hls_and_archive_from_html() {
        let html = r#"
        <html><head>
          <meta property="og:video" content="https://cdn.example.com/trailer.mp4?t=1">
        </head><body>
          <video controls><source src="/media/movie.m3u8"></video>
          <a href="/files/sdk-v2.zip">Download SDK</a>
          <script>var stream = "https:\/\/video.example.com\/live\/hd.mp4?sig=abc";</script>
        </body></html>
        "#;
        let items = extract_page_items("https://example.com/page.html", html);
        let urls: Vec<&str> = items.iter().map(|i| i.url.as_str()).collect();
        assert!(urls.contains(&"https://cdn.example.com/trailer.mp4?t=1"));
        assert!(urls.contains(&"https://example.com/media/movie.m3u8"));
        assert!(urls.contains(&"https://example.com/files/sdk-v2.zip"));
        assert!(urls.contains(&"https://video.example.com/live/hd.mp4?sig=abc"));
    }

    #[test]
    fn resolves_base_href_html_entities_and_json_ld_video_object() {
        let html = r#"
        <html><head>
          <base href="https://cdn.example.com/assets/">
          <script type="application/ld+json">
          {
            "@context": "https://schema.org",
            "@graph": [
              {
                "@type": "VideoObject",
                "name": "Demo Clip",
                "contentUrl": "https://media.example.com/clip.mp4?token=1&amp;ok=1"
              }
            ]
          }
          </script>
        </head><body>
          <a href="builds&#x2F;app.zip?a=1&amp;b=2">App</a>
        </body></html>
        "#;
        let items = extract_page_items("https://example.com/downloads", html);
        let urls: Vec<&str> = items.iter().map(|i| i.url.as_str()).collect();
        assert!(urls.contains(&"https://cdn.example.com/assets/builds/app.zip?a=1&b=2"));
        assert!(urls.contains(&"https://media.example.com/clip.mp4?token=1&ok=1"));
        let jld = items
            .iter()
            .find(|i| i.source == "json-ld")
            .expect("json-ld item");
        assert_eq!(jld.kind, "video");
    }
}
