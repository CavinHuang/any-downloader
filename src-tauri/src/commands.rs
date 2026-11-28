//! Tauri 命令层：UI 进程内直调引擎（DESIGN §3 运行时纪律——命令只发调用，
//! 不 block_on；下载驱动跑在 tauri::async_runtime 上）。

use std::collections::HashMap;

use dl_engine::{DownloadOptions, Engine};
use dl_model::Task;
use tauri::{Emitter, State};
use tauri_plugin_updater::UpdaterExt;

use crate::AppState;

/// 备用直调入口（尚未注册为命令；注册时需改收 `State<AppState>`）
#[allow(dead_code)]
pub async fn download_options(engine: &Engine) -> Result<DownloadOptions, String> {
    engine
        .download_options_from_settings()
        .await
        .map_err(str_err)
}

fn str_err(e: dl_error::DownloadError) -> String {
    e.to_string()
}

#[tauri::command]
pub async fn list_tasks(
    state: State<'_, AppState>,
    status: Option<String>,
) -> Result<Vec<Task>, String> {
    let st = status.as_deref().and_then(dl_model::TaskStatus::parse);
    state.engine.list_tasks(st).await.map_err(str_err)
}

#[tauri::command]
pub async fn tasks_updated_since(
    state: State<'_, AppState>,
    since_ms: i64,
) -> Result<Vec<Task>, String> {
    state
        .engine
        .list_updated_since(since_ms)
        .await
        .map_err(str_err)
}

