// popup：本页资源（过滤/批量推送）+ 主程序任务（实时进度/控制）
const ERROR_TEXT = {
  app_not_running: "主程序未运行（先启动 dl-app）",
  timeout: "主程序响应超时",
  invalid_params: "参数被拒绝",
  app_not_registered: "NM 未注册（先运行 scripts/register-nm-host.ps1）",
};

const STATUS_TEXT = {
  queued: "排队中",
  probing: "探测中",
  downloading: "下载中",
  paused: "已暂停",
  waiting_retry: "等待重试",
  merging: "合并中",
  verifying: "校验中",
  done: "已完成",
  failed: "失败",
  canceled: "已取消",
};

const ACTIVE = ["probing", "downloading", "merging", "verifying"];

function send(msg) {
  return chrome.runtime.sendMessage(msg);
}

function statusTag(status) {
  const cls = ACTIVE.includes(status)
    ? "s-active"
    : status === "done"
      ? "s-done"
      : status === "failed"
        ? "s-failed"
        : "s-warn";
  return `<span class="tag ${cls}">${STATUS_TEXT[status] || status}</span>`;
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

function displayName(t) {
  if (t.filename) return t.filename;
  try {
    const seg = new URL(t.url).pathname.split("/").filter(Boolean).pop();
    return decodeURIComponent(seg || t.url);
  } catch {
    return t.url;
  }
}

let view = "res";
let taskTimer = null;

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
  ["other", "其他"],
];

function resTypeGroup(type) {
  if (type === "video" || type === "hls" || type === "dash" || type === "segment")
    return type === "hls" || type === "dash" ? "hls" : "video";
  if (type === "audio") return "audio";
  if (type === "archive" || type === "binary") return "archive";
  if (type === "file") return "doc";
  return "other";
}

let resItems = [];
let resFilter = "all";

function filteredRes() {
  if (resFilter === "all") return resItems;
  return resItems.filter((x) => resTypeGroup(x.type) === resFilter);
}

function renderResList() {
  const list = document.getElementById("list");
  list.innerHTML = "";
  if (!resItems.length) {
    list.innerHTML = '<div class="empty">本页暂未嗅探到资源</div>';
    return;
  }
  const items = filteredRes().sort((a, b) => (b.bytes || 0) - (a.bytes || 0));
  if (!items.length) {
    list.innerHTML = '<div class="empty">该类型无资源</div>';
    return;
  }
  for (const item of items) list.append(renderResItem(item));
}

