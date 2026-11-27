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
    if tokio::time::timeout(
        std::time::Duration::from_secs(15),
        wait_status(&engine, &t.id, TaskStatus::Paused),
    )
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

/// 问题 1 & 4：16 线程真 HTTP/1.1 + 动态窃取完成大文件下载且字节完全一致。
#[tokio::test]
async fn dynamic_steal_multithread_16_workers_matches_source() {
    let data = make_data(16 * 1024 * 1024, 17);
    let server = Server::start(Source::mem(data.clone()), ServerOptions::default()).unwrap();
    let dbtmp = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = new_engine(&dbtmp).await;
    let t = add_task(&engine, &server.url()).await;

    let done = engine.download(&t.id, out_opts(&out, 16)).await.unwrap();
    assert_eq!(done.status, TaskStatus::Done);
    assert_eq!(done.total_bytes, Some(data.len() as u64));
    let got = std::fs::read(out.path().join("file")).unwrap();
    assert!(got == data);
}

/// 问题 6：中转页（meta-refresh HTML）自动穿透到真实二进制直链，绝不把 HTML 当成品。
#[tokio::test]
async fn interstitial_meta_refresh_bypasses_to_real_file() {
    use std::io::{Read, Write};
    let payload = make_data(2 * 1024 * 1024, 29);
    let data_server =
        Server::start(Source::mem(payload.clone()), ServerOptions::default()).unwrap();
    let target_url = data_server.url();

    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_srv = stop.clone();
    std::thread::spawn(move || {
        listener.set_nonblocking(true).ok();
        while !stop_srv.load(std::sync::atomic::Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut s, _)) => {
                    let u = target_url.clone();
                    std::thread::spawn(move || {
                        s.set_nonblocking(false).ok();
                        let mut buf = [0u8; 2048];
                        let _ = s.read(&mut buf);
                        let html = format!(
                            "<html><head><title>Redirecting</title><meta http-equiv=\"refresh\" content=\"1; url={u}\"></head><body>Please wait</body></html>"
                        );
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            html.len(),
                            html
                        );
                        let _ = s.write_all(resp.as_bytes());
                    });
                }
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
    });

    let dbtmp = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = new_engine(&dbtmp).await;
    let t = add_task(&engine, &format!("http://{addr}/download-page")).await;

    let done = engine.download(&t.id, out_opts(&out, 4)).await.unwrap();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(done.status, TaskStatus::Done);
    let got = std::fs::read(out.path().join("file")).unwrap();
    assert!(got == payload, "必须穿透中转页下到真实二进制文件");
}

/// 问题 6：无法穿透的 HTML 登录/拦截页必须报错 InterstitialPage，不能落盘为 Done。
#[tokio::test]
async fn interstitial_html_wall_fails_instead_of_saving_html() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_srv = stop.clone();
    std::thread::spawn(move || {
        listener.set_nonblocking(true).ok();
        while !stop_srv.load(std::sync::atomic::Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut s, _)) => {
                    std::thread::spawn(move || {
                        s.set_nonblocking(false).ok();
                        let mut buf = [0u8; 2048];
                        let _ = s.read(&mut buf);
                        let html = "<html><head><title>Login Required</title></head><body>Please sign in</body></html>";
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            html.len(),
                            html
                        );
                        let _ = s.write_all(resp.as_bytes());
                    });
                }
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
    });

    let dbtmp = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = new_engine(&dbtmp).await;
    let t = add_task(&engine, &format!("http://{addr}/sdk.zip")).await;

    let err = engine.download(&t.id, out_opts(&out, 4)).await.unwrap_err();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(err.kind.code(), "interstitial_page");
    let cur = engine.get_task(&t.id).await.unwrap().unwrap();
    assert_eq!(cur.status, TaskStatus::Failed);
    assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 0);
}

