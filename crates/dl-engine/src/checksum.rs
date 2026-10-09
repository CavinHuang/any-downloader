//! 哈希校验（完成阶段校验集的增强项，DESIGN §5.3）。
//! 支持 sha256 / sha1 / md5，流式计算（任意大小文件内存恒定）。

use std::io::Read;
use std::path::Path;

use dl_error::{DownloadError, ErrorKind, Res};
use sha2::Digest; // RustCrypto 三件套共用 digest 核心，此处一个 trait 覆盖 new/update/finalize

pub const ALGOS: &[&str] = &["sha256", "sha1", "md5"];

/// 解析校验值："algo:hex" 或裸 hex（按长度推断：64=sha256、40=sha1、32=md5）。
/// 返回 (算法, 小写 hex)。
pub fn parse_checksum(input: &str) -> Option<(String, String)> {
    let s = input.trim();
    if let Some((algo, hex)) = s.split_once(':') {
        let algo = algo.trim().to_ascii_lowercase();
        let hex = hex.trim().to_ascii_lowercase();
        if ALGOS.contains(&algo.as_str())
            && !hex.is_empty()
            && hex.len() % 2 == 0
            && hex.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Some((algo, hex));
        }
        return None;
    }
    let hex = s.trim().to_ascii_lowercase();
    if hex.is_empty() || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        64 => Some(("sha256".into(), hex)),
        40 => Some(("sha1".into(), hex)),
        32 => Some(("md5".into(), hex)),
        _ => None,
    }
}

fn hash_file(path: &Path, algo: &str) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 64 * 1024];
    let hex = match algo {
        "sha256" => {
            let mut h = sha2::Sha256::new();
            loop {
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
            }
            hex::encode(h.finalize())
        }
        "sha1" => {
            let mut h = sha1::Sha1::new();
            loop {
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
            }
            hex::encode(h.finalize())
        }
        "md5" => {
            let mut h = md5::Md5::new();
            loop {
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
            }
            hex::encode(h.finalize())
        }
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("unknown algo {algo}"),
            ))
        }
    };
    Ok(hex)
}

/// 流式计算文件哈希并与期望值比对（大小写不敏感）。true = 一致。
pub fn verify_file(path: &Path, algo: &str, expected_hex: &str) -> Res<bool> {
    let algo = algo.to_ascii_lowercase();
    let got = hash_file(path, &algo).map_err(|e| {
        DownloadError::new(
            ErrorKind::FileAccessDenied,
            format!("hash {}: {e}", path.display()),
        )
    })?;
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
    }
}
