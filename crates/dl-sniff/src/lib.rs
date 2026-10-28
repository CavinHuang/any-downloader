//! dl-sniff —— 桌面嗅探服务（docs/DESKTOP-SNIFF.md）。
//!
//! 面向浏览器之外的应用（首期：微信桌面端视频号，由内置 Chromium 进程
//! WeChatAppEx 渲染、遵循系统代理/PAC）。本地 CONNECT 代理默认对一切域名
//! TCP 透传；仅对解密白名单 SNI（视频号 finder/channels 域）做 MITM，捕获
//! 媒体直链与 `decode_key`（配对成引擎 `postprocess: isaac64-xor` 种子，
//! 解密管线与浏览器 channels 适配器共用）。
//!
//! 安全边界（见 docs/DESKTOP-SNIFF.md）：
//! - 解密范围 = 白名单 SNI，其余域名不解密任何字节；
//! - PAC 只把 qq.com 系域名路由进本代理（宿主负责写 PAC 与还原）；
//! - 根证书仅 CurrentUser 级、可一键卸载（宿主负责安装/卸载）。

pub mod ca;
pub mod capture;
pub mod proxy;
pub mod sni;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use tokio::net::TcpListener;
use tokio::sync::watch;

pub use capture::{Capture, SniffedItem};

pub const DEFAULT_PORT: u16 = 9463;

#[derive(Clone, Debug)]
pub struct SniffConfig {
    /// 监听端口（仅 127.0.0.1）
    pub port: u16,
    /// 精确匹配的解密主机
    pub decrypt_exact: Vec<String>,
    /// 后缀匹配的解密域（如 `.video.qq.com`）
    pub decrypt_suffix: Vec<String>,
    /// 测试用：host → 上游地址覆盖
    pub host_map: HashMap<String, SocketAddr>,
    /// 测试用：不校验上游证书
    pub insecure_upstream: bool,
    /// 条目环形缓冲容量
    pub buffer: usize,
    /// 数据目录（CA 存 `<data_dir>/sniff/`）
    pub data_dir: PathBuf,
}

impl SniffConfig {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            port: DEFAULT_PORT,
            // M1 实测校准点：微信版本更新后域名可能变化，docs/DESKTOP-SNIFF.md
            // 附排查方法（看代理日志的 CONNECT/SNI 行）
            decrypt_exact: vec![
                "channels.weixin.qq.com".into(),
                "finder.video.qq.com".into(),
            ],
            decrypt_suffix: vec![".video.qq.com".into()],
            host_map: HashMap::new(),
            insecure_upstream: false,
            buffer: 50,
            data_dir,
        }
    }

    pub fn should_decrypt(&self, sni: &str) -> bool {
        self.decrypt_exact
            .iter()
            .any(|h| h.eq_ignore_ascii_case(sni))
            || self
                .decrypt_suffix
                .iter()
                .any(|s| sni.len() > s.len() && sni.to_ascii_lowercase().ends_with(s))
    }
}

pub struct SniffService {
    /// 实际监听端口（cfg.port=0 时为系统分配）
    pub port: u16,
    pub shutdown: watch::Sender<bool>,
    pub join: tokio::task::JoinHandle<()>,
}

impl SniffService {
    pub fn stop(self) {
        let _ = self.shutdown.send(true);
        self.join.abort();
    }
}

/// 启动嗅探代理。返回后即可接收连接；条目经 [`Capture::subscribe`] 订阅。
pub async fn start(cfg: SniffConfig, capture: Arc<Capture>) -> Result<SniffService> {
    let ca = ca::SniffCa::load_or_create(&cfg.data_dir.join("sniff"))?;
    // 上游客户端：显式 no_proxy（宿主可能已写 PAC/系统代理，防回环）；
    // host_map（测试/联调用）经 resolve 生效——保持 SNI/Host 为原域名
    let mut cb = reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(std::time::Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(300));
    for (host, addr) in &cfg.host_map {
        cb = cb.resolve(host.as_str(), *addr);
    }
    if cfg.insecure_upstream {
        cb = cb.danger_accept_invalid_certs(true);
    }
    let client = cb.build()?;
    let shared = Arc::new(proxy::ProxyShared {
        cfg: cfg.clone(),
        ca,
        capture,
        client,
    });

    let listener = TcpListener::bind(("127.0.0.1", cfg.port)).await?;
    let port = listener.local_addr()?.port();
    tracing::info!(port, "sniff 代理监听 127.0.0.1");
    let (tx, mut rx) = watch::channel(false);
    let join = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = rx.changed() => break,
                acc = listener.accept() => match acc {
                    Ok((stream, _)) => {
                        let sh = shared.clone();
                        tokio::spawn(proxy::handle_conn(stream, sh));
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "sniff accept 失败");
                        break;
                    }
                },
            }
        }
    });
    Ok(SniffService {
        port,
        shutdown: tx,
        join,
    })
}
