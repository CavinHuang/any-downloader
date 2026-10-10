//! 帧协议：4 字节 LE 长度 + JSON 载荷。
//! 上限 1 MiB 与 Native Messaging 的 host→扩展单条上限对齐（DESIGN §6.1），
//! 未来 NM shim 直接复用同一套编解码。

use dl_engine::Engine;
use dl_error::{DownloadError, Res};
use dl_model::{CreateContentTask, CreateTask, Task, TaskStatus};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: u32 = 1024 * 1024;
/// 请求方向（扩展→宿主）帧上限。Chrome NM 该方向协议上限 4 GB；默认 1 MiB
/// 是 host→扩展方向的对齐值。文章导出（task.add_content）单帧携带 HTML 全文
/// 可达数 MB，请求读按此上限放行（dl-engine::Engine::MAX_CONTENT_BYTES 兜底）。
pub const MAX_REQUEST_BYTES: u32 = 32 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub v: u32,
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub v: u32,
    pub id: u64,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ResponseError>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ResponseError {
    pub code: String,
    pub i18n_key: String,
    pub message: String,
}

impl Response {
    pub fn ok(id: u64, result: serde_json::Value) -> Self {
        Response {
            v: PROTOCOL_VERSION,
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: u64, e: &DownloadError) -> Self {
        Response {
            v: PROTOCOL_VERSION,
            id,
            ok: false,
            result: None,
            error: Some(ResponseError {
                code: e.kind.code().to_string(),
                i18n_key: e.kind.i18n_key(),
                message: e.message.clone(),
            }),
        }
    }

    pub fn simple_err(id: u64, code: &str, message: impl Into<String>) -> Self {
        Response {
            v: PROTOCOL_VERSION,
            id,
            ok: false,
            result: None,
            error: Some(ResponseError {
                code: code.to_string(),
                i18n_key: format!("error.{code}"),
                message: message.into(),
            }),
        }
    }
}

/// 读取一帧；对端正常关闭返回 None。上限 MAX_FRAME_BYTES（host→扩展方向）。
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<Option<Vec<u8>>> {
    read_frame_capped(r, MAX_FRAME_BYTES).await
}

/// 读取请求方向的一帧（扩展→宿主 / shim→管道），上限 MAX_REQUEST_BYTES。
pub async fn read_request<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<Option<Vec<u8>>> {
    read_frame_capped(r, MAX_REQUEST_BYTES).await
}

async fn read_frame_capped<R: AsyncRead + Unpin>(
    r: &mut R,
    max: u32,
) -> std::io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len_buf);
    if len > max {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame exceeds limit",
        ));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf).await?;
    Ok(Some(buf))
}

pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, payload: &[u8]) -> std::io::Result<()> {
    let mut framed = Vec::with_capacity(4 + payload.len());
    framed.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    framed.extend_from_slice(payload);
    w.write_all(&framed).await?;
    w.flush().await
}

/// 单连接服务循环：读请求 → dispatch → 写响应，直到对端关闭。
pub(crate) async fn serve_connection<S>(engine: &Engine, mut stream: S) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    while let Some(payload) = read_request(&mut stream).await? {
        let response = match serde_json::from_slice::<Request>(&payload) {
            Ok(req) => dispatch(engine, req).await,
            Err(e) => Response::simple_err(0, "bad_request", e.to_string()),
        };
        let out = serde_json::to_vec(&response)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        write_frame(&mut stream, &out).await?;
    }
    Ok(())
}

#[derive(Deserialize)]
struct IdParams {
    id: String,
}

#[derive(Deserialize)]
struct SettingParams {
    key: String,
}

#[derive(Deserialize)]
struct ListParams {
    #[serde(default)]
    status: Option<String>,
}

fn task_json(t: Task) -> serde_json::Value {
    serde_json::to_value(t).unwrap_or(serde_json::Value::Null)
}

fn task_response(id: u64, r: Res<Task>) -> Response {
    match r {
        Ok(t) => Response::ok(id, task_json(t)),
        Err(e) => Response::err(id, &e),
    }
}

fn bad_params(id: u64, e: serde_json::Error) -> Response {
    Response::simple_err(id, "bad_params", e.to_string())
}

