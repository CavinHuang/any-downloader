//! 捕获规则与条目缓冲。
//!
//! 两类捕获源（对齐浏览器 channels 适配器的配对语义，docs/MEDIA-ADAPTERS.md §3.1）：
//! - 解密域上的媒体请求：URL 命中 `.mp4`（finder CDN 形态）→ 记条目（url/headers/大小）；
//! - 解密域上的 API 响应：JSON 含 `decode_key` → 记待配对 key（120s 有效），
//!   与随后的最近一条媒体配对成 `postprocess: isaac64-xor` 种子；同一响应若
//!   同时带媒体 URL 则直接成条。
//! 条目环形缓冲（默认 50）+ broadcast 事件供宿主（Tauri）转发 UI。

use std::collections::VecDeque;
use std::sync::Mutex;

use serde::Serialize;
use tokio::sync::broadcast;

/// 与引擎 HEADER_ALLOWLIST 对齐的头子集（小写键）。
pub const KEEP_HEADERS: &[&str] = &[
    "referer",
    "user-agent",
    "cookie",
    "authorization",
    "accept",
    "accept-language",
];

const KEY_TTL_MS: i64 = 120_000;
const KEY_KEEP: usize = 16;

#[derive(Clone, Debug, Serialize)]
pub struct SniffedItem {
    pub id: String,
    /// 媒体直链（已解密域 MITM 视角）
    pub url: String,
    /// 来源应用标记（当前仅微信视频号）
    pub app: &'static str,
    /// 随任务下发的请求头（小写键）
    pub headers: std::collections::HashMap<String, String>,
    /// Content-Length（可能缺失）
    pub bytes: Option<u64>,
    /// 视频号 isaac64-xor 种子（decode_key；无加密的视频为 None）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<String>,
    /// 标题线索（API 里的 title/desc，尽力而为）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 捕获时间（ms epoch）
    pub at: i64,
}

impl SniffedItem {
    /// 引擎 CreateTask 的 postprocess 形态（dl-model PostProcess）
    pub fn postprocess(&self) -> Option<Vec<(String, String)>> {
        self.seed
            .as_ref()
            .map(|s| vec![("isaac64-xor".to_string(), s.clone())])
    }
}

#[derive(Default)]
struct Keys {
    /// (decode_key, at) 环形
    pending: VecDeque<(String, i64)>,
}

pub struct Capture {
    items: Mutex<VecDeque<SniffedItem>>,
    keys: Mutex<Keys>,
    tx: broadcast::Sender<SniffedItem>,
    cap: usize,
}

impl Capture {
    pub fn new(cap: usize) -> Self {
        let (tx, _) = broadcast::channel(64);
        Self { items: Mutex::new(VecDeque::new()), keys: Mutex::new(Keys::default()), tx, cap: cap.max(4) }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SniffedItem> {
        self.tx.subscribe()
    }

    pub fn items(&self) -> Vec<SniffedItem> {
        self.items.lock().unwrap().iter().cloned().collect()
    }

    pub fn clear(&self) {
        self.items.lock().unwrap().clear();
    }

    pub fn latest(&self) -> Option<SniffedItem> {
        self.items.lock().unwrap().back().cloned()
    }

    fn push(&self, mut item: SniffedItem) {
        // 同 URL 重复捕获（Range 重读/多连接）：保最新（种子可能后来才配上）
        let mut q = self.items.lock().unwrap();
        if let Some(old) = q.iter_mut().find(|i| i.url == item.url) {
            if item.seed.is_none() {
                item.seed = old.seed.take();
            }
            *old = item.clone();
        } else {
            q.push_back(item.clone());
            while q.len() > self.cap {
                q.pop_front();
            }
        }
        drop(q);
        let _ = self.tx.send(item);
    }

    /// API 响应里发现 decode_key（媒体 URL 未知，等后续媒体请求来配对）
    pub fn note_key(&self, key: String, at: i64) {
        let mut k = self.keys.lock().unwrap();
        if !k.pending.iter().any(|(x, _)| x == &key) {
            k.pending.push_back((key, at));
            while k.pending.len() > KEY_KEEP {
                k.pending.pop_front();
            }
        }
    }

    /// 媒体直链捕获；URL 不命中媒体形态返回 false。
    pub fn media(
        &self,
        url: &str,
        headers: std::collections::HashMap<String, String>,
        bytes: Option<u64>,
        at: i64,
    ) -> bool {
        if !is_media_url(url) {
            return false;
        }
        let seed = self.take_key(at);
        tracing::info!(url, ?seed, "sniff 捕获媒体");
        self.push(SniffedItem {
            id: uuid::Uuid::new_v4().to_string(),
            url: url.to_string(),
            app: "wechat",
            headers,
            bytes,
            seed,
            title: None,
            at,
        });
        true
    }

    fn take_key(&self, at: i64) -> Option<String> {
        let mut k = self.keys.lock().unwrap();
        while let Some((key, t)) = k.pending.front().map(|(key, t)| (key.clone(), *t)) {
            if at - t > KEY_TTL_MS {
                k.pending.pop_front();
                continue;
            }
            k.pending.pop_front();
            return Some(key);
        }
        None
    }
}

/// 媒体直链形态：finder CDN 的 .mp4（扩展名判断与浏览器嗅探 EXT_TYPE 一致；
/// query 参数不影响）。
pub fn is_media_url(url: &str) -> bool {
    let path = url.split('#').next().unwrap_or("").split('?').next().unwrap_or("");
    path.ends_with(".mp4") || path.ends_with(".m3u8")
}

pub struct ApiScan {
    pub key: Option<String>,
    pub media_url: Option<String>,
    pub title: Option<String>,
}

/// 在 finder API JSON 里深挖 decode_key / 媒体 URL / 标题。
/// 递归遍历对象与数组；字符串值若像媒体 URL 也认（fields 名不定：url/media_url/file_url/...）。
pub fn scan_api_json(body: &str) -> ApiScan {
    let mut out = ApiScan { key: None, media_url: None, title: None };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return out;
    };
    walk(&v, &mut out);
    out
}

