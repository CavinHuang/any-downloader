//! M1 集成测试：分段下载、断线内部重试、journal 崩溃恢复（暂停续传）、
//! 内容变更重启、无 Range 降级、限速生效（DESIGN §11 测试策略）。

use std::time::{Duration, Instant};

use dl_engine::{DownloadOptions, Engine};
use dl_model::TaskStatus;
use dl_model::{CreateTask, Task};
use dl_testserver::{Options as ServerOptions, Server, Source};

fn init_log() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("debug")),
        )
        .with_writer(std::io::stderr)
        .try_init();
}

fn make_data(size: usize, seed: u8) -> Vec<u8> {
    (0..size)
        .map(|i| (i as u8).wrapping_mul(seed).wrapping_add(seed))
        .collect()
}

async fn new_engine(dir: &tempfile::TempDir) -> Engine {
    Engine::open(&dir.path().join("data.db")).await.unwrap()
}

async fn add_task(engine: &Engine, url: &str) -> Task {
    engine.create_task(CreateTask::of(url)).await.unwrap()
}

async fn wait_progress(engine: &Engine, id: &str, min_bytes: u64) {
    let mut rx = engine.subscribe();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        assert!(Instant::now() < deadline, "timed out waiting for progress");
        match rx.recv().await {
            Ok(dl_engine::EngineEvent::Progress {
                task_id,
                downloaded,
                ..
            }) if task_id == id => {
                if downloaded >= min_bytes {
                    return;
                }
            }
            Ok(_) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(e) => panic!("event stream closed: {e}"),
        }
    }
}

async fn wait_status(engine: &Engine, id: &str, target: TaskStatus) {
    // 先查一次（可能已落库），再订阅事件等待
    if let Ok(Some(t)) = engine.get_task(id).await {
        if t.status == target {
            return;
        }
    }
    let mut rx = engine.subscribe();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {target:?}"
        );
        if let Ok(Some(t)) = engine.get_task(id).await {
            if t.status == target {
                return;
            }
        }
        match rx.recv().await {
            Ok(dl_engine::EngineEvent::StateChanged { task_id, status }) if task_id == id => {
                if status == target {
                    return;
                }
            }
            Ok(_) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(e) => panic!("event stream closed: {e}"),
        }
    }
}

fn out_opts(dir: &tempfile::TempDir, segments: usize) -> DownloadOptions {
    DownloadOptions {
        dir: dir.path().to_path_buf(),
        segments,
        ..Default::default()
    }
}

#[tokio::test]
async fn segmented_download_matches_source() {
    init_log();
    let data = make_data(8 * 1024 * 1024, 7);
    let server = Server::start(Source::mem(data.clone()), ServerOptions::default()).unwrap();
    let dbtmp = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = new_engine(&dbtmp).await;
    let t = add_task(&engine, &server.url()).await;

    let done = engine.download(&t.id, out_opts(&out, 4)).await.unwrap();
    assert_eq!(done.status, TaskStatus::Done);
    assert_eq!(done.total_bytes, Some(data.len() as u64));
    assert_eq!(done.error, None);

    let got = std::fs::read(out.path().join("file")).unwrap();
    assert_eq!(got.len(), data.len());
    assert!(got == data, "downloaded bytes must match source");

    // journal 已清理，无 .dlpart 残留
    assert!(!dbtmp
        .path()
        .join("dl-journals")
        .join(format!("{}.dlctrl", t.id))
        .exists());
    assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 1);
}

#[tokio::test]
async fn resumes_after_killed_connection() {
    init_log();
    let data = make_data(4 * 1024 * 1024, 31);
    let opts = ServerOptions {
        kill_first_range_after: Some(64 * 1024),
        ..Default::default()
    };
    let server = Server::start(Source::mem(data.clone()), opts).unwrap();
    let dbtmp = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = new_engine(&dbtmp).await;
    let t = add_task(&engine, &server.url()).await;

    let done = engine.download(&t.id, out_opts(&out, 2)).await.unwrap();
    assert_eq!(done.status, TaskStatus::Done);
    let got = std::fs::read(out.path().join("file")).unwrap();
    assert!(got == data, "bytes must match after internal retry");
}

