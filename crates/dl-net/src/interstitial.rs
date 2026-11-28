//! 下载中转页识别与直链穿透（问题 6）：
//! 处理 GitHub blob、Dropbox、Google Drive、SourceForge、MediaFire、
//! meta-refresh、JS 跳转与显式二进制下载按钮。

use std::io::Read;

const BINARY_EXTS: &[&str] = &[
    "zip",
    "rar",
    "7z",
    "tar",
    "gz",
    "tgz",
    "bz2",
    "xz",
    "zst",
    "exe",
    "msi",
    "dmg",
    "pkg",
    "apk",
    "ipa",
    "iso",
    "img",
    "deb",
    "rpm",
    "appimage",
    "mp4",
    "mkv",
    "webm",
    "mov",
    "avi",
    "mp3",
    "flac",
    "m4a",
    "wav",
    "pdf",
    "epub",
    "bin",
    "gguf",
    "safetensors",
    "onnx",
    "ckpt",
    "pt",
    "pth",
    "whl",
];

/// 解码常见 HTML 实体（`&amp;`, `&lt;`, `&gt;`, `&quot;`, `&apos;`, `&nbsp;`, `&#NN;`, `&#xNN;`）。
pub fn decode_html_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let reference = rest[1..].find(';').filter(|&n| n <= 10).and_then(|n| {
            let name = &rest[1..1 + n];
            let c = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some('\u{a0}'),
                _ => match name.strip_prefix('#') {
                    Some(hex) if hex.starts_with(['x', 'X']) => u32::from_str_radix(&hex[1..], 16)
                        .ok()
                        .and_then(char::from_u32),
                    Some(dec) => dec.parse().ok().and_then(char::from_u32),
                    None => None,
                },
            };
            c.map(|c| (c, n + 2))
        });
        match reference {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

pub(crate) fn decode_base64(input: &str) -> Option<Vec<u8>> {
    let clean: String = input.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    if clean.is_empty() {
        return None;
    }
    let pad = (4 - (clean.len() % 4)) % 4;
    let mut padded = clean;
    for _ in 0..pad {
        padded.push('=');
    }
    let bytes = padded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let val = |b: u8| -> Option<u8> {
        match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'+' | b'-' => Some(62),
            b'/' | b'_' => Some(63),
            _ => None,
        }
    };
    for chunk in bytes.as_chunks::<4>().0 {
        let a = val(chunk[0])?;
        let b = val(chunk[1])?;
        out.push((a << 2) | (b >> 4));
        if chunk[2] != b'=' {
            let c = val(chunk[2])?;
            out.push((b << 4) | (c >> 2));
            if chunk[3] != b'=' {
                let d = val(chunk[3])?;
                out.push((c << 6) | d);
            }
        } else if chunk[3] != b'=' {
            return None;
        }
    }
    Some(out)
}

/// 解析迅雷 (`thunder://`)、快车 (`flashget://`)、QQ 旋风 (`qqdl://`) 加密直链（参照 Ghost-Downloader-3）。
pub fn unwrap_wrapped_link(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let (scheme, payload) = trimmed.split_once("://")?;
    let scheme_lower = scheme.to_ascii_lowercase();
    if !matches!(scheme_lower.as_str(), "thunder" | "flashget" | "qqdl") {
        return None;
    }
    let payload = payload
        .split('&')
        .next()
        .unwrap_or("")
        .trim_end_matches('/');
    let decoded = decode_base64(payload)?;
    let text = String::from_utf8_lossy(&decoded).into_owned();
    let inner = match scheme_lower.as_str() {
        "thunder" => text.strip_prefix("AA")?.strip_suffix("ZZ")?.trim(),
        "flashget" => {
            let s = text.strip_prefix("[FLASHGET]").unwrap_or(text.as_str());
            s.strip_suffix("[FLASHGET]")
                .or_else(|| s.strip_suffix("[/FLASHGET]"))
                .unwrap_or(s)
                .trim()
        }
        "qqdl" => text.trim(),
        _ => return None,
    };
    let parsed = url::Url::parse(inner).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    Some(parsed.to_string())
}

/// 通过魔数嗅探响应体前部是否为 HTML 文档（处理缺少或误标 Content-Type 的中转页）。
pub fn looks_like_html_body(bytes: &[u8]) -> bool {
    let s = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let idx = s
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(s.len());
    let head = &s[idx..s.len().min(idx + 64)];
    let lower: Vec<u8> = head.iter().map(|b| b.to_ascii_lowercase()).collect();
    [
        b"<!doctype html".as_slice(),
        b"<html".as_slice(),
        b"<head".as_slice(),
        b"<meta".as_slice(),
        b"<script".as_slice(),
    ]
    .iter()
    .any(|pat| lower.starts_with(pat))
}