/// 问题 3：队列并发控制 + start_queue / reorder_queue。
#[tokio::test]
async fn queue_respects_max_concurrent_and_reorders() {
    let data = make_data(512 * 1024, 13);
    let servers: Vec<_> = (0..4)
        .map(|_| Server::start(Source::mem(data.clone()), ServerOptions::default()).unwrap())
        .collect();
    let dbtmp = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = new_engine(&dbtmp).await;
    engine
        .set_setting("default_dir", out.path().to_str().unwrap())
        .await
        .unwrap();
    engine.set_setting("max_concurrent", "2").await.unwrap();

    let mut ids = Vec::new();
    for (i, srv) in servers.iter().enumerate() {
        let t = engine
            .create_task(CreateTask {
                filename: Some(format!("q_{i}.bin")),
                ..CreateTask::of(srv.url())
            })
            .await
            .unwrap();
        ids.push(t.id);
    }

    // 置顶第 4 个任务
    engine.reorder_queue(&ids[3], "top").await.unwrap();
    let queued = engine.list_tasks(Some(TaskStatus::Queued)).await.unwrap();
    assert_eq!(queued[0].id, ids[3]);

    let started = engine.start_queue().await.unwrap();
    assert_eq!(started, 4);

    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(engine.running_count() <= 2, "并发不得超过 max_concurrent=2");
        let list = engine.list_tasks(None).await.unwrap();
        if let Some(failed) = list.iter().find(|t| t.status == TaskStatus::Failed) {
            panic!("queued task unexpectedly failed: {:?}", failed.error);
        }
        if list.iter().all(|t| t.status == TaskStatus::Done) {
            break;
        }
        assert!(Instant::now() < deadline, "queue timed out");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    for i in 0..4 {
        let got = std::fs::read(out.path().join(format!("q_{i}.bin"))).unwrap();
        assert_eq!(got.len(), data.len());
    }
}

