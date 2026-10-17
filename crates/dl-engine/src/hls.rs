//! HLS VOD 管线（DESIGN §5.4，仅 VOD：playlist 必须含 #EXT-X-ENDLIST）。
//!
//! 崩溃安全模型与 HTTP 分段不同：每个分片落为临时目录中的独立文件
//! （`<idx>.part` → rename `<idx>` 原子收尾），文件存在即完成——分片本身就是
//! journal，恢复零成本。全部就绪后合并为成品：
//! - fMP4（EXT-X-MAP）：init + 分片原始拼接即合法 fMP4 流，无需 ffmpeg
//! - TS：拼接后尝试 ffmpeg remux 为 mp4（stream copy），缺失则降级输出 .ts

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
use dl_error::ErrorKind;
use m3u8_rs::Playlist;

use crate::driver::{drive_from, finalize, with_task_headers, Ctx, DriveErr, TargetPaths};

type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

const WORKERS: usize = 16;
const SEG_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone)]
struct HlsSeg {
    uri: String,
    range: Option<(u64, u64)>, // (offset, len)
    /// AES-128：(key_uri 绝对地址, iv 16 字节)
    key: Option<(String, [u8; 16])>,
    /// EXT-X-MAP（fMP4 init segment），绝对地址
    map: Option<String>,
}

// ---------------------------------------------------------------- playlist

/// 主清单 → 取最高带宽 variant；媒体清单直接返回。递归深度 1。
/// 返回 (媒体清单 URL, 解析结果)。
async fn resolve_media(
    ctx: &Ctx,
    url: &str,
    text: &str,
    max_depth: u32,
) -> Result<(String, m3u8_rs::MediaPlaylist), DriveErr> {
    let mut url = url.to_string();
    let mut text = text.to_string();
    for _ in 0..=max_depth {
        match m3u8_rs::parse_playlist_res(text.as_bytes()) {
            Ok(Playlist::MasterPlaylist(m)) => {
                let best = m
                    .variants
                    .iter()
                    .max_by_key(|v| v.bandwidth)
                    .ok_or_else(|| {
                        DriveErr::Fatal(
                            ErrorKind::Internal,
                            "master playlist has no variants".into(),
                        )
                    })?;
                url = absolute_url(&url, &best.uri);
                text = fetch_text(ctx, &url).await?;
            }
            Ok(Playlist::MediaPlaylist(p)) => return Ok((url, p)),
            Err(_) => {
                return Err(DriveErr::Fatal(
                    ErrorKind::InvalidUrl,
                    "not a valid m3u8 playlist".into(),
                ))
            }
        }
    }
    Err(DriveErr::Fatal(
        ErrorKind::Internal,
        "master playlist nested too deep".into(),
    ))
}