/// 常见托管站的网页链接归一化为直链（命中才返回 Some）。
pub fn normalize_download_url(raw: &str) -> Option<String> {
    let mut u = url::Url::parse(raw).ok()?;
    let host = u.host_str()?.to_ascii_lowercase();

    // GitHub: https://github.com/{owner}/{repo}/blob/{ref}/{path...}
    if host == "github.com" {
        let segs: Vec<&str> = u.path().split('/').filter(|s| !s.is_empty()).collect();
        if segs.len() >= 5 && segs[2] == "blob" {
            let owner = segs[0];
            let repo = segs[1];
            let rest = segs[3..].join("/");
            return Some(format!(
                "https://raw.githubusercontent.com/{owner}/{repo}/{rest}"
            ));
        }
    }

    // Dropbox: www.dropbox.com/s/... or /scl/fi/... -> dl=1
    if host == "www.dropbox.com" || host == "dropbox.com" {
        let path = u.path().to_string();
        if path.starts_with("/s/") || path.starts_with("/scl/fi/") {
            let has_dl1 = u.query_pairs().any(|(k, v)| k == "dl" && v == "1");
            if !has_dl1 {
                let pairs: Vec<(String, String)> = u
                    .query_pairs()
                    .filter(|(k, _)| k != "dl")
                    .map(|(k, v)| (k.into_owned(), v.into_owned()))
                    .collect();
                {
                    let mut qp = u.query_pairs_mut();
                    qp.clear();
                    for (k, v) in pairs {
                        qp.append_pair(&k, &v);
                    }
                    qp.append_pair("dl", "1");
                }
                return Some(u.to_string());
            }
        }
    }

    // Google Drive: /file/d/{id}/... 或 open?id={id}
    if host == "drive.google.com" {
        let segs: Vec<&str> = u.path().split('/').filter(|s| !s.is_empty()).collect();
        let file_id = if segs.len() >= 3 && segs[0] == "file" && segs[1] == "d" {
            Some(segs[2].to_string())
        } else if u.path() == "/open" {
            u.query_pairs()
                .find(|(k, _)| k == "id")
                .map(|(_, v)| v.into_owned())
        } else {
            None
        };
        if let Some(id) = file_id.filter(|s| !s.is_empty()) {
            return Some(format!(
                "https://drive.usercontent.google.com/download?id={id}&export=download&confirm=t"
            ));
        }
    }

    // Hugging Face / hf-mirror: /blob/{rev}/{path} -> /resolve/{rev}/{path}
    if matches!(
        host.as_str(),
        "huggingface.co" | "www.huggingface.co" | "hf-mirror.com" | "www.hf-mirror.com"
    ) {
        let segs: Vec<&str> = u.path().split('/').filter(|s| !s.is_empty()).collect();
        let has_prefix = matches!(segs.first().copied(), Some("datasets" | "spaces"));
        let action_idx = if has_prefix { 3 } else { 2 };
        if segs.len() >= action_idx + 3 && segs[action_idx] == "blob" {
            let prefix = segs[..action_idx].join("/");
            let rest = segs[action_idx + 1..].join("/");
            let scheme = u.scheme();
            let q = u.query().map(|q| format!("?{q}")).unwrap_or_default();
            return Some(format!("{scheme}://{host}/{prefix}/resolve/{rest}{q}"));
        }
    }

    // ModelScope: /{models|datasets}/{org}/{repo}/file/view/{rev}/{path} -> .../resolve/{rev}/{path}
    if matches!(host.as_str(), "modelscope.cn" | "www.modelscope.cn") {
        let segs: Vec<&str> = u.path().split('/').filter(|s| !s.is_empty()).collect();
        if segs.len() >= 7
            && matches!(segs[0], "models" | "datasets")
            && segs[3] == "file"
            && segs[4] == "view"
        {
            let head = segs[..3].join("/");
            let rest = segs[5..].join("/");
            return Some(format!("https://{host}/{head}/resolve/{rest}"));
        }
    }

    // GitLab: /-/blob/ -> /-/raw/
    if matches!(host.as_str(), "gitlab.com" | "www.gitlab.com") && u.path().contains("/-/blob/") {
        let next_path = u.path().replacen("/-/blob/", "/-/raw/", 1);
        let q = u.query().map(|q| format!("?{q}")).unwrap_or_default();
        return Some(format!("https://{host}{next_path}{q}"));
    }

    // Gitee: /{owner}/{repo}/blob/{ref}/{path} -> .../raw/{ref}/{path}
    if matches!(host.as_str(), "gitee.com" | "www.gitee.com") {
        let segs: Vec<&str> = u.path().split('/').filter(|s| !s.is_empty()).collect();
        if segs.len() >= 5 && segs[2] == "blob" {
            let owner = segs[0];
            let repo = segs[1];
            let rest = segs[3..].join("/");
            return Some(format!("https://{host}/{owner}/{repo}/raw/{rest}"));
        }
    }

    // arXiv: /abs/{id} -> https://arxiv.org/pdf/{id}.pdf
    if matches!(host.as_str(), "arxiv.org" | "www.arxiv.org") {
        if let Some(id) = u
            .path()
            .strip_prefix("/abs/")
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let clean = id.strip_suffix(".pdf").unwrap_or(id);
            return Some(format!("https://arxiv.org/pdf/{clean}.pdf"));
        }
    }

    None
}