#[tauri::command]
pub async fn get_task(state: State<'_, AppState>, id: String) -> Result<Option<Task>, String> {
    state.engine.get_task(&id).await.map_err(str_err)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn add_task(
    state: State<'_, AppState>,
    url: String,
    dir: Option<String>,
    filename: Option<String>,
    headers: Option<HashMap<String, String>>,
    checksum: Option<String>,
    mirrors: Option<Vec<String>>,
    proxy: Option<String>,
    format: Option<String>,
) -> Result<Task, String> {
    let mut c = dl_model::CreateTask::of(url);
    c.dir = dir.filter(|s| !s.is_empty());
    c.filename = filename.filter(|s| !s.is_empty());
    c.headers = headers.filter(|h| !h.is_empty());
    c.checksum = checksum.filter(|s| !s.is_empty());
    c.mirrors = mirrors.filter(|m| !m.is_empty());
    c.proxy = proxy.filter(|s| !s.trim().is_empty());
    c.format = format.filter(|s| !s.trim().is_empty());
    state.engine.create_task(c).await.map_err(str_err)
}

#[tauri::command]
pub async fn inspect_media_url(
    state: State<'_, AppState>,
    url: String,
) -> Result<dl_ytdlp::InspectResult, String> {
    dl_engine::ytdlp_task::inspect_url(&state.engine, &url)
        .await
        .map_err(str_err)
}

#[tauri::command]
pub async fn inspect_site_url(
    state: State<'_, AppState>,
    url: String,
) -> Result<dl_engine::site_adapt::SiteInspectResult, String> {
    dl_engine::site_adapt::inspect_site_url(&state.engine, &url)
        .await
        .map_err(str_err)
}

#[tauri::command]
pub async fn probe_site_mirrors(
    state: State<'_, AppState>,
) -> Result<Vec<dl_engine::site_adapt::MirrorLatency>, String> {
    Ok(dl_engine::site_adapt::probe_site_mirrors(&state.engine).await)
}

#[tauri::command]
pub async fn set_priority(
    state: State<'_, AppState>,
    id: String,
    priority: i32,
) -> Result<Task, String> {
    state
        .engine
        .set_priority(&id, priority)
        .await
        .map_err(str_err)
}

/// 收藏室全文搜索：文章任务正文命中（返回 id + 文件名，≤50 条）。
#[tauri::command]
pub async fn search_content(
    state: State<'_, AppState>,
    query: String,
) -> Result<Vec<serde_json::Value>, String> {
    let hits = state
        .engine
        .search_content(query.trim())
        .await
        .map_err(str_err)?;
    Ok(hits
        .into_iter()
        .map(|(id, filename)| serde_json::json!({ "id": id, "filename": filename }))
        .collect())
}

/// 删除任务（运行中任务需先取消/暂停）。
#[tauri::command]
pub async fn remove_task(state: State<'_, AppState>, id: String) -> Result<bool, String> {
    state.engine.remove(&id).await.map_err(str_err)
}

#[tauri::command]
pub async fn retry_task(state: State<'_, AppState>, id: String) -> Result<Task, String> {
    state.engine.retry(&id).await.map_err(str_err)
}

#[tauri::command]
pub async fn move_to_queue(state: State<'_, AppState>, id: String) -> Result<Task, String> {
    state.engine.move_to_queue(&id).await.map_err(str_err)
}

/// 启动下载：spawn 驱动任务，进度经 engine-event 推给前端。
/// 失败自动重试：网络类错误按退避重排至多 max_auto_retry 次（设置可调，0 = 关闭）。
#[tauri::command]
pub async fn start_download(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.engine.start(&id).await.map(|_| ()).map_err(str_err)
}

#[tauri::command]
pub async fn rename_task(
    state: State<'_, AppState>,
    id: String,
    filename: String,
) -> Result<Task, String> {
    state
        .engine
        .rename_task(&id, &filename)
        .await
        .map_err(str_err)
}

#[tauri::command]
pub async fn update_task_url(
    state: State<'_, AppState>,
    id: String,
    url: String,
) -> Result<Task, String> {
    state
        .engine
        .update_task_url(&id, &url)
        .await
        .map_err(str_err)
}

#[tauri::command]
pub async fn compute_task_checksum(
    state: State<'_, AppState>,
    id: String,
    algo: String,
) -> Result<String, String> {
    state
        .engine
        .compute_task_checksum(&id, &algo)
        .await
        .map_err(str_err)
}

/// 暂停：运行中走协作取消（checkpoint 后落 Paused）；非运行态直接状态迁移。
#[tauri::command]
pub async fn pause_task(state: State<'_, AppState>, id: String) -> Result<Task, String> {
    if state.engine.request_pause(&id) {
        Ok(state.engine.get_task(&id).await.map_err(str_err)?.unwrap())
    } else {
        state.engine.pause(&id).await.map_err(str_err)
    }
}

#[tauri::command]
pub async fn cancel_task(state: State<'_, AppState>, id: String) -> Result<Task, String> {
    if state.engine.request_cancel(&id) {
        Ok(state.engine.get_task(&id).await.map_err(str_err)?.unwrap())
    } else {
        state.engine.cancel(&id).await.map_err(str_err)
    }
}

#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let engine = &state.engine;
    let mut out = serde_json::Map::new();
    for key in [
        "default_dir",
        "segments",
        "task_bps",
        "global_bps",
        "proxy_mode",
        "proxy",
        "conflict",
        "ffmpeg_path",
        "yt_dlp_path",
        "yt_dlp_subs",
        "yt_dlp_meta",
        "yt_dlp_cookies",
        "max_concurrent",
        "max_auto_retry",
        "auto_speed_up",
        "auto_categorize",
        "after_done",
        "clipboard_monitor",
        "prevent_sleep",
        "auto_resume_on_start",
        "preserve_mtime",
        "yt_dlp_quality",
        "hf_mirror_enabled",
        "hf_mirror_site",
        "hf_token",
        "github_mirror_enabled",
        "github_mirror_site",
        "github_mirror_custom",
        "ytdlp_auto_update",
        "schedule_enabled",
        "schedule_window",
        "schedule_outside",
        "schedule_outside_bps",
        "update_auto",
        "run_command",
    ] {
        let v = engine.get_setting(key).await.map_err(str_err)?;
        out.insert(
            key.to_string(),
            match v {
                Some(v) => serde_json::Value::String(v),
                None => serde_json::Value::Null,
            },
        );
    }
    Ok(serde_json::Value::Object(out))
}

#[tauri::command]
pub async fn set_setting(
    state: State<'_, AppState>,
    key: String,
    value: String,
) -> Result<(), String> {
    state
        .engine
        .set_setting(&key, &value)
        .await
        .map_err(str_err)
}

#[tauri::command]
pub fn default_download_dir() -> String {
    default_download_dir_impl()
}

/// URL 探测（新建任务对话框的「识别」按钮，含中转页自动穿透/拦截提示）。
#[tauri::command]
pub async fn probe_url(
    state: State<'_, AppState>,
    url: String,
) -> Result<serde_json::Value, String> {
    let proxy = state.engine.effective_proxy().await.ok().flatten();
    let client = dl_net::build_client(proxy.as_deref()).map_err(str_err)?;
    match dl_net::probe::probe(&client, &url, &HashMap::new()).await {
        Ok(r) => Ok(serde_json::json!({
            "filename": r.filename,
            "total_bytes": r.total_bytes,
            "mime": r.mime,
            "segmented": r.segmented,
            "final_url": r.final_url,
            "resolved_from_interstitial": r.resolved_from_interstitial,
            "is_interstitial": false,
            "interstitial_title": serde_json::Value::Null,
        })),
        Err(e) => match e.kind {
            dl_error::ErrorKind::InterstitialPage {
                url: final_url,
                title,
            } => Ok(serde_json::json!({
                "filename": serde_json::Value::Null,
                "total_bytes": serde_json::Value::Null,
                "mime": "text/html",
                "segmented": false,
                "final_url": final_url,
                "resolved_from_interstitial": false,
                "is_interstitial": true,
                "interstitial_title": title,
            })),
            _ => Err(str_err(e)),
        },
    }
}

