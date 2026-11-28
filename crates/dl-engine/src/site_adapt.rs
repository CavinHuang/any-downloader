//! 常用站点深度适配（参照 Ghost-Downloader-3 Feature Packs）：
//! - Hugging Face：镜像站加速（默认 hf-mirror.com，双源互备）、Token 鉴权、整仓/子目录解析与自动 SHA-256 校验；
//! - GitHub：多镜像代理并发测速选优 + 主备 Failover + Releases 资产清单展开（含 digest 提取）；
//! - ModelScope（魔搭）：模型/数据集文件树解析与自动 SHA-256 校验；
//! - Bilibili：`b23.tv` 短链还原、多 P / 合集（`ugc_season`）/ 列表 / 收藏夹展开为队列条目。

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use dl_error::{DownloadError, ErrorKind, Res};
use dl_model::CreateTask;
use serde::{Deserialize, Serialize};
use tokio::task::JoinSet;

use crate::Engine;

pub const DEFAULT_HF_MIRROR: &str = "https://hf-mirror.com";
const HF_PROBE_TARGET: &str = "huggingface.co/api/models/gpt2";
const GH_PROBE_TARGET: &str =
    "https://github.com/cli/cli/releases/download/v2.62.0/gh_2.62.0_linux_amd64.tar.gz";

pub const GITHUB_PROXY_SITES: &[&str] = &[
    "https://gh-proxy.org",
    "https://gh-proxy.com",
    "https://cdn.gh-proxy.org",
    "https://edgeone.gh-proxy.org",
    "https://hk.gh-proxy.org",
    "https://ghfast.top",
    "https://wget.la",
];

static CACHED_BEST_GH_PROXY: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn best_gh_proxy_cache() -> &'static Mutex<Option<String>> {
    CACHED_BEST_GH_PROXY.get_or_init(|| Mutex::new(None))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteFileItem {
    pub relative_path: String,
    pub filename: String,
    pub url: String,
    pub size: Option<u64>,
    pub checksum: Option<String>,
    pub mirrors: Vec<String>,
    pub headers: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirrorLatency {
    pub site: String,
    pub group: String,
    pub latency_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SiteInspectResult {
    Files {
        site: String,
        title: String,
        subtitle: Option<String>,
        folder_name: Option<String>,
        files: Vec<SiteFileItem>,
    },
    Playlist {
        site: String,
        title: String,
        total_count: usize,
        entries: Vec<dl_ytdlp::PlaylistEntry>,
    },
    FallbackYtdlp {
        url: String,
    },
}

// ---------------------------------------------------------------- URL 解析

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HfRef {
    pub repo_type: String, // "model" | "dataset" | "space"
    pub org: String,
    pub repo: String,
    pub revision: String,
    pub action: Option<String>, // None | "resolve" | "blob" | "tree"
    pub file_path: String,
}

impl HfRef {
    pub fn repo_id(&self) -> String {
        format!("{}/{}", self.org, self.repo)
    }

    pub fn api_prefix(&self) -> &'static str {
        match self.repo_type.as_str() {
            "dataset" => "datasets",
            "space" => "spaces",
            _ => "models",
        }
    }

    pub fn url_prefix(&self) -> String {
        match self.repo_type.as_str() {
            "dataset" => format!("datasets/{}/{}", self.org, self.repo),
            "space" => format!("spaces/{}/{}", self.org, self.repo),
            _ => format!("{}/{}", self.org, self.repo),
        }
    }
}

pub fn normalize_site_origin(raw: &str) -> Option<String> {
    let s = raw.trim().trim_end_matches('/');
    if s.is_empty() {
        return None;
    }
    let with_scheme = if s.contains("://") {
        s.to_string()
    } else {
        format!("https://{s}")
    };
    let u = url::Url::parse(&with_scheme).ok()?;
    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() {
        return None;
    }
    Some(format!(
        "{}://{}{}",
        u.scheme(),
        u.host_str()?,
        u.port().map(|p| format!(":{p}")).unwrap_or_default()
    ))
}

fn host_matches_origin(host: &str, origin: &str) -> bool {
    url::Url::parse(origin)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.eq_ignore_ascii_case(host)))
        .unwrap_or(false)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn encode_uri_path(path: &str) -> String {
    path.split('/')
        .filter(|s| !s.is_empty() && *s != "..")
        .map(|seg| {
            let mut enc = String::with_capacity(seg.len());
            for &b in seg.as_bytes() {
                if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                    enc.push(b as char);
                } else {
                    enc.push_str(&format!("%{b:02X}"));
                }
            }
            enc
        })
        .collect::<Vec<_>>()
        .join("/")
}

pub fn parse_hf_url(raw: &str, custom_mirror: Option<&str>) -> Option<HfRef> {
    let u = url::Url::parse(raw).ok()?;
    if !matches!(u.scheme(), "http" | "https") {
        return None;
    }
    let host = u.host_str()?.to_ascii_lowercase();
    let bare = host.strip_prefix("www.").unwrap_or(&host);
    let known_host = matches!(bare, "huggingface.co" | "hf-mirror.com")
        || custom_mirror
            .and_then(normalize_site_origin)
            .is_some_and(|o| host_matches_origin(&host, &o));
    if !known_host {
        return None;
    }

    let segs: Vec<&str> = u.path().split('/').filter(|s| !s.is_empty()).collect();
    if segs.is_empty() {
        return None;
    }
    const RESERVED_ROOTS: &[&str] = &[
        "settings",
        "models",
        "datasets",
        "spaces",
        "docs",
        "login",
        "join",
        "blog",
        "papers",
        "collections",
        "tasks",
        "enterprise",
        "pricing",
        "api",
    ];
    let (repo_type, idx) = match segs[0] {
        "datasets" => ("dataset".to_string(), 1usize),
        "spaces" => ("space".to_string(), 1usize),
        first if RESERVED_ROOTS.contains(&first) => return None,
        _ => ("model".to_string(), 0usize),
    };
    let org = (*segs.get(idx)?).to_string();
    let repo = (*segs.get(idx + 1)?).to_string();
    if org.is_empty() || repo.is_empty() {
        return None;
    }

    let mut revision = "main".to_string();
    let mut action: Option<String> = None;
    let mut file_path = String::new();
    if let Some(&act) = segs.get(idx + 2) {
        if matches!(act, "resolve" | "blob" | "tree") {
            action = Some(act.to_string());
            if let Some(&rev) = segs.get(idx + 3) {
                revision = percent_decode(rev);
                if segs.len() > idx + 4 {
                    file_path = percent_decode(&segs[idx + 4..].join("/"));
                }
            }
        }
    }

    Some(HfRef {
        repo_type,
        org,
        repo,
        revision,
        action,
        file_path,
    })
}

const GITHUB_FILE_HOSTS: &[&str] = &[
    "api.github.com",
    "codeload.github.com",
    "gist.github.com",
    "gist.githubusercontent.com",
    "github-releases.githubusercontent.com",
    "media.githubusercontent.com",
    "objects.githubusercontent.com",
    "raw.githubusercontent.com",
    "raw.github.com",
    "release-assets.githubusercontent.com",
];

