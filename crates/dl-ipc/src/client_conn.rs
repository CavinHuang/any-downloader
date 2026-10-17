//! IPC 客户端连接：dl-server 的 call 子命令与 nm-shim 共用。
//! 管道名解析：显式参数 > ANYTHING_DL_PIPE 环境变量 > 平台默认值
//! （shim 由 Chrome 拉起、无法收参数，只能靠环境变量覆盖——测试用）。

#[cfg(windows)]
mod imp {
    use tokio::net::windows::named_pipe::ClientOptions;

    pub struct IpcClient(pub tokio::net::windows::named_pipe::NamedPipeClient);

    pub async fn connect(name: &str) -> std::io::Result<IpcClient> {
        Ok(IpcClient(ClientOptions::new().open(name)?))
    }
}

#[cfg(unix)]
mod imp {
    pub struct IpcClient(pub tokio::net::UnixStream);

    pub async fn connect(path: &str) -> std::io::Result<IpcClient> {
        Ok(IpcClient(tokio::net::UnixStream::connect(path).await?))
    }
}

pub use imp::{connect, IpcClient};

/// 平台默认端点：Windows 命名管道；macOS/Linux 走数据目录下的 UDS 路径。
#[cfg(windows)]
pub fn default_pipe_name() -> String {
    r"\\.\pipe\anything-dl".to_string()
}

#[cfg(unix)]
pub fn default_pipe_name() -> String {
    super::uds::default_socket_path()
        .to_string_lossy()
        .into_owned()
}

/// 解析 IPC 端点名称（参数 > 环境变量 > 平台默认）。
pub fn resolve_pipe_name(explicit: Option<&str>) -> String {
    explicit
        .map(|s| s.to_string())
        .or_else(|| std::env::var("ANYTHING_DL_PIPE").ok())
        .unwrap_or_else(default_pipe_name)
}

impl IpcClient {
    /// 发送一帧并读取一帧响应。请求-响应严格串行，与 ipc::serve_connection 匹配。
    pub async fn roundtrip(&mut self, payload: &[u8]) -> std::io::Result<serde_json::Value> {
        super::ipc::write_frame(&mut self.0, payload).await?;
        let resp = super::ipc::read_frame(&mut self.0)
            .await?
            .ok_or_else(|| std::io::Error::other("server closed connection without a response"))?;
        Ok(serde_json::from_slice(&resp)?)
    }
}
