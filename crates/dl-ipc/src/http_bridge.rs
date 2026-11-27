//! 扩展双通道 HTTP 桥（127.0.0.1:14371）：
//! 与 Native Messaging 并行工作，提供免注册/跨浏览器的本地 RPC 与嗅探上报通道
//! （参照 Ghost-Downloader-3 本地服务设计 + Chrome Extension Origin 校验）。
//!
//! 安全边界：
//! - 仅监听回环地址 `127.0.0.1:<port>`；
//! - 若请求带 `Origin` 头（浏览器强制附加且页面脚本不可伪造），仅放行
//!   `chrome-extension://` 与 `moz-extension://`，网页发起的任何跨站请求一律 `403`；
//! - 可选配置 `X-ADL-Token` 校验。

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dl_engine::Engine;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, watch, RwLock};

use crate::ipc::{dispatch, Request, Response, MAX_REQUEST_BYTES};

pub const DEFAULT_BRIDGE_PORT: u16 = 14371;
const MAX_SNIFF_ITEMS: usize = 100;
const IO_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_HEAD_BYTES: usize = 64 * 1024;

static NEXT_SNIFF_ID: AtomicU64 = AtomicU64::new(1);

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn default_kind() -> String {
    "other".to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BrowserSniffVariant {
    pub url: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub bandwidth: Option<u64>,
    #[serde(default)]
    pub resolution: Option<String>,
    #[serde(default)]
    pub height: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BrowserSniffItem {
    #[serde(default)]
    pub id: String,
    pub url: String,
    #[serde(default)]
    pub tab_url: Option<String>,
    #[serde(default)]
    pub page_title: Option<String>,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default)]
    pub mime: Option<String>,
    #[serde(default)]
    pub bytes: Option<u64>,
    #[serde(default)]
    pub duration_sec: Option<f64>,
    #[serde(default)]
    pub variants: Vec<BrowserSniffVariant>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub at: i64,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ReportPayload {
    Batch { items: Vec<BrowserSniffItem> },
    Single(Box<BrowserSniffItem>),
}

#[derive(Clone)]
pub struct BridgeState {
    pub engine: Engine,
    pub port: u16,
    token: Arc<RwLock<Option<String>>>,
    items: Arc<Mutex<VecDeque<BrowserSniffItem>>>,
    last_seen_at: Arc<AtomicI64>,
    tx: broadcast::Sender<BrowserSniffItem>,
}

impl BridgeState {
    pub fn new(engine: Engine, port: u16, token: Option<String>) -> Self {
        let (tx, _) = broadcast::channel(64);
        let clean_token = token
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        Self {
            engine,
            port,
            token: Arc::new(RwLock::new(clean_token)),
            items: Arc::new(Mutex::new(VecDeque::new())),
            last_seen_at: Arc::new(AtomicI64::new(0)),
            tx,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<BrowserSniffItem> {
        self.tx.subscribe()
    }

    pub fn items(&self) -> Vec<BrowserSniffItem> {
        self.items.lock().unwrap().iter().cloned().collect()
    }

    pub fn get_item(&self, id: &str) -> Option<BrowserSniffItem> {
        self.items
            .lock()
            .unwrap()
            .iter()
            .find(|it| it.id == id)
            .cloned()
    }

    pub fn clear(&self) {
        self.items.lock().unwrap().clear();
    }

    pub fn last_seen_at(&self) -> Option<i64> {
        let v = self.last_seen_at.load(Ordering::Relaxed);
        (v > 0).then_some(v)
    }

    pub async fn has_token(&self) -> bool {
        self.token.read().await.is_some()
    }

    pub fn upsert_item(&self, mut item: BrowserSniffItem) {
        if item.url.trim().is_empty() {
            return;
        }
        if item.at <= 0 {
            item.at = now_ms();
        }
        let mut q = self.items.lock().unwrap();
        if let Some(old) = q.iter_mut().find(|x| x.url == item.url) {
            if item.id.is_empty() {
                item.id = old.id.clone();
            }
            if item.filename.is_none() {
                item.filename = old.filename.take();
            }
            if item.page_title.is_none() {
                item.page_title = old.page_title.take();
            }
            if item.bytes.is_none() {
                item.bytes = old.bytes;
            }
            if item.duration_sec.is_none() {
                item.duration_sec = old.duration_sec;
            }
            if item.variants.is_empty() && !old.variants.is_empty() {
                item.variants = std::mem::take(&mut old.variants);
            }
            if item.headers.is_empty() && !old.headers.is_empty() {
                item.headers = std::mem::take(&mut old.headers);
            }
            *old = item.clone();
        } else {
            if item.id.is_empty() {
                let seq = NEXT_SNIFF_ID.fetch_add(1, Ordering::Relaxed);
                item.id = format!("br-{}-{seq}", now_ms());
            }
            q.push_front(item.clone());
            while q.len() > MAX_SNIFF_ITEMS {
                q.pop_back();
            }
        }
        drop(q);
        let _ = self.tx.send(item);
    }

    fn touch_seen(&self) {
        self.last_seen_at.store(now_ms(), Ordering::Relaxed);
    }
}

pub struct HttpBridgeServer {
    pub state: Arc<BridgeState>,
    shutdown: watch::Sender<bool>,
}

impl HttpBridgeServer {
    pub async fn start(engine: Engine, port: u16, token: Option<String>) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port)).await?;
        let actual_port = listener.local_addr()?.port();
        let state = Arc::new(BridgeState::new(engine, actual_port, token));
        let (tx, rx) = watch::channel(false);
        tokio::spawn(accept_loop(listener, state.clone(), rx));
        Ok(Self {
            state,
            shutdown: tx,
        })
    }

    pub fn stop(self) {
        let _ = self.shutdown.send(true);
    }
}

