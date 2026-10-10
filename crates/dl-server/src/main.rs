//! dl-server：本地 IPC 服务 + Native Messaging shim。
//!
//! 三种运行形态（同一二进制）：
//! 1. `serve`      —— 常驻：持有引擎，经命名管道/UDS 服务 IPC 请求
//! 2. `call`       —— 一次性 IPC 客户端（端到端验证用）
//! 3. NM shim      —— 由 Chrome 按 manifest 拉起（argv[1] 为扩展 origin），
//!    stdio(NM 帧) ↔ 管道(帧) 薄桥；stdout 只允许协议字节，日志一律走 stderr

use dl_ipc::{client_conn, ipc};

use clap::{Parser, Subcommand};
use client_conn::IpcClient;

#[derive(Parser)]
#[command(name = "dl-server", version, about = "Anything Downloader 本地服务")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 启动本地 IPC 服务（Windows 命名管道 / macOS UDS）
    Serve {
        /// IPC 端点名（缺省 ANYTHING_DL_PIPE 环境变量，再缺省平台默认值）
        #[arg(long)]
        pipe_name: Option<String>,
    },
    /// 显式以 NM shim 模式运行（正常由 Chrome 拉起，argv[1] 即扩展 origin）
    NmShim,
    /// 发送一次 IPC 请求，用于端到端验证
    Call {
        method: String,
        /// JSON 参数，如 '{"url":"https://example.com/a.zip"}'
        #[arg(default_value = "")]
        params: String,
        /// IPC 端点名（覆盖环境变量）
        #[arg(long)]
        pipe_name: Option<String>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Native Messaging 协议要求 stdout 纯净：Chrome 以 <exe> <origin> 拉起时，
    // 绕过 clap 直接进入 shim 模式。
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && args[1].starts_with("chrome-extension://") {
        return nm_shim(Some(args[1].clone())).await;
    }

    // 日志只写 stderr：NM 模式下 stdout 是协议通道
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    match Cli::parse().cmd {
        Cmd::Serve { pipe_name } => {
            let engine = dl_engine::Engine::open_default().await?;
            let name = dl_ipc::resolve_pipe_name(pipe_name.as_deref());
            tokio::spawn(async move {
                if let Err(e) = dl_ipc::serve(engine, &name).await {
                    tracing::error!(error = %e, "ipc server exited");
                    std::process::exit(1);
                }
            });
            tracing::info!("dl-server ready, waiting for Ctrl-C to exit");
            tokio::signal::ctrl_c().await?;
            tracing::info!("shutting down");
            Ok(())
        }
        Cmd::NmShim => nm_shim(None).await,
        Cmd::Call {
            method,
            params,
            pipe_name,
        } => {
            let params: serde_json::Value = if params.trim().is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::from_str(&params)?
            };
            let req = ipc::Request {
                v: ipc::PROTOCOL_VERSION,
                id: 1,
                method,
                params,
            };
            let payload = serde_json::to_vec(&req)?;
            let name = dl_ipc::resolve_pipe_name(pipe_name.as_deref());
            let mut conn = dl_ipc::connect(&name).await?;
            let response = conn.roundtrip(&payload).await?;
            println!("{}", serde_json::to_string_pretty(&response)?);
            Ok(())
        }
    }
}

/// NM shim 主循环：每条 NM 消息转发为一次 IPC roundtrip，响应原样回写 NM 帧。
/// 断链（主程序重启等）在下一条消息时自动重连一次。
async fn nm_shim(origin: Option<String>) -> anyhow::Result<()> {
    // NM 模式日志强制 stderr 且默认关到 warn：stdout 是协议通道
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .try_init()
        .ok();

    if let Some(o) = &origin {
        // manifest allowed_origins 已在 Chrome 侧把关，这里兜底校验来源形态
        if !o.starts_with("chrome-extension://") {
            anyhow::bail!("unexpected native messaging caller origin: {o}");
        }
    }
    let pipe = dl_ipc::resolve_pipe_name(None);
    tracing::info!(origin = ?origin, pipe = %pipe, "nm shim started");

    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut conn: Option<IpcClient> = None;
    loop {
        // 请求方向（扩展→宿主）放宽到 MAX_REQUEST_BYTES：文章导出单帧可达数 MB
        let Some(payload) = ipc::read_request(&mut stdin).await? else {
            tracing::info!("nm stdin closed, shim exiting");
            break;
        };

        // 取可用连接，失败回 app_not_running
        if conn.is_none() {
            match dl_ipc::connect(&pipe).await {
                Ok(c) => conn = Some(c),
                Err(e) => {
                    tracing::warn!(error = %e, "app not reachable");
                    write_nm_error(
                        &mut stdout,
                        "app_not_running",
                        "download app is not running",
                    )
                    .await?;
                    continue;
                }
            }
        }

        // 转发；断链重连一次
        let c = conn.as_mut().unwrap();
        match c.roundtrip(&payload).await {
            Ok(resp) => {
                let out = serde_json::to_vec(&resp)?;
                ipc::write_frame(&mut stdout, &out).await?;
            }
            Err(e) => {
                tracing::warn!(error = %e, "ipc roundtrip failed, reconnecting once");
                conn = None;
                match dl_ipc::connect(&pipe).await {
                    Ok(mut c2) => match c2.roundtrip(&payload).await {
                        Ok(resp) => {
                            conn = Some(c2);
                            let out = serde_json::to_vec(&resp)?;
                            ipc::write_frame(&mut stdout, &out).await?;
                        }
                        Err(e2) => {
                            conn = Some(c2);
                            write_nm_error(&mut stdout, "ipc_error", &e2.to_string()).await?;
                        }
                    },
                    Err(e2) => {
                        write_nm_error(&mut stdout, "app_not_running", &e2.to_string()).await?;
                    }
                }
            }
        }
    }
    Ok(())
}

async fn write_nm_error<W: tokio::io::AsyncWrite + Unpin>(
    stdout: &mut W,
    code: &str,
    message: &str,
) -> std::io::Result<()> {
    let resp = ipc::Response::simple_err(0, code, message);
    let out = serde_json::to_vec(&resp)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    ipc::write_frame(stdout, &out).await
}