#[tokio::test]
async fn pause_then_resume_via_journal() {
    init_log();
    let data = make_data(8 * 1024 * 1024, 9);
    let server = Server::start(Source::mem(data.clone()), ServerOptions::default()).unwrap();
    let dbtmp = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = new_engine(&dbtmp).await;
    let t = add_task(&engine, &server.url()).await;

    // 限速拖慢下载，确保在完成前能暂停
    let slow = DownloadOptions {
        task_bps: Some(2 * 1024 * 1024),
        ..out_opts(&out, 4)
    };
    let id = t.id.clone();
    let e2 = engine.clone();
    let handle = tokio::spawn(async move { e2.download(&id, slow).await });

    wait_progress(&engine, &t.id, 1024 * 1024).await;
    assert!(engine.request_pause(&t.id), "task should be running");

    let paused = handle.await.unwrap().unwrap();
    assert_eq!(paused.status, TaskStatus::Paused);

    let journal = dbtmp
        .path()
        .join("dl-journals")
        .join(format!("{}.dlctrl", t.id));
    assert!(journal.exists(), "journal must survive pause");

    // 续传：不限速，journal 恢复
    let resumed = engine.download(&t.id, out_opts(&out, 4)).await.unwrap();
    assert_eq!(resumed.status, TaskStatus::Done);
    assert_eq!(resumed.total_bytes, Some(data.len() as u64));
    let got = std::fs::read(out.path().join("file")).unwrap();
    assert!(got == data, "resumed bytes must match source");
    assert!(!journal.exists(), "journal must be removed on completion");
}

/// 回归：经 Engine::pause（CLI/IPC 使用的路径，此前只改 DB 不通知 driver）
/// 暂停运行中的下载 → driver 必须真正停下并落 Paused → 再次 download 可续传。
#[tokio::test]
async fn engine_pause_stops_running_driver_then_resume() {
    let data = make_data(8 * 1024 * 1024, 11);
    let server = Server::start(Source::mem(data.clone()), ServerOptions::default()).unwrap();
    let dbtmp = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = new_engine(&dbtmp).await;
    let t = add_task(&engine, &server.url()).await;

    let slow = DownloadOptions {
        task_bps: Some(2 * 1024 * 1024),
        ..out_opts(&out, 4)
    };
    let id = t.id.clone();
    let e2 = engine.clone();
    let handle = tokio::spawn(async move { e2.download(&id, slow).await });

    wait_progress(&engine, &t.id, 1024 * 1024).await;
    // 关键：走 Engine::pause（此前只改 DB、不通知 driver 的缺陷路径）
    // pause() 对运行中任务返回当前状态（Downloading），driver 异步落 Paused
    let _ = engine.pause(&t.id).await;
    wait_status(&engine, &t.id, TaskStatus::Paused).await;

    let done = handle.await.unwrap().unwrap();
    assert_eq!(done.status, TaskStatus::Paused, "driver 必须真正停下");

    // 立即重新开始（旧缺陷：此处报 already running / 无响应）
    let resumed = engine.download(&t.id, out_opts(&out, 4)).await.unwrap();
    assert_eq!(resumed.status, TaskStatus::Done);
    let got = std::fs::read(out.path().join("file")).unwrap();
    assert!(got == data);
}

#[tokio::test]
async fn content_change_restarts_fresh() {
    init_log();
    let data = make_data(8 * 1024 * 1024, 21);
    let server = Server::start(Source::mem(data.clone()), ServerOptions::default()).unwrap();
    let dbtmp = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = new_engine(&dbtmp).await;
    let t = add_task(&engine, &server.url()).await;

    let slow = DownloadOptions {
        task_bps: Some(2 * 1024 * 1024),
        ..out_opts(&out, 4)
    };
    let id = t.id.clone();
    let e2 = engine.clone();
    let handle = tokio::spawn(async move { e2.download(&id, slow).await });
    wait_progress(&engine, &t.id, 1024 * 1024).await;
    engine.request_pause(&t.id);
    let paused = handle.await.unwrap().unwrap();
    assert_eq!(paused.status, TaskStatus::Paused);

    // 内容变更 → ETag 变化 → If-Range/validator 失配 → 放弃进度整体重下
    let new_data = make_data(8 * 1024 * 1024, 77);
    server.set_data(new_data.clone());
    let resumed = engine.download(&t.id, out_opts(&out, 4)).await.unwrap();
    assert_eq!(resumed.status, TaskStatus::Done);
    let got = std::fs::read(out.path().join("file")).unwrap();
    assert!(got == new_data, "final file must be the NEW content");
}