pub fn is_html_mime(mime: Option<&str>) -> bool {
    let Some(m) = mime else {
        return false;
    };
    let base = m
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    base == "text/html" || base == "application/xhtml+xml"
}

pub fn url_expects_html(raw: &str) -> bool {
    let Ok(u) = url::Url::parse(raw) else {
        return false;
    };
    let Some(last) = u.path_segments().and_then(|mut s| s.next_back()) else {
        return false;
    };
    let lower = last.to_ascii_lowercase();
    lower.ends_with(".html")
        || lower.ends_with(".htm")
        || lower.ends_with(".xhtml")
        || lower.ends_with(".mhtml")
}

pub fn extract_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start_tag = lower.find("<title")?;
    let after_open = lower[start_tag..].find('>')? + start_tag + 1;
    let end_tag = lower[after_open..].find("</title>")? + after_open;
    let raw = &html[after_open..end_tag];
    let decoded = decode_html_entities(raw);
    let clean: String = decoded.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = clean.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.chars().take(120).collect())
    }
}

pub fn decode_body(raw: &[u8], content_encoding: Option<&str>) -> String {
    let enc = content_encoding
        .unwrap_or("")
        .split(',')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let decoded: Vec<u8> = match enc.as_str() {
        "gzip" | "x-gzip" => {
            let mut d = flate2::read::GzDecoder::new(raw);
            let mut out = Vec::new();
            if d.read_to_end(&mut out).is_ok() && !out.is_empty() {
                out
            } else {
                raw.to_vec()
            }
        }
        "deflate" => {
            let mut d = flate2::read::ZlibDecoder::new(raw);
            let mut out = Vec::new();
            if d.read_to_end(&mut out).is_ok() && !out.is_empty() {
                out
            } else {
                raw.to_vec()
            }
        }
        "br" => {
            let mut d = brotli::Decompressor::new(raw, 4096);
            let mut out = Vec::new();
            if d.read_to_end(&mut out).is_ok() && !out.is_empty() {
                out
            } else {
                raw.to_vec()
            }
        }
        _ => raw.to_vec(),
    };
    String::from_utf8_lossy(&decoded).into_owned()
}

/// 从中转页 HTML 提取真实下载直链。
pub fn extract_direct_url(base_url: &str, html: &str) -> Option<String> {
    let base = url::Url::parse(base_url).ok()?;

    // 1. <meta http-equiv="refresh" content="5; url=...">
    if let Some(u) = find_meta_refresh(&base, html) {
        return Some(u);
    }

    // 2. Google Drive 病毒扫描确认表单 <form id="download-form" action="...">
    if let Some(u) = find_gdrive_form(&base, html) {
        return Some(u);
    }

    // 3. SourceForge 直链
    if let Some(u) = find_sourceforge_link(&base, html) {
        return Some(u);
    }

    // 4. MediaFire 直链
    if let Some(u) = find_mediafire_link(&base, html) {
        return Some(u);
    }

    // 5. JS 跳转 (window.location / location.replace)
    if let Some(u) = find_js_redirect(&base, html) {
        return Some(u);
    }

    // 6. 显式 download 属性或与原 URL 匹配的二进制链接
    if let Some(u) = find_explicit_download_anchor(&base, html) {
        return Some(u);
    }

    None
}

