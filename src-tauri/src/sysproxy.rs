//! 系统 PAC 接线（仅 Windows）：把视频号相关域路由到本地嗅探代理。
//!
//! 采用 AutoConfigURL（PAC 文件）而非全局 ProxyServer——reqwest 等本机程序
//! 不读 PAC，天然无回环；未命中的域名 PAC 直接返回 DIRECT，不经过本代理。
//! 启用前备份原 AutoConfigURL；崩溃恢复靠 flag 文件（sniff.rs 启动时还原）。

#![cfg(windows)]

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};

const INET_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PacBackup {
    pub autoconfig: Option<String>,
}

/// 生成 PAC：仅视频号域走本地代理，其余一律 DIRECT。
pub fn write_pac(path: &Path, port: u16) -> Result<()> {
    let pac = format!(
        r#"// Anything Downloader 桌面嗅探：仅视频号相关域走本地代理，其余直连
function FindProxyForURL(url, host) {{
  if (dnsDomainIs(host, "video.qq.com") ||
      dnsDomainIs(host, "channels.weixin.qq.com") ||
      dnsDomainIs(host, "finder.weixin.qq.com")) {{
    return "PROXY 127.0.0.1:{port}";
  }}
  return "DIRECT";
}}
"#
    );
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    std::fs::write(path, pac).context("write pac")?;
    Ok(())
}

/// 本地路径 → file:/// URL（路径按字节百分号编码，兼容含空格/中文的路径）。
pub fn file_url(p: &Path) -> String {
    let mut s = String::from("file:///");
    for b in p.to_string_lossy().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                s.push(b as char)
            }
            b'\\' | b'/' => s.push('/'),
            b':' => s.push(':'),
            other => s.push_str(&format!("%{other:02X}")),
        }
    }
    s
}

/// 写入 AutoConfigURL 指向 PAC；返回原值备份。
pub fn enable(pac_path: &Path) -> Result<PacBackup> {
    let hkcu = winreg::RegKey::predef(HKEY_CURRENT_USER);
    let key = hkcu
        .open_subkey_with_flags(INET_KEY, KEY_READ | KEY_WRITE)
        .context("open Internet Settings")?;
    let prev: Option<String> = key.get_value("AutoConfigURL").ok();
    key.set_value("AutoConfigURL", &file_url(pac_path))
        .context("set AutoConfigURL")?;
    refresh();
    Ok(PacBackup { autoconfig: prev })
}

/// 还原备份（原值为空则删除值）。
pub fn disable(backup: &PacBackup) -> Result<()> {
    let hkcu = winreg::RegKey::predef(HKEY_CURRENT_USER);
    let key = hkcu
        .open_subkey_with_flags(INET_KEY, KEY_READ | KEY_WRITE)
        .context("open Internet Settings")?;
    match &backup.autoconfig {
        Some(v) => key
            .set_value("AutoConfigURL", v)
            .context("restore AutoConfigURL")?,
        None => {
            let _ = key.delete_value("AutoConfigURL");
        }
    }
    refresh();
    Ok(())
}

/// 通知 WinINET/Chromium 系代理设置已变（失败不影响注册表已写，仅提示需重开应用）。
fn refresh() {
    #[allow(non_upper_case_globals)]
    const INTERNET_OPTION_SETTINGS_CHANGED: u32 = 39;
    #[allow(non_upper_case_globals)]
    const INTERNET_OPTION_REFRESH: u32 = 37;
    unsafe {
        windows_sys::Win32::Networking::WinInet::InternetSetOptionW(
            std::ptr::null(),
            INTERNET_OPTION_SETTINGS_CHANGED,
            std::ptr::null(),
            0,
        );
        windows_sys::Win32::Networking::WinInet::InternetSetOptionW(
            std::ptr::null(),
            INTERNET_OPTION_REFRESH,
            std::ptr::null(),
            0,
        );
    }
}

/// 当前生效的 AutoConfigURL（诊断用）。
#[allow(dead_code)]
pub fn current_autoconfig() -> Option<String> {
    let hkcu = winreg::RegKey::predef(HKEY_CURRENT_USER);
    let key = hkcu.open_subkey_with_flags(INET_KEY, KEY_READ).ok()?;
    key.get_value("AutoConfigURL").ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pac_file_url_shape() {
        let p = Path::new(r"C:\Users\A B\Anything\adl-sniff.pac");
        let u = file_url(p);
        assert_eq!(u, "file:///C:/Users/A%20B/Anything/adl-sniff.pac");
    }

    #[test]
    fn pac_content_routes_only_finder() {
        let dir = std::env::temp_dir().join("adl-pac-test");
        let p = dir.join("t.pac");
        write_pac(&p, 9463).unwrap();
        let s = std::fs::read_to_string(&p).unwrap();
        assert!(s.contains("PROXY 127.0.0.1:9463"));
        assert!(s.contains(r#"dnsDomainIs(host, "video.qq.com")"#));
        assert!(s.contains(r#"dnsDomainIs(host, "channels.weixin.qq.com")"#));
    }

    #[test]
    fn backup_roundtrip_json() {
        let b = PacBackup {
            autoconfig: Some("x".into()),
        };
        let j = serde_json::to_string(&b).unwrap();
        assert_eq!(
            serde_json::from_str::<PacBackup>(&j).unwrap().autoconfig,
            Some("x".into())
        );
        let e = PacBackup::default();
        assert_eq!(serde_json::to_string(&e).unwrap(), "{\"autoconfig\":null}");
    }
}
