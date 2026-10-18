// 站点适配层 —— 契约与通用助手（docs/MEDIA-ADAPTERS.md §3）
// 加载环境：content script（ISOLATED）与 background（importScripts）共用。
// 契约：适配器 = 纯提取函数，输入 PageContext，输出 MediaResource[]。
//   MediaResource = { url, kind, filename, headers?, meta? }
//   PageContext   = { url, captured(key), inlineJson(name), dom() }
// 适配器不发下载、不碰推送；push 统一由 background 完成。
self.ADL = self.ADL || {};
ADL.adapters = ADL.adapters || {};

// —— 文件名清洗（与引擎 dl-filename 规则对齐的子集）
ADL.ILLEGAL = /[\\/:*?"<>|\u0000-\u001f]/g;
ADL.cleanName = (s, max = 80) =>
  String(s || "")
    .replace(ADL.ILLEGAL, " ")
    .replace(/\s+/g, " ")
    .trim()
    .slice(0, max)
    .replace(/[.\s]+$/, "") || "";

// —— URL 工具
ADL.toHttps = (u) => String(u || "").replace(/^http:\/\//i, "https://");
// xhs 水印后缀：.../xxx!nd_dft_wgtehjqv10hmmm → 去掉 ! 后缀即为无水印原图
ADL.stripWatermark = (u) => String(u || "").replace(/![^/?#]+(?=[?#]|$)/, "");

// —— 页面内嵌 JSON 提取（A 路线数据源）
// name 形如 "__INITIAL_STATE__" / "_ROUTER_DATA"；兼容 window.x / self.x 赋值。
ADL.findInlineJson = (name) => {
  if (typeof document === "undefined") return null;
  for (const sc of document.querySelectorAll("script:not([src])")) {
    const t = sc.textContent || "";
    const re = new RegExp(
      "(?:window|self)[.\\s]" + name.replace(/[$]/g, "\\$") + "\\s*=\\s*"
    );
    const m = re.exec(t);
    if (!m) continue;
    const start = t.indexOf("{", m.index + m[0].length - 1);
    if (start < 0) continue;
    const json = ADL._balanced(t, start);
    if (json == null) continue;
    const parsed = ADL.parseLooseJson(json);
    if (parsed != null) return parsed;
  }
  return null;
};

// 字符串感知的花括号配对，返回平衡的 JSON 子串
ADL._balanced = (t, start) => {
  let depth = 0, inStr = false, esc = false;
  for (let i = start; i < t.length; i++) {
    const c = t[i];
    if (inStr) {
      if (esc) esc = false;
      else if (c === "\\") esc = true;
      else if (c === '"') inStr = false;
      continue;
    }
    if (c === '"') inStr = true;
    else if (c === "{") depth++;
    else if (c === "}") {
      depth--;
      if (!depth) return t.slice(start, i + 1);
    }
  }
  return null;
};

// 宽松 JSON 解析：xhs 的 SSR JSON 含 undefined 字面量，替换为 null 后重试
ADL.parseLooseJson = (json) => {
  try {
    return JSON.parse(json);
  } catch (_) {}
  try {
    return JSON.parse(json.replace(/([:,[\s])undefined(?=[,}\]\s])/g, "$1null"));
  } catch (_) {
    return null;
  }
};

// —— 适配器注册表（float-button / 测试 / bridge 共用；background 另行 importScripts）
// articles 条目的 re 与 articles.js 的 ARTICLE_SITES 各站 re 的并集等价
// （此处静态写死：background 只加载本文件+core；scripts/test_articles.js 有交叉断言防漂移）
ADL.ARTICLES_RE =
  /^https?:\/\/(?:mp\.weixin\.qq\.com\/s|(?:[^/]*\.)?blog\.csdn\.net\/|devpress\.csdn\.net\/|zhuanlan\.zhihu\.com\/|(?:www\.)?zhihu\.com\/question\/\d+\/answer\/\d+|www\.bilibili\.com\/read\/|sspai\.com\/post\/|(?:www\.)?jianshu\.com\/(?:p|note)\/|juejin\.cn\/post\/|(?:[^/]*\.)?cnblogs\.com\/[^/]+\/p\/|blog\.51cto\.com\/|www\.jb51\.net\/(?:article|book)\/|www\.php\.cn\/|segmentfault\.com\/a\/|my\.oschina\.net\/|(?:[^/]*\.)?medium\.com\/)/i;
ADL.SITE_ADAPTERS = [
  { id: "wechat-article", label: "微信文章图片", re: /^https?:\/\/mp\.weixin\.qq\.com\//i },
  { id: "xhs", label: "小红书", re: /^https?:\/\/(www\.)?xiaohongshu\.com\//i },
  { id: "douyin", label: "抖音", re: /^https?:\/\/(www\.)?douyin\.com\//i },
  { id: "x", label: "X (Twitter)", re: /^https?:\/\/(www\.)?(x|twitter)\.com\//i },
  { id: "channels", label: "视频号", re: /^https?:\/\/channels\.weixin\.qq\.com\//i },
  { id: "articles", label: "文章", re: ADL.ARTICLES_RE },
];
ADL.matchAdapter = (url) =>
  ADL.SITE_ADAPTERS.find((a) => a.re.test(String(url || ""))) || null;
// 命中的全部适配器（微信文章页同时命中 wechat-article[图片] 与 articles[文章本体]，
// probe 时逐个提取合并——单匹配函数保留给旧调用方）
ADL.matchAdapters = (url) =>
  ADL.SITE_ADAPTERS.filter((a) => a.re.test(String(url || "")));
