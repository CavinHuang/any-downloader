// bridge —— ISOLATED world 桥接层（document_start）：
//   1. MAIN world 挂钩器 → postMessage → 这里 → background 环形缓冲
//   2. background 的 adapter-extract 请求 → 调用适配器提取 → 返回资源清单
// 适配器文件（adapters/*.js）在本文件之前加载，挂在 self.ADL 命名空间。
(() => {
  // 1) 挂钩捕获转发
  window.addEventListener("message", (ev) => {
    if (ev.source !== window) return;
    const m = ev.data;
    if (!m || m.source !== "adl-hook") return;
    try {
      chrome.runtime
        .sendMessage({ type: "hook-capture", key: m.key, url: m.url, data: m.data })
        .catch(() => {});
    } catch (_) {}
  });

  // 2) 提取请求：background 已按 URL 选好适配器并附带捕获切片。
  //    adapterIds 为数组（一页可命中多个适配器：微信文章页 = wechat-article 图片
  //    + articles 文章本体）；兼容旧单值 adapterId。逐个提取，单个失败不拖垮整页。
  chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
    if (msg.type !== "adapter-extract") return;
    const reg = (self.ADL && self.ADL.adapters) || {};
    const ids = msg.adapterIds || (msg.adapterId ? [msg.adapterId] : []);
    if (!ids.length || !ids.every((id) => reg[id])) {
      sendResponse({ ok: false, error: "adapter_missing" });
      return;
    }
    try {
      const ctx = {
        url: location.href,
        captured: (key) => (msg.captured && msg.captured[key]) || [],
        inlineJson: (name) => ADL.findInlineJson(name),
        dom: () => document,
      };
      const resources = [];
      for (const id of ids) {
        try {
          resources.push(...(reg[id].extract(ctx) || []));
        } catch (e) {
          console.warn("[adl] adapter " + id + " extract failed:", e);
        }
      }
      sendResponse({ ok: true, resources, source: reg[ids[0]].lastSource || "hook" });
    } catch (e) {
      sendResponse({ ok: false, error: String((e && e.stack) || e) });
    }
  });
})();