fn is_key_like(s: &str) -> bool {
    !s.is_empty() && s.len() <= 20 && s.chars().all(|c| c.is_ascii_digit())
}

fn walk(v: &serde_json::Value, out: &mut ApiScan) {
    match v {
        serde_json::Value::Object(m) => {
            for (k, val) in m {
                match (k.as_str(), val) {
                    ("decode_key", serde_json::Value::String(s)) if is_key_like(s) => {
                        if out.key.is_none() {
                            out.key = Some(s.clone());
                        }
                    }
                    ("title" | "desc" | "object_desc", serde_json::Value::String(s))
                        if out.title.is_none() && !s.trim().is_empty() && s.len() <= 120 =>
                    {
                        out.title = Some(s.chars().take(80).collect());
                    }
                    (_, serde_json::Value::String(s))
                        if out.media_url.is_none()
                            && (s.starts_with("http://") || s.starts_with("https://"))
                            && is_media_url(s) =>
                    {
                        out.media_url = Some(s.clone());
                    }
                    (_, other) => walk(other, out),
                }
            }
        }
        serde_json::Value::Array(a) => {
            for x in a {
                walk(x, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_url_shape() {
        assert!(is_media_url(
            "https://finder.video.qq.com/302/20304/10004/1190000041/f02030xxx.mp4?zz=1"
        ));
        assert!(!is_media_url("https://finder.video.qq.com/api/feed"));
        assert!(is_media_url("https://x/a.m3u8?k=1"));
    }

    #[test]
    fn api_scan_finds_triple() {
        let j = r#"{"data":{"object_desc":"我的猫","media":{"url":"https://finder.video.qq.com/302/a/b.mp4?tk=1"},"decode_key":"2136343393"}}"#;
        let s = scan_api_json(j);
        assert_eq!(s.key.as_deref(), Some("2136343393"));
        assert_eq!(s.media_url.as_deref(), Some("https://finder.video.qq.com/302/a/b.mp4?tk=1"));
        assert_eq!(s.title.as_deref(), Some("我的猫"));
    }

    #[test]
    fn capture_pairs_key_with_later_media() {
        let c = Capture::new(10);
        c.note_key("123456".into(), 1_000);
        assert!(c.media("https://finder.video.qq.com/a.mp4", Default::default(), Some(9), 2_000));
        let it = c.latest().unwrap();
        assert_eq!(it.seed.as_deref(), Some("123456"));
        // 无 key 时照常成条（未加密视频）
        assert!(c.media("https://finder.video.qq.com/b.mp4", Default::default(), None, 3_000));
        assert!(c.latest().unwrap().seed.is_none());
        // 同 URL 更新保最新种子
        c.note_key("999".into(), 3_500);
        assert!(c.media("https://finder.video.qq.com/b.mp4", Default::default(), None, 4_000));
        assert_eq!(c.latest().unwrap().seed.as_deref(), Some("999"));
        assert_eq!(c.items().len(), 2, "同 URL 去重");
    }

    #[test]
    fn key_ttl() {
        let c = Capture::new(10);
        c.note_key("111".into(), 0);
        assert!(c.media("https://finder.video.qq.com/c.mp4", Default::default(), None, KEY_TTL_MS + 1));
        assert!(c.latest().unwrap().seed.is_none(), "过期 key 不配对");
    }
}
