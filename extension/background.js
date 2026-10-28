// Anything Downloader 扩展 — 资源嗅探 + 接管浏览器下载 + 推送（v0.2.2）
//
// 架构（DESIGN §7）：
//   嗅探：onBeforeRequest（无头预筛）→ onBeforeSendHeaders（extraHeaders 抓
//         Referer/Cookie/UA）→ onCompleted（MIME/大小过滤）
//   持久化：SW 随时被回收 → 嗅探列表 500ms 防抖写入 storage.session
//           （跨 SW 回收存活、不跨浏览器重启）；SW 唤醒时恢复合并
//   接管：downloads.onCreated → pause → 推送（复用嗅探头）→ onChanged 复核后
//         cancel+erase / 失败 resume 放行
//   NM：SW 持有长连接 port（避免 sendNativeMessage 每条消息 spawn 进程），
//       断链自动重连，挂起请求超时兜底
//
// 站点适配层（docs/MEDIA-ADAPTERS.md）：hooks/main-world-hook.js 在 MAIN world
//   捕获平台 API 响应 → bridge.js 转发到这里（环形缓冲）→ adapters/* 提取直链
//   → pushToApp 推送。适配器注册表与抖音转换助手与 content world 共用（importScripts）。

importScripts("lib/zip.js", "adapters/types.js", "adapters/core.js");

