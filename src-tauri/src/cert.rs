//! CurrentUser Root 证书安装/卸载/状态（Windows Crypt32，免管理员；mkcert 同款）。
//!
//! 仅当用户在设置页点击「安装证书」时写入；作用域仅当前用户；卸载 = 从
//! Root 删除 + （可选）删除 data_dir/sniff 目录即完全撤销。API 删除失败时
//! 回退 `certutil -user -delstore`。

#![cfg(windows)]

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

fn cert_cn() -> String {
    dl_sniff::ca::CA_CN.to_string()
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 幂等：确保 CA 已生成（存在即加载校验），返回 ca-cert.pem 路径。
pub fn ensure_ca(data_dir: &Path) -> Result<PathBuf> {
    let dir = data_dir.join("sniff");
    dl_sniff::ca::SniffCa::load_or_create(&dir)?;
    Ok(dir.join("ca-cert.pem"))
}

fn pem_to_der(pem: &[u8]) -> Result<Vec<u8>> {
    dl_sniff::ca::pem_cert_der(pem).context("decode ca pem")
}

/// 安装到 CurrentUser Root。返回是否新装（已存在也返回 true）。
pub fn install(ca_pem: &Path) -> Result<()> {
    let der = pem_to_der(&std::fs::read(ca_pem).context("read ca pem")?)?;
    unsafe {
        let store = windows_sys::Win32::Security::Cryptography::CertOpenSystemStoreW(
            0,
            wide("Root").as_ptr(),
        );
        if store.is_null() {
            bail!("CertOpenSystemStore Root 失败");
        }
        let ok = windows_sys::Win32::Security::Cryptography::CertAddEncodedCertificateToStore(
            store,
            windows_sys::Win32::Security::Cryptography::X509_ASN_ENCODING
                | windows_sys::Win32::Security::Cryptography::PKCS_7_ASN_ENCODING,
            der.as_ptr(),
            der.len() as u32,
            windows_sys::Win32::Security::Cryptography::CERT_STORE_ADD_ALWAYS,
            std::ptr::null_mut(),
        );
        windows_sys::Win32::Security::Cryptography::CertCloseStore(store, 0);
        if ok == 0 {
            bail!("CertAddEncodedCertificateToStore 失败（GetLastError 可查原因）");
        }
    }
    tracing::info!(?ca_pem, "sniff CA 已安装到 CurrentUser Root");
    Ok(())
}

type CertCtx = *mut windows_sys::Win32::Security::Cryptography::CERT_CONTEXT;

fn find_ca() -> Option<CertCtx> {
    #[allow(non_upper_case_globals)]
    const CERT_FIND_SUBJECT_STR_A: u32 = 0x0008_0007;
    let mut cn = cert_cn().into_bytes();
    cn.push(0); // CERT_FIND_SUBJECT_STR_A 要求 NUL 结尾 ANSI 串
    unsafe {
        let store = windows_sys::Win32::Security::Cryptography::CertOpenSystemStoreW(
            0,
            wide("Root").as_ptr(),
        );
        if store.is_null() {
            return None;
        }
        let ctx = windows_sys::Win32::Security::Cryptography::CertFindCertificateInStore(
            store,
            windows_sys::Win32::Security::Cryptography::X509_ASN_ENCODING
                | windows_sys::Win32::Security::Cryptography::PKCS_7_ASN_ENCODING,
            0,
            CERT_FIND_SUBJECT_STR_A,
            cn.as_ptr() as *const std::ffi::c_void,
            std::ptr::null_mut(),
        );
        windows_sys::Win32::Security::Cryptography::CertCloseStore(store, 0);
        if ctx.is_null() {
            None
        } else {
            Some(ctx)
        }
    }
}

pub fn status() -> bool {
    match find_ca() {
        Some(ctx) => {
            unsafe {
                windows_sys::Win32::Security::Cryptography::CertFreeCertificateContext(ctx);
            }
            true
        }
        None => false,
    }
}

/// 从 Root 卸载。API 失败回退 certutil。
pub fn uninstall() -> Result<()> {
    match find_ca() {
        Some(ctx) => unsafe {
            let r = windows_sys::Win32::Security::Cryptography::CertDeleteCertificateFromStore(ctx);
            if r == 0 {
                return certutil_uninstall();
            }
        },
        None => return Ok(()), // 本就不在
    }
    tracing::info!("sniff CA 已从 Root 卸载");
    Ok(())
}

fn certutil_uninstall() -> Result<()> {
    use std::os::windows::process::CommandExt;
    let _ = 0;
    let out = std::process::Command::new("certutil")
        .args(["-user", "-delstore", "Root", cert_cn().as_str()])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output()
        .context("certutil delstore")?;
    if !out.status.success() {
        bail!(
            "certutil delstore 失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pem_der_roundtrip() {
        // 用 dl-sniff 生成一个 CA 并做 PEM→DER 解码往返
        let dir = std::env::temp_dir().join("adl-cert-test");
        let ca = dl_sniff::ca::SniffCa::load_or_create(&dir).unwrap();
        let pem = ca.cert_pem();
        let der = pem_to_der(pem.as_bytes()).unwrap();
        assert!(der.len() > 100);
        assert_eq!(&der[..2], &[0x30, 0x82], "DER SEQUENCE 头");
    }
}