function renderResItem(item) {
  const div = document.createElement("div");
  div.className = "item";

  // 行 1：类型标签 + 大小
  const row1 = document.createElement("div");
  row1.className = "row1";
  const tag = document.createElement("span");
  tag.className = "tag t-" + item.type;
  tag.textContent = item.type === "dash" ? "dash" : item.type;
  const size = document.createElement("span");
  size.className = "size";
  size.textContent = item.bytes != null ? fmtBytes(item.bytes) : "大小未知";
  row1.append(tag, size);

  // 行 2：文件名 + 操作
  const row2 = document.createElement("div");
  row2.className = "row2";
  const name = document.createElement("span");
  name.style.cssText =
    "flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-weight:600;font-size:12.5px";
  name.textContent = displayName(item);
  name.title = item.url;
  const btn = document.createElement("button");
  btn.className = "icon";
  if (item.type === "dash") {
    btn.disabled = true;
    btn.title = "DASH 暂不支持";
    btn.textContent = "–";
  } else {
    btn.title = "推送到主程序";
    btn.textContent = "⬇";
  }
  const copy = document.createElement("button");
  copy.className = "icon";
  copy.title = "复制链接";
  copy.textContent = "⧉";
  copy.addEventListener("click", async () => {
    try {
      await navigator.clipboard.writeText(item.url);
      copy.textContent = "✓";
      setTimeout(() => (copy.textContent = "⧉"), 1200);
    } catch (_) {}
  });
  row2.append(name, btn, copy);

  const url = document.createElement("div");
  url.className = "url";
  url.textContent = item.url;

  div.append(row1, row2, url);

  btn.addEventListener("click", async () => {
    btn.disabled = true;
    btn.textContent = "…";
    const resp = await pushTask(item.url, item.headers);
    applyPushResult(div, btn, resp);
  });
  // HLS master：逐清晰度推送
  if (item.type === "hls" && item.variants && item.variants.length > 1) {
    const vrow = document.createElement("div");
    vrow.className = "actions";
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
    div.append(vrow);
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
  div.append(err);
  btn.disabled = false;
  btn.textContent = "重试";
  return false;
}

function renderFilters() {
  const el = document.getElementById("filters");
  el.innerHTML = "";
  // 各类型计数
  const counts = { all: resItems.length };
  for (const item of resItems) {
    const g = resTypeGroup(item.type);
    counts[g] = (counts[g] || 0) + 1;
  }
  for (const [key, label] of TYPE_FILTERS) {
    if (key !== "all" && !counts[key]) continue;
    const b = document.createElement("button");
    b.className = "chip-f" + (resFilter === key ? " active" : "");
    b.textContent = `${label}${counts[key] ? ` ${counts[key]}` : ""}`;
    b.addEventListener("click", () => {
      resFilter = key;
      renderFilters();
      renderResList();
    });
    el.append(b);
  }
  // 可推送的 >1 时显示「全部推送」
  const pushable = filteredRes().filter((x) => x.type !== "dash");
  if (pushable.length > 1) {
    const pa = document.createElement("button");
    pa.className = "chip-f push-all";
    pa.textContent = `全部推送 (${pushable.length})`;
    pa.addEventListener("click", async () => {
      pa.disabled = true;
      pa.textContent = "推送中…";
      let ok = 0;
      for (const item of pushable) {
        const r = await pushTask(item.url, item.headers);
        if (r.ok || (r.error && r.error.code === "task_exists")) ok += 1;
      }
      pa.textContent = `已推送 ${ok}/${pushable.length}`;
      setTimeout(() => {
        pa.disabled = false;
        renderFilters();
      }, 2000);
    });
    el.append(pa);
  }
}

// ---------------------------------------------------------------- 主程序任务

const POLL_MS = 1000;
const prevBytes = new Map(); // id -> { bytes, t } 供速度增量计算

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
  div.className = "item";
  const active = ACTIVE.includes(t.status);
  const name = document.createElement("div");
  name.className = "row2";
  name.innerHTML = `${statusTag(t.status)} <span style="flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-weight:600;font-size:12.5px">${escapeHtml(displayName(t))}</span>`;
  div.append(name);

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
    div.append(bar);
    const row = document.createElement("div");
    row.className = "row2";
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
    div.append(row);
  }
  if (t.status === "failed" && t.error) {
    const err = document.createElement("div");
    err.className = "error";
    err.textContent = "错误: " + String(t.error.code ?? "");
    div.append(err);
  }

  const actions = document.createElement("div");
  actions.className = "actions";
  const errIfAny = (resp) => {
    if (resp && resp.ok) return null;
    const code = resp && resp.error && resp.error.code;
    const msg = ERROR_TEXT[code] || code || "未知";
    const err = document.createElement("div");
    err.className = "error";
    err.textContent = msg;
    div.append(err);
    setTimeout(() => err.remove(), 3000);
    return resp;
  };
  const mk = (label, fn, primary) => {
    const b = document.createElement("button");
    if (primary) b.className = "primary";
    b.textContent = label;
    b.addEventListener("click", async () => {
      try {
        errIfAny(await fn());
      } catch (e) {
        errIfAny({ ok: false, error: { code: String(e) } });
      }
      renderTasks();
    });
    actions.append(b);
  };
  if (active) {
    mk("暂停", async () => {
      await send({ type: "nm", method: "task.pause", params: { id: t.id } });
      renderTasks();
    });
    mk("取消", async () => {
      await send({ type: "nm", method: "task.cancel", params: { id: t.id } });
      renderTasks();
    });
  } else if (t.status !== "done") {
    mk(
      t.status === "failed" || t.status === "canceled" ? "重试" : "开始",
      async () => {
        if (t.status === "failed" || t.status === "canceled") {
          await send({ type: "nm", method: "task.retry", params: { id: t.id } });
        }
        await send({ type: "nm", method: "task.start", params: { id: t.id } });
        renderTasks();
      },
      true
    );
  } else {
    mk("打开", async () => {
      await send({ type: "nm", method: "task.open", params: { id: t.id } });
    });
  }
  div.append(actions);
  return div;
}

