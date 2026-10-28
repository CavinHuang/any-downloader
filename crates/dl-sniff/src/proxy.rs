//! CONNECT 代理：默认透传，白名单 SNI 定向 MITM。
//!
//! 连接生命周期：
//! 1. 读 CONNECT host:port → 回 `200 Connection Established`；
//! 2. 预读 ClientHello 解析 SNI（不解密）：
//!    - 命中解密白名单 → 本地 CA 签叶子证书终止 TLS，按 HTTP/1.1 逐请求转发
//!      （reqwest 上游，禁系统代理防回环），响应边回写边捕获；
//!    - 未命中 → 原样转发已读字节 + TCP 双向透传，不解密任何内容。
//! 3. 非 CONNECT（PAC 下罕见的 http:// 绝对形式）→ 直接转发后关闭。
//! MITM 侧响应统一 chunked + Connection: close（每请求一条连接，本地握手
//! 开销可忽略，规避 keep-alive/pipelining 边界）。

use std::sync::Arc;

use anyhow::{anyhow, bail, Result};
use futures_util::StreamExt;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsAcceptor;

use crate::ca::SniffCa;
use crate::capture::{is_media_url, scan_api_json, Capture, KEEP_HEADERS};
use crate::sni::parse_sni;

pub struct ProxyShared {
    pub cfg: crate::SniffConfig,
    pub ca: SniffCa,
    pub capture: Arc<Capture>,
    pub client: reqwest::Client,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn find_head_end(b: &[u8]) -> Option<usize> {
    b.windows(4).position(|w| w == b"\r\n\r\n")
}

/// 在流上读出完整请求头（含结尾 \r\n\r\n），返回 (头字节, 已读入的剩余字节)。
async fn read_head<S: AsyncRead + Unpin>(s: &mut S) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(pos) = find_head_end(&buf) {
            let rest = buf.split_off(pos + 4);
            buf.shrink_to_fit();
            return Ok((buf, rest));
        }
        if buf.len() > 64 * 1024 {
            bail!("请求头超限");
        }
        let n = s.read(&mut chunk).await?;
        if n == 0 {
            bail!("连接在请求头完成前关闭");
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// 已预读字节 + 内层流的拼接流（把吞进缓冲的 ClientHello 还给 TLS acceptor）。
struct PreStream {
    pre: std::io::Cursor<Vec<u8>>,
    inner: TcpStream,
}

impl AsyncRead for PreStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let pos = self.pre.position() as usize;
        let len = self.pre.get_ref().len();
        if pos < len {
            let n = (len - pos).min(buf.remaining());
            buf.put_slice(&self.pre.get_ref()[pos..pos + n]);
            self.pre.set_position((pos + n) as u64);
            return std::task::Poll::Ready(Ok(()));
        }
        std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for PreStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

pub async fn handle_conn(stream: TcpStream, sh: Arc<ProxyShared>) {
    if let Err(e) = handle_conn_inner(stream, &sh).await {
        // 常规的客户端断开不算错误，降为 debug
        tracing::debug!(error = %e, "代理连接结束");
    }
}

async fn handle_conn_inner(mut stream: TcpStream, sh: &Arc<ProxyShared>) -> Result<()> {
    let (head, _rest) = read_head(&mut stream).await?;
    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut req = httparse::Request::new(&mut headers);
    if req.parse(&head).map_err(|e| anyhow!("bad request: {e}"))?.is_partial() {
        bail!("partial request head");
    }
    let method = req.method.unwrap_or("").to_string();
    if method == "CONNECT" {
        let authority = req.path.unwrap_or("").to_string();
        let (host, port) = split_authority(&authority);
        if host.is_empty() {
            bail!("CONNECT 无主机");
        }
        stream.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await?;
        // 预读 ClientHello（SNI）；记录所有经过的主机便于实测校准白名单（M1）
        let mut pre = Vec::with_capacity(2048);
        let mut chunk = [0u8; 8192];
        let sni = loop {
            if let Some(s) = parse_sni(&pre) {
                break Some(s);
            }
            if pre.len() >= 5 && pre[0] != 0x16 {
                break None; // 不是 TLS
            }
            if pre.len() > 32 * 1024 {
                break None;
            }
            let n = stream.read(&mut chunk).await?;
            if n == 0 {
                break None;
            }
            pre.extend_from_slice(&chunk[..n]);
        };
        let decrypt = sni.as_deref().is_some_and(|s| sh.cfg.should_decrypt(s));
        tracing::info!(host = %authority, sni = ?sni, decrypt, "CONNECT");
        if decrypt {
            let sni = sni.unwrap();
            let acceptor = TlsAcceptor::from(sh.ca.leaf_config(&sni)?);
            let tls = acceptor
                .accept(PreStream { pre: std::io::Cursor::new(pre), inner: stream })
                .await
                .map_err(|e| anyhow!("MITM 握手失败: {e}"))?;
            mitm_loop(tls, &sni, sh).await
        } else {
            passthrough(stream, pre, &host, port, sh).await
        }
    } else {
        plain_http(stream, head, _rest, sh).await
    }
}

fn split_authority(a: &str) -> (String, u16) {
    match a.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().unwrap_or(443)),
        None => (a.to_string(), 443),
    }
}

