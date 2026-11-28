//! 下载期间防止系统休眠（macOS caffeinate / Windows SetThreadExecutionState / Linux systemd-inhibit）。
//!
//! 独立的 OS 工作线程持有平台休眠句柄（Windows `SetThreadExecutionState` 为线程局部状态），
//! 异步监控任务只在“是否有活跃下载且未关闭防休眠”发生状态切换时通知工作线程。

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use dl_engine::Engine;

#[derive(Clone)]
pub struct SleepController {
    tx: Sender<bool>,
}

impl SleepController {
    /// 主动释放休眠阻止（应用退出时调用）。
    pub fn release(&self) {
        let _ = self.tx.send(false);
    }
}

pub fn spawn(engine: Engine) -> SleepController {
    let (tx, rx) = mpsc::channel::<bool>();
    std::thread::Builder::new()
        .name("sleep-guard".into())
        .spawn(move || worker_loop(rx))
        .expect("spawn sleep guard thread");

    let monitor_tx = tx.clone();
    tauri::async_runtime::spawn(async move {
        let mut rx_ev = engine.subscribe();
        let mut active = false;
        loop {
            let prevent = engine
                .get_setting("prevent_sleep")
                .await
                .ok()
                .flatten()
                .map(|v| v != "off")
                .unwrap_or(true);
            let want = prevent && engine.running_count() > 0;
            if want != active {
                active = want;
                if monitor_tx.send(active).is_err() {
                    return;
                }
            }
            tokio::select! {
                ev = rx_ev.recv() => {
                    if let Err(tokio::sync::broadcast::error::RecvError::Closed) = ev {
                        return;
                    }
                }
                _ = tokio::time::sleep(Duration::from_secs(2)) => {}
            }
        }
    });

    SleepController { tx }
}

fn worker_loop(rx: Receiver<bool>) {
    #[cfg(not(target_os = "windows"))]
    let mut child: Option<std::process::Child> = None;

    while let Ok(keep_awake) = rx.recv() {
        // 合并队列中积压的状态切换，只处理最终值
        let mut target = keep_awake;
        while let Ok(newer) = rx.try_recv() {
            target = newer;
        }
        #[cfg(target_os = "windows")]
        set_windows_sleep(target);

        #[cfg(not(target_os = "windows"))]
        set_unix_sleep(&mut child, target);
    }

    // 通道关闭（进程退出）时兜底释放
    #[cfg(target_os = "windows")]
    set_windows_sleep(false);
    #[cfg(not(target_os = "windows"))]
    set_unix_sleep(&mut child, false);
}

#[cfg(target_os = "windows")]
fn set_windows_sleep(keep_awake: bool) {
    use windows_sys::Win32::System::Power::{
        SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED,
    };
    let flags = if keep_awake {
        ES_CONTINUOUS | ES_SYSTEM_REQUIRED
    } else {
        ES_CONTINUOUS
    };
    unsafe {
        SetThreadExecutionState(flags);
    }
}

#[cfg(target_os = "macos")]
fn set_unix_sleep(child: &mut Option<std::process::Child>, keep_awake: bool) {
    if keep_awake {
        if let Some(c) = child.as_mut() {
            if c.try_wait().ok().flatten().is_none() {
                return;
            }
            *child = None;
        }
        let pid = std::process::id().to_string();
        if let Ok(c) = std::process::Command::new("caffeinate")
            .args(["-i", "-w", &pid])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            *child = Some(c);
        }
    } else if let Some(mut c) = child.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn set_unix_sleep(child: &mut Option<std::process::Child>, keep_awake: bool) {
    if keep_awake {
        if let Some(c) = child.as_mut() {
            if c.try_wait().ok().flatten().is_none() {
                return;
            }
            *child = None;
        }
        if let Ok(c) = std::process::Command::new("systemd-inhibit")
            .args([
                "--what=idle:sleep",
                "--who=Anything Downloader",
                "--why=Downloading files",
                "--mode=block",
                "sleep",
                "infinity",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            *child = Some(c);
        }
    } else if let Some(mut c) = child.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
}
