//! MPEG-DASH (.mpd) 点播下载与音视频混流管线。
//!
//! - 支持标准 `<SegmentTemplate>`（含 `<SegmentTimeline>` 与基于 duration/timescale 的模板）、
//!   `<SegmentList>` 与 `<BaseURL>`/`<SegmentBase>` 单文件分轨模式；
//! - 拒绝 `type="dynamic"` 直播清单（返回 `ErrorKind::LiveUnsupported`）与 DRM
//!   `<ContentProtection>` 加密清单（返回 `ErrorKind::DrmUnsupported`）；
//! - 并发分片拉取 + 原子 `.part` → `<key>` 断点续传（`dl-tmp/<task_id>/`）；
//! - 视频轨 + 音频轨分离时通过 ffmpeg 无损 stream-copy 混流为 `.mp4`。

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dl_error::{DownloadError, ErrorKind, Res};
use quick_xml::events::attributes::Attributes;
use quick_xml::events::Event;
use quick_xml::Reader;

use crate::driver::{
    drive_from, finalize, move_file_or_copy, sleep_or_cancel, Ctx, DriveErr, TargetPaths,
};
use crate::hls::{fetch_segment_bytes, fetch_text, find_ffmpeg_with, io_err_kind};

const WORKERS: usize = 16;
const SEG_ATTEMPTS: u32 = 6;
const MAX_GENERATED_SEGS: usize = 50_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashSeg {
    pub key_name: String,
    pub uri: String,
    pub range: Option<(u64, u64)>, // (offset, len)
}

#[derive(Debug, Clone)]
pub struct DashRep {
    pub id: String,
    pub bandwidth: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub init: Option<DashSeg>,
    pub segments: Vec<DashSeg>,
    pub has_drm: bool,
}

#[derive(Debug, Clone)]
pub struct ParsedMpd {
    pub duration_sec: Option<f64>,
    pub video_reps: Vec<DashRep>,
    pub audio_reps: Vec<DashRep>,
}

#[derive(Debug, Clone, Default)]
struct SegTemplateSpec {
    media: Option<String>,
    initialization: Option<String>,
    timescale: u64,
    duration: Option<u64>,
    start_number: u64,
    timeline: Vec<(Option<u64>, u64, i64)>, // (t, d, r)
}

impl SegTemplateSpec {
    fn merged_over(&self, base: &SegTemplateSpec) -> SegTemplateSpec {
        SegTemplateSpec {
            media: self.media.clone().or_else(|| base.media.clone()),
            initialization: self
                .initialization
                .clone()
                .or_else(|| base.initialization.clone()),
            timescale: if self.timescale > 0 {
                self.timescale
            } else if base.timescale > 0 {
                base.timescale
            } else {
                1
            },
            duration: self.duration.or(base.duration),
            start_number: if self.start_number > 0 {
                self.start_number
            } else if base.start_number > 0 {
                base.start_number
            } else {
                1
            },
            timeline: if !self.timeline.is_empty() {
                self.timeline.clone()
            } else {
                base.timeline.clone()
            },
        }
    }
}

#[derive(Debug, Clone, Default)]
struct SegListSpec {
    init_url: Option<String>,
    init_range: Option<(u64, u64)>,
    items: Vec<(String, Option<(u64, u64)>)>,
}