async fn passthrough(
    mut stream: TcpStream,
    pre: Vec<u8>,
    host: &str,
    port: u16,
    sh: &Arc<ProxyShared>,
) -> Result<()> {
    let mut upstream = match sh.cfg.host_map.get(host) {
        Some(addr) => TcpStream::connect(addr).await?,
        None => TcpStream::connect((host, port)).await?,
    };
    upstream.write_all(&pre).await?;
    tokio::io::copy_bidirectional(&mut stream, &mut upstream).await?;
    Ok(())
}

async fn plain_http(mut stream: TcpStream, head: Vec<u8>, _rest: Vec<u8>, sh: &Arc<ProxyShared>) -> Result<()> {
    // absolute-form 请求（http:// 经 PAC 进来）：按原样转发，响应回写
    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut req = httparse::Request::new(&mut headers);
    if req.parse(&head).map_err(|e| anyhow!("bad request: {e}"))?.is_partial() {
        bail!("partial");
    }
    let url = req.path.unwrap_or("").to_string();
    if !url.starts_with("http://") && !url.starts_with("https://") {
        bail!("非绝对形式请求不转发");
    }
    let method = reqwest::Method::from_bytes(req.method.unwrap_or("GET").as_bytes())?;
    let mut rb = sh.client.request(method, &url);
    for h in req.headers.iter() {
        let name = h.name.to_ascii_lowercase();
        if HOP_HEADERS.contains(&name.as_str()) {
            continue;
        }
        rb = rb.header(&name, String::from_utf8_lossy(h.value).trim());
    }
    let resp = rb.send().await?;
    write_response(&mut stream, resp, None).await?;
    Ok(())
}

const HOP_HEADERS: &[&str] = &[
    "connection",
    "proxy-connection",
    "keep-alive",
    "te",
    "trailer",
    "transfer-encoding",
    "content-length",
    "upgrade",
];

