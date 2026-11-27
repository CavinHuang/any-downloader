//! macOS 桌面嗅探系统接线：登录钥匙串根证书管理 + networksetup PAC 代理配置。
#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MacPacBackup {
    pub services: Vec<ServicePacBackup>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServicePacBackup {
    pub service: String,
    pub enabled: bool,
    pub url: Option<String>,
}

fn login_keychain() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    home.join("Library/Keychains/login.keychain-db")
}

pub fn ensure_ca(data_dir: &Path) -> Result<PathBuf> {
    let dir = data_dir.join("sniff");
    dl_sniff::ca::SniffCa::load_or_create(&dir)?;
    Ok(dir.join("ca-cert.pem"))
}

pub fn cert_status() -> bool {
    Command::new("security")
        .args(["find-certificate", "-c", dl_sniff::ca::CA_CN])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn cert_install(ca_pem: &Path) -> Result<()> {
    let kc = login_keychain();
    let out = Command::new("security")
        .args([
            "add-trusted-cert",
            "-r",
            "trustRoot",
            "-k",
            &kc.to_string_lossy(),
        ])
        .arg(ca_pem)
        .output()
        .context("run security add-trusted-cert")?;
    if !out.status.success() {
        bail!(
            "安装证书失败（请在系统弹窗授权）: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

pub fn cert_uninstall() -> Result<()> {
    let out = Command::new("security")
        .args(["delete-certificate", "-c", dl_sniff::ca::CA_CN])
        .output()
        .context("run security delete-certificate")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        if !err.contains("could not be found") {
            bail!("卸载证书失败: {}", err.trim());
        }
    }
    Ok(())
}

fn list_network_services() -> Vec<String> {
    let Ok(out) = Command::new("networksetup")
        .arg("-listallnetworkservices")
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .skip(1) // 首行是提示文案
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with('*'))
        .map(|s| s.to_string())
        .collect()
}

fn read_service_pac(service: &str) -> ServicePacBackup {
    let out = Command::new("networksetup")
        .args(["-getautoproxyurl", service])
        .output();
    let mut enabled = false;
    let mut url = None;
    if let Ok(o) = out {
        if o.status.success() {
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                if let Some(v) = line.strip_prefix("URL:") {
                    let v = v.trim();
                    if !v.is_empty() && v != "(null)" {
                        url = Some(v.to_string());
                    }
                } else if let Some(v) = line.strip_prefix("Enabled:") {
                    enabled = v.trim().eq_ignore_ascii_case("Yes");
                }
            }
        }
    }
    ServicePacBackup {
        service: service.to_string(),
        enabled,
        url,
    }
}

pub fn enable_pac(port: u16) -> Result<MacPacBackup> {
    let pac_url = format!("http://127.0.0.1:{port}/proxy.pac");
    let services = list_network_services();
    let mut backups = Vec::new();
    let mut applied = 0usize;
    for svc in services {
        // 仅接管常用桌面联网接口，避免误触虚拟网卡报错
        if !matches!(svc.as_str(), "Wi-Fi" | "Ethernet" | "USB 10/100/1000 LAN")
            && !svc.contains("Ethernet")
            && !svc.contains("Wi-Fi")
        {
            continue;
        }
        let b = read_service_pac(&svc);
        let st = Command::new("networksetup")
            .args(["-setautoproxyurl", &svc, &pac_url])
            .status();
        let on = Command::new("networksetup")
            .args(["-setautoproxystate", &svc, "on"])
            .status();
        if st.is_ok_and(|s| s.success()) && on.is_ok_and(|s| s.success()) {
            backups.push(b);
            applied += 1;
        }
    }
    if applied == 0 {
        bail!("未找到可配置的 macOS 网络接口（Wi-Fi/Ethernet）");
    }
    Ok(MacPacBackup { services: backups })
}

pub fn disable_pac(backup: &MacPacBackup) -> Result<()> {
    for b in &backup.services {
        if let Some(ref url) = b.url {
            let _ = Command::new("networksetup")
                .args(["-setautoproxyurl", &b.service, url])
                .status();
        }
        let state = if b.enabled { "on" } else { "off" };
        let _ = Command::new("networksetup")
            .args(["-setautoproxystate", &b.service, state])
            .status();
    }
    Ok(())
}
