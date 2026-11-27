//! 桌面嗅探状态机与 Tauri 命令（docs/DESKTOP-SNIFF.md）。
//!
//! 生命周期：启用 = 证书就绪 → 启动 dl-sniff 代理（自带 `/proxy.pac`）→ 接管系统 AutoConfigURL
//! （备份原值进 flag 文件，崩溃后启动时还原）；关闭 = 还原 PAC → 停代理。
//! 条目经 broadcast → `sniff-event` 推给 UI；下载 = CreateTask（含视频号
//! isaac64-xor postprocess）+ 即建即下，走引擎常规管线。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use dl_model::{CreateTask, PostProcess, PostProcessAlgo};
use dl_sniff::{Capture, SniffedItem};
use tauri::{AppHandle, Emitter, State};

use crate::AppState;

pub struct SniffState {
    pub capture: Arc<Capture>,
    inner: tokio::sync::Mutex<Option<Active>>,
    enabled: AtomicBool,
    #[cfg(windows)]
    overlay: tokio::sync::Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
}

struct Active {
    service: dl_sniff::SniffService,
    #[cfg(windows)]
    backup: crate::sysproxy::PacBackup,
    #[cfg(target_os = "macos")]
    backup: crate::macos_sniff::MacPacBackup,
}

impl SniffState {
    pub fn new() -> Self {
        Self {
            capture: Arc::new(Capture::new(50)),
            inner: Default::default(),
            enabled: AtomicBool::new(false),
            #[cfg(windows)]
            overlay: Default::default(),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }
}

const OVERLAY_LABEL: &str = "sniff-overlay";

#[cfg(windows)]
async fn run_overlay_watcher(app: tauri::AppHandle, sniff: Arc<SniffState>) {
    use tauri::Manager;
    let mut last: Option<(i32, i32)> = None;
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        if !sniff.is_enabled() {
            continue;
        }
        let wins = crate::wechat_overlay::wechat_windows();
        let pick = crate::wechat_overlay::pick_window(&wins, crate::wechat_overlay::cursor_pos());
        let Some(win) = pick else {
            if last.take().is_some() {
                if let Some(w) = app.get_webview_window(OVERLAY_LABEL) {
                    let _ = w.hide();
                }
            }
            continue;
        };
        let (x, y) = (win.x + win.w - 52, win.y + 12);
        if last != Some((x, y)) {
            if let Some(w) = app.get_webview_window(OVERLAY_LABEL) {
                let _ = w.set_position(tauri::PhysicalPosition::new(x, y));
                let _ = w.set_size(tauri::PhysicalSize::new(40, 40));
                let _ = w.show();
            }
            last = Some((x, y));
        }
    }
}

fn hide_overlay(app: &AppHandle) {
    use tauri::Manager;
    if let Some(w) = app.get_webview_window(OVERLAY_LABEL) {
        let _ = w.hide();
    }
}

fn data_dir() -> std::path::PathBuf {
    dl_engine::data_dir()
}

fn flag_path() -> std::path::PathBuf {
    data_dir().join("sniff").join("pac-active.json")
}

fn item_to_create(it: &SniffedItem) -> CreateTask {
    let headers = if it.headers.is_empty() {
        None
    } else {
        Some(it.headers.clone())
    };
    let postprocess = it.seed.as_ref().map(|seed| {
        vec![PostProcess {
            algo: PostProcessAlgo::Isaac64Xor,
            seed: seed.clone(),
        }]
    });
    let filename = it
        .title
        .as_deref()
        .map(dl_filename::sanitize)
        .filter(|s| !s.is_empty() && s != "download")
        .map(|s| {
            if s.to_ascii_lowercase().ends_with(".mp4") {
                s
            } else {
                format!("{s}.mp4")
            }
        });
    CreateTask {
        url: it.url.clone(),
        kind: None,
        dir: None,
        filename,
        priority: None,
        headers,
        checksum: None,
        mirrors: None,
        postprocess,
        proxy: None,
    }
}

async fn download_item(state: &AppState, item: &SniffedItem) -> Result<String, String> {
    let task = state
        .engine
        .create_task(item_to_create(item))
        .await
        .map_err(|e| e.to_string())?;
    let id = task.id.clone();
    if let Err(e) = state.engine.start(&id).await {
        tracing::warn!(error = %e, "sniff 任务启动失败（可能已完成/重复）");
    }
    Ok(id)
}

fn platform_name() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

fn proxy_supported() -> bool {
    cfg!(any(windows, target_os = "macos"))
}

fn cert_installed_status() -> bool {
    #[cfg(windows)]
    {
        crate::cert::status()
    }
    #[cfg(target_os = "macos")]
    {
        crate::macos_sniff::cert_status()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        false
    }
}