/// MITM 后的 HTTP/1.1 请求循环（Connection: close：单请求）
async fn mitm_loop<S: AsyncRead + AsyncWrite + Unpin>(
    mut tls: S,
    sni: &str,
    sh: &Arc<ProxyShared>,
) -> Result<()> {
    let (head, mut pending) = read_head(&mut tls).await?;
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut req = httparse::Request::new(&mut headers);
    if req.parse(&head).map_err(|e| anyhow!("mitm bad request: {e}"))?.is_partial() {
        bail!("partial");
    }
    let method = req.method.unwrap_or("").to_string();
    let host = header_str(req.headers.iter(), "host").unwrap_or(sni).to_string();
    let raw_path = req.path.unwrap_or("/").to_string();
    let url = if raw_path.starts_with("http") {
        raw_path.clone()
    } else {
        format!("https://{host}{raw_path}")
    };

    // 请求体（finder API 为 POST JSON，Chromium 用 content-length）
    let mut body: Option<Vec<u8>> = None;
    if let Some(cl) = header_str(req.headers.iter(), "content-length")
        .and_then(|v| v.parse::<usize>().ok())
    {
        if cl > 8 * 1024 * 1024 {
            bail!("请求体超限");
        }
        let mut b = vec![0u8; cl];
        let take = pending.len().min(cl);
        b[..take].copy_from_slice(&pending[..take]);
        pending.drain(..take);
        if take < cl {
            read_exact_from(&mut tls, &mut b[take..]).await?;
        }
        body = Some(b);
    }

    // 保留头（引擎 allowlist 子集）随捕获条目记录
    let mut keep: std::collections::HashMap<String, String> = Default::default();
    let mut rb = sh
        .client
        .request(reqwest::Method::from_bytes(method.as_bytes())?, &url);
    for h in req.headers.iter() {
        let name = h.name.to_ascii_lowercase();
        if HOP_HEADERS.contains(&name.as_str()) || name == "host" {
            continue;
        }
        let val = String::from_utf8_lossy(h.value).trim().to_string();
        if KEEP_HEADERS.contains(&name.as_str()) {
            keep.insert(name.clone(), val.clone());
        }
        rb = rb.header(&name, val);
    }
    if let Some(b) = body {
        rb = rb.body(b);
    }
    let resp = rb.send().await.map_err(|e| anyhow!("上游请求失败 {url}: {e}"))?;

    // 媒体直链：响应头到达即成条（content-length 供 UI 显示大小）
    if is_media_url(&url) {
        let _ = sh.capture.media(&url, keep.clone(), resp.content_length(), now_ms());
    }
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let scan = if content_type.contains("json") {
        Some((sh.capture.clone(), url.as_str(), keep))
    } else {
        None
    };
    write_response(&mut tls, resp, scan).await?;
    Ok(())
}

/// 回写上游响应：chunked + Connection: close。scan 携带 (capture, url, 保留头)
/// 时缓冲 JSON 响应体做 API 扫描（decode_key/直链/标题）。
async fn write_response<S: AsyncWrite + Unpin>(
    out: &mut S,
    resp: reqwest::Response,
    scan: Option<(Arc<crate::capture::Capture>, &str, std::collections::HashMap<String, String>)>,
) -> Result<()> {
    let status = resp.status();
    let mut head = format!(
        "HTTP/1.1 {} {}\r\n",
        status.as_u16(),
        status.canonical_reason().unwrap_or("")
    );
    for (k, v) in resp.headers() {
        let name = k.as_str().to_ascii_lowercase();
        if matches!(name.as_str(), "content-length" | "transfer-encoding" | "connection" | "keep-alive") {
            continue;
        }
        head.push_str(&format!("{}: {}\r\n", k.as_str(), v.to_str().unwrap_or("")));
    }
    head.push_str("Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n");
    out.write_all(head.as_bytes()).await?;

    let mut scanned: Option<Vec<u8>> = scan.as_ref().map(|_| Vec::new());
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| anyhow!("上游流中断: {e}"))?;
        if let Some(buf) = scanned.as_mut() {
            if buf.len() + chunk.len() <= 8 * 1024 * 1024 {
                buf.extend_from_slice(&chunk);
            }
        }
        if !chunk.is_empty() {
            out.write_all(format!("{:x}\r\n", chunk.len()).as_bytes()).await?;
            out.write_all(&chunk).await?;
            out.write_all(b"\r\n").await?;
        }
    }
    out.write_all(b"0\r\n\r\n").await?;
    out.shutdown().await?;

    if let (Some(buf), Some((cap, _url, keep))) = (scanned, scan) {
        let s = scan_api_json(&String::from_utf8_lossy(&buf));
        if let Some(key) = s.key {
            let at = now_ms();
            cap.note_key(key, at);
            if let Some(mu) = s.media_url {
                if is_media_url(&mu) {
                    let _ = cap.media(&mu, keep, None, at);
                }
            }
        }
    }
    Ok(())
}

async fn read_exact_from<S: AsyncRead + Unpin>(s: &mut S, mut buf: &mut [u8]) -> Result<()> {
    while !buf.is_empty() {
        let n = s.read(buf).await?;
        if n == 0 {
            bail!("eof in body");
        }
        buf = &mut buf[n..];
    }
    Ok(())
}

fn header_str<'a>(mut hs: impl Iterator<Item = &'a httparse::Header<'a>>, name: &str) -> Option<&'a str> {
    hs.find(|h| h.name.eq_ignore_ascii_case(name))
        .and_then(|h| std::str::from_utf8(h.value).ok())
        .map(|s| s.trim())
}
