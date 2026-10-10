// 悬浮下载按钮 —— 适配器命中的详情页右下角 ⬇ 浮标。
// 单击 = probe 当前页 → 弹出资源小面板（全选/推送）；平台被禁用时不显示。
(() => {
  if (window.__adlFloat) return;
  window.__adlFloat = true;

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

    const btn = document.createElement("div");
    btn.textContent = "⬇";
    btn.title = `Anything Downloader · ${label}`;
    S(btn, {
      position: "fixed", right: "18px", bottom: "18px", zIndex: "2147483646",
      width: "40px", height: "40px", borderRadius: "50%", cursor: "pointer",
      background: "#2563eb", color: "#fff", fontSize: "18px",
      display: "flex", alignItems: "center", justifyContent: "center",
      boxShadow: "0 2px 10px rgba(15,23,42,.35)", userSelect: "none",
      font: "16px/1 system-ui, sans-serif", opacity: "0.85",
    });
    btn.addEventListener("mouseenter", () => (btn.style.opacity = "1"));
    btn.addEventListener("mouseleave", () => (btn.style.opacity = "0.85"));

    let panel = null;
    const closePanel = () => {
      if (panel) { panel.remove(); panel = null; }
    };

    // 文章站点：操作菜单环境**点击时懒构建**（boot 即构建会在每次文章页加载时
    // 白跑提取管线；点击时 DOM 也已完全就绪）。构建失败不阻塞资源面板。
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

    btn.addEventListener("click", async () => {
      if (panel) { closePanel(); return; }
      buildActions(); // 点击时构建（DOM 完全就绪）
      btn.textContent = "…";
      let r = null;
      try {
        r = await chrome.runtime.sendMessage({ type: "adapter-probe" });
      } catch (_) {}
      btn.textContent = "⬇";
      if (
        !actionEnv &&
        (!r || !r.ok || !r.resources || !r.resources.length)
      ) {
        btn.textContent = "✕";
        btn.title = r && r.error === "adapter_disabled" ? "此平台已禁用" : "本页未发现可下载媒体";
        setTimeout(() => { btn.textContent = "⬇"; }, 1500);
        return;
      }

      panel = S(document.createElement("div"), {
        position: "fixed", right: "18px", bottom: "66px", zIndex: "2147483646",
        width: "300px", maxHeight: "60vh", overflowY: "auto",
        background: "#ffffff", color: "#0f172a", border: "1px solid #e2e8f0",
        borderRadius: "10px", boxShadow: "0 6px 24px rgba(15,23,42,.2)",
        font: "12px/1.5 system-ui, sans-serif", padding: "8px",
      });
      const head = S(document.createElement("div"), {
        display: "flex", alignItems: "center", gap: "6px", marginBottom: "6px", fontWeight: "600",
      });
      head.append(`${label}${r && r.ok && r.resources ? ` · ${r.resources.length} 项` : ""}`);
      const x = S(document.createElement("span"), {
        marginLeft: "auto", cursor: "pointer", padding: "0 6px", color: "#64748b",
      });
      x.textContent = "✕";
      x.addEventListener("click", closePanel);
      head.append(x);
      panel.append(head);

      // —— 旧版脚本提示：扩展重载后未刷新的页面缺新注入（actions.js 不在）
      if (staleScripts) {
        const hint = S(document.createElement("div"), {
          background: "#fef9c3", color: "#854d0e", borderRadius: "6px",
          padding: "6px 8px", marginBottom: "8px", fontSize: "12px",
        });
        hint.textContent = "检测到旧版脚本：请刷新页面（F5）启用文章操作菜单";
        panel.append(hint);
      }

      // —— 文章操作菜单（对齐 code-box；非文章站无此区）
      if (actions.length) {
        const menu = S(document.createElement("div"), {
          display: "grid", gridTemplateColumns: "1fr 1fr", gap: "4px",
          marginBottom: "8px", paddingBottom: "8px", borderBottom: "1px solid #e2e8f0",
        });
        for (const act of actions) {
          const b = S(document.createElement("button"), {
            border: "1px solid #e2e8f0", background: "#f8fafc", color: "#334155",
            borderRadius: "6px", padding: "6px 4px", cursor: "pointer",
            fontSize: "12px", textAlign: "center", overflow: "hidden",
            textOverflow: "ellipsis", whiteSpace: "nowrap",
          });
          b.textContent = act.label;
          b.title = act.label;
          b.addEventListener("click", () => {
            try {
              const r = act.run();
              // 异步动作（下载 Markdown/图片 zip/微信图片内联）的失败也落日志
              if (r && typeof r.catch === "function") {
                r.catch((e) =>
                  console.warn("[adl] action " + act.id + " failed:", e)
                );
              }
            } catch (e) {
              console.warn("[adl] action " + act.id + " failed:", e);
            }
          });
          menu.append(b);
        }
        panel.append(menu);
      }

      // —— 媒体资源区（原有交互）
      if (r && r.ok && r.resources && r.resources.length) {
      const boxes = [];
      for (const res of r.resources) {
        const row = S(document.createElement("label"), {
          display: "flex", alignItems: "center", gap: "6px", padding: "3px 2px", cursor: "pointer",
        });
        const cb = document.createElement("input");
        cb.type = "checkbox";
        cb.checked = !res.optional; // 可选画质变体默认不勾
        boxes.push([cb, res]);
        const name = S(document.createElement("span"), {
          overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", flex: "1",
        });
        name.textContent = res.filename || res.url;
        name.title = res.url;
        const tag = S(document.createElement("span"), {
          fontSize: "10px", color: r.resources.length ? "#2563eb" : "#64748b", flex: "none",
        });
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

      const foot = S(document.createElement("div"), { display: "flex", gap: "6px", marginTop: "8px" });
      const push = document.createElement("button");
      push.textContent = `推送 ${boxes.length} 项`;
      S(push, {
        flex: "1", background: "#2563eb", color: "#fff", border: "none",
        borderRadius: "7px", padding: "6px 0", cursor: "pointer", fontWeight: "600",
      });
      push.addEventListener("click", async () => {
        const urls = boxes.filter(([cb]) => cb.checked).map(([, res]) => res.url);
        if (!urls.length) return;
        push.disabled = true;
        push.textContent = "推送中…";
        let opened = false;
        try {
          const pr = await chrome.runtime.sendMessage({ type: "adapter-push", urls });
          if (pr && pr.ok) {
            // 反馈带落点目录（文章合集模式）；「打开文章」= task.open 打开排版
            // HTML（浏览器内直接预览导出效果），有产物时面板保持打开供点击
            push.textContent = `已保存 ${pr.pushed} 项${pr.dir ? ` → ${pr.dir}/` : ""}`;
            if (pr.docTaskId && !opened) {
              opened = true;
              const open = document.createElement("button");
              open.textContent = "打开文章";
              S(open, {
                flex: "0 0 auto", background: "#f1f5f9", color: "#2563eb", border: "none",
                borderRadius: "7px", padding: "6px 10px", cursor: "pointer", fontWeight: "600",
              });
              open.addEventListener("click", () => {
                chrome.runtime
                  .sendMessage({ type: "nm", method: "task.open", params: { id: pr.docTaskId } })
                  .catch(() => {});
              });
              foot.append(open);
              setTimeout(closePanel, 8000); // 留足点击时间，不再 1.6s 抢关
              return;
            }
          } else {
            push.textContent = "推送失败（主程序未运行？）";
          }
        } catch (e) {
          push.textContent = "推送失败";
        }
        setTimeout(closePanel, 1600);
      });
      foot.append(push);
      panel.append(foot);
      } // end resources section
      document.documentElement.append(panel);
    });

    document.documentElement.append(btn);
  };

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", boot, { once: true });
  } else {
    boot();
  }
})();
