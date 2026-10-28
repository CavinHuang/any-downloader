//! Tauri 命令层：UI 进程内直调引擎（DESIGN §3 运行时纪律——命令只发调用，
//! 不 block_on；下载驱动跑在 tauri::async_runtime 上）。

use std::collections::HashMap;

use dl_engine::{DownloadOptions, Engine};
use dl_model::Task;
use tauri::State;

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
pub async fn add_task(
    state: State<'_, AppState>,
    url: String,
    dir: Option<String>,
    filename: Option<String>,
    headers: Option<HashMap<String, String>>,
    checksum: Option<String>,
    mirrors: Option<Vec<String>>,
) -> Result<Task, String> {
    let mut c = dl_model::CreateTask::of(url);
    c.dir = dir.filter(|s| !s.is_empty());
    c.filename = filename.filter(|s| !s.is_empty());
    c.headers = headers.filter(|h| !h.is_empty());
    c.checksum = checksum.filter(|s| !s.is_empty());
    c.mirrors = mirrors.filter(|m| !m.is_empty());
    state.engine.create_task(c).await.map_err(str_err)
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

/// 删除任务（运行中任务需先取消/暂停）。
#[tauri::command]
pub async fn remove_task(state: State<'_, AppState>, id: String) -> Result<bool, String> {
    state.engine.remove(&id).await.map_err(str_err)
}

#[tauri::command]
pub async fn retry_task(state: State<'_, AppState>, id: String) -> Result<Task, String> {
    state.engine.retry(&id).await.map_err(str_err)
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
        "proxy",
        "conflict",
        "ffmpeg_path",
        "yt_dlp_path",
        "yt_dlp_subs",
        "yt_dlp_meta",
        "yt_dlp_cookies",
        "max_concurrent",
        "max_auto_retry",
        "auto_categorize",
        "after_done",
        "clipboard_monitor",
        "ytdlp_auto_update",
        "schedule_enabled",
        "schedule_window",
        "schedule_outside",
        "schedule_outside_bps",
        "update_auto",
        "run_command",
        "sniff_desktop",
        "sniff_port",
        "sniff_overlay",
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

/// URL 探测（新建任务对话框的「识别」按钮）。
#[tauri::command]
pub async fn probe_url(url: String) -> Result<serde_json::Value, String> {
    let client = dl_net::build_client(None).map_err(str_err)?;
    let r = dl_net::probe::probe(&client, &url, &HashMap::new())
        .await
        .map_err(str_err)?;
    Ok(serde_json::json!({
        "filename": r.filename,
        "total_bytes": r.total_bytes,
        "mime": r.mime,
        "segmented": r.segmented,
    }))
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

/// 浏览器扩展 NM 通道是否已注册（Chrome/Edge 任一即视为已注册）。
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
            support
                .join("Google/Chrome/NativeMessagingHosts/anything_dl.json")
                .exists()
                || support
                    .join("Microsoft Edge/NativeMessagingHosts/anything_dl.json")
                    .exists()
        } else {
            let config = dirs::config_dir().unwrap_or_else(|| home.join(".config"));
            ["google-chrome", "microsoft-edge", "chromium"]
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
