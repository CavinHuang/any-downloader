//! 本地 IPC 能力域：帧协议、管道/UDS 服务、客户端连接。
//! 常驻形态（GUI 或 dl-server serve）经 [[serve]] 托管引擎；NM shim 与 call 走客户端。

pub mod client_conn;
pub mod ipc;

#[cfg(windows)]
pub mod pipe;
#[cfg(unix)]
pub mod uds;

pub use client_conn::{connect, default_pipe_name, resolve_pipe_name, IpcClient};
pub use ipc::{read_frame, write_frame, Request, Response, MAX_FRAME_BYTES, PROTOCOL_VERSION};

/// 以默认端点托管引擎 IPC（阻塞当前 task；由宿主 spawn）。
pub async fn serve(engine: dl_engine::Engine, endpoint: &str) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        pipe::serve(engine, endpoint).await
    }
    #[cfg(unix)]
    {
        uds::serve(engine, std::path::Path::new(endpoint)).await
    }
}