#[tauri::command]
pub async fn queue_status(state: State<'_, AppState>) -> Result<dl_engine::QueueStatus, String> {
    state.engine.queue_status().await.map_err(str_err)
}

#[tauri::command]
pub async fn start_queue(state: State<'_, AppState>) -> Result<usize, String> {
    state.engine.start_queue().await.map_err(str_err)
}

#[tauri::command]
pub async fn pause_queue(state: State<'_, AppState>) -> Result<usize, String> {
    state.engine.pause_queue().await.map_err(str_err)
}

#[tauri::command]
pub async fn reorder_queue(
    state: State<'_, AppState>,
    id: String,
    direction: String,
) -> Result<(), String> {
    state
        .engine
        .reorder_queue(&id, &direction)
        .await
        .map_err(str_err)
}

#[tauri::command]
pub async fn set_max_concurrent(
    state: State<'_, AppState>,
    value: usize,
) -> Result<dl_engine::QueueStatus, String> {
    state
        .engine
        .set_setting("max_concurrent", &value.to_string())
        .await
        .map_err(str_err)?;
    state.engine.queue_status().await.map_err(str_err)
}

#[tauri::command]
pub async fn sniff_page_url(
    state: State<'_, AppState>,
    url: String,
) -> Result<Vec<dl_net::page_sniff::PageSniffItem>, String> {
    let proxy = state.engine.effective_proxy().await.ok().flatten();
    let client = dl_net::build_client(proxy.as_deref()).map_err(str_err)?;
    let mut items = dl_net::page_sniff::sniff_page(&client, &url)
        .await
        .unwrap_or_default();

    let is_ytdlp = reqwest::Url::parse(&url)
        .ok()
        .and_then(|u| u.host_str().map(dl_engine::is_ytdlp_host))
        .unwrap_or(false);
    if is_ytdlp && !items.iter().any(|it| it.url == url) {
        items.insert(
            0,
            dl_net::page_sniff::PageSniffItem {
                url: url.clone(),
                filename: "video".to_string(),
                kind: "ytdlp".to_string(),
                mime: None,
                bytes: None,
                source: "platform".to_string(),
            },
        );
    }
    Ok(items)
}

