//! 局域网远程添加：手机扫码/输地址，把链接推到桌面端。
//! 三个路由的极简 HTTP 服务（tokio 异步实现，跑在宿主的长生命周期
//! runtime 上——下载驱动必须存活于请求返回之后，禁止一次性 runtime）。
//! 配对令牌必带，端口默认 9462；无令牌的一切请求 404。

use std::net::IpAddr;
use std::time::Duration;

use dl_engine::Engine;
use dl_model::CreateTask;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

pub const DEFAULT_PORT: u16 = 9462;
const MAX_BODY: usize = 64 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(10);

/// 运行中的远程添加服务：stop() 即关闭。
pub struct RemoteServer {
    pub port: u16,
    pub token: String,
    pub urls: Vec<String>,
    shutdown: watch::Sender<bool>,
}

impl RemoteServer {
    /// 绑定 0.0.0.0:port，接受循环 spawn 到当前 tokio runtime
    /// （调用方须在长生命周期 runtime 内，如 tauri::async_runtime）。
    pub async fn start(engine: Engine, port: u16, token: String) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("0.0.0.0", port)).await?;
        let (tx, rx) = watch::channel(false);
        let urls = lan_urls(port, &token);
        tokio::spawn(accept_loop(listener, engine, token.clone(), rx));
        Ok(Self {
            port,
            token,
            urls,
            shutdown: tx,
        })
    }

    pub fn stop(self) {
        let _ = self.shutdown.send(true);
    }
}

async fn accept_loop(
    listener: TcpListener,
    engine: Engine,
    token: String,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            _ = shutdown.changed() => return,
            r = listener.accept() => match r {
                Ok((stream, _addr)) => {
                    let engine = engine.clone();
                    let token = token.clone();
                    tokio::spawn(handle_conn(stream, engine, token));
                }
                Err(e) => {
                    tracing::warn!(error = %e, "remote accept failed");
                    return;
                }
            },
        }
    }
}

struct Req {
    method: String,
    path: String,
    query: String,
    body: Vec<u8>,
}

async fn read_request(stream: &mut TcpStream) -> std::io::Result<Option<Req>> {
    let mut buf = Vec::with_capacity(512);
    // 头部：读到 \r\n\r\n 为止（含上限保护）
    let mut byte = [0u8; 1];
    loop {
        let n = match tokio::time::timeout(IO_TIMEOUT, stream.read(&mut byte)).await {
            Err(_) => return Ok(None),
            Ok(Ok(0)) => return Ok(None),
            Ok(Ok(n)) => n,
            Ok(Err(_)) => return Ok(None),
        };
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") || buf.ends_with(b"\n\n") {
            break;
        }
        if buf.len() > 32 * 1024 {
            return Ok(None);
        }
        let _ = n;
    }
    let head = String::from_utf8_lossy(&buf);
    let mut lines = head.split("\n");
    let first = lines.next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("").to_ascii_uppercase();
    let target = parts.next().unwrap_or("/").to_string();
    if method.is_empty() {
        return Ok(None);
    }
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };
    let mut content_length = 0usize;
    for l in lines {
        let t = l.trim();
        if let Some(v) = t
            .strip_prefix("Content-Length:")
            .or_else(|| t.strip_prefix("content-length:"))
        {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }
    if content_length > MAX_BODY {
        return Ok(None);
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0
        && tokio::time::timeout(IO_TIMEOUT, stream.read_exact(&mut body))
            .await
            .is_err()
    {
        return Ok(None);
    }
    Ok(Some(Req {
        method,
        path,
        query,
        body,
    }))
}

async fn write_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await
}

async fn handle_conn(mut stream: TcpStream, engine: Engine, token: String) {
    let req = match read_request(&mut stream).await {
        Ok(Some(r)) => r,
        _ => return,
    };
    let ok_token = query_param(&req.query, "k") == Some(token.as_str());
    let result = match (req.method.as_str(), req.path.as_str(), ok_token) {
        ("GET", "/", true) => {
            write_response(
                &mut stream,
                "200 OK",
                "text/html",
                page_html(&token).as_bytes(),
            )
            .await
        }
        ("POST", "/api/add", true) => {
            let resp = handle_add(&engine, &req.body).await;
            write_response(&mut stream, "200 OK", "application/json", resp.as_bytes()).await
        }
        ("GET", "/favicon.ico", _) => {
            write_response(&mut stream, "204 No Content", "text/plain", b"").await
        }
        _ => write_response(&mut stream, "404 Not Found", "text/plain", b"not found").await,
    };
    if let Err(e) = result {
        tracing::debug!(error = %e, "remote conn write failed");
    }
}

fn query_param<'q>(query: &'q str, key: &str) -> Option<&'q str> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then_some(v)
    })
}

