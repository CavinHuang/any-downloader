//! macOS / Linux Unix domain socket IPC。
//! socket 目录权限 0700；peer credential 校验在 M5 加固阶段落地。

use std::io;
use std::path::{Path, PathBuf};

use dl_engine::Engine;
use tokio::net::UnixListener;

use crate::ipc;

pub fn default_socket_path() -> PathBuf {
    dl_core::data_dir().join("ipc.sock")
}

pub async fn serve(engine: Engine, path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    tracing::info!(path = %path.display(), "uds listening");
    loop {
        let (stream, _) = listener.accept().await?;
        let engine = engine.clone();
        tokio::spawn(async move {
            if let Err(e) = ipc::serve_connection(&engine, stream).await {
                tracing::warn!(error = %e, "ipc connection failed");
            }
        });
    }
}