/// MDM 优化 1：主机并发连接上限自学习（额外连接被 429 拒绝时收敛并发并由存活连接优雅接管完成）。
#[tokio::test]
async fn host_limit_refusal_retires_extra_workers_and_completes() {
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    let data = make_data(4 * 1024 * 1024, 31);
    let data_srv = Arc::new(data.clone());
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let active_range = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let stop_srv = stop.clone();

    std::thread::spawn(move || {
        listener.set_nonblocking(true).ok();
        while !stop_srv.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut s, _)) => {
                    let payload = data_srv.clone();
                    let active = active_range.clone();
                    std::thread::spawn(move || {
                        struct ConnGuard(Arc<AtomicUsize>);
                        impl Drop for ConnGuard {
                            fn drop(&mut self) {
                                self.0.fetch_sub(1, Ordering::SeqCst);
                            }
                        }
                        s.set_nonblocking(false).ok();
                        let mut buf = [0u8; 4096];
                        let Ok(n) = s.read(&mut buf) else { return };
                        let head = String::from_utf8_lossy(&buf[..n]);
                        let mut range: Option<(u64, u64)> = None;
                        for line in head.lines() {
                            if let Some((k, v)) = line.split_once(':') {
                                if k.trim().eq_ignore_ascii_case("range") {
                                    if let Some(spec) = v.trim().strip_prefix("bytes=") {
                                        if let Some((a, b)) = spec.split_once('-') {
                                            if let (Ok(sa), Ok(sb)) =
                                                (a.parse::<u64>(), b.parse::<u64>())
                                            {
                                                range = Some((sa, sb));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        let Some((a, b)) = range else { return };
                        let is_probe = a == 0 && b == 0;
                        let _guard = if !is_probe {
                            let prev = active.fetch_add(1, Ordering::SeqCst);
                            if prev >= 2 {
                                active.fetch_sub(1, Ordering::SeqCst);
                                let resp = "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                                let _ = s.write_all(resp.as_bytes());
                                return;
                            }
                            Some(ConnGuard(active))
                        } else {
                            None
                        };
                        let end = b.min((payload.len() - 1) as u64);
                        let slice_len = end - a + 1;
                        let hdr = format!(
                            "HTTP/1.1 206 Partial Content\r\nETag: \"hostcap-v1\"\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {a}-{end}/{}\r\nContent-Length: {slice_len}\r\nConnection: close\r\n\r\n",
                            payload.len()
                        );
                        if s.write_all(hdr.as_bytes()).is_err() {
                            return;
                        }
                        let mut off = a as usize;
                        let stop_at = (end + 1) as usize;
                        while off < stop_at {
                            let step = (stop_at - off).min(64 * 1024);
                            if s.write_all(&payload[off..off + step]).is_err() {
                                break;
                            }
                            off += step;
                            if !is_probe {
                                std::thread::sleep(Duration::from_millis(2));
                            }
                        }
                    });
                }
                Err(_) => std::thread::sleep(Duration::from_millis(2)),
            }
        }
    });

    let dbtmp = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = new_engine(&dbtmp).await;
    let t = add_task(&engine, &format!("http://{addr}/limited.bin")).await;

    let done = engine.download(&t.id, out_opts(&out, 8)).await.unwrap();
    stop.store(true, Ordering::Relaxed);
    assert_eq!(done.status, TaskStatus::Done);
    let got = std::fs::read(out.path().join("limited.bin")).unwrap();
    assert!(got == data);
    let cap = engine.host_limits().cap("127.0.0.1");
    assert!(
        cap.is_some_and(|c| c <= 3),
        "expected learned host cap <= 3, got {cap:?}"
    );
}

/// MDM 优化 2：CDN 节点 ETag 不一致时自动降级为 Last-Modified 续传（不误判为不支持分段或文件变更）。
#[tokio::test]
async fn per_node_etag_fallback_keeps_multithread() {
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    let data = make_data(2 * 1024 * 1024, 41);
    let data_srv = Arc::new(data.clone());
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let node_seq = Arc::new(AtomicUsize::new(1));
    let stop = Arc::new(AtomicBool::new(false));
    let stop_srv = stop.clone();

    std::thread::spawn(move || {
        listener.set_nonblocking(true).ok();
        let lm = "Wed, 21 Oct 2015 07:28:00 GMT";
        while !stop_srv.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut s, _)) => {
                    let payload = data_srv.clone();
                    let seq = node_seq.clone();
                    std::thread::spawn(move || {
                        s.set_nonblocking(false).ok();
                        let mut buf = [0u8; 4096];
                        let Ok(n) = s.read(&mut buf) else { return };
                        let head = String::from_utf8_lossy(&buf[..n]);
                        let mut range: Option<(u64, u64)> = None;
                        let mut if_range: Option<String> = None;
                        for line in head.lines() {
                            if let Some((k, v)) = line.split_once(':') {
                                let kl = k.trim().to_ascii_lowercase();
                                if kl == "range" {
                                    if let Some(spec) = v.trim().strip_prefix("bytes=") {
                                        if let Some((a, b)) = spec.split_once('-') {
                                            if let (Ok(sa), Ok(sb)) =
                                                (a.parse::<u64>(), b.parse::<u64>())
                                            {
                                                range = Some((sa, sb));
                                            }
                                        }
                                    }
                                } else if kl == "if-range" {
                                    if_range = Some(v.trim().to_string());
                                }
                            }
                        }
                        let id = seq.fetch_add(1, Ordering::Relaxed);
                        let etag = format!("\"cdn-node-{id}\"");
                        // 强 ETag If-Range 永不匹配（模拟 CDN 多节点不同 ETag）；Last-Modified 匹配
                        let if_range_ok = match if_range.as_deref() {
                            None => true,
                            Some(v) => v == lm,
                        };
                        match (range, if_range_ok) {
                            (Some((a, b)), true) => {
                                let end = b.min((payload.len() - 1) as u64);
                                let slice_len = end - a + 1;
                                let hdr = format!(
                                    "HTTP/1.1 206 Partial Content\r\nETag: {etag}\r\nLast-Modified: {lm}\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {a}-{end}/{}\r\nContent-Length: {slice_len}\r\nConnection: close\r\n\r\n",
                                    payload.len()
                                );
                                if s.write_all(hdr.as_bytes()).is_ok() {
                                    let _ = s.write_all(&payload[a as usize..=end as usize]);
                                }
                            }
                            _ => {
                                let hdr = format!(
                                    "HTTP/1.1 200 OK\r\nETag: {etag}\r\nLast-Modified: {lm}\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                    payload.len()
                                );
                                if s.write_all(hdr.as_bytes()).is_ok() {
                                    let _ = s.write_all(&payload);
                                }
                            }
                        }
                    });
                }
                Err(_) => std::thread::sleep(Duration::from_millis(2)),
            }
        }
    });

    let dbtmp = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = new_engine(&dbtmp).await;
    let t = add_task(&engine, &format!("http://{addr}/cdn.bin")).await;

    let done = engine.download(&t.id, out_opts(&out, 4)).await.unwrap();
    stop.store(true, Ordering::Relaxed);
    assert_eq!(done.status, TaskStatus::Done);
    let got = std::fs::read(out.path().join("cdn.bin")).unwrap();
    assert!(got == data);
}

