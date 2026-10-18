// 文章操作菜单（对齐 code-box 微信面板，docs/MEDIA-ADAPTERS.md §12）：
//   文章同步 / 获取封面 / 文章摘要 / 净化链接 / 原始链接 / 编辑 Markdown /
//   下载 Markdown / 解析 Markdown / 打包下载(MD+图片) / 下载 PDF / 下载 Word /
//   下载所有图片 / 生成海报
// code-box 用户可见行为对齐（v0.8 轮，参考 weixinPanel.tsx / weixin.tsx / tools.ts /
//   parseMarkdownHook.ts / makerQRPost.ts / background index.ts）：
//   - 净化链接 = 复制后不经确认直接导航；参数白名单制 + 规则数组序重拼
//   - 获取封面 = og:image 新标签直接打开原图
//   - 下载 Markdown = 浏览器本地 Blob 保存，文件名 `标题-YYYY-MM-DD HH:mm:ss.md`
//     （微信保存前预滚动触发懒图）
//   - 解析 Markdown = MD+指令写 storage["ai-content"] → 按 storage["app-aiType"]
//     直接打开 AI 站点（默认 chatgpt）；微信特例 = 图片 base64 内联后本地存 .md
//   - 下载所有图片 = 本地 zip 落盘（图片字节 background 抓取，lib/zip.js 打包）
//   - 生成海报 = 本地 canvas 360 宽渐变卡 + 文章链接二维码（makerQRPost 版式 + lib/qr.js）
//   - 文章同步 = 经 background open-sync 打开 sync.html?tabId=（weixinPanel.tsx:137-139
//     → background index.ts:143-149 同款工作流；无 tabId 时同步页自动选中并提取）
// 有意偏差（评审登记，均为增强/管线一致性取舍）：
//   - 资源列表「推送」与「打包下载(MD+图片)」继续走 adapter-push 进应用；
//     原始链接保留真实 __biz/mid/sn 重构（code-box 为空占位）；
//     封面/摘要/MD 缺失时 toast 提示（code-box 静默无响应）
//   - ⑤下载 Markdown 产物 = YAML front matter + H1 标题 + 正文（adapters/articles.js
//     buildMarkdown，与推送管线 article.md 同源），code-box 为纯正文转换（tools.ts:60-66）
//   - ⑥解析 Markdown：a) app-aiType 未设置/未知时默认打开 chatgpt（code-box switch
//     无 default 分支不打开任何站点；我方对齐其 options 页展示默认值 setAiType.tsx:9-11，
//     未设置用户点击有反馈而非无响应）；b) 额外把 MD+指令复制到剪贴板（code-box 只写
//     storage["ai-content"]，该键在 code-box 全库无消费方，剪贴板是内容传递的实质通道）
// 通信：push / nm / adapter-probe / adapter-push / zip-download（图片 zip 落盘）/
//   open-sync（打开文章同步页，携带来源 tabId）。
(() => {
  const attr = (el, name) => {
    try {
      return el.getAttribute(name);
    } catch (_) {
      return null;
    }
  };

  // ---------------------------------------------------------------- UI 基件

  const uiTheme = () => window.matchMedia("(prefers-color-scheme: dark)").matches
    ? { panel: "#242e27", text: "#e3e7d9", muted: "#a0ad9b", border: "#3b463c", accent: "#bdd29c", ink: "#22301f", soft: "#34432f", accentInk: "#c1d5aa" }
    : { panel: "#fbfaf6", text: "#293c34", muted: "#687263", border: "#dedfd5", accent: "#344e3c", ink: "#fbfaf6", soft: "#e9eddf", accentInk: "#36543e" };

  // 轻提示（1.8s 自动消失）
  function toast(msg) {
    const theme = uiTheme();
    const t = document.createElement("div");
    t.textContent = msg;
    t.style.cssText =
      "position:fixed;left:50%;top:24px;transform:translateX(-50%);z-index:2147483647;" +
      `background:${theme.panel};color:${theme.text};border:1px solid ${theme.border};font:13px/1.5 system-ui,sans-serif;padding:10px 16px;` +
      "border-radius:7px;box-shadow:0 8px 28px rgba(35,49,38,.2);";
    document.documentElement.appendChild(t);
    setTimeout(() => t.remove(), 1800);
  }

  // 模态：返回 { close }。buildBody(body, close) 负责填充内容。
  function modal(title, buildBody) {
    const theme = uiTheme();
    const opener = document.activeElement;
    const mask = document.createElement("div");
    mask.style.cssText =
      "position:fixed;inset:0;z-index:2147483647;background:rgba(24,39,31,.4);backdrop-filter:blur(3px);";
    const box = document.createElement("div");
    box.style.cssText =
      "position:absolute;left:50%;top:50%;transform:translate(-50%,-50%);" +
      "width:min(720px,92vw);max-height:86vh;display:flex;flex-direction:column;" +
      `background:${theme.panel};color:${theme.text};border:1px solid ${theme.border};border-radius:12px;overflow:hidden;` +
      "box-shadow:0 16px 48px rgba(35,49,38,.2);font:14px/1.6 system-ui,sans-serif;";
    box.setAttribute("role", "dialog");
    box.setAttribute("aria-modal", "true");
    box.setAttribute("aria-label", title);
    const head = document.createElement("div");
    head.style.cssText =
      `display:flex;align-items:center;padding:21px 24px 17px;border-bottom:1px solid ${theme.border};font:400 25px/1.5 SimSun,Georgia,serif;`;
    head.textContent = title;
    const x = document.createElement("button");
    x.type = "button";
    x.setAttribute("aria-label", "关闭");
    x.textContent = "✕";
    x.style.cssText = `margin-left:auto;cursor:pointer;color:${theme.muted};padding:0;width:30px;height:30px;border:0;background:transparent;font:16px/1 system-ui,sans-serif;`;
    x.addEventListener("click", close);
    head.append(x);
    const body = document.createElement("div");
    body.style.cssText = "padding:20px 24px;overflow:auto;";
    box.append(head, body);
    mask.append(box);
    mask.addEventListener("click", (e) => {
      if (e.target === mask) close();
    });
    function close() {
      mask.remove();
      if (opener && opener.isConnected) opener.focus({ preventScroll: true });
    }
    mask.addEventListener("keydown", e => {
      if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); close(); }
      if (e.key !== "Tab") return;
      const elements = [...box.querySelectorAll('button:not(:disabled),textarea:not(:disabled),input:not(:disabled),a[href]')].filter(el => el.getClientRects().length);
      const first = elements[0], last = elements[elements.length - 1];
      if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
      else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
    });
    document.documentElement.append(mask);
    buildBody(body, close);
    x.focus();
    return { close };
  }

  function btn(label, primary) {
    const theme = uiTheme();
    const b = document.createElement("button");
    b.textContent = label;
    b.style.cssText =
      "border:none;border-radius:7px;padding:7px 14px;cursor:pointer;font-weight:600;" +
      "font:12px/1.6 system-ui,sans-serif;" +
      "background:" + (primary ? `${theme.accent};color:${theme.ink}` : `${theme.soft};color:${theme.accentInk}`) + ";";
    return b;
  }

  function copyText(text) {
    const done = () => toast("已复制");
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(text).then(done, () => fallbackCopy(text, done));
    } else {
      fallbackCopy(text, done);
    }
  }
  function fallbackCopy(text, done) {
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.style.cssText = "position:fixed;opacity:0;";
    document.body.appendChild(ta);
    ta.select();
    try {
      document.execCommand("copy");
    } catch (_) {}
    ta.remove();
    done();
  }

  // 文本模态（只读或可编辑）+ 可选动作按钮
  function textModal(title, text, opts) {
    const o = opts || {};
    modal(title, (body, close) => {
      const theme = uiTheme();
      const ta = document.createElement("textarea");
      ta.value = text;
      ta.readOnly = !!o.readOnly;
      ta.spellcheck = false;
      ta.style.cssText =
        "width:100%;height:" + (o.height || "56vh") + `;resize:vertical;border:1px solid ${theme.border};background:${theme.panel};` +
        `border-radius:6px;padding:14px;font:13px/1.8 ui-monospace,Consolas,monospace;color:${theme.text};`;
      const foot = document.createElement("div");
      foot.style.cssText = "display:flex;gap:8px;margin-top:10px;flex-wrap:wrap;";
      const cp = btn("复制", true);
      cp.addEventListener("click", () => copyText(ta.value));
      foot.append(cp);
      if (o.downloadName) {
        const dl = btn("下载文件");
        dl.addEventListener("click", () => {
          const blob = new Blob([ta.value], { type: o.downloadMime || "text/plain;charset=utf-8" });
          const a = document.createElement("a");
          a.href = URL.createObjectURL(blob);
          a.download = o.downloadName;
          a.click();
          setTimeout(() => URL.revokeObjectURL(a.href), 5000);
        });
        foot.append(dl);
      }
      for (const extra of o.actions || []) {
        const b = btn(extra.label);
        b.addEventListener("click", () => {
          if (extra.copy) copyText(ta.value);
          if (extra.url) window.open(extra.url, "_blank");
        });
        foot.append(b);
      }
      const hint = document.createElement("span");
      hint.style.cssText = `margin-left:auto;align-self:center;color:${theme.muted};font-size:11px;`;
      hint.textContent = o.hint || "";
      foot.append(hint);
      body.append(ta, foot);
      if (!o.readOnly) ta.focus();
    });
  }

  // ---------------------------------------------------------------- 链接工具

  // 净化链接规则表（code-box weixinPanel.tsx:63-73 同款，先匹配先赢）：
  //   微信规则仅命中 mp.weixin.qq.com/s? 文章 URL（非全站）；
  //   通用规则命中任何带 query/hash 的 URL，白名单外的参数一律删除。
  const PURE_RULES = [
    {
      testReg: /^http(?:s)?:\/\/mp\.weixin\.qq\.com\/s\?.*$/i,
      query: ["__biz", "idx", "mid", "sn", "src", "timestamp", "ver", "signature"],
    },
    {
      testReg: /^(http(?:s)?:\/\/[^?#]*)[?#].*$/i,
      query: ["id", "tid", "uid", "q", "wd", "query", "keyword", "keywords"],
    },
  ];

  // 净化 URL（纯函数）：base 去掉 ? 与 # 之后的部分，保留参数按规则数组顺序
  // 重拼（每个参数原样携带 key=value，不做解码/再编码）；白名单一个都没命中
  // 则无 ?；末尾拼回原 hash（weixinPanel.tsx:74-99）。
  ADL.actionCleanUrl = (url) => {
    const s = String(url || "");
    const hash = s.replace(/^[^#]*(#.*)?$/, "$1");
    const base = s.replace(/(\?|#).*$/, "");
    let pureUrl = s;
    const getQuery = (key) => {
      const m = s.match(new RegExp("(?:\\?|&)(" + key + "=[^&#]*)", "i"));
      return m === null ? "" : m[1];
    };
    for (const rule of PURE_RULES) {
      if (rule.testReg.test(s)) {
        let newQuerys = "";
        if (rule.query && rule.query.length > 0) {
          for (const key of rule.query) {
            const ret = getQuery(key);
            if (ret !== "") newQuerys += (newQuerys.length ? "&" : "?") + ret;
          }
        }
        newQuerys += hash;
        pureUrl = base + newQuerys;
        break;
      }
    }
    return pureUrl;
  };

  // 原始链接：微信由 __biz/mid/sn 重构规范形态；其他站 og:url / canonical 兜底
  ADL.actionOriginalUrl = (url, doc) => {
    try {
      const u = new URL(url);
      if (/(^|\.)mp\.weixin\.qq\.com$/i.test(u.hostname)) {
        const q = u.searchParams;
        const biz = q.get("__biz");
        const mid = q.get("mid") || q.get("__mid");
        const sn = q.get("sn") || q.get("__sn");
        if (biz && mid && sn) {
          return `https://mp.weixin.qq.com/s?__biz=${biz}&mid=${mid}&idx=${q.get("idx") || 1}&sn=${sn}`;
        }
      }
    } catch (_) {}
    for (const sel of ['meta[property="og:url"]', 'link[rel="canonical"]']) {
      const el = doc && doc.querySelector(sel);
      const v =
        el &&
        (String(el.nodeName).toUpperCase() === "META" ? attr(el, "content") : attr(el, "href"));
      if (v && /^https?:\/\//i.test(v)) return v;
    }
    return url;
  };

  // ------------------------------------------------ 本地保存与对齐助手（⑤⑥⑦⑧）
  // code-box 的「本地保存」= file-saver saveAs(blob, name)（tools.ts:2,60-66），
  // 用户可见效果 = 浏览器下载栏出现该文件；零依赖等价 = Blob + a[download]。

  // dayjs().format("YYYY-MM-DD HH:mm:ss") 的零依赖等价（tools.ts:64 时间戳格式）
  ADL.formatTimestamp = (d) => {
    const t = d instanceof Date ? d : new Date();
    const p = (n) => String(n).padStart(2, "0");
    return (
      `${t.getFullYear()}-${p(t.getMonth() + 1)}-${p(t.getDate())} ` +
      `${p(t.getHours())}:${p(t.getMinutes())}:${p(t.getSeconds())}`
    );
  };

  // ⑤/⑥微信特例的 .md 文件名：`标题-YYYY-MM-DD HH:mm:ss.md`（tools.ts:64 同款）
  ADL.mdFileName = (title) => `${title}-${ADL.formatTimestamp()}.md`;

  function saveBlobLocal(content, mime, filename) {
    const blob = new Blob([content], { type: mime });
    const a = document.createElement("a");
    a.href = URL.createObjectURL(blob);
    a.download = filename;
    a.click();
    setTimeout(() => URL.revokeObjectURL(a.href), 5000);
  }

  // 微信文章页判定（actions 的微信特例分叉用；适配器站点表为准）
  function isWeixinPage() {
    if (ADL.articleSite) {
      const site = ADL.articleSite(location.href);
      if (site) return site.id === "wechat-article";
    }
    return /^https?:\/\/mp\.weixin\.qq\.com\//i.test(location.href);
  }

  // 微信预滚动加载懒图（code-box weixin.tsx:251-276 同款）：每 100ms 下滚
  // 300px 至底部，等 500ms，滚回顶部。无 #js_content（非微信文章页）时即返。
  function scrollToLoadImages() {
    const article = document.querySelector("#js_content");
    if (!article) return Promise.resolve();
    return new Promise((resolve) => {
      const scrollHeight = document.documentElement.scrollHeight;
      const clientHeight = document.documentElement.clientHeight;
      const scrollStep = 300; // 每次滚动的距离
      let currentScroll = 0;
      const scrollInterval = setInterval(() => {
        currentScroll += scrollStep;
        window.scrollTo(0, currentScroll);
        if (currentScroll >= scrollHeight - clientHeight) {
          clearInterval(scrollInterval);
          // 等待图片加载
          setTimeout(() => {
            window.scrollTo(0, 0); // 滚回顶部
            resolve();
          }, 500);
        }
      }, 100);
    });
  }

  // ⑥解析 Markdown 的 AI 指令原文（parseMarkdownHook.ts:20-21：一个换行 +
  // 4 个空格缩进 + 指令，无结尾换行）
  ADL.AI_INSTRUCTION = "\n    将上面的文字,翻译成中文并生成markdown";

  // AI 站点映射（parseMarkdownHook.ts:26-36）；未设置/未知类型默认 chatgpt
  //（code-box options 页下拉默认值，setAiType.tsx:9-11）
  const AI_SITES = {
    kimi: "https://kimi.moonshot.cn/",
    chatgpt: "https://chatgpt.com",
    deepseek: "https://chat.deepseek.com/",
  };
  const aiSiteUrl = (type) => AI_SITES[type] || AI_SITES.chatgpt;

  // —— ⑦下载所有图片：content 侧收图并定名（字节由 background 抓取打包）

  // 微信版扩展名映射（weixin.tsx:199-215）：wx_fmt/mmbiz 的 gif|png|bmp，
  // 其余一律 .jpg（无 webp 分支）
  ADL.weixinImageExt = (url) => {
    if (url.includes("wx_fmt=gif") || url.includes("mmbiz_gif")) return ".gif";
    if (url.includes("wx_fmt=png") || url.includes("mmbiz_png")) return ".png";
    if (url.includes("wx_fmt=bmp") || url.includes("mmbiz_bmp")) return ".bmp";
    return ".jpg";
  };

  // 通用站条目名（code-box background/messages/download.ts:14,23）：URL 路径
  // 最后一段去 query，空则 image-<index>.jpg
  ADL.genericImageName = (url, index) => {
    const name = String(url).split("/").pop().split("?")[0];
    return name || `image-${index}.jpg`;
  };

  // 收集图片任务 [{name, url}]：微信 = #js_content 内 img 的 dataset.src||src，
  // 条目名 `<标题>-<下标><ext>`（下标为过滤后序号，失败张的序号位随 background
  // 跳过保留）；通用站 = 全页 document.images（custom.tsx:79），http(s) 去重
  // （jszip 重名条目互相覆盖的等价语义）。
  function collectImageJobs() {
    const title = String(document.title || "").trim();
    if (isWeixinPage()) {
      const article = document.querySelector("#js_content");
      if (!article) return [];
      return Array.from(article.getElementsByTagName("img"))
        .map((img) => img.dataset.src || img.src)
        // data: 占位图直接剔除：字节抓取在 MV3 service worker（background
        // zipDownload），SW 内 fetch 仅支持 http/https，data: 任务必失败
        //（code-box 在页面上下文 fetch data: 可成功，weixin.tsx:196——上下文不同）
        .filter((url) => url && !/^data:/i.test(url))
        // 协议相对 //host/... 统一补全 https:（含 weixin.tsx:176-188 的
        // mmbizwap 特例）：SW 内相对 URL 以 worker 脚本 URL
        //（chrome-extension://<id>/）为基址解析，//host → chrome-extension://host
        // 必失败；code-box 在页面上下文 fetch，按页面 https 基址解析无此问题
        .map((url) => (url.startsWith("//") ? "https:" + url : url))
        .map((url, i) => ({ name: `${title}-${i}${ADL.weixinImageExt(url)}`, url }));
    }
    const seen = new Set();
    const jobs = [];
    for (const img of document.images) {
      const url = img.src || "";
      if (!/^https?:/i.test(url) || seen.has(url)) continue;
      seen.add(url);
      jobs.push({ name: ADL.genericImageName(url, jobs.length), url });
    }
    return jobs;
  }
  // 导出给测试（scripts/test_articles.js 桩驱动）：协议补全与 data: 过滤是
  // 「下载所有图片」zip 抓取成败的关键前置，两条路径均在 SW 上下文受限
  ADL.collectImageJobs = collectImageJobs;

  // ⑥微信特例：Markdown 图片全部转 base64 内联（tools.ts:68-126 同款：
  // ![alt](url) 全量收集 → 逐张 fetch→blob→FileReader dataURL → 转义后全局
  // 替换 `(${url})`；单图 fetch 失败保留原 URL，不阻塞其余图片）
  ADL.inlineMarkdownImages = (markdown) => {
    const imageRegex = /!\[([^\]]*)\]\(([^)]+)\)/g;
    const images = [];
    let match;
    while ((match = imageRegex.exec(markdown)) !== null) {
      images.push({ url: match[2] });
    }
    if (images.length === 0) return Promise.resolve(markdown);
    const fetchBase64 = (url) =>
      fetch(url)
        .then((resp) => resp.blob())
        .then(
          (blob) =>
            new Promise((resolve) => {
              const reader = new FileReader();
              reader.onloadend = () => resolve(String(reader.result || url));
              reader.onerror = () => resolve(url);
              reader.readAsDataURL(blob);
            })
        )
        .catch(() => url); // 失败保持原 URL（tools.ts:107）
    return Promise.all(
      images.map((img) => fetchBase64(img.url).then((base64) => ({ url: img.url, base64 })))
    ).then((downloaded) => {
      let out = markdown;
      for (const img of downloaded) {
        out = out.replace(
          new RegExp(`\\(${img.url.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\)`, "g"),
          `(${img.base64})`
        );
      }
      return out;
    });
  };

  // ---------------------------------------------------------------- 导出 HTML
  // 仅供 PDF 打印 / Word 落盘（非文档产物）；基线阅读样式，无 TOC/脚本。

  function escHtml(s) {
    return String(s || "")
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;");
  }

  const BASE_CSS =
    "body{margin:0;padding:24px;font:14pt/1.8 'Segoe UI','PingFang SC','Microsoft YaHei',serif;color:#111;}" +
    "main{max-width:760px;margin:0 auto;}h1{font-size:22pt;line-height:1.4;}" +
    "img{max-width:100%;height:auto;}pre{background:#f6f8fa;padding:12px;overflow:auto;" +
    "font:11pt/1.6 Consolas,monospace;white-space:pre-wrap;word-wrap:break-word;}" +
    "code{font-family:Consolas,monospace;}blockquote{border-left:4px solid #dfe2e5;" +
    "margin:12px 0;padding:4px 16px;color:#555;}table{border-collapse:collapse;width:100%;}" +
    "th,td{border:1px solid #ccc;padding:6px 10px;}a{color:#0366d6;}";

  ADL.actionBuildPrintHtml = (env) =>
    `<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<title>${escHtml(env.title)}</title>
<style>${BASE_CSS}@page{margin:18mm;}</style>
</head>
<body>
<main>
<h1>${escHtml(env.title)}</h1>
<p style="color:#6a737d;font-size:10pt;">${escHtml(env.site.label)} · ${escHtml(env.pageUrl)}</p>
${env.contentEl.outerHTML}
</main>
</body>
</html>
`;

  ADL.actionBuildWordHtml = (env) => {
    const date = new Date().toISOString().slice(0, 10);
    return `<html xmlns:o="urn:schemas-microsoft-com:office:office"
xmlns:w="urn:schemas-microsoft-com:office:word"
xmlns="http://www.w3.org/TR/REC-html40">
<head>
<meta charset="utf-8">
<title>${escHtml(env.title)}</title>
<style>
body { font-family: '宋体', SimSun, serif; font-size: 14pt; line-height: 1.8; padding: 20px; }
img { max-width: 100%; height: auto; }
h1, h2, h3, h4 { font-family: '黑体', SimHei, sans-serif; }
pre, code { font-family: 'Courier New', monospace; font-size: 12pt; }
table { border-collapse: collapse; width: 100%; }
td, th { border: 1px solid #ccc; padding: 6px; }
a { color: #576b95; }
</style>
</head>
<body>
<h1>${escHtml(env.title)}</h1>
<p style="color:#666;font-size:10pt;">${escHtml(env.site.label)} · ${escHtml(env.pageUrl)} · 导出于 ${date}</p>
${env.contentEl.outerHTML}
</body>
</html>`;
  };

  // ---------------------------------------------------------------- 海报
  // code-box 本地海报版（utils/makerQRPost.ts:53-250）版式对齐：
  //   360 宽垂直渐变卡（#2980B9→#6DD5FA→#FFFFFF）+ 底部白色装饰带（两圆两矩形）
  //   + 居中标题（Bold 18px Microsoft YaHei）+ 左对齐摘要（16px）
  //   + 288 白色底板上的 260px 文章链接二维码（extension/lib/qr.js，安静区 4 模块）
  //   + 两行灰色提示。保存 PNG，文件名 = 页面标题 + ".png"（无时间戳无后缀词）。
  // 有意偏差：QR 纠错级为 lib 的 M 级（ADLQR 不支持 code-box 的 Q）；
  //   无 200ms×30 重试循环（本地编码器同步出码，无外部库加载，lib 缺失按
  //   code-box 同款文案提示中止）；摘要保留正文首段兜底（summary 为空时
  //   跳过摘要区，高度公式含条件项）；品牌提示行替换为我方文案。

  // 画布高度公式（makerQRPost.ts:133-138）
  ADL.posterCardHeight = (titleLines, descLines) =>
    36 * 2.5 + 18 * titleLines * 1.6 + 16 * descLines * 1.6 + (descLines ? 36 / 2 : 0) + 300;

  // 逐字符 measureText 折行（makerQRPost.ts:60-81 同款算法；ctx 可注入便于单测）
  ADL.posterWrapText = (text, style, width, ctx) => {
    if (String(text).replace(/\s/g, "").length === 0) return [];
    ctx.font = style;
    let temp = 0;
    const row = [];
    for (let i = 0; i <= text.length; i++) {
      if (ctx.measureText(text.substring(temp, i)).width >= width) {
        row.push(text.substring(temp, i - 1));
        temp = i - 1;
      } else if (i === text.length) {
        row.push(text.substring(temp, i));
      }
    }
    return row;
  };

  ADL.actionMakePoster = (env, done) => {
    const cardWidth = 360;
    const lineHeight = 1.6;
    const margin = 36;
    const titleStyle = 'Bold 18px "Microsoft YaHei"';
    const descStyle = '16px "Microsoft YaHei"';
    const title = String(env.title || "");
    const desc = String(env.summary || "");

    // 二维码：内容 = 当前页 URL（makerQRPost.ts:226-233）。lib 缺失按 code-box
    // 载入失败文案中止；编码失败（如链接超出容量）仅告警并中止海报。
    if (typeof self.ADLQR === "undefined" || !self.ADLQR) {
      toast("没能成功载入功能库，请刷新重试~");
      return;
    }
    let matrix = null;
    try {
      matrix = self.ADLQR.encode(location.href);
    } catch (e) {
      console.warn("[adl] 海报二维码编码失败：", e);
      toast("二维码生成失败（链接过长？），海报中止");
      return;
    }

    const canvas = document.createElement("canvas");
    canvas.id = "qrcodepostmaker";
    canvas.style.display = "none";
    document.body.appendChild(canvas);
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    // 绘制文本（makerQRPost.ts:84-104：逐行 y += fontSize*lineHeight 后 fillText）
    const textWriter = (textArr, style, align, color, borderColor, fontSize, lh, pos) => {
      ctx.font = style;
      ctx.textAlign = align;
      ctx.fillStyle = color;
      ctx.strokeStyle = borderColor;
      for (const text of textArr) {
        pos.y += fontSize * lh;
        ctx.fillText(text, pos.x, pos.y);
      }
      return pos;
    };

    const titleArray = ADL.posterWrapText(title, titleStyle, cardWidth - margin * 2, ctx);
    const descArray = ADL.posterWrapText(desc, descStyle, cardWidth - margin * 2, ctx);
    const cardHeight = ADL.posterCardHeight(titleArray.length, descArray.length);
    canvas.width = cardWidth;
    canvas.height = cardHeight;

    // 垂直线性渐变背景（makerQRPost.ts:143-149）
    const myGradient = ctx.createLinearGradient(0, 0, 0, cardHeight);
    myGradient.addColorStop(0, "#2980B9");
    myGradient.addColorStop(0.5, "#6DD5FA");
    myGradient.addColorStop(1, "#FFFFFF");
    ctx.fillStyle = myGradient;
    ctx.fillRect(0, 0, cardWidth, cardHeight);
    ctx.fillStyle = "#FFFFFF";
    // 底部白色装饰带：两个白圆 + 两个白矩形盖住左半/右半底部
    ctx.beginPath();
    ctx.arc(margin * 0.4, cardHeight - margin * 5, margin, 0, Math.PI * 2, true);
    ctx.arc(cardWidth - margin * 0.4, cardHeight - margin * 3, margin * 0.8, 0, Math.PI * 2, true);
    ctx.fillRect(0, cardHeight - margin * 5, cardWidth / 2, margin * 5);
    ctx.fillRect(cardWidth / 2, cardHeight - margin * 3, cardWidth / 2, margin * 3);
    ctx.fill();

    // 标题居中 → 摘要左对齐（起笔 y = 标题末行 y + margin/2）
    const titleEnd = textWriter(
      titleArray,
      titleStyle,
      "center",
      "black",
      "rgba(255, 255, 255, 0.6)",
      18,
      lineHeight,
      { x: cardWidth / 2, y: margin }
    );
    const descEnd = textWriter(
      descArray,
      descStyle,
      "left",
      "black",
      "rgba(255, 255, 255, 0.6)",
      16,
      lineHeight,
      { x: margin, y: titleEnd.y + margin / 2 }
    );

    // 白色二维码底板 288×288 + 260px 二维码贴入（makerQRPost.ts:194-196 的
    // getImageData/putImageData 等价绘制：node-qrcode 默认 4 模块安静区同款留白）
    ctx.fillStyle = "white";
    ctx.fillRect((cardWidth - 288) / 2, descEnd.y + margin - 24, 288, 288);
    const qrSize = matrix.length;
    const cell = 260 / (qrSize + 8);
    const qrX = (cardWidth - 256) / 2;
    const qrY = Math.floor(descEnd.y + margin);
    ctx.fillStyle = "#ffffff";
    ctx.fillRect(qrX, qrY, 260, 260);
    ctx.fillStyle = "#000000";
    for (let r = 0; r < qrSize; r++) {
      for (let c = 0; c < qrSize; c++) {
        if (matrix[r][c]) {
          ctx.fillRect(
            qrX + (4 + c) * cell,
            qrY + (4 + r) * cell,
            Math.ceil(cell),
            Math.ceil(cell)
          );
        }
      }
    }

    // 两行灰色提示（makerQRPost.ts:198-219；第二行品牌文案替换为我方）
    textWriter(
      ["（长按识别二维码进行访问）"],
      descStyle,
      "center",
      "rgba(0, 0, 0, 0.6)",
      "rgba(255, 255, 255, 0.6)",
      16,
      lineHeight,
      { x: cardWidth / 2, y: descEnd.y + margin + 256 }
    );
    textWriter(
      ["Anything Downloader-一键下载文章"],
      descStyle,
      "center",
      "rgba(0, 0, 0, 0.6)",
      "rgba(255, 255, 255, 0.6)",
      16,
      lineHeight,
      { x: cardWidth / 2, y: descEnd.y + margin + 280 }
    );

    // PNG 落盘，文件名 = 页面标题 + ".png"（makerQRPost.ts:221）
    const dataUrl = canvas.toDataURL("image/png");
    canvas.remove();
    done(dataUrl);
  };

  // ---------------------------------------------------------------- 动作定义

  const msgBackground = (msg) =>
    new Promise((resolve) => {
      try {
        chrome.runtime.sendMessage(msg).then(resolve, () => resolve(null));
      } catch (_) {
        resolve(null);
      }
    });
  const nm = (method, params, timeoutMs) =>
    msgBackground({ type: "nm", method, params, timeoutMs });

  async function collectionDir(title) {
    // 合集目录 <default_dir>/<标题>/（拿不到设置则返回 null 降级默认目录）
    const s = await nm("settings.get", { key: "default_dir" });
    const base = s && s.ok && s.result && s.result.value;
    if (!base || !String(base).trim()) return null;
    const name = ADL.cleanName(title, 80) || "article";
    const sep = String(base).includes("\\") ? "\\" : "/";
    return String(base).replace(/[\\/]+$/, "") + sep + name;
  }

  // 探测当前页（复用 background 缓存，拿资源清单用于推送类动作）
  async function probeResources() {
    const r = await msgBackground({ type: "adapter-probe" });
    return r && r.ok ? r.resources || [] : [];
  }

  ADL.articleActions = (env) => {
    if (!env) return [];
    return [
      {
        id: "sync",
        label: "文章同步",
        run() {
          // code-box weixinPanel.tsx:137-139 经 background index.ts:143-149：带来源
          // tabId 打开同步页，页面 boot 自动锁定该标签页并提取文章（?tabId= 分支）
          msgBackground({ type: "open-sync" });
        },
      },
      {
        id: "cover",
        label: "获取封面",
        run() {
          // code-box weixin.tsx:159-164：og:image 新标签直接打开封面原图
          if (!env.coverUrl) {
            toast("本页未找到封面（og:image）"); // 有意偏差：code-box 缺失时静默无响应
            return;
          }
          window.open(env.coverUrl);
        },
      },
      {
        id: "summary",
        label: "文章摘要",
        run() {
          if (!env.summary) {
            toast("本页未找到摘要（meta description）");
            return;
          }
          textModal("文章摘要", env.summary, { readOnly: true, height: "24vh" });
        },
      },
      {
        id: "purelink",
        label: "净化链接",
        run() {
          // code-box weixinPanel.tsx:102-121：复制净化 URL 后不经确认直接导航
          //（URL 未变则 reload）；复制抛异常降级 prompt，点确定同样导航
          const pureUrl = ADL.actionCleanUrl(location.href);
          const input = document.createElement("input");
          input.id = "pure-url-for-copy";
          input.value = pureUrl;
          document.body.appendChild(input);
          input.select();
          try {
            document.execCommand("copy");
            if (location.href === pureUrl) {
              location.reload();
            } else {
              location.href = pureUrl;
            }
          } catch (_) {
            if (prompt("净化后的网址是：", pureUrl) !== null) {
              location.href = pureUrl;
            }
          }
          document.body.removeChild(input);
        },
      },
      {
        id: "origlink",
        label: "原始链接",
        run() {
          textModal("原始链接", ADL.actionOriginalUrl(env.pageUrl, document), {
            readOnly: true,
            height: "12vh",
          });
        },
      },
      {
        id: "editmd",
        label: "编辑 Markdown",
        run() {
          textModal(`编辑 Markdown — ${env.title}`, env.md, {
            height: "60vh",
            downloadName: `${env.baseName}.md`,
            downloadMime: "text/markdown;charset=utf-8",
            hint: "可编辑；下载为浏览器本地保存",
          });
        },
      },
      {
        id: "dlmd",
        label: "下载 Markdown",
        async run() {
          // code-box tools.ts:60-66：本地 Blob（text/markdown;charset=utf-8）
          // 经浏览器下载栏保存，文件名 `标题-YYYY-MM-DD HH:mm:ss.md`；
          // 微信先预滚动触发懒图再转换（weixin.tsx:251-284）
          let e = env;
          if (isWeixinPage()) {
            await scrollToLoadImages();
            try {
              e = ADL.articleActionEnv() || env; // 滚动后按最新 DOM 重新生成 MD
            } catch (_) {}
          }
          const md = e.md || "";
          if (!md) {
            toast("未生成 Markdown"); // tools.ts:61 无 MD 不保存；此处补提示
            return;
          }
          saveBlobLocal(md, "text/markdown;charset=utf-8", ADL.mdFileName(e.title || env.title));
          toast("Markdown 已保存");
        },
      },
      {
        id: "parsemd",
        label: "解析 Markdown",
        run() {
          const md = env.md || "";
          if (!md) {
            toast("未生成 Markdown");
            return;
          }
          if (isWeixinPage()) {
            // code-box 微信特例（weixin.tsx:355-359）：不走 AI，图片全部转
            // base64 内联后本地保存 .md（tools.ts:68-126），命名同⑤
            toast("正在内联图片…");
            ADL.inlineMarkdownImages(md).then((out) => {
              saveBlobLocal(out, "text/markdown;charset=utf-8", ADL.mdFileName(env.title));
              toast("Markdown（图片已内联）已保存");
            });
            return;
          }
          // AI 流（parseMarkdownHook.ts:18-37）：MD+指令写 storage["ai-content"]，
          // 读 storage["app-aiType"] 直接打开对应站点（未设置默认 chatgpt）；
          // 复制在前，打开后粘贴即用
          const text = md + ADL.AI_INSTRUCTION;
          copyText(text);
          const openAi = (type) => window.open(aiSiteUrl(type), "_blank");
          try {
            chrome.storage.local.set({ "ai-content": text }, () => {
              try {
                chrome.storage.local.get("app-aiType", (o) => openAi(o && o["app-aiType"]));
              } catch (_) {
                openAi(null);
              }
            });
          } catch (_) {
            openAi(null);
          }
        },
      },
      {
        id: "pack",
        label: "打包下载(MD+图片)",
        async run() {
          const rs = await probeResources();
          const urls = rs.filter((r) => !r.optional).map((r) => r.url);
          if (!urls.length) {
            toast("本页无可打包资源");
            return;
          }
          const pr = await msgBackground({ type: "adapter-push", urls });
          toast(
            pr && pr.ok
              ? `已打包 ${pr.pushed} 项${pr.dir ? ` → ${pr.dir}/` : ""}`
              : "打包失败（主程序未运行？）"
          );
        },
      },
      {
        id: "pdf",
        label: "下载 PDF",
        run() {
          // 隐藏 iframe 打印（code-box 同款原理）：浏览器打印对话框里"另存为 PDF"
          const iframe = document.createElement("iframe");
          iframe.style.cssText = "position:fixed;right:0;bottom:0;width:1px;height:1px;opacity:0;border:0;";
          document.body.appendChild(iframe);
          const idoc = iframe.contentWindow.document;
          idoc.open();
          idoc.write(ADL.actionBuildPrintHtml(env));
          idoc.close();
          setTimeout(() => {
            try {
              iframe.contentWindow.focus();
              iframe.contentWindow.print();
            } catch (_) {}
            setTimeout(() => iframe.remove(), 60000);
          }, 900);
          toast("已调起打印（目标选择『另存为 PDF』）");
        },
      },
      {
        id: "word",
        label: "下载 Word",
        async run() {
          const dir = await collectionDir(env.title);
          const resp = await nm(
            "task.add_content",
            {
              filename: `${env.baseName}.doc`,
              mime: "application/msword",
              content: ADL.actionBuildWordHtml(env),
              dir: dir || undefined,
              source_url: env.pageUrl,
            },
            20000
          );
          toast(resp && resp.ok ? `Word 已保存${dir ? ` → ${env.title}/` : ""}` : "保存失败（主程序未运行？）");
        },
      },
      {
        id: "dlimgs",
        label: "下载所有图片",
        async run() {
          // code-box：本地 zip 落盘（微信 weixin.tsx:227-228 `${title}-images.zip`；
          // 通用站 download.ts:29-41 `${title}.zip`）。content 侧收图定名 →
          // background 抓原始字节（host_permissions 免 CORS）→ lib/zip.js 打包
          // → chrome.downloads 落盘（zip-download 消息）。
          const jobs = collectImageJobs();
          if (!jobs.length) {
            toast("本页未发现文章图片");
            return;
          }
          const base = String(document.title || "").trim();
          const filename = isWeixinPage() ? `${base}-images.zip` : `${base}.zip`;
          toast(`正在打包 ${jobs.length} 张图片…`);
          const r = await msgBackground({ type: "zip-download", jobs, filename });
          toast(
            r && r.ok
              ? `已打包 ${r.saved}/${jobs.length} 张图片 → ${r.filename}`
              : "图片打包失败"
          );
        },
      },
      {
        id: "poster",
        label: "生成海报",
        run() {
          // code-box makerQRPost.ts:221：PNG 落盘，文件名 = 页面标题 + ".png"
          ADL.actionMakePoster(env, (dataUrl) => {
            const a = document.createElement("a");
            a.href = dataUrl;
            a.download = `${env.title}.png`;
            a.click();
            toast("海报已生成并下载");
          });
        },
      },
    ];
  };

  // —— 面板环境构建：prepare（共享提取管线）+ 页面元数据（封面/摘要）+ MD
  ADL.articleActionEnv = () => {
    if (!ADL.articlePrepare) return null;
    const ctx = {
      url: location.href,
      captured: () => [],
      inlineJson: () => null,
      dom: () => document,
    };
    const p = ADL.articlePrepare(ctx);
    if (!p) return null;
    const metaContent = (sel) => {
      try {
        const el = document.querySelector(sel);
        return el ? attr(el, "content") : null;
      } catch (_) {
        return null;
      }
    };
    let summary = metaContent('meta[name="description"]') || "";
    if (!summary) {
      // 兜底：正文首个非空段落截断
      const first = p.content.querySelector("p");
      summary = first
        ? String(first.textContent || "").replace(/\s+/g, " ").trim().slice(0, 150)
        : "";
    }
    const md = ADL.buildMarkdown
      ? ADL.buildMarkdown({
          title: p.title,
          siteLabel: p.site.label,
          pageUrl: p.pageUrl,
          contentEl: p.content,
          baseUrl: p.pageUrl,
          author: p.meta && p.meta.author,
          published: p.meta && p.meta.published,
        })
      : "";
    return {
      ...p,
      summary: String(summary || "").trim(),
      coverUrl: metaContent('meta[property="og:image"]'),
      md,
    };
  };
})();