#[tauri::command]
pub fn open_external_url(url: String) -> Result<(), String> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("only http/https URLs can be opened".into());
    }
    tauri_plugin_opener::open_url(url, None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn reveal_task(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let t = state
        .engine
        .get_task(&id)
        .await
        .map_err(str_err)?
        .ok_or_else(|| "task not found".to_string())?;
    let dir = t.dir.clone().unwrap_or_else(default_download_dir_impl);
    let path = std::path::Path::new(&dir).join(t.filename.as_deref().unwrap_or(""));
    if path.exists() {
        tauri_plugin_opener::reveal_item_in_dir(&path).map_err(|e| e.to_string())
    } else {
        tauri_plugin_opener::open_path(dir, None::<&str>).map_err(|e| e.to_string())
    }
}

/// 导出任务列表（URL/目录/文件名/类型；不含 Cookie 等敏感头，DESIGN §8）。
#[tauri::command]
pub async fn export_tasks(state: State<'_, AppState>, path: String) -> Result<usize, String> {
    let tasks = state.engine.list_tasks(None).await.map_err(str_err)?;
    let out: Vec<serde_json::Value> = tasks
        .iter()
        .map(|t| {
            serde_json::json!({
                "url": t.url,
                "dir": t.dir,
                "filename": t.filename,
                "kind": t.kind,
            })
        })
        .collect();
    let n = out.len();
    let json = serde_json::to_vec_pretty(&out).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(n)
}

/// 从 JSON 导入任务（不自动开始下载）；返回导入条数。
#[tauri::command]
pub async fn import_tasks(state: State<'_, AppState>, path: String) -> Result<usize, String> {
    let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let list: Vec<serde_json::Value> = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    let mut imported = 0;
    for item in list {
        let Some(url) = item.get("url").and_then(|v| v.as_str()) else {
            continue;
        };
        let mut c = dl_model::CreateTask::of(url.to_string());
        c.dir = item.get("dir").and_then(|v| v.as_str()).map(String::from);
        c.filename = item
            .get("filename")
            .and_then(|v| v.as_str())
            .map(String::from);
        c.kind = item
            .get("kind")
            .and_then(|v| v.as_str())
            .and_then(dl_model::TaskKind::parse);
        if state.engine.create_task(c).await.is_ok() {
            imported += 1;
        }
    }
    Ok(imported)
}

/// 打开成品：文件存在则打开文件，否则打开目录。
#[tauri::command]
pub async fn open_task(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let t = state
        .engine
        .get_task(&id)
        .await
        .map_err(str_err)?
        .ok_or_else(|| "task not found".to_string())?;
    let dir = t.dir.clone().unwrap_or_else(default_download_dir_impl);
    let path = std::path::Path::new(&dir).join(t.filename.as_deref().unwrap_or(""));
    if path.exists() {
        tauri_plugin_opener::open_path(path, None::<&str>).map_err(|e| e.to_string())
    } else {
        tauri_plugin_opener::open_path(dir, None::<&str>).map_err(|e| e.to_string())
    }
}

/// 浏览器扩展 NM 通道是否已注册（Chrome/Edge/Brave/Arc/Vivaldi/Chromium 任一即视为已注册）。
#[tauri::command]
pub fn nm_registered() -> bool {
    #[cfg(windows)]
    {
        use winreg::enums::HKEY_CURRENT_USER;
        use winreg::RegKey;
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        for sub in [
            r"Software\Google\Chrome\NativeMessagingHosts\anything_dl",
            r"Software\Microsoft\Edge\NativeMessagingHosts\anything_dl",
            r"Software\BraveSoftware\Brave-Browser\NativeMessagingHosts\anything_dl",
            r"Software\Vivaldi\NativeMessagingHosts\anything_dl",
        ] {
            if hkcu.open_subkey(sub).is_ok() {
                return true;
            }
        }
        false
    }
    #[cfg(not(windows))]
    {
        let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
            return false;
        };
        if cfg!(target_os = "macos") {
            let support = home.join("Library/Application Support");
            [
                "Google/Chrome/NativeMessagingHosts/anything_dl.json",
                "Microsoft Edge/NativeMessagingHosts/anything_dl.json",
                "BraveSoftware/Brave-Browser/NativeMessagingHosts/anything_dl.json",
                "Arc/User Data/NativeMessagingHosts/anything_dl.json",
                "Vivaldi/NativeMessagingHosts/anything_dl.json",
                "Chromium/NativeMessagingHosts/anything_dl.json",
            ]
            .iter()
            .any(|p| support.join(p).exists())
        } else {
            let config = dirs::config_dir().unwrap_or_else(|| home.join(".config"));
            [
                "google-chrome",
                "microsoft-edge",
                "chromium",
                "BraveSoftware/Brave-Browser",
                "vivaldi",
            ]
            .iter()
            .any(|b| {
                config
                    .join(b)
                    .join("NativeMessagingHosts/anything_dl.json")
                    .exists()
            })
        }
    }
}

#[tauri::command]
pub fn open_logs_dir() -> Result<(), String> {
    let dir = dl_engine::data_dir().join("logs");
    let _ = std::fs::create_dir_all(&dir);
    tauri_plugin_opener::open_path(dir, None::<&str>).map_err(|e| e.to_string())
}

/// 随桌面端分发的浏览器扩展安装信息（「安装浏览器扩展」弹窗用）。
/// 查找顺序：资源目录（安装器布局）→ exe 同目录（便携）→ 仓库 extension/
/// （开发布局：target/release → 仓库根）。
#[tauri::command]
pub fn extension_bundle(app: tauri::AppHandle) -> serde_json::Value {
    use tauri::Manager;

    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(dir) = app.path().resource_dir() {
        candidates.push(dir.join("extension"));
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.to_path_buf()))
    {
        candidates.push(dir.join("extension"));
        candidates.push(dir.join("../../extension"));
    }
    for dir in &candidates {
        if dir.join("manifest.json").is_file() {
            let resolved = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            return serde_json::json!({
                "path": display_path(&resolved),
                "present": true
            });
        }
    }
    serde_json::json!({
        "path": candidates.first().map(|d| d.display().to_string()),
        "present": false
    })
}