fn resolve_candidate(base: &url::Url, raw: &str) -> Option<String> {
    let cleaned = decode_html_entities(raw.trim())
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .to_string();
    if cleaned.is_empty()
        || cleaned.starts_with('#')
        || cleaned.starts_with("javascript:")
        || cleaned.starts_with("data:")
    {
        return None;
    }
    let resolved = base.join(&cleaned).ok()?;
    if !matches!(resolved.scheme(), "http" | "https") {
        return None;
    }
    // 忽略指向自身或站点根路径的无效跳转
    if resolved.as_str() == base.as_str() {
        return None;
    }
    if resolved.path() == "/" && resolved.query().is_none() {
        return None;
    }
    Some(resolved.to_string())
}

fn has_binary_ext(url_str: &str) -> bool {
    let Ok(u) = url::Url::parse(url_str) else {
        return false;
    };
    let Some(last) = u.path_segments().and_then(|mut it| it.next_back()) else {
        return false;
    };
    let lower = last.to_ascii_lowercase();
    BINARY_EXTS
        .iter()
        .any(|ext| lower.ends_with(&format!(".{ext}")))
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
        let rest = &tag[after..];
        let trimmed = rest.trim_start();
        if let Some(eq_rest) = trimmed.strip_prefix('=') {
            let val_start = eq_rest.trim_start();
            if let Some(q) = val_start.chars().next().filter(|c| *c == '"' || *c == '\'') {
                let inner = &val_start[1..];
                if let Some(end) = inner.find(q) {
                    return Some(inner[..end].to_string());
                }
            } else {
                let end = val_start
                    .find(|c: char| c.is_ascii_whitespace() || c == '>')
                    .unwrap_or(val_start.len());
                return Some(val_start[..end].to_string());
            }
        }
        search_from = after;
    }
    None
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

fn find_meta_refresh(base: &url::Url, html: &str) -> Option<String> {
    for tag in iter_tags(html, "meta") {
        let equiv = extract_attr(tag, "http-equiv")
            .unwrap_or_default()
            .to_ascii_lowercase();
        if equiv != "refresh" {
            continue;
        }
        let Some(content) = extract_attr(tag, "content") else {
            continue;
        };
        let lower = content.to_ascii_lowercase();
        if let Some(idx) = lower.find("url=") {
            let raw = &content[idx + 4..];
            if let Some(url) = resolve_candidate(base, raw) {
                return Some(url);
            }
        }
    }
    None
}

fn find_gdrive_form(base: &url::Url, html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let form_start = lower
        .find("id=\"download-form\"")
        .or_else(|| lower.find("id='download-form'"))?;
    let tag_start = lower[..form_start].rfind("<form")?;
    let form_end = lower[form_start..]
        .find("</form>")
        .map(|e| form_start + e)?;
    let form_block = &html[tag_start..form_end];
    let form_open_end = form_block.find('>')?;
    let form_tag = &form_block[..=form_open_end];
    let action = extract_attr(form_tag, "action")?;
    let mut action_url = url::Url::parse(&resolve_candidate(base, &action)?).ok()?;
    let mut pairs = Vec::new();
    for input in iter_tags(form_block, "input") {
        if let (Some(name), Some(val)) = (extract_attr(input, "name"), extract_attr(input, "value"))
        {
            pairs.push((name, val));
        }
    }
    if !pairs.is_empty() {
        let mut qp = action_url.query_pairs_mut();
        for (k, v) in pairs {
            qp.append_pair(&k, &v);
        }
    }
    Some(action_url.to_string())
}

fn find_sourceforge_link(base: &url::Url, html: &str) -> Option<String> {
    for tag in iter_tags(html, "a") {
        let Some(href) = extract_attr(tag, "href") else {
            continue;
        };
        let class = extract_attr(tag, "class")
            .unwrap_or_default()
            .to_ascii_lowercase();
        if href.contains("downloads.sourceforge.net/") || class.contains("direct-download") {
            if let Some(url) = resolve_candidate(base, &href) {
                return Some(url);
            }
        }
    }
    None
}

fn find_mediafire_link(base: &url::Url, html: &str) -> Option<String> {
    for tag in iter_tags(html, "a") {
        let id = extract_attr(tag, "id").unwrap_or_default();
        let Some(href) = extract_attr(tag, "href") else {
            continue;
        };
        if (id == "downloadButton" || href.contains(".mediafire.com/")) && href.starts_with("http")
        {
            if let Some(url) = resolve_candidate(base, &href) {
                return Some(url);
            }
        }
    }
    None
}

