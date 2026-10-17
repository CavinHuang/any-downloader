//! 定时错峰下载：设置下载时段（如 23:00-07:00），离开时段按策略
//! 「暂停全部」或「时段外限速」，进入时段恢复。每 30s 轮询设置，
//! 沿时段边沿触发（窗口内不反复动作）。
//! 暂停策略记录被调度暂停的任务 id，进入时段只恢复这些——不越权恢复
//! 用户手动暂停的任务。限速策略用引擎的全局覆盖（对共享 Limiter 实时生效；
//! yt-dlp 托管子进程仅对新启动任务生效）。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Timelike;
use dl_engine::Engine;

pub fn spawn(engine: Engine) {
    tauri::async_runtime::spawn(async move {
        let state = Arc::new(Sched {
            auto_paused: Mutex::new(Vec::new()),
            phase: Mutex::new(Phase::Unknown),
        });
        loop {
            if let Err(e) = tick(&engine, &state).await {
                tracing::warn!(error = %e, "schedule tick failed");
            }
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    });
}

struct Sched {
    auto_paused: Mutex<Vec<String>>,
    phase: Mutex<Phase>,
}

#[derive(PartialEq, Clone, Copy)]
enum Phase {
    Unknown,
    InWindow,
    OutWindow,
}

#[derive(PartialEq, Clone, Copy)]
enum Outside {
    Pause,
    Limit,
}

struct Cfg {
    start: (u32, u32),
    end: (u32, u32),
    outside: Outside,
    outside_bps: u64,
}

async fn read_cfg(engine: &Engine) -> Option<Cfg> {
    let on = engine
        .get_setting("schedule_enabled")
        .await
        .ok()
        .flatten()
        .is_some_and(|v| v == "on");
    if !on {
        return None;
    }
    let window = engine
        .get_setting("schedule_window")
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "23:00-07:00".into());
    let (start, end) = parse_window(&window)?;
    let outside = match engine
        .get_setting("schedule_outside")
        .await
        .ok()
        .flatten()
        .as_deref()
    {
        Some("limit") => Outside::Limit,
        _ => Outside::Pause,
    };
    let outside_bps = engine
        .get_setting("schedule_outside_bps")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    Some(Cfg {
        start,
        end,
        outside,
        outside_bps,
    })
}

/// "HH:MM-HH:MM" → ((h,m),(h,m))；非法返回 None（视作未启用）。
fn parse_window(s: &str) -> Option<((u32, u32), (u32, u32))> {
    let (a, b) = s.trim().split_once('-')?;
    let parse = |t: &str| -> Option<(u32, u32)> {
        let (h, m) = t.trim().split_once(':')?;
        Some((h.trim().parse().ok()?, m.trim().parse().ok()?))
    };
    Some((parse(a)?, parse(b)?))
}

fn in_window(cfg: &Cfg) -> bool {
    let now = chrono::Local::now();
    let n = now.hour() * 60 + now.minute();
    let (s, e) = (to_mins(cfg.start), to_mins(cfg.end));
    if s == e {
        return true; // 零长时段视作全天
    }
    if s > e {
        n >= s || n < e // 跨午夜（23:00-07:00）
    } else {
        n >= s && n < e
    }
}

fn to_mins(t: (u32, u32)) -> u32 {
    t.0 * 60 + t.1
}

async fn tick(engine: &Engine, state: &Sched) -> Result<(), String> {
    let Some(cfg) = read_cfg(engine).await else {
        // 关闭：清覆盖、清记忆；不自动恢复任务（用户自行决定）
        engine.set_global_rate_override(0);
        *state.phase.lock().unwrap() = Phase::Unknown;
        state.auto_paused.lock().unwrap().clear();
        return Ok(());
    };
    let now_in = in_window(&cfg);
    let prev = *state.phase.lock().unwrap();
    let entering = now_in && prev != Phase::InWindow;
    let leaving = !now_in && prev != Phase::OutWindow;

    if leaving {
        match cfg.outside {
            Outside::Pause => {
                // 逐任务暂停：运行中/探测中走 checkpoint，排队与等待重试转 paused；
                // 合并/校验等收尾阶段不可暂停，让它们自然跑完
                let mut paused = Vec::new();
                if let Ok(list) = engine.list_tasks(None).await {
                    for t in list {
                        if matches!(
                            t.status,
                            dl_model::TaskStatus::Downloading
                                | dl_model::TaskStatus::Probing
                                | dl_model::TaskStatus::Queued
                                | dl_model::TaskStatus::WaitingRetry
                        ) && engine.pause(&t.id).await.is_ok()
                        {
                            paused.push(t.id);
                        }
                    }
                }
                let n = paused.len();
                *state.auto_paused.lock().unwrap() = paused;
                tracing::info!(paused = n, "off-peak: paused scheduled tasks");
            }
            Outside::Limit => {
                engine.set_global_rate_override(cfg.outside_bps);
                tracing::info!(bps = cfg.outside_bps, "off-peak: rate override on");
            }
        }
    }
    if entering {
        match cfg.outside {
            Outside::Pause => {
                let ids: Vec<String> = state.auto_paused.lock().unwrap().drain(..).collect();
                let mut started = 0;
                for id in ids {
                    if engine.start(&id).await.is_ok() {
                        started += 1;
                    }
                }
                tracing::info!(started, "window open: resumed scheduled tasks");
            }
            Outside::Limit => {
                engine.set_global_rate_override(0);
                tracing::info!("window open: rate override cleared");
            }
        }
    }
    *state.phase.lock().unwrap() = if now_in {
        Phase::InWindow
    } else {
        Phase::OutWindow
    };
    Ok(())
}
