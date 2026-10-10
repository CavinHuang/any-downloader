// 文章同步提取器（content script，ISOLATED world）—— 只承担「必须 DOM」的
// 提取一环（code-box contents/sync.ts:59-89 的分工：扩展页发消息，这里回文本）。
// 消息协议（本文件自持 onMessage，不触碰 background.js 既有分发、bridge.js 的
// adapter-extract）：
//   adl-sync-extract  → 复用 articles.js 的 ADL.articlePrepare + ADL.turndown
//                       （buildMarkdown 的底层引擎）提取正文，返回
//                       { title, html, markdown(纯正文，无 front matter), cover, ... }
//   adl-sync-cookies  → 回 document.cookie（博客园 XSRF-TOKEN 的内容脚本降级读法，
//                       见 platforms.js cnblogsXsrfToken；httpOnly 时读不到）
// 未知/非 adl-sync-* 消息一律不响应，与既有协议零干扰。
(() => {
  // meta 取值（og:image 封面，code-box extension/extractor.ts:286 同源）
  function metaContent(sel) {
    try {
      const el = document.querySelector(sel);
      return el ? el.getAttribute("content") : null;
    } catch (_) {
      return null;
    }
  }

  function extractArticle() {
    if (!self.ADL || !ADL.articlePrepare) {
      return { ok: false, error: "文章适配器未加载（请刷新页面后重试）" };
    }
    const ctx = {
      url: location.href,
      captured: () => [],
      inlineJson: () => null,
      dom: () => document,
    };
    const p = ADL.articlePrepare(ctx);
    if (!p) {
      return { ok: false, error: "本页未命中文章站点选择器，无法提取" };
    }
    // 纯正文 Markdown（无 YAML front matter / H1 —— 平台草稿由页面标题字段承载，
    // 故不直接用 ADL.buildMarkdown 的成品，而用其底层 ADL.turndown）
    const markdown = ADL.turndown ? ADL.turndown(p.content, { baseUrl: p.pageUrl }) : "";
    const cover =
      metaContent('meta[property="og:image"]') || metaContent('meta[name="twitter:image"]') || "";
    return {
      ok: true,
      article: {
        title: p.title,
        html: p.content.outerHTML,
        markdown,
        cover,
        url: p.pageUrl,
        siteId: p.site.id,
        siteLabel: p.site.label,
        wordCount: String(markdown).replace(/\s+/g, "").length,
      },
    };
  }

  chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
    if (!msg || typeof msg.type !== "string" || msg.type.indexOf("adl-sync-") !== 0) {
      return; // 非本子系统消息：不响应、不拦截
    }
    if (msg.type === "adl-sync-cookies") {
      sendResponse({ ok: true, cookie: document.cookie });
      return;
    }
    if (msg.type === "adl-sync-extract") {
      // 提取含 DOM 遍历与 turndown 转换，走异步避免阻塞消息通道
      Promise.resolve()
        .then(() => extractArticle())
        .then(sendResponse)
        .catch((e) => sendResponse({ ok: false, error: String((e && e.stack) || e) }));
      return true; // 异步 sendResponse
    }
    return; // 未知 adl-sync-* 子类型
  });
})();
