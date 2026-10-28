// popup：本页资源（过滤/多选批量推送）+ 主程序任务（实时进度/控制）
const ERROR_TEXT = {
  app_not_running: "主程序未运行——请先启动桌面端",
  timeout: "主程序响应超时",
  invalid_params: "参数被拒绝",
  app_not_registered: "浏览器连接未授权——请打开桌面端完成连接授权后重试",
};

const STATUS_TEXT = {
  queued: "排队中",
  probing: "探测中",
  downloading: "下载中",
  paused: "已暂停",
  waiting_retry: "等待重试",
  merging: "合并中",
  verifying: "校验中",
  decrypting: "解密中",
  done: "已完成",
  failed: "失败",
  canceled: "已取消",
};

const ACTIVE = ["probing", "downloading", "merging", "verifying", "decrypting"];

const TYPE_NAMES = { video: "视频", hls: "流媒体", audio: "音频", archive: "压缩包", binary: "软件", file: "文档", doc: "文档", image: "图片", segment: "分片", other: "文件", dash: "DASH · 暂不支持" };
const FILE_PATHS = {
  video: '<rect x="3" y="5" width="18" height="14" rx="2"/><path d="m10 9 5 3-5 3z"/>',
  audio: '<path d="M9 17V5l11-2v12M9 8l11-2"/><ellipse cx="6" cy="17" rx="3" ry="3"/><ellipse cx="17" cy="15" rx="3" ry="3"/>',
  image: '<rect x="3" y="3" width="18" height="18" rx="2"/><circle cx="8" cy="8" r="1.5"/><path d="m3 17 6-5 4 4 3-3 5 5"/>',
  archive: '<rect x="4" y="3" width="16" height="18" rx="2"/><path d="M11 3v3h2v3h-2v3m0 0h3v5h-3z"/>',
  file: '<path d="M14 3H5v18h14V8zM14 3v5h5M8 12h8M8 16h5"/>',
};

function resourceIcon(type) {
  const icon = document.createElement("span");
  icon.className = "resource-icon " + type;
  icon.setAttribute("aria-hidden", "true");
  const key = type === "hls" || type === "dash" || type === "segment" ? "video" : type === "binary" ? "archive" : type;
  icon.innerHTML = `<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round">${FILE_PATHS[key] || FILE_PATHS.file}</svg>`;
  return icon;
}

function taskType(task) {
  const name = displayName(task);
  if (task.kind === "hls" || /\.(mp4|mkv|webm|mov|ts)$/i.test(name)) return "video";
  if (/\.(mp3|wav|flac|m4a|ogg)$/i.test(name)) return "audio";
  if (/\.(jpe?g|png|webp|gif|svg)$/i.test(name)) return "image";
  if (/\.(zip|rar|7z|gz|exe|msi|apk|dmg)$/i.test(name)) return "archive";
  return "file";
}

function emptyMarkup(title, note = "") {
  return `<div class="empty"><svg width="86" height="54" viewBox="0 0 120 76" fill="none" stroke="currentColor" stroke-width="1.2" aria-hidden="true"><path d="m17 42 12-24h62l12 24v24H17z"/><path d="M17 42h28l6 8h18l6-8h28"/><path d="M41 33V7h35v26"/><path d="M48 15h21M48 21h16"/><path d="M51 59h18"/></svg><strong>${escapeHtml(title)}</strong>${note ? `<p>${escapeHtml(note)}</p>` : ""}</div>`;
}

function send(msg, timeoutMs = 5000) {
  // 兜底：任何情况下 sendMessage 都在超时内落定，绝不永久挂起
  return Promise.race([
    chrome.runtime.sendMessage(msg),
    new Promise((_, rej) =>
      setTimeout(() => rej(new Error("timeout")), timeoutMs)
    ),
  ]);
}

function statusTag(status) {
  const cls = ACTIVE.includes(status)
    ? "s-active"
    : status === "done"
      ? "s-done"
      : status === "failed"
        ? "s-failed"
        : "s-warn";
  return `<span class="tag ${cls}">${escapeHtml(String(STATUS_TEXT[status] || status))}</span>`;
}