async fn handle_add(engine: &Engine, body: &[u8]) -> String {
    let parsed: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return format!(r#"{{"ok":false,"error":"invalid json: {e}"}}"#),
    };
    let Some(urls) = parsed["urls"].as_array() else {
        return r#"{"ok":false,"error":"missing urls"}"#.into();
    };
    let mut added = 0usize;
    let mut failed = 0usize;
    for u in urls {
        let Some(url) = u.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        match engine.create_task(CreateTask::of(url)).await {
            Ok(t) => {
                // start 只入队/驱动；并发上限与排队由引擎准入控制
                let _ = engine.start(&t.id).await;
                added += 1;
            }
            Err(_) => failed += 1,
        }
    }
    format!(r#"{{"ok":true,"added":{added},"failed":{failed}}}"#)
}

// ---- 配对信息 ----

/// 本机局域网地址（UDP connect 探测默认路由出口）。
pub fn lan_ip() -> Option<IpAddr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:80").ok()?;
    Some(s.local_addr().ok()?.ip())
}

pub fn lan_urls(port: u16, token: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(ip) = lan_ip() {
        out.push(format!("http://{ip}:{port}/?k={token}"));
    }
    out.push(format!("http://127.0.0.1:{port}/?k={token}"));
    out
}

/// 配对二维码（内联 SVG，设置页直接 <img src=data:>）。
pub fn qr_svg(text: &str) -> Option<String> {
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    Some(
        code.render::<qrcode::render::svg::Color>()
            .dark_color(qrcode::render::svg::Color("#293c34"))
            .light_color(qrcode::render::svg::Color("#fbfaf6"))
            .quiet_zone(true)
            .build(),
    )
}

fn page_html(token: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>anything. 远程添加</title>
<style>
  :root {{ color-scheme: light dark; }}
  body {{ margin: 0; font: 16px/1.7 -apple-system,"Segoe UI","Microsoft YaHei UI",system-ui,sans-serif;
         background: #f4f3ec; color: #293c34; min-height: 100vh; display: flex; align-items: center; justify-content: center; }}
  @media (prefers-color-scheme: dark) {{ body {{ background: #1d2621; color: #e3e7d9; }} }}
  .card {{ width: min(440px, 92vw); background: #fbfaf6; border: 1px solid #dedfd5; border-radius: 16px; padding: 26px 22px; box-shadow: 0 18px 70px #23312624; }}
  @media (prefers-color-scheme: dark) {{ .card {{ background: #242e27; border-color: #3b463c; }} }}
  .brand {{ font: 600 22px Georgia,serif; }}
  .brand i {{ font-style: normal; background: #e4edb5; color: #344e3c; border-radius: 4px; padding: 0 3px; }}
  p.sub {{ color: #687263; font-size: 13.5px; margin: 8px 0 18px; }}
  textarea {{ width: 100%; min-height: 130px; box-sizing: border-box; padding: 12px; border: 1px solid #dedfd5;
              border-radius: 8px; font: inherit; background: #fff; color: inherit; }}
  @media (prefers-color-scheme: dark) {{ textarea {{ background: #1d2621; border-color: #3b463c; }} }}
  button {{ width: 100%; margin-top: 14px; padding: 13px; border: 0; border-radius: 8px; background: #344e3c;
            color: #f4f3ec; font: 600 16px inherit; font-family: inherit; cursor: pointer; }}
  button:disabled {{ opacity: .5; }}
  #msg {{ margin-top: 12px; font-size: 14px; min-height: 22px; }}
</style>
</head>
<body>
<div class="card">
  <div class="brand">anything<i>.</i></div>
  <p class="sub">粘贴链接（每行一个），推送到桌面端开始下载。</p>
  <textarea id="u" placeholder="https://…"></textarea>
  <button id="b" onclick="add()">推送到桌面端</button>
  <div id="msg"></div>
</div>
<script>
async function add() {{
  const u = document.getElementById("u").value.trim();
  const m = document.getElementById("msg");
  if (!u) {{ m.textContent = "先粘贴至少一个链接"; return; }}
  const b = document.getElementById("b");
  b.disabled = true; m.textContent = "推送中…";
  try {{
    const r = await fetch("/api/add?k={token}", {{
      method: "POST",
      headers: {{ "Content-Type": "application/json" }},
      body: JSON.stringify({{ urls: u.split(/\n+/).map(s => s.trim()).filter(Boolean) }})
    }});
    const j = await r.json();
    m.textContent = j.ok ? `已推送 ${{j.added}} 个链接${{j.failed ? "，失败 " + j.failed : ""}}` : "失败：" + (j.error || r.status);
    if (j.ok) document.getElementById("u").value = "";
  }} catch (e) {{
    m.textContent = "网络错误：" + e;
  }} finally {{
    b.disabled = false;
  }}
}}
</script>
</body>
</html>"#
    )
}