async function renderTasks() {
  const stats = document.getElementById("tasks-stats");
  const el = document.getElementById("task-list");
  const resp = await send({
    type: "nm",
    method: "task.list",
    params: {},
  });
  if (!resp || !resp.ok) {
    stats.textContent = "";
    el.innerHTML = '<div class="empty">主程序未运行，无法获取任务</div>';
    return;
  }
  const tasks = resp.result || [];
  const rank = (t) => (ACTIVE.includes(t.status) ? 0 : 1);
  const shown = tasks
    .sort((a, b) => rank(a) - rank(b) || b.updated_at - a.updated_at)
    .slice(0, 30);

  const activeTasks = shown.filter((x) => ACTIVE.includes(x.status));
  let totalBps = 0;
  for (const t of activeTasks) totalBps += computeSpeed(t);
  stats.textContent = activeTasks.length
    ? `${activeTasks.length} 个任务进行中 · 总速度 ${fmtSpeed(totalBps)}`
    : "无进行中的任务";
  stats.style.color = activeTasks.length ? "var(--accent)" : "var(--muted)";

  el.innerHTML = "";
  if (shown.length === 0) {
    el.innerHTML = '<div class="empty">主程序暂无任务</div>';
    return;
  }
  for (const t of shown) el.append(renderTask(t, computeSpeed(t)));
}

function escapeHtml(s) {
  return s.replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c])
  );
}

// ---------------------------------------------------------------- 接管记录

async function renderTakeoverInfo() {
  const el = document.getElementById("takeover-info");
  const data = await send({ type: "takeover-data" });
  if (!data) return;
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
      await send({ type: "unignore-host", host: h });
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
    row.innerHTML = `最近接管: <span class="break" style="user-select:text">${escapeHtml(displayName(last))}</span> `;
    const b = document.createElement("button");
    b.textContent = "忽略此站点";
    b.style.cssText = "font-size:11px;padding:1px 8px";
    b.addEventListener("click", async () => {
      await send({ type: "ignore-host", host: last.host });
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
  document.getElementById("res-wrap").classList.add("hidden");
  document.getElementById("tasks").classList.remove("hidden");
  renderTasks();
  clearInterval(taskTimer);
  taskTimer = setInterval(() => {
    if (view === "tasks") renderTasks();
  }, POLL_MS);
}

function switchToRes() {
  view = "res";
  document.getElementById("tab-res").classList.add("active");
  document.getElementById("tab-tasks").classList.remove("active");
  document.getElementById("res-wrap").classList.remove("hidden");
  document.getElementById("tasks").classList.add("hidden");
  clearInterval(taskTimer);
}

(async () => {
  const tab = await getActiveTab();
  if (!tab) return;

  // 主程序存活探测 + 接管开关
  const ping = await send({ type: "ping" });
  const statusEl = document.getElementById("status");
  if (ping && ping.ok) {
    statusEl.textContent = "已连接";
    statusEl.classList.add("ok");
  } else {
    statusEl.textContent = "未运行";
    statusEl.classList.add("err");
  }

  const takeoverEl = document.getElementById("takeover");
  const { takeover } = await chrome.storage.local.get("takeover");
  takeoverEl.checked = takeover === "on";
  takeoverEl.addEventListener("change", async () => {
    await chrome.storage.local.set({ takeover: takeoverEl.checked ? "on" : "off" });
  });

  // 本页资源
  const resp = await send({ type: "list", tabId: tab.id });
  if (resp && resp.items) {
    resItems = resp.items.sort((a, b) => (b.bytes || 0) - (a.bytes || 0));
  }
  renderFilters();
  renderResList();

  // 扫描页面元素（图片/媒体元素，webRequest 看不到的静态资源）
  const scanBtn = document.createElement("button");
  scanBtn.className = "chip-f";
  scanBtn.textContent = "扫描页面元素";
  scanBtn.addEventListener("click", async () => {
    scanBtn.disabled = true;
    scanBtn.textContent = "扫描中…";
    const r = await send({ type: "scan-elements", tabId: tab.id });
    if (r && r.ok) {
      scanBtn.textContent = `+${r.added} 项`;
      if (r.blobVideos > 0) {
        scanBtn.title = "页面含内嵌播放器（blob），请从嗅探列表选择 HLS 资源";
      }
    } else {
      scanBtn.textContent = "扫描失败";
    }
    const { sniff } = await chrome.storage.session.get("sniff");
    if (sniff && sniff[tab.id]) {
      resItems = sniff[tab.id];
      renderFilters();
      renderResList();
    }
    setTimeout(() => {
      scanBtn.disabled = false;
      scanBtn.textContent = "扫描页面元素";
    }, 2000);
  });
  document.getElementById("filters").append(scanBtn);

  // 主程序任务初始渲染
  await renderTasks();

  document.getElementById("tab-res").addEventListener("click", switchToRes);
  document.getElementById("tab-tasks").addEventListener("click", switchToTasks);
  taskTimer = setInterval(() => {
    if (view === "tasks") renderTasks();
  }, POLL_MS);

  renderTakeoverInfo();
})();