async fn accept_loop(
    listener: TcpListener,
    state: Arc<BridgeState>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            _ = shutdown.changed() => return,
            r = listener.accept() => match r {
                Ok((stream, _)) => {
                    let st = state.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_conn(stream, st).await {
                            tracing::debug!(error = %e, "http bridge connection ended");
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!(error = %e, "http bridge accept failed");
                    return;
                }
            }
        }
    }
}

struct HttpReq {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

enum ReadErr {
    TimeoutOrEof,
    TooLarge,
}

async fn read_http(stream: &mut TcpStream) -> Result<HttpReq, ReadErr> {
    let mut buf = Vec::with_capacity(1024);
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        if buf.len() > MAX_HEAD_BYTES {
            return Err(ReadErr::TooLarge);
        }
        let n = match tokio::time::timeout(IO_TIMEOUT, stream.read(&mut tmp)).await {
            Ok(Ok(n)) if n > 0 => n,
            _ => return Err(ReadErr::TimeoutOrEof),
        };
        buf.extend_from_slice(&tmp[..n]);
    };

    let mut body = buf.split_off(head_end + 4);
    let head_str = String::from_utf8_lossy(&buf);
    let mut lines = head_str.split("\r\n");
    let first = lines.next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("").to_ascii_uppercase();
    let raw_target = parts.next().unwrap_or("/");
    let path = raw_target.split('?').next().unwrap_or("/").to_string();
    if method.is_empty() {
        return Err(ReadErr::TimeoutOrEof);
    }

    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let content_length: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if content_length > MAX_REQUEST_BYTES as usize {
        return Err(ReadErr::TooLarge);
    }
    if body.len() > content_length {
        body.truncate(content_length);
    }
    while body.len() < content_length {
        let remaining = content_length - body.len();
        let mut chunk = vec![0u8; remaining.min(64 * 1024)];
        let n = match tokio::time::timeout(IO_TIMEOUT, stream.read(&mut chunk)).await {
            Ok(Ok(n)) if n > 0 => n,
            _ => return Err(ReadErr::TimeoutOrEof),
        };
        body.extend_from_slice(&chunk[..n]);
    }

    Ok(HttpReq {
        method,
        path,
        headers,
        body,
    })
}

fn is_allowed_extension_origin(origin: &str) -> bool {
    origin.starts_with("chrome-extension://") || origin.starts_with("moz-extension://")
}

async fn write_http(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    allow_origin: Option<&str>,
    body: &[u8],
) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(origin) = allow_origin {
        head.push_str(&format!(
            "Access-Control-Allow-Origin: {origin}\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type, X-ADL-Token\r\nVary: Origin\r\n"
        ));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).await?;
    if !body.is_empty() {
        stream.write_all(body).await?;
    }
    stream.flush().await
}

