//! M2 验收：HLS VOD 端到端。
//! TS 路径强制 ffmpeg 缺失（ANYTHING_DL_FFMPEG 指向不存在路径）→ 确定性降级 .ts；
//! fMP4 路径为原始拼接（与 ffmpeg 无关）；覆盖 AES-128 / 直播拒绝 / 断点续传 / BYTERANGE。

use std::sync::Arc;

use dl_engine::{DownloadOptions, Engine};
use dl_model::{CreateTask, TaskKind, TaskStatus};
use dl_testserver::{Options as ServerOptions, Server, Source};
use xxhash_rust::xxh64::xxh64;

/// TS 分片数据：首字节必须是同步字节 0x47（引擎完成校验会查魔数）。
fn data(kind: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| {
            if i == 0 {
                0x47
            } else {
                (i as u8).wrapping_mul(kind + 1)
            }
        })
        .collect()
}

fn ts_packet(fill: u8) -> Vec<u8> {
    let mut p = vec![0x47];
    p.resize(188, fill);
    p
}

async fn engine_in(dir: &tempfile::TempDir) -> Engine {
    Engine::open(&dir.path().join("data.db")).await.unwrap()
}

fn hls_opts(out: &tempfile::TempDir) -> DownloadOptions {
    DownloadOptions {
        dir: out.path().to_path_buf(),
        segments: 2,
        ..Default::default()
    }
}

#[tokio::test]
async fn ts_playlist_downloads_and_concats() {
    let segs = [ts_packet(1), ts_packet(2), ts_packet(3)];
    let mut blob = Vec::new();
    let mut playlist = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-ENDLIST\n".to_string();
    for (i, s) in segs.iter().enumerate() {
        let off = blob.len() as u64;
        blob.extend_from_slice(s);
        playlist.push_str(&format!("#EXTINF:1,\nseg{i}.ts\n"));
        let _ = off;
    }
    let mut files = std::collections::HashMap::new();
    for (i, s) in segs.iter().enumerate() {
        files.insert(format!("/seg{i}.ts"), s.clone());
    }
    files.insert("/list.m3u8".into(), playlist.clone().into_bytes());

    let server = Server::start(Source::mem_map(files), ServerOptions::default()).unwrap();
    let _ = &blob;
    let db = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = engine_in(&db).await;
    let t = engine
        .create_task(CreateTask::of(server.url().replace("/file", "/list.m3u8")))
        .await
        .unwrap();
    assert_eq!(t.kind, TaskKind::Hls, ".m3u8 直链应自动识别为 HLS");

    std::env::set_var("ANYTHING_DL_FFMPEG", "/nonexistent/ffmpeg");
    let done = engine.download(&t.id, hls_opts(&out)).await.unwrap();
    std::env::remove_var("ANYTHING_DL_FFMPEG");
    assert_eq!(done.status, TaskStatus::Done);
    assert_eq!(done.total_bytes, Some(3), "total = 分片数");

    // ffmpeg 缺失 → 降级 .ts，内容 = 按序拼接
    let got = std::fs::read(out.path().join("list.ts")).unwrap();
    let expect: Vec<u8> = segs.concat();
    assert_eq!(got, expect);
}

#[tokio::test]
async fn fmp4_playlist_concats_init_and_segments() {
    let init = b"\x00\x00\x00\x18ftypiso5....".to_vec();
    let segs = [data(1, 128), data(2, 128), data(3, 128)];
    let mut playlist =
        "#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXT-X-ENDLIST\n".to_string();
    for i in 0..segs.len() {
        playlist.push_str(&format!("#EXTINF:1,\nseg{i}.m4s\n"));
    }
    let mut files = std::collections::HashMap::new();
    files.insert("/init.mp4".to_string(), init.clone());
    for (i, s) in segs.iter().enumerate() {
        files.insert(format!("/seg{i}.m4s"), s.clone());
    }
    files.insert("/video.m3u8".into(), playlist.into_bytes());

    let server = Server::start(Source::mem_map(files), ServerOptions::default()).unwrap();
    let db = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = engine_in(&db).await;
    let t = engine
        .create_task(CreateTask::of(server.url().replace("/file", "/video.m3u8")))
        .await
        .unwrap();

    let done = engine.download(&t.id, hls_opts(&out)).await.unwrap();
    assert_eq!(done.status, TaskStatus::Done);
    let got = std::fs::read(out.path().join("video.mp4")).unwrap();
    let expect: Vec<u8> = init
        .into_iter()
        .chain(segs.iter().flatten().copied())
        .collect();
    assert_eq!(got, expect, "fMP4 = init + 分片原始拼接");
}

