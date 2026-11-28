//! Anything Downloader 桌面壳。
//!
//! 本进程即常驻主程序：引擎 + 内嵌 IPC 管道（扩展推送直达 GUI）+ 托盘。
//! 关窗 = 隐藏到托盘；托盘菜单/双击恢复；单实例由插件保证（二次启动聚焦首实例）。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod nm;
mod power;
mod schedule;
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
            let (recovered, resumable_ids) =
                tauri::async_runtime::block_on(engine.recover_stale_active_task_ids());
            if recovered > 0 {
                tracing::info!(recovered, "recovered stale active tasks to paused/failed");
            }
            let auto_resume = tauri::async_runtime::block_on(engine.get_setting("auto_resume_on_start"))
                .ok()
                .flatten()
                .map(|v| v != "off")
                .unwrap_or(true);
            if !auto_resume {
                let has_queued = tauri::async_runtime::block_on(
                    engine.list_tasks(Some(dl_model::TaskStatus::Queued)),
                )
                .map(|list| !list.is_empty())
                .unwrap_or(false);
                if has_queued {
                    let _ = tauri::async_runtime::block_on(engine.pause_queue());
                }
            }
            if auto_resume && !resumable_ids.is_empty() {
                let resume_engine = engine.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                    let queue_paused = resume_engine
                        .queue_status()
                        .await
                        .map(|q| q.paused)
                        .unwrap_or(false);
                    if !queue_paused {
                        for id in resumable_ids {
                            if let Ok(Some(t)) = resume_engine.get_task(&id).await {
                                if t.status == dl_model::TaskStatus::Paused {
                                    let _ = resume_engine.start_queued(&id).await;
                                }
                            }
                        }
                    }
                    resume_engine.spawn_queue_scheduler();
                });
            } else {
                let sched_engine = engine.clone();
                tauri::async_runtime::spawn(async move {
                    sched_engine.spawn_queue_scheduler();
                });
            }

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

            // 浏览器扩展本地 HTTP 桥（127.0.0.1:14371）：与 NM 双通道并行，免注册直连
            let bridge_handle = commands::BridgeHandle::default();
            {
                let bridge_port = tauri::async_runtime::block_on(engine.get_setting("bridge_port"))
                    .ok()
                    .flatten()
                    .and_then(|s| s.trim().parse::<u16>().ok())
                    .unwrap_or(dl_ipc::http_bridge::DEFAULT_BRIDGE_PORT);
                let bridge_token =
                    tauri::async_runtime::block_on(engine.get_setting("bridge_token"))
                        .ok()
                        .flatten();
                match tauri::async_runtime::block_on(
                    dl_ipc::http_bridge::HttpBridgeServer::start(
                        engine.clone(),
                        bridge_port,
                        bridge_token,
                    ),
                ) {
                    Ok(srv) => {
                        let st = srv.state.clone();
                        *bridge_handle.state.lock().unwrap() = Some(st.clone());
                        // 保活（随进程生命周期存活）
                        std::mem::forget(srv);
                        let web_handle = handle.clone();
                        let mut rx = st.subscribe();
                        tauri::async_runtime::spawn(async move {
                            loop {
                                match rx.recv().await {
                                    Ok(item) => {
                                        let _ = web_handle.emit("browser-sniff-event", &item);
                                    }
                                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                        continue
                                    }
                                    Err(_) => break,
                                }
                            }
                        });
                        tracing::info!(port = bridge_port, "extension http bridge listening");
                    }
                    Err(e) => {
                        tracing::warn!(port = bridge_port, error = %e, "extension http bridge start failed");
                    }
                }
            }
            app.manage(bridge_handle);

            // 剪贴板监听（可开关）：仅对真实可下载直链或支持识别的站点/视频链接弹窗提示
            {
                let handle = handle.clone();
                tauri::async_runtime::spawn(async move {
                    use tauri_plugin_clipboard_manager::ClipboardExt;
                    let mut last = handle.clipboard().read_text().unwrap_or_default();
                    loop {
                        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                        let eng = &handle.state::<AppState>().engine;
                        let enabled = eng
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
                        let hf_mirror = eng.get_setting("hf_mirror_site").await.ok().flatten();
                        let candidates = dl_engine::site_adapt::extract_downloadable_urls(
                            &text,
                            hf_mirror.as_deref(),
                        );
                        for (url, kind) in candidates {
                            if eng.has_active_url(&url).await {
                                continue;
                            }
                            let _ = handle.emit(
                                "clipboard-url",
                                serde_json::json!({ "url": url, "kind": kind }),
                            );
                            break;
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

            // yt-dlp 自动更新（延迟首查 + 24h 轮询）、防休眠守卫与定时错峰调度
            let engine_bg = engine.clone();
            ytdlp_update::spawn(handle.clone(), engine_bg.clone());
            schedule::spawn(engine_bg.clone());
            app.manage(power::spawn(engine_bg));

            // 远程添加：恢复上次的开关状态
            app.manage(commands::RemoteState::default());
            commands::remote_autostart(&engine, &app.state::<commands::RemoteState>());

            app.manage(commands::AppUpdateState::default());
            app.manage(commands::SpeedTestState::default());
            app.manage(AppState { engine });

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
                        app.state::<power::SleepController>().release();
                        // 优雅退出：先对运行中任务请求暂停（触发 journal checkpoint）
                        let n = app.state::<AppState>().engine.request_pause_all();
                        if n > 0 {
                            std::thread::sleep(std::time::Duration::from_millis(800));
                        }
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
            commands::move_to_queue,
            commands::cancel_task,
            commands::get_settings,
            commands::set_setting,
            commands::get_system_proxy,
            commands::detect_network_ip,
            commands::run_speed_test,
            commands::cancel_speed_test,
            commands::check_app_update,
            commands::install_app_update,
            commands::default_download_dir,
            commands::probe_url,
            commands::inspect_media_url,
            commands::inspect_site_url,
            commands::probe_site_mirrors,
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
            commands::update_task_url,
            commands::compute_task_checksum,
            commands::queue_status,
            commands::start_queue,
            commands::pause_queue,
            commands::reorder_queue,
            commands::set_max_concurrent,
            commands::sniff_page_url,
            commands::open_external_url,
            commands::remote_status,
            commands::set_remote_enabled,
            commands::extension_bridge_status,
            commands::extension_bridge_clear,
            commands::extension_bridge_download,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn show_main(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}
