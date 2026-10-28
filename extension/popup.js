// popup：本页资源（过滤/多选批量推送）+ 主程序任务（实时进度/控制）
const ERROR_TEXT = {
  app_not_running: "主程序未运行——请先启动桌面端",
  timeout: "主程序响应超时",
  invalid_params: "参数被拒绝",
  unsupported_site: "暂不支持此分享链接，请尝试资源直链",
  no_item: "未找到可下载内容",
  no_link: "请输入 HTTP 或 HTTPS 链接或支持的分享口令",
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
  return `<div class="empty"><svg width="38" height="38" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 3v12m-5-5 5 5 5-5M4 16v4h16v-4"/></svg><strong>${escapeHtml(title)}</strong>${note ? `<p>${escapeHtml(note)}</p>` : ""}</div>`;
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
const downloadedResources = new Map();
let batchBusy = false;
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
  document.getElementById("resource-count").textContent = String(resItems.length);
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
    pa.textContent = `全部下载 · ${pushable.length}`;
    pa.disabled = batchBusy;
    pa.addEventListener("click", () => downloadResources(pushable));
    tools.append(pa);
  }
  updateBatchBar();
}

function renderResList() {
  const list = document.getElementById("list");
  if (!Array.isArray(resItems)) resItems = [];
  list.innerHTML = "";
  if (!resItems.length) {
    list.innerHTML = emptyMarkup("没有检测到可下载资源", "播放视频或音频后，点击「重新扫描」。");
    updateBatchBar();
    return;
  }
  const items = filteredRes().sort((a, b) => (b.bytes || 0) - (a.bytes || 0));
  if (!items.length) {
    list.innerHTML = emptyMarkup("此类型下没有资源", "切换到「全部」查看其他资源。");
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
  btn.className = "download-button";
  if (item.type === "dash") {
    btn.disabled = true;
    btn.title = "DASH 暂不支持";
    btn.textContent = "–";
  } else {
    const previous = downloadedResources.get(item.url);
    btn.textContent = previous ? previous.error === "task_exists" ? "已存在" : previous.started ? "已开始" : "已添加" : "下载";
    btn.disabled = !!previous;
    if (previous) btn.className = "pushed";
    btn.addEventListener("click", async () => {
      btn.disabled = true;
      btn.textContent = "…";
      const resp = await pushTask(item.url, item.headers);
      if (resp?.ok || resp?.error === "task_exists") downloadedResources.set(item.url, resp);
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
    } catch (_) {
      const feedback = document.getElementById("download-result");
      feedback.textContent = "复制链接失败，请重试。";
      feedback.className = "result-error";
    }
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
        const success = resp?.ok || resp?.error === "task_exists";
        vb.textContent = success ? "已添加" : "重试";
        vb.disabled = !!success;
        if (success) vb.classList.add("pushed");
        else {
          const feedback = document.getElementById("download-result");
          feedback.className = "result-error";
          feedback.textContent = "下载失败：" + (ERROR_TEXT[resp?.error] || resp?.error || "未知错误");
        }
      });
      vrow.append(vb);
    }
    mid.append(vrow);
  }

  return div;
}

function applyPushResult(div, btn, resp) {
  div.querySelectorAll(".resource-error").forEach(error => error.remove());
  if (resp?.error === "task_exists") {
    btn.textContent = "已存在";
    btn.className = "pushed";
    btn.disabled = true;
    return true;
  }
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
    err.textContent = "下载失败：" + (ERROR_TEXT[code] || code || "未知");
  }
  err.classList.add("resource-error");
  (div.querySelector(".mid") || div).append(err);
  btn.disabled = false;
  btn.textContent = "重试";
  return false;
}

// ---------------------------------------------------------------- 批量推送

async function pushTask(url, headers, filename) {
  try {
    return await send({
      type: "push",
      rec: { url, filename, headers: headers && Object.keys(headers).length ? headers : null },
    });
  } catch (e) {
    return { ok: false, error: String(e && e.message) || "unknown" };
  }
}

