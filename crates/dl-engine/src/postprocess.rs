//! 下载后处理（docs/MEDIA-ADAPTERS.md §5.1）。
//!
//! 目前唯一算法：视频号 Isaac64-XOR。规格（已用 Evil0ctal/WeChat-Channels-
//! Video-File-Decryption 的公开样本逐位对拍锁定）：
//! - 只有文件**前 131072 字节（128KB）**被加密；
//! - `decode_key` 为十进制数字符串（如 "2136343393"），解析为 u64 后放入
//!   Isaac64 的 `randrsl[0]`（其余为 0）；
//! - 密钥流 = 16384 个 `next_u64()` 的**大端**字节序拼接（131072 字节），
//!   生成后对整个缓冲做 **reverse**；
//! - `plain[i] = cipher[i] ^ keystream[i]`（i < min(131072, len)）。
//! - 解密成功标志：MP4 头部出现 `ftyp`。
//!
//! 依赖 rand_isaac（已停维护但行为冻结）——这里不能升级版本：必须与微信
//! WASM 的输出逐位一致。

#![allow(deprecated)]

use dl_error::{DownloadError, ErrorKind};
use dl_model::PostProcess;
use std::path::Path;

/// 加密前缀长度：微信只加密文件头 128KB。
pub const ENCRYPTED_PREFIX: usize = 131072;

/// Isaac64 密钥流（已 reverse，可直接 XOR）。
pub fn isaac64_keystream_reversed(seed: u64, n: usize) -> Vec<u8> {
    use rand_core::{Rng, SeedableRng};
    let mut seed_bytes = [0u8; 32]; // randrsl[0..8] = seed (LE)，其余 0
    seed_bytes[..8].copy_from_slice(&seed.to_le_bytes());
    let mut rng = rand_isaac::Isaac64Rng::from_seed(seed_bytes);
    let mut ks = Vec::with_capacity(n);
    while ks.len() < n {
        ks.extend_from_slice(&rng.next_u64().to_be_bytes());
    }
    ks.truncate(n);
    ks.reverse();
    ks
}

/// 执行后处理列表（按序）。文件以 `.dec` 临时副本落地后原子改名，
/// 中途中断不会产出半解密文件（重跑幂等）。
pub fn apply_all(path: &Path, steps: &[PostProcess]) -> Result<(), DownloadError> {
    for step in steps {
        match step.algo {
            dl_model::PostProcessAlgo::Isaac64Xor => decrypt_isaac64_xor(path, &step.seed)?,
        }
    }
    Ok(())
}

fn decrypt_isaac64_xor(path: &Path, seed_str: &str) -> Result<(), DownloadError> {
    let seed: u64 = seed_str.trim().parse().map_err(|_| {
        DownloadError::new(
            ErrorKind::DecryptFailed,
            format!("bad isaac64 seed: {seed_str:?}"),
        )
    })?;
    let meta = std::fs::metadata(path).map_err(io_err(path))?;
    let n = meta.len().min(ENCRYPTED_PREFIX as u64) as usize;
    if n == 0 {
        return Ok(());
    }

    let mut plain = std::fs::read(path).map_err(io_err(path))?;
    let ks = isaac64_keystream_reversed(seed, n);
    for i in 0..n {
        plain[i] ^= ks[i];
    }

    // 原子落地：写 .dec 副本 → fsync → 改名盖回原文件
    let tmp = path.with_extension("dec");
    std::fs::write(&tmp, &plain).map_err(io_err(&tmp))?;
    if let Ok(f) = std::fs::File::open(&tmp) {
        let _ = f.sync_all();
    }
    std::fs::rename(&tmp, path).map_err(io_err(path))?;
    Ok(())
}

fn io_err(path: &Path) -> impl Fn(std::io::Error) -> DownloadError + '_ {
    move |e| {
        DownloadError::new(
            ErrorKind::FileAccessDenied,
            format!("{}: {e}", path.display()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dl_model::PostProcessAlgo;

    /// 官方样本（Evil0ctal/WeChat-Channels-Video-File-Decryption）的密钥流
    /// 前 16 字节：明文头 00 00 00 20 66 74 79 70(ftyp) 与密文
    /// 23 76 6a 16 ff 8f fe 1a 异或所得，decode_key = 2136343393。
    const KS_FIRST16: [u8; 16] = [
        0x33, 0xf9, 0x41, 0xfe, 0xe3, 0x7b, 0x71, 0x77, 0x48, 0xc3, 0x79, 0x6a, 0x62, 0xb0,
        0x42, 0xec,
    ];

    #[test]
    fn isaac64_keystream_matches_wechat_wasm() {
        let ks = isaac64_keystream_reversed(2136343393, ENCRYPTED_PREFIX);
        assert_eq!(&ks[..16], &KS_FIRST16);
        // 密钥流尾部是头部的逆序（reverse 自洽性）
        assert_eq!(&ks[ENCRYPTED_PREFIX - 8..], &[0x6a, 0x87, 0xfb, 0x99, 0x36, 0x6a, 0x76, 0x23]);
    }

    #[test]
    fn decrypt_roundtrip_on_synthetic_mp4() {
        // 合成"加密"文件：明文 = ftyp 头 + 填充，密文 = 明文 ^ 密钥流
        let ks = isaac64_keystream_reversed(2136343393, ENCRYPTED_PREFIX);
        let mut plain = vec![0u8; 500_000];
        plain[..4].copy_from_slice(&[0x00, 0x00, 0x00, 0x20]);
        plain[4..8].copy_from_slice(b"ftyp");
        let mut cipher = plain.clone();
        for i in 0..ENCRYPTED_PREFIX {
            cipher[i] ^= ks[i];
        }
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("v.mp4");
        std::fs::write(&p, &cipher).unwrap();

        let steps = vec![PostProcess {
            algo: PostProcessAlgo::Isaac64Xor,
            seed: "2136343393".into(),
        }];
        apply_all(&p, &steps).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), plain);
        // .dec 临时文件不残留
        assert!(!dir.path().join("v.dec").exists());
    }

    #[test]
    fn bad_seed_fails_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("v.mp4");
        std::fs::write(&p, b"12345678").unwrap();
        let steps = vec![PostProcess {
            algo: PostProcessAlgo::Isaac64Xor,
            seed: "not-a-number".into(),
        }];
        let err = apply_all(&p, &steps).unwrap_err();
        assert_eq!(err.kind.code(), "decrypt_failed");
    }
}