pub fn is_github_file_url(raw: &str) -> bool {
    let Ok(u) = url::Url::parse(raw) else {
        return false;
    };
    if !matches!(u.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = u.host_str().map(|h| h.to_ascii_lowercase()) else {
        return false;
    };
    let bare = host.strip_prefix("www.").unwrap_or(&host);
    if GITHUB_FILE_HOSTS.contains(&bare) {
        return true;
    }
    if bare != "github.com" {
        return false;
    }
    let path = u.path().to_ascii_lowercase();
    path.contains("/archive/")
        || path.contains("/raw/")
        || path.contains("/blob/")
        || path.contains("/releases/download/")
        || path.contains("/releases/latest/download/")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubReleaseRef {
    pub owner: String,
    pub repo: String,
    pub tag: Option<String>,
}

pub fn parse_github_release_url(raw: &str) -> Option<GithubReleaseRef> {
    let u = url::Url::parse(raw).ok()?;
    if !matches!(u.scheme(), "http" | "https") {
        return None;
    }
    let host = u.host_str()?.to_ascii_lowercase();
    let bare = host.strip_prefix("www.").unwrap_or(&host);
    if bare != "github.com" {
        return None;
    }
    let segs: Vec<&str> = u.path().split('/').filter(|s| !s.is_empty()).collect();
    if segs.len() < 3 || segs[2] != "releases" {
        return None;
    }
    let owner = segs[0].to_string();
    let repo = segs[1].to_string();
    match segs.get(3).copied() {
        None | Some("latest") => Some(GithubReleaseRef {
            owner,
            repo,
            tag: None,
        }),
        Some("tag") => {
            let tag = segs
                .get(4)
                .map(|s| percent_decode(s))
                .filter(|s| !s.is_empty())?;
            Some(GithubReleaseRef {
                owner,
                repo,
                tag: Some(tag),
            })
        }
        _ => None, // `/releases/download/...` 是具体文件直链，不作为 Releases 页解析
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelScopeRef {
    pub repo_type: String, // "models" | "datasets"
    pub org: String,
    pub repo: String,
    pub revision: String,
    pub file_path: String,
}

pub fn parse_modelscope_url(raw: &str) -> Option<ModelScopeRef> {
    let u = url::Url::parse(raw).ok()?;
    if !matches!(u.scheme(), "http" | "https") {
        return None;
    }
    let host = u.host_str()?.to_ascii_lowercase();
    let bare = host.strip_prefix("www.").unwrap_or(&host);
    if bare != "modelscope.cn" {
        return None;
    }
    let segs: Vec<&str> = u.path().split('/').filter(|s| !s.is_empty()).collect();
    if segs.len() < 3 || !matches!(segs[0], "models" | "datasets") {
        return None;
    }
    let repo_type = segs[0].to_string();
    let org = segs[1].to_string();
    let repo = segs[2].to_string();
    let mut revision = "master".to_string();
    let mut file_path = String::new();
    if segs.len() >= 6 && segs[3] == "file" && segs[4] == "view" {
        revision = percent_decode(segs[5]);
        if segs.len() > 6 {
            file_path = percent_decode(&segs[6..].join("/"));
        }
    } else if segs.len() >= 5 && segs[3] == "resolve" {
        revision = percent_decode(segs[4]);
        if segs.len() > 5 {
            file_path = percent_decode(&segs[5..].join("/"));
        }
    }
    Some(ModelScopeRef {
        repo_type,
        org,
        repo,
        revision,
        file_path,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BiliRef {
    Short(String),
    Video {
        bvid: Option<String>,
        aid: Option<u64>,
    },
    Season {
        mid: u64,
        season_id: u64,
    },
    Favorite {
        fid: u64,
    },
}

pub fn parse_bili_url(raw: &str) -> Option<BiliRef> {
    let u = url::Url::parse(raw).ok()?;
    if !matches!(u.scheme(), "http" | "https") {
        return None;
    }
    let host = u.host_str()?.to_ascii_lowercase();
    if host == "b23.tv" || host.ends_with(".b23.tv") {
        return Some(BiliRef::Short(raw.to_string()));
    }
    if host != "bilibili.com" && !host.ends_with(".bilibili.com") {
        return None;
    }
    let segs: Vec<&str> = u.path().split('/').filter(|s| !s.is_empty()).collect();
    // /video/BV... 或 /video/av...
    if let Some(pos) = segs.iter().position(|&s| s == "video") {
        if let Some(&id) = segs.get(pos + 1) {
            if id.starts_with("BV") {
                return Some(BiliRef::Video {
                    bvid: Some(id.to_string()),
                    aid: None,
                });
            }
            if let Some(num) = id
                .get(..2)
                .filter(|p| p.eq_ignore_ascii_case("av"))
                .and_then(|_| id[2..].parse::<u64>().ok())
            {
                return Some(BiliRef::Video {
                    bvid: None,
                    aid: Some(num),
                });
            }
        }
    }
    // /{mid}/lists/{season_id}
    if segs.len() >= 3 && segs[1] == "lists" {
        if let (Ok(mid), Ok(sid)) = (segs[0].parse::<u64>(), segs[2].parse::<u64>()) {
            return Some(BiliRef::Season {
                mid,
                season_id: sid,
            });
        }
    }
    // /{mid}/favlist?fid=... (ctype=21 为合集，否则为收藏夹)
    if segs.len() >= 2 && segs[1] == "favlist" {
        if let Ok(mid) = segs[0].parse::<u64>() {
            let fid = u
                .query_pairs()
                .find(|(k, _)| k == "fid")
                .and_then(|(_, v)| v.parse::<u64>().ok())?;
            let ctype = u
                .query_pairs()
                .find(|(k, _)| k == "ctype")
                .map(|(_, v)| v.into_owned())
                .unwrap_or_default();
            if ctype == "21" {
                return Some(BiliRef::Season {
                    mid,
                    season_id: fid,
                });
            }
            return Some(BiliRef::Favorite { fid });
        }
    }
    // /list/ml{fid} 或 /medialist/detail/ml{fid}
    if let Some(last) = segs.last() {
        if let Some(rest) = last.strip_prefix("ml") {
            if let Ok(fid) = rest.parse::<u64>() {
                return Some(BiliRef::Favorite { fid });
            }
        }
    }
    // /{mid}/channel/collectiondetail?sid=...
    if segs.len() >= 3 && segs[1] == "channel" && segs[2] == "collectiondetail" {
        if let Ok(mid) = segs[0].parse::<u64>() {
            let sid = u
                .query_pairs()
                .find(|(k, _)| k == "sid")
                .and_then(|(_, v)| v.parse::<u64>().ok())?;
            return Some(BiliRef::Season {
                mid,
                season_id: sid,
            });
        }
    }
    None
}

// ---------------------------------------------------------------- 创建任务自适应

fn has_sensitive_auth_header(headers: &Option<HashMap<String, String>>) -> bool {
    headers.as_ref().is_some_and(|m| {
        m.keys()
            .any(|k| matches!(k.to_ascii_lowercase().as_str(), "authorization" | "cookie"))
    })
}

fn push_unique_mirror(mirrors: &mut Option<Vec<String>>, primary: &str, candidate: String) {
    if candidate == primary {
        return;
    }
    let list = mirrors.get_or_insert_with(Vec::new);
    if !list.iter().any(|x| x == &candidate) && list.len() < 8 {
        list.push(candidate);
    }
}

/// 建任务前透明适配常用站点（HF 镜像/Token 注入、GitHub 代理镜像、ModelScope/GitLab/Gitee/arXiv 直链归一）。
pub(crate) async fn adapt_create_task(engine: &Engine, c: &mut CreateTask) {
    if let Some(norm) = dl_net::interstitial::normalize_download_url(&c.url) {
        c.url = norm;
    }

    let hf_mirror_raw = engine.get_setting("hf_mirror_site").await.ok().flatten();
    let custom_hf = hf_mirror_raw
        .as_deref()
        .and_then(normalize_site_origin)
        .unwrap_or_else(|| DEFAULT_HF_MIRROR.to_string());

    if let Some(hf) = parse_hf_url(&c.url, Some(&custom_hf)) {
        if matches!(hf.action.as_deref(), Some("resolve" | "blob")) && !hf.file_path.is_empty() {
            let mirror_on = engine
                .get_setting("hf_mirror_enabled")
                .await
                .ok()
                .flatten()
                .map(|v| v != "off")
                .unwrap_or(true);
            let encoded = encode_uri_path(&hf.file_path);
            let canonical = format!(
                "https://huggingface.co/{}/resolve/{}/{}",
                hf.url_prefix(),
                hf.revision,
                encoded
            );
            let mirrored = format!(
                "{}/{}/resolve/{}/{}",
                custom_hf,
                hf.url_prefix(),
                hf.revision,
                encoded
            );
            if mirror_on {
                c.url = mirrored.clone();
                push_unique_mirror(&mut c.mirrors, &c.url, canonical);
            } else {
                c.url = canonical.clone();
                push_unique_mirror(&mut c.mirrors, &c.url, mirrored);
            }
            if let Ok(Some(tok)) = engine.get_setting("hf_token").await {
                let tok = tok.trim();
                if !tok.is_empty() {
                    let hdrs = c.headers.get_or_insert_with(HashMap::new);
                    let has_auth = hdrs.keys().any(|k| k.eq_ignore_ascii_case("authorization"));
                    if !has_auth {
                        hdrs.insert("authorization".to_string(), format!("Bearer {tok}"));
                    }
                }
            }
            return;
        }
    }

    if is_github_file_url(&c.url) && !has_sensitive_auth_header(&c.headers) {
        let gh_on = engine
            .get_setting("github_mirror_enabled")
            .await
            .ok()
            .flatten()
            .map(|v| v == "on")
            .unwrap_or(false);
        if gh_on {
            let selected = engine
                .get_setting("github_mirror_site")
                .await
                .ok()
                .flatten()
                .unwrap_or_else(|| "auto".into());
            let custom = engine
                .get_setting("github_mirror_custom")
                .await
                .ok()
                .flatten();
            let original = c.url.clone();
            let ordered_sites = build_github_proxy_order(&selected, custom.as_deref());
            if let Some(first) = ordered_sites.first() {
                c.url = format!("{first}/{original}");
                for s in ordered_sites.iter().skip(1).take(3) {
                    push_unique_mirror(&mut c.mirrors, &c.url, format!("{s}/{original}"));
                }
                push_unique_mirror(&mut c.mirrors, &c.url, original);
            }
        }
    }
}

pub(crate) fn build_github_proxy_order(selected: &str, custom: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    let sel = selected.trim();
    if sel == "custom" {
        if let Some(c) = custom.and_then(normalize_site_origin) {
            out.push(c);
        }
    } else if sel != "auto" && !sel.is_empty() {
        if let Some(s) = normalize_site_origin(sel) {
            out.push(s);
        }
    } else if let Some(cached) = best_gh_proxy_cache().lock().unwrap().clone() {
        out.push(cached);
    }
    for &preset in GITHUB_PROXY_SITES {
        if !out.iter().any(|x| x == preset) {
            out.push(preset.to_string());
        }
    }
    out
}

// ---------------------------------------------------------------- 站点解析（Files / Playlist）

async fn build_site_client(engine: &Engine) -> Res<reqwest::Client> {
    let proxy = engine.effective_proxy().await.ok().flatten();
    dl_net::build_client(proxy.as_deref())
}

pub async fn inspect_site_url(engine: &Engine, raw_url: &str) -> Res<SiteInspectResult> {
    let trimmed = raw_url.trim();
    let hf_mirror_raw = engine.get_setting("hf_mirror_site").await.ok().flatten();
    let custom_hf = hf_mirror_raw
        .as_deref()
        .and_then(normalize_site_origin)
        .unwrap_or_else(|| DEFAULT_HF_MIRROR.to_string());

    if let Some(hf) = parse_hf_url(trimmed, Some(&custom_hf)) {
        return inspect_hf(engine, &hf, &custom_hf).await;
    }
    if let Some(ms) = parse_modelscope_url(trimmed) {
        return inspect_modelscope(engine, &ms).await;
    }
    if let Some(gh) = parse_github_release_url(trimmed) {
        return inspect_github_release(engine, &gh).await;
    }
    if let Some(bili) = parse_bili_url(trimmed) {
        return inspect_bilibili(engine, &bili, trimmed).await;
    }
    Ok(SiteInspectResult::FallbackYtdlp {
        url: trimmed.to_string(),
    })
}

async fn inspect_hf(engine: &Engine, hf: &HfRef, mirror_origin: &str) -> Res<SiteInspectResult> {
    let client = build_site_client(engine).await?;
    let mirror_on = engine
        .get_setting("hf_mirror_enabled")
        .await
        .ok()
        .flatten()
        .map(|v| v != "off")
        .unwrap_or(true);
    let token = engine
        .get_setting("hf_token")
        .await
        .ok()
        .flatten()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let api_path = format!(
        "/api/{}/{}/revision/{}?blobs=true",
        hf.api_prefix(),
        hf.repo_id(),
        encode_uri_path(&hf.revision)
    );
    let origins = if mirror_on {
        vec![
            mirror_origin.to_string(),
            "https://huggingface.co".to_string(),
        ]
    } else {
        vec![
            "https://huggingface.co".to_string(),
            mirror_origin.to_string(),
        ]
    };

    let mut last_err: Option<DownloadError> = None;
    let mut repo_json: Option<serde_json::Value> = None;
    for origin in &origins {
        let api_url = format!("{origin}{api_path}");
        let mut req = dl_net::identity_headers(client.get(&api_url));
        if let Some(ref tok) = token {
            req = req.header(reqwest::header::AUTHORIZATION, format!("Bearer {tok}"));
        }
        match req.send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                if status == 401 || status == 403 {
                    return Err(DownloadError::new(
                        ErrorKind::AuthExpired,
                        "该 Hugging Face 仓库需要登录或授权（Gated/Private）：请在设置中填写 Hugging Face Token，并先在网页同意模型协议",
                    ));
                }
                if status >= 400 {
                    last_err = Some(DownloadError::new(
                        ErrorKind::Http {
                            status,
                            retry_after_secs: None,
                        },
                        format!("Hugging Face API returned HTTP {status}"),
                    ));
                    continue;
                }
                match read_json_resp(resp, "Hugging Face API").await {
                    Ok(v) => {
                        repo_json = Some(v);
                        break;
                    }
                    Err(e) => {
                        last_err = Some(e);
                    }
                }
            }
            Err(e) => {
                last_err = Some(DownloadError::new(
                    dl_net::map_reqwest_err(&e),
                    e.to_string(),
                ))
            }
        }
    }

    let json = repo_json.ok_or_else(|| {
        last_err.unwrap_or_else(|| {
            DownloadError::new(
                ErrorKind::Internal,
                "failed to inspect Hugging Face repository",
            )
        })
    })?;
    parse_hf_repo_json(&json, hf, mirror_origin, mirror_on, token.as_deref())
}

pub fn parse_hf_repo_json(
    repo: &serde_json::Value,
    hf: &HfRef,
    mirror_origin: &str,
    mirror_on: bool,
    token: Option<&str>,
) -> Res<SiteInspectResult> {
    let commit = repo
        .get("sha")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(hf.revision.as_str());
    let siblings = repo
        .get("siblings")
        .and_then(|s| s.as_array())
        .ok_or_else(|| {
            DownloadError::new(
                ErrorKind::Internal,
                "Hugging Face response missing siblings",
            )
        })?;

    let prefix_filter = hf.file_path.trim_matches('/');
    let mut headers = HashMap::new();
    if let Some(tok) = token.map(str::trim).filter(|s| !s.is_empty()) {
        headers.insert("authorization".to_string(), format!("Bearer {tok}"));
    }

    let mut files = Vec::new();
    for s in siblings {
        let Some(rfilename) = s.get("rfilename").and_then(|v| v.as_str()) else {
            continue;
        };
        if !prefix_filter.is_empty()
            && rfilename != prefix_filter
            && !rfilename.starts_with(&format!("{prefix_filter}/"))
        {
            continue;
        }
        let encoded_path = encode_uri_path(rfilename);
        let canonical = format!(
            "https://huggingface.co/{}/resolve/{commit}/{encoded_path}",
            hf.url_prefix()
        );
        let mirrored = format!(
            "{mirror_origin}/{}/resolve/{commit}/{encoded_path}",
            hf.url_prefix()
        );
        let (url, mirrors) = if mirror_on {
            (mirrored, vec![canonical])
        } else {
            (canonical, vec![mirrored])
        };
        let size = s
            .get("lfs")
            .and_then(|l| l.get("size"))
            .and_then(|n| n.as_u64())
            .or_else(|| s.get("size").and_then(|n| n.as_u64()));
        let checksum = s
            .get("lfs")
            .and_then(|l| l.get("oid"))
            .and_then(|o| o.as_str())
            .filter(|o| o.len() == 64 && o.bytes().all(|b| b.is_ascii_hexdigit()))
            .map(|o| format!("sha256:{}", o.to_ascii_lowercase()));
        let filename = rfilename
            .rsplit('/')
            .next()
            .unwrap_or(rfilename)
            .to_string();
        files.push(SiteFileItem {
            relative_path: rfilename.to_string(),
            filename,
            url,
            size,
            checksum,
            mirrors,
            headers: headers.clone(),
        });
    }
    if files.is_empty() {
        return Err(DownloadError::new(
            ErrorKind::NotFound,
            format!("仓库 {} 中未找到匹配文件", hf.repo_id()),
        ));
    }
    let folder_name = (files.len() > 1).then(|| hf.repo_id().replace('/', "_"));
    Ok(SiteInspectResult::Files {
        site: "huggingface".into(),
        title: hf.repo_id(),
        subtitle: Some(format!("{} · {}", hf.repo_type, hf.revision)),
        folder_name,
        files,
    })
}

async fn inspect_modelscope(engine: &Engine, ms: &ModelScopeRef) -> Res<SiteInspectResult> {
    let client = build_site_client(engine).await?;
    let api_url = format!(
        "https://modelscope.cn/api/v1/{}/{}/{}/repo/files?Revision={}&Recursive=True",
        ms.repo_type,
        ms.org,
        ms.repo,
        encode_uri_path(&ms.revision)
    );
    let resp = dl_net::identity_headers(client.get(&api_url))
        .send()
        .await
        .map_err(|e| DownloadError::new(dl_net::map_reqwest_err(&e), e.to_string()))?;
    let status = resp.status().as_u16();
    if status >= 400 {
        return Err(DownloadError::new(
            ErrorKind::Http {
                status,
                retry_after_secs: None,
            },
            format!("ModelScope API returned HTTP {status}"),
        ));
    }
    let json = read_json_resp(resp, "ModelScope").await?;
    parse_modelscope_json(&json, ms)
}

pub fn parse_modelscope_json(
    json: &serde_json::Value,
    ms: &ModelScopeRef,
) -> Res<SiteInspectResult> {
    let list = json
        .get("Data")
        .and_then(|d| d.get("Files"))
        .and_then(|f| f.as_array())
        .or_else(|| json.get("Data").and_then(|d| d.as_array()))
        .ok_or_else(|| {
            DownloadError::new(ErrorKind::Internal, "ModelScope response missing Files")
        })?;

    let prefix_filter = ms.file_path.trim_matches('/');
    let mut files = Vec::new();
    for item in list {
        let typ = item.get("Type").and_then(|s| s.as_str()).unwrap_or("blob");
        if typ != "blob" {
            continue;
        }
        let Some(rel) = item
            .get("Path")
            .or_else(|| item.get("Name"))
            .and_then(|s| s.as_str())
        else {
            continue;
        };
        if !prefix_filter.is_empty()
            && rel != prefix_filter
            && !rel.starts_with(&format!("{prefix_filter}/"))
        {
            continue;
        }
        let encoded = encode_uri_path(rel);
        let url = format!(
            "https://modelscope.cn/{}/{}/{}/resolve/{}/{}",
            ms.repo_type, ms.org, ms.repo, ms.revision, encoded
        );
        let size = item.get("Size").and_then(|n| n.as_u64());
        let checksum = item
            .get("Sha256")
            .and_then(|s| s.as_str())
            .filter(|o| o.len() == 64 && o.bytes().all(|b| b.is_ascii_hexdigit()))
            .map(|o| format!("sha256:{}", o.to_ascii_lowercase()));
        let filename = rel.rsplit('/').next().unwrap_or(rel).to_string();
        files.push(SiteFileItem {
            relative_path: rel.to_string(),
            filename,
            url,
            size,
            checksum,
            mirrors: Vec::new(),
            headers: HashMap::new(),
        });
    }
    if files.is_empty() {
        return Err(DownloadError::new(
            ErrorKind::NotFound,
            format!("ModelScope 仓库 {}/{} 中未找到文件", ms.org, ms.repo),
        ));
    }
    let folder_name = (files.len() > 1).then(|| format!("{}_{}", ms.org, ms.repo));
    Ok(SiteInspectResult::Files {
        site: "modelscope".into(),
        title: format!("{}/{}", ms.org, ms.repo),
        subtitle: Some(format!("ModelScope · {}", ms.revision)),
        folder_name,
        files,
    })
}

async fn inspect_github_release(engine: &Engine, gh: &GithubReleaseRef) -> Res<SiteInspectResult> {
    let client = build_site_client(engine).await?;
    let api_url = match &gh.tag {
        Some(tag) => format!(
            "https://api.github.com/repos/{}/{}/releases/tags/{}",
            gh.owner,
            gh.repo,
            encode_uri_path(tag)
        ),
        None => format!(
            "https://api.github.com/repos/{}/{}/releases/latest",
            gh.owner, gh.repo
        ),
    };
    let resp = dl_net::identity_headers(client.get(&api_url))
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| DownloadError::new(dl_net::map_reqwest_err(&e), e.to_string()))?;
    let status = resp.status().as_u16();
    if status >= 400 {
        return Err(DownloadError::new(
            ErrorKind::Http {
                status,
                retry_after_secs: None,
            },
            format!("GitHub Releases API returned HTTP {status}"),
        ));
    }
    let json = read_json_resp(resp, "GitHub").await?;

    let gh_on = engine
        .get_setting("github_mirror_enabled")
        .await
        .ok()
        .flatten()
        .map(|v| v == "on")
        .unwrap_or(false);
    let selected = engine
        .get_setting("github_mirror_site")
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "auto".into());
    let custom = engine
        .get_setting("github_mirror_custom")
        .await
        .ok()
        .flatten();
    let proxies = if gh_on {
        build_github_proxy_order(&selected, custom.as_deref())
    } else {
        Vec::new()
    };

    parse_github_release_json(&json, gh, &proxies)
}

pub fn parse_github_release_json(
    json: &serde_json::Value,
    gh: &GithubReleaseRef,
    proxies: &[String],
) -> Res<SiteInspectResult> {
    let tag_name = json
        .get("tag_name")
        .and_then(|s| s.as_str())
        .unwrap_or("release")
        .to_string();
    let rel_name = json
        .get("name")
        .and_then(|s| s.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&tag_name)
        .to_string();

    let apply_proxies = |orig: String| -> (String, Vec<String>) {
        if let Some(first) = proxies.first() {
            let mut m: Vec<String> = proxies
                .iter()
                .skip(1)
                .take(3)
                .map(|p| format!("{p}/{orig}"))
                .collect();
            m.push(orig.clone());
            (format!("{first}/{orig}"), m)
        } else {
            (orig, Vec::new())
        }
    };

    let mut files = Vec::new();
    if let Some(assets) = json.get("assets").and_then(|a| a.as_array()) {
        for asset in assets {
            let Some(name) = asset.get("name").and_then(|s| s.as_str()) else {
                continue;
            };
            let Some(dl_url) = asset.get("browser_download_url").and_then(|s| s.as_str()) else {
                continue;
            };
            let size = asset.get("size").and_then(|n| n.as_u64());
            let checksum = asset
                .get("digest")
                .and_then(|d| d.as_str())
                .and_then(crate::checksum::parse_checksum)
                .map(|(a, h)| format!("{a}:{h}"));
            let (url, mirrors) = apply_proxies(dl_url.to_string());
            files.push(SiteFileItem {
                relative_path: name.to_string(),
                filename: name.to_string(),
                url,
                size,
                checksum,
                mirrors,
                headers: HashMap::new(),
            });
        }
    }
    if files.is_empty() {
        if let Some(zip_url) = json.get("zipball_url").and_then(|s| s.as_str()) {
            let name = format!("{}-{}.zip", gh.repo, tag_name);
            let (url, mirrors) = apply_proxies(zip_url.to_string());
            files.push(SiteFileItem {
                relative_path: name.clone(),
                filename: name,
                url,
                size: None,
                checksum: None,
                mirrors,
                headers: HashMap::new(),
            });
        }
    }
    if files.is_empty() {
        return Err(DownloadError::new(
            ErrorKind::NotFound,
            "该 GitHub Release 没有可下载资产",
        ));
    }

    Ok(SiteInspectResult::Files {
        site: "github".into(),
        title: format!("{}/{} · {}", gh.owner, gh.repo, rel_name),
        subtitle: Some(tag_name),
        folder_name: None,
        files,
    })
}

async fn inspect_bilibili(
    engine: &Engine,
    bili: &BiliRef,
    orig_url: &str,
) -> Res<SiteInspectResult> {
    let client = build_site_client(engine).await?;
    let resolved_url = match bili {
        BiliRef::Short(short_url) => {
            let resp = dl_net::identity_headers(client.get(short_url))
                .send()
                .await
                .map_err(|e| DownloadError::new(dl_net::map_reqwest_err(&e), e.to_string()))?;
            resp.url().to_string()
        }
        _ => orig_url.to_string(),
    };
    let parsed_ref = if matches!(bili, BiliRef::Short(_)) {
        match parse_bili_url(&resolved_url) {
            Some(r) if !matches!(r, BiliRef::Short(_)) => r,
            _ => {
                return Ok(SiteInspectResult::FallbackYtdlp { url: resolved_url });
            }
        }
    } else {
        bili.clone()
    };

    match parsed_ref {
        BiliRef::Video { bvid, aid } => {
            let query = match (bvid.as_deref(), aid) {
                (Some(b), _) => format!("bvid={b}"),
                (None, Some(a)) => format!("aid={a}"),
                _ => return Ok(SiteInspectResult::FallbackYtdlp { url: resolved_url }),
            };
            let api = format!("https://api.bilibili.com/x/web-interface/view?{query}");
            let v = fetch_bili_json(&client, &api).await?;
            parse_bili_view_json(&v, &resolved_url)
        }
        BiliRef::Season { mid, season_id } => {
            let api = format!(
                "https://api.bilibili.com/x/polymer/web-space/seasons_archives_list?mid={mid}&season_id={season_id}&page_num=1&page_size=100"
            );
            let v = fetch_bili_json(&client, &api).await?;
            parse_bili_season_json(&v)
        }
        BiliRef::Favorite { fid } => {
            let mut entries = Vec::new();
            let mut title = format!("Bilibili 收藏夹 ml{fid}");
            for pn in 1..=5 {
                let api = format!(
                    "https://api.bilibili.com/x/v3/fav/resource/list?media_id={fid}&pn={pn}&ps=20"
                );
                let v = fetch_bili_json(&client, &api).await?;
                let (t, mut batch, has_more) = parse_bili_fav_page(&v)?;
                if let Some(t) = t {
                    title = t;
                }
                entries.append(&mut batch);
                if !has_more || entries.len() >= 100 {
                    break;
                }
            }
            if entries.is_empty() {
                return Err(DownloadError::new(
                    ErrorKind::NotFound,
                    "该 B 站收藏夹没有可下载视频",
                ));
            }
            let total_count = entries.len();
            Ok(SiteInspectResult::Playlist {
                site: "bilibili".into(),
                title,
                total_count,
                entries,
            })
        }
        BiliRef::Short(_) => Ok(SiteInspectResult::FallbackYtdlp { url: resolved_url }),
    }
}

async fn read_json_resp(resp: reqwest::Response, label: &str) -> Res<serde_json::Value> {
    let text = resp
        .text()
        .await
        .map_err(|e| DownloadError::new(dl_net::map_reqwest_err(&e), e.to_string()))?;
    serde_json::from_str(&text)
        .map_err(|e| DownloadError::new(ErrorKind::Internal, format!("parse {label} json: {e}")))
}

async fn fetch_bili_json(client: &reqwest::Client, url: &str) -> Res<serde_json::Value> {
    let resp = dl_net::identity_headers(client.get(url))
        .header(reqwest::header::REFERER, "https://www.bilibili.com/")
        .send()
        .await
        .map_err(|e| DownloadError::new(dl_net::map_reqwest_err(&e), e.to_string()))?;
    let status = resp.status().as_u16();
    if status >= 400 {
        return Err(DownloadError::new(
            ErrorKind::Http {
                status,
                retry_after_secs: None,
            },
            format!("Bilibili API returned HTTP {status}"),
        ));
    }
    read_json_resp(resp, "Bilibili").await
}

pub fn parse_bili_view_json(
    json: &serde_json::Value,
    fallback_url: &str,
) -> Res<SiteInspectResult> {
    let code = json.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
    if code != 0 {
        return Ok(SiteInspectResult::FallbackYtdlp {
            url: fallback_url.to_string(),
        });
    }
    let Some(data) = json.get("data") else {
        return Ok(SiteInspectResult::FallbackYtdlp {
            url: fallback_url.to_string(),
        });
    };
    let main_title = data
        .get("title")
        .and_then(|s| s.as_str())
        .unwrap_or("Bilibili 视频")
        .trim()
        .to_string();

    if let Some(season) = data.get("ugc_season").filter(|v| v.is_object()) {
        let s_title = season
            .get("title")
            .and_then(|s| s.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&main_title)
            .to_string();
        let mut entries = Vec::new();
        if let Some(sections) = season.get("sections").and_then(|s| s.as_array()) {
            for sec in sections {
                if let Some(eps) = sec.get("episodes").and_then(|e| e.as_array()) {
                    for ep in eps {
                        let bvid = ep.get("bvid").and_then(|s| s.as_str()).unwrap_or("");
                        if bvid.is_empty() {
                            continue;
                        }
                        let ep_title = ep
                            .get("title")
                            .and_then(|s| s.as_str())
                            .unwrap_or(bvid)
                            .trim()
                            .to_string();
                        let dur = ep
                            .get("arc")
                            .and_then(|a| a.get("duration"))
                            .and_then(|d| d.as_f64())
                            .or_else(|| {
                                ep.get("page")
                                    .and_then(|p| p.get("duration"))
                                    .and_then(|d| d.as_f64())
                            });
                        entries.push(dl_ytdlp::PlaylistEntry {
                            id: bvid.to_string(),
                            title: ep_title,
                            url: format!("https://www.bilibili.com/video/{bvid}"),
                            duration_sec: dur,
                        });
                    }
                }
            }
        }
        if entries.len() > 1 {
            let total_count = entries.len();
            return Ok(SiteInspectResult::Playlist {
                site: "bilibili".into(),
                title: s_title,
                total_count,
                entries,
            });
        }
    }

    if let Some(pages) = data.get("pages").and_then(|p| p.as_array()) {
        if pages.len() > 1 {
            let bvid = data.get("bvid").and_then(|s| s.as_str()).unwrap_or("");
            if !bvid.is_empty() {
                let mut entries = Vec::with_capacity(pages.len());
                for pg in pages {
                    let pnum = pg.get("page").and_then(|n| n.as_u64()).unwrap_or(1);
                    let part = pg
                        .get("part")
                        .and_then(|s| s.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .unwrap_or("分P");
                    let dur = pg.get("duration").and_then(|d| d.as_f64());
                    entries.push(dl_ytdlp::PlaylistEntry {
                        id: format!("{bvid}_p{pnum}"),
                        title: format!("{main_title} - P{pnum} {part}"),
                        url: format!("https://www.bilibili.com/video/{bvid}?p={pnum}"),
                        duration_sec: dur,
                    });
                }
                let total_count = entries.len();
                return Ok(SiteInspectResult::Playlist {
                    site: "bilibili".into(),
                    title: main_title,
                    total_count,
                    entries,
                });
            }
        }
    }

    Ok(SiteInspectResult::FallbackYtdlp {
        url: fallback_url.to_string(),
    })
}

pub fn parse_bili_season_json(json: &serde_json::Value) -> Res<SiteInspectResult> {
    let data = json
        .get("data")
        .ok_or_else(|| DownloadError::new(ErrorKind::Internal, "Bilibili season missing data"))?;
    let title = data
        .get("meta")
        .and_then(|m| m.get("name"))
        .and_then(|s| s.as_str())
        .unwrap_or("Bilibili 合集")
        .to_string();
    let archives = data
        .get("archives")
        .and_then(|a| a.as_array())
        .ok_or_else(|| DownloadError::new(ErrorKind::NotFound, "Bilibili 合集为空"))?;
    let mut entries = Vec::new();
    for a in archives {
        let Some(bvid) = a.get("bvid").and_then(|s| s.as_str()) else {
            continue;
        };
        let t = a
            .get("title")
            .and_then(|s| s.as_str())
            .unwrap_or(bvid)
            .to_string();
        let dur = a.get("duration").and_then(|d| d.as_f64());
        entries.push(dl_ytdlp::PlaylistEntry {
            id: bvid.to_string(),
            title: t,
            url: format!("https://www.bilibili.com/video/{bvid}"),
            duration_sec: dur,
        });
    }
    let total_count = entries.len();
    Ok(SiteInspectResult::Playlist {
        site: "bilibili".into(),
        title,
        total_count,
        entries,
    })
}

pub fn parse_bili_fav_page(
    json: &serde_json::Value,
) -> Res<(Option<String>, Vec<dl_ytdlp::PlaylistEntry>, bool)> {
    let data = json
        .get("data")
        .ok_or_else(|| DownloadError::new(ErrorKind::Internal, "Bilibili fav missing data"))?;
    let title = data
        .get("info")
        .and_then(|i| i.get("title"))
        .and_then(|s| s.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let has_more = data
        .get("has_more")
        .and_then(|b| b.as_bool())
        .unwrap_or(false);
    let mut out = Vec::new();
    if let Some(medias) = data.get("medias").and_then(|m| m.as_array()) {
        for m in medias {
            if m.get("type").and_then(|t| t.as_i64()) != Some(2) {
                continue;
            }
            let Some(bvid) = m.get("bvid").and_then(|s| s.as_str()) else {
                continue;
            };
            let t = m
                .get("title")
                .and_then(|s| s.as_str())
                .unwrap_or(bvid)
                .to_string();
            let dur = m.get("duration").and_then(|d| d.as_f64());
            out.push(dl_ytdlp::PlaylistEntry {
                id: bvid.to_string(),
                title: t,
                url: format!("https://www.bilibili.com/video/{bvid}"),
                duration_sec: dur,
            });
        }
    }
    Ok((title, out, has_more))
}

// ---------------------------------------------------------------- 节点测速

pub async fn probe_site_mirrors(engine: &Engine) -> Vec<MirrorLatency> {
    let client = match build_site_client(engine).await {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let custom_gh = engine
        .get_setting("github_mirror_custom")
        .await
        .ok()
        .flatten()
        .and_then(|s| normalize_site_origin(&s));
    let custom_hf = engine
        .get_setting("hf_mirror_site")
        .await
        .ok()
        .flatten()
        .and_then(|s| normalize_site_origin(&s));

    let mut gh_sites: Vec<String> = GITHUB_PROXY_SITES.iter().map(|s| s.to_string()).collect();
    if let Some(c) = custom_gh {
        if !gh_sites.contains(&c) {
            gh_sites.push(c);
        }
    }

    let mut hf_sites: Vec<String> = vec![DEFAULT_HF_MIRROR.to_string()];
    if let Some(c) = custom_hf {
        if !hf_sites.contains(&c) {
            hf_sites.push(c);
        }
    }

    let mut set = JoinSet::new();
    for site in gh_sites {
        let cl = client.clone();
        set.spawn(async move {
            let url = format!("{site}/{GH_PROBE_TARGET}");
            let t0 = Instant::now();
            let ok = tokio::time::timeout(Duration::from_secs(5), async {
                let resp = dl_net::identity_headers(cl.get(&url))
                    .header(reqwest::header::RANGE, "bytes=0-0")
                    .send()
                    .await?;
                let st = resp.status().as_u16();
                let is_html = resp
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|m| dl_net::interstitial::is_html_mime(Some(m)));
                Ok::<bool, reqwest::Error>((st == 200 || st == 206) && !is_html)
            })
            .await
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or(false);
            MirrorLatency {
                site,
                group: "github".into(),
                latency_ms: ok.then(|| t0.elapsed().as_millis() as u64),
            }
        });
    }
    for site in hf_sites {
        let cl = client.clone();
        set.spawn(async move {
            let url = format!("{site}/{HF_PROBE_TARGET}");
            let t0 = Instant::now();
            let ok = tokio::time::timeout(Duration::from_secs(5), async {
                let resp = dl_net::identity_headers(cl.get(&url)).send().await?;
                Ok::<bool, reqwest::Error>(resp.status().as_u16() < 400)
            })
            .await
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or(false);
            MirrorLatency {
                site,
                group: "huggingface".into(),
                latency_ms: ok.then(|| t0.elapsed().as_millis() as u64),
            }
        });
    }

    let mut results = Vec::new();
    while let Some(res) = set.join_next().await {
        if let Ok(item) = res {
            results.push(item);
        }
    }
    results.sort_by_key(|m| (m.group.clone(), m.latency_ms.unwrap_or(u64::MAX)));

    if let Some(best_gh) = results
        .iter()
        .find(|m| m.group == "github" && m.latency_ms.is_some())
    {
        *best_gh_proxy_cache().lock().unwrap() = Some(best_gh.site.clone());
    }
    let _ = HashSet::<String>::new();
    results
}

// ---------------------------------------------------------------- 可下载链接智能判定

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateKind {
    /// 直接文件 / 直链 / 封装链接（可直接创建任务下载）
    Direct,
    /// 仓库 / Release / 视频作品或合集（适合打开识别弹窗选文件或画质）
    Inspect,
}

const DOWNLOADABLE_EXTS: &[&str] = &[
    // 视频与流媒体
    "mp4",
    "mkv",
    "webm",
    "mov",
    "avi",
    "flv",
    "m4v",
    "ts",
    "m3u8",
    "m3u",
    "mpd",
    "wmv",
    "rmvb",
    // 音频
    "mp3",
    "flac",
    "m4a",
    "aac",
    "ogg",
    "wav",
    "opus",
    "ape",
    "wma",
    // AI 模型与科学计算权重/包
    "gguf",
    "safetensors",
    "onnx",
    "ckpt",
    "pt",
    "pth",
    "bin",
    "model",
    "parquet",
    "h5",
    "pb",
    "tflite",
    "whl",
    // 压缩包
    "zip",
    "rar",
    "7z",
    "tar",
    "gz",
    "tgz",
    "bz2",
    "xz",
    "zst",
    "lz4",
    "cab",
    // 安装包与系统镜像
    "exe",
    "msi",
    "msix",
    "dmg",
    "pkg",
    "apk",
    "xapk",
    "ipa",
    "iso",
    "img",
    "deb",
    "rpm",
    "appimage",
    "flatpak",
    "snap",
    // 文档与电子书
    "pdf",
    "epub",
    "mobi",
    "azw3",
    "doc",
    "docx",
    "xls",
    "xlsx",
    "ppt",
    "pptx",
];

const WEBPAGE_EXTS: &[&str] = &[
    "html", "htm", "xhtml", "php", "asp", "aspx", "jsp", "css", "js", "mjs", "map", "svg", "ico",
    "woff", "woff2", "ttf", "eot",
];

fn path_ext_lower(u: &url::Url) -> Option<String> {
    let seg = u.path_segments()?.next_back()?;
    let decoded = percent_decode(seg);
    let trimmed = decoded.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (_, ext) = trimmed.rsplit_once('.')?;
    if ext.is_empty() || ext.len() > 16 || ext.contains('/') {
        return None;
    }
    Some(ext.to_ascii_lowercase())
}

fn is_downloadable_ext(ext: &str) -> bool {
    DOWNLOADABLE_EXTS.contains(&ext)
}

fn is_webpage_ext(ext: &str) -> bool {
    WEBPAGE_EXTS.contains(&ext)
}

fn is_local_host(host: &str) -> bool {
    let h = host.trim_matches(['[', ']']).to_ascii_lowercase();
    h == "localhost" || h == "127.0.0.1" || h == "0.0.0.0" || h == "::1" || h.ends_with(".local")
}

fn is_11_yt_id(s: &str) -> bool {
    s.len() == 11
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn is_video_site_work_url(u: &url::Url) -> bool {
    let Some(host) = u.host_str().map(|h| h.to_ascii_lowercase()) else {
        return false;
    };
    let bare = host.strip_prefix("www.").unwrap_or(&host);
    let segs: Vec<&str> = u.path().split('/').filter(|s| !s.is_empty()).collect();

    // YouTube
    if bare == "youtu.be" {
        return segs.first().is_some_and(|s| is_11_yt_id(s));
    }
    if bare == "youtube.com"
        || bare.ends_with(".youtube.com")
        || bare == "youtube-nocookie.com"
        || bare.ends_with(".youtube-nocookie.com")
    {
        if u.query_pairs()
            .any(|(k, v)| (k == "v" && is_11_yt_id(&v)) || (k == "list" && !v.trim().is_empty()))
        {
            return true;
        }
        return matches!(segs.as_slice(), ["shorts" | "live" | "embed" | "v", id, ..] if is_11_yt_id(id))
            || u.path().eq_ignore_ascii_case("/playlist");
    }

    // X / Twitter
    if bare == "x.com"
        || bare.ends_with(".x.com")
        || bare == "twitter.com"
        || bare.ends_with(".twitter.com")
    {
        if let Some(pos) = segs.iter().position(|&s| s == "status" || s == "statuses") {
            return segs
                .get(pos + 1)
                .is_some_and(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()));
        }
        return false;
    }

    // TikTok
    if bare == "vt.tiktok.com" || bare == "vm.tiktok.com" {
        return !segs.is_empty();
    }
    if bare == "tiktok.com" || bare.ends_with(".tiktok.com") {
        if let Some(pos) = segs.iter().position(|&s| s == "video" || s == "photo") {
            return segs
                .get(pos + 1)
                .is_some_and(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()));
        }
        return false;
    }

    // Douyin
    if bare == "v.douyin.com" {
        return !segs.is_empty();
    }
    if bare == "douyin.com"
        || bare.ends_with(".douyin.com")
        || bare == "iesdouyin.com"
        || bare.ends_with(".iesdouyin.com")
    {
        if let Some(pos) = segs.iter().position(|&s| s == "video" || s == "note") {
            return segs
                .get(pos + 1)
                .is_some_and(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()));
        }
        return false;
    }

    false
}

/// 判断 URL 是否为真实可下载的资源或支持的站点作品/仓库链接；普通网页一律返回 `None`。
pub fn classify_candidate_url(raw: &str, custom_hf_mirror: Option<&str>) -> Option<CandidateKind> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("thunder://")
        || lower.starts_with("flashget://")
        || lower.starts_with("qqdl://")
    {
        return dl_net::interstitial::unwrap_wrapped_link(trimmed).map(|_| CandidateKind::Direct);
    }

    let u = url::Url::parse(trimmed).ok()?;
    if !matches!(u.scheme(), "http" | "https") {
        return None;
    }
    let host = u.host_str()?.to_ascii_lowercase();
    if is_local_host(&host) {
        return None;
    }

    // 1. Hugging Face
    if let Some(hf) = parse_hf_url(trimmed, custom_hf_mirror) {
        if matches!(hf.action.as_deref(), Some("resolve" | "blob")) && !hf.file_path.is_empty() {
            return Some(CandidateKind::Direct);
        }
        return Some(CandidateKind::Inspect);
    }

    // 2. ModelScope
    if let Some(ms) = parse_modelscope_url(trimmed) {
        if !ms.file_path.is_empty() {
            return Some(CandidateKind::Direct);
        }
        return Some(CandidateKind::Inspect);
    }

    // 3. GitHub Releases 页 & 直链资产
    if parse_github_release_url(trimmed).is_some() {
        return Some(CandidateKind::Inspect);
    }
    if is_github_file_url(trimmed) {
        if host.ends_with("github.com") && u.path().contains("/blob/") {
            let ext = path_ext_lower(&u)?;
            return is_downloadable_ext(&ext).then_some(CandidateKind::Direct);
        }
        return Some(CandidateKind::Direct);
    }

    // 4. Bilibili & 支持的视频作品页
    if parse_bili_url(trimmed).is_some() || is_video_site_work_url(&u) {
        return Some(CandidateKind::Inspect);
    }

    // 5. GitLab / Gitee blob 仅在目标路径是可下载文件后缀时提示
    let bare = host.strip_prefix("www.").unwrap_or(&host);
    if (bare == "gitlab.com" && u.path().contains("/-/blob/"))
        || (bare == "gitee.com" && u.path().contains("/blob/"))
    {
        let ext = path_ext_lower(&u)?;
        return is_downloadable_ext(&ext).then_some(CandidateKind::Direct);
    }

    // 6. 网盘 / arXiv 明确下载页直链
    if matches!(bare, "dropbox.com" | "drive.google.com" | "arxiv.org")
        && dl_net::interstitial::normalize_download_url(trimmed).is_some()
    {
        return Some(CandidateKind::Direct);
    }

    // 7. 常规直链文件名或明确下载语义参数检查
    if let Some(ext) = path_ext_lower(&u) {
        if is_webpage_ext(&ext) {
            return None;
        }
        if is_downloadable_ext(&ext) {
            return Some(CandidateKind::Direct);
        }
    }

    let has_download_hint = u.query_pairs().any(|(k, v)| {
        let lk = k.to_ascii_lowercase();
        let lv = v.to_ascii_lowercase();
        (matches!(
            lk.as_str(),
            "filename" | "file" | "attname" | "download_name"
        ) && !v.trim().is_empty())
            || (lk == "response-content-disposition" && lv.contains("attachment"))
            || (lk == "download" && matches!(lv.as_str(), "1" | "true") && u.path().len() > 1)
    });
    if has_download_hint {
        return Some(CandidateKind::Direct);
    }

    if (bare == "sourceforge.net"
        && u.path().contains("/files/")
        && u.path().ends_with("/download"))
        || (bare == "mediafire.com" && u.path().starts_with("/file/"))
    {
        return Some(CandidateKind::Direct);
    }

    None
}

/// 从任意剪贴板文本（含中文分享文案）提取真实可下载链接并按出现顺序去重。
pub fn extract_downloadable_urls(
    text: &str,
    custom_hf_mirror: Option<&str>,
) -> Vec<(String, CandidateKind)> {
    const PREFIXES: &[&str] = &[
        "https://",
        "http://",
        "thunder://",
        "flashget://",
        "qqdl://",
    ];
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let bytes = text.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        let rest = &text[i..];
        let matched = PREFIXES.iter().find(|&&p| {
            rest.get(..p.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(p))
        });
        if let Some(prefix) = matched {
            let mut end = i + prefix.len();
            while end < bytes.len() {
                let ch = text[end..].chars().next().unwrap();
                if ch.is_whitespace()
                    || !ch.is_ascii()
                    || matches!(ch, '"' | '\'' | '<' | '>' | '[' | ']' | '{' | '}')
                {
                    break;
                }
                end += ch.len_utf8();
            }
            let raw_cand = text[i..end].trim_end_matches(|c: char| {
                matches!(
                    c,
                    ',' | '.' | ')' | ';' | ':' | '!' | '?' | '。' | '，' | '！' | '？'
                )
            });
            if seen.insert(raw_cand.to_string()) {
                if let Some(kind) = classify_candidate_url(raw_cand, custom_hf_mirror) {
                    out.push((raw_cand.to_string(), kind));
                }
            }
            i = end;
        } else {
            let ch = rest.chars().next().unwrap();
            i += ch.len_utf8();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hf_model_dataset_and_file_urls() {
        let r1 = parse_hf_url("https://huggingface.co/Qwen/Qwen2.5-7B-GGUF", None).unwrap();
        assert_eq!(r1.repo_type, "model");
        assert_eq!(r1.repo_id(), "Qwen/Qwen2.5-7B-GGUF");
        assert_eq!(r1.revision, "main");
        assert_eq!(r1.action, None);

        let r2 = parse_hf_url(
            "https://hf-mirror.com/datasets/open-llm/bench/resolve/v1.0/sub%20dir/data.parquet",
            None,
        )
        .unwrap();
        assert_eq!(r2.repo_type, "dataset");
        assert_eq!(r2.revision, "v1.0");
        assert_eq!(r2.action.as_deref(), Some("resolve"));
        assert_eq!(r2.file_path, "sub dir/data.parquet");

        assert!(parse_hf_url("https://huggingface.co/models", None).is_none());
        assert!(parse_hf_url("https://huggingface.co/settings/tokens", None).is_none());
    }

    #[test]
    fn parses_github_files_and_releases() {
        assert!(is_github_file_url(
            "https://github.com/cli/cli/releases/download/v2.62.0/gh.zip"
        ));
        assert!(is_github_file_url(
            "https://raw.githubusercontent.com/user/repo/main/a.txt"
        ));
        assert!(!is_github_file_url("https://github.com/cli/cli/issues/1"));

        let rel =
            parse_github_release_url("https://github.com/cli/cli/releases/tag/v2.62.0").unwrap();
        assert_eq!(rel.owner, "cli");
        assert_eq!(rel.repo, "cli");
        assert_eq!(rel.tag.as_deref(), Some("v2.62.0"));
        assert!(
            parse_github_release_url("https://github.com/cli/cli/releases/download/v1/a.exe")
                .is_none()
        );
    }

    #[test]
    fn parses_modelscope_and_bilibili_urls() {
        let ms = parse_modelscope_url(
            "https://modelscope.cn/models/Qwen/Qwen2.5-7B/file/view/master/qwen.gguf",
        )
        .unwrap();
        assert_eq!(ms.repo_type, "models");
        assert_eq!(ms.org, "Qwen");
        assert_eq!(ms.file_path, "qwen.gguf");

        assert_eq!(
            parse_bili_url("https://b23.tv/abc123"),
            Some(BiliRef::Short("https://b23.tv/abc123".into()))
        );
        assert_eq!(
            parse_bili_url("https://www.bilibili.com/video/BV1xx411c7mD?p=2"),
            Some(BiliRef::Video {
                bvid: Some("BV1xx411c7mD".into()),
                aid: None,
            })
        );
        assert_eq!(
            parse_bili_url("https://space.bilibili.com/12345/lists/6789?type=season"),
            Some(BiliRef::Season {
                mid: 12345,
                season_id: 6789,
            })
        );
        assert_eq!(
            parse_bili_url("https://space.bilibili.com/12345/favlist?fid=999"),
            Some(BiliRef::Favorite { fid: 999 })
        );
        assert_eq!(
            parse_bili_url("https://space.bilibili.com/12345/favlist?fid=888&ctype=21"),
            Some(BiliRef::Season {
                mid: 12345,
                season_id: 888,
            })
        );
    }

    #[test]
    fn parses_hf_modelscope_github_and_bili_json() {
        let hf = parse_hf_url("https://huggingface.co/org/model", None).unwrap();
        let hf_json = serde_json::json!({
            "sha": "abc123commit",
            "siblings": [
                {"rfilename": "weights/model.gguf", "size": 100, "lfs": {"size": 2048, "oid": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}
            ]
        });
        let res =
            parse_hf_repo_json(&hf_json, &hf, DEFAULT_HF_MIRROR, true, Some("hf_tok")).unwrap();
        match res {
            SiteInspectResult::Files { files, .. } => {
                assert_eq!(files.len(), 1);
                assert_eq!(
                    files[0].url,
                    "https://hf-mirror.com/org/model/resolve/abc123commit/weights/model.gguf"
                );
                assert_eq!(
                    files[0].mirrors,
                    vec![
                        "https://huggingface.co/org/model/resolve/abc123commit/weights/model.gguf"
                    ]
                );
                assert_eq!(files[0].size, Some(2048));
                assert_eq!(
                    files[0].checksum.as_deref(),
                    Some("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                );
                assert_eq!(
                    files[0].headers.get("authorization").map(String::as_str),
                    Some("Bearer hf_tok")
                );
            }
            other => panic!("unexpected {other:?}"),
        }

        let ms = parse_modelscope_url("https://modelscope.cn/models/org/m").unwrap();
        let ms_json = serde_json::json!({
            "Data": {
                "Files": [
                    {"Type": "tree", "Path": "sub"},
                    {"Type": "blob", "Path": "sub/m.safetensors", "Size": 4096, "Sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}
                ]
            }
        });
        let ms_res = parse_modelscope_json(&ms_json, &ms).unwrap();
        match ms_res {
            SiteInspectResult::Files { files, .. } => {
                assert_eq!(files.len(), 1);
                assert_eq!(files[0].filename, "m.safetensors");
                assert_eq!(
                    files[0].checksum.as_deref(),
                    Some("sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
                );
            }
            other => panic!("unexpected {other:?}"),
        }

        let bili_json = serde_json::json!({
            "code": 0,
            "data": {
                "bvid": "BV1xx411c7mD",
                "title": "Course",
                "pages": [
                    {"page": 1, "part": "Intro", "duration": 60},
                    {"page": 2, "part": "Deep", "duration": 120}
                ]
            }
        });
        match parse_bili_view_json(&bili_json, "https://www.bilibili.com/video/BV1xx411c7mD")
            .unwrap()
        {
            SiteInspectResult::Playlist { entries, .. } => {
                assert_eq!(entries.len(), 2);
                assert_eq!(
                    entries[1].url,
                    "https://www.bilibili.com/video/BV1xx411c7mD?p=2"
                );
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn adapt_create_task_skips_github_proxy_when_sensitive_headers_present() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(&dir.path().join("data.db")).await.unwrap();
        engine
            .set_setting("github_mirror_enabled", "on")
            .await
            .unwrap();

        let mut hdrs = HashMap::new();
        hdrs.insert("Authorization".into(), "Bearer secret".into());
        let t = engine
            .create_task(CreateTask {
                headers: Some(hdrs),
                ..CreateTask::of("https://github.com/cli/cli/releases/download/v1/app.zip")
            })
            .await
            .unwrap();
        assert_eq!(
            t.url,
            "https://github.com/cli/cli/releases/download/v1/app.zip"
        );
        assert!(t.mirrors_json.is_none());

        let t2 = engine
            .create_task(CreateTask::of(
                "https://github.com/cli/cli/releases/download/v1/public.zip",
            ))
            .await
            .unwrap();
        assert!(t2
            .url
            .starts_with("https://gh-proxy.org/https://github.com/"));
        assert!(t2
            .mirrors_json
            .as_deref()
            .unwrap_or("")
            .contains("https://github.com/cli/cli/releases/download/v1/public.zip"));
    }

    #[test]
    fn classifies_downloadable_urls_and_ignores_regular_webpages() {
        // 普通网页/文档/代码页面/本机地址应被忽略
        for ignored in [
            "https://www.google.com/search?q=rust",
            "https://github.com/XiaoYouChR/Ghost-Downloader-3",
            "https://github.com/cli/cli/issues/123",
            "https://github.com/cli/cli/blob/trunk/README.md",
            "https://www.bilibili.com/",
            "https://www.youtube.com/feed/subscriptions",
            "https://example.com/blog/how-to-download.html",
            "http://localhost:3000/test.zip",
            "https://127.0.0.1:8080/app.exe",
        ] {
            assert_eq!(
                classify_candidate_url(ignored, None),
                None,
                "should ignore {ignored}"
            );
        }

        // 直链文件 / 模型 / 安装包 / 压缩包 / 流媒体应返回 Direct
        for direct in [
            "https://cdn.example.com/Qwen2.5-7B-Q4_K_M.gguf",
            "https://cdn.example.com/model.safetensors?download=true",
            "https://cdn.example.com/installer_v2.exe",
            "https://cdn.example.com/archive.zip",
            "https://cdn.example.com/live/index.m3u8",
            "https://cdn.example.com/dash/manifest.mpd",
            "https://github.com/cli/cli/releases/download/v2.62.0/gh.zip",
            "https://huggingface.co/Qwen/Qwen2.5-7B/resolve/main/config.json",
            "https://modelscope.cn/models/Qwen/Qwen2.5-7B/file/view/master/qwen.gguf",
            "https://arxiv.org/abs/2401.12345",
            "thunder://QUFodHRwczovL2V4YW1wbGUuY29tL2EuemlwWlo=",
        ] {
            assert_eq!(
                classify_candidate_url(direct, None),
                Some(CandidateKind::Direct),
                "should be Direct: {direct}"
            );
        }

        // 仓库 / Releases / 视频作品应返回 Inspect
        for inspect in [
            "https://huggingface.co/Qwen/Qwen2.5-7B-Instruct-GGUF",
            "https://modelscope.cn/models/Qwen/Qwen2.5-7B",
            "https://github.com/cli/cli/releases/latest",
            "https://www.bilibili.com/video/BV1xx411c7mD",
            "https://b23.tv/abc123",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://x.com/user/status/1700000000000000001",
            "https://v.douyin.com/iRNBxyz/",
        ] {
            assert_eq!(
                classify_candidate_url(inspect, None),
                Some(CandidateKind::Inspect),
                "should be Inspect: {inspect}"
            );
        }

        // 混合中文分享文案提取：过滤无效网址并保留真实链接
        let mixed = "先看主页 https://github.com/foo/bar ，下载模型看这个 https://huggingface.co/Qwen/Qwen2.5-7B-GGUF ，还有视频 https://v.douyin.com/abc123/ 复制此链接打开。";
        let got = extract_downloadable_urls(mixed, None);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0, "https://huggingface.co/Qwen/Qwen2.5-7B-GGUF");
        assert_eq!(got[0].1, CandidateKind::Inspect);
        assert_eq!(got[1].0, "https://v.douyin.com/abc123/");
        assert_eq!(got[1].1, CandidateKind::Inspect);
    }
}
