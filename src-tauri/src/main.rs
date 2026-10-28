//! Anything Downloader 桌面壳。
//!
//! 本进程即常驻主程序：引擎 + 内嵌 IPC 管道（扩展推送直达 GUI）+ 托盘。
//! 关窗 = 隐藏到托盘；托盘菜单/双击恢复；单实例由插件保证（二次启动聚焦首实例）。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
mod cert;
mod commands;
mod nm;
mod schedule;
mod sniff;
#[cfg(windows)]
mod sysproxy;
#[cfg(windows)]
mod wechat_overlay;
mod ytdlp_update;

use dl_engine::Engine;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager,
};

pub struct AppState {
    pub engine: Engine,
}

fn main() {
    // 日志：GUI 无控制台 → 按天轮转落盘（数据目录/logs）+ stderr 镜像
    let logs_dir = dl_engine::data_dir().join("logs");
    let _ = std::fs::create_dir_all(&logs_dir);
    let appender = tracing_appender::rolling::daily(&logs_dir, "dl.log");
    let (log_writer, log_guard) = tracing_appender::non_blocking(appender);
    std::mem::forget(log_guard); // 进程生命周期内持有
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(log_writer)
        .with_ansi(false)
        .init();

    // 崩溃日志（M5）：panic 信息脱敏后落盘，供「导出诊断包」使用
    let crash_dir = dl_engine::data_dir().join("crash");
    let _ = std::fs::create_dir_all(&crash_dir);
    std::panic::set_hook(Box::new(move |info| {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let msg = format!(
            "panic at {}
thread: {:?}
{}
",
            ts,
            std::thread::current().name().unwrap_or("<unnamed>"),
            info
        );
        let _ = std::fs::write(crash_dir.join(format!("crash-{ts}.log")), &msg);
        eprintln!("{msg}");
    }));

    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        // 自动更新（GitHub Releases 分发）+ 重启装新版本
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .setup(|app| {
            let handle = app.handle().clone();
            let engine = tauri::async_runtime::block_on(Engine::open_default())
                .expect("open engine database");

            // 扩展通道：启动即把 NM 指向自带 dl-server（幂等、best-effort）
            nm::ensure_registered();

            // 内嵌 IPC 管道：扩展推送 / dl-server call 直达本进程
            {
                let engine = engine.clone();
                let endpoint = dl_ipc::resolve_pipe_name(None);
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = dl_ipc::serve(engine, &endpoint).await {
                        tracing::error!(error = %e, "ipc pipe server exited");
                    }
                });
            }

            // 剪贴板监听（可开关）：新出现直链 → 事件提示 UI（DESIGN §8）
            {
                let handle = handle.clone();
                tauri::async_runtime::spawn(async move {
                    use tauri_plugin_clipboard_manager::ClipboardExt;
                    let mut last = String::new();
                    loop {
                        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                        let enabled = handle
                            .state::<AppState>()
                            .engine
                            .get_setting("clipboard_monitor")
                            .await
                            .ok()
                            .flatten()
                            .map(|v| v != "off")
                            .unwrap_or(true);
                        if !enabled {
                            continue;
                        }
                        let Ok(text) = handle.clipboard().read_text() else {
                            continue;
                        };
                        if text == last {
                            continue;
                        }
                        last = text.clone();
                        for url in extract_urls(&text) {
                            let _ = handle.emit("clipboard-url", serde_json::json!({ "url": url }));
                        }
                    }
                });
            }

            // 引擎事件 → WebView（100ms 聚合进度 + 状态变更）
            {
                let engine = engine.clone();
                let handle = handle.clone();
                tauri::async_runtime::spawn(async move {
                    let mut rx = engine.subscribe();
                    loop {
                        match rx.recv().await {
                            Ok(ev) => {
                                let _ = handle.emit("engine-event", &ev);
                                // 完成通知（聚合与免打扰在 M7/M8 治理）
                                if let dl_engine::EngineEvent::StateChanged {
                                    status: dl_model::TaskStatus::Done,
                                    task_id,
                                } = &ev
                                {
                                    let st = handle.state::<AppState>();
                                    if let Ok(Some(t)) = st.engine.get_task(task_id).await {
                                        use tauri_plugin_notification::NotificationExt;
                                        let _ = handle
                                            .notification()
                                            .builder()
                                            .title("下载完成")
                                            .body(t.filename.as_deref().unwrap_or(task_id))
                                            .show();

                                        // 完成后动作（设置 after_done；引擎只存设置，执行在应用层）
                                        let action = st
                                            .engine
                                            .get_setting("after_done")
                                            .await
                                            .ok()
                                            .flatten()
                                            .unwrap_or_else(|| "none".into());
                                        if action != "none" {
                                            let dir = t.dir.clone().unwrap_or_else(
                                                commands::default_download_dir_impl,
                                            );
                                            let file = std::path::Path::new(&dir)
                                                .join(t.filename.as_deref().unwrap_or(""))
                                                .display()
                                                .to_string();
                                            let run_cmd = st
                                                .engine
                                                .get_setting("run_command")
                                                .await
                                                .ok()
                                                .flatten()
                                                .unwrap_or_default();
                                            commands::run_after_done(
                                                &st.engine, &action, &run_cmd, &file, &dir, task_id,
                                            )
                                            .await;
                                        }
                                    }
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(_) => break,
                        }
                    }
                });
            }

            // yt-dlp 自动更新（延迟首查 + 24h 轮询）与定时错峰调度
            let engine_bg = engine.clone();
            ytdlp_update::spawn(handle.clone(), engine_bg.clone());
            schedule::spawn(engine_bg);

            // 远程添加：恢复上次的开关状态
            app.manage(commands::RemoteState::default());
            commands::remote_autostart(&engine, &app.state::<commands::RemoteState>());

            app.manage(AppState { engine });

            // 桌面嗅探（视频号）：崩溃恢复 PAC + 按设置自启 + 条目事件泵
            {
                let sniff_state = std::sync::Arc::new(sniff::SniffState::new());
                app.manage(sniff_state.clone());
                let app_state = AppState {
                    engine: app.state::<AppState>().engine.clone(),
                };
                sniff::bootstrap(&handle, sniff_state, app_state);
            }

            // 托盘：左键/菜单恢复窗口，菜单退出
            let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            let mut tray = TrayIconBuilder::with_id("main-tray")
                .menu(&menu)
                .tooltip("Anything Downloader")
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => show_main(app),
                    "quit" => {
                        // 优雅退出：先对运行中任务请求暂停（触发 journal checkpoint）
                        let n = app.state::<AppState>().engine.request_pause_all();
                        if n > 0 {
                            std::thread::sleep(std::time::Duration::from_millis(800));
                        }
                        // 桌面嗅探退出清理（还原系统 PAC）
                        sniff::shutdown_cleanup(
                            app,
                            &app.state::<std::sync::Arc<sniff::SniffState>>(),
                        );
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main(tray.app_handle());
                    }
                });
            if let Some(icon) = app.default_window_icon().cloned() {
                tray = tray.icon(icon);
            }
            tray.build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            // 关窗即隐藏到托盘（退出走托盘菜单）
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_tasks,
            commands::tasks_updated_since,
            commands::get_task,
            commands::add_task,
            commands::remove_task,
            commands::retry_task,
            commands::start_download,
            commands::pause_task,
            commands::cancel_task,
            commands::get_settings,
            commands::set_setting,
            commands::default_download_dir,
            commands::probe_url,
            commands::reveal_task,
            commands::export_tasks,
            commands::import_tasks,
            commands::open_task,
            commands::get_autostart,
            commands::set_autostart,
            commands::open_logs_dir,
            commands::extension_bundle,
            commands::search_content,
            commands::open_extension_dir,
            commands::open_extension_doc,
            commands::nm_registered,
            commands::set_priority,
            commands::rename_task,
            commands::remote_status,
            commands::set_remote_enabled,
            sniff::sniff_status,
            sniff::sniff_set_enabled,
            sniff::sniff_cert_install,
            sniff::sniff_cert_uninstall,
            sniff::sniff_list,
            sniff::sniff_clear,
            sniff::sniff_download,
            sniff::sniff_overlay_download,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// 从剪贴板文本提取 http(s) 直链（按空白切分、去尾部标点）。
fn extract_urls(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| w.trim_matches(|c: char| c == ',' || c == ')' || c == '"' || c == '<'))
        .filter(|w| w.starts_with("http://") || w.starts_with("https://"))
        .filter(|w| w.len() > 11)
        .map(|w| w.to_string())
        .collect()
}

fn show_main(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}
