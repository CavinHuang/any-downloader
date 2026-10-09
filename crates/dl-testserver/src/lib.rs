//! 极简静态 HTTP 源：Range / ETag / If-Range 支持，thread-per-connection。
//! 用途：集成测试、M1 吞吐验收与 kill -9 恢复演练（DESIGN §11 测试源）。
//! 仅测试与验收工具，不在产品请求路径上。

use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

const CHUNK: usize = 64 * 1024;

#[derive(Clone)]
pub enum Source {
    Mem(Arc<RwLock<Vec<u8>>>),
    File(PathBuf),
    /// 按路径的多文件源（HLS 测试：清单 + 分片各自成 URI，键含前导 /）。
    MemMap(Arc<std::collections::HashMap<String, Vec<u8>>>),
}

impl Source {
    pub fn mem(data: Vec<u8>) -> Source {
        Source::Mem(Arc::new(RwLock::new(data)))
    }

    pub fn file(path: PathBuf) -> Source {
        Source::File(path)
    }

    pub fn mem_map(map: std::collections::HashMap<String, Vec<u8>>) -> Source {
        Source::MemMap(Arc::new(map))
    }

    fn len_of(&self, path: &str) -> u64 {
        match self {
            Source::Mem(v) => v.read().unwrap().len() as u64,
            Source::File(p) => std::fs::metadata(p).map(|m| m.len()).unwrap_or(0),
            Source::MemMap(m) => m.get(path).map(|v| v.len() as u64).unwrap_or(0),
        }
    }

    fn etag_of(&self, path: &str) -> String {
        match self {
            Source::Mem(v) => {
                let v = v.read().unwrap();
                let h = xxhash_rust::xxh64::xxh64(&v, 0);
                format!("\"{h:x}-{}\"", v.len())
            }
            Source::File(p) => {
                let meta = std::fs::metadata(p).ok();
                let len = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                let mtime = meta
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                format!("\"{mtime:x}-{len}\"")
            }
            Source::MemMap(m) => {
                let v = m.get(path).cloned().unwrap_or_default();
                let h = xxhash_rust::xxh64::xxh64(&v, 0);
                format!("\"{h:x}-{}\"", v.len())
            }
        }
    }