#[tauri::command]
pub async fn sniff_status(state: State<'_, Arc<SniffState>>) -> Result<serde_json::Value, String> {
    let port = current_port(&state).await;
    let pac_url = port.map(|p| format!("http://127.0.0.1:{p}/proxy.pac"));
    Ok(serde_json::json!({
        "enabled": state.is_enabled(),
        "certInstalled": cert_installed_status(),
        "port": port,
        "pacUrl": pac_url,
        "platform": platform_name(),
        "supported": proxy_supported(),
        "items": state.capture.items(),
    }))
}

async fn current_port(state: &State<'_, Arc<SniffState>>) -> Option<u16> {
    let g = state.inner.lock().await;
    g.as_ref().map(|a| a.service.port)
}

#[tauri::command]
pub async fn sniff_set_enabled(
    app: AppHandle,
    sniff: State<'_, Arc<SniffState>>,
    engine: State<'_, AppState>,
    on: bool,
) -> Result<serde_json::Value, String> {
    set_enabled(&app, &sniff, &engine, on).await
}

pub async fn set_enabled(
    _app: &AppHandle,
    sniff: &Arc<SniffState>,
    engine: &AppState,
    on: bool,
) -> Result<serde_json::Value, String> {
    let mut g = sniff.inner.lock().await;
    if on {
        if g.is_some() {
            return Ok(ok_json(sniff));
        }
        #[cfg(windows)]
        {
            crate::cert::ensure_ca(&data_dir()).map_err(|e| e.to_string())?;
            if !crate::cert::status() {
                return Err("cert_missing".into());
            }
            let port = configured_port(engine).await;
            let mut cfg = dl_sniff::SniffConfig::new(data_dir());
            cfg.port = port;
            let svc = dl_sniff::start(cfg, sniff.capture.clone())
                .await
                .map_err(|e| e.to_string())?;
            let backup = match crate::sysproxy::enable(svc.port) {
                Ok(b) => b,
                Err(e) => {
                    svc.stop();
                    return Err(e.to_string());
                }
            };
            let _ = std::fs::create_dir_all(data_dir().join("sniff"));
            let _ = std::fs::write(
                flag_path(),
                serde_json::to_string(&backup).unwrap_or_default(),
            );
            sniff.enabled.store(true, Ordering::Relaxed);
            *g = Some(Active {
                service: svc,
                backup,
            });
            let mut ov = sniff.overlay.lock().await;
            if let Some(old) = ov.take() {
                old.abort();
            }
            *ov = Some(tauri::async_runtime::spawn(run_overlay_watcher(
                _app.clone(),
                sniff.clone(),
            )));
        }
        #[cfg(target_os = "macos")]
        {
            crate::macos_sniff::ensure_ca(&data_dir()).map_err(|e| e.to_string())?;
            if !crate::macos_sniff::cert_status() {
                return Err("cert_missing".into());
            }
            let port = configured_port(engine).await;
            let mut cfg = dl_sniff::SniffConfig::new(data_dir());
            cfg.port = port;
            let svc = dl_sniff::start(cfg, sniff.capture.clone())
                .await
                .map_err(|e| e.to_string())?;
            let backup = match crate::macos_sniff::enable_pac(svc.port) {
                Ok(b) => b,
                Err(e) => {
                    svc.stop();
                    return Err(e.to_string());
                }
            };
            let _ = std::fs::create_dir_all(data_dir().join("sniff"));
            let _ = std::fs::write(
                flag_path(),
                serde_json::to_string(&backup).unwrap_or_default(),
            );
            sniff.enabled.store(true, Ordering::Relaxed);
            *g = Some(Active {
                service: svc,
                backup,
            });
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            let _ = engine;
            return Err("proxy_sniff_unsupported_os".into());
        }
    } else if let Some(a) = g.take() {
        #[cfg(windows)]
        {
            let _ = crate::sysproxy::disable(&a.backup);
        }
        #[cfg(target_os = "macos")]
        {
            let _ = crate::macos_sniff::disable_pac(&a.backup);
        }
        let _ = std::fs::remove_file(flag_path());
        a.service.stop();
        sniff.enabled.store(false, Ordering::Relaxed);
        #[cfg(windows)]
        if let Some(ov) = sniff.overlay.lock().await.take() {
            ov.abort();
        }
        hide_overlay(_app);
    }
    let _ = engine
        .engine
        .set_setting("sniff_desktop", if on { "on" } else { "off" })
        .await;
    Ok(ok_json(sniff))
}

fn ok_json(sniff: &SniffState) -> serde_json::Value {
    serde_json::json!({ "ok": true, "enabled": sniff.is_enabled() })
}

#[cfg(any(windows, target_os = "macos"))]
async fn configured_port(engine: &AppState) -> u16 {
    engine
        .engine
        .get_setting("sniff_port")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(dl_sniff::DEFAULT_PORT)
}