impl SegListSpec {
    fn merged_over(&self, base: &SegListSpec) -> SegListSpec {
        SegListSpec {
            init_url: self.init_url.clone().or_else(|| base.init_url.clone()),
            init_range: self.init_range.or(base.init_range),
            items: if !self.items.is_empty() {
                self.items.clone()
            } else {
                base.items.clone()
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MediaRole {
    Video,
    Audio,
    Other,
}

#[derive(Debug, Clone, Default)]
struct AdaptBuilder {
    mime_type: Option<String>,
    content_type: Option<String>,
    base_url: Option<String>,
    seg_template: Option<SegTemplateSpec>,
    seg_list: Option<SegListSpec>,
    has_drm: bool,
    reps: Vec<RepBuilder>,
}

#[derive(Debug, Clone, Default)]
struct RepBuilder {
    id: String,
    mime_type: Option<String>,
    bandwidth: u64,
    width: Option<u32>,
    height: Option<u32>,
    base_url: Option<String>,
    seg_template: Option<SegTemplateSpec>,
    seg_list: Option<SegListSpec>,
    has_drm: bool,
}

pub fn strip_url_fragment(url: &str) -> (String, Option<String>) {
    if let Some((base, frag)) = url.split_once('#') {
        let rep = frag
            .split('&')
            .find_map(|kv| kv.strip_prefix("adl_rep="))
            .map(percent_decode_simple)
            .filter(|s| !s.is_empty());
        (base.to_string(), rep)
    } else {
        (url.to_string(), None)
    }
}

fn percent_decode_simple(s: &str) -> String {
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

/// 解析 ISO-8601 时长（如 `PT1H2M3.5S`、`PT45S`）为秒。
pub fn parse_iso8601_duration(raw: &str) -> Option<f64> {
    let s = raw.trim();
    let rest = s.strip_prefix("PT").or_else(|| s.strip_prefix("pt"))?;
    if rest.is_empty() {
        return None;
    }
    let mut num = String::new();
    let mut total = 0.0f64;
    let mut matched = false;
    for ch in rest.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            num.push(ch);
        } else {
            let val: f64 = num.parse().ok()?;
            num.clear();
            match ch.to_ascii_uppercase() {
                'H' => total += val * 3600.0,
                'M' => total += val * 60.0,
                'S' => total += val,
                _ => return None,
            }
            matched = true;
        }
    }
    if !num.is_empty() || !matched {
        return None;
    }
    (total > 0.0).then_some(total)
}

/// 解析 byte range 字符串 `"start-end"` 为 `(offset, len)`。
fn parse_byte_range(spec: &str) -> Option<(u64, u64)> {
    let (a, b) = spec.trim().split_once('-')?;
    let start: u64 = a.trim().parse().ok()?;
    let end: u64 = b.trim().parse().ok()?;
    (end >= start).then_some((start, end - start + 1))
}

fn resolve_url(base: &str, rel: &str) -> String {
    let r = rel.trim();
    if r.starts_with("http://") || r.starts_with("https://") {
        return r.to_string();
    }
    url::Url::parse(base)
        .and_then(|u| u.join(r))
        .map(|u| u.to_string())
        .unwrap_or_else(|_| r.to_string())
}

/// 展开 DASH SegmentTemplate 占位符：
/// `$RepresentationID$` / `$Bandwidth$` / `$Number$` / `$Number%0Nd$` / `$Time$` / `$$`。
pub fn expand_template(tpl: &str, rep_id: &str, bandwidth: u64, number: u64, time: u64) -> String {
    let mut out = String::with_capacity(tpl.len() + 16);
    let mut rest = tpl;
    while let Some(start) = rest.find('$') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('$') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let token = &after[..end];
        if token.is_empty() {
            out.push('$');
        } else if token == "RepresentationID" {
            out.push_str(rep_id);
        } else if token == "Bandwidth" {
            out.push_str(&bandwidth.to_string());
        } else if token == "Time" {
            out.push_str(&time.to_string());
        } else if token == "Number" {
            out.push_str(&number.to_string());
        } else if let Some(fmt) = token.strip_prefix("Number%") {
            let width = fmt
                .strip_suffix('d')
                .and_then(|w| w.parse::<usize>().ok())
                .unwrap_or(0)
                .min(16);
            if width > 0 {
                out.push_str(&format!("{number:0width$}"));
            } else {
                out.push_str(&number.to_string());
            }
        } else if let Some(fmt) = token.strip_prefix("Time%") {
            let width = fmt
                .strip_suffix('d')
                .and_then(|w| w.parse::<usize>().ok())
                .unwrap_or(0)
                .min(16);
            if width > 0 {
                out.push_str(&format!("{time:0width$}"));
            } else {
                out.push_str(&time.to_string());
            }
        } else {
            out.push('$');
            out.push_str(token);
            out.push('$');
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

fn attr_str(attrs: Attributes<'_>, want: &str) -> Option<String> {
    for a in attrs.flatten() {
        let key: &str = a.key.as_ref();
        if key.eq_ignore_ascii_case(want) {
            return Some(a.value.as_ref().to_string());
        }
    }
    None
}

fn is_drm_attrs(attrs: Attributes<'_>) -> bool {
    let mut scheme = String::new();
    let mut value = String::new();
    let mut has_kid = false;
    for a in attrs.flatten() {
        let key = a.key.as_ref().to_ascii_lowercase();
        let val = a.value.as_ref().to_ascii_lowercase();
        if key == "schemeiduri" {
            scheme = val;
        } else if key == "value" {
            value = val;
        } else if key.ends_with("default_kid") {
            has_kid = true;
        }
    }
    if has_kid {
        return true;
    }
    scheme.contains("edef8ba9") // Widevine
        || scheme.contains("9a04f079") // PlayReady
        || scheme.contains("94ce86fb") // FairPlay
        || (scheme.contains("mp4protection") && value.contains("cenc"))
}

fn local_name(raw: &str) -> &str {
    raw.rsplit(':').next().unwrap_or(raw)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Mpd,
    Period,
    Adaptation,
    Representation,
    SegTemplate,
    SegList,
    Other,
}

fn parse_seg_template_attrs(attrs: Attributes<'_>) -> SegTemplateSpec {
    let mut spec = SegTemplateSpec::default();
    for a in attrs.flatten() {
        let key: &str = a.key.as_ref();
        let val = a.value.as_ref().to_string();
        if key.eq_ignore_ascii_case("media") {
            spec.media = Some(val);
        } else if key.eq_ignore_ascii_case("initialization") {
            spec.initialization = Some(val);
        } else if key.eq_ignore_ascii_case("timescale") {
            spec.timescale = val.trim().parse().unwrap_or(1);
        } else if key.eq_ignore_ascii_case("duration") {
            spec.duration = val.trim().parse().ok();
        } else if key.eq_ignore_ascii_case("startNumber") {
            spec.start_number = val.trim().parse().unwrap_or(1);
        }
    }
    spec
}

pub fn parse_mpd(text: &str, mpd_url: &str) -> Res<ParsedMpd> {
    let (clean_mpd_url, _) = strip_url_fragment(mpd_url);
    let mut reader = Reader::from_str(text);
    reader.config_mut().trim_text(true);

    let mut buf = Vec::new();
    let mut saw_mpd = false;
    let mut duration_sec: Option<f64> = None;
    let mut mpd_base_url: Option<String> = None;

    let mut period_index: usize = 0;
    let mut in_first_period = false;
    let mut period_base_url: Option<String> = None;

    let mut stack: Vec<Scope> = Vec::new();
    let mut cur_adapt: Option<AdaptBuilder> = None;
    let mut cur_rep: Option<RepBuilder> = None;
    let mut cur_tpl: Option<SegTemplateSpec> = None;
    let mut cur_list: Option<SegListSpec> = None;
    let mut adapts: Vec<AdaptBuilder> = Vec::new();
    let mut base_text = String::new();
    let mut in_base_url = false;

    loop {
        let ev = reader.read_event_into(&mut buf).map_err(|e| {
            DownloadError::new(ErrorKind::InvalidUrl, format!("invalid MPD XML: {e}"))
        })?;
        match ev {
            Event::Eof => break,
            Event::Start(ref e) | Event::Empty(ref e) => {
                let is_empty = matches!(ev, Event::Empty(_));
                let tag = local_name(e.name().as_ref()).to_string();
                match tag.as_str() {
                    "MPD" => {
                        saw_mpd = true;
                        if let Some(t) = attr_str(e.attributes(), "type") {
                            if t.trim().eq_ignore_ascii_case("dynamic") {
                                return Err(DownloadError::new(
                                    ErrorKind::LiveUnsupported,
                                    "live DASH streams (type=\"dynamic\") are not supported",
                                ));
                            }
                        }
                        if let Some(d) = attr_str(e.attributes(), "mediaPresentationDuration") {
                            duration_sec = parse_iso8601_duration(&d);
                        }
                        if !is_empty {
                            stack.push(Scope::Mpd);
                        }
                    }
                    "Period" => {
                        period_index += 1;
                        if period_index == 1 {
                            in_first_period = true;
                        }
                        if !is_empty {
                            stack.push(Scope::Period);
                        }
                    }
                    "AdaptationSet" if in_first_period => {
                        let mut ab = AdaptBuilder::default();
                        for a in e.attributes().flatten() {
                            let key: &str = a.key.as_ref();
                            let val = a.value.as_ref().to_string();
                            if key.eq_ignore_ascii_case("mimeType") {
                                ab.mime_type = Some(val);
                            } else if key.eq_ignore_ascii_case("contentType") {
                                ab.content_type = Some(val);
                            }
                        }
                        if is_empty {
                            adapts.push(ab);
                        } else {
                            cur_adapt = Some(ab);
                            stack.push(Scope::Adaptation);
                        }
                    }
                    "Representation" if in_first_period && cur_adapt.is_some() => {
                        let mut rb = RepBuilder::default();
                        for a in e.attributes().flatten() {
                            let key: &str = a.key.as_ref();
                            let val = a.value.as_ref().to_string();
                            if key.eq_ignore_ascii_case("id") {
                                rb.id = val;
                            } else if key.eq_ignore_ascii_case("mimeType") {
                                rb.mime_type = Some(val);
                            } else if key.eq_ignore_ascii_case("bandwidth") {
                                rb.bandwidth = val.trim().parse().unwrap_or(0);
                            } else if key.eq_ignore_ascii_case("width") {
                                rb.width = val.trim().parse().ok();
                            } else if key.eq_ignore_ascii_case("height") {
                                rb.height = val.trim().parse().ok();
                            }
                        }
                        if is_empty {
                            if let Some(ab) = cur_adapt.as_mut() {
                                ab.reps.push(rb);
                            }
                        } else {
                            cur_rep = Some(rb);
                            stack.push(Scope::Representation);
                        }
                    }
                    "SegmentTemplate" if in_first_period && cur_adapt.is_some() => {
                        let spec = parse_seg_template_attrs(e.attributes());
                        if is_empty {
                            if let Some(rep) = cur_rep.as_mut() {
                                rep.seg_template = Some(spec);
                            } else if let Some(ab) = cur_adapt.as_mut() {
                                ab.seg_template = Some(spec);
                            }
                        } else {
                            cur_tpl = Some(spec);
                            stack.push(Scope::SegTemplate);
                        }
                    }
                    "S" if matches!(stack.last(), Some(Scope::SegTemplate | Scope::Other)) => {
                        if let Some(tpl) = cur_tpl.as_mut() {
                            let mut t: Option<u64> = None;
                            let mut d: Option<u64> = None;
                            let mut r: i64 = 0;
                            for a in e.attributes().flatten() {
                                let key: &str = a.key.as_ref();
                                let val: &str = a.value.as_ref();
                                if key.eq_ignore_ascii_case("t") {
                                    t = val.trim().parse().ok();
                                } else if key.eq_ignore_ascii_case("d") {
                                    d = val.trim().parse().ok();
                                } else if key.eq_ignore_ascii_case("r") {
                                    r = val.trim().parse().unwrap_or(0);
                                }
                            }
                            if let Some(dur) = d {
                                tpl.timeline.push((t, dur, r));
                            }
                        }
                        if !is_empty {
                            stack.push(Scope::Other);
                        }
                    }
                    "SegmentList" if in_first_period && cur_adapt.is_some() => {
                        if is_empty {
                            // empty SegmentList adds nothing
                        } else {
                            cur_list = Some(SegListSpec::default());
                            stack.push(Scope::SegList);
                        }
                    }
                    "Initialization" if matches!(stack.last(), Some(Scope::SegList)) => {
                        if let Some(list) = cur_list.as_mut() {
                            list.init_url = attr_str(e.attributes(), "sourceURL");
                            list.init_range = attr_str(e.attributes(), "range")
                                .and_then(|r| parse_byte_range(&r));
                        }
                        if !is_empty {
                            stack.push(Scope::Other);
                        }
                    }
                    "SegmentURL" if matches!(stack.last(), Some(Scope::SegList)) => {
                        if let Some(list) = cur_list.as_mut() {
                            let media = attr_str(e.attributes(), "media").unwrap_or_default();
                            let range = attr_str(e.attributes(), "mediaRange")
                                .and_then(|r| parse_byte_range(&r));
                            if !media.is_empty() || range.is_some() {
                                list.items.push((media, range));
                            }
                        }
                        if !is_empty {
                            stack.push(Scope::Other);
                        }
                    }
                    "ContentProtection" if in_first_period => {
                        if is_drm_attrs(e.attributes()) {
                            if let Some(rep) = cur_rep.as_mut() {
                                rep.has_drm = true;
                            } else if let Some(ab) = cur_adapt.as_mut() {
                                ab.has_drm = true;
                            }
                        }
                        if !is_empty {
                            stack.push(Scope::Other);
                        }
                    }
                    "BaseURL" if !is_empty => {
                        base_text.clear();
                        in_base_url = true;
                        stack.push(Scope::Other);
                    }
                    _ => {
                        if !is_empty {
                            stack.push(Scope::Other);
                        }
                    }
                }
            }
            Event::Text(e) if in_base_url => {
                let raw = e.xml10_content();
                match quick_xml::escape::unescape(&raw) {
                    Ok(decoded) => base_text.push_str(&decoded),
                    Err(_) => base_text.push_str(&raw),
                }
            }
            Event::End(ref e) => {
                let qname = e.name();
                let tag = local_name(qname.as_ref());
                if tag == "BaseURL" && in_base_url {
                    in_base_url = false;
                    let trimmed = base_text.trim().to_string();
                    if !trimmed.is_empty() {
                        // stack currently has Scope::Other for BaseURL on top
                        let parent_scope = if stack.len() >= 2 {
                            stack.get(stack.len() - 2).copied()
                        } else {
                            None
                        };
                        match parent_scope {
                            Some(Scope::Mpd) => mpd_base_url = Some(trimmed),
                            Some(Scope::Period) if in_first_period => {
                                period_base_url = Some(trimmed)
                            }
                            Some(Scope::Adaptation) => {
                                if let Some(ab) = cur_adapt.as_mut() {
                                    ab.base_url = Some(trimmed);
                                }
                            }
                            Some(Scope::Representation) => {
                                if let Some(rb) = cur_rep.as_mut() {
                                    rb.base_url = Some(trimmed);
                                }
                            }
                            _ => {}
                        }
                    }
                    stack.pop();
                    continue;
                }
                match stack.pop() {
                    Some(Scope::Period) => {
                        in_first_period = false;
                    }
                    Some(Scope::Adaptation) => {
                        if let Some(ab) = cur_adapt.take() {
                            adapts.push(ab);
                        }
                    }
                    Some(Scope::Representation) => {
                        if let (Some(ab), Some(rb)) = (cur_adapt.as_mut(), cur_rep.take()) {
                            ab.reps.push(rb);
                        }
                    }
                    Some(Scope::SegTemplate) => {
                        if let Some(tpl) = cur_tpl.take() {
                            if let Some(rep) = cur_rep.as_mut() {
                                rep.seg_template = Some(tpl);
                            } else if let Some(ab) = cur_adapt.as_mut() {
                                ab.seg_template = Some(tpl);
                            }
                        }
                    }
                    Some(Scope::SegList) => {
                        if let Some(list) = cur_list.take() {
                            if let Some(rep) = cur_rep.as_mut() {
                                rep.seg_list = Some(list);
                            } else if let Some(ab) = cur_adapt.as_mut() {
                                ab.seg_list = Some(list);
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        buf.clear();
    }

    if !saw_mpd {
        return Err(DownloadError::new(
            ErrorKind::InvalidUrl,
            "not a valid DASH MPD manifest",
        ));
    }

    let mut base_after_mpd = clean_mpd_url.clone();
    if let Some(ref b) = mpd_base_url {
        base_after_mpd = resolve_url(&base_after_mpd, b);
    }
    if let Some(ref b) = period_base_url {
        base_after_mpd = resolve_url(&base_after_mpd, b);
    }

    let mut video_reps = Vec::new();
    let mut audio_reps = Vec::new();

    for ab in adapts {
        let mut adapt_base = base_after_mpd.clone();
        if let Some(ref b) = ab.base_url {
            adapt_base = resolve_url(&adapt_base, b);
        }
        let role = classify_adaptation(&ab);
        let track_prefix = match role {
            MediaRole::Video => "v",
            MediaRole::Audio => "a",
            MediaRole::Other => continue,
        };

        for (rep_idx, rb) in ab.reps.iter().enumerate() {
            let mut rep_base = adapt_base.clone();
            if let Some(ref b) = rb.base_url {
                rep_base = resolve_url(&rep_base, b);
            }
            let rep_id = if rb.id.is_empty() {
                format!("{track_prefix}{rep_idx}")
            } else {
                rb.id.clone()
            };
            let tpl = match (&ab.seg_template, &rb.seg_template) {
                (Some(a), Some(r)) => Some(r.merged_over(a)),
                (None, Some(r)) => Some(r.clone()),
                (Some(a), None) => Some(a.clone()),
                (None, None) => None,
            };
            let list = match (&ab.seg_list, &rb.seg_list) {
                (Some(a), Some(r)) => Some(r.merged_over(a)),
                (None, Some(r)) => Some(r.clone()),
                (Some(a), None) => Some(a.clone()),
                (None, None) => None,
            };

            let mut init: Option<DashSeg> = None;
            let mut segments: Vec<DashSeg> = Vec::new();

            if let Some(ref st) = tpl {
                if let Some(ref init_tpl) = st.initialization {
                    let expanded =
                        expand_template(init_tpl, &rep_id, rb.bandwidth, st.start_number.max(1), 0);
                    init = Some(DashSeg {
                        key_name: format!("{track_prefix}_init"),
                        uri: resolve_url(&rep_base, &expanded),
                        range: None,
                    });
                }
                if let Some(ref media_tpl) = st.media {
                    let start_num = st.start_number.max(1);
                    if !st.timeline.is_empty() {
                        let mut num = start_num;
                        let mut cur_t = 0u64;
                        'timeline: for &(t_opt, dur, repeat) in &st.timeline {
                            if let Some(t) = t_opt {
                                cur_t = t;
                            }
                            let count = if repeat < 0 {
                                if let (Some(total_sec), true) = (duration_sec, dur > 0) {
                                    let ts = st.timescale.max(1) as f64;
                                    let end_ticks = (total_sec * ts).ceil() as u64;
                                    let rem_ticks = end_ticks.saturating_sub(cur_t);
                                    rem_ticks.div_ceil(dur).min(MAX_GENERATED_SEGS as u64)
                                } else {
                                    1
                                }
                            } else {
                                (repeat as u64).saturating_add(1)
                            };
                            for _ in 0..count {
                                if segments.len() >= MAX_GENERATED_SEGS {
                                    break 'timeline;
                                }
                                let expanded =
                                    expand_template(media_tpl, &rep_id, rb.bandwidth, num, cur_t);
                                segments.push(DashSeg {
                                    key_name: format!("{track_prefix}_{}", segments.len()),
                                    uri: resolve_url(&rep_base, &expanded),
                                    range: None,
                                });
                                num = num.saturating_add(1);
                                cur_t = cur_t.saturating_add(dur);
                            }
                        }
                    } else if let (Some(dur), Some(total_sec)) = (st.duration, duration_sec) {
                        if dur > 0 {
                            let ts = st.timescale.max(1) as f64;
                            let count = ((total_sec * ts) / dur as f64).ceil() as usize;
                            let count = count.clamp(1, MAX_GENERATED_SEGS);
                            for i in 0..count {
                                let num = start_num + i as u64;
                                let time = (i as u64).saturating_mul(dur);
                                let expanded =
                                    expand_template(media_tpl, &rep_id, rb.bandwidth, num, time);
                                segments.push(DashSeg {
                                    key_name: format!("{track_prefix}_{i}"),
                                    uri: resolve_url(&rep_base, &expanded),
                                    range: None,
                                });
                            }
                        }
                    }
                }
            }

            if segments.is_empty() {
                if let Some(ref sl) = list {
                    if let Some(ref src) = sl.init_url {
                        init = Some(DashSeg {
                            key_name: format!("{track_prefix}_init"),
                            uri: resolve_url(&rep_base, src),
                            range: sl.init_range,
                        });
                    }
                    for (i, (media, range)) in sl.items.iter().enumerate() {
                        let uri = if media.is_empty() {
                            rep_base.clone()
                        } else {
                            resolve_url(&rep_base, media)
                        };
                        segments.push(DashSeg {
                            key_name: format!("{track_prefix}_{i}"),
                            uri,
                            range: *range,
                        });
                    }
                }
            }

            if segments.is_empty() && rb.base_url.is_some() {
                segments.push(DashSeg {
                    key_name: format!("{track_prefix}_0"),
                    uri: rep_base,
                    range: None,
                });
            }

            if segments.is_empty() {
                continue;
            }

            let rep = DashRep {
                id: rep_id,
                bandwidth: rb.bandwidth,
                width: rb.width,
                height: rb.height,
                init,
                segments,
                has_drm: ab.has_drm || rb.has_drm,
            };
            match role {
                MediaRole::Video => video_reps.push(rep),
                MediaRole::Audio => audio_reps.push(rep),
                MediaRole::Other => {}
            }
        }
    }

    Ok(ParsedMpd {
        duration_sec,
        video_reps,
        audio_reps,
    })
}

fn classify_adaptation(ab: &AdaptBuilder) -> MediaRole {
    let ct = ab
        .content_type
        .as_deref()
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if ct == "video" {
        return MediaRole::Video;
    }
    if ct == "audio" {
        return MediaRole::Audio;
    }
    let mt = ab
        .mime_type
        .as_deref()
        .or_else(|| ab.reps.first().and_then(|r| r.mime_type.as_deref()))
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if mt.starts_with("video/") {
        return MediaRole::Video;
    }
    if mt.starts_with("audio/") {
        return MediaRole::Audio;
    }
    if ab
        .reps
        .iter()
        .any(|r| r.width.is_some() || r.height.is_some())
    {
        return MediaRole::Video;
    }
    MediaRole::Other
}

fn pick_tracks<'a>(
    mpd: &'a ParsedMpd,
    want_rep_id: Option<&str>,
) -> Result<(Option<&'a DashRep>, Option<&'a DashRep>), DriveErr> {
    let pick_best_non_drm = |reps: &'a [DashRep]| -> Option<&'a DashRep> {
        reps.iter()
            .filter(|r| !r.has_drm)
            .max_by_key(|r| (r.height.unwrap_or(0), r.bandwidth))
    };

    let video = if let Some(wanted) = want_rep_id {
        mpd.video_reps
            .iter()
            .find(|r| r.id == wanted && !r.has_drm)
            .or_else(|| pick_best_non_drm(&mpd.video_reps))
    } else {
        pick_best_non_drm(&mpd.video_reps)
    };
    let audio = mpd
        .audio_reps
        .iter()
        .filter(|r| !r.has_drm)
        .max_by_key(|r| r.bandwidth);

    if video.is_none() && audio.is_none() {
        let any_drm =
            mpd.video_reps.iter().any(|r| r.has_drm) || mpd.audio_reps.iter().any(|r| r.has_drm);
        if any_drm {
            return Err(DriveErr::Fatal(
                ErrorKind::DrmUnsupported,
                "DRM-protected DASH streams are not supported".into(),
            ));
        }
        return Err(DriveErr::Fatal(
            ErrorKind::Internal,
            "MPD manifest has no downloadable video/audio streams".into(),
        ));
    }
    Ok((video, audio))
}

struct DashShared {
    done_count: AtomicU64,
    bytes: AtomicU64,
    fatal: Mutex<Option<ErrorKind>>,
    restart: AtomicBool,
}

impl DashShared {
    fn take_fatal(&self) -> Option<ErrorKind> {
        self.fatal.lock().unwrap().take()
    }
}

async fn dash_worker(
    ctx: Arc<Ctx>,
    client: reqwest::Client,
    segs: Arc<Vec<DashSeg>>,
    tmp: Arc<PathBuf>,
    shared: Arc<DashShared>,
    cursor: Arc<AtomicU64>,
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
        let seg = &segs[idx as usize];
        let final_path = tmp.join(&seg.key_name);
        if let Ok(m) = std::fs::metadata(&final_path) {
            if m.len() > 0 {
                continue;
            }
            let _ = std::fs::remove_file(&final_path);
        }
        match download_one_seg(&ctx, &client, seg, &final_path).await {
            Ok(len) => {
                shared.done_count.fetch_add(1, Ordering::Relaxed);
                shared.bytes.fetch_add(len, Ordering::Relaxed);
            }
            Err(kind) => {
                if ctx.cancel.is_cancelled() {
                    return;
                }
                if matches!(kind, ErrorKind::AuthExpired | ErrorKind::UrlExpired) {
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

async fn download_one_seg(
    ctx: &Ctx,
    client: &reqwest::Client,
    seg: &DashSeg,
    final_path: &Path,
) -> Result<u64, ErrorKind> {
    let mut attempts = 0;
    loop {
        if ctx.cancel.is_cancelled() {
            return Err(ErrorKind::NetworkConnectionReset);
        }
        attempts += 1;
        if attempts > SEG_ATTEMPTS {
            return Err(ErrorKind::NetworkConnectionReset);
        }
        let bytes = match fetch_segment_bytes(ctx, client, &seg.uri, seg.range).await {
            Ok(b) => b,
            Err(e) if e.is_retryable() => {
                if !sleep_or_cancel(
                    &ctx.cancel,
                    Duration::from_secs(1 << attempts.min(4).saturating_sub(1)),
                )
                .await
                {
                    return Err(ErrorKind::NetworkConnectionReset);
                }
                continue;
            }
            Err(e) => return Err(e),
        };
        if bytes.is_empty() {
            return Err(ErrorKind::NetworkConnectionReset);
        }
        let mut rem = bytes.len() as u64;
        while rem > 0 {
            let take = rem.min(dl_throttle::Limiter::ACQUIRE_CAP);
            ctx.limiter.acquire(take).await;
            rem -= take;
        }
        let part = tmp_part_path(final_path);
        let mut f = std::fs::File::create(&part).map_err(|e| io_err_kind(&e))?;
        f.write_all(&bytes).map_err(|e| io_err_kind(&e))?;
        drop(f);
        std::fs::rename(&part, final_path).map_err(|e| io_err_kind(&e))?;
        return Ok(bytes.len() as u64);
    }
}

fn tmp_part_path(final_path: &Path) -> PathBuf {
    let mut p = final_path.as_os_str().to_os_string();
    p.push(".part");
    PathBuf::from(p)
}

fn dash_target_paths(
    ctx: &Ctx,
    task: &dl_model::Task,
    has_video: bool,
) -> Result<TargetPaths, DriveErr> {
    let (clean_url, _) = strip_url_fragment(&task.url);
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
    let base = dl_filename::pick_name(task.filename.as_deref(), None, &clean_url, None)
        .replace(".mpd", "");
    let base = if base.is_empty() {
        "dash_video".to_string()
    } else {
        base
    };
    let ext = if has_video { "mp4" } else { "m4a" };
    let name = if base.to_ascii_lowercase().ends_with(&format!(".{ext}")) {
        base
    } else {
        format!("{base}.{ext}")
    };
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

fn concat_keys(tmp: &Path, keys: &[String], out_path: &Path) -> Result<(), DriveErr> {
    let out_file = std::fs::File::create(out_path)
        .map_err(|e| DriveErr::Fatal(io_err_kind(&e), format!("create track: {e}")))?;
    let mut writer = std::io::BufWriter::with_capacity(256 * 1024, out_file);
    for key in keys {
        let mut f = std::fs::File::open(tmp.join(key))
            .map_err(|e| DriveErr::Fatal(io_err_kind(&e), format!("open seg {key}: {e}")))?;
        std::io::copy(&mut f, &mut writer)
            .map_err(|e| DriveErr::Fatal(io_err_kind(&e), format!("write seg {key}: {e}")))?;
    }
    writer
        .flush()
        .map_err(|e| DriveErr::Fatal(io_err_kind(&e), format!("flush track: {e}")))?;
    writer
        .get_ref()
        .sync_data()
        .map_err(|e| DriveErr::Fatal(io_err_kind(&e), format!("sync track: {e}")))?;
    Ok(())
}

async fn mux_video_audio(
    video: &Path,
    audio: &Path,
    out: &Path,
    ffmpeg_hint: Option<&str>,
) -> Result<(), DriveErr> {
    let ffmpeg = find_ffmpeg_with(ffmpeg_hint).ok_or_else(|| {
        DriveErr::Fatal(
            ErrorKind::Internal,
            "DASH 音视频合并需要 ffmpeg，请在设置中配置 ffmpeg 路径".into(),
        )
    })?;
    let _ = std::fs::remove_file(out);
    let mut cmd = tokio::process::Command::new(ffmpeg);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let output = cmd
        .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(video)
        .arg("-i")
        .arg(audio)
        .args(["-c", "copy", "-movflags", "+faststart"])
        .arg(out)
        .output()
        .await
        .map_err(|e| DriveErr::Fatal(ErrorKind::Internal, format!("spawn ffmpeg: {e}")))?;
    if output.status.success() {
        Ok(())
    } else {
        let msg = String::from_utf8_lossy(&output.stderr)
            .chars()
            .take(300)
            .collect::<String>();
        Err(DriveErr::Fatal(
            ErrorKind::Internal,
            format!("ffmpeg mux failed: {msg}"),
        ))
    }
}

pub(crate) async fn phase(ctx: Arc<Ctx>) -> Result<(), DriveErr> {
    let ctx = &*ctx;
    let task = ctx.engine.require(&ctx.task_id).await.map_err(drive_from)?;
    let (clean_url, wanted_rep) = strip_url_fragment(&task.url);

    let text = fetch_text(ctx, &clean_url).await?;
    if ctx.cancel.is_cancelled() {
        return Err(DriveErr::Paused);
    }

    let mpd = parse_mpd(&text, &clean_url).map_err(drive_from)?;
    if !mpd.video_reps.is_empty() {
        let list: Vec<serde_json::Value> = mpd
            .video_reps
            .iter()
            .filter(|r| !r.has_drm)
            .map(|r| {
                let res = r.width.zip(r.height).map(|(w, h)| format!("{w}x{h}"));
                serde_json::json!({
                    "bandwidth": r.bandwidth,
                    "resolution": res,
                    "url": format!("{clean_url}#adl_rep={}", r.id),
                })
            })
            .collect();
        if !list.is_empty() {
            if let Ok(json) = serde_json::to_string(&list) {
                let _ = ctx.engine.record_variants(&ctx.task_id, &json).await;
            }
        }
    }

    let (video_opt, audio_opt) = pick_tracks(&mpd, wanted_rep.as_deref())?;
    let mut all_segs: Vec<DashSeg> = Vec::new();
    let mut video_keys: Vec<String> = Vec::new();
    let mut audio_keys: Vec<String> = Vec::new();

    if let Some(v) = video_opt {
        if let Some(init) = &v.init {
            video_keys.push(init.key_name.clone());
            all_segs.push(init.clone());
        }
        for s in &v.segments {
            video_keys.push(s.key_name.clone());
            all_segs.push(s.clone());
        }
    }
    if let Some(a) = audio_opt {
        if let Some(init) = &a.init {
            audio_keys.push(init.key_name.clone());
            all_segs.push(init.clone());
        }
        for s in &a.segments {
            audio_keys.push(s.key_name.clone());
            all_segs.push(s.clone());
        }
    }

    if all_segs.is_empty() {
        return Err(DriveErr::Fatal(
            ErrorKind::Internal,
            "DASH manifest has no segments".into(),
        ));
    }

    ctx.engine
        .record_probe(
            &ctx.task_id,
            Some(all_segs.len() as u64),
            None,
            None,
            Some("application/dash+xml".into()),
            Some(clean_url),
        )
        .await
        .map_err(drive_from)?;

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
            format!("create dash tmp dir: {e}"),
        )
    })?;

    let sig_src = format!("{text}|{}", wanted_rep.as_deref().unwrap_or(""));
    let manifest_hash = xxhash_rust::xxh64::xxh64(sig_src.as_bytes(), 0);
    let meta_path = tmp_dir.join("manifest.meta");
    if let Ok(existing) = std::fs::read_to_string(&meta_path) {
        if existing != format!("{manifest_hash:x}") {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            let _ = std::fs::create_dir_all(&tmp_dir);
        }
    }
    let _ = std::fs::write(&meta_path, format!("{manifest_hash:x}"));

    let existing_done = all_segs
        .iter()
        .filter(|s| {
            std::fs::metadata(tmp_dir.join(&s.key_name))
                .map(|m| m.len() > 0)
                .unwrap_or(false)
        })
        .count() as u64;

    let total = all_segs.len() as u64;
    let shared = Arc::new(DashShared {
        done_count: AtomicU64::new(existing_done),
        bytes: AtomicU64::new(0),
        fatal: Mutex::new(None),
        restart: AtomicBool::new(false),
    });

    let prog_cancel = ctx.cancel.child_token();
    {
        let ectx = ctx.clone();
        let eshared = shared.clone();
        let p_cancel = prog_cancel.clone();
        tokio::spawn(async move {
            let mut last_bytes = 0u64;
            let mut last_t = std::time::Instant::now();
            let mut bps = 0f64;
            let mut last_persisted = std::time::Instant::now();
            loop {
                tokio::select! {
                    _ = p_cancel.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_millis(300)) => {}
                }
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
                        total: Some(total),
                        bps: bps as u64,
                        segments: None,
                    });
                    if last_persisted.elapsed() >= Duration::from_secs(2) {
                        let _ = ectx.engine.record_downloaded(&ectx.task_id, done).await;
                        last_persisted = std::time::Instant::now();
                    }
                }
                if done >= total && bytes == last_bytes {
                    break;
                }
            }
        });
    }

    let dash_client = dl_net::build_pooled_worker_client(ctx.opts.proxy.as_deref(), WORKERS)
        .unwrap_or_else(|_| ctx.client.clone());
    let segs_arc = Arc::new(all_segs);
    let tmp_arc = Arc::new(tmp_dir);
    let cursor = Arc::new(AtomicU64::new(0));
    let mut handles = Vec::new();
    for _ in 0..WORKERS.min(segs_arc.len().max(1)) {
        handles.push(tokio::spawn(dash_worker(
            Arc::new(ctx.clone()),
            dash_client.clone(),
            segs_arc.clone(),
            tmp_arc.clone(),
            shared.clone(),
            cursor.clone(),
        )));
    }
    for h in handles {
        let _ = h.await;
    }
    prog_cancel.cancel();

    if let Some(kind) = shared.take_fatal() {
        return Err(DriveErr::Fatal(kind, "DASH segment download failed".into()));
    }
    if ctx.cancel.is_cancelled() {
        return Err(DriveErr::Paused);
    }
    if shared.restart.load(Ordering::Relaxed) {
        let _ = std::fs::remove_dir_all(&*tmp_arc);
        return Err(DriveErr::Restart);
    }
    if shared.done_count.load(Ordering::Relaxed) < total {
        return Err(DriveErr::Fatal(
            ErrorKind::Internal,
            "some DASH segments missing after download".into(),
        ));
    }

    ctx.engine
        .transition(&ctx.task_id, dl_model::TaskStatus::Merging, None)
        .await
        .map_err(drive_from)?;

    let paths = dash_target_paths(ctx, &task, !video_keys.is_empty())?;
    let video_path = tmp_arc.join("_video.m4s");
    let audio_path = tmp_arc.join("_audio.m4a");
    let has_video = !video_keys.is_empty();
    let has_audio = !audio_keys.is_empty();

    let tmp_clone = (*tmp_arc).clone();
    let v_keys = video_keys.clone();
    let a_keys = audio_keys.clone();
    let vp_clone = video_path.clone();
    let ap_clone = audio_path.clone();
    tokio::task::spawn_blocking(move || -> Result<(), DriveErr> {
        if !v_keys.is_empty() {
            concat_keys(&tmp_clone, &v_keys, &vp_clone)?;
        }
        if !a_keys.is_empty() {
            concat_keys(&tmp_clone, &a_keys, &ap_clone)?;
        }
        Ok(())
    })
    .await
    .map_err(|e| DriveErr::Fatal(ErrorKind::Internal, format!("dash merge task: {e}")))??;

    if has_video && has_audio {
        let muxed = tmp_arc.join("_muxed.mp4");
        let ffmpeg_hint = ctx.engine.get_setting("ffmpeg_path").await.ok().flatten();
        mux_video_audio(&video_path, &audio_path, &muxed, ffmpeg_hint.as_deref()).await?;
        move_file_or_copy(&muxed, &paths.part)
            .map_err(|e| DriveErr::Fatal(io_err_kind(&e), format!("move muxed: {e}")))?;
    } else if has_video {
        move_file_or_copy(&video_path, &paths.part)
            .map_err(|e| DriveErr::Fatal(io_err_kind(&e), format!("move video: {e}")))?;
    } else {
        move_file_or_copy(&audio_path, &paths.part)
            .map_err(|e| DriveErr::Fatal(io_err_kind(&e), format!("move audio: {e}")))?;
    }

    ctx.engine
        .transition(&ctx.task_id, dl_model::TaskStatus::Verifying, None)
        .await
        .map_err(drive_from)?;

    let meta = std::fs::metadata(&paths.part)
        .map_err(|e| DriveErr::Fatal(io_err_kind(&e), format!("verify: {e}")))?;
    if meta.len() == 0 {
        return Err(DriveErr::Fatal(
            ErrorKind::Internal,
            "merged DASH output is empty".into(),
        ));
    }

    if let Some((algo, expect)) = ctx.checksum.clone() {
        let part_path = paths.part.clone();
        let algo_c = algo.clone();
        let ok = tokio::task::spawn_blocking(move || {
            crate::checksum::verify_file(&part_path, &algo_c, &expect)
        })
        .await
        .map_err(|e| DriveErr::Fatal(ErrorKind::Internal, e.to_string()))?
        .map_err(|e| DriveErr::Fatal(e.kind, e.message))?;
        if !ok {
            return Err(DriveErr::Fatal(
                ErrorKind::ChecksumMismatch,
                format!("checksum {algo} mismatch"),
            ));
        }
    }

    finalize(ctx, &paths).await?;
    let _ = std::fs::remove_dir_all(&*tmp_arc);
    ctx.engine
        .transition(&ctx.task_id, dl_model::TaskStatus::Done, None)
        .await
        .map_err(drive_from)?;
    Ok(())
}