async fn handle_conn(mut stream: TcpStream, state: Arc<BridgeState>) -> std::io::Result<()> {
    let req = match read_http(&mut stream).await {
        Ok(r) => r,
        Err(ReadErr::TooLarge) => {
            return write_http(
                &mut stream,
                "413 Payload Too Large",
                "text/plain",
                None,
                b"too large",
            )
            .await;
        }
        Err(ReadErr::TimeoutOrEof) => return Ok(()),
    };

    let origin = req.headers.get("origin").map(String::as_str);
    if let Some(o) = origin {
        if !is_allowed_extension_origin(o) {
            return write_http(
                &mut stream,
                "403 Forbidden",
                "application/json",
                None,
                br#"{"ok":false,"error":"forbidden_origin"}"#,
            )
            .await;
        }
    }
    let allow_origin = origin;

    if req.method == "OPTIONS" {
        return write_http(
            &mut stream,
            "204 No Content",
            "text/plain",
            allow_origin,
            b"",
        )
        .await;
    }

    if let Some(expected) = state.token.read().await.as_deref() {
        let got = req.headers.get("x-adl-token").map(String::as_str);
        if got != Some(expected) {
            return write_http(
                &mut stream,
                "401 Unauthorized",
                "application/json",
                allow_origin,
                br#"{"ok":false,"error":"unauthorized"}"#,
            )
            .await;
        }
    }

    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/health") => {
            state.touch_seen();
            let body = serde_json::json!({
                "ok": true,
                "protocol": crate::ipc::PROTOCOL_VERSION,
                "port": state.port,
            })
            .to_string();
            write_http(
                &mut stream,
                "200 OK",
                "application/json",
                allow_origin,
                body.as_bytes(),
            )
            .await
        }
        ("POST", "/rpc") => {
            state.touch_seen();
            let resp = match serde_json::from_slice::<Request>(&req.body) {
                Ok(rpc_req) => dispatch(&state.engine, rpc_req).await,
                Err(e) => Response::simple_err(0, "bad_request", e.to_string()),
            };
            let out = serde_json::to_vec(&resp).unwrap_or_else(|_| b"{}".to_vec());
            write_http(
                &mut stream,
                "200 OK",
                "application/json",
                allow_origin,
                &out,
            )
            .await
        }
        ("POST", "/sniff/report") => {
            state.touch_seen();
            match serde_json::from_slice::<ReportPayload>(&req.body) {
                Ok(ReportPayload::Batch { items }) => {
                    let count = items.len();
                    for it in items {
                        state.upsert_item(it);
                    }
                    let out = format!(r#"{{"ok":true,"count":{count}}}"#);
                    write_http(
                        &mut stream,
                        "200 OK",
                        "application/json",
                        allow_origin,
                        out.as_bytes(),
                    )
                    .await
                }
                Ok(ReportPayload::Single(it)) => {
                    state.upsert_item(*it);
                    write_http(
                        &mut stream,
                        "200 OK",
                        "application/json",
                        allow_origin,
                        br#"{"ok":true,"count":1}"#,
                    )
                    .await
                }
                Err(e) => {
                    let out =
                        serde_json::json!({ "ok": false, "error": e.to_string() }).to_string();
                    write_http(
                        &mut stream,
                        "400 Bad Request",
                        "application/json",
                        allow_origin,
                        out.as_bytes(),
                    )
                    .await
                }
            }
        }
        _ => {
            write_http(
                &mut stream,
                "404 Not Found",
                "application/json",
                allow_origin,
                br#"{"ok":false,"error":"not_found"}"#,
            )
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn send_raw(port: u16, raw: &[u8]) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        s.write_all(raw).await.unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf).to_string()
    }

    #[tokio::test]
    async fn bridge_blocks_web_origin_and_routes_rpc_and_sniff() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(&dir.path().join("bridge.db")).await.unwrap();
        let srv = HttpBridgeServer::start(engine, 0, Some("secret-123".into()))
            .await
            .unwrap();
        let port = srv.state.port;

        // 1. 网页 Origin 被 403 拦截
        let evil = send_raw(
            port,
            b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: https://evil.example\r\nContent-Length: 0\r\n\r\n",
        )
        .await;
        assert!(evil.starts_with("HTTP/1.1 403 Forbidden"));

        // 2. 扩展 Origin 的 OPTIONS 预检放行
        let preflight = send_raw(
            port,
            b"OPTIONS /rpc HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: chrome-extension://abcdef\r\nContent-Length: 0\r\n\r\n",
        )
        .await;
        assert!(preflight.starts_with("HTTP/1.1 204 No Content"));
        assert!(preflight.contains("Access-Control-Allow-Origin: chrome-extension://abcdef"));

        // 3. 缺 token → 401
        let unauth = send_raw(
            port,
            b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: chrome-extension://abcdef\r\nContent-Length: 0\r\n\r\n",
        )
        .await;
        assert!(unauth.starts_with("HTTP/1.1 401 Unauthorized"));

        // 4. POST /rpc ping + task.add
        let rpc_body = br#"{"v":1,"id":42,"method":"task.add","params":{"url":"https://example.com/demo.zip"}}"#;
        let rpc_req = format!(
            "POST /rpc HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: chrome-extension://abcdef\r\nX-ADL-Token: secret-123\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            rpc_body.len(),
            String::from_utf8_lossy(rpc_body)
        );
        let rpc_resp = send_raw(port, rpc_req.as_bytes()).await;
        assert!(rpc_resp.starts_with("HTTP/1.1 200 OK"));
        assert!(rpc_resp.contains(r#""ok":true"#));
        assert!(rpc_resp.contains("https://example.com/demo.zip"));

        // 5. POST /sniff/report 去重与更新
        let rep_body = br#"{"url":"https://cdn.example.com/v.m3u8","kind":"hls","page_title":"Demo Page","filename":"Demo Page.mp4","variants":[{"url":"https://cdn.example.com/1080.m3u8","resolution":"1920x1080","height":1080}]}"#;
        let rep_req = format!(
            "POST /sniff/report HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: chrome-extension://abcdef\r\nX-ADL-Token: secret-123\r\nContent-Length: {}\r\n\r\n{}",
            rep_body.len(),
            String::from_utf8_lossy(rep_body)
        );
        let rep_resp = send_raw(port, rep_req.as_bytes()).await;
        assert!(rep_resp.starts_with("HTTP/1.1 200 OK"));
        assert_eq!(srv.state.items().len(), 1);
        assert_eq!(srv.state.items()[0].variants.len(), 1);
        assert!(srv.state.last_seen_at().is_some());

        srv.stop();
    }
}