#[tokio::test]
async fn no_range_server_downloads_single_connection() {
    init_log();
    let data = make_data(2 * 1024 * 1024, 5);
    let opts = ServerOptions {
        ignore_range: true,
        ..Default::default()
    };
    let server = Server::start(Source::mem(data.clone()), opts).unwrap();
    let dbtmp = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = new_engine(&dbtmp).await;
    let t = add_task(&engine, &server.url()).await;

    let done = engine.download(&t.id, out_opts(&out, 4)).await.unwrap();
    assert_eq!(done.status, TaskStatus::Done);
    let got = std::fs::read(out.path().join("file")).unwrap();
    assert!(got == data);
}

/// GitHub 类场景：探测请求丢失 Range（200 + Accept-Ranges: bytes）但真实
/// 下载支持 Range → 依据 Accept-Ranges 决策多线程 → 分段全部 206 → 成功。
#[tokio::test]
async fn probe_without_range_but_ar_header_still_multithreads() {
    let data: Vec<u8> = (0..8 * 1024 * 1024u32).map(|i| (i * 5 + 2) as u8).collect();
    let opts = ServerOptions {
        plain_probe_only: true,
        ..Default::default()
    };
    let server = Server::start(Source::mem(data.clone()), opts).unwrap();
    let dbtmp = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = new_engine(&dbtmp).await;
    let t = add_task(&engine, &server.url()).await;

    let done = engine
        .download(&t.id, out_opts(&out, 8))
        .await
        .unwrap_or_else(|e| panic!("AR 决策应走多线程并成功: {e}"));
    assert_eq!(done.status, TaskStatus::Done);
    assert_eq!(done.total_bytes, Some(data.len() as u64));
    let got = std::fs::read(out.path().join("file")).unwrap();
    assert!(got == data);
}

/// 多镜像：主镜像断线，备用镜像接手，字节数一致。
#[tokio::test]
async fn mirror_fallback_when_primary_dies() {
    let data: Vec<u8> = (0..4 * 1024 * 1024u32).map(|i| (i * 7 + 3) as u8).collect();
    // 主镜像：首个 Range 请求掐断连接（劣质镜像）
    let primary = Server::start(
        Source::mem(data.clone()),
        ServerOptions {
            kill_first_range_after: Some(64 * 1024),
            ..Default::default()
        },
    )
    .unwrap();
    // 备用镜像：健康
    let backup = Server::start(Source::mem(data.clone()), ServerOptions::default()).unwrap();

    let dbtmp = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = new_engine(&dbtmp).await;
    let t = engine
        .create_task(CreateTask {
            mirrors: Some(vec![backup.url()]),
            postprocess: None,
            ..CreateTask::of(primary.url())
        })
        .await
        .unwrap();

    let done = engine
        .download(&t.id, out_opts(&out, 4))
        .await
        .unwrap_or_else(|e| panic!("镜像切换应成功: {e}"));
    assert_eq!(done.status, TaskStatus::Done);
    let got = std::fs::read(out.path().join("file")).unwrap();
    assert!(got == data);
}

/// 双健康镜像：轮转下载，字节数一致。
#[tokio::test]
async fn dual_mirrors_round_robin() {
    let data: Vec<u8> = (0..4 * 1024 * 1024u32)
        .map(|i| (i * 11 + 5) as u8)
        .collect();
    let s1 = Server::start(Source::mem(data.clone()), ServerOptions::default()).unwrap();
    let s2 = Server::start(Source::mem(data.clone()), ServerOptions::default()).unwrap();

    let dbtmp = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = new_engine(&dbtmp).await;
    let t = engine
        .create_task(CreateTask {
            mirrors: Some(vec![s2.url()]),
            postprocess: None,
            ..CreateTask::of(s1.url())
        })
        .await
        .unwrap();

    let done = engine.download(&t.id, out_opts(&out, 4)).await.unwrap();
    assert_eq!(done.status, TaskStatus::Done);
    let got = std::fs::read(out.path().join("file")).unwrap();
    assert!(got == data);
}

