//! 文件名确定与净化（DESIGN §5.5）：确定性算法而非模糊「校验」。
//! 来源优先级：Content-Disposition（RFC 5987 filename* 优先）> URL path 末段 > 兜底；
//! 扩展名缺失时按 MIME 补充。

use std::path::{Path, PathBuf};

use dl_error::{DownloadError, ErrorKind};

/// 冲突策略，finalize 原子 rename 阶段生效（DESIGN §5.5）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    /// 默认：`name (1).ext` 递增。
    #[default]
    Rename,
    Overwrite,
    Skip,
    Ask,
}

impl ConflictPolicy {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "rename" => Some(Self::Rename),
            "overwrite" => Some(Self::Overwrite),
            "skip" => Some(Self::Skip),
            "ask" => Some(Self::Ask),
            _ => None,
        }
    }
}

const WINDOWS_RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// 净化单个文件名成分：剥离路径成分 → 替换非法字符 → 保留名加前缀 → 去尾部点空格。
pub fn sanitize(input: &str) -> String {
    // 1. 剥离路径成分（Content-Disposition/URL 可能携带路径，防穿越）
    let base = input
        .rsplit(['/', '\\'])
        .find(|s| !s.is_empty())
        .unwrap_or(input);
    // 2. 非法字符与控制字符
    let cleaned: String = base
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' => '-',
            c if (c as u32) < 0x20 => '-',
            c => c,
        })
        .collect();
    // 3. Windows 保留设备名（CON.txt 同样保留）
    let stem = cleaned.split('.').next().unwrap_or("");
    let cleaned = if WINDOWS_RESERVED.contains(&stem.to_ascii_uppercase().as_str()) {
        format!("_{cleaned}")
    } else {
        cleaned
    };
    // 4. 尾部空格与点（Win32 可建但 Explorer 行为异常）
    let trimmed = cleaned.trim_end_matches([' ', '.']);
    // 5. 兜底
    if trimmed.is_empty() {
        "download".to_string()
    } else {
        trimmed.to_string()
    }
}

/// 按 MIME 补扩展名（常见类型小表）。
pub fn extension_for_mime(mime: &str) -> Option<&'static str> {
    let base = mime.split(';').next()?.trim().to_ascii_lowercase();
    Some(match base.as_str() {
        "video/mp4" => "mp4",
        "video/x-matroska" => "mkv",
        "video/webm" => "webm",
        "video/mpeg" => "mpeg",
        "video/quicktime" => "mov",
        "audio/mpeg" => "mp3",
        "audio/mp4" => "m4a",
        "audio/aac" => "aac",
        "audio/ogg" => "ogg",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/flac" => "flac",
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "application/pdf" => "pdf",
        "application/zip" => "zip",
        "application/gzip" => "gz",
        "application/x-7z-compressed" => "7z",
        "application/x-rar-compressed" | "application/vnd.rar" => "rar",
        "application/x-ns-proxy-autoconfig" => "pac",
        "text/plain" => "txt",
        "text/html" => "html",
        "application/json" => "json",
        "application/vnd.apple.mpegurl" | "application/x-mpegurl" => "m3u8",
        "application/dash+xml" => "mpd",
        _ => return None,
    })
}

/// 确定文件名（不含目录）。用户指定名（任务创建时给定）优先于探测结果。
pub fn pick_name(
    user: Option<&str>,
    disposition: Option<&str>,
    url_path: &str,
    mime: Option<&str>,
) -> String {
    if let Some(u) = user.map(sanitize) {
        return u;
    }
    let raw = disposition
        .map(|s| s.to_string())
        .or_else(|| {
            url::Url::parse(&format!("http://x{}", url_path))
                .ok()
                .and_then(|u| {
                    u.path_segments()
                        .and_then(|mut it| it.next_back())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                })
        })
        .unwrap_or_else(|| "download".to_string());
    let name = sanitize(&raw);
    if Path::new(&name).extension().is_none() {
        if let Some(ext) = mime.and_then(extension_for_mime) {
            return format!("{name}.{ext}");
        }
    }
    name
}

/// 解析 Content-Disposition 的 filename*（RFC 5987，优先）与 filename。
pub fn disposition_filename(cd: &str) -> Option<String> {
    let mut plain = None;
    for part in cd.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix("filename*=") {
            // charset'lang'percent-encoded
            let mut it = v.splitn(3, '\'');
            let charset = it.next().unwrap_or("utf-8");
            let encoded = it.nth(1);
            if let (Some(encoded), true) = (encoded, charset.eq_ignore_ascii_case("utf-8")) {
                if let Some(decoded) = percent_decode_utf8(encoded) {
                    return Some(decoded);
                }
            }
        } else if let Some(v) = part.strip_prefix("filename=") {
            let v = v.trim();
            let v = v.strip_prefix('"').unwrap_or(v);
            let v = v.strip_suffix('"').unwrap_or(v);
            plain = Some(v.to_string());
        }
    }
    plain
}