/// master 清单 → 变体列表（带宽、分辨率、绝对 URL），GUI 换清晰度用。
pub(crate) fn master_variants(text: &str, base: &str) -> Vec<(u64, Option<String>, String)> {
    match m3u8_rs::parse_playlist_res(text.as_bytes()) {
        Ok(Playlist::MasterPlaylist(m)) => m
            .variants
            .iter()
            .map(|v| {
                let resolution = v
                    .resolution
                    .as_ref()
                    .map(|r| format!("{}x{}", r.width, r.height));
                (v.bandwidth, resolution, absolute_url(base, &v.uri))
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn absolute_url(base: &str, uri: &str) -> String {
    if uri.starts_with("http://") || uri.starts_with("https://") {
        return uri.to_string();
    }
    url::Url::parse(base)
        .and_then(|u| u.join(uri))
        .map(|u| u.to_string())
        .unwrap_or_else(|_| uri.to_string())
}

/// 构建分片表（BYTERANGE 沿链补偏移；AES-128 key 解析出绝对地址 + IV；
/// key/map 属性延续自上一分片，直到被重新声明）。
fn build_segments(media: &m3u8_rs::MediaPlaylist, base_url: &str) -> Result<Vec<HlsSeg>, DriveErr> {
    let mut out = Vec::with_capacity(media.segments.len());
    let mut running_offset: u64 = 0;
    let mut current_key: Option<(String, [u8; 16])> = None;
    let mut current_map: Option<String> = None;
    for (i, seg) in media.segments.iter().enumerate() {
        let seq = media.media_sequence + i as u64;
        match &seg.key {
            Some(k) if matches!(k.method, m3u8_rs::KeyMethod::None) => current_key = None,
            Some(k) if matches!(k.method, m3u8_rs::KeyMethod::AES128) => {
                let uri = k.uri.as_deref().ok_or_else(|| {
                    DriveErr::Fatal(ErrorKind::Internal, "AES-128 key without URI".into())
                })?;
                let key_uri = absolute_url(base_url, uri);
                let iv = match &k.iv {
                    Some(hex_iv) => parse_iv(hex_iv).map_err(|e| {
                        DriveErr::Fatal(ErrorKind::Internal, format!("bad IV: {e}"))
                    })?,
                    None => derive_iv_from_seq(seq),
                };
                current_key = Some((key_uri, iv));
            }
            Some(k) if matches!(k.method, m3u8_rs::KeyMethod::SampleAES) => {
                return Err(DriveErr::Fatal(
                    ErrorKind::DrmUnsupported,
                    "SAMPLE-AES is not supported".into(),
                ));
            }
            _ => {}
        }
        let range = seg.byte_range.as_ref().map(|br| {
            let offset = br.offset.unwrap_or(running_offset);
            (offset, br.length)
        });
        if let Some((off, len)) = range {
            running_offset = off + len;
        }
        if let Some(m) = &seg.map {
            current_map = Some(absolute_url(base_url, &m.uri));
        }
        out.push(HlsSeg {
            uri: absolute_url(base_url, &seg.uri),
            range,
            key: current_key.clone(),
            map: current_map.clone(),
        });
    }
    Ok(out)
}

fn parse_iv(s: &str) -> Result<[u8; 16], String> {
    let clean = s.trim().trim_start_matches("0x").trim_start_matches("0X");
    let bytes = hex::decode(clean).map_err(|e| e.to_string())?;
    bytes
        .try_into()
        .map_err(|_| "IV must be 16 bytes".to_string())
}

/// IV 缺省 = 媒体序列号的大端 128 位表示（HLS 规范）。
fn derive_iv_from_seq(seq: u64) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv[8..].copy_from_slice(&seq.to_be_bytes());
    iv
}

// ---------------------------------------------------------------- IO 帮手

async fn fetch_text(ctx: &Ctx, url: &str) -> Result<String, DriveErr> {
    let req = with_task_headers(
        dl_net::identity_headers(ctx.client.get(url)),
        &ctx.task_headers,
    );
    let resp = req
        .send()
        .await
        .map_err(|e| DriveErr::Fatal(dl_net::map_reqwest_err(&e), e.to_string()))?;
    let status = resp.status().as_u16();
    if status != 200 {
        let kind = dl_net::classify_status(status).unwrap_or(ErrorKind::Http {
            status,
            retry_after_secs: None,
        });
        return Err(DriveErr::Fatal(
            kind,
            format!("playlist fetch got HTTP {status}"),
        ));
    }
    resp.text()
        .await
        .map_err(|e| DriveErr::Fatal(dl_net::map_reqwest_err(&e), e.to_string()))
}

async fn fetch_segment_bytes(
    ctx: &Ctx,
    url: &str,
    range: Option<(u64, u64)>,
) -> Result<Vec<u8>, ErrorKind> {
    let mut req = dl_net::identity_headers(ctx.client.get(url));
    if let Some((off, len)) = range {
        req = req.header("Range", format!("bytes={off}-{}", off + len - 1));
    }
    let req = with_task_headers(req, &ctx.task_headers);
    let resp = req.send().await.map_err(|e| dl_net::map_reqwest_err(&e))?;
    let status = resp.status().as_u16();
    if status != 200 && status != 206 {
        return Err(dl_net::classify_status(status).unwrap_or(ErrorKind::Http {
            status,
            retry_after_secs: None,
        }));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| dl_net::map_reqwest_err(&e))?;
    Ok(bytes.to_vec())
}

/// AES-128-CBC 解密；PKCS7 校验失败时退回原始 CBC 逐块解密（兼容非规范流）。
fn decrypt_aes128(data: &[u8], key: &[u8; 16], iv: &[u8; 16]) -> Result<Vec<u8>, ErrorKind> {
    if data.is_empty() || !data.len().is_multiple_of(16) {
        return Err(ErrorKind::Internal);
    }
    {
        if let Ok(pt) =
            Aes128CbcDec::new(key.into(), iv.into()).decrypt_padded_vec_mut::<Pkcs7>(data)
        {
            return Ok(pt);
        }
    }
    // 无填充回退：手工 CBC（当前密文块解密后异或前一密文块）
    let mut prev: [u8; 16] = *iv;
    let mut out = Vec::with_capacity(data.len());
    let mut block = [0u8; 16];
    let cipher = <aes::Aes128 as aes::cipher::KeyInit>::new(key.into());
    use aes::cipher::BlockDecrypt;
    for chunk in data.as_chunks::<16>().0 {
        block.copy_from_slice(chunk);
        let ct = block;
        let ga = aes::cipher::generic_array::GenericArray::from_mut_slice(&mut block);
        cipher.decrypt_block(ga);
        for i in 0..16 {
            block[i] ^= prev[i];
        }
        out.extend_from_slice(&block);
        prev = ct;
    }
    Ok(out)
}

// ---------------------------------------------------------------- 阶段入口

pub(crate) async fn phase(ctx: Arc<Ctx>) -> Result<(), DriveErr> {
    let ctx = &*ctx; // &Ctx，内部函数统一走引用
    let task = ctx.engine.require(&ctx.task_id).await.map_err(drive_from)?;

    // —— 探测：拉清单（master → 最高带宽 variant）
    let text = fetch_text(ctx, &task.url).await?;
    if ctx.cancel.is_cancelled() {
        return Err(DriveErr::Paused);
    }
    // master 清单 → 记录变体列表（GUI「换清晰度」数据源）
    let variants = master_variants(&text, &task.url);
    if !variants.is_empty() {
        let list: Vec<serde_json::Value> = variants
            .iter()
            .map(|(bw, res, url)| {
                serde_json::json!({ "bandwidth": bw, "resolution": res, "url": url })
            })
            .collect();
        if let Ok(json) = serde_json::to_string(&list) {
            let _ = ctx.engine.record_variants(&ctx.task_id, &json).await;
        }
    }

    let (media_url, media) = resolve_media(ctx, &task.url, &text, 1).await?;
    if !media.end_list {
        return Err(DriveErr::Fatal(
            ErrorKind::LiveUnsupported,
            "live streams (no #EXT-X-ENDLIST) are not supported".into(),
        ));
    }
    let segs = build_segments(&media, &media_url)?;
    if segs.is_empty() {
        return Err(DriveErr::Fatal(
            ErrorKind::Internal,
            "playlist has no segments".into(),
        ));
    }
    let playlist_hash = xxhash_rust::xxh64::xxh64(text.as_bytes(), 0);

    // 元数据回写：total = 分片数（GUI 按分片渲染进度）
    ctx.engine
        .record_probe(
            &ctx.task_id,
            Some(segs.len() as u64),
            None,
            None,
            Some("application/vnd.apple.mpegurl".into()),
            Some(task.url.clone()),
        )
        .await
        .map_err(drive_from)?;

    // 崩溃残留的 Downloading 状态不重复迁移（状态机禁止自迁移）
    let cur = ctx.engine.require(&ctx.task_id).await.map_err(drive_from)?;
    if cur.status != dl_model::TaskStatus::Downloading {
        ctx.engine
            .transition(&ctx.task_id, dl_model::TaskStatus::Downloading, None)
            .await
            .map_err(drive_from)?;
    }

    let tmp_dir = ctx
        .engine
        .journal_dir()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(crate::data_dir)
        .join("dl-tmp")
        .join(&ctx.task_id);
    std::fs::create_dir_all(&tmp_dir).map_err(|e| {
        DriveErr::Fatal(
            ErrorKind::FileAccessDenied,
            format!("create hls tmp dir: {e}"),
        )
    })?;

    // 断点续传：清单指纹不一致 → 旧分片全部作废（DESIGN §5.4.8）
    let meta_path = tmp_dir.join("playlist.meta");
    if let Ok(existing) = std::fs::read_to_string(&meta_path) {
        if existing != format!("{playlist_hash:x}") {
            tracing::info!("playlist changed on resume; discarding old segments");
            let _ = std::fs::remove_dir_all(&tmp_dir);
            let _ = std::fs::create_dir_all(&tmp_dir);
        }
    }
    let _ = std::fs::write(&meta_path, format!("{playlist_hash:x}"));

    let total = segs.len() as u64;
    let shared = Arc::new(HlsShared {
        done_count: AtomicU64::new(0),
        bytes: AtomicU64::new(0),
        fatal: Mutex::new(None),
        restart: AtomicBool::new(false),
    });
    shared.done_count.store(
        count_existing(&tmp_dir, segs.len()) as u64,
        Ordering::Relaxed,
    );

    // 进度上报（字节速率 + 分片计数）
    {
        let ectx = ctx.clone();
        let eshared = shared.clone();
        tokio::spawn(async move {
            let mut last_bytes = 0u64;
            let mut last_t = std::time::Instant::now();
            let mut bps = 0f64;
            let total_local = total;
            let mut last_persisted = std::time::Instant::now();
            loop {
                tokio::time::sleep(Duration::from_millis(300)).await;
                let done = eshared.done_count.load(Ordering::Relaxed);
                let bytes = eshared.bytes.load(Ordering::Relaxed);
                let dt = last_t.elapsed().as_secs_f64();
                if dt >= 0.3 {
                    let inst = (bytes.saturating_sub(last_bytes)) as f64 / dt;
                    bps = if bps == 0.0 {
                        inst
                    } else {
                        bps * 0.6 + inst * 0.4
                    };
                    last_bytes = bytes;
                    last_t = std::time::Instant::now();
                    ectx.engine.emit(crate::engine::EngineEvent::Progress {
                        task_id: ectx.task_id.clone(),
                        downloaded: done,
                        total: Some(total_local),
                        bps: bps as u64,
                        segments: None,
                    });
                    // 分片计数持久化（暂停后 UI 恢复显示）
                    if last_persisted.elapsed() >= Duration::from_secs(2) {
                        let _ = ectx.engine.record_downloaded(&ectx.task_id, done).await;
                        last_persisted = std::time::Instant::now();
                    }
                }
                if done >= total_local && bytes == last_bytes {
                    break;
                }
            }
        });
    }

    // 分片 worker 池
    let segs = Arc::new(segs);
    let tmp = Arc::new(tmp_dir);
    let cursor = Arc::new(AtomicU64::new(0));
    let key_cache: Arc<Mutex<HashMap<String, Vec<u8>>>> = Arc::new(Mutex::new(HashMap::new()));
    let mut handles = Vec::new();
    for _ in 0..WORKERS.min(segs.len().max(1)) {
        let worker_ctx = ctx.engine.clone();
        let _ = &worker_ctx;
        handles.push(tokio::spawn(hls_worker(
            std::sync::Arc::new(ctx.clone()),
            segs.clone(),
            tmp.clone(),
            shared.clone(),
            cursor.clone(),
            key_cache.clone(),
        )));
    }
    for h in handles {
        let _ = h.await;
    }

    if let Some(kind) = shared.take_fatal() {
        return Err(DriveErr::Fatal(kind, "segment download failed".into()));
    }
    if ctx.cancel.is_cancelled() {
        return Err(DriveErr::Paused); // 分片文件已持久化，续传零成本
    }
    if shared.restart.load(Ordering::Relaxed) {
        // 分片签名批量失效 → 清掉旧分片，外层 Restart 重拉清单重来一次
        let _ = std::fs::remove_dir_all(&*tmp);
        return Err(DriveErr::Restart);
    }
    if shared.done_count.load(Ordering::Relaxed) < total {
        return Err(DriveErr::Fatal(
            ErrorKind::Internal,
            "some segments missing after download".into(),
        ));
    }

    // —— 合并
    ctx.engine
        .transition(&ctx.task_id, dl_model::TaskStatus::Merging, None)
        .await
        .map_err(drive_from)?;

    let flavor = detect_flavor(&segs);
    let mut paths = hls_target_paths(ctx, &task, flavor)?;
    // 设置里的 ffmpeg 路径优先（GUI 设置页可配）
    let ffmpeg_hint = ctx.engine.get_setting("ffmpeg_path").await.ok().flatten();
    merge(
        ctx,
        segs.as_ref(),
        &tmp,
        &mut paths,
        flavor,
        ffmpeg_hint.as_deref(),
    )
    .await?;

    ctx.engine
        .transition(&ctx.task_id, dl_model::TaskStatus::Verifying, None)
        .await
        .map_err(drive_from)?;
    verify_hls(&paths.part, flavor)?;

    // 哈希校验（与 HTTP 路径同语义）
    if let Some((algo, expect)) = &ctx.checksum {
        let ok = crate::checksum::verify_file(&paths.part, algo, expect)
            .map_err(|e| DriveErr::Fatal(e.kind, e.message))?;
        if !ok {
            return Err(DriveErr::Fatal(
                ErrorKind::ChecksumMismatch,
                format!("checksum {algo} mismatch"),
            ));
        }
    }

    finalize(ctx, &paths).await?;
    let _ = std::fs::remove_dir_all(&*tmp);

    ctx.engine
        .transition(&ctx.task_id, dl_model::TaskStatus::Done, None)
        .await
        .map_err(drive_from)?;
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Flavor {
    Ts,
    Fmp4,
}

fn detect_flavor(segs: &[HlsSeg]) -> Flavor {
    if segs
        .iter()
        .any(|s| s.map.is_some() || s.uri.contains(".m4s"))
    {
        Flavor::Fmp4
    } else {
        Flavor::Ts
    }
}

struct HlsShared {
    done_count: AtomicU64,
    bytes: AtomicU64,
    fatal: Mutex<Option<ErrorKind>>,
    restart: AtomicBool,
}

impl HlsShared {
    fn take_fatal(&self) -> Option<ErrorKind> {
        self.fatal.lock().unwrap().take()
    }
}

async fn hls_worker(
    ctx: Arc<Ctx>,
    segs: Arc<Vec<HlsSeg>>,
    tmp: Arc<PathBuf>,
    shared: Arc<HlsShared>,
    cursor: Arc<AtomicU64>,
    key_cache: Arc<Mutex<HashMap<String, Vec<u8>>>>,
) {
    let total = segs.len() as u64;
    loop {
        if shared.fatal.lock().unwrap().is_some() || ctx.cancel.is_cancelled() {
            return;
        }
        let idx = cursor.fetch_add(1, Ordering::Relaxed);
        if idx >= total {
            return;
        }
        let final_path = tmp.join(idx.to_string());
        if final_path.exists() {
            shared.done_count.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        match download_one(&ctx, &segs[idx as usize], &final_path, &key_cache).await {
            Ok(len) => {
                shared.done_count.fetch_add(1, Ordering::Relaxed);
                shared.bytes.fetch_add(len, Ordering::Relaxed);
            }
            Err(kind) => {
                if matches!(kind, ErrorKind::AuthExpired | ErrorKind::UrlExpired) {
                    // 分片签名批量失效 → 触发一次清单刷新重下（DESIGN §5.4.8）
                    shared.restart.store(true, Ordering::Relaxed);
                } else {
                    let mut f = shared.fatal.lock().unwrap();
                    if f.is_none() {
                        *f = Some(kind);
                    }
                }
                return;
            }
        }
    }
}

async fn download_one(
    ctx: &Ctx,
    seg: &HlsSeg,
    final_path: &Path,
    key_cache: &Mutex<HashMap<String, Vec<u8>>>,
) -> Result<u64, ErrorKind> {
    let mut attempts = 0;
    loop {
        attempts += 1;
        if attempts > SEG_ATTEMPTS {
            return Err(ErrorKind::NetworkConnectionReset);
        }
        let raw = match fetch_segment_bytes(ctx, &seg.uri, seg.range).await {
            Ok(b) => b,
            Err(e) if e.is_retryable() => {
                tokio::time::sleep(Duration::from_secs(1 << attempts.min(3).saturating_sub(1)))
                    .await;
                continue;
            }
            Err(e) => return Err(e),
        };
        let plain = match &seg.key {
            Some((key_uri, iv)) => {
                // key 按 URI 缓存；缓存未命中时在锁外抓取（DESIGN §5.4.6）
                let key_bytes = {
                    let cached = key_cache.lock().unwrap().get(key_uri).cloned();
                    match cached {
                        Some(k) => k,
                        None => {
                            let k = fetch_segment_bytes(ctx, key_uri, None).await?;
                            key_cache.lock().unwrap().insert(key_uri.clone(), k.clone());
                            k
                        }
                    }
                };
                let key: [u8; 16] = key_bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| ErrorKind::Internal)?;
                decrypt_aes128(&raw, &key, iv)?
            }
            None => raw,
        };
        // 原子落盘：<idx>.part → <idx>
        let part = final_path.with_extension("part");
        let mut f = std::fs::File::create(&part).map_err(|e| io_err_kind(&e))?;
        f.write_all(&plain).map_err(|e| io_err_kind(&e))?;
        f.sync_all().map_err(|e| io_err_kind(&e))?;
        drop(f);
        std::fs::rename(&part, final_path).map_err(|e| io_err_kind(&e))?;
        return Ok(plain.len() as u64);
    }
}

fn io_err_kind(e: &std::io::Error) -> ErrorKind {
    match e.kind() {
        std::io::ErrorKind::StorageFull => ErrorKind::DiskFull,
        std::io::ErrorKind::PermissionDenied => ErrorKind::FileAccessDenied,
        _ => ErrorKind::Internal,
    }
}

fn count_existing(tmp: &Path, len: usize) -> usize {
    (0..len)
        .filter(|i| tmp.join(i.to_string()).exists())
        .count()
}

// ---------------------------------------------------------------- 合并

fn hls_target_paths(
    ctx: &Ctx,
    task: &dl_model::Task,
    flavor: Flavor,
) -> Result<TargetPaths, DriveErr> {
    let dir = PathBuf::from(
        task.dir
            .clone()
            .unwrap_or_else(|| ctx.opts.dir.display().to_string()),
    );
    std::fs::create_dir_all(&dir).map_err(|e| {
        DriveErr::Fatal(
            ErrorKind::FileAccessDenied,
            format!("create download dir: {e}"),
        )
    })?;
    // 命名：用户名 > URL 末段（去掉 .m3u8）> "playlist"；扩展名按成品形态
    let base = dl_filename::pick_name(task.filename.as_deref(), None, &task.url, None)
        .replace(".m3u8", "")
        .replace(".m3u", "");
    let base = if base.is_empty() {
        "playlist".to_string()
    } else {
        base
    };
    let ext = match flavor {
        Flavor::Fmp4 => "mp4", // init + m4s 拼接即 mp4 流
        Flavor::Ts => "mp4",   // ffmpeg remux；失败降级 .ts（merge 内改 paths.name）
    };
    let name = format!("{base}.{ext}");
    let part = dir.join(format!("{name}.dlpart"));
    let journal_dir = ctx.engine.journal_dir();
    std::fs::create_dir_all(&journal_dir).map_err(|e| {
        DriveErr::Fatal(
            ErrorKind::FileAccessDenied,
            format!("create journal dir: {e}"),
        )
    })?;
    Ok(TargetPaths {
        journal: journal_dir.join(format!("{}.dlctrl", ctx.task_id)),
        name,
        part,
        dir,
    })
}

async fn merge(
    ctx: &Ctx,
    segs: &[HlsSeg],
    tmp: &Path,
    paths: &mut TargetPaths,
    flavor: Flavor,
    ffmpeg_hint: Option<&str>,
) -> Result<(), DriveErr> {
    let concat_path = tmp.join("_concat.bin");
    let mut out = std::fs::File::create(&concat_path)
        .map_err(|e| DriveErr::Fatal(io_err_kind(&e), format!("create concat: {e}")))?;
    for (i, seg) in segs.iter().enumerate() {
        if i == 0 {
            if let Some(map) = &seg.map {
                // EXT-X-MAP init segment：必须位于拼接流最前（DESIGN §5.4.4）
                let init = fetch_segment_bytes(ctx, map, None)
                    .await
                    .map_err(|e| DriveErr::Fatal(e, "fetch init segment failed".into()))?;
                out.write_all(&init)
                    .map_err(|e| DriveErr::Fatal(io_err_kind(&e), format!("write init: {e}")))?;
            }
        }
        let data = std::fs::read(tmp.join(i.to_string()))
            .map_err(|e| DriveErr::Fatal(io_err_kind(&e), format!("read segment {i}: {e}")))?;
        out.write_all(&data)
            .map_err(|e| DriveErr::Fatal(io_err_kind(&e), format!("write segment {i}: {e}")))?;
    }
    out.sync_all()
        .map_err(|e| DriveErr::Fatal(io_err_kind(&e), format!("sync concat: {e}")))?;
    drop(out);

    match flavor {
        Flavor::Fmp4 => {
            std::fs::rename(&concat_path, &paths.part)
                .map_err(|e| DriveErr::Fatal(io_err_kind(&e), format!("rename merged: {e}")))?;
        }
        Flavor::Ts => {
            // ffmpeg 依扩展名猜 muxer：先输出到 .mp4 临时文件再 rename 成 .dlpart
            let remux_out = tmp.join("_remux.mp4");
            match remux_with_ffmpeg(&concat_path, &remux_out, ffmpeg_hint).await {
                Ok(()) => {
                    let _ = std::fs::remove_file(&concat_path);
                    std::fs::rename(&remux_out, &paths.part).map_err(|e| {
                        DriveErr::Fatal(io_err_kind(&e), format!("rename remuxed: {e}"))
                    })?;
                }
                Err(e) => {
                    // 降级：原始 TS 拼接输出 .ts（播放器兼容性弱于 mp4，明示）
                    tracing::warn!(error = %e, "ffmpeg remux unavailable; falling back to .ts");
                    let ts_name = format!("{}.ts", paths.name.trim_end_matches(".mp4"));
                    let ts_part = paths.dir.join(format!("{ts_name}.dlpart"));
                    std::fs::rename(&concat_path, &ts_part)
                        .map_err(|e| DriveErr::Fatal(io_err_kind(&e), format!("rename ts: {e}")))?;
                    paths.name = ts_name;
                    paths.part = ts_part;
                }
            }
        }
    }
    Ok(())
}

/// TS remux：ffmpeg -i concat -c copy -movflags +faststart out.mp4。
/// 看门狗：M5 统一补（当前依赖 ffmpeg 自身错误返回）。
async fn remux_with_ffmpeg(concat: &Path, out: &Path, hint: Option<&str>) -> Result<(), String> {
    let ffmpeg = find_ffmpeg_with(hint).ok_or_else(|| "ffmpeg not found".to_string())?;
    let _ = std::fs::remove_file(out); // 残留会使 ffmpeg -y 之外的场景失败
    let output = tokio::process::Command::new(ffmpeg)
        .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(concat)
        .args(["-c", "copy", "-movflags", "+faststart"])
        .arg(out)
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "ffmpeg failed: {}",
            String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(300)
                .collect::<String>()
        ))
    }
}

/// ffmpeg 查找顺序：显式提示（设置 ffmpeg_path / 环境变量）> 应用目录 > PATH。
pub fn find_ffmpeg() -> Option<PathBuf> {
    let hint = std::env::var("ANYTHING_DL_FFMPEG").ok();
    find_ffmpeg_with(hint.as_deref())
}

pub fn find_ffmpeg_with(hint: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = hint {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for name in ["ffmpeg.exe", "ffmpeg"] {
                let p = dir.join(name);
                if p.exists() {
                    return Some(p);
                }
            }
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for name in ["ffmpeg.exe", "ffmpeg"] {
                let p = dir.join(name);
                if p.exists() {
                    return Some(p);
                }
            }
        }
    }
    None
}

