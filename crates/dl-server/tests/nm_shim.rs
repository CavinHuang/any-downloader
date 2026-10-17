//! M1.5 验收：扩展推送 URL → 下载成功。
//! 真实浏览器之外的全链路：serve（引擎持有者）+ NM shim（Chrome 拉起形态，
//! argv[1] = origin）+ NM 帧 task.add → 引擎 download → 文件字节一致。

use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use dl_testserver::{Options as ServerOptions, Server, Source};

const ORIGIN: &str = "chrome-extension://jeigecniniligfiigmmanjeiaooiiomc/";

fn nm_frame(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

fn read_nm_frame(r: &mut impl Read) -> std::io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(Some(buf))
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Env {
    db_dir: std::path::PathBuf,
    pipe: String,
}

fn make_env(tag: &str) -> (tempfile::TempDir, Env) {
    let dir = tempfile::tempdir().unwrap();
    // 端点形态随平台：Windows 命名管道；unix 为 tempdir 内 UDS 路径（dir 由调用方持有）
    let pipe = if cfg!(windows) {
        format!(r"\\.\pipe\anything-dl-test-{tag}")
    } else {
        dir.path()
            .join(format!("anything-dl-test-{tag}.sock"))
            .to_string_lossy()
            .into_owned()
    };
    let env = Env {
        db_dir: dir.path().join("data"),
        pipe,
    };
    std::fs::create_dir_all(&env.db_dir).unwrap();
    (dir, env)
}

fn spawn_serve(env: &Env) -> ChildGuard {
    let child = Command::new(env!("CARGO_BIN_EXE_dl-server"))
        .args(["serve"])
        .env("ANYTHING_DL_PIPE", &env.pipe)
        .env("ANYTHING_DL_DATA_DIR", &env.db_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    ChildGuard(child)
}

fn spawn_shim(env: &Env) -> (ChildGuard, ChildStdin, ChildStdout) {
    // 真实 Chrome 拉起形态：<exe> chrome-extension://<id>/（origin 走 argv[1] 检测）
    let mut child = Command::new(env!("CARGO_BIN_EXE_dl-server"))
        .arg(ORIGIN)
        .env("ANYTHING_DL_PIPE", &env.pipe)
        .env("ANYTHING_DL_DATA_DIR", &env.db_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    (ChildGuard(child), stdin, stdout)
}

use std::process::{ChildStdin, ChildStdout};

fn rpc(
    stdin: &mut ChildStdin,
    stdout: &mut ChildStdout,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    let req = serde_json::json!({ "v": 1, "id": 1, "method": method, "params": params });
    stdin
        .write_all(&nm_frame(serde_json::to_vec(&req).unwrap().as_slice()))
        .unwrap();
    stdin.flush().unwrap();
    let resp = read_nm_frame(stdout)
        .unwrap()
        .expect("shim closed without response");
    serde_json::from_slice(&resp).expect("invalid response json")
}

fn wait_serve(stdin: &mut ChildStdin, stdout: &mut ChildStdout) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(Instant::now() < deadline, "serve did not come up in time");
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            rpc(stdin, stdout, "ping", serde_json::Value::Null)
        })) {
            Ok(resp) if resp["ok"] == true => return,
            _ => std::thread::sleep(Duration::from_millis(300)),
        }
    }
}

#[test]
fn extension_push_creates_task_and_downloads() {
    let data: Vec<u8> = (0..256 * 1024u32).map(|i| (i * 7 + 3) as u8).collect();
    let server = Server::start(Source::mem(data.clone()), ServerOptions::default()).unwrap();

    let (_dir, env) = make_env("e2e");
    let _serve = spawn_serve(&env);
    let (shim_guard, mut shim_in, mut shim_out) = spawn_shim(&env);

    // 1. 扩展 ping → pong（popup 存活探测同款）
    wait_serve(&mut shim_in, &mut shim_out);
    let pong = rpc(&mut shim_in, &mut shim_out, "ping", serde_json::Value::Null);
    assert_eq!(pong["ok"], true);

    // 2. 扩展推送嗅探资源（带 Referer/Cookie 透传头）
    let push = rpc(
        &mut shim_in,
        &mut shim_out,
        "task.add",
        serde_json::json!({
            "url": server.url(),
            "headers": {
                "Referer": "https://example.com/page",
                "Cookie": "session=abc123",
                "X-Evil": "not-allowlisted"
            }
        }),
    );
    assert_eq!(push["ok"], true, "push failed: {push}");
    let task_id = push["result"]["id"].as_str().unwrap().to_string();

    // 3. 引擎侧下载（CLI download 的同款路径），确认透传头到达 HTTP 层：
    //    dl-testserver 不校验头，这里校验头确实进了任务记录
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let engine = rt.block_on(async {
        let engine = dl_engine::Engine::open(&env.db_dir.join("data.db"))
            .await
            .unwrap();
        let t = engine.get_task(&task_id).await.unwrap().unwrap();
        let hj = t.headers_json.as_deref().unwrap();
        assert!(hj.contains("session=abc123"), "cookie must persist: {hj}");
        assert!(
            !hj.contains("X-Evil"),
            "non-allowlisted header must be dropped: {hj}"
        );

        let out = tempfile::tempdir().unwrap();
        let opts = dl_engine::DownloadOptions {
            dir: out.path().to_path_buf(),
            segments: 2,
            ..Default::default()
        };
        let done = engine.download(&task_id, opts).await.unwrap();
        assert_eq!(done.status, dl_model::TaskStatus::Done);
        let got = std::fs::read(out.path().join("file")).unwrap();
        assert_eq!(got, data, "downloaded bytes must match source");
        out.keep() // 保住文件到断言后（其实已在断言中读取，keep 仅为语义清晰）
    });
    let _ = engine;

    // 4. 主程序未运行时（杀 serve）push 得到明确错误而非挂死
    drop(_serve);
    std::thread::sleep(Duration::from_millis(200));
    let resp = rpc(&mut shim_in, &mut shim_out, "ping", serde_json::Value::Null);
    assert_eq!(resp["ok"], false);
    assert_eq!(resp["error"]["code"], "app_not_running");

    drop(shim_guard);
}