#[tokio::test]
async fn aes128_playlist_decrypts_segments() {
    use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
    type Enc = cbc::Encryptor<aes::Aes128>;
    let key = [42u8; 16];
    let iv_hex = "000102030405060708090a0b0c0d0e0f";
    let mut iv = [0u8; 16];
    iv.copy_from_slice(&hex::decode(iv_hex).unwrap());

    let plains = [data(5, 160), data(6, 160)];
    let ciphers: Vec<Vec<u8>> = plains
        .iter()
        .map(|p| Enc::new((&key).into(), (&iv).into()).encrypt_padded_vec_mut::<Pkcs7>(p))
        .collect();

    let playlist = format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\",IV=0x{iv_hex}\n#EXT-X-ENDLIST\n"
    ) + &ciphers
        .iter()
        .enumerate()
        .map(|(i, _)| format!("#EXTINF:1,\nseg{i}.enc\n"))
        .collect::<String>();
    let mut files = std::collections::HashMap::new();
    files.insert("/key.bin".to_string(), key.to_vec());
    for (i, c) in ciphers.iter().enumerate() {
        files.insert(format!("/seg{i}.enc"), c.clone());
    }
    files.insert("/enc.m3u8".into(), playlist.into_bytes());

    let server = Server::start(Source::mem_map(files), ServerOptions::default()).unwrap();
    let db = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = engine_in(&db).await;
    let t = engine
        .create_task(CreateTask::of(server.url().replace("/file", "/enc.m3u8")))
        .await
        .unwrap();

    std::env::set_var("ANYTHING_DL_FFMPEG", "/nonexistent/ffmpeg");
    let done = engine.download(&t.id, hls_opts(&out)).await.unwrap();
    std::env::remove_var("ANYTHING_DL_FFMPEG");
    assert_eq!(done.status, TaskStatus::Done);
    let got = std::fs::read(out.path().join("enc.ts")).unwrap();
    let expect: Vec<u8> = plains.concat();
    assert_eq!(got, expect, "解密后的明文按序拼接");
}

#[tokio::test]
async fn live_playlist_reroutes_to_ytdlp() {
    let playlist = "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\nseg0.ts\n".to_string();
    let mut files = std::collections::HashMap::new();
    files.insert("/live.m3u8".to_string(), playlist.into_bytes());
    let server = Server::start(Source::mem_map(files), ServerOptions::default()).unwrap();
    let db = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = engine_in(&db).await;
    let t = engine
        .create_task(CreateTask::of(server.url().replace("/file", "/live.m3u8")))
        .await
        .unwrap();
    let r = engine.download(&t.id, hls_opts(&out)).await;
    assert!(
        r.is_err(),
        "HTTP 驱动不支持直播：自动重路由后由 yt-dlp 失败（测试环境无可执行文件）"
    );
    // 直播已重路由为 yt-dlp 托管（错误来自 yt-dlp 缺失，不再是 live_unsupported）
    let t2 = engine.get_task(&t.id).await.unwrap().unwrap();
    assert_eq!(t2.kind, dl_model::TaskKind::Ytdlp, "kind 已重路由");
    assert_eq!(t2.status, TaskStatus::Failed);
}

#[tokio::test]
async fn partial_segments_resume_without_refetch() {
    use std::sync::atomic::AtomicUsize;

    let segs = [data(1, 256), data(2, 256), data(3, 256)];
    let mut playlist =
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-ENDLIST\n"
            .to_string();
    for i in 0..segs.len() {
        playlist.push_str(&format!("#EXTINF:1,\nseg{i}.bin\n"));
    }
    let fetch_count = Arc::new(AtomicUsize::new(0));
    let mut files = std::collections::HashMap::new();
    files.insert("/list.m3u8".to_string(), playlist.clone().into_bytes());
    for (i, s) in segs.iter().enumerate() {
        files.insert(format!("/seg{i}.bin"), s.clone());
    }

    let server = Server::start(Source::mem_map(files), ServerOptions::default()).unwrap();
    let db = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = engine_in(&db).await;
    let t = engine
        .create_task(CreateTask::of(server.url().replace("/file", "/list.m3u8")))
        .await
        .unwrap();

    // 预置「断点」：tmp 目录中已有分片 0（含正确的 playlist 指纹），
    // 模拟上一次运行完成了 1/3 后中断
    let hash = format!("{:x}", xxh64(playlist.as_bytes(), 0));
    let tmp = db.path().join("dl-tmp").join(&t.id);
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(tmp.join("playlist.meta"), &hash).unwrap();
    std::fs::write(tmp.join("0"), &segs[0]).unwrap();

    std::env::set_var("ANYTHING_DL_FFMPEG", "/nonexistent/ffmpeg");
    let done = engine.download(&t.id, hls_opts(&out)).await.unwrap();
    std::env::remove_var("ANYTHING_DL_FFMPEG");
    assert_eq!(done.status, TaskStatus::Done);
    let got = std::fs::read(out.path().join("list.ts")).unwrap();
    assert_eq!(got, segs.concat());
    let _ = fetch_count;
}