/// 打开扩展所在目录（弹窗「打开扩展文件夹」）；要求目标确为扩展目录。
#[tauri::command]
pub fn open_extension_dir(path: String) -> Result<(), String> {
    if !std::path::Path::new(&path).join("manifest.json").is_file() {
        return Err("extension manifest.json not found".into());
    }
    tauri_plugin_opener::open_path(path, None::<&str>).map_err(|e| e.to_string())
}

/// 打开扩展安装教程（外部浏览器）。
#[tauri::command]
pub fn open_extension_doc() -> Result<(), String> {
    tauri_plugin_opener::open_url(
        "https://github.com/CavinHuang/any-downloader/blob/main/docs/EXTENSION-INSTALL.md",
        None::<&str>,
    )
    .map_err(|e| e.to_string())
}

/// Windows 上 canonicalize 产生的 `\\?\` 前缀不适合直接展示/复制，剥掉。
fn display_path(p: &std::path::Path) -> String {
    let s = p.display().to_string();
    s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
}

#[tauri::command]
pub async fn get_autostart(app: tauri::AppHandle) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_autostart(app: tauri::AppHandle, enable: bool) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    let al = app.autolaunch();
    if enable {
        al.enable().map_err(|e| e.to_string())
    } else {
        al.disable().map_err(|e| e.to_string())
    }
}

/// 完成后动作执行（设置 after_done；引擎只存键，执行在应用层——需要
/// opener/shell 与「其余任务是否仍在跑」的判断）。
/// shutdown/sleep 有全局效果：仅在列表里没有其他未完成任务时才触发
/// （首个任务完成就关机不是任何人的预期）；Windows 关机给 60s 宽限。
pub async fn run_after_done(
    engine: &dl_engine::Engine,
    action: &str,
    run_cmd: &str,
    file: &str,
    dir: &str,
    done_id: &str,
) {
    use dl_model::TaskStatus;
    if matches!(action, "shutdown" | "sleep") {
        let pending = [
            TaskStatus::Queued,
            TaskStatus::Probing,
            TaskStatus::Downloading,
            TaskStatus::Paused,
            TaskStatus::WaitingRetry,
            TaskStatus::Merging,
            TaskStatus::Verifying,
            TaskStatus::Decrypting,
        ];
        let mut busy = false;
        for st in pending {
            if let Ok(list) = engine.list_tasks(Some(st)).await {
                if list.iter().any(|t| t.id != done_id) {
                    busy = true;
                    break;
                }
            }
        }
        if busy {
            return;
        }
    }
    match action {
        "open_file" => {
            let p = std::path::Path::new(file);
            let target = if p.exists() { file } else { dir };
            let _ = tauri_plugin_opener::open_path(target, None::<&str>);
        }
        "open_folder" => {
            let _ = tauri_plugin_opener::open_path(dir, None::<&str>);
        }
        "run_command" => {
            // {file}/{dir} 占位符；本机用户自配的命令，与 IDM「完成后运行」同级信任
            let cmd = run_cmd.replace("{file}", file).replace("{dir}", dir);
            if cmd.trim().is_empty() {
                return;
            }
            #[cfg(target_os = "windows")]
            {
                use std::os::windows::process::CommandExt;
                let _ = std::process::Command::new("cmd")
                    .args(["/C", &cmd])
                    .creation_flags(0x0800_0000) // CREATE_NO_WINDOW：GUI 进程不闪黑框
                    .spawn();
            }
            #[cfg(not(target_os = "windows"))]
            {
                let _ = std::process::Command::new("sh").args(["-c", &cmd]).spawn();
            }
        }
        "shutdown" => {
            #[cfg(target_os = "windows")]
            {
                let _ = std::process::Command::new("shutdown")
                    .args(["/s", "/t", "60"])
                    .spawn();
            }
            #[cfg(target_os = "macos")]
            {
                let _ = std::process::Command::new("osascript")
                    .args(["-e", "tell app \"System Events\" to shut down"])
                    .spawn();
            }
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                let _ = std::process::Command::new("shutdown")
                    .args(["-h", "+1"])
                    .spawn();
            }
        }
        "sleep" => {
            #[cfg(target_os = "windows")]
            {
                let _ = std::process::Command::new("rundll32.exe")
                    .args(["powrprof.dll,SetSuspendState", "0,1,0"])
                    .spawn();
            }
            #[cfg(target_os = "macos")]
            {
                let _ = std::process::Command::new("pmset").arg("sleepnow").spawn();
            }
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                let _ = std::process::Command::new("systemctl")
                    .arg("suspend")
                    .spawn();
            }
        }
        "extract_zip" => {
            let p = std::path::PathBuf::from(file);
            if p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
                && p.is_file()
            {
                let dest_parent = std::path::PathBuf::from(dir);
                match tokio::task::spawn_blocking(move || extract_zip_safely(&p, &dest_parent))
                    .await
                {
                    Ok(Ok(out)) => {
                        tracing::info!(dir = %out.display(), "auto-extracted zip archive")
                    }
                    Ok(Err(e)) => tracing::warn!(error = %e, "auto-extract zip failed"),
                    Err(e) => tracing::warn!(error = %e, "auto-extract zip task panicked"),
                }
            }
        }
        _ => {}
    }
}