fn percent_decode_utf8(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            let byte = u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// 冲突策略应用：返回最终目标路径。
/// Skip → Ok(None)；Overwrite 原样返回（调用方 rename 前删除）；Rename 给出递增名。
pub fn resolve_conflict(
    dir: &Path,
    name: &str,
    policy: ConflictPolicy,
) -> Result<Option<PathBuf>, DownloadError> {
    let target = dir.join(name);
    if !target.exists() {
        return Ok(Some(target));
    }
    match policy {
        ConflictPolicy::Overwrite => Ok(Some(target)),
        ConflictPolicy::Skip => Ok(None),
        ConflictPolicy::Ask => Err(DownloadError::new(
            ErrorKind::NameConflict {
                path: target.display().to_string(),
            },
            format!("target already exists: {}", target.display()),
        )),
        ConflictPolicy::Rename => {
            let stem = target
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("download")
                .to_string();
            let ext = target
                .extension()
                .and_then(|s| s.to_str())
                .map(|e| format!(".{e}"))
                .unwrap_or_default();
            for n in 1..10000u32 {
                let candidate = dir.join(format!("{stem} ({n}){ext}"));
                if !candidate.exists() {
                    return Ok(Some(candidate));
                }
            }
            Err(DownloadError::new(
                ErrorKind::NameConflict {
                    path: target.display().to_string(),
                },
                "could not find a free numbered name",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_path_components() {
        assert_eq!(sanitize("../../etc/passwd"), "passwd");
        assert_eq!(sanitize("a\\b\\c.txt"), "c.txt");
        assert_eq!(sanitize("C:\\evil\\x.exe"), "x.exe");
    }

    #[test]
    fn sanitize_windows_rules() {
        assert_eq!(sanitize("a<b>:c?.txt"), "a-b--c-.txt");
        assert_eq!(sanitize("CON.mp4"), "_CON.mp4");
        assert_eq!(sanitize("com1"), "_com1");
        assert_eq!(sanitize("trailing. .."), "trailing");
        assert_eq!(sanitize("   "), "download");
        assert_eq!(sanitize(""), "download");
    }

    #[test]
    fn mime_extension_supplement() {
        assert_eq!(
            pick_name(None, Some("video"), "/a/b", Some("video/mp4")),
            "video.mp4"
        );
        assert_eq!(
            pick_name(None, Some("clip.mp4"), "/a", Some("video/webm")),
            "clip.mp4"
        );
    }

    #[test]
    fn disposition_prefers_rfc5987() {
        assert_eq!(
            disposition_filename(
                "attachment; filename=\"fallback.bin\"; filename*=UTF-8''%E8%A7%86%E9%A2%91.mp4"
            ),
            Some("视频.mp4".to_string())
        );
        assert_eq!(
            disposition_filename("attachment; filename=\"a b.zip\""),
            Some("a b.zip".to_string())
        );
    }

    #[test]
    fn url_last_segment_used() {
        assert_eq!(
            pick_name(None, None, "/files/big+file.zip", None),
            "big+file.zip"
        );
        assert_eq!(pick_name(None, None, "/", None), "download");
    }

    #[test]
    fn user_filename_wins() {
        assert_eq!(
            pick_name(Some("mine.bin"), Some("server.bin"), "/x", None),
            "mine.bin"
        );
        // 用户名也过净化：取末段 + 替换非法字符
        assert_eq!(pick_name(Some("a/b<c"), None, "/x", None), "b-c");
    }

    #[test]
    fn conflict_rename_increments() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, b"x").unwrap();
        let r = resolve_conflict(dir.path(), "a.txt", ConflictPolicy::Rename)
            .unwrap()
            .unwrap();
        assert_eq!(r, dir.path().join("a (1).txt"));
        assert!(resolve_conflict(dir.path(), "a.txt", ConflictPolicy::Skip)
            .unwrap()
            .is_none());
        assert_eq!(
            resolve_conflict(dir.path(), "a.txt", ConflictPolicy::Overwrite)
                .unwrap()
                .unwrap(),
            p
        );
        assert!(resolve_conflict(dir.path(), "a.txt", ConflictPolicy::Ask).is_err());
    }
}
