//! 哈希校验（完成阶段校验集的增强项，DESIGN §5.3）。
//! 支持 sha512 / sha256 / sha1 / md5，流式计算（任意大小文件内存恒定）。

use std::io::Read;
use std::path::Path;

use dl_error::{DownloadError, ErrorKind, Res};
use sha2::Digest; // RustCrypto 三件套共用 digest 核心，此处一个 trait 覆盖 new/update/finalize

pub const ALGOS: &[&str] = &["sha512", "sha256", "sha1", "md5"];

/// 解析校验值：支持 `algo:hex`、`algo=hex` 或裸 hex，也支持 `sha256sum` 格式（`<hex>  filename`）。
/// 裸 hex 按长度推断：128=sha512、64=sha256、40=sha1、32=md5。
/// 返回 (算法, 小写 hex)。
pub fn parse_checksum(input: &str) -> Option<(String, String)> {
    let s = input.trim();
    if let Some((algo_raw, rest)) = s.split_once([':', '=']) {
        let algo = algo_raw.trim().to_ascii_lowercase().replace('-', "");
        let hex = rest
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let expect_len = match algo.as_str() {
            "sha512" => 128,
            "sha256" => 64,
            "sha1" => 40,
            "md5" => 32,
            _ => return None,
        };
        if hex.len() == expect_len && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some((algo, hex));
        }
        return None;
    }
    let hex = s
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if hex.is_empty() || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        128 => Some(("sha512".into(), hex)),
        64 => Some(("sha256".into(), hex)),
        40 => Some(("sha1".into(), hex)),
        32 => Some(("md5".into(), hex)),
        _ => None,
    }
}

fn hash_file(path: &Path, algo: &str) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 256 * 1024];
    macro_rules! digest_with {
        ($h:expr) => {{
            let mut h = $h;
            loop {
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
            }
            hex::encode(h.finalize())
        }};
    }
    let hex = match algo {
        "sha512" => digest_with!(sha2::Sha512::new()),
        "sha256" => digest_with!(sha2::Sha256::new()),
        "sha1" => digest_with!(sha1::Sha1::new()),
        "md5" => digest_with!(md5::Md5::new()),
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("unknown algo {algo}"),
            ))
        }
    };
    Ok(hex)
}

/// 流式计算文件哈希，返回小写 hex（支持 `sha512` / `sha256` / `sha1` / `md5`）。
pub fn compute_file_hash(path: &Path, algo: &str) -> Res<String> {
    let algo = algo.trim().to_ascii_lowercase().replace('-', "");
    hash_file(path, &algo).map_err(|e| {
        DownloadError::new(
            ErrorKind::FileAccessDenied,
            format!("hash {}: {e}", path.display()),
        )
    })
}

/// 流式计算文件哈希并与期望值比对（大小写不敏感）。true = 一致。
pub fn verify_file(path: &Path, algo: &str, expected_hex: &str) -> Res<bool> {
    let got = compute_file_hash(path, algo)?;
    Ok(got.eq_ignore_ascii_case(expected_hex))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_algo_prefixed_and_bare_hex() {
        assert_eq!(
            parse_checksum(
                "sha256:ABCD1234ABCD1234ABCD1234ABCD1234ABCD1234ABCD1234ABCD1234ABCD1234"
            ),
            Some((
                "sha256".into(),
                "abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234".into()
            ))
        );
        assert_eq!(
            parse_checksum("d41d8cd98f00b204e9800998ecf8427e"),
            Some(("md5".into(), "d41d8cd98f00b204e9800998ecf8427e".into()))
        );
        assert_eq!(
            parse_checksum("SHA1:d41d8cd98f00b204e9800998ecf8427ed41d8cd9"),
            Some((
                "sha1".into(),
                "d41d8cd98f00b204e9800998ecf8427ed41d8cd9".into()
            ))
        );
        assert!(parse_checksum("xyz:abcd").is_none());
        assert!(parse_checksum("abcd").is_none()); // 长度 4 不合法
        assert!(parse_checksum("").is_none());
    }

    #[test]
    fn verify_file_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.bin");
        std::fs::write(&p, b"hello checksum").unwrap();

        // sha256 of "hello checksum"
        let mut h = sha2::Sha256::new();
        h.update(b"hello checksum");
        let sha = hex::encode(h.finalize());

        assert!(verify_file(&p, "sha256", &sha).unwrap());
        assert!(!verify_file(&p, "sha256", &"0".repeat(64)).unwrap());
        // 大小写不敏感
        assert!(verify_file(&p, "SHA256", &sha.to_uppercase()).unwrap());
        assert_eq!(compute_file_hash(&p, "sha256").unwrap(), sha);
    }

    #[test]
    fn parses_sha512_equals_and_sha256sum_trailing_filename() {
        let s512 = "a".repeat(128);
        assert_eq!(parse_checksum(&s512), Some(("sha512".into(), s512.clone())));
        let s256 = "b".repeat(64);
        assert_eq!(
            parse_checksum(&format!("SHA-256={s256}")),
            Some(("sha256".into(), s256.clone()))
        );
        assert_eq!(
            parse_checksum(&format!("{s256}  *ubuntu-24.04.iso")),
            Some(("sha256".into(), s256))
        );
    }
}