const NM_HOST = "anything_dl";
const MIN_BYTES = 1024 * 1024; // 直链媒体噪音阈值（HLS 清单/分片豁免）
const HLS_RE = /\.(m3u8)(?=[?#]|$)/i;
// —— 资源扩展名白名单（唯一事实来源）：不在表里的 URL 一律视为非资源，
//    页面跳转/js/css/json/接口地址永远不会进嗅探列表；图片单独一个开关
const EXT_TYPE = (() => {
  const m = {};
  const add = (type, exts) => {
    for (const e of exts.split("|")) m[e] = type;
  };
  add("video", "mp4|mkv|webm|flv|avi|mov|wmv|rmvb|m4v|3gp");
  add("hls", "m3u8");
  add("dash", "mpd");
  add("segment", "ts|m4s");
  add("audio", "mp3|flac|m4a|aac|ogg|wav|ape|opus|wma");
  add("archive", "zip|rar|7z|tar|gz|bz2|xz|zst");
  add("binary", "exe|msi|apk|ipa|dmg|iso|img|deb|rpm|pkg|appimage");
  add("doc", "pdf|epub|mobi|azw3|doc|docx|xls|xlsx|ppt|pptx|csv|chm|srt|ass|ssa|vtt");
  add("image", "jpg|jpeg|png|gif|webp|svg|bmp|ico|avif");
  return m;
})();
const esc = (e) => e.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const mediaExts = Object.entries(EXT_TYPE)
  .filter(([, t]) => t !== "image")
  .map(([e]) => esc(e));
const imageExts = Object.entries(EXT_TYPE)
  .filter(([, t]) => t === "image")
  .map(([e]) => esc(e));
const MEDIA_RE = new RegExp("\\.(" + mediaExts.join("|") + ")(?=[?#]|$)", "i");
const IMAGE_RE = new RegExp("\\.(" + imageExts.join("|") + ")(?=[?#]|$)", "i");

/** MIME → 资源类型（无扩展名 URL 的兜底判定；非资源返回 null，text/html 永不命中） */
function mimeToType(mime) {
  if (!mime) return null;
  const m = mime.split(";")[0].trim().toLowerCase();
  if (m === "application/vnd.apple.mpegurl" || m === "application/x-mpegurl") return "hls";
  if (m === "application/dash+xml") return "dash";
  if (m.startsWith("video/")) return "video";
  if (m.startsWith("audio/")) return "audio";
  if (m.startsWith("image/")) return "image";
  if (m === "application/zip" || m === "application/x-rar-compressed" ||
      m === "application/x-7z-compressed" || m === "application/gzip" ||
      m === "application/x-tar") return "archive";
  if (m === "application/pdf" || m === "application/epub+zip" ||
      m === "application/msword" || m === "application/vnd.ms-excel" ||
      m === "application/vnd.ms-powerpoint" ||
      m.startsWith("application/vnd.openxmlformats-officedocument")) return "doc";
  if (m === "application/octet-stream" || m === "application/vnd.android.package-archive" ||
      m === "application/x-msdownload" || m === "application/x-apple-diskimage" ||
      m === "application/x-iso9660-image") return "binary";
  return null;
}

// —— 接管规则（DESIGN §7.3）
const TAKEOVER_RE =
  /\.(zip|rar|7z|tar|gz|mp4|mkv|webm|mp3|flac|m4a|aac|ogg|wav|avi|mov|wmv|pdf|epub|apk|exe|msi|dmg|iso)(?=[?#]|$)/i;
const MIN_TAKEOVER_BYTES = 1024 * 1024;

// —— yt-dlp 托管域名（镜像 crates/dl-engine/src/ytdlp_task.rs is_ytdlp_host；
//    引擎建任务按此自动转 TaskKind::Ytdlp）。视频悬浮下载的兜底路线：MSE 站点
//    （YouTube/B站等）无直链可取时推页面 URL 交 yt-dlp 托管。改引擎侧须同步。
const YTDLP_HOSTS = [
  "youtube.com",
  "youtu.be",
  "youtube-nocookie.com",
  "x.com",
  "twitter.com",
  "bilibili.com",
  "b23.tv",
  "tiktok.com",
  "douyin.com",
  "iesdouyin.com",
];

const MAX_PER_TAB = 100; // 单页资源上限（超限丢最旧，storage.session 配额治理）

// —— 嗅探配置（popup 设置页写入，storage.local.sniffCfg；内存缓存避免监听器内异步）
const CFG_DEFAULTS = {
  types: { video: true, hls: true, audio: true, archive: true, binary: true, doc: true, image: false, other: true },
  minSizeMB: 1,
};
let CFG = JSON.parse(JSON.stringify(CFG_DEFAULTS));
async function loadCfg() {
  try {
    const { sniffCfg } = await chrome.storage.local.get("sniffCfg");
    if (sniffCfg) {
      CFG = {
        types: { ...CFG_DEFAULTS.types, ...(sniffCfg.types || {}) },
        minSizeMB: sniffCfg.minSizeMB ?? 1,
      };
    }
  } catch (_) {}
}
loadCfg();
chrome.storage.onChanged.addListener((changes, area) => {
  if (area === "local" && changes.sniffCfg) loadCfg();
});
function groupOf(type) {
  if (type === "hls" || type === "dash" || type === "segment") return "video";
  if (type === "image") return "image";
  if (type === "binary") return "binary";
  if (type === "file") return "doc";
  return type;
}
function groupAllowed(type) {
  return CFG.types[groupOf(type)] !== false;
}
function minBytesCfg() {
  return Math.max(0, CFG.minSizeMB || 0) * 1024 * 1024;
}
const RECENT_HEADERS_MAX = 500;
const RECENT_HEADERS_TTL = 60 * 1000;

// ---------------------------------------------------------------- NM 长连接

let port = null;
let msgSeq = 0;
const pending = new Map(); // id -> { resolve, timer }

function ensurePort() {
  if (port) return;
  port = chrome.runtime.connectNative(NM_HOST);
  port.onMessage.addListener((resp) => {
    const p = pending.get(resp.id);
    if (p) {
      clearTimeout(p.timer);
      pending.delete(resp.id);
      p.resolve(resp);
    }
  });
  port.onDisconnect.addListener(() => {
    port = null;
    for (const [, p] of pending) {
      clearTimeout(p.timer);
      p.resolve({ ok: false, error: "app_not_running" });
    }
    pending.clear();
  });
}

/** NM 请求-响应（port 串行 roundtrip）；超时/断链以结构化错误兜底。 */
function nmCall(method, params, timeoutMs = 8000) {
  return new Promise((resolve) => {
    // connectNative 找不到/无法启动 host 时会同步抛错——必须在这里兜底，
    // 否则 Promise 永不落定，popup 的 sendMessage 将永久挂起
    try {
      ensurePort();
    } catch (e) {
      resolve({ ok: false, error: "app_not_running" });
      return;
    }
    const id = ++msgSeq;
    const timer = setTimeout(() => {
      pending.delete(id);
      resolve({ ok: false, error: "timeout" });
    }, timeoutMs);
    pending.set(id, { resolve, timer });
    try {
      port.postMessage({ v: 1, id, method, params });
    } catch (e) {
      clearTimeout(timer);
      pending.delete(id);
      port = null;
      resolve({ ok: false, error: "app_not_running" });
    }
  });
}

// ---------------------------------------------------------------- 嗅探存储

/** tabId -> Map(url -> rec)。内存为主，storage.session 做跨 SW 回收快照。 */
const byTab = new Map();

let flushTimer = null;
function scheduleFlush() {
  if (flushTimer) return;
  flushTimer = setTimeout(flushSniff, 500); // 防抖：不逐请求写 storage（IPC 风暴）
}

async function flushSniff() {
  flushTimer = null;
  const out = {};
  for (const [tabId, m] of byTab) {
    out[tabId] = [...m.values()];
  }
  try {
    await chrome.storage.session.set({ sniff: out });
  } catch (_) {
    // 配额超限（10MB）：降级为只保 m3u8 清单，丢分片明细后重试一次
    const slim = {};
    for (const [tabId, arr] of Object.entries(out)) {
      slim[tabId] = arr.filter((r) => r.type === "hls");
    }
    try {
      await chrome.storage.session.set({ sniff: slim });
    } catch (_) {}
  }
}

/** SW 唤醒：从 storage.session 恢复（合并式，不清掉唤醒后新产生的事件）。 */
async function restoreSniff() {
  try {
    const { sniff } = await chrome.storage.session.get("sniff");
    if (!sniff) return;
    for (const [tid, arr] of Object.entries(sniff)) {
      const tabId = Number(tid);
      const m = tabMap(tabId);
      for (const rec of arr) {
        if (!m.has(rec.url)) m.set(rec.url, rec);
      }
      updateBadge(tabId);
    }
  } catch (_) {}
}
restoreSniff();

function tabMap(tabId) {
  let m = byTab.get(tabId);
  if (!m) {
    m = new Map();
    byTab.set(tabId, m);
  }
  return m;
}

function putRec(tabId, rec) {
  const m = tabMap(tabId);
  if (!m.has(rec.url) && m.size >= MAX_PER_TAB) {
    // 丢最旧
    let oldestKey = null;
    let oldestAt = Infinity;
    for (const [k, v] of m) {
      if (v.at < oldestAt) {
        oldestAt = v.at;
        oldestKey = k;
      }
    }
    if (oldestKey != null) m.delete(oldestKey);
  }
  m.set(rec.url, rec);
  scheduleFlush();
}

function guessType(url) {
  let path = url;
  try {
    path = new URL(url).pathname; // 天然排除 ?query 和 #hash
  } catch (_) {
    return "other";
  }
  const m = /\.([a-z0-9]+)$/i.exec(path);
  if (!m) return "other";
  return EXT_TYPE[m[1].toLowerCase()] || "other";
}

function updateBadge(tabId) {
  const n = byTab.get(tabId)?.size || 0;
  chrome.action.setBadgeText({ tabId, text: n ? String(n) : "" });
  chrome.action.setBadgeBackgroundColor({ tabId, color: "#2563eb" });
}

// ---------------------------------------------------------------- 嗅探管线

// —— 1. 预筛：URL 特征命中即建候选（无 extraInfoSpec，开销最低）
chrome.webRequest.onBeforeRequest.addListener(
  (details) => {
    if (details.tabId < 0) return;
    if (!MEDIA_RE.test(details.url) && !IMAGE_RE.test(details.url)) return;
    const type = guessType(details.url);
    if (!groupAllowed(type)) return;
    putRec(details.tabId, {
      url: details.url,
      tabId: details.tabId,
      type: guessType(details.url),
      mime: null,
      bytes: null,
      headers: null,
      at: Date.now(),
    });
    updateBadge(details.tabId);
  },
  { urls: ["<all_urls>"] }
);

// —— 2. 候选请求抓头（extraHeaders 才能看到 Cookie/Referer，Chrome 72+ 脱敏）
chrome.webRequest.onBeforeSendHeaders.addListener(
  (details) => {
    if (details.tabId < 0) return;
    const rec = byTab.get(details.tabId)?.get(details.url);
    if (!rec) return;
    const h = Object.fromEntries(
      (details.requestHeaders || []).map((x) => [x.name.toLowerCase(), x.value])
    );
    rec.headers = {};
    for (const k of ["referer", "user-agent", "cookie"]) {
      if (h[k]) rec.headers[k] = h[k];
    }
    if (Object.keys(rec.headers).length) {
      rememberHeaders(details.url, rec.headers);
      scheduleFlush();
    }
  },
  { urls: ["<all_urls>"] },
  ["requestHeaders", "extraHeaders"]
);

// —— 3. 完成：抓 MIME/大小；直链小文件丢弃（内存缓存命中的请求 webRequest 不可见，
//     大小标注「已观测值」，实际以主程序探测为准）
chrome.webRequest.onCompleted.addListener(
  (details) => {
    if (details.tabId < 0) return;
    const map = tabMap(details.tabId);
    let rec = map.get(details.url);
    const h = Object.fromEntries(
      (details.responseHeaders || []).map((x) => [x.name.toLowerCase(), x.value])
    );
    if (!rec) {
      // 扩展名没命中时按响应 MIME 兜底：真资源（视频/音频/压缩包/二进制…）
      // 即使 URL 无扩展名也收录；text/html、json、接口响应永远不进列表
      const mt = mimeToType(h["content-type"]);
      if (mt && groupAllowed(mt)) {
        rec = {
          url: details.url,
          tabId: details.tabId,
          type: mt,
          mime: h["content-type"] || null,
          bytes: null,
          headers: null,
          at: Date.now(),
        };
        map.set(details.url, rec);
      }
    }
    if (!rec) return;
    rec.mime = h["content-type"] || null;
    const len = h["content-length"] ? parseInt(h["content-length"], 10) : null;
    const exempt =
      rec.type === "hls" || rec.type === "segment" || rec.type === "dash";
    if (len != null && len < MIN_BYTES && !exempt) {
      map.delete(details.url);
      updateBadge(details.tabId);
      scheduleFlush();
      return;
    }
    rec.bytes = len;
    updateBadge(details.tabId);
    scheduleFlush();
  },
  { urls: ["<all_urls>"] },
  ["responseHeaders"]
);

// —— 4. 失败/页面关闭清理
chrome.webRequest.onErrorOccurred.addListener(
  (details) => {
    const map = byTab.get(details.tabId);
    if (map && map.delete(details.url)) {
      updateBadge(details.tabId);
      scheduleFlush();
    }
  },
  { urls: ["<all_urls>"] }
);

chrome.tabs.onRemoved.addListener((tabId) => {
  if (byTab.delete(tabId)) scheduleFlush();
});

// ---------------------------------------------------------------- 接管

/** 最近请求头快照：接管时按 URL 匹配（LRU + TTL）。 */
const recentHeaders = new Map();
function rememberHeaders(url, headers) {
  recentHeaders.set(url, { headers, at: Date.now() });
  if (recentHeaders.size > RECENT_HEADERS_MAX) {
    recentHeaders.delete(recentHeaders.keys().next().value);
  }
  // TTL 清理（懒扫）
  if (recentHeaders.size % 100 === 0) {
    const cutoff = Date.now() - RECENT_HEADERS_TTL;
    for (const [k, v] of recentHeaders) {
      if (v.at < cutoff) recentHeaders.delete(k);
    }
  }
}

// 非媒体请求也记 Referer/UA（无 extraHeaders，Cookie 脱敏——接管非媒体直链时降级为匿名下载）
chrome.webRequest.onBeforeSendHeaders.addListener(
  (details) => {
    if (recentHeaders.has(details.url)) return;
    const h = Object.fromEntries(
      (details.requestHeaders || []).map((x) => [x.name.toLowerCase(), x.value])
    );
    const headers = {};
    for (const k of ["referer", "user-agent"]) {
      if (h[k]) headers[k] = h[k];
    }
    if (Object.keys(headers).length) {
      rememberHeaders(details.url, headers);
    }
  },
  { urls: ["<all_urls>"] },
  ["requestHeaders"]
);

/** cancel + onChanged 复核（cancel 对已完成下载是静默 no-op——必须以
 *  onChanged 状态为准，DESIGN §7.3）。返回 "canceled" | "complete" | "timeout"。 */
function cancelWithVerify(id, timeoutMs = 3000) {
  return new Promise((resolve) => {
    let done = false;
    const listener = (delta) => {
      if (delta.id !== id) return;
      const st = delta.state && delta.state.current;
      if (st === "interrupted") {
        const err = delta.error && delta.error.current;
        finish(err === "USER_CANCELED" ? "canceled" : "interrupted");
      } else if (st === "complete") {
        finish("complete");
      }
    };
    function finish(outcome) {
      if (done) return;
      done = true;
      chrome.downloads.onChanged.removeListener(listener);
      clearTimeout(timer);
      resolve(outcome);
    }
    const timer = setTimeout(() => finish("timeout"), timeoutMs);
    chrome.downloads.onChanged.addListener(listener);
    chrome.downloads.cancel(id).catch(() => finish("timeout"));
  });
}

chrome.downloads.onCreated.addListener(async (item) => {
  if (!(await takeoverEnabled())) return;
  const url = item.finalUrl || item.url;
  if (!/^https?:/i.test(url)) return;
  try {
    const host = new URL(url).hostname;
    const ignored = await ignoredHosts();
    if (host && ignored.includes(host)) return; // 用户忽略过的站点不接管
  } catch (_) {}
  const nameHit = TAKEOVER_RE.test(url) || TAKEOVER_RE.test(item.filename || "");
  // fileSize 可能为 0（未知）：未知大小时放行类型命中者
  const sizeOk =
    item.fileSize == null ||
    item.fileSize === 0 ||
    item.fileSize >= MIN_TAKEOVER_BYTES;
  if (!nameHit || !sizeOk) return;

  try {
    await chrome.downloads.pause(item.id);
  } catch (_) {
    return; // 已完成/无法暂停：放行
  }

  // 请求头：优先嗅探缓存（可能含 Cookie），其次近期快照（匿名头）
  const sniffed = findByUrl(url);
  const headers =
    (sniffed && sniffed.headers && Object.keys(sniffed.headers).length
      ? sniffed.headers
      : null) ||
    recentHeaders.get(url)?.headers ||
    null;

  const resp = await pushTask(url, headers);
  if (resp.ok) {
    rememberTakeover(url);
    const outcome = await cancelWithVerify(item.id);
    if (outcome === "complete") {
      // 浏览器侧已下完：删掉记录避免误解（文件留在下载目录）
      try {
        await chrome.downloads.removeFile(item.id);
      } catch (_) {}
    }
    try {
      await chrome.downloads.erase({ id: item.id });
    } catch (_) {}
  } else {
    // 主程序不在/推送失败 → 放行浏览器原生下载
    try {
      await chrome.downloads.resume(item.id);
    } catch (_) {}
  }
});

/** 右键菜单等无 popup 场景的推送反馈：badge 闪 ✓ / ! 后恢复嗅探计数。 */
function flashBadge(tabId, ok) {
  if (tabId == null) return;
  const text = ok ? "✓" : "!";
  const color = ok ? "#16a34a" : "#dc2626";
  chrome.action.setBadgeText({ tabId, text });
  chrome.action.setBadgeBackgroundColor({ tabId, color });
  setTimeout(() => updateBadge(tabId), 1500);
}

function findByUrl(url) {
  for (const m of byTab.values()) {
    const rec = m.get(url);
    if (rec) return rec;
  }
  return null;
}

// master 清单变体解析（尽力而为：抓取失败静默跳过，推送主清单本身也能下）
async function fetchVariants(rec) {
  try {
    const init = { headers: rec.headers || undefined };
    const resp = await fetch(rec.url, init);
    const text = await resp.text();
    if (!/#EXT-X-STREAM-INF/i.test(text)) return; // 媒体清单，无变体
    const variants = [];
    const re = /#EXT-X-STREAM-INF:([^\r\n]*)\r?\n([^#\r\n]+)/g;
    let m;
    while ((m = re.exec(text))) {
      const attrs = m[1];
      const uri = m[2].trim();
      const bw = /BANDWIDTH=(\d+)/i.exec(attrs);
      const res = /RESOLUTION=(\d+x\d+)/i.exec(attrs);
      const name = /NAME="([^"]+)"/i.exec(attrs);
      variants.push({
        url: new URL(uri, rec.url).href,
        bandwidth: bw ? parseInt(bw[1], 10) : 0,
        resolution: res ? res[1] : null,
        name: name ? name[1] : null,
      });
    }
    if (variants.length > 1) {
      rec.variants = variants.sort((a, b) => b.bandwidth - a.bandwidth);
      scheduleFlush();
    }
  } catch (_) {
    // 跨域/网络失败：master 本身仍可推送（引擎自动选最高码率）
  }
}

// ---------------------------------------------------------------- 推送

// —— 划线标注（highlights.js 采集）：读同页标注列表，推送文档前由
//    ADL.appendHighlights 追加「## 划线摘录」区块。读不到（无标注）返回 null。
async function highlightsFor(pageUrl) {
  try {
    const key = String(pageUrl || "").split("#")[0];
    if (!/^https?:/i.test(key)) return null;
    const all = await chrome.storage.local.get("adl_highlights");
    const list = (all && all.adl_highlights && all.adl_highlights[key]) || null;
    return list && list.length ? list : null;
  } catch (_) {
    return null;
  }
}

// —— Cookie 透传兜底：嗅探/快照头没有 Cookie 时，从浏览器 cookie store 取该
//    URL 命中的全部 cookie 拼 Cookie 头（<all_urls> host 权限下 httpOnly 也可读），
//    登录态直链不再依赖「恰好被嗅探到」。Cookie 只进本机桌面端（本地优先，
//    headers 导出本就排除敏感头）；8KB 上限防 NM 帧膨胀；失败静默降级。
async function cookieHeaderFor(url) {
  try {
    if (!/^https?:/i.test(url) || !chrome.cookies || !chrome.cookies.getAll) return null;
    const jars = await chrome.cookies.getAll({ url });
    if (!jars || !jars.length) return null;
    const head = jars.map((c) => `${c.name}=${c.value}`).join("; ");
    return head ? head.slice(0, 8192) : null;
  } catch (_) {
    return null;
  }
}

async function pushTask(url, headers, filename, postprocess, dir) {
  // Cookie 兜底：显式头（嗅探/适配器）没有 Cookie 时从浏览器 cookie store 补
  let head = headers && Object.keys(headers).length ? { ...headers } : null;
  if (!head || !head.Cookie) {
    const ck = await cookieHeaderFor(url);
    if (ck) head = { ...(head || {}), Cookie: ck };
  }
  const add = await nmCall(
    "task.add",
    {
      url,
      headers: head && Object.keys(head).length ? head : undefined,
      filename: filename || undefined,
      postprocess: postprocess && postprocess.length ? postprocess : undefined,
      dir: dir || undefined,
    },
    5000
  );
  if (!add.ok) {
    return { ok: false, error: (add.error && add.error.code) || "unknown" };
  }
  const taskId = add.result && add.result.id;
  // 即建即下：推送成功后立刻请求宿主开始下载（DESIGN §5.4 即建即下）
  const start = await nmCall("task.start", { id: taskId }, 5000);
  return { ok: true, taskId, started: !!start.ok };
}

async function pushToApp(rec) {
  return pushTask(rec.url, rec.headers, rec.filename, rec.postprocess, rec.dir);
}

// ---------------------------------------------------------------- 站点适配层

// —— 平台开关（storage.local.adaptersCfg；与 sniffCfg 同模式）
const ADAPTER_DEFAULTS = {
  douyin: true,
  xhs: true,
  x: true,
  "wechat-article": true,
  channels: true,
  articles: true, // 文章适配器（12 站点：排版 HTML/Markdown/文章图片）
};
let ADAPTERS = { ...ADAPTER_DEFAULTS };
async function loadAdaptersCfg() {
  try {
    const { adaptersCfg } = await chrome.storage.local.get("adaptersCfg");
    if (adaptersCfg) ADAPTERS = { ...ADAPTER_DEFAULTS, ...adaptersCfg };
  } catch (_) {}
}
loadAdaptersCfg();
chrome.storage.onChanged.addListener((changes, area) => {
  if (area === "local" && changes.adaptersCfg) loadAdaptersCfg();
});
const adapterEnabled = (id) => ADAPTERS[id] !== false;
// 适配器 → hook 缓冲 key（xhs/wechat-article/articles 纯 DOM/SSR，无 hook）
const ADAPTER_HOOK_KEYS = {
  douyin: ["douyin"],
  x: ["x"],
  channels: ["channels"],
  xhs: ["xhs"],
  "wechat-article": [],
  articles: [],
};

// —— L1 捕获缓冲：tabId -> Map(key -> [{url, at, data}])，每 key 环形上限。
//     常驻内存不落盘：SW 被回收即丢失，页面下一次请求会重新填充（可接受）。
const hookBuf = new Map();
const HOOK_KEEP = { douyin: 20, x: 30, channels: 50, xhs: 30 };

function hookCapture(tabId, key, url, data) {
  if (!key || data == null) return;
  const keep = HOOK_KEEP[key];
  if (!keep) return;
  let m = hookBuf.get(tabId);
  if (!m) {
    m = new Map();
    hookBuf.set(tabId, m);
  }
  let arr = m.get(key);
  if (!arr) {
    arr = [];
    m.set(key, arr);
  }
  // 同一 URL 只留最新
  const idx = arr.findIndex((x) => x.url === url);
  if (idx >= 0) arr.splice(idx, 1);
  arr.push({ url, at: Date.now(), data });
  if (arr.length > keep) arr.splice(0, arr.length - keep);
}

function sliceCaptures(tabId, adapterId) {
  const keys = ADAPTER_HOOK_KEYS[adapterId] || [];
  const m = hookBuf.get(tabId);
  const out = {};
  for (const k of keys) {
    out[k] = ((m && m.get(k)) || []).map((x) => ({ url: x.url, data: x.data }));
  }
  return out;
}

// —— probe：路由到页面内适配器提取，结果缓存（push 直接复用，避免重复提取）。
//     一页可命中多个适配器（微信文章页 = wechat-article 图片 + articles 文章本体），
//     逐个提取后按 URL 合并去重。
const lastProbe = new Map(); // tabId -> { adapterIds, resources, at }

async function adapterProbe(tabId) {
  let tab = null;
  try {
    tab = await chrome.tabs.get(tabId);
  } catch (_) {
    return { ok: false, error: "no_tab" };
  }
  const matched = ADL.matchAdapters(tab.url || "");
  if (!matched.length) return { ok: false, error: "no_adapter" };
  const enabled = matched.filter((ad) => adapterEnabled(ad.id));
  if (!enabled.length) return { ok: false, error: "adapter_disabled" };
  const adapterIds = enabled.map((ad) => ad.id);
  // 捕获切片 = 各适配器 hook key 的并集
  const captured = {};
  for (const ad of enabled) {
    Object.assign(captured, sliceCaptures(tabId, ad.id));
  }
  let resp = null;
  try {
    resp = await chrome.tabs.sendMessage(tabId, {
      type: "adapter-extract",
      adapterIds,
      captured,
    });
  } catch (e) {
    return { ok: false, error: "no_content_script:" + String(e && e.message) };
  }
  if (resp && resp.ok) {
    // 按 URL 合并去重（保序：先到者胜——wechat-article 的 mmbiz 原图规则优先）
    const seen = new Set();
    const resources = [];
    for (const r of resp.resources || []) {
      if (seen.has(r.url)) continue;
      seen.add(r.url);
      resources.push(r);
    }
    resp.resources = resources;
    lastProbe.set(tabId, { adapterIds, resources, at: Date.now(), sourceUrl: tab.url });
  }
  return resp ? { ...resp, adapterId: adapterIds.join("+"), adapterIds } : { ok: false, error: "extract_failed" };
}

async function adapterPush(tabId, urls) {
  let entry = lastProbe.get(tabId);
  if (!entry) {
    const r = await adapterProbe(tabId);
    if (r.ok) entry = lastProbe.get(tabId);
  }
  if (!entry) return { ok: false, error: "probe_failed" };
  const list = entry.resources.filter((r) => {
    if (urls && urls.length) return urls.includes(r.url); // 显式勾选（含可选变体）
    return !r.optional; // 一键推送：跳过可选画质变体
  });
  const docs = list.filter((r) => r.kind === "document" && r.meta && r.meta.document);
  const imgs = list.filter((r) => r.kind !== "document");

  // —— 文章合集模式（docs/MEDIA-ADAPTERS.md §11.4）：文档+图片同批推送时
  //     统一落 <default_dir>/<标题>/，文档内图片引用重写为相对文件名
  //     （离线完整阅读）。拿不到 default_dir（旧版主程序）或只推文档时
  //     降级为普通模式（默认目录 + 外链引用）。
  let collection = null;
  if (docs.length && imgs.length) {
    try {
      const s = await nmCall("settings.get", { key: "default_dir" }, 5000);
      const base = s && s.ok && s.result && s.result.value;
      if (base && String(base).trim()) {
        const title = String(docs[0].filename || "article").replace(/\.(html|md)$/i, "");
        const name = ADL.cleanName(title, 80) || "article";
        const sep = String(base).includes("\\") ? "\\" : "/";
        collection = { dir: String(base).replace(/[\\/]+$/, "") + sep + name, name };
      }
    } catch (_) {}
  }
  if (collection) {
    const relMap = {};
    for (const r of imgs) relMap[r.url.split("#")[0].split("?")[0]] = r.filename;
    for (const d of docs) {
      d.meta.document.content = ADL.rewriteDocRefs(d.meta.document.content, relMap);
    }
  }
  // 划线标注随文章入库（品类核心体验）：追加「## 划线摘录」到所有文档产物
  if (docs.length) {
    const hl = await highlightsFor(entry.sourceUrl);
    if (hl) {
      for (const d of docs) {
        d.meta.document.content = ADL.appendHighlights(d.meta.document.content, hl);
      }
    }
  }

  let pushed = 0;
  let docTaskId = null;
  // 图片先推（即建即下）；文档瞬时完成殿后——用户打开文章时图片基本就绪
  for (const r of [...imgs, ...docs]) {
    if (r.kind === "document") {
      const doc = r.meta.document;
      // 文章导出（排版 HTML / Markdown）：内容生成型 → task.add_content 落盘，
      // 引擎侧直接 Done，无下载阶段
      const resp = await nmCall(
        "task.add_content",
        {
          filename: r.filename,
          mime: doc.mime,
          content: doc.content,
          dir: collection ? collection.dir : undefined,
          source_url: /^https?:/i.test(entry.sourceUrl || "") ? entry.sourceUrl : undefined,
        },
        20000 // 大文章 HTML 数 MB：帧传输 + 落盘，超时放宽
      );
      if (resp.ok) {
        pushed += 1;
        docTaskId = resp.result && resp.result.id;
      }
      continue;
    }
    // 视频号等平台的后处理（解密）随任务下发，引擎完成下载后执行
    const dec = r.meta && r.meta.decrypt;
    const resp = await pushToApp({
      url: r.url,
      headers: r.headers,
      filename: r.filename,
      dir: collection ? collection.dir : undefined,
      postprocess:
        dec && dec.seed ? [{ algo: dec.algo || "isaac64-xor", seed: dec.seed }] : null,
    });
    if (resp.ok || resp.error === "task_exists") pushed += 1;
  }
  flashBadge(tabId, pushed > 0);
  return {
    ok: pushed > 0,
    pushed,
    total: list.length,
    dir: collection ? collection.name : undefined,
    docTaskId: docTaskId || undefined,
  };
}

// —— 视频悬浮下载（video-overlays.js 单击入口）：智能解析当前视频来源并推送。
//     决策逻辑在 core.js pickVideoSource（纯函数，node 可测）；这里负责取
//     probe 缓存/嗅探快照、按计划分派到 adapterPush（保解密/文件名规则）或
//     pushTask（即建即下 + Cookie 兜底）。
async function videoDownload(tabId, msg) {
  if (tabId == null) return { ok: false, error: "no_tab" };
  // 站点适配器命中且启用 → 取该页资源（复用 lastProbe 缓存，无缓存现 probe）
  let adapterResources = [];
  try {
    const tab = await chrome.tabs.get(tabId);
    const matched = (ADL.matchAdapters(tab.url || "") || []).filter((ad) => adapterEnabled(ad.id));
    if (matched.length) {
      const r = await adapterProbe(tabId);
      if (r && r.ok && Array.isArray(r.resources)) adapterResources = r.resources;
    }
  } catch (_) {}
  const recs = [...(byTab.get(tabId) || new Map()).values()];
  const plan = ADL.pickVideoSource({
    srcUrl: msg.srcUrl,
    pageUrl: msg.pageUrl,
    recs,
    adapterResources,
    duration: msg.duration,
    ytdlpHosts: YTDLP_HOSTS,
  });
  let resp = null;
  const norm = (r, via) => {
    if (r && r.error === "task_exists") r = { ...r, ok: true, via, exists: true }; // 重复推送按成功反馈
    return r && r.ok ? { ...r, via } : r;
  };
  if (plan.via === "adapter") {
    resp = await adapterPush(tabId, plan.urls);
    resp = norm(resp, "adapter");
  } else if (plan.via === "direct" || plan.via === "sniff") {
    const rec = plan.via === "sniff" ? findByUrl(plan.url) : null;
    resp = await pushTask(plan.url, rec && rec.headers, undefined, rec && rec.postprocess);
    resp = norm(resp, plan.via);
  } else if (plan.via === "ytdlp") {
    resp = await pushTask(plan.url);
    resp = norm(resp, "ytdlp");
  }
  if (!resp) return { ok: false, error: "no_source" };
  flashBadge(tabId, resp.ok || resp.error === "task_exists");
  return resp;
}

// —— 分享链接解析（粘贴口令/短链）：抖音（移动端 UA）+ 小红书（带 Cookie 跟随）
async function adapterResolve(text) {
  const link = ADL.extractLink(text);
  if (!link) return { ok: false, error: "no_link" };
  if (/douyin\.com|iesdouyin\.com/i.test(link)) {
    try {
      const res = await fetch(link, {
        headers: { "User-Agent": ADL.DOUYIN_UA },
        redirect: "follow",
      });
      const html = await res.text();
      const awemes = ADL.douyinShareHtmlAwemes(html);
      if (!awemes.length) return { ok: false, error: "no_item" };
      const resources = awemes.flatMap((a) => ADL.awemeToResources(a));
      return { ok: resources.length > 0, adapterId: "douyin", resources };
    } catch (e) {
      return { ok: false, error: String(e && e.message) };
    }
  }
  if (/xiaohongshu\.com|xhslink\.com/i.test(link)) {
    try {
      const res = await fetch(link, {
        redirect: "follow",
        credentials: "include", // 带登录 Cookie（高清/部分笔记必需）
        headers: { "User-Agent": navigator.userAgent },
      });
      const html = await res.text();
      const resources = ADL.xhsShareHtmlResources(html);
      return { ok: resources.length > 0, adapterId: "xhs", resources };
    } catch (e) {
      return { ok: false, error: String(e && e.message) };
    }
  }
  // X (Twitter)：syndication 端点匿名解析（无需登录；token 为推文 id 确定性算式，
  // 见 core.js。走浏览器自身网络栈，用户能打开 X 就能解析）。
  const xId = ADL.xStatusId(link);
  if (xId) {
    try {
      const res = await fetch(
        `https://cdn.syndication.twimg.com/tweet-result?id=${xId}&token=${ADL.xSyndicationToken(xId)}&lang=zh-cn`,
        { redirect: "follow" }
      );
      if (!res.ok) return { ok: false, error: `syndication_http_${res.status}` };
      const d = await res.json();
      const resources = ADL.xSyndicationResources(d, xId);
      return { ok: resources.length > 0, adapterId: "x", resources };
    } catch (e) {
      return { ok: false, error: String(e && e.message) };
    }
  }
  return { ok: false, error: "unsupported_site" };
}

chrome.tabs.onRemoved.addListener((tabId) => {
  hookBuf.delete(tabId);
  lastProbe.delete(tabId);
});

// 真实导航（hard navigation / reload）清空该 tab 的适配层缓冲（docs/MEDIA-ADAPTERS.md
// §2 生命周期契约）。SPA 内部路由（pushState）只触发 changeInfo.url、不改 status，
// 因此不会误清；status 变 loading 仅发生在主框架真实加载时。
chrome.tabs.onUpdated.addListener((tabId, changeInfo) => {
  if (changeInfo.status === "loading") {
    hookBuf.delete(tabId);
    lastProbe.delete(tabId);
  }
});

// ---------------------------------------------------------------- 本地 zip 落盘
// ⑦下载所有图片（code-box 本地 zip 行为对齐）：content 侧收图并定名
// （actions.js collectImageJobs），这里抓原始字节（host_permissions <all_urls>
// 免 CORS）→ lib/zip.js STORE 打包 → data: URL 经 chrome.downloads 落盘
// （SW 无 Blob.createObjectURL / FileReader，手写分块 base64；code-box
// download.ts:29-41 同为 data URL 走 downloads API）。data: URL 下载不会触发
// 自接管（onCreated 的 /^https?:/ 预筛直接放行）。

function bytesToBase64(bytes) {
  let bin = "";
  const CHUNK = 0x8000;
  for (let i = 0; i < bytes.length; i += CHUNK) {
    bin += String.fromCharCode.apply(null, bytes.subarray(i, i + CHUNK));
  }
  return btoa(bin);
}

async function zipDownload(msg) {
  const jobs = (Array.isArray(msg.jobs) ? msg.jobs : []).filter(
    (j) => j && typeof j.url === "string" && typeof j.name === "string"
  );
  if (!jobs.length || typeof ADLZip === "undefined") {
    return { ok: false, error: "no_jobs" };
  }
  const files = [];
  const usedNames = new Set();
  for (const job of jobs) {
    // zip 重名条目会互相覆盖（jszip 语义）：加 (n) 序号去重保住每张图
    let name = job.name;
    if (usedNames.has(name)) {
      const dot = name.lastIndexOf(".");
      const stem = dot > 0 ? name.slice(0, dot) : name;
      const ext = dot > 0 ? name.slice(dot) : "";
      let n = 2;
      while (usedNames.has(`${stem}(${n})${ext}`)) n += 1;
      name = `${stem}(${n})${ext}`;
    }
    try {
      // 头优先级：嗅探缓存可能带 Referer/Cookie（防盗链站）；单张失败
      // console.error 后跳过，序号位保留（code-box weixin.tsx:222-224 同款）
      const rec = findByUrl(job.url);
      const init = rec && rec.headers ? { headers: rec.headers } : undefined;
      const resp = await fetch(job.url, init);
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      files.push({ name, data: new Uint8Array(await resp.arrayBuffer()) });
      usedNames.add(name);
    } catch (e) {
      console.error("[adl] zip 图片抓取失败：", job.url, e);
    }
  }
  if (!files.length) return { ok: false, error: "fetch_failed" };
  const filename =
    (ADL.cleanName(String(msg.filename || "").replace(/\.zip$/i, ""), 100) || "images") + ".zip";
  try {
    const bytes = await ADLZip.build(files);
    await chrome.downloads.download({
      url: "data:application/zip;base64," + bytesToBase64(bytes),
      filename,
      conflictAction: "uniquify",
    });
  } catch (e) {
    return { ok: false, error: String((e && e.message) || e) };
  }
  return { ok: true, saved: files.length, total: jobs.length, filename };
}

// ---------------------------------------------------------------- 消息路由

// —— 右键菜单（IDM 式）：链接 / 媒体 / 图片 一键推送
chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.removeAll(() => {
    chrome.contextMenus.create({
      id: "adl-link",
      title: "用 Anything Downloader 下载链接",
      contexts: ["link"],
    });
    chrome.contextMenus.create({
      id: "adl-media",
      title: "用 Anything Downloader 下载此媒体",
      contexts: ["video", "audio"],
    });
    chrome.contextMenus.create({
      id: "adl-image",
      title: "用 Anything Downloader 下载此图片",
      contexts: ["image"],
    });
    // 站点适配：仅适配器命中的页面显示（微信文章/小红书/抖音/X + 文章站点）
    chrome.contextMenus.create({
      id: "adl-site-page",
      title: "下载本页媒体（站点适配）",
      contexts: ["page"],
      documentUrlPatterns: [
        "*://mp.weixin.qq.com/*",
        "*://*.xiaohongshu.com/*",
        "*://*.douyin.com/*",
        "*://*.x.com/*",
        "*://*.twitter.com/*",
        "*://channels.weixin.qq.com/*",
        "*://*.blog.csdn.net/*",
        "*://zhuanlan.zhihu.com/*",
        "*://www.jianshu.com/*",
        "*://juejin.cn/*",
        "*://*.cnblogs.com/*",
        "*://blog.51cto.com/*",
        "*://www.jb51.net/*",
        "*://www.php.cn/*",
        "*://segmentfault.com/*",
        "*://my.oschina.net/*",
        "*://*.medium.com/*",
      ],
    });
  });
});

chrome.contextMenus.onClicked.addListener(async (info, tab) => {
  if (info.menuItemId === "adl-site-page") {
    if (!tab || tab.id < 0) return;
    await adapterProbe(tab.id);
    await adapterPush(tab.id);
    return;
  }
  const url = info.linkUrl || info.srcUrl;
  if (!url || !/^https?:/i.test(url)) return;
  // 头优先级：嗅探缓存（可能含 Cookie）> 近期快照
  let headers = null;
  if (tab != null) {
    const rec = byTab.get(tab.id)?.get(url);
    if (rec && rec.headers && Object.keys(rec.headers).length) headers = rec.headers;
  }
  if (!headers) headers = recentHeaders.get(url)?.headers || null;
  const resp = await pushTask(url, headers);
  flashBadge(tab?.id, resp.ok || (resp.error && resp.error.code === "task_exists"));
});

chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  if (msg.type === "ping") {
    // 存活探测：popup 打开时一次，不轮询（DESIGN §7.5）
    nmCall("ping", undefined, 3000)
      .then((resp) => sendResponse({ ok: !!resp.ok }))
      .catch(() => sendResponse({ ok: false }));
    return true; // 异步响应
  }
  if (msg.type === "list") {
    const items = [...(byTab.get(msg.tabId) || new Map()).values()];
    sendResponse({ items });
    return;
  }
  if (msg.type === "push") {
    pushToApp(msg.rec).then(sendResponse);
    return true; // 异步响应
  }
  if (msg.type === "hook-capture") {
    const tabId = _sender.tab && _sender.tab.id;
    if (tabId != null && tabId >= 0) hookCapture(tabId, msg.key, msg.url, msg.data);
    return;
  }
  if (msg.type === "adapter-probe") {
    // tabId 缺省 = 调用方所在页（悬浮按钮从页面内发起）
    const tabId = msg.tabId != null ? msg.tabId : _sender.tab && _sender.tab.id;
    if (tabId == null) {
      sendResponse({ ok: false, error: "no_tab" });
      return;
    }
    adapterProbe(tabId).then(sendResponse);
    return true;
  }
  if (msg.type === "adapter-push") {
    const tabId = msg.tabId != null ? msg.tabId : _sender.tab && _sender.tab.id;
    if (tabId == null) {
      sendResponse({ ok: false, error: "no_tab" });
      return;
    }
    adapterPush(tabId, msg.urls).then(sendResponse);
    return true;
  }
  if (msg.type === "video-download") {
    // 视频悬浮按钮（video-overlays.js）：tabId 固定为发起页
    videoDownload(_sender.tab && _sender.tab.id, msg).then(sendResponse);
    return true;
  }
  if (msg.type === "zip-download") {
    // ⑦下载所有图片：content 收集的图片任务 → 本地 zip 落盘（不进应用管线）
    zipDownload(msg).then(sendResponse);
    return true;
  }
  if (msg.type === "adapter-resolve") {
    adapterResolve(msg.link).then(sendResponse);
    return true;
  }
  if (msg.type === "open-sync") {
    // ①文章同步入口（code-box background/index.ts:143-149 同款）：带来源 tabId
    //   打开同步页，sync.js boot 的 ?tabId= 分支自动锁定并提取该文章。
    //   注意路径映射：本仓库同步页在扩展根目录 sync.html（manifest options_ui
    //   同款解析），非 code-box 的 tabs/sync.html 子目录形态
    const syncTabId = _sender.tab && _sender.tab.id;
    const query = syncTabId != null ? `?tabId=${syncTabId}` : "";
    chrome.tabs.create({ url: chrome.runtime.getURL("sync.html") + query });
    sendResponse({ ok: true });
    return;
  }
  if (msg.type === "adapters-cfg") {
    sendResponse({ ok: true, cfg: ADAPTERS });
    return;
  }
  if (msg.type === "scan-page") {
    // 主动嗅探：扫描页面 DOM 里的 a[href] / img / media 元素（被动嗅探盲区）
    (async () => {
      try {
        const [{ result }] = await chrome.scripting.executeScript({
          target: { tabId: msg.tabId },
          func: () => {
            const abs = (u) => {
              try {
                return new URL(u, location.href).href;
              } catch {
                return null;
              }
            };
            const links = [...document.querySelectorAll("a[href]")]
              .map((a) => abs(a.getAttribute("href")))
              .filter((u) => u && /^https?:/.test(u));
            const imgs = [...document.images]
              .map((i) => abs(i.src))
              .filter((u) => u && /^https?:/.test(u));
            const media = [
              ...document.querySelectorAll("video, audio, source"),
            ]
              .map((m) => abs(m.src || m.currentSrc || ""))
              .filter((u) => u && /^https?:/.test(u));
            const blobVideos = [
              ...document.querySelectorAll("video"),
            ].filter((v) => (v.src || "").startsWith("blob:")).length;
            return { links, imgs, media, blobVideos };
          },
        });
        const tabId = msg.tabId;
        const map = tabMap(tabId);
        let added = 0;
        const addRec = (u, type) => {
          if (map.has(u)) return;
          map.set(u, {
            url: u,
            tabId,
            type,
            mime: null,
            bytes: null,
            headers: null,
            at: Date.now(),
          });
          added += 1;
        };
        for (const u of result.links) {
          const type = guessType(u);
          if (type === "other") continue; // 纯页面跳转链接不是资源
          if (!groupAllowed(type)) continue;
          addRec(u, type);
        }
        for (const u of result.imgs) {
          if (groupAllowed("image")) addRec(u, "image");
        }
        for (const u of result.media) {
          const type = guessType(u) === "other" ? "video" : guessType(u);
          if (groupAllowed(type)) addRec(u, type);
        }
        updateBadge(tabId);
        // 立即落盘并随响应返回列表：popup 不再二次读 storage（防 500ms 防抖竞态）
        await flushSniff();
        sendResponse({
          ok: true,
          added,
          total: map.size,
          items: [...map.values()],
          blobVideos: result.blobVideos,
        });
      } catch (e) {
        sendResponse({ ok: false, error: String(e) });
      }
    })();
    return true;
  }
  if (msg.type === "scan-elements") {
    (async () => {
      try {
        const [{ result }] = await chrome.scripting.executeScript({
          target: { tabId: msg.tabId },
          func: () => {
            const abs = (u) => {
              try {
                return new URL(u, location.href).href;
              } catch {
                return null;
              }
            };
            const imgs = [...document.images]
              .map((i) => abs(i.src))
              .filter((u) => u && /^https?:/.test(u));
            const media = [
              ...document.querySelectorAll("video, audio, source"),
            ]
              .map((m) => abs(m.src || m.currentSrc || ""))
              .filter((u) => u && /^https?:/.test(u));
            const blobVideos = [
              ...document.querySelectorAll("video"),
            ].filter((v) => (v.src || "").startsWith("blob:")).length;
            return { imgs, media, blobVideos };
          },
        });
        const tabId = msg.tabId;
        const map = tabMap(tabId);
        for (const u of result.imgs) {
          if (!map.has(u)) {
            map.set(u, {
              url: u,
              tabId,
              type: "image",
              mime: null,
              bytes: null,
              headers: null,
              at: Date.now(),
            });
          }
        }
        for (const u of result.media) {
          if (!map.has(u)) {
            map.set(u, {
              url: u,
              tabId,
              type: guessType(u) === "other" ? "video" : guessType(u),
              mime: null,
              bytes: null,
              headers: null,
              at: Date.now(),
            });
          }
        }
        updateBadge(tabId);
        scheduleFlush();
        sendResponse({ ok: true, added: result.imgs.length + result.media.length, blobVideos: result.blobVideos });
      } catch (e) {
        sendResponse({ ok: false, error: String(e) });
      }
    })();
    return true;
  }
  if (msg.type === "nm") {
    // popup/float 控制转发：task.* / settings.get / task.add_content...
    // （超时可由调用方指定：大文章 add_content 单帧可达数 MB，5s 默认不够）
    nmCall(msg.method, msg.params, Math.min(msg.timeoutMs || 5000, 30000)).then((resp) =>
      sendResponse(resp)
    );
    return true;
  }
  if (msg.type === "takeover-data") {
    (async () => {
      const { recentTakeovers: takeovers = [], ignoredHosts: hosts = [] } =
        await chrome.storage.local.get(["recentTakeovers", "ignoredHosts"]);
      sendResponse({ takeovers, ignoredHosts: hosts });
    })();
    return true;
  }
  if (msg.type === "ignore-host") {
    (async () => {
      const { ignoredHosts: hosts = [] } = await chrome.storage.local.get("ignoredHosts");
      if (!hosts.includes(msg.host)) hosts.push(msg.host);
      await chrome.storage.local.set({ ignoredHosts: hosts });
      sendResponse({ ok: true });
    })();
    return true;
  }
  if (msg.type === "unignore-host") {
    (async () => {
      const { ignoredHosts: hosts = [] } = await chrome.storage.local.get("ignoredHosts");
      await chrome.storage.local.set({
        ignoredHosts: hosts.filter((h) => h !== msg.host),
      });
      sendResponse({ ok: true });
    })();
    return true;
  }
});

async function takeoverEnabled() {
  try {
    const { takeover } = await chrome.storage.local.get("takeover");
    return takeover === "on";
  } catch {
    return false;
  }
}

async function ignoredHosts() {
  try {
    const { ignoredHosts: list } = await chrome.storage.local.get("ignoredHosts");
    return Array.isArray(list) ? list : [];
  } catch {
    return [];
  }
}

async function rememberTakeover(url) {
  try {
    let host = "";
    try {
      host = new URL(url).hostname;
    } catch (_) {}
    const { recentTakeovers: list } = await chrome.storage.local.get("recentTakeovers");
    const arr = Array.isArray(list) ? list : [];
    arr.unshift({ url, host, at: Date.now() });
    await chrome.storage.local.set({
      recentTakeovers: arr.slice(0, 10), // LRU 10
    });
  } catch (_) {}
}