#[tauri::command]
pub async fn sniff_cert_install() -> Result<serde_json::Value, String> {
    #[cfg(windows)]
    {
        let ca = crate::cert::ensure_ca(&data_dir()).map_err(|e| e.to_string())?;
        crate::cert::install(&ca).map_err(|e| e.to_string())?;
        Ok(serde_json::json!({ "ok": true }))
    }
    #[cfg(target_os = "macos")]
    {
        let ca = crate::macos_sniff::ensure_ca(&data_dir()).map_err(|e| e.to_string())?;
        crate::macos_sniff::cert_install(&ca).map_err(|e| e.to_string())?;
        Ok(serde_json::json!({ "ok": true }))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        Err("proxy_sniff_unsupported_os".into())
    }
}

#[tauri::command]
pub async fn sniff_cert_uninstall() -> Result<serde_json::Value, String> {
    #[cfg(windows)]
    {
        crate::cert::uninstall().map_err(|e| e.to_string())?;
        Ok(serde_json::json!({ "ok": true }))
    }
    #[cfg(target_os = "macos")]
    {
        crate::macos_sniff::cert_uninstall().map_err(|e| e.to_string())?;
        Ok(serde_json::json!({ "ok": true }))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        Err("proxy_sniff_unsupported_os".into())
    }
}

#[tauri::command]
pub async fn sniff_list(state: State<'_, Arc<SniffState>>) -> Result<Vec<SniffedItem>, String> {
    Ok(state.capture.items())
}

#[tauri::command]
pub async fn sniff_clear(state: State<'_, Arc<SniffState>>) -> Result<serde_json::Value, String> {
    state.capture.clear();
    Ok(serde_json::json!({ "ok": true }))
}

#[tauri::command]
pub async fn sniff_download(
    sniff: State<'_, Arc<SniffState>>,
    state: State<'_, AppState>,
    id: String,
) -> Result<serde_json::Value, String> {
    let item = sniff
        .capture
        .items()
        .into_iter()
        .find(|i| i.id == id)
        .ok_or("item_not_found")?;
    let task_id = download_item(&state, &item).await?;
    Ok(serde_json::json!({ "ok": true, "taskId": task_id }))
}

#[tauri::command]
pub async fn sniff_overlay_download(
    sniff: State<'_, Arc<SniffState>>,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let item = sniff.capture.latest().ok_or("no_item")?;
    let task_id = download_item(&state, &item).await?;
    Ok(serde_json::json!({ "ok": true, "taskId": task_id, "url": item.url }))
}

pub fn bootstrap(app: &AppHandle, sniff: Arc<SniffState>, engine: AppState) {
    {
        use tauri::Manager;
        if let Some(w) = app.get_webview_window(OVERLAY_LABEL) {
            let _ = w.set_focusable(false);
        }
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        #[cfg(windows)]
        if let Ok(txt) = std::fs::read_to_string(flag_path()) {
            let backup: Option<crate::sysproxy::PacBackup> =
                serde_json::from_str(&txt).ok().or(Some(Default::default()));
            if let Some(b) = backup {
                let _ = crate::sysproxy::disable(&b);
            }
            let _ = std::fs::remove_file(flag_path());
        }
        #[cfg(target_os = "macos")]
        if let Ok(txt) = std::fs::read_to_string(flag_path()) {
            if let Ok(b) = serde_json::from_str::<crate::macos_sniff::MacPacBackup>(&txt) {
                let _ = crate::macos_sniff::disable_pac(&b);
            }
            let _ = std::fs::remove_file(flag_path());
        }
        let want = engine
            .engine
            .get_setting("sniff_desktop")
            .await
            .ok()
            .flatten()
            .is_some_and(|v| v == "on");
        if want {
            if let Err(e) = set_enabled(&app, &sniff, &engine, true).await {
                tracing::info!(error = %e, "桌面嗅探未自动恢复（常见：证书未安装）");
            }
        }
        let mut rx = sniff.capture.subscribe();
        while let Ok(item) = rx.recv().await {
            let _ = app.emit(
                "sniff-event",
                serde_json::json!({ "type": "item", "item": item }),
            );
        }
    });
}

pub fn shutdown_cleanup(app: &AppHandle, sniff: &SniffState) {
    hide_overlay(app);
    #[cfg(windows)]
    if sniff.is_enabled() {
        if let Ok(txt) = std::fs::read_to_string(flag_path()) {
            if let Ok(b) = serde_json::from_str::<crate::sysproxy::PacBackup>(&txt) {
                let _ = crate::sysproxy::disable(&b);
            }
        }
        let _ = std::fs::remove_file(flag_path());
    }
    #[cfg(target_os = "macos")]
    if sniff.is_enabled() {
        if let Ok(txt) = std::fs::read_to_string(flag_path()) {
            if let Ok(b) = serde_json::from_str::<crate::macos_sniff::MacPacBackup>(&txt) {
                let _ = crate::macos_sniff::disable_pac(&b);
            }
        }
        let _ = std::fs::remove_file(flag_path());
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    let _ = sniff;
}