/// MDM 优化 3：无 ETag/Last-Modified 校验器时，续传通过 4KiB 盘尾重叠采样检测服务端内容暗中变更并重下。
#[tokio::test]
async fn resume_without_validators_detects_changed_file_via_overlap() {
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, RwLock};

    let data_v1 = make_data(2 * 1024 * 1024, 53);
    let data_v2 = make_data(2 * 1024 * 1024, 99);
    let current = Arc::new(RwLock::new(data_v1.clone()));
    let current_srv = current.clone();
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_srv = stop.clone();

    std::thread::spawn(move || {
        listener.set_nonblocking(true).ok();
        while !stop_srv.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut s, _)) => {
                    let payload_lock = current_srv.clone();
                    std::thread::spawn(move || {
                        s.set_nonblocking(false).ok();
                        let mut buf = [0u8; 4096];
                        let Ok(n) = s.read(&mut buf) else { return };
                        let head = String::from_utf8_lossy(&buf[..n]);
                        let mut range: Option<(u64, u64)> = None;
                        for line in head.lines() {
                            if let Some((k, v)) = line.split_once(':') {
                                if k.trim().eq_ignore_ascii_case("range") {
                                    if let Some(spec) = v.trim().strip_prefix("bytes=") {
                                        if let Some((a, b)) = spec.split_once('-') {
                                            if let (Ok(sa), Ok(sb)) =
                                                (a.parse::<u64>(), b.parse::<u64>())
                                            {
                                                range = Some((sa, sb));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        let payload = payload_lock.read().unwrap().clone();
                        match range {
                            Some((a, b)) => {
                                let end = b.min((payload.len() - 1) as u64);
                                let slice_len = end - a + 1;
                                // 故意不返回 ETag / Last-Modified
                                let hdr = format!(
                                    "HTTP/1.1 206 Partial Content\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {a}-{end}/{}\r\nContent-Length: {slice_len}\r\nConnection: close\r\n\r\n",
                                    payload.len()
                                );
                                if s.write_all(hdr.as_bytes()).is_ok() {
                                    let _ = s.write_all(&payload[a as usize..=end as usize]);
                                }
                            }
                            None => {
                                let hdr = format!(
                                    "HTTP/1.1 200 OK\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                    payload.len()
                                );
                                if s.write_all(hdr.as_bytes()).is_ok() {
                                    let _ = s.write_all(&payload);
                                }
                            }
                        }
                    });
                }
                Err(_) => std::thread::sleep(Duration::from_millis(2)),
            }
        }
    });

    let dbtmp = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = new_engine(&dbtmp).await;
    let t = add_task(&engine, &format!("http://{addr}/mutable.bin")).await;

    let slow = DownloadOptions {
        task_bps: Some(512 * 1024),
        ..out_opts(&out, 4)
    };
    let id = t.id.clone();
    let e2 = engine.clone();
    let handle = tokio::spawn(async move { e2.download(&id, slow).await });
    wait_progress(&engine, &t.id, 300 * 1024).await;
    let _ = engine.pause(&t.id).await;
    wait_status(&engine, &t.id, TaskStatus::Paused).await;
    let _ = handle.await.unwrap();

    // 无 ETag/Last-Modified 下替换服务端文件内容（同长度）
    *current.write().unwrap() = data_v2.clone();

    let done = engine.download(&t.id, out_opts(&out, 4)).await.unwrap();
    stop.store(true, Ordering::Relaxed);
    assert_eq!(done.status, TaskStatus::Done);
    let got = std::fs::read(out.path().join("mutable.bin")).unwrap();
    assert!(got == data_v2, "必须检测到盘尾 4KiB 变更并完整重下新版本");
}

fn b64_encode(input: &[u8]) -> String {
    const LUT: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let v = (b0 << 16) | (b1 << 8) | b2;
        out.push(LUT[((v >> 18) & 0x3f) as usize] as char);
        out.push(LUT[((v >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            LUT[((v >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            LUT[(v & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[tokio::test]
async fn server_repr_digest_auto_verifies_and_catches_corruption() {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    init_log();
    let payload = make_data(64 * 1024, 0x3C);
    let real_digest = Sha256::digest(&payload);
    let good_b64 = b64_encode(&real_digest);
    let mut bad_digest = real_digest;
    bad_digest[0] ^= 0xFF;
    let bad_b64 = b64_encode(&bad_digest);

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_srv = stop.clone();

    std::thread::spawn(move || {
        while !stop_srv.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut s, _)) => {
                    let data = payload.clone();
                    let good = good_b64.clone();
                    let bad = bad_b64.clone();
                    std::thread::spawn(move || {
                        let _ = s.set_nonblocking(false);
                        let mut buf = [0u8; 4096];
                        let n = s.read(&mut buf).unwrap_or(0);
                        let req = String::from_utf8_lossy(&buf[..n]);
                        let is_bad = req.contains("GET /bad.bin ");
                        let digest_b64 = if is_bad { bad } else { good };
                        let hdr = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nRepr-Digest: sha-256=:{digest_b64}:\r\nConnection: close\r\n\r\n",
                            data.len()
                        );
                        if s.write_all(hdr.as_bytes()).is_ok() {
                            let _ = s.write_all(&data);
                        }
                    });
                }
                Err(_) => std::thread::sleep(Duration::from_millis(2)),
            }
        }
    });

    let dbtmp = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = new_engine(&dbtmp).await;

    let ok_task = add_task(&engine, &format!("http://{addr}/good.bin")).await;
    let done = engine
        .download(&ok_task.id, out_opts(&out, 4))
        .await
        .unwrap();
    assert_eq!(done.status, TaskStatus::Done);

    let bad_task = add_task(&engine, &format!("http://{addr}/bad.bin")).await;
    let err = engine
        .download(&bad_task.id, out_opts(&out, 4))
        .await
        .unwrap_err();
    assert_eq!(err.kind, dl_error::ErrorKind::ChecksumMismatch);

    stop.store(true, Ordering::Relaxed);
}

#[tokio::test]
async fn relink_expired_url_resumes_from_existing_progress() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    init_log();
    let payload = Arc::new(make_data(2 * 1024 * 1024, 0x5A));
    let bytes_served_b = Arc::new(AtomicUsize::new(0));

    let spawn_srv = |etag: &'static str, counter: Option<Arc<AtomicUsize>>| {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_srv = stop.clone();
        let payload = payload.clone();
        std::thread::spawn(move || {
            while !stop_srv.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut s, _)) => {
                        let data = payload.clone();
                        let cnt = counter.clone();
                        std::thread::spawn(move || {
                            let _ = s.set_nonblocking(false);
                            let mut buf = [0u8; 4096];
                            let n = s.read(&mut buf).unwrap_or(0);
                            let req = String::from_utf8_lossy(&buf[..n]);
                            let mut range: Option<(u64, u64)> = None;
                            for line in req.lines() {
                                if let Some((k, v)) = line.split_once(':') {
                                    if k.eq_ignore_ascii_case("range") {
                                        if let Some(r) = v.trim().strip_prefix("bytes=") {
                                            if let Some((a, b)) = r.split_once('-') {
                                                let sa = a.parse::<u64>().ok();
                                                let sb = if b.is_empty() {
                                                    Some((data.len() - 1) as u64)
                                                } else {
                                                    b.parse::<u64>().ok()
                                                };
                                                if let (Some(sa), Some(sb)) = (sa, sb) {
                                                    range = Some((sa, sb));
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            match range {
                                Some((a, b)) => {
                                    let end = b.min((data.len() - 1) as u64);
                                    let slice = &data[a as usize..=end as usize];
                                    if let Some(c) = &cnt {
                                        c.fetch_add(slice.len(), Ordering::Relaxed);
                                    }
                                    let hdr = format!(
                                        "HTTP/1.1 206 Partial Content\r\nAccept-Ranges: bytes\r\nETag: {etag}\r\nContent-Range: bytes {a}-{end}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                        data.len(),
                                        slice.len()
                                    );
                                    if s.write_all(hdr.as_bytes()).is_ok() {
                                        let _ = s.write_all(slice);
                                    }
                                }
                                None => {
                                    if let Some(c) = &cnt {
                                        c.fetch_add(data.len(), Ordering::Relaxed);
                                    }
                                    let hdr = format!(
                                        "HTTP/1.1 200 OK\r\nAccept-Ranges: bytes\r\nETag: {etag}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                        data.len()
                                    );
                                    if s.write_all(hdr.as_bytes()).is_ok() {
                                        let _ = s.write_all(&data);
                                    }
                                }
                            }
                        });
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(2)),
                }
            }
        });
        (addr, stop)
    };

    let (addr_a, stop_a) = spawn_srv("\"etag-old\"", None);
    let (addr_b, stop_b) = spawn_srv("\"etag-new\"", Some(bytes_served_b.clone()));

    let dbtmp = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = new_engine(&dbtmp).await;
    let t = add_task(&engine, &format!("http://{addr_a}/archive.bin")).await;

    let slow = DownloadOptions {
        task_bps: Some(512 * 1024),
        ..out_opts(&out, 4)
    };
    let id = t.id.clone();
    let e2 = engine.clone();
    let handle = tokio::spawn(async move { e2.download(&id, slow).await });
    wait_progress(&engine, &t.id, 400 * 1024).await;
    let _ = engine.pause(&t.id).await;
    wait_status(&engine, &t.id, TaskStatus::Paused).await;
    let _ = handle.await.unwrap();
    stop_a.store(true, Ordering::Relaxed);

    let new_url = format!("http://{addr_b}/archive.bin");
    let relinked = engine.update_task_url(&t.id, &new_url).await.unwrap();
    assert_eq!(relinked.url, new_url);

    let done = engine.download(&t.id, out_opts(&out, 4)).await.unwrap();
    stop_b.store(true, Ordering::Relaxed);
    assert_eq!(done.status, TaskStatus::Done);
    let got = std::fs::read(out.path().join("archive.bin")).unwrap();
    assert_eq!(got, *payload);
    let served_b = bytes_served_b.load(Ordering::Relaxed);
    assert!(
        served_b < payload.len() - 256 * 1024,
        "expected hot-relink to resume from .dlpart instead of full redownload, served_b={served_b}"
    );
}

#[tokio::test]
async fn antibot_403_retries_with_own_user_agent_and_succeeds() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    init_log();
    let payload = Arc::new(make_data(256 * 1024, 0x77));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_srv = stop.clone();
    let srv_payload = payload.clone();

    std::thread::spawn(move || {
        while !stop_srv.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut s, _)) => {
                    let data = srv_payload.clone();
                    std::thread::spawn(move || {
                        let _ = s.set_nonblocking(false);
                        let mut buf = [0u8; 4096];
                        let n = s.read(&mut buf).unwrap_or(0);
                        let req = String::from_utf8_lossy(&buf[..n]);
                        let mut ua = String::new();
                        let mut range: Option<(u64, u64)> = None;
                        for line in req.lines() {
                            if let Some((k, v)) = line.split_once(':') {
                                if k.eq_ignore_ascii_case("user-agent") {
                                    ua = v.trim().to_string();
                                } else if k.eq_ignore_ascii_case("range") {
                                    if let Some(r) = v.trim().strip_prefix("bytes=") {
                                        if let Some((a, b)) = r.split_once('-') {
                                            let sa = a.parse::<u64>().ok();
                                            let sb = if b.is_empty() {
                                                Some((data.len() - 1) as u64)
                                            } else {
                                                b.parse::<u64>().ok()
                                            };
                                            if let (Some(sa), Some(sb)) = (sa, sb) {
                                                range = Some((sa, sb));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        if !ua.contains("AnythingDownloader/") {
                            let body = b"blocked browser spoof";
                            let hdr = format!(
                                "HTTP/1.1 403 Forbidden\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            if s.write_all(hdr.as_bytes()).is_ok() {
                                let _ = s.write_all(body);
                            }
                            return;
                        }
                        match range {
                            Some((a, b)) => {
                                let end = b.min((data.len() - 1) as u64);
                                let slice = &data[a as usize..=end as usize];
                                let hdr = format!(
                                    "HTTP/1.1 206 Partial Content\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {a}-{end}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                    data.len(),
                                    slice.len()
                                );
                                if s.write_all(hdr.as_bytes()).is_ok() {
                                    let _ = s.write_all(slice);
                                }
                            }
                            None => {
                                let hdr = format!(
                                    "HTTP/1.1 200 OK\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                    data.len()
                                );
                                if s.write_all(hdr.as_bytes()).is_ok() {
                                    let _ = s.write_all(&data);
                                }
                            }
                        }
                    });
                }
                Err(_) => std::thread::sleep(Duration::from_millis(2)),
            }
        }
    });

    let dbtmp = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = new_engine(&dbtmp).await;
    let t = add_task(&engine, &format!("http://{addr}/ua.bin")).await;
    let done = engine.download(&t.id, out_opts(&out, 4)).await.unwrap();
    stop.store(true, Ordering::Relaxed);

    assert_eq!(done.status, TaskStatus::Done);
    let got = std::fs::read(out.path().join("ua.bin")).unwrap();
    assert_eq!(got, *payload);
}

#[tokio::test]
async fn sequential_premature_eof_retries_to_complete_file() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    init_log();
    let payload = Arc::new(make_data(128 * 1024, 0x2B));
    let conn_num = Arc::new(AtomicUsize::new(0));

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_srv = stop.clone();
    let srv_payload = payload.clone();
    let srv_conn = conn_num.clone();

    std::thread::spawn(move || {
        while !stop_srv.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut s, _)) => {
                    let data = srv_payload.clone();
                    let n_conn = srv_conn.fetch_add(1, Ordering::Relaxed);
                    std::thread::spawn(move || {
                        let _ = s.set_nonblocking(false);
                        let mut buf = [0u8; 4096];
                        let n = s.read(&mut buf).unwrap_or(0);
                        let req = String::from_utf8_lossy(&buf[..n]);
                        let mut start = None;
                        for line in req.lines() {
                            if let Some((k, v)) = line.split_once(':') {
                                if k.eq_ignore_ascii_case("range") {
                                    if let Some(r) = v.trim().strip_prefix("bytes=") {
                                        if let Some((a, _)) = r.split_once('-') {
                                            start = a.parse::<u64>().ok();
                                        }
                                    }
                                }
                            }
                        }
                        // 第 1 次（探测）：返回 200（禁用分段，进入 download_sequential）
                        // 第 2 次（顺序下载首包）：声明总长 128KiB，但故意只发一半即 EOF
                        // 第 3 次（Range 断点续传）：补发剩余字节
                        if n_conn == 0 {
                            let hdr = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                data.len()
                            );
                            if s.write_all(hdr.as_bytes()).is_ok() {
                                let _ = s.write_all(&data);
                            }
                        } else if n_conn == 1 && start.is_none() {
                            let half = data.len() / 2;
                            let hdr = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                data.len()
                            );
                            if s.write_all(hdr.as_bytes()).is_ok() {
                                let _ = s.write_all(&data[..half]);
                            }
                        } else {
                            let a = start.unwrap_or(0) as usize;
                            let end = data.len() - 1;
                            let slice = &data[a..];
                            let hdr = format!(
                                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {a}-{end}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                data.len(),
                                slice.len()
                            );
                            if s.write_all(hdr.as_bytes()).is_ok() {
                                let _ = s.write_all(slice);
                            }
                        }
                    });
                }
                Err(_) => std::thread::sleep(Duration::from_millis(2)),
            }
        }
    });

    let dbtmp = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = new_engine(&dbtmp).await;
    let t = add_task(&engine, &format!("http://{addr}/seq.bin")).await;
    let done = engine.download(&t.id, out_opts(&out, 4)).await.unwrap();
    stop.store(true, Ordering::Relaxed);

    assert_eq!(done.status, TaskStatus::Done);
    assert!(conn_num.load(Ordering::Relaxed) >= 3);
    let got = std::fs::read(out.path().join("seq.bin")).unwrap();
    assert_eq!(got, *payload);
}