    fn read_range_of(&self, path: &str, start: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Source::Mem(v) => {
                let v = v.read().unwrap();
                let s = (start as usize).min(v.len());
                let n = buf.len().min(v.len() - s);
                buf[..n].copy_from_slice(&v[s..s + n]);
                Ok(n)
            }
            Source::File(p) => {
                let mut f = std::fs::File::open(p)?;
                f.seek(SeekFrom::Start(start))?;
                let mut total = 0;
                while total < buf.len() {
                    let n = f.read(&mut buf[total..])?;
                    if n == 0 {
                        break;
                    }
                    total += n;
                }
                Ok(total)
            }
            Source::MemMap(m) => {
                let v = m.get(path).cloned().unwrap_or_default();
                let s = (start as usize).min(v.len());
                let n = buf.len().min(v.len() - s);
                buf[..n].copy_from_slice(&v[s..s + n]);
                Ok(n)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// 第一个 Range 请求只写这么多字节就掐断连接（模拟中途断线，测内部重试）。
    pub kill_first_range_after: Option<usize>,
    /// 忽略 Range 头，恒返回 200（测单连接降级路径）。
    pub ignore_range: bool,
    /// 首个 Range 响应谎报 Content-Range 起点（+1000，测客户端校验重试）。
    pub wrong_range_once: bool,
    /// 仅探测请求（单字节 Range）不给 Range（恒 200 + Accept-Ranges）——
    /// 模拟「探测丢失 Range 但真实下载支持」的 CDN 场景。
    pub plain_probe_only: bool,
}

struct State {
    source: Source,
    opts: Options,
    killed: AtomicBool,
    range_lied: AtomicBool,
    plain_probe_hit: AtomicBool,
    shutdown: AtomicBool,
}

pub struct Server {
    pub addr: SocketAddr,
    state: Arc<State>,
}

impl Server {
    pub fn start_mem(data: Vec<u8>) -> std::io::Result<Server> {
        Self::start(Source::mem(data), Options::default())
    }

    pub fn start(source: Source, opts: Options) -> std::io::Result<Server> {
        Self::start_on(0, source, opts)
    }

    pub fn start_on(port: u16, source: Source, opts: Options) -> std::io::Result<Server> {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        let addr = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let state = Arc::new(State {
            source,
            opts,
            killed: AtomicBool::new(false),
            range_lied: AtomicBool::new(false),
            plain_probe_hit: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
        });
        let s = Arc::clone(&state);
        std::thread::Builder::new()
            .name("dl-testsrv".into())
            .spawn(move || loop {
                if s.shutdown.load(Ordering::Relaxed) {
                    break;
                }
                match listener.accept() {
                    Ok((stream, _)) => {
                        let s = Arc::clone(&s);
                        std::thread::spawn(move || {
                            if let Err(e) = handle_conn(stream, &s) {
                                eprintln!("[testserver] conn error: {e}");
                            }
                        });
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            })?;
        Ok(Server { addr, state })
    }

    pub fn url(&self) -> String {
        format!("http://{}/file", self.addr)
    }

    pub fn etag(&self) -> String {
        self.state.source.etag_of("/file")
    }

    /// 替换内容（etag 随之变化），用于内容变更→If-Range 检测→重启测试。
    pub fn set_data(&self, data: Vec<u8>) {
        if let Source::Mem(v) = &self.state.source {
            *v.write().unwrap() = data;
        }
    }

    pub fn stop(&self) {
        self.state.shutdown.store(true, Ordering::Relaxed);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

fn handle_conn(mut stream: TcpStream, state: &State) -> std::io::Result<()> {
    // Windows 上 accept 出的 socket 继承 listener 的非阻塞标志，必须关掉
    stream.set_nonblocking(false).ok();
    stream.set_nodelay(true).ok();
    let head = read_head(&mut stream)?;
    let Some(head) = head else { return Ok(()) };
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let path = request_line.split_whitespace().nth(1).unwrap_or("");
    let mut range: Option<(u64, Option<u64>)> = None;
    let mut if_range: Option<String> = None;
    for line in lines {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let k = k.trim().to_ascii_lowercase();
        let v = v.trim();
        if k == "range" {
            range = parse_range(v);
        } else if k == "if-range" {
            if_range = Some(v.to_string());
        }
    }
    match &state.source {
        Source::Mem(_) | Source::File(_) => {
            if path != "/file" {
                return respond_error(&mut stream, 404, "not found");
            }
        }
        Source::MemMap(m) => {
            if !m.contains_key(path) {
                return respond_error(&mut stream, 404, "not found");
            }
        }
    }

    let etag = state.source.etag_of(path);
    let len = state.source.len_of(path);

    // If-Range 不匹配 → 实体已变 → 必须整体 200
    let range = match &if_range {
        Some(ir) if ir != &etag => None,
        _ => range,
    };
    let ignore = state.opts.ignore_range
        || (state.opts.plain_probe_only
            && matches!(range, Some((0, Some(0))))
            && state.plain_probe_hit.swap(true, Ordering::Relaxed));
    let range = if ignore { None } else { range };

    match range {
        None => {
            let head = format!(
                "HTTP/1.1 200 OK\r\nETag: {etag}\r\nAccept-Ranges: bytes\r\nContent-Type: application/octet-stream\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(head.as_bytes())?;
            let mut buf = vec![0u8; CHUNK];
            let mut off = 0u64;
            while off < len {
                let n = state.source.read_range_of(path, off, &mut buf)?;
                if n == 0 {
                    break;
                }
                stream.write_all(&buf[..n])?;
                off += n as u64;
            }
            Ok(())
        }
        Some((a, b)) => {
            let end = b
                .map(|e| e.min(len.saturating_sub(1)))
                .unwrap_or(len.saturating_sub(1));
            if a >= len || a > end {
                let head = format!(
                    "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{len}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                return stream.write_all(head.as_bytes());
            }
            let slice_len = end - a + 1;
            // 首个 Range 响应谎报 Content-Range 起点（+1000，仅头部；body 仍从 a 读）
            let lie = state
                .opts
                .wrong_range_once
                .then(|| state.range_lied.swap(true, Ordering::Relaxed))
                .eq(&Some(false));
            let cr_start = if lie { a + 1000 } else { a };
            let head = format!(
                "HTTP/1.1 206 Partial Content\r\nETag: {etag}\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {cr_start}-{end}/{len}\r\nContent-Type: application/octet-stream\r\nContent-Length: {slice_len}\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(head.as_bytes())?;

            // 模拟中途断线：首个 Range 请求只写 N 字节就掐断。
            // 单字节 range 是探测请求（bytes=0-0），不当断线目标。
            let killable = slice_len > 1;
            let kill_after = if killable && !state.killed.swap(true, Ordering::Relaxed) {
                state.opts.kill_first_range_after
            } else {
                None
            };
            let limit = kill_after
                .map(|k| k.min(slice_len as usize) as u64)
                .unwrap_or(slice_len);

            let mut buf = vec![0u8; CHUNK];
            let mut sent: u64 = 0;
            while sent < limit {
                let want = (limit - sent).min(CHUNK as u64) as usize;
                let n = state
                    .source
                    .read_range_of(path, a + sent, &mut buf[..want])?;
                if n == 0 {
                    break;
                }
                stream.write_all(&buf[..n])?;
                sent += n as u64;
            }
            // kill 场景：不写完就断（连 Connection: close 收尾都不给）
            Ok(())
        }
    }
}

fn respond_error(stream: &mut TcpStream, code: u16, msg: &str) -> std::io::Result<()> {
    let body = format!("{msg}\n");
    let head = format!(
        "HTTP/1.1 {code} {msg}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(head.as_bytes())
}

fn read_head(stream: &mut TcpStream) -> std::io::Result<Option<String>> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_head_end(&buf) {
            let head = String::from_utf8_lossy(&buf[..pos]).to_string();
            return Ok(Some(head));
        }
        if buf.len() > 32 * 1024 {
            return Ok(None);
        }
    }
}

fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// "bytes=a-b" / "bytes=a-" → Some((a, Some(b) | None))
fn parse_range(v: &str) -> Option<(u64, Option<u64>)> {
    let spec = v.strip_prefix("bytes=")?;
    let (a, b) = spec.split_once('-')?;
    let a = a.trim().parse::<u64>().ok()?;
    let b = b.trim().parse::<u64>().ok();
    Some((a, b))
}
