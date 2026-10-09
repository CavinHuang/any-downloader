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

const NM_HOST = "anything_dl";
const MIN_BYTES = 1024 * 1024; // 直链媒体噪音阈值（HLS 清单/分片豁免）
const HLS_RE = /\.(m3u8)(?=[?#]|$)/i;
const MEDIA_RE =
  /\.(mp4|m3u8|mpd|ts|m4s|mkv|webm|flv|mp3|flac|m4a|aac|ogg|wav|ape|avi|mov|wmv|rmvb|zip|rar|7z|tar|gz|pdf|epub|apk|exe|msi|dmg|iso)(?=[?#]|$)/i;

// —— 接管规则（DESIGN §7.3）
const TAKEOVER_RE =
  /\.(zip|rar|7z|tar|gz|mp4|mkv|webm|mp3|flac|m4a|aac|ogg|wav|avi|mov|wmv|pdf|epub|apk|exe|msi|dmg|iso)(?=[?#]|$)/i;
const MIN_TAKEOVER_BYTES = 1024 * 1024;

const MAX_PER_TAB = 100; // 单页资源上限（超限丢最旧，storage.session 配额治理）
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
    ensurePort();
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
  if (/\.(mpd)(?=[?#]|$)/i.test(url)) return "dash";
  if (HLS_RE.test(url)) return "hls";
  if (/\.(ts|m4s)(?=[?#]|$)/i.test(url)) return "segment";
  if (/\.(mp4|mkv|webm|flv|avi|mov|wmv|rmvb)(?=[?#]|$)/i.test(url)) return "video";
  if (/\.(mp3|flac|m4a|aac|ogg|wav|ape)(?=[?#]|$)/i.test(url)) return "audio";
  if (/\.(zip|rar|7z|tar|gz)(?=[?#]|$)/i.test(url)) return "archive";
  if (/\.(exe|msi|apk|dmg|iso|img)(?=[?#]|$)/i.test(url)) return "binary";
  return "file";
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
    if (!MEDIA_RE.test(details.url)) return;
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
    if (!rec && MEDIA_RE.test(details.url)) {
      rec = {
        url: details.url,
        tabId: details.tabId,
        type: guessType(details.url),
        mime: null,
        bytes: null,
        headers: null,
        at: Date.now(),
      };
      map.set(details.url, rec);
    }
    if (!rec) return;
    const h = Object.fromEntries(
      (details.responseHeaders || []).map((x) => [x.name.toLowerCase(), x.value])
    );
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

async function pushTask(url, headers) {
  const add = await nmCall(
    "task.add",
    {
      url,
      headers: headers && Object.keys(headers).length ? headers : undefined,
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
  return pushTask(rec.url, rec.headers);
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
  });
});

chrome.contextMenus.onClicked.addListener(async (info, tab) => {
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
    nmCall("ping", undefined, 3000).then((resp) => {
      sendResponse({ ok: !!resp.ok });
    });
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
    // popup 控制转发：task.list / task.pause / task.resume / task.cancel / task.open...
    nmCall(msg.method, msg.params, 5000).then((resp) => sendResponse(resp));
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