fn verify_hls(part: &Path, flavor: Flavor) -> Result<(), DriveErr> {
    let meta = std::fs::metadata(part)
        .map_err(|e| DriveErr::Fatal(io_err_kind(&e), format!("verify: {e}")))?;
    if meta.len() == 0 {
        return Err(DriveErr::Fatal(
            ErrorKind::Internal,
            "merged output is empty".into(),
        ));
    }
    // 轻量魔数校验：fMP4 首 12 字节内含 "ftyp"；TS 首字节 0x47
    let mut head = [0u8; 12];
    if let Ok(mut f) = std::fs::File::open(part) {
        use std::io::Read;
        let _ = f.read_exact(&mut head);
    }
    match flavor {
        Flavor::Fmp4 => {
            if &head[4..8] != b"ftyp" {
                return Err(DriveErr::Fatal(
                    ErrorKind::Internal,
                    "merged output is not a valid fMP4 stream".into(),
                ));
            }
        }
        Flavor::Ts => {
            // remux 成功 → mp4 容器；降级 → 原始 TS。两者都是合法成品。
            let is_mp4 = &head[4..8] == b"ftyp";
            let is_ts = head[0] == 0x47;
            if !is_mp4 && !is_ts {
                return Err(DriveErr::Fatal(
                    ErrorKind::Internal,
                    "merged output is neither mp4 nor MPEG-TS".into(),
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iv_from_sequence() {
        assert_eq!(derive_iv_from_seq(1)[15], 1);
        assert_eq!(derive_iv_from_seq(0x0102)[14], 1);
        assert_eq!(derive_iv_from_seq(0x0102)[15], 2);
        assert!(derive_iv_from_seq(0).iter().all(|&b| b == 0));
    }

    #[test]
    fn parse_explicit_iv() {
        assert_eq!(
            parse_iv("0x000102030405060708090a0b0c0d0e0f").unwrap()[0],
            0
        );
        assert_eq!(
            parse_iv("ABCDEF0123456789ABCDEF0123456789").unwrap()[15],
            0x89
        );
        assert!(parse_iv("1234").is_err());
    }

    #[test]
    fn absolute_url_resolution() {
        assert_eq!(
            absolute_url("https://a/x/list.m3u8", "seg001.ts"),
            "https://a/x/seg001.ts"
        );
        assert_eq!(
            absolute_url("https://a/x/list.m3u8", "/root.ts"),
            "https://a/root.ts"
        );
        assert_eq!(
            absolute_url("https://a/x/list.m3u8", "https://b/t.ts"),
            "https://b/t.ts"
        );
    }

    #[test]
    fn decrypt_roundtrip_pkcs7_and_raw_fallback() {
        use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
        type Enc = cbc::Encryptor<aes::Aes128>;
        let key = [7u8; 16];
        let iv = [9u8; 16];
        let plaintext = b"hello hls segment data!!"; // 24 字节
        let ct = Enc::new((&key).into(), (&iv).into()).encrypt_padded_vec_mut::<Pkcs7>(plaintext);
        let got = decrypt_aes128(&ct, &key, &iv).unwrap();
        assert_eq!(got, plaintext);

        // 非 PKCS7 数据（手工构造：16 字节无填充密文）走原始回退
        let mut enc2 = Enc::new((&key).into(), (&iv).into());
        let mut block = *plaintext.first_chunk::<16>().unwrap();
        let ga = aes::cipher::generic_array::GenericArray::from_mut_slice(&mut block);
        enc2.encrypt_block_mut(ga);
        let got2 = decrypt_aes128(&block, &key, &iv).unwrap();
        assert_eq!(&got2[..16], &plaintext[..16]);
    }

    #[test]
    fn detect_flavor_by_map_and_uri() {
        let mk = |uri: &str, map: Option<&str>| HlsSeg {
            uri: uri.into(),
            range: None,
            key: None,
            map: map.map(|m| m.to_string()),
        };
        assert_eq!(
            detect_flavor(&[mk("https://x/0.m4s", Some("https://x/init.mp4"))]),
            Flavor::Fmp4
        );
        assert_eq!(detect_flavor(&[mk("https://x/0.ts", None)]), Flavor::Ts);
    }

    #[test]
    fn build_segments_byterange_chain_and_keys() {
        let text = "#EXTM3U\n#EXT-X-VERSION:4\n#EXT-X-TARGETDURATION:10\n#EXT-X-MEDIA-SEQUENCE:100\n#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\",IV=0x000102030405060708090a0b0c0d0e0f\n#EXTINF:10,\n#EXT-X-BYTERANGE:1000@0\na.ts\n#EXTINF:10,\n#EXT-X-BYTERANGE:2000\na.ts\n";
        let media = match m3u8_rs::parse_playlist_res(text.as_bytes()).unwrap() {
            Playlist::MediaPlaylist(p) => p,
            _ => panic!("expected media playlist"),
        };
        {
            {
                let segs = build_segments(&media, "https://h/x/list.m3u8").expect("build_segments");
                assert_eq!(segs.len(), 2);
                assert_eq!(segs[0].range, Some((0, 1000)));
                assert_eq!(segs[1].range, Some((1000, 2000)), "offset 沿链推导");
                let (kuri, iv) = segs[0].key.clone().unwrap();
                assert_eq!(kuri, "https://h/x/key.bin");
                assert_eq!(iv[0], 0);
                assert_eq!(iv[15], 0x0f);
            }
        }
    }

    #[test]
    fn master_variants_extracted() {
        let text = "#EXTM3U
#EXT-X-STREAM-INF:BANDWIDTH=1280000,RESOLUTION=720x480
lo.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=2560000,RESOLUTION=1280x720
hi.m3u8
";
        let v = master_variants(text, "https://h/x/master.m3u8");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].0, 1280000);
        assert_eq!(v[1].1.as_deref(), Some("1280x720"));
        assert_eq!(v[1].2, "https://h/x/hi.m3u8");
        // 媒体清单 → 无变体
        assert!(master_variants(
            "#EXTM3U
#EXT-X-ENDLIST
",
            "https://h/x"
        )
        .is_empty());
    }

    #[test]
    fn live_playlist_detection_is_upstream_of_build() {
        // 无 ENDLIST → 上层报 LiveUnsupported；这里验证解析器能区分
        let live = "#EXTM3U\n#EXT-X-TARGETDURATION:10\n#EXTINF:10,\na.ts\n";
        let vod = "#EXTM3U\n#EXT-X-TARGETDURATION:10\n#EXT-X-ENDLIST\n#EXTINF:10,\na.ts\n";
        match m3u8_rs::parse_playlist_res(vod.as_bytes()).unwrap() {
            Playlist::MediaPlaylist(p) => assert!(p.end_list),
            _ => panic!(),
        }
        match m3u8_rs::parse_playlist_res(live.as_bytes()).unwrap() {
            Playlist::MediaPlaylist(p) => assert!(!p.end_list),
            _ => panic!(),
        }
    }
}