fn find_js_redirect(base: &url::Url, html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    for pat in [
        "window.location.href",
        "location.href",
        "window.location.replace(",
        "location.replace(",
    ] {
        let mut pos = 0;
        while let Some(idx) = lower[pos..].find(pat) {
            let after = pos + idx + pat.len();
            let slice = &html[after..html.len().min(after + 512)];
            if let Some(q_idx) = slice.find(['"', '\'']) {
                let quote = slice.as_bytes()[q_idx] as char;
                let rest = &slice[q_idx + 1..];
                if let Some(end) = rest.find(quote) {
                    let candidate = &rest[..end];
                    if let Some(resolved) = resolve_candidate(base, candidate) {
                        if has_binary_ext(&resolved) || resolved.contains("download") {
                            return Some(resolved);
                        }
                    }
                }
            }
            pos = after;
        }
    }
    None
}

fn find_explicit_download_anchor(base: &url::Url, html: &str) -> Option<String> {
    let base_last = base
        .path_segments()
        .and_then(|mut it| it.next_back())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase());

    let mut fallback: Option<String> = None;
    for tag in iter_tags(html, "a") {
        let Some(href) = extract_attr(tag, "href") else {
            continue;
        };
        let Some(resolved) = resolve_candidate(base, &href) else {
            continue;
        };
        let has_dl_attr = tag.to_ascii_lowercase().contains(" download");
        if has_dl_attr && (has_binary_ext(&resolved) || resolved.contains("download")) {
            return Some(resolved);
        }
        if has_binary_ext(&resolved) {
            if let Some(ref target_name) = base_last {
                if resolved.to_ascii_lowercase().contains(target_name) {
                    return Some(resolved);
                }
            }
            if fallback.is_none() {
                fallback = Some(resolved);
            }
        }
    }
    fallback
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_github_dropbox_gdrive() {
        assert_eq!(
            normalize_download_url("https://github.com/foo/bar/blob/main/dist/app.zip").as_deref(),
            Some("https://raw.githubusercontent.com/foo/bar/main/dist/app.zip")
        );
        assert_eq!(
            normalize_download_url("https://www.dropbox.com/s/abc123/test.zip?dl=0").as_deref(),
            Some("https://www.dropbox.com/s/abc123/test.zip?dl=1")
        );
        assert_eq!(
            normalize_download_url("https://drive.google.com/file/d/1AbCdEfG/view?usp=sharing")
                .as_deref(),
            Some("https://drive.usercontent.google.com/download?id=1AbCdEfG&export=download&confirm=t")
        );
    }

    #[test]
    fn extracts_meta_refresh_and_sourceforge() {
        let html = r#"<html><head><title>Downloading...</title><meta http-equiv="refresh" content="3; url=/files/release-1.0.zip"></head></html>"#;
        assert_eq!(
            extract_direct_url("https://example.com/get", html).as_deref(),
            Some("https://example.com/files/release-1.0.zip")
        );
        let sf = r#"<html><body><a class="direct-download" href="https://downloads.sourceforge.net/project/demo/demo.zip?ts=1">Download</a></body></html>"#;
        assert_eq!(
            extract_direct_url(
                "https://sourceforge.net/projects/demo/files/demo.zip/download",
                sf
            )
            .as_deref(),
            Some("https://downloads.sourceforge.net/project/demo/demo.zip?ts=1")
        );
    }

    #[test]
    fn extracts_gdrive_confirm_form_and_mediafire() {
        let gd = r#"<html><body><form id="download-form" action="https://drive.usercontent.google.com/download" method="get"><input type="hidden" name="id" value="XYZ"><input type="hidden" name="confirm" value="t"></form></body></html>"#;
        let out = extract_direct_url("https://drive.google.com/uc?id=XYZ", gd).unwrap();
        assert!(out.contains("id=XYZ"));
        assert!(out.contains("confirm=t"));

        let mf = r#"<html><body><a id="downloadButton" href="https://download123.mediafire.com/abc/file.7z">Download</a></body></html>"#;
        assert_eq!(
            extract_direct_url("https://www.mediafire.com/file/abc/file.7z/file", mf).as_deref(),
            Some("https://download123.mediafire.com/abc/file.7z")
        );
    }

    #[test]
    fn login_wall_returns_none() {
        let html = r#"<html><head><title>Sign in &amp; Verify</title></head><body><form action="/login"><input name="user"></form></body></html>"#;
        assert_eq!(extract_title(html).as_deref(), Some("Sign in & Verify"));
        assert_eq!(
            extract_direct_url("https://example.com/file.zip", html),
            None
        );
    }

    #[test]
    fn unwraps_thunder_flashget_qqdl_and_rejects_nested() {
        // AAhttps://example.com/a.zipZZ -> QUFodHRwczovL2V4YW1wbGUuY29tL2EuemlwWlo=
        assert_eq!(
            unwrap_wrapped_link("thunder://QUFodHRwczovL2V4YW1wbGUuY29tL2EuemlwWlo=").as_deref(),
            Some("https://example.com/a.zip")
        );
        // [FLASHGET]https://example.com/b.iso[FLASHGET] -> W0ZMQVNIR0VUXWh0dHBzOi8vZXhhbXBsZS5jb20vYi5pc29bRkxBU0hHRVRd
        assert_eq!(
            unwrap_wrapped_link(
                "FLASHGET://W0ZMQVNIR0VUXWh0dHBzOi8vZXhhbXBsZS5jb20vYi5pc29bRkxBU0hHRVRd&ref=1"
            )
            .as_deref(),
            Some("https://example.com/b.iso")
        );
        // https://example.com/c.tar.gz -> aHR0cHM6Ly9leGFtcGxlLmNvbS9jLnRhci5neg==
        assert_eq!(
            unwrap_wrapped_link("qqdl://aHR0cHM6Ly9leGFtcGxlLmNvbS9jLnRhci5neg==").as_deref(),
            Some("https://example.com/c.tar.gz")
        );
        // Nested thunder:// inside thunder:// must be rejected
        // AAthunder://QUFodHRwczovL2EuY29tWlo=ZZ -> QUF0aHVuZGVyOi8vUVVGaGRIUndjem92TDJFdVkyOXRXbG89Wlo=
        assert_eq!(
            unwrap_wrapped_link("thunder://QUF0aHVuZGVyOi8vUVVGaGRIUndjem92TDJFdVkyOXRXbG89Wlo="),
            None
        );
        assert_eq!(unwrap_wrapped_link("thunder://invalid!!!"), None);
    }

    #[test]
    fn decodes_html_entities_and_detects_html_body() {
        assert_eq!(
            decode_html_entities("https://cdn.example.com/dl?a=1&amp;b=2&#x2F;ok&quot;"),
            "https://cdn.example.com/dl?a=1&b=2/ok\""
        );
        assert!(looks_like_html_body(
            b"\xEF\xBB\xBF  \n<!DOCTYPE html><html>"
        ));
        assert!(looks_like_html_body(b"  <HTML><head>"));
        assert!(!looks_like_html_body(b"PK\x03\x04\x14\x00"));
    }

    #[test]
    fn normalizes_hf_modelscope_gitlab_gitee_arxiv() {
        assert_eq!(
            normalize_download_url(
                "https://huggingface.co/Qwen/Qwen2.5-7B-Instruct-GGUF/blob/main/qwen2.5-7b-instruct-q4_k_m.gguf"
            )
            .as_deref(),
            Some(
                "https://huggingface.co/Qwen/Qwen2.5-7B-Instruct-GGUF/resolve/main/qwen2.5-7b-instruct-q4_k_m.gguf"
            )
        );
        assert_eq!(
            normalize_download_url(
                "https://hf-mirror.com/datasets/org/ds/blob/v1.0/data/train.parquet"
            )
            .as_deref(),
            Some("https://hf-mirror.com/datasets/org/ds/resolve/v1.0/data/train.parquet")
        );
        assert_eq!(
            normalize_download_url(
                "https://modelscope.cn/models/Qwen/Qwen2.5-7B/file/view/master/config.json"
            )
            .as_deref(),
            Some("https://modelscope.cn/models/Qwen/Qwen2.5-7B/resolve/master/config.json")
        );
        assert_eq!(
            normalize_download_url("https://gitlab.com/group/sub/repo/-/blob/main/release.zip")
                .as_deref(),
            Some("https://gitlab.com/group/sub/repo/-/raw/main/release.zip")
        );
        assert_eq!(
            normalize_download_url("https://gitee.com/owner/repo/blob/master/dist/app.exe")
                .as_deref(),
            Some("https://gitee.com/owner/repo/raw/master/dist/app.exe")
        );
        assert_eq!(
            normalize_download_url("https://arxiv.org/abs/2401.12345").as_deref(),
            Some("https://arxiv.org/pdf/2401.12345.pdf")
        );
    }
}