/// 回归：CDN 边缘谎报 Content-Range 起点 → 客户端校验 → 重试 → 字节正确。
#[tokio::test]
async fn content_range_mismatch_retries_and_stays_correct() {
    let data: Vec<u8> = (0..2 * 1024 * 1024u32).map(|i| (i * 3 + 1) as u8).collect();
    let opts = ServerOptions {
        wrong_range_once: true,
        ..Default::default()
    };
    let server = Server::start(Source::mem(data.clone()), opts).unwrap();
    let dbtmp = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = new_engine(&dbtmp).await;
    let t = add_task(&engine, &server.url()).await;

    let done = engine
        .download(&t.id, out_opts(&out, 2))
        .await
        .unwrap_or_else(|e| panic!("mismatch 校验后重试应成功: {e}"));
    assert_eq!(done.status, TaskStatus::Done);
    let got = std::fs::read(out.path().join("file")).unwrap();
    assert!(got == data, "错位响应绝不能写入本地偏移");
}

#[tokio::test]
async fn throttle_limits_throughput() {
    init_log();
    let data = make_data(2 * 1024 * 1024, 3); // 2MiB @ 512KB/s ≈ 3.5s
    let server = Server::start(Source::mem(data.clone()), ServerOptions::default()).unwrap();
    let dbtmp = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = new_engine(&dbtmp).await;
    let t = add_task(&engine, &server.url()).await;

    let opts = DownloadOptions {
        task_bps: Some(512 * 1024),
        ..out_opts(&out, 2)
    };
    let start = Instant::now();
    let done = engine.download(&t.id, opts).await.unwrap();
    let elapsed = start.elapsed();
    eprintln!("[throttle] elapsed {elapsed:?}");
    assert_eq!(done.status, TaskStatus::Done);
    assert!(
        elapsed >= Duration::from_secs(3),
        "task_bps=512KB on 2MiB should take >=3s, took {elapsed:?}"
    );
}

// —— yt-dlp 暂停续传回归 ——
// 根因：run_ytdlp 无条件走 Queued→Probing→Downloading，而迁移表禁止
// Paused→Probing，暂停后点「开始」报 InvalidTransition——错误落在后台
// 重试循环的日志里，UI 表现为「没有反应」且任务永远停在 Paused。
// 修复：入口按当前状态分支（Paused/Downloading 直接进 Downloading）。
// 本测试用假 yt-dlp（--version 退 0；真实运行时挂起）驱动真实的
// 「运行中暂停 → 从 Paused 恢复」流程。
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread")]
async fn ytdlp_paused_task_resumes_download() {
    fn step<S: std::fmt::Display>(s: S) {
        eprintln!("[ytdlp-resume] {s}");
    }
    // 所有等待都带硬超时：把「挂住」变成「带位置的失败」
    macro_rules! within {
        ($secs:expr, $fut:expr) => {
            tokio::time::timeout(std::time::Duration::from_secs($secs), $fut)
                .await
                .expect("step timed out")
        };
    }

    let dbtmp = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = new_engine(&dbtmp).await;
    // 假 yt-dlp 必须放独立 tempdir：out 会被 async move 捕获进下载任务，
    // 任务结束时 TempDir 析构会连 fake 一起删掉（run#2 就会"不可用"）
    let fakedir = tempfile::tempdir().unwrap();
    let fake = fakedir.path().join("fake-ytdlp.cmd");
    std::fs::write(
        &fake,
        // 真实运行分支必须是 cmd 自旋、不派生子进程：杀掉 cmd.exe 才能同时
        // 关闭 stdio 管道；若用 ping 之类孙进程，管道被孙进程握住，取消后
        // wait() 等不到 EOF 会永久挂起
        "@echo off\r\nif \"%~1\"==\"--version\" exit /b 0\r\n:loop\r\ngoto loop\r\n",
    )
    .unwrap();
    engine
        .set_setting("yt_dlp_path", fake.to_str().unwrap())
        .await
        .unwrap();

    // x.com URL 自动识别为 Ytdlp（is_ytdlp_host）
    let t = add_task(&engine, "https://x.com/testuser/status/1700000000000000001").await;
    assert_eq!(t.kind, dl_model::TaskKind::Ytdlp);

    let opts = out_opts(&out, 4);
    // 首启：Queued → Probing → Downloading，随后在「下载中」暂停
    let id = t.id.clone();
    let e2 = engine.clone();
    let handle = tokio::spawn(async move { e2.download(&id, out_opts(&out, 4)).await });
    step("spawn#1 → wait Downloading");
    within!(15, wait_status(&engine, &t.id, TaskStatus::Downloading));
    step("Downloading ✓ → pause");
    let _ = engine.pause(&t.id).await;
    step("wait Paused");
    within!(15, wait_status(&engine, &t.id, TaskStatus::Paused));
    step("Paused ✓ → await download() exit");
    let ended = within!(15, handle).unwrap();
    assert_eq!(
        ended.unwrap().status,
        TaskStatus::Paused,
        "driver 必须真正停下"
    );
    step("driver#1 exited ✓");

    // 回归点：Paused 再次 download() 必须重新进入 Downloading
    // （修复前此处报 InvalidTransition，状态停留在 Paused 不动）
    let id2 = t.id.clone();
    let e3 = engine.clone();
    let handle2 = tokio::spawn(async move { e3.download(&id2, opts).await });
    step("spawn#2 (from Paused) → wait Downloading");
    within!(15, wait_status(&engine, &t.id, TaskStatus::Downloading));
    step("Downloading ✓（修复生效：Paused 入口放行）");
    // 收尾：再次暂停，确认 driver 停下（不等假 yt-dlp 的自旋）
    let _ = engine.pause(&t.id).await;
    step("wait Paused#2");
    if tokio::time::timeout(std::time::Duration::from_secs(15), wait_status(
        &engine, &t.id, TaskStatus::Paused,
    ))
    .await
    .is_err()
    {
        let cur = engine.get_task(&t.id).await.unwrap().unwrap();
        step(format!(
            "STUCK: status={:?} running={} err={:?}",
            cur.status,
            engine.running_count(),
            cur.error
        ));
        // 就地复刻 dl_ytdlp::available 的判定，找出 run#2 判假的真凶
        let exists = fake.exists();
        let probe = tokio::process::Command::new(&fake)
            .arg("--version")
            .output()
            .await;
        match probe {
            Ok(out) => step(format!(
                "probe: exists={exists} status={:?} out={:?}",
                out.status.code(),
                String::from_utf8_lossy(&out.stdout)
            )),
            Err(e) => step(format!("probe: exists={exists} spawn error: {e}")),
        }
        // download #2 已经退出（running=0）：把它的真实错误消息挖出来
        let msg = match handle2.await {
            Ok(Ok(t)) => format!("unexpectedly Ok({:?})", t.status),
            Ok(Err(e)) => format!("download#2 error: {e}"),
            Err(j) => format!("join error: {j}"),
        };
        step(msg);
        panic!("pause #2 did not land");
    }
    step("await download()#2 exit");
    let _ = within!(15, handle2).unwrap();
    step("ALL OK");
}