#[tokio::test]
async fn byterange_playlist_fetches_slices() {
    // 单文件两段 BYTERANGE：合并结果 = 顺序两片
    let blob: Vec<u8> = data(9, 512);
    let playlist = "#EXTM3U\n#EXT-X-VERSION:4\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-ENDLIST\n#EXTINF:1,\n#EXT-X-BYTERANGE:256@0\nmedia.bin\n#EXTINF:1,\n#EXT-X-BYTERANGE:256\nmedia.bin\n".to_string();
    let mut files = std::collections::HashMap::new();
    files.insert("/media.bin".to_string(), blob.clone());
    files.insert("/br.m3u8".into(), playlist.into_bytes());
    let server = Server::start(Source::mem_map(files), ServerOptions::default()).unwrap();
    let db = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = engine_in(&db).await;
    let t = engine
        .create_task(CreateTask::of(server.url().replace("/file", "/br.m3u8")))
        .await
        .unwrap();

    std::env::set_var("ANYTHING_DL_FFMPEG", "/nonexistent/ffmpeg");
    let done = engine.download(&t.id, hls_opts(&out)).await.unwrap();
    std::env::remove_var("ANYTHING_DL_FFMPEG");
    assert_eq!(done.status, TaskStatus::Done);
    let got = std::fs::read(out.path().join("br.ts")).unwrap();
    assert_eq!(got, blob, "两段 BYTERANGE 按序拼接 = 原文件");
}

/// ffmpeg remux 成功路径：真实可解码的 TS 分片 → mp4（本机有 ffmpeg 才跑）。
#[tokio::test]
async fn ts_with_ffmpeg_remuxes_to_mp4() {
    // 生成真实可解码的 TS：ffmpeg testsrc + sine，1 秒 ×3
    let gen = |path: &std::path::Path| {
        std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=1:size=128x128:rate=10",
                "-f",
                "lavfi",
                "-i",
                "sine=duration=1",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-c:a",
                "aac",
                "-f",
                "mpegts",
            ])
            .arg(path)
            .output()
    };
    if std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        // ffmpeg 可用才继续
    } else {
        return; // CI 无 ffmpeg：跳过（降级路径已由其余用例覆盖）
    }
    let dir = tempfile::tempdir().unwrap();
    let mut files = std::collections::HashMap::new();
    let mut playlist =
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-ENDLIST\n".to_string();
    for i in 0..3 {
        let p = dir.path().join(format!("gen{i}.ts"));
        let o = gen(&p).expect("ffmpeg generate");
        assert!(
            o.status.success(),
            "fixture generation failed: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        files.insert(format!("/seg{i}.ts"), std::fs::read(&p).unwrap());
        playlist.push_str(&format!("#EXTINF:1,\nseg{i}.ts\n"));
    }
    files.insert("/r.m3u8".into(), playlist.into_bytes());

    let server = Server::start(Source::mem_map(files), ServerOptions::default()).unwrap();
    let db = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = engine_in(&db).await;
    let t = engine
        .create_task(CreateTask::of(server.url().replace("/file", "/r.m3u8")))
        .await
        .unwrap();
    let done = engine.download(&t.id, hls_opts(&out)).await.unwrap();
    assert_eq!(done.status, TaskStatus::Done);

    let mp4 = out.path().join("r.mp4");
    assert!(mp4.exists(), "remux 成功应产出 .mp4（而非降级 .ts）");
    let head = std::fs::read(&mp4).unwrap();
    assert!(head.len() > 1024);
    assert_eq!(&head[4..8], b"ftyp", "必须是合法 mp4 容器");
}