#[tokio::test]
async fn probe_retries_transient_503_and_succeeds() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    init_log();
    let payload = Arc::new(make_data(128 * 1024, 0x4E));
    let req_num = Arc::new(AtomicUsize::new(0));

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_srv = stop.clone();
    let srv_payload = payload.clone();
    let srv_req = req_num.clone();

    std::thread::spawn(move || {
        while !stop_srv.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut s, _)) => {
                    let data = srv_payload.clone();
                    let n_req = srv_req.fetch_add(1, Ordering::Relaxed);
                    std::thread::spawn(move || {
                        let _ = s.set_nonblocking(false);
                        let mut buf = [0u8; 4096];
                        let n = s.read(&mut buf).unwrap_or(0);
                        let req = String::from_utf8_lossy(&buf[..n]);
                        if n_req == 0 {
                            let hdr = "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 4\r\nConnection: close\r\n\r\nbusy";
                            let _ = s.write_all(hdr.as_bytes());
                            return;
                        }
                        let mut range: Option<(u64, u64)> = None;
                        for line in req.lines() {
                            if let Some((k, v)) = line.split_once(':') {
                                if k.eq_ignore_ascii_case("range") {
                                    if let Some(r) = v.trim().strip_prefix("bytes=") {
                                        if let Some((a, b)) = r.split_once('-') {
                                            if let (Ok(sa), Ok(sb)) =
                                                (a.parse::<u64>(), b.parse::<u64>())
                                            {
                                                range = Some((sa, sb));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        match range {
                            Some((a, b)) => {
                                let end = b.min((data.len() - 1) as u64);
                                let slice = &data[a as usize..=end as usize];
                                let hdr = format!(
                                    "HTTP/1.1 206 Partial Content\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {a}-{end}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                    data.len(),
                                    slice.len()
                                );
                                if s.write_all(hdr.as_bytes()).is_ok() {
                                    let _ = s.write_all(slice);
                                }
                            }
                            None => {
                                let hdr = format!(
                                    "HTTP/1.1 200 OK\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                    data.len()
                                );
                                if s.write_all(hdr.as_bytes()).is_ok() {
                                    let _ = s.write_all(&data);
                                }
                            }
                        }
                    });
                }
                Err(_) => std::thread::sleep(Duration::from_millis(2)),
            }
        }
    });

    let dbtmp = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = new_engine(&dbtmp).await;
    let t = add_task(&engine, &format!("http://{addr}/retry503.bin")).await;
    let done = engine.download(&t.id, out_opts(&out, 4)).await.unwrap();
    stop.store(true, Ordering::Relaxed);

    assert_eq!(done.status, TaskStatus::Done);
    assert!(req_num.load(Ordering::Relaxed) >= 2);
    let got = std::fs::read(out.path().join("retry503.bin")).unwrap();
    assert_eq!(got, *payload);
}
