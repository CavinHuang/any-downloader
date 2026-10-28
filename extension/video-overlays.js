// 视频悬浮下载按钮 —— 任意站点的 <video> 右上角 ⬇ 浮标。
// 鼠标移入视频区域 → 右上角浮现下载按钮，单击 = 后台智能解析（站点适配器 >
// 视频直链 > 本页嗅探记录 > yt-dlp 页面托管，见 core.js pickVideoSource）推送
// 桌面端「即建即下」。
// 样式隔离与 float-button.js 同款：closed Shadow DOM + 构造式样式表（防宿主页
// CSP 与样式污染；shadow 内 <style> 受 style-src 约束会整块失效）；宿主
// position:fixed + z-index 2147483646，仅按钮本体 pointer-events:auto。
// 视频发现走「自有视频清单 + getBoundingClientRect 命中检测」而非事件 target：
// mousemove 的 target 在开放 Shadow DOM 内会被截断成宿主元素，逐视频比矩形才
// 能穿透嵌套播放器；命中多个时取面积最小者（内层视频优先）。
// 不依赖 actions.js（本脚本注入 <all_urls>，文章工具栈只在适配器站点加载），
// 反馈用自带迷你 toast。
(() => {
  if (window.__adlVideoOverlay) return;
  window.__adlVideoOverlay = true;

  const BTN = 34; // 按钮边长（px）
  const MARGIN = 10; // 距视频右上角的边距
  const MIN_W = 240; // 过窄视频（广告位/缩略图）不挂浮标
  const NEAR = 60; // 指针容差（离开视频但仍在附近时暂不隐藏）
  const HIDE_DELAY = 800; // 指针离开后的隐藏延迟
  const RESCAN_MS = 3000; // SPA 兜底轮询（MutationObserver 之外）
  const RESCAN_DEBOUNCE = 500;
  const MAX_DEPTH = 8; // shadow DOM 递归深度上限（防病态嵌套）

  let enabled = true; // sniffCfg.videoOverlay（默认开）
  let blocked = false; // videoOverlayHosts 黑名单命中当前站点
  let installed = false;

  let host = null;
  let btn = null;
  let icons = null; // { dl, ok, err, spin } 预建节点，切状态只切 display
  let toastEl = null;

  let videos = []; // 最近一次扫描到的 video 元素（含开放 shadow DOM 内）
  let active = null; // 当前悬浮命中的视频
  let lastMouse = { x: -1, y: -1 };
  let placed = { left: 0, top: 0 }; // 自记录定位（offsetLeft 对 fixed 元素语义不可靠）
  let hideTimer = 0;
  let rafId = 0;
  let busy = false;

  // —— Anything Downloader 色板（float-button.js TOKENS 精简副本）
  const TOKENS = {
    light: {
      panel: "#ffffff", ink: "#243147", border: "#e3e8ef",
      accent: "#3779ec", accentInk: "#ffffff",
      okBg: "#dcfce7", okBorder: "#86efac", okInk: "#166534",
      errBg: "#fee2e2", errBorder: "#fca5a5", errInk: "#991b1b",
      shadow: "0 6px 22px rgba(24,36,56,.22)",
    },
    dark: {
      panel: "#1c2634", ink: "#e0e7f2", border: "#303d50",
      accent: "#326bd0", accentInk: "#ffffff",
      okBg: "#203d35", okBorder: "#2f6d56", okInk: "#83d5b1",
      errBg: "#442c35", errBorder: "#5c3a33", errInk: "#f2a4a4",
      shadow: "0 6px 22px rgba(0,0,0,.45)",
    },
  };
  const theme = () => (matchMedia("(prefers-color-scheme: dark)").matches ? TOKENS.dark : TOKENS.light);

  function sheetCss(t) {
    return `
:host {
  all: initial; /* 阻断宿主可继承样式；关键盒样式由宿主 inline 提供（CSSOM 赋值不受 CSP 约束） */
  position: fixed; left: 0; top: 0; z-index: 2147483646;
  pointer-events: none; display: none; box-sizing: border-box;
}
* { box-sizing: border-box; }
.btn {
  pointer-events: auto; width: ${BTN}px; height: ${BTN}px;
  display: flex; align-items: center; justify-content: center;
  border-radius: 10px; border: 1px solid ${t.border};
  background: ${t.panel}; color: ${t.ink};
  box-shadow: ${t.shadow}; cursor: pointer; padding: 0;
  font: 12px/1 "Segoe UI", "Microsoft YaHei UI", system-ui, sans-serif;
  transition: background .15s ease, color .15s ease, opacity .15s ease;
}
.btn:hover { background: ${t.accent}; color: ${t.accentInk}; border-color: ${t.accent}; }
.btn:active { opacity: .85; }
.btn:focus-visible { outline: 2px solid ${t.accent}; outline-offset: 2px; }
.btn.busy { cursor: progress; opacity: .9; }
.btn.ok { background: ${t.okBg}; border-color: ${t.okBorder}; color: ${t.okInk}; }
.btn.err { background: ${t.errBg}; border-color: ${t.errBorder}; color: ${t.errInk}; }
.spin {
  width: 14px; height: 14px; border-radius: 50%;
  border: 2px solid ${t.border}; border-top-color: ${t.accent};
  animation: adl-vo-spin .7s linear infinite;
}
@keyframes adl-vo-spin { to { transform: rotate(360deg); } }
.ico, .spin { display: none; }
.st-idle .ico-dl, .st-busy .spin, .st-ok .ico-ok, .st-err .ico-err { display: block; }
.toast {
  position: absolute; top: ${BTN + 8}px; right: 0; white-space: nowrap;
  background: ${t.panel}; color: ${t.ink}; border: 1px solid ${t.border};
  border-radius: 8px; box-shadow: ${t.shadow}; padding: 6px 10px;
  font: 12px/1.5 "Segoe UI", "Microsoft YaHei UI", system-ui, sans-serif;
  max-width: 260px; overflow: hidden; text-overflow: ellipsis;
  opacity: 0; transition: opacity .18s ease; pointer-events: none;
}
.toast.on { opacity: 1; }
@media (prefers-reduced-motion: reduce) {
  .btn, .toast { transition-duration: .01ms !important; animation: none !important; }
}
`;
  }

  const svg = (d, w) =>
    `<svg class="ico ico-${d === "dl" ? "dl" : d}" width="${w}" height="${w}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="${d === "dl" ? 2 : 2.4}" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${
      d === "dl"
        ? '<path d="M12 4v11"/><path d="m7 12 5 5 5-5"/><path d="M5 20h14"/>'
        : d === "ok"
          ? '<path d="m5 13 4 4L19 7"/>'
          : '<path d="M6 6l12 12M18 6 6 18"/>'
    }</svg>`;

  function ensureUi() {
    if (host) return;
    host = document.createElement("div");
    host.id = "adl-video-dl";
    const shadow = host.attachShadow({ mode: "closed" });
    const applySheet = () => {
      const sheet = new CSSStyleSheet();
      sheet.replaceSync(sheetCss(theme()));
      shadow.adoptedStyleSheets = [sheet];
    };
    applySheet();
    const root = document.createElement("div");
    root.innerHTML =
      `<button class="btn st-idle" type="button" aria-label="用 Anything Downloader 下载此视频" title="下载此视频">${svg("dl", 16)}${svg("ok", 16)}${svg("err", 16)}<span class="spin"></span></button>` +
      `<div class="toast" role="status"></div>`;
    btn = root.querySelector(".btn");
    toastEl = root.querySelector(".toast");
    icons = {
      dl: btn.querySelector(".ico-dl"),
      ok: btn.querySelector(".ico-ok"),
      err: btn.querySelector(".ico-err"),
    };
    shadow.appendChild(root);
    btn.addEventListener("click", onClick, true);
    document.documentElement.appendChild(host);
    try {
      matchMedia("(prefers-color-scheme: dark)").addEventListener("change", applySheet);
    } catch (_) {}
  }

  // ---------------------------------------------------------------- 视频发现

  function collectVideos(root, depth) {
    let out = [];
    try {
      out = [...root.querySelectorAll("video")];
    } catch (_) {}
    if (depth >= MAX_DEPTH) return out;
    let all = [];
    try {
      all = root.querySelectorAll("*");
    } catch (_) {}
    for (const el of all) {
      if (el.shadowRoot) out = out.concat(collectVideos(el.shadowRoot, depth + 1));
    }
    return out;
  }

  function rescan() {
    if (!enabled || blocked) return;
    videos = collectVideos(document, 0);
    if (active && !videos.includes(active)) hide(); // 挂掉的视频立即失效
  }

  // ---------------------------------------------------------------- 显隐与定位

  function hostRoot() {
    // 全屏时宿主须移进全屏元素，否则被全屏层遮挡；<video> 原生全屏不是容器，
    // 挂其父级
    const fs = document.fullscreenElement;
    if (!fs) return document.documentElement;
    return fs.tagName === "VIDEO" ? fs.parentElement || document.documentElement : fs;
  }

  function place() {
    if (!host || !active) return;
    const r = active.getBoundingClientRect();
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    placed.left = Math.min(Math.max(r.right - BTN - MARGIN, 4), Math.max(vw - BTN - 4, 4));
    placed.top = Math.min(Math.max(r.top + MARGIN, 4), Math.max(vh - BTN - 4, 4));
    host.style.left = placed.left + "px";
    host.style.top = placed.top + "px";
    host.style.display = "block";
  }

  function track() {
    rafId = 0;
    if (!active) return hide();
    if (!host || !host.isConnected || host.parentElement !== hostRoot()) {
      hostRoot().appendChild(host);
    }
    place();
    // 指针仍在视频附近（NEAR 容差）或悬在本按钮上：保持；否则起延时隐藏
    const r = active.getBoundingClientRect();
    const nearVideo =
      lastMouse.x >= r.left - NEAR &&
      lastMouse.x <= r.right + NEAR &&
      lastMouse.y >= r.top - NEAR &&
      lastMouse.y <= r.bottom + NEAR;
    const onBtn =
      lastMouse.x >= placed.left - 4 &&
      lastMouse.x <= placed.left + BTN + 4 &&
      lastMouse.y >= placed.top - 4 &&
      lastMouse.y <= placed.top + BTN + 4;
    if (nearVideo || onBtn) {
      clearTimeout(hideTimer);
      hideTimer = 0;
    } else if (!hideTimer) {
      hideTimer = setTimeout(hide, HIDE_DELAY);
    }
    rafId = requestAnimationFrame(track);
  }

  function showFor(video) {
    active = video;
    ensureUi();
    setState("idle"); // 上一次点击的 ok/err 残留状态复位
    hideToast();
    place();
    if (!rafId) rafId = requestAnimationFrame(track);
  }

  function hide() {
    active = null;
    if (rafId) {
      cancelAnimationFrame(rafId);
      rafId = 0;
    }
    clearTimeout(hideTimer);
    hideTimer = 0;
    if (host) host.style.display = "none";
    hideToast();
  }

  function onMove(e) {
    if (!enabled || blocked) return;
    lastMouse = { x: e.clientX, y: e.clientY };
    if (busy) return; // 下载中锁定浮标反馈，不做命中切换
    // 命中检测：包含指针的最小面积视频（嵌套播放器取内层）
    let hit = null;
    let hitArea = Infinity;
    for (const v of videos) {
      if (!v.isConnected) continue;
      const r = v.getBoundingClientRect();
      if (r.width < MIN_W) continue;
      if (e.clientX >= r.left && e.clientX <= r.right && e.clientY >= r.top && e.clientY <= r.bottom) {
        const a = r.width * r.height;
        if (a < hitArea) {
          hit = v;
          hitArea = a;
        }
      }
    }
    if (hit) showFor(hit);
  }

  // ---------------------------------------------------------------- 点击推送

  const ERR_TEXT = {
    no_source: "未嗅探到可下载资源",
    app_not_running: "主程序未运行——请先启动桌面端",
    timeout: "主程序响应超时",
    no_tab: "未找到来源页面",
  };

  function setState(st) {
    if (!btn) return;
    btn.classList.remove("st-idle", "st-busy", "st-ok", "st-err", "busy");
    btn.classList.add("st-" + st);
    if (st === "busy") btn.classList.add("busy");
  }

  let toastTimer = 0;
  function showToast(text) {
    if (!toastEl) return;
    toastEl.textContent = text;
    toastEl.classList.add("on");
    clearTimeout(toastTimer);
    toastTimer = setTimeout(hideToast, 1800);
  }
  function hideToast() {
    if (toastEl) toastEl.classList.remove("on");
  }

  async function onClick() {
    const v = active;
    if (!v || busy) return;
    busy = true;
    setState("busy");
    const resp = await new Promise((resolve) => {
      try {
        chrome.runtime.sendMessage(
          {
            type: "video-download",
            srcUrl: v.currentSrc || v.src || "",
            pageUrl: location.href,
            title: (document.title || "").slice(0, 120),
            duration: isFinite(v.duration) ? v.duration : 0,
            vw: v.videoWidth || 0,
            vh: v.videoHeight || 0,
          },
          (r) => {
            if (chrome.runtime.lastError) resolve(null);
            else resolve(r);
          }
        );
      } catch (_) {
        resolve(null);
      }
    });
    busy = false;
    const ok = !!(resp && resp.ok);
    setState(ok ? "ok" : "err");
    if (ok) {
      showToast(
        resp.exists ? "任务已存在" : resp.via === "ytdlp" ? "已添加视频下载任务" : "已创建下载任务"
      );
    } else {
      showToast(ERR_TEXT[(resp && resp.error) || ""] || "下载任务添加失败，请重试");
    }
    setTimeout(() => {
      setState("idle");
      if (ok) hide();
    }, 1400);
  }

  // ---------------------------------------------------------------- 安装/配置

  function install() {
    if (installed) return;
    installed = true;
    document.addEventListener("mousemove", onMove, { passive: true, capture: true });
    document.addEventListener("fullscreenchange", () => {
      hide();
      rescan();
    });
    window.addEventListener("blur", hide);
    let moTimer = 0;
    const mo = new MutationObserver(() => {
      clearTimeout(moTimer);
      moTimer = setTimeout(rescan, RESCAN_DEBOUNCE);
    });
    const startMo = () => {
      if (document.body) mo.observe(document.documentElement, { childList: true, subtree: true });
      else setTimeout(startMo, 300);
    };
    startMo();
    setInterval(rescan, RESCAN_MS);
    rescan();
  }

  function apply() {
    if (enabled && !blocked) install();
    else hide();
  }

  async function loadCfg() {
    try {
      const { sniffCfg } = await chrome.storage.local.get("sniffCfg");
      enabled = !(sniffCfg && sniffCfg.videoOverlay === false);
      const hosts = (sniffCfg && sniffCfg.videoOverlayHosts) || [];
      blocked = hosts.includes(location.hostname);
    } catch (_) {}
    apply();
  }
  loadCfg();
  chrome.storage.onChanged.addListener((changes, area) => {
    if (area === "local" && changes.sniffCfg) loadCfg();
  });
})();