pub fn default_download_dir_impl() -> String {
    dirs::download_dir()
        .map(|d| d.display().to_string())
        .unwrap_or_else(|| dl_engine::data_dir().display().to_string())
}

// ---- 远程添加（手机扫码推链接到桌面端）----

/// 远程服务运行态（None = 未启动）。独立可变状态，与引擎解耦。
#[derive(Default)]
pub struct RemoteState {
    pub server: std::sync::Mutex<Option<dl_remote::RemoteServer>>,
}

/// 配对令牌：时间纳秒 ⊕ 进程 id 折叠成 8 位 hex，仅局域网配对用。
fn gen_token() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() ^ (d.subsec_nanos() as u64).rotate_left(20))
        .unwrap_or(0)
        ^ (std::process::id() as u64).rotate_left(13);
    format!("{:08x}", n & 0xffff_ffff)
}

async fn remote_conf(engine: &Engine) -> (u16, String) {
    let port = engine
        .get_setting("remote_port")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(dl_remote::DEFAULT_PORT);
    let token = match engine
        .get_setting("remote_token")
        .await
        .ok()
        .flatten()
        .filter(|s| s.len() >= 6)
    {
        Some(t) => t,
        None => {
            let t = gen_token();
            let _ = engine.set_setting("remote_token", &t).await;
            t
        }
    };
    (port, token)
}

#[tauri::command]
pub async fn remote_status(
    state: State<'_, RemoteState>,
    engine: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let enabled = state.server.lock().unwrap().is_some();
    let (port, token) = remote_conf(&engine.engine).await;
    let urls = dl_remote::lan_urls(port, &token);
    let qr = urls.first().and_then(|u| dl_remote::qr_svg(u));
    Ok(serde_json::json!({
        "enabled": enabled,
        "port": port,
        "urls": urls,
        "qr": qr,
    }))
}

#[tauri::command]
pub async fn set_remote_enabled(
    on: bool,
    state: State<'_, RemoteState>,
    engine: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    if on {
        let already = {
            let guard = state.server.lock().unwrap();
            guard.is_some()
        };
        if already {
            return remote_status(state, engine).await;
        }
        let (port, token) = remote_conf(&engine.engine).await;
        // 先启动后加锁（await 不持 std 锁）；竞态多启的实例随 drop 关闭
        let server = dl_remote::RemoteServer::start(engine.engine.clone(), port, token)
            .await
            .map_err(|e| format!("启动失败（端口 {port} 可能被占用）：{e}"))?;
        {
            let mut guard = state.server.lock().unwrap();
            if guard.is_none() {
                *guard = Some(server);
            }
        }
    } else if let Some(s) = state.server.lock().unwrap().take() {
        s.stop();
    }
    let _ = engine
        .engine
        .set_setting("remote_enabled", if on { "on" } else { "off" })
        .await;
    remote_status(state, engine).await
}

/// 开机即恢复上次的远程开关（setup 同步上下文里调用）。
pub fn remote_autostart(engine: &Engine, state: &RemoteState) {
    let on = tauri::async_runtime::block_on(async {
        engine
            .get_setting("remote_enabled")
            .await
            .ok()
            .flatten()
            .is_some_and(|v| v == "on")
    });
    if !on {
        return;
    }
    let (port, token) = tauri::async_runtime::block_on(remote_conf(engine));
    match tauri::async_runtime::block_on(dl_remote::RemoteServer::start(
        engine.clone(),
        port,
        token,
    )) {
        Ok(server) => {
            *state.server.lock().unwrap() = Some(server);
            tracing::info!(port, "remote add server started");
        }
        Err(e) => tracing::warn!(error = %e, "remote server autostart failed"),
    }
}

