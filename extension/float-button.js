// 悬浮下载按钮 —— 适配器命中的详情页右下角 ⬇ 浮标。
// 单击 = probe 当前页（5s 超时兜底）→ 弹出资源小面板（全选/推送）；平台被禁用时不显示。
// 样式隔离（DESIGN §5.2 F2）：浮标本体保持 inline style（含 button 复位，防宿主
// preflight 撑变形）；面板进 closed Shadow DOM + 构造式样式表（adoptedStyleSheets）
// —— shadow 内 <style> 元素受宿主页 CSP style-src 约束会整块失效，而
// new CSSStyleSheet() 不经元素注入、不受 CSP 限制（运行环境 Chromium 73+）。
// 不向宿主 document 注入任何全局样式/元素选择器规则；z-index 维持 2147483646。
(() => {
  if (window.__adlFloat) return;
  window.__adlFloat = true;

  // —— Anything Studio 色板副本（与 extension/studio.css 一致）。
  //     内容脚本无法跨 document 共享 CSS 变量，故按主题各存一组，matchMedia 切换
  const TOKENS = {
    light: {
      panel: "#fbfaf6", text: "#293c34", muted: "#687263",
      border: "#dedfd5", borderSoft: "#e9e9df",
      accent: "#47644d", accentStrong: "#344e3c", accentInk: "#36543e", accentSoft: "#e9eddf", buttonInk: "#fbfaf6",
      okSoft: "#e9eddf", okInk: "#45633e",
      errSoft: "#f7e6df", errInk: "#a14d3d",
      warnSoft: "#f1e9d7", warnInk: "#856225",
      shadow: "0 12px 40px rgba(35,49,38,.16)",
    },
    dark: {
      panel: "#242e27", text: "#e3e7d9", muted: "#a0ad9b",
      border: "#3b463c", borderSoft: "#303b32",
      accent: "#b6c99c", accentStrong: "#bdd29c", accentInk: "#c1d5aa", accentSoft: "#34432f", buttonInk: "#22301f",
      okSoft: "#34432f", okInk: "#c1d5aa",
      errSoft: "#4d302b", errInk: "#edaf98",
      warnSoft: "#453d2b", warnInk: "#e0c18b",
      shadow: "0 12px 40px rgba(0,0,0,.3)",
    },
  };

  function panelCss(t) {
    return `
:host {
  all: initial; /* 阻断宿主页可继承样式渗入；关键盒样式另由宿主元素 inline 提供 */
  display: block; box-sizing: border-box; padding: 16px;
  background: ${t.panel}; color: ${t.text};
  border: 1px solid ${t.border}; border-radius: 12px;
  box-shadow: ${t.shadow};
  font: 12px/1.6 "Segoe UI", "Microsoft YaHei UI", system-ui, sans-serif;
}
* { box-sizing: border-box; }
button {
  font: inherit; color: ${t.text}; background: ${t.panel};
  border: 1px solid ${t.border}; border-radius: 6px;
  padding: 7px 9px; cursor: pointer; box-sizing: border-box;
}
button:hover { border-color: ${t.accentInk}; color: ${t.accentInk}; }
button:active { opacity: .85; }
button:focus-visible { outline: 2px solid ${t.accentInk}; outline-offset: 2px; }
button:disabled { opacity: .5; cursor: default; }
input[type="checkbox"] { accent-color: ${t.accent}; margin: 0; flex: none; }
.head { display: flex; align-items: center; gap: 6px; margin-bottom: 5px; }
.wordmark { font: 600 25px/1 Georgia, serif; letter-spacing: -1.5px; }
.context-line { color: ${t.muted}; font-size: 10px; padding-bottom: 13px; margin-bottom: 10px; border-bottom: 1px solid ${t.border}; }
.section-label { display: flex; align-items: center; gap: 8px; font-size: 11px; color: ${t.muted}; padding: 2px 0 9px; }
.x {
  margin-left: auto; flex: none; width: 28px; height: 28px; padding: 0;
  display: inline-flex; align-items: center; justify-content: center;
  border: none; background: none; color: ${t.muted};
  font-size: 14px; line-height: 1; border-radius: 6px;
}
.x:hover { border: none; background: ${t.borderSoft}; color: ${t.text}; }
.hint {
  background: ${t.warnSoft}; color: ${t.warnInk}; border-radius: 6px;
  padding: 6px 8px; margin-bottom: 8px; font-size: 12px;
}
.menu {
  display: grid; grid-template-columns: 1fr 1fr; gap: 4px 10px; max-height: 260px; overflow-y: auto;
  margin-bottom: 13px; padding-bottom: 10px; border-bottom: 1px solid ${t.border};
}
.menu button {
  display: flex; align-items: center; gap: 8px; padding: 7px 5px; text-align: left; overflow: hidden;
  text-overflow: ellipsis; white-space: nowrap;
  background: transparent; border-color: transparent; font-size: 11px;
}
.menu button::before { content: attr(data-index); color: ${t.muted}; font: 9px/1.5 Consolas, monospace; }
.menu button:hover { background: ${t.accentSoft}; }
.res-row { display: flex; align-items: center; gap: 8px; padding: 9px 2px; cursor: pointer; border-bottom: 1px solid ${t.borderSoft}; font-size: 11px; }
.res-row:hover { background: ${t.accentSoft}; }
.res-row .nm { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; flex: 1; }
.res-row .tag { font-size: 10px; color: ${t.accentInk}; flex: none; }
.err-row {
  display: none; margin-top: 6px; padding: 6px 8px; border-radius: 6px;
  background: ${t.errSoft}; color: ${t.errInk};
  font-size: 12px; word-break: break-all;
}
.err-row.on { display: block; }
.foot { display: flex; gap: 6px; margin-top: 15px; }
.push {
  flex: 1; background: ${t.accentStrong}; color: ${t.buttonInk}; border: none;
  border-radius: 6px; padding: 9px 0; font-weight: 500;
}
.push:hover { color: ${t.buttonInk}; opacity: .88; border: none; }
.push:disabled { opacity: .5; cursor: default; }
.open-doc {
  flex: 0 0 auto; background: ${t.borderSoft}; color: ${t.accentInk};
  border: none; border-radius: 7px; padding: 6px 10px; font-weight: 600;
}
@media (prefers-reduced-motion: reduce) {
  *, *::before, *::after {
    transition-duration: .01ms !important;
    animation: none !important;
  }
}
`;
  }

  const PROBE_MS = 5000;
  const PUSH_ERROR_TEXT = {
    app_not_running: "主程序未运行——请先启动桌面端",
    adapter_disabled: "此平台已在扩展设置中停用",
    no_tab: "未找到来源页面，请刷新后重试",
    probe_failed: "本页探测失败，请刷新页面后重试",
    timeout: "主程序响应超时",
  };
  const errText = (e) => (e && e.message) || String(e) || "未知错误";
  const pushErrText = (pr) => {
    const e = pr && pr.error;
    const raw =
      typeof e === "string" ? e : (e && (e.message || e.code)) || "";
    return PUSH_ERROR_TEXT[raw] || raw || "主程序未运行或全部推送失败，请确认桌面端已启动";
  };

  const boot = async () => {
    const ads = ADL.matchAdapters(location.href);
    if (!ads.length) return;
    let cfg = null;
    try {
      cfg = await chrome.runtime.sendMessage({ type: "adapters-cfg" });
    } catch (_) {
      return;
    }
    // 任一命中的适配器被启用即显示浮标
    if (!cfg || !cfg.ok || !ads.some((ad) => cfg.cfg[ad.id] !== false)) return;
    // 文章站点精准显示：正文节点不存在（列表页/首页/失效选择器）不注入浮标
    if (ADL.articleSite) {
      const site = ADL.articleSite(location.href);
      if (site) {
        const hasRoot = (site.root || []).some((s) => {
          try {
            return !!document.querySelector(s);
          } catch (_) {
            return false;
          }
        });
        if (!hasRoot) return;
      }
    }
    const label = ads
      .filter((ad) => cfg.cfg[ad.id] !== false)
      .map((ad) => ad.label)
      .join(" + ");

    const S = (el, css) => {
      for (const k in css) el.style[k] = css[k];
      return el;
    };

    // —— F1：浮标 div → button（键盘可达），内联补 button 复位，防宿主
    //     preflight（重置 button 背景/边距/字号）把 40px 圆形撑变形
    const baseTitle = `Anything Downloader · ${label}`;
    const btn = document.createElement("button");
    btn.type = "button";
    btn.setAttribute("aria-label", baseTitle);
    btn.setAttribute("aria-haspopup", "dialog");
    btn.setAttribute("aria-expanded", "false");
    const bookmarkIcon = '<svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m5 4 11-1 1 4M3 6l11-1 2 15-11 1z"/><path d="m14 8 6-1 1 14-6 1M7 11h4m-3 4h4"/></svg>';
    const restoreButton = () => { btn.innerHTML = bookmarkIcon; btn.title = baseTitle; };
    restoreButton();
    btn.title = baseTitle;
    S(btn, {
      // button 复位（宿主页样式表无法穿透 inline style）
      margin: "0", padding: "0", border: "none", boxSizing: "border-box",
      lineHeight: "1", appearance: "none",
      // 显式声明 background 与 font（Tailwind preflight 会重置这两项）
      background: "#e4edb5", color: "#293c34", border: "1px solid #c7d19f",
      font: "16px/1 system-ui, sans-serif",
      // 形态
      position: "fixed", right: "18px", bottom: "18px", zIndex: "2147483646",
      width: "46px", height: "46px", borderRadius: "12px", cursor: "pointer",
      display: "flex", alignItems: "center", justifyContent: "center",
      boxShadow: "0 4px 18px rgba(35,49,38,.18)", userSelect: "none",
      opacity: "0.85",
    });
    btn.addEventListener("mouseenter", () => (btn.style.opacity = "1"));
    btn.addEventListener("mouseleave", () => (btn.style.opacity = "0.85"));
    // 键盘焦点环（inline 无法表达 :focus-visible；双色 ring 保证任意页面可辨）
    btn.addEventListener("focus", () => {
      btn.style.boxShadow =
        "0 4px 18px rgba(35,49,38,.18), 0 0 0 2px #fbfaf6, 0 0 0 4px #47644d";
    });
    btn.addEventListener("blur", () => {
      btn.style.boxShadow = "0 4px 18px rgba(35,49,38,.18)";
    });

    // —— 面板状态（闭包内；host= Shadow DOM 宿主元素）
    let panelHost = null;
    let panelShadow = null;
    let panelEl = null;
    let liveSheet = null;
    let errTimer = null;
    let panelTimer = null;
    let probing = false;

    const darkMq = window.matchMedia("(prefers-color-scheme: dark)");
    // F2：主题变化时重建构造式样式表（亮/暗两组硬编码 token 副本）
    const applyTheme = () => {
      if (liveSheet) {
        liveSheet.replaceSync(panelCss(darkMq.matches ? TOKENS.dark : TOKENS.light));
      }
    };
    darkMq.addEventListener("change", applyTheme);

    const focusables = () =>
      panelEl
        ? [...panelEl.querySelectorAll("button, input, select, a[href], [tabindex]:not([tabindex='-1'])")]
            .filter((el) => !el.disabled)
        : [];

    const onDocClick = (e) => {
      if (!panelHost) return;
      const path = typeof e.composedPath === "function" ? e.composedPath() : [e.target];
      if (path.includes(panelHost) || e.target === btn || (btn && btn.contains(e.target))) return;
      closePanel(false); // F6：点击面板/浮标之外关闭
    };

    // F1：Esc 关闭 + Tab 焦点循环困于面板内
    const onHostKeydown = (e) => {
      if (!panelHost) return;
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        closePanel(true);
        return;
      }
      if (e.key !== "Tab") return;
      const els = focusables();
      if (!els.length) {
        e.preventDefault();
        return;
      }
      const cur = panelShadow.activeElement;
      const idx = els.indexOf(cur);
      let next;
      if (e.shiftKey) next = idx <= 0 ? els[els.length - 1] : els[idx - 1];
      else next = idx === -1 || idx === els.length - 1 ? els[0] : els[idx + 1];
      next.focus();
      e.preventDefault();
    };

    function closePanel(restoreFocus) {
      if (!panelHost) return;
      document.removeEventListener("click", onDocClick, true);
      if (errTimer) { clearTimeout(errTimer); errTimer = null; }
      if (panelTimer) { clearTimeout(panelTimer); panelTimer = null; }
      const host = panelHost;
      panelHost = null;
      panelShadow = null;
      panelEl = null;
      liveSheet = null;
      btn.setAttribute("aria-expanded", "false");
      host.remove();
      if (restoreFocus) btn.focus(); // 关闭后焦点归还触发元素
    }

    // F4/F3：面板内错误行（err-soft 底 + err-ink 字，3s 或指定时长自动消失）
    const showPanelError = (msg, ttlMs) => {
      if (!panelEl) return;
      let row = panelEl.querySelector(".err-row");
      if (!row) {
        row = document.createElement("div");
        row.className = "err-row";
        panelEl.append(row);
      }
      row.textContent = msg;
      row.classList.add("on");
      if (errTimer) clearTimeout(errTimer);
      errTimer = ttlMs
        ? setTimeout(() => {
            if (row) row.classList.remove("on");
          }, ttlMs)
        : null;
    };

    const openPanel = (r) => {
      const host = document.createElement("div");
      // 关键盒样式 inline 在宿主元素上（优先级高于宿主页样式表，防挤破布局）
      S(host, {
        position: "fixed", right: "18px", bottom: "76px", zIndex: "2147483646",
        width: "min(348px, calc(100vw - 36px))", maxHeight: "min(70vh, 580px)", overflowY: "auto",
      });
      host.setAttribute("role", "dialog");
      host.tabIndex = -1;
      host.setAttribute("aria-label", "Anything Downloader 下载面板");
      const shadow = host.attachShadow({ mode: "closed" });
      const panel = document.createElement("div");
      shadow.append(panel);
      const sheet = new CSSStyleSheet(); // 不经元素注入，不受宿主页 CSP style-src 约束
      sheet.replaceSync(panelCss(darkMq.matches ? TOKENS.dark : TOKENS.light));
      shadow.adoptedStyleSheets = [sheet];
      liveSheet = sheet;
      panelHost = host;
      panelShadow = shadow;
      panelEl = panel;
      btn.setAttribute("aria-expanded", "true");
      host.addEventListener("keydown", onHostKeydown);

      const head = document.createElement("div");
      head.className = "head";
      const wordmark = document.createElement("span");
      wordmark.className = "wordmark";
      wordmark.textContent = "anything.";
      head.append(wordmark);
      // F1：✕ span → button，命中区 28×28，带 aria-label
      const x = document.createElement("button");
      x.type = "button";
      x.className = "x";
      x.setAttribute("aria-label", "关闭");
      x.textContent = "✕";
      x.addEventListener("click", () => closePanel(true));
      head.append(x);
      panel.append(head);
      const contextLine = document.createElement("div");
      contextLine.className = "context-line";
      contextLine.textContent = `${label} / 把这一页，收好。`;
      panel.append(contextLine);

      // —— 旧版脚本提示：扩展重载后未刷新的页面缺新注入（actions.js 不在）
      if (staleScripts) {
        const hint = document.createElement("div");
        hint.className = "hint";
        hint.textContent = "检测到旧版脚本：请刷新页面（F5）启用文章操作菜单";
        panel.append(hint);
      }

      // —— 文章操作菜单（对齐 code-box；非文章站无此区）
      if (actions.length) {
        const menuLabel = document.createElement("div");
        menuLabel.className = "section-label";
        menuLabel.textContent = "文章工具";
        panel.append(menuLabel);
        const menu = document.createElement("div");
        menu.className = "menu";
        for (const act of actions) {
          const b = document.createElement("button");
          b.type = "button";
          b.textContent = act.label;
          b.title = act.label;
          b.dataset.index = String(menu.children.length + 1).padStart(2, "0");
          b.addEventListener("click", () => {
            let r2 = null;
            try {
              r2 = act.run();
            } catch (e) {
              // F4：失败在面板内给红字提示（3s 自动消失），不再只落日志
              console.warn("[adl] action " + act.id + " failed:", e);
              showPanelError(`${act.label}失败：${errText(e)}`, 3000);
              return;
            }
            if (r2 && typeof r2.catch === "function") {
              r2.catch((e) => {
                console.warn("[adl] action " + act.id + " failed:", e);
                showPanelError(`${act.label}失败：${errText(e)}`, 3000);
              });
            }
          });
          menu.append(b);
        }
        panel.append(menu);
      }

      // —— 媒体资源区
      if (r && r.ok && r.resources && r.resources.length) {
        const resourceLabel = document.createElement("div");
        resourceLabel.className = "section-label";
        resourceLabel.textContent = `本页发现 / ${r.resources.length} 项`;
        panel.append(resourceLabel);
        const boxes = [];
        for (const res of r.resources) {
          const row = document.createElement("label");
          row.className = "res-row";
          const cb = document.createElement("input");
          cb.type = "checkbox";
          cb.checked = !res.optional; // 可选画质变体默认不勾
          boxes.push([cb, res]);
          const name = document.createElement("span");
          name.className = "nm";
          name.textContent = res.filename || res.url;
          name.title = res.url;
          const tag = document.createElement("span");
          tag.className = "tag";
          tag.textContent =
            (res.kind === "video"
              ? "视频"
              : res.kind === "livephoto"
              ? "Live"
              : res.kind === "document"
              ? "文档"
              : "图片") + (res.meta && res.meta.quality ? ` ${res.meta.quality}` : "");
          row.append(cb, name, tag);
          panel.append(row);
        }

        const foot = document.createElement("div");
        foot.className = "foot";
        const push = document.createElement("button");
        push.type = "button";
        push.className = "push";
        // F3：计数随勾选实时联动；0 项禁用并给 title 说明（替代静默 return）
        const syncCount = () => {
          const n = boxes.filter(([cb]) => cb.checked).length;
          push.textContent = `收下 ${n} 项 ↗`;
          push.disabled = n === 0;
          push.title = n === 0 ? "请先勾选要推送的资源" : "";
        };
        for (const [cb] of boxes) cb.addEventListener("change", syncCount);
        syncCount();
        push.addEventListener("click", async () => {
          const urls = boxes.filter(([cb]) => cb.checked).map(([, res]) => res.url);
          if (!urls.length) return; // 双保险：0 项时按钮已 disabled
          push.disabled = true;
          push.textContent = "推送中…";
          try {
            const pr = await chrome.runtime.sendMessage({ type: "adapter-push", urls });
            if (pr && pr.ok) {
              // 反馈带落点目录（文章合集模式）；部分失败时如实报 N/M
              const partial =
                pr.total && pr.pushed < pr.total ? `/${pr.total}` : "";
              push.textContent = `已保存 ${pr.pushed}${partial} 项${pr.dir ? ` → ${pr.dir}/` : ""}`;
              if (pr.docTaskId) {
                // 「打开文章」= task.open 打开排版 HTML（浏览器内直接预览导出效果）
                const open = document.createElement("button");
                open.type = "button";
                open.className = "open-doc";
                open.textContent = "打开文章";
                open.addEventListener("click", () => {
                  chrome.runtime
                    .sendMessage({ type: "nm", method: "task.open", params: { id: pr.docTaskId } })
                    .catch(() => {});
                });
                foot.append(open);
              }
              // 成功留足查看时间（8s），不再 1.6s 抢关
              panelTimer = setTimeout(() => { if (panelHost) closePanel(false); }, 8000);
            } else {
              // F3：失败带原因、停留 ≥6s 或至用户关闭；按钮转「重试」，面板不抢关
              showPanelError("推送失败：" + pushErrText(pr), 6000);
              push.disabled = false;
              push.textContent = "重试";
            }
          } catch (e) {
            showPanelError("推送失败：" + errText(e), 6000);
            push.disabled = false;
            push.textContent = "重试";
          }
        });
        foot.append(push);
        panel.append(foot);
      }

      document.documentElement.append(host);
      document.addEventListener("click", onDocClick, true);
      // F1：打开时焦点落首交互元素（✕）；Tab 循环由 onHostKeydown 兜住
      const first = focusables()[0];
      if (first) first.focus();
    };

    // 旧版脚本检测：扩展重载后已打开标签页仍是旧注入（actions.js 缺失）→
    // articleSite 命中但 articleActionEnv 不存在 → 面板提示刷新页面。
    const isArticleSite = !!(ADL.articleSite && ADL.articleSite(location.href));
    const staleScripts = isArticleSite && typeof ADL.articleActionEnv !== "function";
    let actionEnv = null;
    let actions = [];
    const buildActions = () => {
      if (!isArticleSite || typeof ADL.articleActionEnv !== "function") return;
      if (actionEnv) return;
      try {
        actionEnv = ADL.articleActionEnv();
        actions = actionEnv ? ADL.articleActions(actionEnv) : [];
      } catch (e) {
        console.warn("[adl] 文章操作环境构建失败：", e);
        actionEnv = null;
        actions = [];
      }
    };

    // F5：probe 5s 超时 race；探测期间点击无效，超时后迟到的结果直接丢弃不弹面板
    btn.addEventListener("click", async () => {
      if (probing) return;
      if (panelHost) { closePanel(true); return; }
      probing = true;
      btn.disabled = true;
      btn.setAttribute("aria-busy", "true");
      btn.textContent = "…";
      buildActions(); // 点击时构建（DOM 完全就绪）
      let r = null;
      let timedOut = false;
      try {
        const res = await Promise.race([
          chrome.runtime.sendMessage({ type: "adapter-probe" }),
          new Promise((resolve) => setTimeout(() => resolve("adl-probe-timeout"), PROBE_MS)),
        ]);
        if (res === "adl-probe-timeout") timedOut = true;
        else r = res;
      } catch (_) {}
      probing = false;
      btn.disabled = false;
      btn.removeAttribute("aria-busy");
      restoreButton();
      if (timedOut) {
        // 迟到的 probe 结果不保存引用即不会被应用；用户再次点击 = 全新 probe
        btn.textContent = "⌛";
        setTimeout(() => {
          if (!probing && !panelHost) restoreButton();
        }, 1500);
        return;
      }
      if (
        !actionEnv &&
        (!r || !r.ok || !r.resources || !r.resources.length)
      ) {
        btn.textContent = "✕";
        btn.title = r && r.error === "adapter_disabled" ? "此平台已禁用" : "本页未发现可下载媒体";
        setTimeout(() => {
          if (!probing && !panelHost) restoreButton();
        }, 1500);
        return;
      }
      openPanel(r);
    });

    document.documentElement.append(btn);
  };

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", boot, { once: true });
  } else {
    boot();
  }
})();
