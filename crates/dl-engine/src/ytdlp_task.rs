//! yt-dlp 任务驱动（yt-dlp 托管模式，docs/MEDIA-ADAPTERS.md §5.2 模式二）。
//!
//! 引擎把整条下载交给 yt-dlp 进程（含 PO Token/nsig 处理与音视频合并），
//! 进度行桥接为 EngineEvent::Progress；完成文件复用 finalize 的冲突处理与
//! 改名链路。解析模式（直链喂多线程引擎）留作后续优化——托管模式正确性
//! 由 yt-dlp 保证，不受平台风控变化影响。
//!
//! 平台识别：YouTube 之外扩展 X/Twitter、Bilibili、TikTok、抖音（域名 +
//! 结构化单作品 URL 判定，见 [`is_ytdlp_host`]）。这些平台的直链同样受
//! 登录态/签名/动态清单保护，统一交给 yt-dlp 应对。抖音的 yt-dlp 提取器
//! 依赖分享页 SSR 数据（a_bogus 签名场景少），公开视频可用；登录态内容
//! （喜欢/关注流）不在覆盖范围。

use std::sync::Arc;

use crate::driver::{drive_from, Ctx, DriveErr, TargetPaths};
use crate::engine::EngineEvent;
use dl_error::ErrorKind;
use dl_model::TaskStatus;

/// YouTube 视频页判定所需的媒体扩展名（产出回填与出模板双扩展名剥离共用）。
const MEDIA_EXTS: &[&str] = &[
    "mp4", "mkv", "webm", "mov", "m4a", "mp3", "opus", "ogg", "flac", "wav", "aac",
];

fn is_youtube_host(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    h == "youtu.be"
        || h == "youtube.com"
        || h.ends_with(".youtube.com")
        || h == "youtube-nocookie.com"
        || h.ends_with(".youtube-nocookie.com")
}

/// host 等于 bare 或其子域（www.x.com/m.x.com/vm.x.com…）。
fn host_is(host: &str, bare: &str) -> bool {
    host == bare || host.ends_with(&format!(".{bare}"))
}

/// yt-dlp 托管模式识别的域名：URL 建任务时的自动 kind 判定用（engine.rs）。
/// 各平台直链受风控/签名/动态清单保护，多线程引擎无法直接下，交给 yt-dlp。
pub(crate) fn is_ytdlp_host(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    is_youtube_host(&h)
        || host_is(&h, "x.com")
        || host_is(&h, "twitter.com")
        || host_is(&h, "bilibili.com")
        || h == "b23.tv"
        || host_is(&h, "tiktok.com")
        || host_is(&h, "douyin.com")
        || host_is(&h, "iesdouyin.com")
}

