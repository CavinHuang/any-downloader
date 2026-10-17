//! 浏览器扩展 NM（Native Messaging）通道注册。
//!
//! 桌面端每次启动把通道指向「当前 exe 同目录的 dl-server」：安装器布局、
//! 便携目录、`cargo build --release` 产物目录都天然满足该布局，开发与生产
//! 共用一套逻辑（`scripts/register-nm-host.ps1` 仅保留给取消注册用）。
//!
//! 平台语义（依 Chromium 源码 launch_context_posix.cc / chrome_paths.cc）：
//! - Windows：manifest 放数据目录 nm/ 下，HKCU 注册表（Chrome/Edge）默认值
//!   指向 manifest 路径；
//! - macOS / Linux：清单直接写在各浏览器的 NativeMessagingHosts 目录——
//!   非 Windows 平台该路径文件即清单本体，无指针间接层，path 必须绝对路径。
//!
//! 全程尽力而为：失败只记日志，绝不阻塞启动。

use serde_json::json;

const NM_HOST: &str = "anything_dl";
/// 由 extension/manifest.json 的 key 固定，NM allowed_origins 依赖它。
const EXTENSION_ID: &str = "jeigecniniligfiigmmanjeiaooiiomc";

/// 注册 / 刷新 NM 通道。找不到相邻 dl-server 时跳过并返回 false。
pub fn ensure_registered() -> bool {
    let Some(server) = sidecar_server() else {
        tracing::info!("nm: dl-server not found next to executable, skip register");
        return false;
    };
    // NM manifest（Chrome 每次连接时重新解析，改后无需重启浏览器）
    let body = json!({
        "name": NM_HOST,
        "description": "Anything Downloader native messaging host",
        "path": server,
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{EXTENSION_ID}/")],
    })
    .to_string();

    #[cfg(windows)]
    return register_windows(&body);
    #[cfg(not(windows))]
    return register_posix(&body);
}

/// 当前 exe 同目录的 dl-server（dl-app 与 dl-server 始终同目录分发）。
fn sidecar_server() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let name = if cfg!(windows) {
        "dl-server.exe"
    } else {
        "dl-server"
    };
    // absolute（而非 canonicalize）：Windows 上避免 \\?\ 前缀进 manifest
    let p = std::path::absolute(dir.join(name)).ok()?;
    p.is_file().then_some(p)
}

#[cfg(windows)]
fn register_windows(manifest_body: &str) -> bool {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let dir = dl_engine::data_dir().join("nm");
    let manifest_path = dir.join(format!("{NM_HOST}.json"));
    if let Err(e) =
        std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&manifest_path, manifest_body))
    {
        tracing::warn!(error = %e, "nm: write manifest failed");
        return false;
    }
    let path_str = manifest_path.display().to_string();
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let mut ok = false;
    for sub in [
        format!(r"Software\Google\Chrome\NativeMessagingHosts\{NM_HOST}"),
        format!(r"Software\Microsoft\Edge\NativeMessagingHosts\{NM_HOST}"),
    ] {
        match hkcu
            .create_subkey(&sub)
            .and_then(|(k, _)| k.set_value("", &path_str))
        {
            Ok(()) => {
                tracing::info!(key = %sub, "nm: registered");
                ok = true;
            }
            Err(e) => tracing::warn!(key = %sub, error = %e, "nm: register failed"),
        }
    }
    ok
}

#[cfg(not(windows))]
fn register_posix(manifest_body: &str) -> bool {
    let mut ok = false;
    for dir in browser_nm_dirs() {
        let target = dir.join(format!("{NM_HOST}.json"));
        match std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&target, manifest_body)) {
            Ok(()) => {
                tracing::info!(manifest = %target.display(), "nm: registered");
                ok = true;
            }
            Err(e) => {
                tracing::warn!(manifest = %target.display(), error = %e, "nm: register failed")
            }
        }
    }
    ok
}

/// 各浏览器的用户级 NativeMessagingHosts 目录（写入即清单本体）。
#[cfg(not(windows))]
fn browser_nm_dirs() -> Vec<std::path::PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    if cfg!(target_os = "macos") {
        let support = home.join("Library/Application Support");
        vec![
            support.join("Google/Chrome/NativeMessagingHosts"),
            support.join("Microsoft Edge/NativeMessagingHosts"),
        ]
    } else {
        let config = dirs::config_dir().unwrap_or_else(|| home.join(".config"));
        vec![
            config.join("google-chrome/NativeMessagingHosts"),
            config.join("microsoft-edge/NativeMessagingHosts"),
            config.join("chromium/NativeMessagingHosts"),
        ]
    }
}