async function downloadResources(items) {
  if (batchBusy || items.length === 0) return;
  batchBusy = true;
  document.getElementById("sel-push").disabled = true;
  document.getElementById("sel-push").textContent = "正在添加…";
  renderFilters();
  let ok = 0, duplicate = 0;
  const failures = [];
  try {
    for (const item of items) {
      const result = await pushTask(item.url, item.headers);
      if (result?.ok || result?.error === "task_exists") {
        if (result.error === "task_exists") duplicate += 1;
        else ok += 1;
        downloadedResources.set(item.url, result);
        selected.delete(item.url);
      } else {
        failures.push(result?.error || "未知错误");
      }
    }
    const feedback = document.getElementById("download-result");
    feedback.className = failures.length || duplicate ? "result-skip" : "result-ok";
    feedback.textContent = `已添加 ${ok} 项 · 已存在 ${duplicate} 项 · 失败 ${failures.length} 项`;
    if (failures.length) feedback.textContent += `：${ERROR_TEXT[failures[0]] || failures[0]}`;
  } finally {
    batchBusy = false;
    document.getElementById("sel-push").disabled = false;
    document.getElementById("sel-push").textContent = "下载选中";
    renderFilters();
    renderResList();
  }
}

function pushSelected() {
  return downloadResources(resItems.filter(x => selected.has(x.url) && x.type !== "dash"));
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
  el.textContent = ok ? "已连接" : "未连接";
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

let taskFilter = "all";
const pendingTaskOps = new Set();
function matchesTaskFilter(task) {
  if (taskFilter === "all") return true;
  if (taskFilter === "active") return ACTIVE.includes(task.status);
  if (taskFilter === "queued") return task.status === "queued" || task.status === "waiting_retry";
  return task.status === taskFilter;
}

async function taskCommand(method, id) {
  const result = await send({ type: "nm", method, params: { id } });
  if (!result?.ok) throw new Error(ERROR_TEXT[result?.error] || result?.error?.message || result?.error?.code || result?.error || "操作失败，请重试");
  return result;
}

function renderTask(t, bps) {
  const div = document.createElement("div");
  div.className = "item task-item";
  div.dataset.taskId = t.id;
  const mid = document.createElement("div");
  mid.className = "mid";
  div.append(resourceIcon(taskType(t)), mid);
  const active = ACTIVE.includes(t.status);
  const hasTotal = t.total_bytes != null && t.total_bytes > 0;
  const pct = hasTotal ? t.status === "done" ? 100 : Math.min(100, Math.round(((t.downloaded_bytes || 0) / t.total_bytes) * 100)) : null;
  const head = document.createElement("div");
  head.className = "task-head";
  head.innerHTML = `<span class="name">${escapeHtml(displayName(t))}</span><span class="task-status">${statusTag(t.status)}<span class="task-percent">${pct == null ? "" : pct + "%"}</span></span>`;
  head.title = displayName(t);
  mid.append(head);

  if (hasTotal || active) {
    const bar = document.createElement("div");
    bar.className = "bar" + (!hasTotal ? " indet" : "");
    bar.setAttribute("role", "progressbar");
    bar.setAttribute("aria-label", displayName(t) + " 下载进度");
    if (hasTotal) {
      bar.setAttribute("aria-valuenow", String(pct));
      bar.setAttribute("aria-valuemin", "0");
      bar.setAttribute("aria-valuemax", "100");
      bar.innerHTML = `<div class="fill ${t.status === "done" ? "done" : t.status === "failed" ? "failed" : t.status === "paused" ? "paused" : ""}" style="width:${pct}%"></div>`;
    }
    mid.append(bar);
  }
  const row = document.createElement("div");
  row.className = "task-meta";
  const size = document.createElement("span");
  size.className = "size";
  const downloaded = t.status === "done" && hasTotal ? t.total_bytes : t.downloaded_bytes || 0;
  size.textContent = t.kind === "hls"
    ? `${downloaded}${hasTotal ? "/" + t.total_bytes : ""} 分片`
    : `${fmtBytes(downloaded)}${hasTotal ? " / " + fmtBytes(t.total_bytes) : active ? " / 大小未知" : ""}`;
  const speed = document.createElement("span");
  speed.className = "speed";
  speed.style.marginLeft = "auto";
  speed.textContent = active && bps > 0 && t.kind !== "hls" ? fmtSpeed(bps) : "";
  row.append(size, speed);
  mid.append(row);
  if (t.status === "failed" && t.error) {
    const error = document.createElement("div");
    error.className = "error";
    error.textContent = "错误：" + String(t.error.message || t.error.code || "下载失败");
    mid.append(error);
  }

  const actions = document.createElement("div");
  actions.className = "actions";
  const mk = (label, action, primary) => {
    const button = document.createElement("button");
    if (primary) button.className = "primary";
    button.textContent = label;
    button.dataset.action = label;
    button.disabled = pendingTaskOps.has(t.id);
    button.addEventListener("click", async () => {
      if (pendingTaskOps.has(t.id)) return;
      if (label === "打开" && /\.(exe|msi|apk|dmg)$/i.test(displayName(t)) && !window.confirm("即将打开可执行文件，请确认文件来源可信。")) return;
      pendingTaskOps.add(t.id);
      actions.querySelectorAll("button").forEach(control => control.disabled = true);
      const feedback = document.getElementById("task-result");
      try {
        await action();
        feedback.className = "";
        feedback.textContent = `${label}：${displayName(t)}`;
      } catch (error) {
        feedback.className = "error";
        feedback.textContent = `${label}失败：${String(error.message || error)}`;
      } finally {
        pendingTaskOps.delete(t.id);
        await renderTasks();
      }
    });
    actions.append(button);
  };
  if (active) {
    mk("暂停", () => taskCommand("task.pause", t.id));
    mk("取消", () => taskCommand("task.cancel", t.id));
  } else if (t.status !== "done") {
    mk(t.status === "failed" || t.status === "canceled" ? "重试" : "开始", async () => {
      if (t.status === "failed" || t.status === "canceled") await taskCommand("task.retry", t.id);
      await taskCommand("task.start", t.id);
    }, true);
  } else {
    mk("打开", () => taskCommand("task.open", t.id));
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

  const tasks = Array.isArray(resp.result) ? resp.result : [];
  document.getElementById("task-count").textContent = String(tasks.length);
  const rank = (t) => (ACTIVE.includes(t.status) ? 0 : 1);
  const sorted = tasks.filter(matchesTaskFilter).sort(
    (a, b) => rank(a) - rank(b) || b.updated_at - a.updated_at
  );
  const shown = sorted.slice(0, 30);
  const truncated = sorted.length > shown.length;

  // E3：每任务每轮只算一次速度存入 Map，总速求和与卡片渲染共读一份
  //（原先 renderTask 内二次调用时 prev 刚更新、dt≈0，速度恒为 0）
  const speedMap = new Map();
  const activeTasks = tasks.filter((x) => ACTIVE.includes(x.status));
  let totalBps = 0;
  for (const t of activeTasks) {
    const bps = t.kind === "hls" ? 0 : computeSpeed(t);
    speedMap.set(t.id, bps);
    totalBps += bps;
  }
  stats.textContent =
    activeTasks.length
      ? `${activeTasks.length} 个任务进行中 · 总速度 ${fmtSpeed(totalBps)}`
      : "无进行中的任务";
  if (truncated) stats.textContent += ` · 共 ${tasks.length} 条`;
  stats.textContent += ` · 当前 ${sorted.length} 项`;
  stats.style.color = activeTasks.length ? "var(--accent-ink)" : "var(--muted)";

  const focused = el.contains(document.activeElement) ? { id: document.activeElement.closest(".task-item")?.dataset.taskId, action: document.activeElement.dataset.action } : null;
  el.innerHTML = "";
  if (shown.length === 0) {
    el.innerHTML = taskFilter === "all" ? emptyMarkup("还没有下载任务", "下载网页资源或粘贴链接后，在这里查看进度。") : emptyMarkup("此状态下没有任务", "切换到「全部下载」查看其他任务。");
    return;
  }
  for (const t of shown) el.append(renderTask(t, speedMap.get(t.id) || 0));
  if (focused) {
    const task = [...el.querySelectorAll(".task-item")].find(row => row.dataset.taskId === focused.id);
    [...task?.querySelectorAll("button") || []].find(button => button.dataset.action === focused.action && !button.disabled)?.focus({ preventScroll: true });
  }
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
    '<div class="empty" data-offline="1">桌面应用未连接，启动并授权后将自动重连<br>' +
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

async function savePreference(control, persist, rollback) {
  const feedback = document.getElementById("settings-feedback");
  control.disabled = true;
  feedback.className = "";
  feedback.textContent = "正在保存…";
  try {
    await persist();
    feedback.textContent = "设置已保存。";
  } catch (_) {
    rollback();
    feedback.className = "error";
    feedback.textContent = "保存失败，已恢复原设置，请重试。";
  } finally {
    control.disabled = false;
  }
}

function init() {
  // —— 静态控件接线（同步，最先执行；此后数据加载错误不影响按钮可用）
  const settingsEl = document.getElementById("settings");
  const takeover = document.getElementById("takeover");
  chrome.storage.local.get("takeover").then(data => { takeover.checked = data.takeover === "on"; }).catch(() => {});
  takeover.addEventListener("change", async () => {
    takeover.disabled = true;
    try { await chrome.storage.local.set({ takeover: takeover.checked ? "on" : "off" }); }
    catch { takeover.checked = !takeover.checked; document.getElementById("paste-result").textContent = "设置保存失败，请重试。"; }
    finally { takeover.disabled = false; }
  });
  document.getElementById("task-filter").addEventListener("change", event => { taskFilter = event.target.value; renderTasks(); });
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
      btn.textContent = "重新扫描";
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
      btn.textContent = "重新扫描";
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
        const previous = !box.checked;
        CFG.types[g] = box.checked;
        await savePreference(box, () => chrome.storage.local.set({ sniffCfg: CFG }), () => { CFG.types[g] = previous; box.checked = previous; });
      });
    }
    const minEl = document.getElementById("set-minsize");
    minEl.value = CFG.minSizeMB;
    minEl.addEventListener("change", async () => {
      const previous = CFG.minSizeMB;
      CFG.minSizeMB = Math.max(0, Number(minEl.value) || 0);
      minEl.value = CFG.minSizeMB;
      await savePreference(minEl, () => chrome.storage.local.set({ sniffCfg: CFG }), () => { CFG.minSizeMB = previous; minEl.value = previous; });
    });

    // —— 视频悬浮下载按钮开关（video-overlays.js 读同一配置即时生效）
    const voEl = document.getElementById("set-video-overlay");
    if (voEl) {
      voEl.checked = CFG.videoOverlay;
      voEl.addEventListener("change", async () => {
        const previous = CFG.videoOverlay;
        CFG.videoOverlay = voEl.checked;
        await savePreference(voEl, () => chrome.storage.local.set({ sniffCfg: CFG }), () => { CFG.videoOverlay = previous; voEl.checked = previous; });
      });
    }

    // —— 平台适配器开关（storage.local.adaptersCfg）
    try {
      const { adaptersCfg } = await chrome.storage.local.get("adaptersCfg");
      for (const box of settingsEl.querySelectorAll("input[data-adapter]")) {
        const id = box.dataset.adapter;
        box.checked = !(adaptersCfg && adaptersCfg[id] === false);
        box.addEventListener("change", async () => {
          const previous = !box.checked;
          await savePreference(box, async () => {
            const current = (await chrome.storage.local.get("adaptersCfg")).adaptersCfg || {};
            current[id] = box.checked;
            await chrome.storage.local.set({ adaptersCfg: current });
          }, () => { box.checked = previous; });
        });
      }
    } catch (_) {}
  });

  // —— 粘贴分享口令：抖音/小红书链接 → 解析 → 直推主程序
  const pasteGo = document.getElementById("paste-go");
  const pasteInput = document.getElementById("paste-input");
  const pasteNote = document.getElementById("paste-result");
  if (pasteGo && pasteInput) {
    let busy = false;
    pasteGo.disabled = !pasteInput.value.trim();
    pasteInput.addEventListener("input", () => { pasteGo.disabled = busy || !pasteInput.value.trim(); });
    pasteGo.addEventListener("click", async () => {
      const text = pasteInput.value.trim();
      if (!text || busy) return;
      busy = true;
      pasteGo.disabled = true;
      pasteGo.textContent = "添加中…";
      pasteNote.className = "";
      pasteNote.textContent = "正在解析下载链接…";
      try {
        let resolved = await send({ type: "adapter-resolve", link: text }, 15000);
        // The share resolver handles supported sites. Plain HTTP(S) URLs go straight to the downloader.
        if (resolved?.error === "unsupported_site") {
          try {
            const direct = new URL(text);
            if (/^https?:$/.test(direct.protocol) && !/\s/.test(text)) resolved = { ok: true, resources: [{ url: direct.href }] };
          } catch (_) {}
        }
        const resources = resolved?.ok && Array.isArray(resolved.resources) ? resolved.resources.filter(resource => !resource.optional) : [];
        if (resources.length === 0) throw new Error(ERROR_TEXT[resolved?.error] || resolved?.error || "未识别到可下载内容");
        let ok = 0, duplicate = 0;
        const failures = [];
        for (const resource of resources) {
          const result = await pushTask(resource.url, resource.headers, resource.filename);
          if (result?.ok) ok += 1;
          else if (result?.error === "task_exists") duplicate += 1;
          else failures.push(result?.error || "未知错误");
        }
        pasteNote.className = failures.length ? "warn" : "";
        pasteNote.textContent = `已添加 ${ok} 项 · 已存在 ${duplicate} 项 · 失败 ${failures.length} 项`;
        if (failures.length) pasteNote.textContent += `：${ERROR_TEXT[failures[0]] || failures[0]}`;
        else pasteInput.value = "";
        if (ok > 0) renderTasks();
      } catch (error) {
        pasteNote.className = "error";
        pasteNote.textContent = "下载失败：" + String(error.message || error);
      } finally {
        busy = false;
        pasteGo.disabled = !pasteInput.value.trim();
        pasteGo.textContent = "下载";
      }
    });
    pasteInput.addEventListener("keydown", event => {
      if (event.key === "Enter") { event.preventDefault(); pasteGo.click(); }
    });
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
    fresh.textContent = "正在添加…";
    const feedback = document.getElementById("download-result");
    try {
      const result = await send({ type: "adapter-push", tabId });
      if (!result?.ok) throw new Error(ERROR_TEXT[result?.error] || result?.error || "未能添加下载任务");
      const partial = result.total && result.pushed < result.total;
      fresh.textContent = partial ? "重试未完成资源" : "已添加下载任务";
      feedback.className = partial ? "result-skip" : "result-ok";
      feedback.textContent = `已添加 ${result.pushed}${result.total ? "/" + result.total : ""} 项${result.dir ? " · 保存目录：" + result.dir : ""}`;
      fresh.disabled = !partial;
      renderTasks();
    } catch (error) {
      fresh.disabled = false;
      fresh.textContent = "重试下载";
      feedback.className = "result-error";
      feedback.textContent = "下载失败：" + String(error.message || error);
    }
  });
  card.classList.remove("hidden");
}

start();