// —— 暂停任务续传失败的可见性回归 ——
// 真实事故：签名 URL 过期（微软源 msixbundle）后点「开始」，恢复探测失败，
// 驱动要把任务标 Failed，但迁移表没有 Paused→Failed 这条边——迁移被拒、
// 错误被替换成 InvalidTransition 落进后台日志，任务永远卡在「已暂停」且
// UI 毫无反应。修复后：探测失败必须可见地落 Failed 并带错误原因。
#[tokio::test]
async fn paused_resume_with_dead_source_turns_failed() {
    let data = make_data(2 * 1024 * 1024, 5);
    let server = Server::start(Source::mem(data.clone()), ServerOptions::default()).unwrap();
    let dbtmp = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = new_engine(&dbtmp).await;
    let t = add_task(&engine, &server.url()).await;

    // 下载一段后暂停（模拟用户在 39% 暂停）
    let slow = DownloadOptions {
        task_bps: Some(2 * 1024 * 1024),
        ..out_opts(&out, 4)
    };
    let id = t.id.clone();
    let e2 = engine.clone();
    let handle = tokio::spawn(async move { e2.download(&id, slow).await });
    wait_progress(&engine, &t.id, 512 * 1024).await;
    let _ = engine.pause(&t.id).await;
    wait_status(&engine, &t.id, TaskStatus::Paused).await;
    let _ = handle.await.unwrap();

    // 源站失效（关停服务器 = 签名 URL 过期/403 的等价物）
    drop(server);

    // 开始：必须报错且任务可见地转为 Failed（修复前卡在 Paused 无任何反应）
    let r = engine.download(&t.id, out_opts(&out, 4)).await;
    assert!(r.is_err(), "死源必须报错");
    let cur = engine.get_task(&t.id).await.unwrap().unwrap();
    assert_eq!(
        cur.status,
        TaskStatus::Failed,
        "续传探测失败必须落 Failed，不能卡在 Paused"
    );
    assert!(cur.error.is_some(), "失败原因必须入库供详情查看");
}