fn is_video_id(s: &str) -> bool {
    s.len() == 11
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// 结构化提取 11 位视频 ID：watch?v=（任意参数顺序）、youtu.be/<id>（含
/// 分享按钮带的 ?si=…）、/shorts|live|embed|v/<id>、m./music./youtube-nocookie
/// 子域。旧版靠「URL 里任意 11 位 token」瞎猜，youtu.be?id 场景（如
/// `youtu.be/dQw4w9WgXcQ?si=…`，token 变成 14 位的 `…?si`）提取不到，只能落
/// 时间戳文件名。
fn youtube_video_id(url: &str) -> Option<String> {
    let u = url::Url::parse(url).ok()?;
    if !u.host_str().is_some_and(is_youtube_host) {
        return None;
    }
    if let Some((_, v)) = u.query_pairs().find(|(k, _)| k == "v") {
        if is_video_id(&v) {
            return Some(v.into_owned());
        }
    }
    let segs: Vec<&str> = u.path().split('/').filter(|s| !s.is_empty()).collect();
    let candidate = match (u.host_str(), segs.as_slice()) {
        (Some(h), [id]) if h.eq_ignore_ascii_case("youtu.be") => Some(*id),
        (_, [key, id, ..]) if matches!(*key, "shorts" | "live" | "embed" | "v") => Some(*id),
        _ => None,
    }?;
    is_video_id(candidate).then(|| candidate.to_string())
}

/// 平台结构化单作品 id → (平台前缀, id)，task.filename 缺省时的命名来源：
/// - YouTube：[`youtube_video_id`]（watch?v=、youtu.be/、shorts/live/embed/v）
/// - X/Twitter：路径任意深度的 /status(es)?/<纯数字 id>（详情页/移动页形态）
/// - Bilibili：/video/BV…、/video/av<数字>（b23.tv 短链无 id → None 走时间戳）
/// - TikTok：/@user/video|photo/<数字 id>（vt.tiktok.com 短链同上）
fn platform_id(url: &str) -> Option<(&'static str, String)> {
    let u = url::Url::parse(url).ok()?;
    let host = u.host_str()?.to_ascii_lowercase();
    if is_youtube_host(&host) {
        return youtube_video_id(url).map(|id| ("youtube", id));
    }
    let segs: Vec<&str> = u.path().split('/').filter(|s| !s.is_empty()).collect();
    let after_key = |keys: &[&str]| -> Option<String> {
        let pos = segs.iter().position(|s| keys.contains(s))?;
        let id = *segs.get(pos + 1)?;
        id.chars()
            .all(|c| c.is_ascii_alphanumeric())
            .then(|| id.to_string())
    };
    if host_is(&host, "x.com") || host_is(&host, "twitter.com") {
        return after_key(&["status", "statuses"]).map(|id| ("x", id));
    }
    if host_is(&host, "bilibili.com") {
        let id = after_key(&["video"])?;
        // BV… 直接收；av<数字> 收；其余路径段（如分区页残留）不猜。
        // get(..2) 而非 [..2]：路径段以多字节字符开头时后者会 panic
        let is_av = match id.get(..2) {
            Some(p) => p.eq_ignore_ascii_case("av") && id[2..].chars().all(|c| c.is_ascii_digit()),
            None => false,
        };
        return (id.starts_with("BV") || is_av).then_some(("bili", id));
    }
    if host_is(&host, "tiktok.com") {
        let id = after_key(&["video", "photo"])?;
        return id
            .chars()
            .all(|c| c.is_ascii_digit())
            .then_some(("tiktok", id));
    }
    if host_is(&host, "douyin.com") || host_is(&host, "iesdouyin.com") {
        // /video/{id}、/note/{id}（图集）、/share/video|note/{id}（分享页）；
        // v.douyin.com 短链无 id → None（yt-dlp 跟随重定向）
        let id = after_key(&["video", "note"])?;
        return id
            .chars()
            .all(|c| c.is_ascii_digit())
            .then_some(("douyin", id));
    }
    None
}

/// 合集/频道/播放列表/用户主页 URL：单文件任务无法表达（所有条目写同一个
/// 输出名，互相覆盖只剩最后一个视频），直接报错而不是静默浪费带宽。
/// watch?v=X&list=Y 有具体视频 → --no-playlist 已处理，不算合集。
/// 新平台同样只拦明确的主页形态：X 单段路径（用户时间线）、B 站用户空间、
/// TikTok @user 主页——未识别的页面交给 yt-dlp 自己报错。
fn is_batch_url(url: &str) -> bool {
    let Ok(u) = url::Url::parse(url) else {
        return false;
    };
    let Some(host) = u.host_str().map(|h| h.to_ascii_lowercase()) else {
        return false;
    };
    if is_youtube_host(&host) {
        if platform_id(url).is_some() {
            return false;
        }
        let path = u.path().to_ascii_lowercase();
        let has_list = u.query_pairs().any(|(k, _)| k == "list");
        return path == "/playlist"
            || has_list
            || path.starts_with("/channel/")
            || path.starts_with("/c/")
            || path.starts_with("/user/")
            || path.starts_with("/@");
    }
    let segs: Vec<&str> = u.path().split('/').filter(|s| !s.is_empty()).collect();
    if host_is(&host, "x.com") || host_is(&host, "twitter.com") {
        // /{user} 主页时间线；/i、/search 等功能页放行给 yt-dlp 自然报错
        return segs.len() == 1
            && !matches!(
                segs[0],
                "i" | "search" | "hashtag" | "explore" | "notifications" | "messages" | "settings"
            );
    }
    if host_is(&host, "bilibili.com") {
        return host == "space.bilibili.com" || u.path().to_ascii_lowercase().starts_with("/space");
    }
    if host_is(&host, "tiktok.com") {
        return segs.len() == 1 && segs[0].starts_with('@');
    }
    if host_is(&host, "douyin.com") || host_is(&host, "iesdouyin.com") {
        // /user/{sec_uid} 主页；根路径（推荐流）与 live 放行给 yt-dlp 报错
        return u.path().to_ascii_lowercase().starts_with("/user");
    }
    false
}

/// URL → 作品基名（task.filename 优先且经 sanitize）。
/// 平台结构化 id 优先（youtube_{id} / x_{id} / bili_{id} / tiktok_{id} /
/// douyin_{id}）；结构化解析失败（含手动指定 Ytdlp 的任意站点）再退 11 位
/// token 启发式，最后退 yt_{时间戳}。
fn base_name(filename: Option<&str>, url: &str, fallback_ms: i64) -> String {
    if let Some(f) = filename.map(str::trim).filter(|f| !f.is_empty()) {
        // 引擎此前直接拿 filename 拼 out 路径：`..\..\x` 会路径穿越、非法字符
        // 在 Windows 上 spawn/落盘失败。与 HTTP 驱动（pick_name）对齐做清洗。
        return dl_filename::sanitize(f);
    }
    let id = platform_id(url)
        .map(|(plat, id)| format!("{plat}_{id}"))
        .or_else(|| {
            url.split(['/', '=', '?', '&'])
                .find(|s| is_video_id(s))
                .map(|s| format!("youtube_{s}"))
        })
        .unwrap_or_else(|| format!("youtube_yt_{fallback_ms}"));
    id
}

/// 剥离文件名末尾的媒体扩展名得到 yt-dlp 输出基名：模板交给 yt-dlp 后由它
/// 追加真实扩展名——不剥的话 `video.mp4` 会产出 `video.mp4.mp4`（或容器不是
/// mp4 时变成 `video.mp4.webm` 这类扩展名与内容不符的文件）。
/// 非媒体扩展名（用户就想叫 `archive.webp`）不剥；`.mp4`（纯扩展名文件名）
/// 的 stem 为空则原样保留。
fn out_stem(name: &str) -> String {
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    match ext {
        Some(e) if MEDIA_EXTS.contains(&e.as_str()) => {
            let stem = &name[..name.len() - e.len() - 1];
            if stem.is_empty() {
                name.to_string()
            } else {
                stem.to_string()
            }
        }
        _ => name.to_string(),
    }
}

pub(crate) async fn run_ytdlp(ctx: Arc<Ctx>, task: &dl_model::Task) -> Result<(), DriveErr> {
    // 合集/主页链接先快速失败，避免 yt-dlp 拉下几十个视频全写同一个文件名
    if is_batch_url(&ctx.url) {
        return Err(DriveErr::Fatal(
            ErrorKind::InvalidUrl,
            "合集/频道/播放列表/用户主页链接无法作为单文件任务下载（只会互相覆盖、只剩最后一个视频）：请打开具体作品页（如 watch?v=…、/status/…、/video/BV…）后再试"
                .into(),
        ));
    }

    // 状态机入口：Queued（新任务）与 WaitingRetry（重试退避残留）走 Probing 直通
    // （无网络请求）；Paused（续传）与 Downloading（崩溃残留）直接进 Downloading
    // ——迁移表不允许 Paused/Downloading → Probing，此前无条件走探测直通导致
    // 暂停后点「开始」静默失败（InvalidTransition 只落在后台重试循环的日志里）。
    match task.status {
        TaskStatus::Queued | TaskStatus::WaitingRetry => {
            ctx.engine
                .transition(&ctx.task_id, TaskStatus::Probing, None)
                .await
                .map_err(drive_from)?;
        }
        TaskStatus::Paused | TaskStatus::Downloading => {}
        s => {
            return Err(DriveErr::Fatal(
                ErrorKind::InvalidTransition {
                    from: s.as_str().into(),
                    to: "downloading".into(),
                },
                format!("cannot download from state {}", s.as_str()),
            ));
        }
    }
    ctx.engine
        .transition(&ctx.task_id, TaskStatus::Downloading, None)
        .await
        .map_err(drive_from)?;

    let dir = task
        .dir
        .clone()
        .unwrap_or_else(|| ctx.opts.dir.display().to_string());
    std::fs::create_dir_all(&dir).map_err(|e| {
        DriveErr::Fatal(
            ErrorKind::FileAccessDenied,
            format!("create download dir {dir}: {e}"),
        )
    })?;
    let raw_name = base_name(task.filename.as_deref(), &task.url, dl_model::now_ms());
    let stem = out_stem(&raw_name);
    let out_path = std::path::Path::new(&dir).join(&stem);

    // 解析链：设置显式路径 > 应用目录内置副本（安装器自带）> PATH
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|x| x.to_path_buf()));
    let bundled = exe_dir
        .as_ref()
        .map(|d| d.join("yt-dlp.exe"))
        .filter(|p| p.exists());
    let (exe, via_setting) = match ctx
        .engine
        .get_setting("yt_dlp_path")
        .await
        .map_err(drive_from)?
        .filter(|s| !s.trim().is_empty())
    {
        Some(p) => (p, true),
        None => match bundled {
            Some(p) => (p.display().to_string(), false),
            None => ("yt-dlp".into(), false),
        },
    };
    if !dl_ytdlp::available(&exe).await {
        let hint = if via_setting {
            format!("yt-dlp 不可用（{exe}）：请检查设置中的路径")
        } else {
            "yt-dlp 不可用：请在设置中配置 yt-dlp 路径".into()
        };
        return Err(DriveErr::Fatal(ErrorKind::Internal, hint));
    }
    let ffmpeg_dir = ctx
        .engine
        .get_setting("ffmpeg_path")
        .await
        .map_err(drive_from)?
        .filter(|s| !s.trim().is_empty())
        .map(std::path::PathBuf::from)
        .and_then(|p| p.parent().map(|x| x.to_path_buf()));

    // PATH 注入：内置目录（yt-dlp/deno 同目录）、ffmpeg 目录
    let mut path_prepend = Vec::new();
    if let Some(d) = &exe_dir {
        if d.join("yt-dlp.exe").exists() || d.join("deno.exe").exists() {
            path_prepend.push(d.clone());
        }
    }
    if let Some(ff) = &ffmpeg_dir {
        path_prepend.push(ff.clone());
    }

    // 透传引擎选项：代理（YouTube 场景常配代理，否则 yt-dlp 子进程直连失败）
    // 与任务级限速——HTTP 驱动有 Limiter，托管进程只能靠 yt-dlp 自己的参数
    let mut extra_args: Vec<String> = Vec::new();
    if let Some(proxy) = ctx
        .opts
        .proxy
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        extra_args.push("--proxy".into());
        extra_args.push(proxy.to_string());
    }
    if let Some(bps) = [ctx.opts.task_bps, ctx.opts.global_bps]
        .into_iter()
        .flatten()
        .filter(|v| *v > 0)
        .min()
    {
        extra_args.push("--limit-rate".into());
        extra_args.push(bps.to_string());
    }

    let spec = dl_ytdlp::YtdlpSpec {
        url: ctx.url.clone(),
        exe,
        out_path,
        ffmpeg_dir,
        extra_args,
        path_prepend,
    };

    let ectx = ctx.clone();
    let result = dl_ytdlp::run(spec, ctx.cancel.clone(), move |ev| match ev {
        dl_ytdlp::YtdlpEvent::Progress {
            downloaded,
            total,
            bps,
        } => {
            ectx.engine.emit(EngineEvent::Progress {
                task_id: ectx.task_id.clone(),
                downloaded,
                total,
                bps,
                segments: None,
            });
        }
        dl_ytdlp::YtdlpEvent::Log(l) => {
            tracing::info!(target: "ytdlp", "{l}");
        }
    })
    .await;

    let produced = match result {
        Ok(p) => p,
        Err(dl_ytdlp::YtdlpError::Canceled) => return Err(DriveErr::Paused),
        Err(dl_ytdlp::YtdlpError::Failed(msg)) => {
            return Err(DriveErr::Fatal(
                ErrorKind::Internal,
                format!("yt-dlp: {msg}"),
            ))
        }
    };
    // 取消竞态护栏：cancel 恰在 yt-dlp 成功退出后、收尾前触发时，与 HTTP 驱动
    // 一致按 Paused 处理——不再改名/校验，避免对 Canceled 意图的任务继续走完
    // 状态机（Probing/Downloading 之外的状态迁移会被拒）。
    if ctx.cancel.is_cancelled() {
        return Err(DriveErr::Paused);
    }

    // 产出扩展名回填最终名：yt-dlp 决定容器格式（mp4/mkv/webm…），任务名
    // 只提供 stem，保证「显示名 = 落盘名 = 真实格式」。
    let ext = produced
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    let final_name = if ext.is_empty() {
        stem.clone()
    } else {
        format!("{stem}.{ext}")
    };

    // 产出 → .dlpart → 复用 finalize（冲突处理/改名/journal 清理一致）
    let journal = ctx
        .engine
        .journal_dir()
        .join(format!("{}.dlctrl", ctx.task_id));
    let paths = TargetPaths {
        dir: std::path::PathBuf::from(&dir),
        name: final_name,
        part: std::path::Path::new(&dir).join(format!("{stem}.dlpart")),
        journal,
    };
    if produced != paths.part {
        std::fs::rename(&produced, &paths.part).map_err(|e| {
            DriveErr::Fatal(
                ErrorKind::FileAccessDenied,
                format!("{} → {}: {e}", produced.display(), paths.part.display()),
            )
        })?;
    }

    ctx.engine
        .transition(&ctx.task_id, TaskStatus::Verifying, None)
        .await
        .map_err(drive_from)?;
    // 校验和（可选）：针对 yt-dlp 产出的最终文件
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
    crate::driver::finalize(&ctx, &paths).await?;
    ctx.engine
        .transition(&ctx.task_id, TaskStatus::Done, None)
        .await
        .map_err(drive_from)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_id_structured_forms() {
        for url in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            // v= 不在首位也取得到
            "https://www.youtube.com/watch?t=42&v=dQw4w9WgXcQ&list=PLabc",
            // 分享短链 + si= 追踪参数（分享按钮默认形态）
            "https://youtu.be/dQw4w9WgXcQ?si=AbC_123-xyz",
            "https://www.youtube.com/live/dQw4w9WgXcQ?feature=share",
            "https://www.youtube.com/embed/dQw4w9WgXcQ",
            "https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ",
            "https://music.youtube.com/watch?v=dQw4w9WgXcQ&list=RDAMVM",
            "https://www.youtube.com/v/dQw4w9WgXcQ",
        ] {
            assert_eq!(
                youtube_video_id(url).as_deref(),
                Some("dQw4w9WgXcQ"),
                "{url}"
            );
        }
        for url in [
            "https://www.youtube.com/shorts/abcdefghijk",
            "https://m.youtube.com/shorts/abcdefghijk?feature=share",
            "https://youtu.be/abcdefghijk",
        ] {
            assert_eq!(
                youtube_video_id(url).as_deref(),
                Some("abcdefghijk"),
                "{url}"
            );
        }
    }

    #[test]
    fn video_id_rejects_non_video_urls() {
        assert_eq!(youtube_video_id("https://vimeo.com/dQw4w9WgXcQ"), None);
        assert_eq!(
            youtube_video_id("https://www.youtube.com/playlist?list=PLxyz123"),
            None
        );
        // 11 位 token 但不在视频位（无 v=、非 shorts/live/embed 路径）：不猜
        assert_eq!(
            youtube_video_id("https://www.youtube.com/watch?t=abcdefghijk"),
            None
        );
        assert_eq!(youtube_video_id("not a url"), None);
    }

    #[test]
    fn base_name_prefers_sanitized_filename() {
        assert_eq!(
            base_name(Some("  我的视频.mp4  "), "https://youtu.be/dQw4w9WgXcQ", 1),
            dl_filename::sanitize("我的视频.mp4")
        );
        // 空白文件名 = 未设置，走 URL 提取
        assert_eq!(
            base_name(Some("   "), "https://youtu.be/dQw4w9WgXcQ", 1),
            "youtube_dQw4w9WgXcQ"
        );
    }

    #[test]
    fn base_name_covers_youtu_be_shorts_live() {
        assert_eq!(
            base_name(None, "https://youtu.be/dQw4w9WgXcQ?si=x", 1),
            "youtube_dQw4w9WgXcQ"
        );
        assert_eq!(
            base_name(None, "https://www.youtube.com/shorts/abcdefghijk", 1),
            "youtube_abcdefghijk"
        );
        assert_eq!(
            base_name(
                None,
                "https://www.youtube.com/live/dQw4w9WgXcQ?feature=share",
                1
            ),
            "youtube_dQw4w9WgXcQ"
        );
    }

    #[test]
    fn base_name_heuristic_and_timestamp_fallbacks() {
        // 非 youtube 域名（用户手动指定 Ytdlp 类型）：11 位 token 启发式兜底
        assert_eq!(
            base_name(None, "https://example.com/dQw4w9WgXcQ", 5),
            "youtube_dQw4w9WgXcQ"
        );
        assert_eq!(base_name(None, "not a url", 42), "youtube_yt_42");
    }

    #[test]
    fn batch_urls_detected() {
        for url in [
            "https://www.youtube.com/playlist?list=PLabc",
            "https://music.youtube.com/playlist?list=OLAK5uy_abc",
            // watch 页但缺 v=：只有合集
            "https://www.youtube.com/watch?list=PLabc",
            "https://www.youtube.com/@somehandle/videos",
            "https://www.youtube.com/channel/UCabcd123456/videos",
            "https://www.youtube.com/c/somechannel/videos",
            "https://www.youtube.com/user/someuser/videos",
        ] {
            assert!(is_batch_url(url), "{url}");
        }
    }

    #[test]
    fn single_video_with_list_param_is_not_batch() {
        for url in [
            // --no-playlist 的正常工作范围：有具体视频
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=PLabc",
            "https://youtu.be/dQw4w9WgXcQ?si=x&list=PLabc",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            // 非 youtube 域名不适用该规则
            "https://example.com/playlist?list=PLabc",
        ] {
            assert!(!is_batch_url(url), "{url}");
        }
    }

    #[test]
    fn out_stem_strips_media_extension_only() {
        assert_eq!(out_stem("video.mp4"), "video");
        assert_eq!(out_stem("video.MP4"), "video");
        assert_eq!(out_stem("_video.m4a"), "_video");
        assert_eq!(out_stem("song.flac"), "song");
        // 无扩展名 / 非媒体扩展名 / 纯扩展名文件名：原样
        assert_eq!(out_stem("youtube_dQw4w9WgXcQ"), "youtube_dQw4w9WgXcQ");
        assert_eq!(out_stem("archive.webp"), "archive.webp");
        assert_eq!(out_stem(".mp4"), ".mp4");
    }

    #[test]
    fn platform_ids_extracted() {
        // X：详情页 / 移动页 / statuses 旧形态
        assert_eq!(
            platform_id("https://x.com/testuser/status/1700000000000000001"),
            Some(("x", "1700000000000000001".into()))
        );
        assert_eq!(
            platform_id("https://mobile.twitter.com/i/web/statuses/12345"),
            Some(("x", "12345".into()))
        );
        // Bilibili：BV 号与 av 号
        assert_eq!(
            platform_id("https://www.bilibili.com/video/BV1xx411c7mD"),
            Some(("bili", "BV1xx411c7mD".into()))
        );
        assert_eq!(
            platform_id("https://www.bilibili.com/video/av170001?p=1"),
            Some(("bili", "av170001".into()))
        );
        // 多字节路径段不 panic（曾用 [..2] 切片）、非作品页不猜
        assert_eq!(platform_id("https://www.bilibili.com/video/分区"), None);
        // TikTok：video 与 photo（图文帖）
        assert_eq!(
            platform_id("https://www.tiktok.com/@someuser/video/7301234567890123456"),
            Some(("tiktok", "7301234567890123456".into()))
        );
        assert_eq!(
            platform_id("https://www.tiktok.com/@someuser/photo/7301234567890123456"),
            Some(("tiktok", "7301234567890123456".into()))
        );
        // 短链无 id → None（命名走时间戳，重定向由 yt-dlp 跟随）
        assert_eq!(platform_id("https://b23.tv/abc123"), None);
        assert_eq!(platform_id("https://vt.tiktok.com/abc123/"), None);
        // 抖音：video/note/share 页形态；短链无 id
        assert_eq!(
            platform_id("https://www.douyin.com/video/7301234567890123456"),
            Some(("douyin", "7301234567890123456".into()))
        );
        assert_eq!(
            platform_id("https://www.douyin.com/note/7301234567890123456"),
            Some(("douyin", "7301234567890123456".into()))
        );
        assert_eq!(
            platform_id("https://www.iesdouyin.com/share/video/7301234567890123456"),
            Some(("douyin", "7301234567890123456".into()))
        );
        assert_eq!(platform_id("https://v.douyin.com/abcXYZ/"), None);
        // 含 x.com 子串的站点不误判
        assert_eq!(platform_id("https://netflix.com/title/123"), None);
    }

    #[test]
    fn base_name_per_platform() {
        assert_eq!(
            base_name(None, "https://x.com/testuser/status/1700000000000000001", 1),
            "x_1700000000000000001"
        );
        assert_eq!(
            base_name(None, "https://www.bilibili.com/video/BV1xx411c7mD", 1),
            "bili_BV1xx411c7mD"
        );
        assert_eq!(
            base_name(
                None,
                "https://www.tiktok.com/@u/video/7301234567890123456",
                1
            ),
            "tiktok_7301234567890123456"
        );
        assert_eq!(
            base_name(None, "https://www.douyin.com/video/7301234567890123456", 1),
            "douyin_7301234567890123456"
        );
        // b23.tv 短链：结构化失败 → 时间戳兜底（保留旧前缀行为）
        assert_eq!(base_name(None, "https://b23.tv/abc123", 7), "youtube_yt_7");
    }

    #[test]
    fn batch_urls_non_youtube() {
        for url in [
            "https://x.com/testuser",
            "https://twitter.com/testuser",
            "https://space.bilibili.com/123456",
            "https://www.bilibili.com/space/bilibili",
            "https://www.tiktok.com/@someuser",
            "https://www.douyin.com/user/MS4wLjABAAAAabcd1234",
        ] {
            assert!(is_batch_url(url), "{url}");
        }
        for url in [
            "https://x.com/testuser/status/1700000000000000001",
            // 空间音频等 /i/ 功能路径放行给 yt-dlp
            "https://x.com/i/spaces/1vOxwrabP",
            "https://www.bilibili.com/video/BV1xx411c7mD",
            "https://www.tiktok.com/@someuser/video/7301234567890123456",
            "https://www.douyin.com/video/7301234567890123456",
            "https://www.douyin.com/note/7301234567890123456",
            "https://www.iesdouyin.com/share/video/7301234567890123456",
        ] {
            assert!(!is_batch_url(url), "{url}");
        }
    }
}
