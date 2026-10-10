use std::path::PathBuf;

use clap::{Parser, Subcommand};
use dl_engine::DownloadOptions;
use dl_engine::{Engine, EngineEvent};
use dl_filename::ConflictPolicy;
use dl_model::TaskStatus;
use dl_model::{CreateTask, Task, TaskKind};

#[derive(Parser)]
#[command(name = "dl-cli", version, about = "Anything Downloader 引擎 CLI")]
struct Cli {
    /// 数据库文件路径（默认 <数据目录>/data.db）
    #[arg(long, global = true, value_name = "PATH")]
    db: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 创建任务
    Add {
        url: String,
        /// 任务类型：http | hls（缺省 http）
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        dir: Option<String>,
        #[arg(long)]
        filename: Option<String>,
        /// 附加镜像 URL（可多次提供）
        #[arg(long = "mirror")]
        mirrors: Vec<String>,
        /// 完成后哈希校验（sha256/sha1/md5，可选）
        #[arg(long)]
        checksum: Option<String>,
        /// 视频号解密：decode_key（下载完成后对前 128KB 做 Isaac64-XOR）
        #[arg(long)]
        isaac64_seed: Option<String>,
    },
    /// 列出任务
    List {
        /// 仅列出指定状态
        #[arg(long)]
        status: Option<String>,
    },
    /// 查看任务详情（JSON）
    Show {
        id: String,
    },
    Pause {
        id: String,
    },
    Resume {
        id: String,
    },
    Cancel {
        id: String,
    },
    /// 失败/取消后重新入队
    Retry {
        id: String,
    },
    /// 重命名任务（仅非活跃态）
    Rename {
        id: String,
        filename: String,
    },
    Remove {
        id: String,
    },
    /// 下载指定任务（Queued 或 Paused 续传），Ctrl-C 优雅暂停
    Download {
        id: String,
        /// 分段数（1-32）
        #[arg(long, default_value_t = 8)]
        segments: usize,
        /// 单任务限速（字节/秒）
        #[arg(long)]
        max_speed: Option<u64>,
        /// 全局限速（字节/秒）
        #[arg(long)]
        global_speed: Option<u64>,
        /// 代理：http://… | socks5://… | socks5h://…
        #[arg(long)]
        proxy: Option<String>,
        /// 冲突策略：rename | overwrite | skip | ask
        #[arg(long, default_value = "rename")]
        conflict: String,
    },
    /// 处理队列中全部任务
    Run {
        #[arg(long, default_value_t = 3)]
        concurrency: usize,
        #[arg(long, default_value_t = 8)]
        segments: usize,
        #[arg(long)]
        proxy: Option<String>,
    },
    /// 内置 Range 静态源（吞吐基准 / kill -9 恢复演练）
    TestServer {
        file: PathBuf,
        #[arg(default_value_t = 8080)]
        port: u16,
    },
}

fn parse_status(s: &str) -> anyhow::Result<TaskStatus> {
    TaskStatus::parse(s).ok_or_else(|| anyhow::anyhow!("unknown status {s:?}"))
}

fn parse_kind(s: &str) -> anyhow::Result<TaskKind> {
    TaskKind::parse(s).ok_or_else(|| anyhow::anyhow!("unknown kind {s:?}, expected http or hls"))
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n}B")
    } else {
        format!("{v:.1}{}", UNITS[u])
    }
}

fn print_task(t: &Task) {
    let id = t.id.get(..8).unwrap_or(&t.id);
    let total = t.total_bytes.map(human_bytes).unwrap_or_else(|| "-".into());
    println!(
        "{id:<8}  {:<13} {:<4} {:>10}  {}",
        t.status.as_str(),
        t.kind.as_str(),
        total,
        t.url
    );
}

fn parse_common(
    segments: usize,
    max_speed: Option<u64>,
    global_speed: Option<u64>,
    proxy: Option<String>,
    conflict: &str,
    dir: PathBuf,
) -> anyhow::Result<DownloadOptions> {
    Ok(DownloadOptions {
        dir,
        segments: segments.clamp(1, dl_engine::MAX_SEGMENTS),
        task_bps: max_speed,
        global_bps: global_speed,
        proxy,
        conflict: ConflictPolicy::parse(conflict)
            .ok_or_else(|| anyhow::anyhow!("unknown conflict policy {conflict:?}"))?,
        spot_checks: 2,
    })
}