pub(crate) async fn dispatch(engine: &Engine, req: Request) -> Response {
    if req.v != PROTOCOL_VERSION {
        return Response::simple_err(
            req.id,
            "protocol_version",
            format!("expected protocol v{PROTOCOL_VERSION}"),
        );
    }

    match req.method.as_str() {
        "ping" => Response::ok(
            req.id,
            serde_json::json!({ "pong": true, "protocol": PROTOCOL_VERSION }),
        ),
        "task.add" => match serde_json::from_value::<CreateTask>(req.params) {
            Ok(c) => task_response(req.id, engine.create_task(c).await),
            Err(e) => bad_params(req.id, e),
        },
        "task.add_content" => match serde_json::from_value::<CreateContentTask>(req.params) {
            Ok(c) => task_response(req.id, engine.create_content_task(c).await),
            Err(e) => bad_params(req.id, e),
        },
        "settings.get" => {
            // 白名单暴露：扩展只需要下载根目录与归类开关（文章合集目录拼接用），
            // 其余设置（代理/限速/密钥类）不经 NM 通道外泄
            let Some(p) = serde_json::from_value::<SettingParams>(req.params).ok() else {
                return Response::simple_err(req.id, "bad_params", "expected {\"key\": ...}");
            };
            match p.key.as_str() {
                "default_dir" | "auto_categorize" => match engine.get_setting(&p.key).await {
                    Ok(v) => Response::ok(req.id, serde_json::json!({ "value": v })),
                    Err(e) => Response::err(req.id, &e),
                },
                other => Response::simple_err(
                    req.id,
                    "bad_params",
                    format!("setting {other:?} is not exposed over IPC"),
                ),
            }
        }
        "task.get" => match serde_json::from_value::<IdParams>(req.params) {
            Ok(p) => match engine.get_task(&p.id).await {
                Ok(Some(t)) => Response::ok(req.id, task_json(t)),
                Ok(None) => {
                    Response::simple_err(req.id, "not_found", format!("task {} not found", p.id))
                }
                Err(e) => Response::err(req.id, &e),
            },
            Err(e) => bad_params(req.id, e),
        },
        "task.list" => {
            // 允许省略参数：params 为 null 时按空对象处理
            let params = if req.params.is_null() {
                serde_json::json!({})
            } else {
                req.params
            };
            match serde_json::from_value::<ListParams>(params) {
                Ok(p) => {
                    let status = match p.status.as_deref().map(TaskStatus::parse) {
                        Some(Some(s)) => Some(s),
                        Some(None) => {
                            return Response::simple_err(
                                req.id,
                                "bad_params",
                                format!("unknown status {:?}", p.status.unwrap_or_default()),
                            )
                        }
                        None => None,
                    };
                    match engine.list_tasks(status).await {
                        Ok(list) => Response::ok(
                            req.id,
                            serde_json::to_value(list).unwrap_or(serde_json::Value::Null),
                        ),
                        Err(e) => Response::err(req.id, &e),
                    }
                }
                Err(e) => bad_params(req.id, e),
            }
        }
        "task.open" => {
            // 打开成品：文件存在则打开文件，否则打开目录（跨平台 shell 调用）
            let Some(p) = serde_json::from_value::<IdParams>(req.params).ok() else {
                return Response::simple_err(req.id, "bad_params", "expected {\"id\": ...}");
            };
            let task = match engine.get_task(&p.id).await {
                Ok(Some(t)) => t,
                Ok(None) => {
                    return Response::simple_err(
                        req.id,
                        "not_found",
                        format!("task {} not found", p.id),
                    )
                }
                Err(e) => return Response::err(req.id, &e),
            };
            let dir = task.dir.unwrap_or_else(|| ".".into());
            let name = task.filename.unwrap_or_default();
            let full = std::path::Path::new(&dir).join(&name);
            let target = if full.exists() {
                full
            } else {
                std::path::PathBuf::from(&dir)
            };
            // open crate = ShellExecuteW(Win)/open(mac)/xdg-open(linux)：
            // 原生处理含空格/中文的路径。旧实现 explorer /select,"<带空格路径>"
            // 被 explorer 自身的命令行解析规则破坏 → 「找不到应用程序」。
            let _ = open::that_in_background(&target);
            Response::ok(req.id, serde_json::json!({ "opened": true }))
        }
        "task.start" => {
            // 由宿主进程（GUI / serve）执行下载：进度经宿主事件流分发给 UI
            let Some(p) = serde_json::from_value::<IdParams>(req.params).ok() else {
                return Response::simple_err(req.id, "bad_params", "expected {\"id\": ...}");
            };
            let task = match engine.get_task(&p.id).await {
                Ok(Some(t)) => t,
                Ok(None) => {
                    return Response::simple_err(
                        req.id,
                        "not_found",
                        format!("task {} not found", p.id),
                    )
                }
                Err(e) => return Response::err(req.id, &e),
            };
            let opts = dl_engine::DownloadOptions {
                dir: task
                    .dir
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| std::path::PathBuf::from(".")),
                ..engine
                    .download_options_from_settings()
                    .await
                    .unwrap_or_default()
            };
            let engine2 = engine.clone();
            tokio::spawn(async move {
                if let Err(e) = engine2.download(&p.id, opts).await {
                    tracing::warn!(task = %p.id, error = %e, "ipc-started download failed");
                }
            });
            Response::ok(req.id, serde_json::json!({ "started": true }))
        }
        method @ ("task.pause" | "task.resume" | "task.cancel" | "task.retry" | "task.remove") => {
            let Some(p) = serde_json::from_value::<IdParams>(req.params).ok() else {
                return Response::simple_err(req.id, "bad_params", "expected {\"id\": ...}");
            };
            if method == "task.remove" {
                return match engine.remove(&p.id).await {
                    Ok(true) => Response::ok(req.id, serde_json::json!({ "removed": true })),
                    Ok(false) => Response::simple_err(
                        req.id,
                        "not_found",
                        format!("task {} not found", p.id),
                    ),
                    Err(e) => Response::err(req.id, &e),
                };
            }
            let r = match method {
                "task.pause" => engine.pause(&p.id).await,
                "task.resume" => engine.resume(&p.id).await,
                "task.cancel" => engine.cancel(&p.id).await,
                _ => engine.retry(&p.id).await,
            };
            task_response(req.id, r)
        }
        other => Response::simple_err(req.id, "unknown_method", format!("unknown method {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip_bounds() {
        let payload = br#"{"v":1,"id":1,"method":"ping"}"#;
        let mut framed = Vec::new();
        framed.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        framed.extend_from_slice(payload);
        assert_eq!(framed.len(), 4 + payload.len());
    }

    #[tokio::test]
    async fn dispatch_rejects_wrong_protocol_version() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(&dir.path().join("t.db")).await.unwrap();
        let resp = dispatch(
            &engine,
            Request {
                v: 99,
                id: 7,
                method: "ping".into(),
                params: serde_json::Value::Null,
            },
        )
        .await;
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().code, "protocol_version");
    }

    #[tokio::test]
    async fn dispatch_ping_and_task_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(&dir.path().join("t.db")).await.unwrap();

        let req = |method: &str, params: serde_json::Value| Request {
            v: PROTOCOL_VERSION,
            id: 1,
            method: method.into(),
            params,
        };

        let resp = dispatch(&engine, req("ping", serde_json::Value::Null)).await;
        assert!(resp.ok);

        let resp = dispatch(
            &engine,
            req(
                "task.add",
                serde_json::json!({ "url": "https://example.com/a.zip" }),
            ),
        )
        .await;
        assert!(resp.ok);
        let id = resp.result.unwrap()["id"].as_str().unwrap().to_string();

        let resp = dispatch(&engine, req("task.pause", serde_json::json!({ "id": id }))).await;
        assert!(!resp.ok, "pause on queued must be rejected");
        assert_eq!(resp.error.unwrap().code, "invalid_transition");

        let resp = dispatch(
            &engine,
            req("task.list", serde_json::json!({ "status": "queued" })),
        )
        .await;
        assert!(resp.ok);
        assert_eq!(resp.result.unwrap().as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn dispatch_list_accepts_omitted_params() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(&dir.path().join("t.db")).await.unwrap();
        let resp = dispatch(
            &engine,
            Request {
                v: PROTOCOL_VERSION,
                id: 1,
                method: "task.list".into(),
                params: serde_json::Value::Null,
            },
        )
        .await;
        assert!(resp.ok, "task.list without params must succeed");
        assert_eq!(resp.result.unwrap().as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn dispatch_task_add_content_writes_done_task() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(&dir.path().join("t.db")).await.unwrap();
        // 内容任务落盘目录必须显式指定：缺省会写到系统下载目录（真实用户目录）
        let out = dir.path().join("out");
        engine
            .set_setting("default_dir", out.display().to_string().as_str())
            .await
            .unwrap();

        let resp = dispatch(
            &engine,
            Request {
                v: PROTOCOL_VERSION,
                id: 9,
                method: "task.add_content".into(),
                params: serde_json::json!({
                    "filename": "文章.md",
                    "mime": "text/markdown;charset=utf-8",
                    "content": "# 标题\n\n正文",
                    "source_url": "https://juejin.cn/post/1"
                }),
            },
        )
        .await;
        assert!(resp.ok, "task.add_content must succeed: {:?}", resp.error);
        let task = resp.result.unwrap();
        assert_eq!(task["kind"], "content");
        assert_eq!(task["status"], "done");
        assert_eq!(task["filename"], "文章.md");
        assert_eq!(task["url"], "https://juejin.cn/post/1", "原文链接进 url");

        // 缺 content 字段 → bad_params
        let resp = dispatch(
            &engine,
            Request {
                v: PROTOCOL_VERSION,
                id: 10,
                method: "task.add_content".into(),
                params: serde_json::json!({ "filename": "x.md" }),
            },
        )
        .await;
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().code, "bad_params");
    }

    #[tokio::test]
    async fn dispatch_settings_get_whitelist() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(&dir.path().join("t.db")).await.unwrap();
        engine
            .set_setting("default_dir", "D:\\Downloads")
            .await
            .unwrap();

        let call = |id: u64, key: &str| {
            dispatch(
                &engine,
                Request {
                    v: PROTOCOL_VERSION,
                    id,
                    method: "settings.get".into(),
                    params: serde_json::json!({ "key": key }),
                },
            )
        };
        let resp = call(1, "default_dir").await;
        assert!(resp.ok);
        assert_eq!(resp.result.unwrap()["value"], "D:\\Downloads");

        // 未设置过的白名单键 → null 值（非错误）
        let resp = call(2, "auto_categorize").await;
        assert!(resp.ok);
        assert!(resp.result.unwrap()["value"].is_null());

        // 白名单外 → 拒绝（代理/限速/密钥类设置不经 NM 外泄）
        let resp = call(3, "proxy").await;
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().code, "bad_params");
    }

    #[tokio::test]
    async fn read_request_accepts_large_frames_up_to_cap() {
        // 请求方向上限 MAX_REQUEST_BYTES（文章导出单帧可达数 MB）；
        // 默认 read_frame 仍按 1 MiB 拒绝（host→扩展方向语义）。
        let big = vec![0u8; MAX_REQUEST_BYTES as usize];
        let mut framed = Vec::new();
        framed.extend_from_slice(&(big.len() as u32).to_le_bytes());
        framed.extend_from_slice(&big);
        let got = read_request(&mut std::io::Cursor::new(framed))
            .await
            .unwrap();
        assert_eq!(got.unwrap().len(), MAX_REQUEST_BYTES as usize);

        let over = vec![0u8; MAX_REQUEST_BYTES as usize + 1];
        let mut framed2 = Vec::new();
        framed2.extend_from_slice(&(over.len() as u32).to_le_bytes());
        framed2.extend_from_slice(&over);
        let err = read_request(&mut std::io::Cursor::new(framed2))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);

        let mut framed3 = Vec::new();
        framed3.extend_from_slice(&(big.len() as u32).to_le_bytes());
        framed3.extend_from_slice(&big);
        let err3 = read_frame(&mut std::io::Cursor::new(framed3))
            .await
            .unwrap_err();
        assert_eq!(err3.kind(), std::io::ErrorKind::InvalidData);
    }
}
