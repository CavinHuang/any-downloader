//! Windows 命名管道 IPC。
//! M0 骨架：默认 DACL（创建者所有）。显式 DACL 限当前用户 + SYSTEM 在 M5 加固阶段落地。

use tokio::net::windows::named_pipe::ServerOptions;

use crate::ipc;
use dl_engine::Engine;

pub async fn serve(engine: Engine, name: &str) -> std::io::Result<()> {
    // 标准模式：建一个实例等连接；连上后立刻建下一个实例，再处理当前连接，
    // 保证等待队列里始终有可用实例。
    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(name)?;
    tracing::info!(name = %name, "named pipe listening");
    loop {
        // 客户端在 connect 完成前消失等竞态会让 connect 返回 Err——
        // 绝不能让单个实例失效拖垮整个管道服务：重建实例继续
        if let Err(e) = server.connect().await {
            tracing::warn!(error = %e, "pipe connect race; recreating instance");
            match ServerOptions::new().create(name) {
                Ok(next) => {
                    server = next;
                    continue;
                }
                Err(e) => {
                    tracing::error!(error = %e, "pipe instance recreate failed; ipc server exiting");
                    return Err(e);
                }
            }
        }
        let current = server;
        server = ServerOptions::new().create(name)?;
        let engine = engine.clone();
        tokio::spawn(async move {
            if let Err(e) = ipc::serve_connection(&engine, current).await {
                tracing::warn!(error = %e, "ipc connection failed");
            }
        });
    }
}
