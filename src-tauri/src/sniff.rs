//! 桌面嗅探状态机与 Tauri 命令（docs/DESKTOP-SNIFF.md）。
//!
//! 生命周期：启用 = 证书就绪 → 启动 dl-sniff 代理 → 写 PAC → 接 AutoConfigURL
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
    /// 微信窗口悬浮图标看护任务（启用时起、停用时止）
    overlay: tokio::sync::Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
}

struct Active {
    service: dl_sniff::SniffService,
    #[cfg(windows)]
    backup: crate::sysproxy::PacBackup,
}

impl SniffState {
    pub fn new() -> Self {
        Self {
            capture: Arc::new(Capture::new(50)),
            inner: Default::default(),
            enabled: AtomicBool::new(false),
            overlay: Default::default(),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------- 悬浮图标

const OVERLAY_LABEL: &str = "sniff-overlay";

/// 250ms 轮询：找视频号宿主窗 → 钉 `sniff-overlay` 于右上角；无候选即隐藏。
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
        // 右上角内收（52/12 与悬浮窗 40px 尺寸匹配）
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

/// 条目 → 引擎任务（种子 → postprocess；头子集与引擎 allowlist 对齐）
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
    CreateTask {
        url: it.url.clone(),
        kind: None,
        dir: None,
        filename: None,
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
    // 已存在的活动任务直接视为成功（重复点击场景）
    if let Err(e) = state.engine.start(&id).await {
        tracing::warn!(error = %e, "sniff 任务启动失败（可能已完成/重复）");
    }
    Ok(id)
}

// ---------------------------------------------------------------- 命令

#[tauri::command]
pub async fn sniff_status(state: State<'_, Arc<SniffState>>) -> Result<serde_json::Value, String> {
    #[cfg(windows)]
    let cert_installed = crate::cert::status();
    #[cfg(not(windows))]
    let cert_installed = false;
    Ok(serde_json::json!({
        "enabled": state.is_enabled(),
        "certInstalled": cert_installed,
        "port": current_port(&state).await,
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
            crate::cert::ensure_ca(&data_dir()).map_err(|e| e.to_string())?; // 幂等：确保 CA 已生成
            if !crate::cert::status() {
                return Err("cert_missing".into());
            }
            let port = configured_port(engine).await;
            let mut cfg = dl_sniff::SniffConfig::new(data_dir());
            cfg.port = port;
            let svc = dl_sniff::start(cfg, sniff.capture.clone())
                .await
                .map_err(|e| e.to_string())?;
            let pac = data_dir().join("sniff").join("adl-sniff.pac");
            crate::sysproxy::write_pac(&pac, svc.port).map_err(|e| e.to_string())?;
            let backup = match crate::sysproxy::enable(&pac) {
                Ok(b) => b,
                Err(e) => {
                    svc.stop();
                    return Err(e.to_string());
                }
            };
            // flag 文件：崩溃后下次启动还原（PacBackup JSON）
            let _ = std::fs::write(
                flag_path(),
                serde_json::to_string(&backup).unwrap_or_default(),
            );
            sniff.enabled.store(true, Ordering::Relaxed);
            *g = Some(Active {
                service: svc,
                backup,
            });
            // 悬浮图标看护（config 里 sniff-overlay 窗口初始隐藏）
            let mut ov = sniff.overlay.lock().await;
            if let Some(old) = ov.take() {
                old.abort();
            }
            *ov = Some(tauri::async_runtime::spawn(run_overlay_watcher(
                _app.clone(),
                sniff.clone(),
            )));
        }
        #[cfg(not(windows))]
        {
            let _ = engine;
            return Err("windows_only".into());
        }
    } else if let Some(a) = g.take() {
        #[cfg(windows)]
        {
            let _ = crate::sysproxy::disable(&a.backup);
        }
        let _ = std::fs::remove_file(flag_path());
        a.service.stop();
        sniff.enabled.store(false, Ordering::Relaxed);
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

#[cfg(windows)]
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
    #[cfg(not(windows))]
    {
        Err("windows_only".into())
    }
}

#[tauri::command]
pub async fn sniff_cert_uninstall() -> Result<serde_json::Value, String> {
    #[cfg(windows)]
    {
        crate::cert::uninstall().map_err(|e| e.to_string())?;
        Ok(serde_json::json!({ "ok": true }))
    }
    #[cfg(not(windows))]
    {
        Err("windows_only".into())
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

/// 悬浮图标单击：下载最近一条嗅探条目（Phase C 入口）。
#[tauri::command]
pub async fn sniff_overlay_download(
    sniff: State<'_, Arc<SniffState>>,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let item = sniff.capture.latest().ok_or("no_item")?;
    let task_id = download_item(&state, &item).await?;
    Ok(serde_json::json!({ "ok": true, "taskId": task_id, "url": item.url }))
}

// ---------------------------------------------------------------- 启动/退出

/// 启动接线：崩溃恢复（flag 残留 → 还原 PAC），按设置自动恢复嗅探，事件泵。
pub fn bootstrap(app: &AppHandle, sniff: Arc<SniffState>, engine: AppState) {
    {
        use tauri::Manager;
        if let Some(w) = app.get_webview_window(OVERLAY_LABEL) {
            // 不抢微信焦点（WS_EX_NOACTIVATE 语义）；其余窗口属性走 config
            let _ = w.set_focusable(false);
        }
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // 崩溃残留：还原原 AutoConfigURL 后按设置重新启用
        #[cfg(windows)]
        if let Ok(txt) = std::fs::read_to_string(flag_path()) {
            let backup: Option<crate::sysproxy::PacBackup> =
                serde_json::from_str(&txt).ok().or(Some(Default::default()));
            if let Some(b) = backup {
                let _ = crate::sysproxy::disable(&b);
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
        // 事件泵：新条目 → UI（主窗口 + 悬浮图标窗口都监听同一事件）
        let mut rx = sniff.capture.subscribe();
        while let Ok(item) = rx.recv().await {
            let _ = app.emit(
                "sniff-event",
                serde_json::json!({ "type": "item", "item": item }),
            );
        }
    });
}

/// 退出清理（托盘退出路径调用）：还原 PAC。服务随进程消亡。
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
    #[cfg(not(windows))]
    let _ = sniff;
}