/// 浏览器扩展本地 HTTP 桥状态（与 Native Messaging 双通道并行）。
#[derive(Default)]
pub struct BridgeHandle {
    pub state: std::sync::Mutex<Option<std::sync::Arc<dl_ipc::http_bridge::BridgeState>>>,
}

#[tauri::command]
pub async fn extension_bridge_status(
    bridge: State<'_, BridgeHandle>,
) -> Result<serde_json::Value, String> {
    let st = bridge.state.lock().unwrap().clone();
    let nm = nm_registered();
    if let Some(st) = st {
        let has_token = st.has_token().await;
        Ok(serde_json::json!({
            "running": true,
            "port": st.port,
            "hasToken": has_token,
            "nmRegistered": nm,
            "lastSeenAt": st.last_seen_at(),
            "items": st.items(),
        }))
    } else {
        Ok(serde_json::json!({
            "running": false,
            "port": dl_ipc::http_bridge::DEFAULT_BRIDGE_PORT,
            "hasToken": false,
            "nmRegistered": nm,
            "lastSeenAt": null,
            "items": [],
        }))
    }
}

#[tauri::command]
pub async fn extension_bridge_clear(bridge: State<'_, BridgeHandle>) -> Result<(), String> {
    let st = bridge.state.lock().unwrap().clone();
    if let Some(st) = st {
        st.clear();
    }
    Ok(())
}

#[tauri::command]
pub async fn extension_bridge_download(
    bridge: State<'_, BridgeHandle>,
    app_state: State<'_, AppState>,
    id: String,
    variant_url: Option<String>,
) -> Result<Task, String> {
    let st = bridge
        .state
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| "extension bridge is not running".to_string())?;
    let item = st
        .get_item(&id)
        .ok_or_else(|| format!("sniffed item {id} not found"))?;
    let url = variant_url
        .filter(|u| !u.trim().is_empty())
        .unwrap_or(item.url);
    let mut create = dl_model::CreateTask::of(url);
    create.filename = item.filename.filter(|s| !s.trim().is_empty());
    if !item.headers.is_empty() {
        create.headers = Some(item.headers);
    }
    let task = app_state
        .engine
        .create_task(create)
        .await
        .map_err(str_err)?;
    let _ = app_state.engine.start(&task.id).await;
    Ok(task)
}

fn unique_extract_dir(parent: &std::path::Path, zip_path: &std::path::Path) -> std::path::PathBuf {
    let stem = zip_path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("extracted");
    let first = parent.join(stem);
    if !first.exists() {
        return first;
    }
    for i in 2..1000 {
        let cand = parent.join(format!("{stem} ({i})"));
        if !cand.exists() {
            return cand;
        }
    }
    parent.join(format!("{stem}-{}", dl_model::now_ms()))
}

pub(crate) fn extract_zip_safely(
    zip_path: &std::path::Path,
    dest_parent: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    let file = std::fs::File::open(zip_path).map_err(|e| format!("open zip: {e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("read zip: {e}"))?;
    let out_dir = unique_extract_dir(dest_parent, zip_path);
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("create extract dir: {e}"))?;

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("zip entry {i}: {e}"))?;
        if entry.is_symlink() {
            continue;
        }
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        let target = out_dir.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&target)
                .map_err(|e| format!("mkdir {}: {e}", target.display()))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        let mut out_file = std::fs::File::create(&target)
            .map_err(|e| format!("create {}: {e}", target.display()))?;
        std::io::copy(&mut entry, &mut out_file)
            .map_err(|e| format!("write {}: {e}", target.display()))?;
    }
    Ok(out_dir)
}

#[derive(Default)]
pub struct AppUpdateState(pub tokio::sync::Mutex<Option<tauri_plugin_updater::Update>>);

#[tauri::command]
pub async fn check_app_update(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    upd_state: State<'_, AppUpdateState>,
) -> Result<Option<serde_json::Value>, String> {
    let proxy = state.engine.effective_proxy().await.map_err(str_err)?;
    let mut builder = app.updater_builder();
    if dl_net::is_direct_proxy(proxy.as_deref()) {
        builder = builder.no_proxy();
    } else if let Some(p) = proxy {
        let url = tauri::Url::parse(&p).map_err(|e| format!("invalid proxy url: {e}"))?;
        builder = builder.proxy(url);
    }
    let updater = builder.build().map_err(|e| e.to_string())?;
    let update = updater.check().await.map_err(|e| e.to_string())?;
    let info = update
        .as_ref()
        .map(|u| serde_json::json!({ "version": u.version }));
    *upd_state.0.lock().await = update;
    Ok(info)
}

