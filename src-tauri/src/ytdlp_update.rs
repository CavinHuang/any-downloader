//! yt-dlp 自动更新：媒体站点改版频繁，过期的 yt-dlp 是适配失效的第一原因。
//! 启动后延迟首查 + 每 24h 轮询；版本探测走 releases/latest/download 的
//! 302 Location（GitHub API 匿名限额极易 403，绕开），发现新版下载到
//! 数据目录托管副本（`managed/`，可写、跨应用版本存活，解析链优先于
//! 安装器内置副本），校验 `--version` 与 tag 一致后原子落盘。
//! 开关：设置 `ytdlp_auto_update`（缺省开）。代理沿用设置 `proxy`。

use std::time::Duration;

use dl_engine::Engine;
use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;

pub fn spawn(handle: AppHandle, engine: Engine) {
    tauri::async_runtime::spawn(async move {
        // 延迟首查：避开启动瞬间的磁盘/网络争用
        tokio::time::sleep(Duration::from_secs(45)).await;
        loop {
            if let Err(e) = tick(&handle, &engine).await {
                tracing::warn!(error = %e, "yt-dlp auto-update tick failed");
            }
            tokio::time::sleep(Duration::from_secs(24 * 3600)).await;
        }
    });
}

async fn tick(handle: &AppHandle, engine: &Engine) -> Result<(), String> {
    let off = engine
        .get_setting("ytdlp_auto_update")
        .await
        .map_err(|e| e.to_string())?
        .is_some_and(|v| v == "off");
    if off {
        return Ok(());
    }
    let proxy = engine
        .get_setting("proxy")
        .await
        .map_err(|e| e.to_string())?
        .filter(|s| !s.trim().is_empty());

    let local = local_version(engine).await;
    let Some((tag, url)) = fetch_latest(proxy.as_deref()).await? else {
        return Err("release payload missing asset".into());
    };
    if local.as_deref() == Some(tag.as_str()) {
        tracing::debug!(%tag, "yt-dlp up to date");
        return Ok(());
    }
    // 日期格式 tag（YYYY.MM.DD[.N]）：仅向新走，避免源回滚时本地反被降级
    if let (Some(l), Some(r)) = (parse_date(&tag), local.as_deref().and_then(parse_date)) {
        if r >= l {
            return Ok(());
        }
    }
    tracing::info!(%tag, ?local, "updating yt-dlp");
    download_verified(engine, &tag, &url).await?;
    tracing::info!(%tag, "yt-dlp updated");
    let _ = handle
        .notification()
        .builder()
        .title("yt-dlp 已自动更新")
        .body(format!("站点适配内核升级至 {tag}，媒体下载能力保持最新。"))
        .show();
    Ok(())
}

// ---- 版本探测：与 ytdlp_task 的解析链保持同序（设置 > 托管 > 内置 > PATH）----

async fn local_version(engine: &Engine) -> Option<String> {
    let setting = engine.get_setting("yt_dlp_path").await.ok().flatten();
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|x| x.to_path_buf()));
    let managed = dl_engine::managed_bin_dir().join(dl_engine::managed_ytdlp_name());
    let bundled = exe_dir.as_ref().map(|d| {
        d.join(if cfg!(windows) {
            "yt-dlp.exe"
        } else {
            "yt-dlp"
        })
    });
    let candidates: Vec<String> = [
        setting.filter(|s| !s.trim().is_empty()),
        managed.exists().then(|| managed.display().to_string()),
        bundled
            .filter(|p| p.exists())
            .map(|p| p.display().to_string()),
        Some("yt-dlp".to_string()),
    ]
    .into_iter()
    .flatten()
    .collect();
    for exe in candidates {
        if let Some(v) = run_version(&exe).await {
            return Some(v);
        }
    }
    None
}

async fn run_version(exe: &str) -> Option<String> {
    let out = tokio::process::Command::new(exe)
        .arg("--version")
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

// ---- 最新版本探测：不走 GitHub API（匿名 60 次/时/IP 极易限流），改用
// releases/latest/download 的 302 Location 头解析 tag，全程零限额 ----

const LATEST_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download";

async fn fetch_latest(proxy: Option<&str>) -> Result<Option<(String, String)>, String> {
    let tag = latest_tag(proxy).await?;
    let Some(tag) = tag else {
        return Ok(None);
    };
    let url = format!("{LATEST_URL}/{}", platform_asset());
    // 版本化 URL 等价于 latest 资产；tag 已知，直接构造
    let url = url.replace("/latest/download/", &format!("/download/{tag}/"));
    Ok(Some((tag, url)))
}

/// 从 `releases/latest/download/<asset>` 的 302 Location 提取最新 tag。
async fn latest_tag(proxy: Option<&str>) -> Result<Option<String>, String> {
    let mut build = reqwest::Client::builder()
        .user_agent(concat!("AnythingDownloader/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20));
    if let Some(p) = proxy {
        let proxy = reqwest::Proxy::all(p).map_err(|e| e.to_string())?;
        build = build.proxy(proxy);
    }
    let client = build.build().map_err(|e| e.to_string())?;
    let resp = client
        .get(format!("{LATEST_URL}/{}", platform_asset()))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    // 302 → Location: .../releases/download/<tag>/<asset>
    let loc = resp
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| format!("expected redirect, got status {}", resp.status()))?;
    let tag = loc
        .split("/download/")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .map(str::to_string);
    Ok(tag)
}

fn platform_asset() -> &'static str {
    if cfg!(target_os = "windows") {
        "yt-dlp.exe"
    } else if cfg!(target_os = "macos") {
        "yt-dlp_macos"
    } else if cfg!(target_arch = "aarch64") {
        "yt-dlp_linux_aarch64"
    } else {
        "yt-dlp_linux"
    }
}

async fn download_verified(
    engine: &Engine,
    tag: &str,
    url: &str,
) -> Result<std::path::PathBuf, String> {
    let proxy = engine
        .get_setting("proxy")
        .await
        .map_err(|e| e.to_string())?
        .filter(|s| !s.trim().is_empty());
    let client = dl_net::client::build_client(proxy.as_deref()).map_err(|e| e.to_string())?;
    let dir = dl_engine::managed_bin_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| e.to_string())?;
    let name = dl_engine::managed_ytdlp_name();
    let part = dir.join(format!("{name}.part"));
    let final_path = dir.join(name);

    let mut resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    let mut file = tokio::io::BufWriter::new(
        tokio::fs::File::create(&part)
            .await
            .map_err(|e| e.to_string())?,
    );
    use tokio::io::AsyncWriteExt;
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
        file.write_all(&chunk).await.map_err(|e| e.to_string())?;
    }
    file.flush().await.map_err(|e| e.to_string())?;
    drop(file);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }

    // 校验：能执行且版本与 tag 一致才落盘（防半截包/平台错包）
    let got = run_version(part.to_str().ok_or("non-utf8 path")?)
        .await
        .ok_or("downloaded yt-dlp failed to run")?;
    if got != tag {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(format!("version mismatch: got {got}, want {tag}"));
    }
    tokio::fs::rename(&part, &final_path)
        .await
        .map_err(|e| e.to_string())?;
    Ok(final_path)
}

/// "2026.08.19" / "2026.08.19.2" → (2026, 8, 19)
fn parse_date(tag: &str) -> Option<(u32, u32, u32)> {
    let mut it = tag.trim().split('.');
    let y: u32 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.parse().ok()?;
    Some((y, m, d))
}