/// 订阅事件并打印目标任务的进度行。
fn spawn_printer(mut rx: tokio::sync::broadcast::Receiver<EngineEvent>, task_id: String) {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(EngineEvent::Progress {
                    task_id: id,
                    downloaded,
                    total,
                    bps,
                    ..
                }) if id == task_id => {
                    let total_str = total.map(human_bytes).unwrap_or_else(|| "?".into());
                    eprint!(
                        "\r  {} / {}  {}/s   ",
                        human_bytes(downloaded),
                        total_str,
                        human_bytes(bps)
                    );
                }
                Ok(EngineEvent::StateChanged {
                    task_id: id,
                    status,
                }) if id == task_id => {
                    eprintln!("\r  status -> {}", status.as_str());
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    });
}

async fn wait_ctrl_c_then_pause(engine: Engine, ids: Vec<String>) {
    if tokio::signal::ctrl_c().await.is_ok() {
        eprintln!("\nreceived Ctrl-C, pausing gracefully (checkpoint + resume later)…");
        for id in ids {
            engine.request_pause(&id);
        }
    }
}

/// 控制类命令优先走 IPC（主程序/GUI 托管引擎时跨进程生效）；
/// 主程序不在 → 返回 None（调用方回退本地引擎）。服务端业务错误 → 报错退出。
async fn ipc_control(
    method: &str,
    params: serde_json::Value,
) -> anyhow::Result<Option<serde_json::Value>> {
    let name = dl_ipc::resolve_pipe_name(None);
    let attempt = async {
        let mut conn = dl_ipc::connect(&name).await?;
        let req = dl_ipc::Request {
            v: dl_ipc::PROTOCOL_VERSION,
            id: 1,
            method: method.to_string(),
            params,
        };
        let payload = serde_json::to_vec(&req)?;
        conn.roundtrip(&payload).await
    };
    match tokio::time::timeout(std::time::Duration::from_secs(3), attempt).await {
        Ok(Ok(resp)) => {
            if resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                Ok(Some(resp))
            } else {
                let msg = resp
                    .pointer("/error/message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown error")
                    .to_string();
                anyhow::bail!("{msg}")
            }
        }
        _ => Ok(None),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let cli = Cli::parse();
    let db_path = cli
        .db
        .unwrap_or_else(|| dl_engine::data_dir().join("data.db"));
    let engine = Engine::open(&db_path).await?;

    match cli.cmd {
        Cmd::Add {
            url,
            kind,
            dir,
            filename,
            mirrors,
            checksum,
            isaac64_seed,
        } => {
            let mut c = CreateTask::of(url);
            c.kind = kind.map(|k| parse_kind(&k)).transpose()?;
            c.dir = dir;
            c.filename = filename;
            c.mirrors = Some(mirrors).filter(|m| !m.is_empty());
            c.checksum = checksum;
            c.postprocess = isaac64_seed.map(|seed| {
                vec![dl_model::PostProcess {
                    algo: dl_model::PostProcessAlgo::Isaac64Xor,
                    seed,
                }]
            });
            let t = engine.create_task(c).await?;
            println!("created {} [{}]", t.id, t.status.as_str());
        }
        Cmd::List { status } => {
            let filter = status.map(|s| parse_status(&s)).transpose()?;
            for t in engine.list_tasks(filter).await? {
                print_task(&t);
            }
        }
        Cmd::Show { id } => {
            let t = engine
                .get_task(&id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("task {id} not found"))?;
            println!("{}", serde_json::to_string_pretty(&t)?);
        }
        Cmd::Pause { id } => {
            // IPC 优先：主程序/GUI 托管引擎时跨进程生效
            if ipc_control("task.pause", serde_json::json!({ "id": id }))
                .await?
                .is_some()
            {
                println!("pause requested (host app will checkpoint)");
            } else if !engine.request_pause(&id) {
                println!("paused {}", engine.pause(&id).await?.id);
            } else {
                println!("pause requested (running task will checkpoint)");
            }
        }
        Cmd::Resume { id } => {
            if ipc_control("task.resume", serde_json::json!({ "id": id }))
                .await?
                .is_some()
            {
                println!("resumed on host app");
            } else {
                println!("resumed {}", engine.resume(&id).await?.id);
            }
        }
        Cmd::Cancel { id } => {
            if ipc_control("task.cancel", serde_json::json!({ "id": id }))
                .await?
                .is_some()
            {
                println!("cancel requested on host app");
            } else if !engine.request_cancel(&id) {
                println!("canceled {}", engine.cancel(&id).await?.id);
            } else {
                println!("cancel requested");
            }
        }
        Cmd::Retry { id } => println!("requeued {}", engine.retry(&id).await?.id),
        Cmd::Rename { id, filename } => {
            println!("renamed {}", engine.rename_task(&id, &filename).await?.id);
        }
        Cmd::Remove { id } => {
            if engine.remove(&id).await? {
                println!("removed {id}");
            } else {
                anyhow::bail!("task {id} not found");
            }
        }
        Cmd::Download {
            id,
            segments,
            max_speed,
            global_speed,
            proxy,
            conflict,
        } => {
            let dir: PathBuf = engine
                .get_task(&id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("task {id} not found"))?
                .dir
                .clone()
                .unwrap_or_else(|| ".".into())
                .into();
            let opts = parse_common(segments, max_speed, global_speed, proxy, &conflict, dir)?;
            let ctrl_engine = engine.clone();
            let ctrl = tokio::spawn(wait_ctrl_c_then_pause(ctrl_engine, vec![id.clone()]));
            spawn_printer(engine.subscribe(), id.clone());
            // 弱网韧性：失败自动重试（与 GUI 一致：仅网络类错误 + 退避，可配次数）
            let max_auto_retry = engine
                .get_setting("max_auto_retry")
                .await
                .unwrap_or(None)
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(3);
            let mut attempts: u32 = 0;
            let t = loop {
                match engine.download(&id, opts.clone()).await {
                    Ok(t) => break t,
                    Err(e) => {
                        attempts += 1;
                        let current_failed = engine
                            .get_task(&id)
                            .await
                            .ok()
                            .flatten()
                            .map(|t| t.status == TaskStatus::Failed)
                            .unwrap_or(false);
                        if !e.kind.is_retryable() || !current_failed || attempts > max_auto_retry {
                            return Err(e.into());
                        }
                        let backoff = std::time::Duration::from_secs(1u64 << attempts.min(4));
                        eprintln!("download failed ({e}); retry {attempts}/{max_auto_retry} in {backoff:?}");
                        tokio::time::sleep(backoff).await;
                        engine.retry(&id).await?;
                    }
                }
            };
            ctrl.abort();
            println!("finished: {} [{}]", t.id, t.status.as_str());
            if t.status != TaskStatus::Done {
                std::process::exit(2);
            }
        }
        Cmd::Run {
            concurrency,
            segments,
            proxy,
        } => {
            let ids: Vec<String> = engine
                .list_tasks(Some(TaskStatus::Queued))
                .await?
                .into_iter()
                .map(|t| t.id)
                .collect();
            let opts = parse_common(segments, None, None, proxy, "rename", ".".into())?;
            let ctrl_engine = engine.clone();
            let ctrl = tokio::spawn(wait_ctrl_c_then_pause(ctrl_engine, ids));
            let results = engine.run_queued(opts, concurrency).await?;
            ctrl.abort();
            let mut failed = 0;
            for r in results {
                match r {
                    Ok(t) if t.status == TaskStatus::Done => {}
                    Ok(t) => {
                        eprintln!("  task {} ended [{}]", &t.id[..8], t.status.as_str());
                        failed += 1;
                    }
                    Err(e) => {
                        eprintln!("  task error: {e}");
                        failed += 1;
                    }
                }
            }
            if failed > 0 {
                std::process::exit(2);
            }
        }
        Cmd::TestServer { file, port } => {
            let server = dl_testserver::Server::start_on(
                port,
                dl_testserver::Source::file(file.clone()),
                dl_testserver::Options::default(),
            )?;
            eprintln!(
                "serving {} on http://{}/file (Ctrl-C to stop)",
                file.display(),
                server.addr
            );
            tokio::signal::ctrl_c().await?;
        }
    }
    Ok(())
}