#[tauri::command]
pub async fn install_app_update(
    app: tauri::AppHandle,
    upd_state: State<'_, AppUpdateState>,
) -> Result<(), String> {
    let update = upd_state
        .0
        .lock()
        .await
        .take()
        .ok_or_else(|| "no pending update".to_string())?;
    let mut downloaded: u64 = 0;
    update
        .download_and_install(
            |chunk_len, content_len| {
                downloaded += chunk_len as u64;
                let _ = app.emit(
                    "app-update-progress",
                    serde_json::json!({ "downloaded": downloaded, "total": content_len }),
                );
            },
            || {},
        )
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_system_proxy() -> Option<String> {
    dl_net::detect_system_proxy()
}

#[derive(Default)]
pub struct SpeedTestState(pub tokio::sync::Mutex<Option<tokio_util::sync::CancellationToken>>);

#[tauri::command]
pub async fn detect_network_ip(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let raw_mode = state
        .engine
        .get_setting("proxy_mode")
        .await
        .ok()
        .flatten()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty());
    let raw_proxy = state
        .engine
        .get_setting("proxy")
        .await
        .ok()
        .flatten()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let effective_mode = match raw_mode.as_deref() {
        Some("direct") => "direct",
        Some("system") => "system",
        Some("custom") => "custom",
        _ => {
            if raw_proxy.is_some() {
                "custom"
            } else {
                "system"
            }
        }
    };
    let effective = state.engine.effective_proxy().await.map_err(str_err)?;
    let sys_proxy = dl_net::detect_system_proxy();
    let display_proxy = match effective_mode {
        "direct" => None,
        "system" => sys_proxy.clone(),
        "custom" => raw_proxy,
        _ => None,
    };
    let proxy_active = display_proxy.is_some();

    let (active_res, direct_res) = if proxy_active {
        let (a, d) = tokio::join!(
            dl_net::diag::detect_ip(effective.as_deref()),
            dl_net::diag::detect_ip(Some(dl_net::DIRECT_PROXY)),
        );
        (a, Some(d))
    } else {
        (dl_net::diag::detect_ip(effective.as_deref()).await, None)
    };

    Ok(serde_json::json!({
        "proxy_mode": effective_mode,
        "effective_proxy": display_proxy,
        "active": active_res.as_ref().ok(),
        "active_error": active_res.as_ref().err().map(|e| e.to_string()),
        "direct": direct_res.and_then(|r| r.ok()),
    }))
}

#[tauri::command]
pub async fn run_speed_test(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    st: State<'_, SpeedTestState>,
    server: Option<String>,
) -> Result<dl_net::diag::SpeedTestResult, String> {
    let token = tokio_util::sync::CancellationToken::new();
    {
        let mut guard = st.0.lock().await;
        if let Some(prev) = guard.replace(token.clone()) {
            prev.cancel();
        }
    }
    let effective = state.engine.effective_proxy().await.map_err(str_err)?;
    let srv = server.unwrap_or_else(|| "auto".into());
    let app_ev = app.clone();
    let res = dl_net::diag::run_speed_test(effective.as_deref(), &srv, token, move |p| {
        let _ = app_ev.emit("net-speed-progress", &p);
    })
    .await
    .map_err(str_err);
    *st.0.lock().await = None;
    res
}

#[tauri::command]
pub async fn cancel_speed_test(st: State<'_, SpeedTestState>) -> Result<(), String> {
    if let Some(tok) = st.0.lock().await.take() {
        tok.cancel();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    #[test]
    fn extract_zip_safely_extracts_valid_entries_and_blocks_zip_slip() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("pkg.zip");
        {
            let f = std::fs::File::create(&zip_path).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            let opts = zip::write::SimpleFileOptions::default();
            zw.start_file("sub/hello.txt", opts).unwrap();
            zw.write_all(b"world").unwrap();
            zw.start_file("../evil.txt", opts).unwrap();
            zw.write_all(b"pwned").unwrap();
            zw.finish().unwrap();
        }

        let extracted = super::extract_zip_safely(&zip_path, dir.path()).unwrap();
        assert_eq!(extracted, dir.path().join("pkg"));
        assert_eq!(
            std::fs::read_to_string(extracted.join("sub/hello.txt")).unwrap(),
            "world"
        );
        assert!(!dir.path().join("evil.txt").exists());
        assert!(!dir.path().join("pkg").join("../evil.txt").exists());
        assert!(zip_path.exists(), "original zip must remain intact");
    }
}