function fmtBytes(n) {
  if (n == null) return "?";
  const u = ["B", "KB", "MB", "GB"];
  let i = 0;
  let v = n;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${v.toFixed(i ? 1 : 0)}${u[i]}`;
}

function fmtSpeed(bps) {
  return `${fmtBytes(bps)}/s`;
}

function displayNameOfUrl(url) {
  try {
    const seg = new URL(url).pathname.split("/").filter(Boolean).pop();
    return decodeURIComponent(seg || url);
  } catch {
    return url;
  }
}

function displayName(t) {
  if (t.filename) return t.filename;
  return displayNameOfUrl(t.url);
}

async function getActiveTab() {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  return tab;
}

// ---------------------------------------------------------------- 本页资源

const TYPE_FILTERS = [
  ["all", "全部"],
  ["video", "视频"],
  ["hls", "HLS"],
  ["audio", "音频"],
  ["archive", "压缩包"],
  ["doc", "文档"],
  ["image", "图片"],
  ["other", "其他"],
];

function resTypeGroup(type) {
  if (type === "video" || type === "hls" || type === "dash" || type === "segment")
    return type === "hls" || type === "dash" ? "hls" : "video";
  if (type === "audio") return "audio";
  if (type === "image") return "image";
  if (type === "archive" || type === "binary") return "archive";
  if (type === "file") return "doc";
  return "other";
}

let resItems = [];
let resFilter = "all";
const selected = new Set(); // 选中资源 url 集合

// 嗅探配置（与 background 共享 storage.local.sniffCfg）
const CFG_DEFAULTS = {
  types: { video: true, hls: true, audio: true, archive: true, binary: true, doc: true, image: false, other: true },
  minSizeMB: 1,
  videoOverlay: true, // 视频悬浮下载按钮（video-overlays.js）
  videoOverlayHosts: [], // 站点黑名单（预留，暂无编辑 UI）
};
let CFG = JSON.parse(JSON.stringify(CFG_DEFAULTS));
async function loadCfg() {
  try {
    const { sniffCfg } = await chrome.storage.local.get("sniffCfg");
    if (sniffCfg) {
      CFG = {
        types: { ...CFG_DEFAULTS.types, ...(sniffCfg.types || {}) },
        minSizeMB: sniffCfg.minSizeMB ?? 1,
        videoOverlay: sniffCfg.videoOverlay !== false,
        videoOverlayHosts: Array.isArray(sniffCfg.videoOverlayHosts) ? sniffCfg.videoOverlayHosts : [],
      };
    }
  } catch (_) {}
}
function groupOf(type) {
  if (type === "hls" || type === "dash" || type === "segment") return "video";
  if (type === "image") return "image";
  if (type === "binary") return "binary";
  if (type === "file") return "doc";
  return type; // video / audio / archive / other 直映射
}
function groupEnabled(type) {
  return CFG.types[groupOf(type)] !== false;
}
function minBytesCfg() {
  return Math.max(0, CFG.minSizeMB || 0) * 1024 * 1024;
}

function filteredRes() {
  if (!Array.isArray(resItems)) resItems = [];
  if (resFilter === "all") return resItems;
  return resItems.filter((x) => resTypeGroup(x.type) === resFilter);
}

function updateBatchBar() {
  const bar = document.getElementById("batchbar");
  const n = selected.size;
  if (n === 0) {
    bar.classList.add("hidden");
  } else {
    bar.classList.remove("hidden");
    document.getElementById("sel-n").textContent = String(n);
  }
  const selChip = document.getElementById("chip-select-all");
  if (selChip) {
    const allOn = filteredRes().filter(x => x.type !== "dash").every(x => selected.has(x.url));
    selChip.classList.toggle("on", n > 0);
    selChip.textContent = allOn ? "取消全选" : "全选";
  }
}

function renderFilters() {
  const el = document.getElementById("filters");
  if (!Array.isArray(resItems)) resItems = [];
  el.innerHTML = "";
  const tools = document.getElementById("resource-tools");
  tools.textContent = "";
  document.getElementById("resource-count").textContent = String(resItems.length).padStart(2, "0");
  const counts = { all: resItems.length };
  for (const item of resItems) {
    const g = resTypeGroup(item.type);
    counts[g] = (counts[g] || 0) + 1;
  }
  for (const [key, label] of TYPE_FILTERS) {
    if (key !== "all" && !counts[key]) continue;
    const b = document.createElement("button");
    b.className = "chip-f" + (resFilter === key ? " active" : "");
    b.append(label);
    if (counts[key]) {
      const count = document.createElement("span");
      count.className = "filter-count";
      count.textContent = String(counts[key]);
      b.append(count);
    }
    b.setAttribute("aria-pressed", String(resFilter === key));
    b.addEventListener("click", () => {
      resFilter = key;
      renderFilters();
      renderResList();
    });
    el.append(b);
  }
  const pushable = filteredRes().filter((x) => x.type !== "dash");
  if (pushable.length > 1) {
    const sel = document.createElement("button");
    sel.id = "chip-select-all";
    sel.className = "chip-f select";
    sel.textContent = pushable.every(x => selected.has(x.url)) ? "取消全选" : "全选";
    sel.addEventListener("click", () => {
      const allOn = pushable.every(x => selected.has(x.url));
      if (allOn) {
        for (const x of pushable) selected.delete(x.url);
      } else {
        for (const x of pushable) selected.add(x.url);
      }
      renderResList();
      updateBatchBar();
    });
    tools.append(sel);
    const pa = document.createElement("button");
    pa.className = "chip-f push-all";
    pa.textContent = `全部收下 · ${pushable.length}`;
    pa.addEventListener("click", async () => {
      pa.disabled = true;
      pa.textContent = "推送中…";
      let ok = 0;
      for (const item of pushable) {
        const r = await pushTask(item.url, item.headers);
        if (r.ok || r.error === "task_exists") ok += 1;
      }
      pa.textContent = `已推送 ${ok}/${pushable.length}`;
      setTimeout(() => {
        pa.disabled = false;
        renderFilters();
      }, 2000);
    });
    tools.append(pa);
  }
  updateBatchBar();
}

function renderResList() {
  const list = document.getElementById("list");
  if (!Array.isArray(resItems)) resItems = [];
  list.innerHTML = "";
  if (!resItems.length) {
    list.innerHTML = emptyMarkup("好东西，值得等一等。", "试试播放媒体，再点「重新发现」。");
    updateBatchBar();
    return;
  }
  const items = filteredRes().sort((a, b) => (b.bytes || 0) - (a.bytes || 0));
  if (!items.length) {
    list.innerHTML = emptyMarkup("这里暂时还没有发现。", "换一种类型，看看还有什么。");
    updateBatchBar();
    return;
  }
  for (const item of items) list.append(renderResItem(item));
  updateBatchBar();
}

function renderResItem(item) {
  const div = document.createElement("div");
  div.className = "item" + (selected.has(item.url) ? " selected" : "");

  if (item.type !== "dash") {
    const cb = document.createElement("input");
    cb.type = "checkbox";
    cb.checked = selected.has(item.url);
    cb.setAttribute("aria-label", "选择 " + displayNameOfUrl(item.url));
    cb.addEventListener("change", () => {
      if (cb.checked) selected.add(item.url);
      else selected.delete(item.url);
      div.classList.toggle("selected", cb.checked);
      updateBatchBar();
    });
    div.append(cb);
  }

  div.append(resourceIcon(item.type));
  const mid = document.createElement("div");
  mid.className = "mid";

  const name = document.createElement("div");
  name.className = "name";
  name.textContent = displayNameOfUrl(item.url);
  name.title = item.url;

  const url = document.createElement("div");
  url.className = "url";
  try { url.textContent = new URL(item.url).hostname.replace(/^www\./, ""); } catch { url.textContent = item.url; }
  url.title = item.url;

  const meta = document.createElement("div");
  meta.className = "meta";
  const tag = document.createElement("span");
  tag.className = "tag t-" + item.type;
  tag.textContent = TYPE_NAMES[item.type] || "文件";
  const size = document.createElement("span");
  size.className = "size";
  size.textContent = item.bytes != null ? fmtBytes(item.bytes) : "大小未知";
  meta.append(tag, size);
  mid.append(name, url, meta);

  const actions = document.createElement("div");
  actions.className = "actions";
  const btn = document.createElement("button");
  btn.className = "collect-button";
  if (item.type === "dash") {
    btn.disabled = true;
    btn.title = "DASH 暂不支持";
    btn.textContent = "–";
  } else {
    btn.textContent = "收下 ↗";
    btn.addEventListener("click", async () => {
      btn.disabled = true;
      btn.textContent = "…";
      const resp = await pushTask(item.url, item.headers);
      applyPushResult(div, btn, resp);
    });
  }
  const copy = document.createElement("button");
  copy.className = "copy";
  copy.textContent = "复制";
  copy.addEventListener("click", async () => {
    try {
      await navigator.clipboard.writeText(item.url);
      copy.textContent = "✓";
      setTimeout(() => (copy.textContent = "复制"), 1200);
    } catch (_) {}
  });
  actions.append(btn, copy);
  div.append(mid, actions);

  // HLS master：逐清晰度推送
  if (item.type === "hls" && item.variants && item.variants.length > 1) {
    const vrow = document.createElement("div");
    vrow.className = "variants";
    for (const v of item.variants) {
      const vb = document.createElement("button");
      vb.className = "copy";
      vb.textContent = v.resolution || `${Math.round(v.bandwidth / 1000)}k`;
      vb.title = v.url;
      vb.addEventListener("click", async () => {
        vb.disabled = true;
        const resp = await pushTask(v.url, item.headers);
        vb.textContent = resp.ok ? "✓" : "×";
        if (resp.ok) vb.classList.add("pushed");
      });
      vrow.append(vb);
    }
    mid.append(vrow);
  }

  return div;
}

function applyPushResult(div, btn, resp) {
  if (resp && resp.ok) {
    btn.textContent = resp.started ? "已开始" : "已创建";
    btn.className = "pushed";
    return true;
  }
  const err = document.createElement("div");
  err.className = "error";
  const code = resp && resp.error;
  if (code === "task_exists") {
    err.className = "warn-text";
    err.textContent = "该链接已有进行中的任务";
  } else {
    err.textContent = "失败: " + (ERROR_TEXT[code] || code || "未知");
  }
  (div.querySelector(".mid") || div).append(err);
  btn.disabled = false;
  btn.textContent = "重试";
  return false;
}

// ---------------------------------------------------------------- 批量推送

async function pushTask(url, headers) {
  try {
    return await send({
      type: "push",
      rec: { url, headers: headers && Object.keys(headers).length ? headers : null },
    });
  } catch (e) {
    return { ok: false, error: String(e && e.message) || "unknown" };
  }
}

async function pushSelected() {
  const items = resItems.filter((x) => selected.has(x.url) && x.type !== "dash");
  const btn = document.getElementById("sel-push");
  btn.disabled = true;
  btn.textContent = "推送中…";
  let ok = 0;
  let dup = 0;
  let fail = 0;
  for (const item of items) {
    const r = await pushTask(item.url, item.headers);
    if (r.ok) {
      ok += 1;
    } else if (r.error === "task_exists") {
      dup += 1;
      ok += 1;
    } else {
      fail += 1;
    }
  }
  selected.clear();
  btn.disabled = false;
  btn.textContent = "收下选中";
  updateBatchBar();
  renderResList();
  const feedback = document.getElementById("collection-result");
  const note = document.createElement("span");
  note.className = fail > 0 ? "result-skip" : "result-ok";
  note.style.fontSize = "11px";
  // E4：ok 已含重复（dup），直接报 ok 并注明，不再做 ok-fail+dup 的重复计数
  note.textContent =
    fail > 0
      ? `成功 ${ok}（含重复 ${dup}）· 失败 ${fail}`
      : `已推送 ${ok} 项`;
  feedback.replaceChildren(note);
  setTimeout(() => note.remove(), 3000);
}

// ---------------------------------------------------------------- 主程序任务

const POLL_MS = 1000;
const PING_MS = 5000;
let taskTimer = null;
let pingTimer = null;
let pollFailures = 0;
let offlineMode = false;
let view = "res"; // 显式声明（原先靠隐式全局）
const prevBytes = new Map();

// E7/E2：状态 pill 随每轮轮询更新（不再只开窗检测一次）
function setStatusPill(ok) {
  const el = document.getElementById("status");
  el.textContent = ok ? "已连接" : "未运行";
  el.classList.toggle("ok", ok);
  el.classList.toggle("err", !ok);
  el.title = ok ? "主程序连接正常" : "主程序未连接，点击查看指引";
  if (ok) showGuide(false);
}

function showGuide(open) {
  const guide = document.getElementById("offline-guide");
  guide.classList.toggle("hidden", !open);
  document.getElementById("status").setAttribute("aria-expanded", String(open));
  if (open) guide.scrollIntoView({ behavior: "smooth", block: "nearest" });
}

function computeSpeed(t) {
  const now = t.downloaded_bytes || 0;
  const prev = prevBytes.get(t.id);
  let bps = 0;
  if (prev && now >= prev.bytes) {
    const dt = (Date.now() - prev.t) / 1000;
    if (dt > 0.2) bps = (now - prev.bytes) / dt;
  }
  prevBytes.set(t.id, { bytes: now, t: Date.now() });
  return bps;
}

function renderTask(t, bps) {
  const div = document.createElement("div");
  div.className = "item task-item";
  const mid = document.createElement("div");
  mid.className = "mid";
  div.append(resourceIcon(taskType(t)), mid);
  const active = ACTIVE.includes(t.status);
  const name = document.createElement("div");
  name.className = "task-head";
  name.innerHTML = `${statusTag(t.status)} <span class="name">${escapeHtml(displayName(t))}</span>`;
  name.title = displayName(t);
  mid.append(name);

  if (t.total_bytes != null && t.total_bytes > 0) {
    const pct =
      t.status === "done"
        ? 100
        : Math.min(
            100,
            Math.round(((t.downloaded_bytes || 0) / t.total_bytes) * 100)
          );
    const bar = document.createElement("div");
    bar.className = "bar";
    bar.innerHTML = `<div class="fill ${t.status === "done" ? "done" : t.status === "failed" ? "failed" : ""}" style="width:${pct}%"></div>`;
    mid.append(bar);
    const row = document.createElement("div");
    row.className = "task-meta";
    const size = document.createElement("span");
    size.className = "size";
    size.textContent =
      t.kind === "hls"
        ? `${t.downloaded_bytes || 0}/${t.total_bytes} 分片`
        : `${fmtBytes(t.downloaded_bytes || 0)} / ${fmtBytes(t.total_bytes)}`;
    const speed = document.createElement("span");
    speed.className = "speed";
    speed.style.marginLeft = "auto";
    speed.textContent = active && bps > 0 ? fmtSpeed(bps) : "";
    row.append(size, speed);
    mid.append(row);
  }
  if (t.status === "failed" && t.error) {
    const err = document.createElement("div");
    err.className = "error";
    err.textContent = "错误: " + String(t.error.code ?? "");
    mid.append(err);
  }

  const actions = document.createElement("div");
  actions.className = "actions";
  const mk = (label, fn, primary) => {
    const b = document.createElement("button");
    if (primary) b.className = "primary";
    b.textContent = label;
    b.addEventListener("click", async () => {
      try {
        await fn();
      } catch (e) {
        const err = document.createElement("div");
        err.className = "error";
        err.textContent = String(e);
        div.append(err);
      }
      renderTasks();
    });
    actions.append(b);
  };
  if (active) {
    mk("暂停", () => send({ type: "nm", method: "task.pause", params: { id: t.id } }));
    mk("取消", () => send({ type: "nm", method: "task.cancel", params: { id: t.id } }));
  } else if (t.status !== "done") {
    mk(
      t.status === "failed" || t.status === "canceled" ? "重试" : "开始",
      async () => {
        if (t.status === "failed" || t.status === "canceled") {
          await send({ type: "nm", method: "task.retry", params: { id: t.id } });
        }
        await send({ type: "nm", method: "task.start", params: { id: t.id } });
      },
      true
    );
  } else {
    mk("打开", () => send({ type: "nm", method: "task.open", params: { id: t.id } }));
  }
  div.append(actions);
  return div;
}

async function renderTasks() {
  const stats = document.getElementById("tasks-stats");
  const el = document.getElementById("task-list");
  // E2：send 包 try/catch——主程序中途退出不再产生未捕获 rejection，
  // 而是进入离线态（连续失败 ≥2 次）
  let resp = null;
  try {
    resp = await send({
      type: "nm",
      method: "task.list",
      params: {},
    });
  } catch (_) {
    resp = null;
  }
  if (!resp || !resp.ok) {
    pollFailures += 1;
    // E2：单次失败可能是瞬时抖动，保持原列表不打扰用户；
    // 连续失败 ≥2 次（或已在离线态）才显示离线 UI 并改 5s ping 探活
    if (pollFailures >= 2 || offlineMode) {
      if (!offlineMode) enterOfflineMode();
      renderOfflineState(stats, el);
    }
    return;
  }
  pollFailures = 0;
  if (offlineMode) exitOffline();
  setStatusPill(true);

  const tasks = resp.result || [];
  const rank = (t) => (ACTIVE.includes(t.status) ? 0 : 1);
  const sorted = tasks.sort(
    (a, b) => rank(a) - rank(b) || b.updated_at - a.updated_at
  );
  const shown = sorted.slice(0, 30);
  const truncated = sorted.length > shown.length;

  // E3：每任务每轮只算一次速度存入 Map，总速求和与卡片渲染共读一份
  //（原先 renderTask 内二次调用时 prev 刚更新、dt≈0，速度恒为 0）
  const speedMap = new Map();
  const activeTasks = shown.filter((x) => ACTIVE.includes(x.status));
  let totalBps = 0;
  for (const t of activeTasks) {
    const bps = computeSpeed(t);
    speedMap.set(t.id, bps);
    totalBps += bps;
  }
  stats.textContent =
    activeTasks.length
      ? `${activeTasks.length} 个任务进行中 · 总速度 ${fmtSpeed(totalBps)}`
      : "无进行中的任务";
  if (truncated) stats.textContent += ` · 共 ${tasks.length} 条`;
  stats.style.color = activeTasks.length ? "var(--accent-ink)" : "var(--muted)";

  el.innerHTML = "";
  if (shown.length === 0) {
    el.innerHTML = emptyMarkup("暂时安静，随时出发。", "收下一份新发现，它会在这里抵达。");
    return;
  }
  for (const t of shown) el.append(renderTask(t, speedMap.get(t.id) || 0));
  // E10：30 条截断出口（不新建打开主程序的通道，克制）
  if (truncated) {
    const trunc = document.createElement("div");
    trunc.className = "list-trunc";
    trunc.textContent = `仅显示最近 ${shown.length} 条 · 全部任务请在主程序窗口查看`;
    el.append(trunc);
  }
}

// E2：离线态——连续失败 ≥2 次暂停 1s 任务轮询，改 5s ping 探活；成功即恢复 1s 轮询
function renderOfflineState(stats, el) {
  setStatusPill(false);
  stats.textContent = "";
  // 用 data-offline 专属标识去重：泛查 .empty 会把「主程序暂无任务」误当成离线空态，
  // 导致零任务时掉线永远换不上离线提示
  if (el.querySelector("[data-offline]")) return;
  el.innerHTML =
    '<div class="empty" data-offline="1">主程序未运行——启动后将自动恢复连接<br>' +
    '<button id="btn-guide" class="chip-f" style="margin-top:8px">查看连接指引</button></div>';
  document
    .getElementById("btn-guide")
    .addEventListener("click", () => showGuide(true));
}

function enterOfflineMode() {
  offlineMode = true;
  clearInterval(taskTimer);
  taskTimer = null;
  clearInterval(pingTimer);
  pingTimer = setInterval(pingProbe, PING_MS);
}

async function pingProbe() {
  try {
    const r = await send({ type: "ping" }, 4000);
    if (r && r.ok) {
      pollFailures = 0;
      exitOffline();
      renderTasks(); // 恢复后立即刷新，兑现「启动后自动恢复」
    }
  } catch (_) {}
}

function exitOffline() {
  offlineMode = false;
  clearInterval(pingTimer);
  pingTimer = null;
  setStatusPill(true);
  startTaskPoll();
}

function startTaskPoll() {
  clearInterval(taskTimer);
  taskTimer = setInterval(() => {
    if (view === "tasks" && !offlineMode) renderTasks();
  }, POLL_MS);
}

function escapeHtml(s) {
  return s.replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c])
  );
}

// ---------------------------------------------------------------- 接管记录

async function renderTakeoverInfo() {
  const el = document.getElementById("takeover-info");
  let data = null;
  try {
    data = await send({ type: "takeover-data" });
  } catch (_) {
    return; // 主程序不可达时静默退出（离线态由任务区统一呈现）
  }
  if (!data) return;
  el.textContent = "";
  const hosts = data.ignoredHosts || [];
  const parts = [];
  for (const h of hosts) {
    const chip = document.createElement("span");
    chip.className = "chip";
    chip.append(`${h} `);
    const x = document.createElement("button");
    x.textContent = "×";
    x.title = "恢复接管此站点";
    x.addEventListener("click", async () => {
      try {
        await send({ type: "unignore-host", host: h });
      } catch (_) {}
      renderTakeoverInfo();
    });
    chip.append(x);
    parts.push(chip);
  }
  if (parts.length) {
    const label = document.createElement("div");
    label.textContent = "已忽略站点（点击 × 恢复接管）：";
    label.style.cssText = "margin-top:6px;color:var(--muted)";
    el.append(label, ...parts);
  }
  if (data.takeovers && data.takeovers.length) {
    const last = data.takeovers[0];
    const row = document.createElement("div");
    row.style.cssText = "margin-top:6px;color:var(--muted)";
    row.innerHTML = `最近接管: <span style="user-select:text">${escapeHtml(displayNameOfUrl(last.url))}</span> `;
    const b = document.createElement("button");
    b.textContent = "忽略此站点";
    b.style.cssText = "font-size:11px;padding:1px 8px";
    b.addEventListener("click", async () => {
      try {
        await send({ type: "ignore-host", host: last.host });
      } catch (_) {}
      renderTakeoverInfo();
    });
    row.append(b);
    el.append(row);
  }
}

// ---------------------------------------------------------------- 视图切换与启动

function switchToTasks() {
  view = "tasks";
  document.getElementById("tab-res").classList.remove("active");
  document.getElementById("tab-tasks").classList.add("active");
  document.getElementById("tab-res").setAttribute("aria-pressed", "false");
  document.getElementById("tab-tasks").setAttribute("aria-pressed", "true");
  document.getElementById("res-wrap").classList.add("hidden");
  document.getElementById("tasks").classList.remove("hidden");
  document.getElementById("batchbar").classList.add("hidden");
  renderTasks();
  startTaskPoll();
}

function switchToRes() {
  view = "res";
  document.getElementById("tab-res").classList.add("active");
  document.getElementById("tab-tasks").classList.remove("active");
  document.getElementById("tab-res").setAttribute("aria-pressed", "true");
  document.getElementById("tab-tasks").setAttribute("aria-pressed", "false");
  document.getElementById("res-wrap").classList.remove("hidden");
  document.getElementById("tasks").classList.add("hidden");
  clearInterval(taskTimer);
}

function setSettingsOpen(open) {
  const settings = document.getElementById("settings");
  settings.classList.toggle("hidden", !open);
  const trigger = document.getElementById("btn-settings");
  trigger.classList.toggle("active", open);
  trigger.setAttribute("aria-expanded", String(open));
  for (const id of ["popup-main", "popup-bottom", "offline-guide"]) document.getElementById(id).inert = open;
  document.querySelector("header").inert = open;
  (open ? document.getElementById("btn-settings-close") : trigger).focus();
}

function init() {
  // —— 静态控件接线（同步，最先执行；此后数据加载错误不影响按钮可用）
  const settingsEl = document.getElementById("settings");
  const takeover = document.getElementById("takeover");
  chrome.storage.local.get("takeover").then(data => { takeover.checked = data.takeover === "on"; }).catch(() => {});
  takeover.addEventListener("change", async () => {
    takeover.disabled = true;
    try { await chrome.storage.local.set({ takeover: takeover.checked ? "on" : "off" }); }
    catch { takeover.checked = !takeover.checked; document.getElementById("paste-result").textContent = "偏好未能保存，请重试。"; }
    finally { takeover.disabled = false; }
  });
  document.getElementById("tab-res").addEventListener("click", switchToRes);
  document.getElementById("tab-tasks").addEventListener("click", switchToTasks);
  document.getElementById("btn-sync").addEventListener("click", async () => {
    try { await chrome.runtime.openOptionsPage(); }
    catch { document.getElementById("paste-result").textContent = "工作台未能打开，请在扩展管理中打开「选项」。"; }
  });
  document.getElementById("sel-cancel").addEventListener("click", () => {
    selected.clear();
    renderResList();
    updateBatchBar();
  });
  document.getElementById("sel-push").addEventListener("click", pushSelected);
  document.getElementById("btn-scan").addEventListener("click", async () => {
    const tab = await getActiveTab();
    if (!tab) return;
    const btn = document.getElementById("btn-scan");
    const note = document.getElementById("scan-result");
    btn.disabled = true;
    btn.textContent = "扫描中…";
    try {
      const r = await send({ type: "scan-page", tabId: tab.id });
      btn.disabled = false;
      btn.textContent = "重新发现 ↻";
      if (r && r.ok) {
        note.textContent = `新增 ${r.added} 项（共 ${r.total}）`;
        if (r.blobVideos > 0) {
          note.textContent += " · 页面含内嵌播放器（blob），请选择 HLS 资源";
        }
        if (Array.isArray(r.items)) {
          resItems = r.items;
          renderFilters();
          renderResList();
        }
      } else {
        note.textContent = "扫描失败: " + ((r && r.error) || "未知");
      }
    } catch (e) {
      btn.disabled = false;
      btn.textContent = "重新发现 ↻";
      note.textContent = "扫描失败: " + e;
    }
    setTimeout(() => (note.textContent = ""), 4000);
  });
  document.getElementById("btn-settings").addEventListener("click", () => {
    setSettingsOpen(settingsEl.classList.contains("hidden"));
  });
  document.getElementById("btn-settings-close").addEventListener("click", () => setSettingsOpen(false));
  settingsEl.addEventListener("keydown", event => {
    if (event.key === "Escape") { event.preventDefault(); setSettingsOpen(false); return; }
    if (event.key !== "Tab") return;
    const elements = [...settingsEl.querySelectorAll("button:not(:disabled), input:not(:disabled)")].filter(el => el.getClientRects().length);
    const first = elements[0], last = elements[elements.length - 1];
    if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
    else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
  });
  // E7：未运行 pill 可点击 → 展开用户向连接指引（替换开发者术语）
  document.getElementById("status").addEventListener("click", () => {
    const pill = document.getElementById("status");
    if (!pill.classList.contains("err")) return;
    const guide = document.getElementById("offline-guide");
    showGuide(guide.classList.contains("hidden"));
  });

  // —— 嗅探配置（storage.local.sniffCfg，与 background 即时共享）
  loadCfg().then(async () => {
    for (const box of settingsEl.querySelectorAll("input[data-type]")) {
      const g = box.dataset.type;
      box.checked = CFG.types[g] !== false;
      box.addEventListener("change", async () => {
        CFG.types[g] = box.checked;
        await chrome.storage.local.set({ sniffCfg: CFG });
      });
    }
    const minEl = document.getElementById("set-minsize");
    minEl.value = CFG.minSizeMB;
    minEl.addEventListener("change", async () => {
      CFG.minSizeMB = Math.max(0, Number(minEl.value) || 0);
      await chrome.storage.local.set({ sniffCfg: CFG });
    });

    // —— 视频悬浮下载按钮开关（video-overlays.js 读同一配置即时生效）
    const voEl = document.getElementById("set-video-overlay");
    if (voEl) {
      voEl.checked = CFG.videoOverlay;
      voEl.addEventListener("change", async () => {
        CFG.videoOverlay = voEl.checked;
        await chrome.storage.local.set({ sniffCfg: CFG });
      });
    }

    // —— 平台适配器开关（storage.local.adaptersCfg）
    try {
      const { adaptersCfg } = await chrome.storage.local.get("adaptersCfg");
      for (const box of settingsEl.querySelectorAll("input[data-adapter]")) {
        const id = box.dataset.adapter;
        box.checked = !(adaptersCfg && adaptersCfg[id] === false);
        box.addEventListener("change", async () => {
          const cur =
            (await chrome.storage.local.get("adaptersCfg")).adaptersCfg || {};
          cur[id] = box.checked;
          await chrome.storage.local.set({ adaptersCfg: cur });
        });
      }
    } catch (_) {}
  });

  // —— 粘贴分享口令：抖音/小红书链接 → 解析 → 直推主程序
  const pasteGo = document.getElementById("paste-go");
  const pasteInput = document.getElementById("paste-input");
  const pasteNote = document.getElementById("paste-result");
  if (pasteGo && pasteInput) {
    pasteGo.addEventListener("click", async () => {
      const text = pasteInput.value.trim();
      if (!text) return;
      pasteGo.disabled = true;
      pasteNote.textContent = "解析中…";
      try {
        const r = await send({ type: "adapter-resolve", link: text }, 15000);
        if (!r || !r.ok || !r.resources || !r.resources.length) {
          pasteNote.textContent =
            "解析失败：" + ((r && r.error) || "未识别到可下载内容");
        } else {
          // 与 background adapterPush 同契约（core.js 转换器注释）：一键推送只推
          // 非 optional —— 否则抖音分享解析会把同视频的多档画质副本重复推送
          const list = r.resources.filter((x) => !x.optional);
          let ok = 0;
          for (const res of list) {
            const pr = await send({
              type: "push",
              rec: { url: res.url, headers: res.headers, filename: res.filename },
            });
            if (pr && pr.ok) ok += 1;
          }
          pasteNote.textContent = `已推送 ${ok}/${list.length} 项到主程序`;
          pasteInput.value = "";
        }
      } catch (e) {
        pasteNote.textContent = "解析失败：" + e;
      }
      pasteGo.disabled = false;
      setTimeout(() => (pasteNote.textContent = ""), 6000);
    });
    pasteInput.addEventListener("keydown", (ev) => {
      if (ev.key === "Enter") pasteGo.click();
    });
    // E6：popup 打开即聚焦头牌操作——粘贴框，省一步点击
    pasteInput.focus();
  }
}

const SKELETON_HTML = '<div class="skel"></div>'.repeat(3);
let resLoaded = false;
let tasksLoaded = false;

async function start() {
  try {
    init();
  } catch (e) {
    console.error("popup init error:", e);
    const el = document.getElementById("status");
    el.textContent = "初始化出错";
    el.className = "pill err";
    const b = document.createElement("div");
    b.className = "error";
    b.style.cssText = "margin:0 14px;user-select:text";
    b.textContent = String((e && e.stack) || e);
    document.body.prepend(b);
  }

  // E9：首屏骨架——仅首次加载且列表为空，延迟 300ms 出现，数据到达即撤
  const listEl = document.getElementById("list");
  const taskListEl = document.getElementById("task-list");
  listEl.textContent = "";
  taskListEl.textContent = "";
  resLoaded = false;
  tasksLoaded = false;
  setTimeout(() => {
    if (!resLoaded) listEl.innerHTML = SKELETON_HTML;
  }, 300);
  setTimeout(() => {
    if (!tasksLoaded) taskListEl.innerHTML = SKELETON_HTML;
  }, 300);

  // 状态 pill 首检；此后随每轮轮询更新（E2）
  let appOnline = false;
  try {
    const ping = await send({ type: "ping" });
    appOnline = !!(ping && ping.ok);
  } catch (_) {}
  setStatusPill(appOnline);

  let tab = null;
  try {
    tab = await getActiveTab();
  } catch (_) {}
  if (tab) {
    try { document.getElementById("page-source").textContent = new URL(tab.url).hostname.replace(/^www\./, ""); }
    catch { document.getElementById("page-source").textContent = "当前页面"; }
    try {
      const resp = await send({ type: "list", tabId: tab.id });
      if (resp && resp.items) {
        resItems = resp.items.sort((a, b) => (b.bytes || 0) - (a.bytes || 0));
      }
    } catch (_) {}
    renderSiteCard(tab.id); // 站点适配卡片（独立容错，失败不影响嗅探列表）
  }
  resLoaded = true; // 数据到达（或确认无 tab 数据）即撤骨架
  renderFilters();
  renderResList();

  await renderTasks();
  tasksLoaded = true;
  try {
    await renderTakeoverInfo();
  } catch (_) {}
}

// ---------------------------------------------------------------- 站点适配卡片

const ADAPTER_LABELS = {
  douyin: "抖音",
  xhs: "小红书",
  x: "X (Twitter)",
  "wechat-article": "微信文章图片",
  channels: "视频号",
  articles: "文章",
};

async function renderSiteCard(tabId) {
  const card = document.getElementById("site-card");
  if (!card) return;
  let r = null;
  try {
    r = await send({ type: "adapter-probe", tabId });
  } catch (_) {
    return;
  }
  if (!r || !r.ok || !r.resources || !r.resources.length) return;

  // 计数排除可选画质变体（一键推送也不会推它们）
  const main = r.resources.filter((x) => !x.optional);
  const nv = main.filter((x) => x.kind === "video").length;
  const nd = main.filter((x) => x.kind === "document").length;
  const ni = main.length - nv - nd;
  const ids = r.adapterIds || (r.adapterId ? [r.adapterId] : []);
  document.getElementById("site-name").textContent =
    ids.map((id) => ADAPTER_LABELS[id] || id).join("+") || r.adapterId || "";
  document.getElementById("site-counts").textContent = [
    nv ? `视频 ${nv}` : "",
    ni ? `图片 ${ni}` : "",
    nd ? `文章 ${nd}` : "",
  ]
    .filter(Boolean)
    .join(" · ");
  const push = document.getElementById("site-push");
  const fresh = push.cloneNode(true); // 重开 popup 时重置监听器
  push.replaceWith(fresh);
  fresh.addEventListener("click", async () => {
    fresh.disabled = true;
    fresh.textContent = "推送中…";
    try {
      const pr = await send({ type: "adapter-push", tabId });
      fresh.textContent = pr && pr.ok ? `已保存 ${pr.pushed} 项${pr.dir ? ` → ${pr.dir}/` : ""}` : "失败（主程序未运行？）";
    } catch (e) {
      fresh.textContent = "失败";
    }
    setTimeout(() => {
      fresh.disabled = false;
      fresh.textContent = "收好这一页 ↗";
    }, 2500);
  });
  card.classList.remove("hidden");
}

start();
