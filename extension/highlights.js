// 划词标注（content script，文章命中页注入）——品类核心体验（对标简悦/Obsidian
// Clipper 的划词高亮）：
//   页内划选文字 → 浮出「✚ 标注」按钮 → 收入 chrome.storage.local
//   `adl_highlights`（按页面规范 URL 分组，去重、每页上限 50 条）。
// 入库路径：background.adapterPush 在推送文档前读同页标注，经
//   ADL.appendHighlights 追加「## 划线摘录」区块（actions.js 编辑器同源）。
// 本文件只做「采集与存储」，不碰 background 既有消息协议。
(() => {
  const STORE_KEY = "adl_highlights";
  const MAX_PER_PAGE = 50;
  const MAX_TEXT = 500;

  const canonical = () => location.href.split("#")[0];

  // 内存镜像：启动时载入一次，saveHighlight 同步更新——让同步消费方
  // （actions.js 的 articleActionEnv → 编辑/解析 Markdown）无需 await。
  const mem = {};
  (async () => {
    try {
      const all = await chrome.storage.local.get(STORE_KEY);
      Object.assign(mem, all[STORE_KEY] || {});
    } catch (_) {}
  })();
  // 本页标注（同步读；无标注返回 null）
  if (typeof window.ADL === "object" && window.ADL) {
    window.ADL.pageHighlights = () => {
      const l = mem[canonical()];
      return l && l.length ? l : null;
    };
  }

  const selText = () =>
    String(window.getSelection ? window.getSelection() : "").replace(/\s+/g, " ").trim();

  async function saveHighlight(text) {
    const all = await chrome.storage.local.get(STORE_KEY);
    const map = { ...(all[STORE_KEY] || {}) };
    const key = canonical();
    const list = [...(map[key] || [])];
    if (list.some((h) => h.text === text)) return { ok: true, dup: true, count: list.length };
    list.push({ text: text.slice(0, MAX_TEXT), at: Date.now() });
    while (list.length > MAX_PER_PAGE) list.shift();
    map[key] = list;
    await chrome.storage.local.set({ [STORE_KEY]: map });
    mem[key] = list;
    return { ok: true, count: list.length };
  }

  // —— 浮动按钮：跟随选区，保存标注，点击别处/再选自动消失
  let btn = null;
  function hideBtn() {
    if (btn) {
      btn.remove();
      btn = null;
    }
  }
  function showBtn(rect, text) {
    hideBtn();
    btn = document.createElement("button");
    btn.type = "button";
    btn.id = "adl-hl-btn";
    btn.textContent = "✚ 标注";
    btn.style.cssText =
      "position:fixed;z-index:2147483647;padding:6px 12px;border-radius:8px;" +
      "background:#2865d3;color:#fff;font:600 12px/1.4 system-ui,sans-serif;" +
      "cursor:pointer;box-shadow:0 6px 18px #18243833;border:0;user-select:none;";
    btn.addEventListener("mousedown", (e) => e.stopPropagation());
    btn.addEventListener("click", async (e) => {
      e.stopPropagation();
      const buttonStyle = btn.style.cssText;
      hideBtn();
      let r;
      try { r = await saveHighlight(text); }
      catch (_) {
        const error = document.createElement("div");
        error.setAttribute("role", "alert");
        error.textContent = "标注保存失败，请重试。";
        error.style.cssText = buttonStyle + "background:#b83a3a;cursor:default;";
        error.style.left = Math.max(8, rect.left) + "px";
        error.style.top = Math.max(8, rect.top - 38) + "px";
        document.documentElement.appendChild(error);
        setTimeout(() => error.remove(), 3000);
        return;
      }
      const tip = document.createElement("div");
      tip.id = "adl-hl-tip";
      tip.textContent = r && r.dup ? "标注已存在" : `已保存标注（本页 ${r && r.count || 1} 条）`;
      tip.setAttribute("role", "status");
      tip.style.cssText = buttonStyle + "background:#217d5c;cursor:default;";
      tip.style.left = rect.left + "px";
      tip.style.top = rect.top - 8 + "px";
      document.documentElement.appendChild(tip);
      setTimeout(() => tip.remove(), 1400);
    });
    document.documentElement.appendChild(btn);
    const w = 76;
    btn.style.left = Math.max(8, Math.min(rect.left + rect.width / 2 - w / 2, window.innerWidth - w - 8)) + "px";
    btn.style.top = Math.max(8, rect.top - 38) + "px";
  }

  document.addEventListener("mouseup", () => {
    // 等选区稳定（点击按钮本身时 selText 已空，btn 自行隐藏）
    setTimeout(() => {
      if (document.getElementById("adl-hl-btn")) return;
      const text = selText();
      if (!text || text.length < 2) {
        hideBtn();
        return;
      }
      const sel = window.getSelection();
      const range = sel && sel.rangeCount ? sel.getRangeAt(0).getBoundingClientRect() : null;
      if (range && (range.width || range.height)) showBtn(range, text);
    }, 10);
  });
  document.addEventListener("mousedown", (e) => {
    if (e.target && e.target.id !== "adl-hl-btn") hideBtn();
  });
  window.addEventListener("scroll", hideBtn, { passive: true });
})();
