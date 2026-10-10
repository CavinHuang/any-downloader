// 文章适配器（articles）—— code-box 能力集成（docs/MEDIA-ADAPTERS.md §11）：
//   多站点文章正文提取 → ①排版 HTML（自包含模板 + 保留节点内联样式/高亮 class）
//   ② Markdown（ADL.turndown）两个 document 资源 + ③ 文章图片直链清单。
// 图片与文档都经现有管线：图片推送引擎下载；文档经 task.add_content 落盘。
// 站点选择器来源：code-box contents/*.tsx（2026-10 校对），每站给 fallback 链防改版。
(() => {
  // —— 站点表。root = 导出范围（含标题/作者时导出保留），content = 正文（缺省 = root）。
  //     images:false 表示该站图片由其他适配器负责（微信：wechat-article 的
  //     mmbiz 原图规则更精确，避免同图重复收录）。
  //     transform(clone, doc)：站点特有修正（懒加载/初始隐藏等），作用于克隆。
  const SITES = [
    {
      id: "wechat-article",
      label: "微信公众号",
      re: /^https?:\/\/mp\.weixin\.qq\.com\/s/i,
      title: ["#activity-name", ".rich_media_title", 'meta[property="og:title"]'],
      root: ["#img-content"],
      content: ["#js_content"],
      images: false,
      // 正文 JS 揭示前 style="visibility:hidden;opacity:0"（不剥则导出全文不可见）；
      // 懒加载真实地址在 data-src
      transform(clone) {
        clone.removeAttribute("style");
        const jsContent = pickFirst(clone, ["#js_content"]);
        if (jsContent) jsContent.removeAttribute("style");
      },
      // cleanup 懒加载落地后跑：导出 HTML 内的 mmbiz src 与 wechat-article
      // 适配器产出的图片 URL 同形（/0 化、剥 tp=webp）——跨适配器合集模式的
      // 相对引用重写依赖两边 URL 一致
      postCleanup(clone) {
        for (const im of clone.querySelectorAll("img")) {
          const u = attr(im, "src") || "";
          if (/mmbiz\.qpic\.cn/i.test(u)) {
            im.setAttribute("src", ADL.mmbizNormalize(u));
          }
        }
      },
    },
    {
      id: "csdn",
      label: "CSDN",
      re: /^https?:\/\/(?:[^/]*\.)?blog\.csdn\.net\/|devpress\.csdn\.net\//i,
      title: [".title-article", "h1.title-article", "h1"],
      root: [".blog-content-box", ".article-detail"],
      content: ["#content_views", ".article_content"],
    },
    {
      id: "zhihu",
      label: "知乎专栏",
      re: /^https?:\/\/zhuanlan\.zhihu\.com\//i,
      title: ["h1.Post-Title", 'meta[property="og:title"]'],
      root: ["article.Post-Main", ".Post-Main"],
      content: [".Post-RichTextContainer", ".RichText", "article"],
      // 知乎懒加载原图在 data-actualsrc；公式为 img.eequation（外链保留）
      lazyAttrs: ["data-actualsrc", "data-src"],
    },
    {
      id: "juejin",
      label: "掘金",
      re: /^https?:\/\/juejin\.cn\/post\//i,
      title: ["h1.article-title", "h1"],
      root: ["article.article", ".article-main"],
      content: [".article-content", "article"],
    },
    {
      id: "jianshu",
      label: "简书",
      re: /^https?:\/\/(?:www\.)?jianshu\.com\/(?:p|note)\//i,
      title: ["h1", 'meta[property="og:title"]'],
      root: ["article"],
      content: ["article"],
    },
    {
      id: "cnblogs",
      label: "博客园",
      re: /^https?:\/\/(?:[^/]*\.)?cnblogs\.com\/[^/]+\/p\//i,
      title: ["#cb_post_title_url", "h1.postTitle", "h1"],
      root: ["#post_detail"],
      content: ["#cnblogs_post_body", ".post"],
    },
    {
      id: "51cto",
      label: "51CTO",
      re: /^https?:\/\/blog\.51cto\.com\//i,
      title: ["h1.article-title", "h1.title", "h1"],
      root: ["article"],
      content: ["article .article-detail", ".article-content", "article"],
    },
    {
      id: "jb51",
      label: "脚本之家",
      re: /^https?:\/\/www\.jb51\.net\/(?:article|book)\//i,
      title: ["h1", ".title"],
      root: ["#article", ".article"],
      content: ["#content", "#article"],
    },
    {
      id: "php",
      label: "php中文网",
      re: /^https?:\/\/www\.php\.cn\//i,
      title: [".wzctTitle", "h1"],
      root: [".phpscMain", "article"],
      content: [".php-article", ".phpscMain"],
    },
    {
      id: "segmentfault",
      label: "SegmentFault",
      re: /^https?:\/\/segmentfault\.com\/a\//i,
      title: ["h1.article-title", "h1"],
      root: [".article-wrap", "article"],
      content: [".article-content", "article", ".container"],
    },
    {
      id: "oschina",
      label: "开源中国",
      re: /^https?:\/\/my\.oschina\.net\//i,
      title: ["h1.blog-detail-title", ".blog-title", "h1"],
      root: [".blog-detail-container", ".news-detail-container"],
      content: [".blog-detail", ".article-detail", undefined], // undefined = root 本身
    },
    {
      id: "medium",
      label: "Medium",
      re: /^https?:\/\/(?:[^/]*\.)?medium\.com\//i,
      title: ["h1", 'meta[property="og:title"]'],
      root: ["article"],
      content: ["article"],
    },
  ];

  const TITLE_SUFFIX_RE =
    /\s*[-_｜|]\s*(?:CSDN博客|知乎|简书|博客园|思否|SegmentFault 思否|掘金|个人文章|OSCHINA|中文社区|Medium|51CTO博客|脚本之家|php中文网)\s*$/;

  // —— 小工具
  const attr = (el, name) => {
    try {
      return el.getAttribute(name);
    } catch (_) {
      return null;
    }
  };
  const absUrl = (u, base) => {
    const t = String(u || "").trim();
    if (!t) return null;
    if (/^data:|^blob:/i.test(t)) return null;
    try {
      return new URL(t, base || undefined).href;
    } catch (_) {
      return null;
    }
  };
  const IMG_EXTS = ["jpeg", "png", "gif", "webp", "bmp", "svg", "avif"];

  // 按选择器 fallback 链取第一个命中（选择器只支持测试桩覆盖的简单形态）
  function pickFirst(scope, sels) {
    for (const s of sels || []) {
      if (!s) continue;
      try {
        const el = scope.querySelector(s);
        if (el) return el;
      } catch (_) {}
    }
    return null;
  }

  function pickTitle(doc, root, site) {
    for (const sel of site.title || []) {
      const el = doc.querySelector(sel) || root.querySelector(sel);
      if (!el) continue;
      const t =
        String((el.nodeName || "").toUpperCase()) === "META"
          ? attr(el, "content")
          : el.textContent;
      const clean = ADL.cleanName(t, 120);
      if (clean) return clean;
    }
    return ADL.cleanName(doc.title || "", 120) || "文章";
  }

  // —— 通用清理（作用于克隆；code-box 各站 done in-site 的动作收敛于此）
  // 1) 不可离线化的元素删除（脚本/样式/表单控件/iframe 媒体卡片等）
  // 2) 懒加载图：data-src 族 → src；占位图（data:）无真实地址时删节点
  // 3) <a> 相对地址绝对化；img 去 srcset（离线打开时 srcset 优先级高于 src 且常为缩放地址）
  // 4) 与标题重复的正文 h1 去除（导出模板头部已带标题）
  function cleanup(clone, doc, opts) {
    for (const sel of [
      "script", "style", "noscript", "template", "link", "meta",
      "iframe", "object", "embed", "button", "input", "select",
      "textarea", "audio", "video", "canvas", "form",
      ".hljs-button", ".pre-numbering", ".copy-btn",
    ]) {
      for (const el of clone.querySelectorAll(sel)) {
        try {
          el.remove();
        } catch (_) {}
      }
    }
    const lazy = (opts && opts.lazyAttrs) || ["data-src", "data-original", "data-lazy-src"];
    const base = (opts && opts.baseUrl) || "";
    for (const im of clone.querySelectorAll("img")) {
      // 懒加载真实地址优先于 src（知乎 data-actualsrc=原图、src=缩放图；
      // 微信 data-src=真实地址、src=占位）。占位 data: 一律跳过。
      let src = null;
      for (const a of lazy) {
        const v = attr(im, a);
        if (v && !/^data:/i.test(v)) {
          src = v;
          break;
        }
      }
      if (!src) src = attr(im, "src") || "";
      const abs = absUrl(src, base);
      if (!abs) {
        try {
          im.remove();
        } catch (_) {}
        continue;
      }
      im.setAttribute("src", abs);
      try {
        im.removeAttribute("srcset");
        im.removeAttribute("loading");
      } catch (_) {}
    }
    for (const a of clone.querySelectorAll("a[href]")) {
      const abs = absUrl(attr(a, "href"), base);
      if (abs) a.setAttribute("href", abs);
    }
    if (opts && opts.title) {
      for (const h of clone.querySelectorAll("h1")) {
        if (ADL.cleanName(h.textContent, 120) === opts.title) {
          try {
            h.remove();
          } catch (_) {}
          break;
        }
      }
    }
    if (opts.postCleanup) {
      try {
        opts.postCleanup(clone);
      } catch (_) {}
    }
  }

  // —— Markdown 导出：YAML front matter（Obsidian/Hugo/Hexo 等直接识别元数据）
  //     + H1 标题 + 正文。front matter 值统一双引号包裹，内部 " 与 \ 转义。
  function yamlStr(s) {
    return `"${String(s || "").replace(/\\/g, "\\\\").replace(/"/g, '\\"')}"`;
  }

  // 导出给 actions.js（编辑/解析 Markdown 菜单直接转换当前页）
  ADL.buildMarkdown = (opts) => buildMarkdown(opts);

  function buildMarkdown(opts) {
    const { title, siteLabel, pageUrl, contentEl, baseUrl } = opts;
    const body = ADL.turndown(contentEl, { baseUrl });
    const date = new Date().toISOString().slice(0, 10);
    const head =
      `---\n` +
      `title: ${yamlStr(title)}\n` +
      `source: ${yamlStr(pageUrl)}\n` +
      `site: ${yamlStr(siteLabel)}\n` +
      `exported: ${yamlStr(date)}\n` +
      `---\n\n` +
      `# ${title}\n\n`;
    return head + body;
  }

  // —— Markdown 内图片引用重写（文章合集离线阅读，background 推送前调用）：
  //     同批推送的图片 URL → 相对文件名；文件名含空格/括号时用 <...>
  //     角链接形态（CommonMark 合法）防提前截断。
  ADL.rewriteDocRefs = (content, byUrl) => {
    if (!content || !byUrl) return content;
    const keyOf = (u) => String(u || "").split("#")[0].split("?")[0];
    return String(content).replace(/(\]\()([^)\s]+)(\))/g, (m, pre, url, post) => {
      const name = byUrl[keyOf(url)];
      if (!name) return m;
      return pre + (/[()\s]/.test(name) ? `<${name}>` : name) + post;
    });
  };

  // —— 适配器注册（bridge 按 id 调用 extract(ctx)）
  ADL.ARTICLE_SITES = SITES;
  ADL.articleSite = (url) => SITES.find((s) => s.re.test(String(url || ""))) || null;

  // —— 提取管线共享入口：extract（资源清单）与 actions.js（文章操作菜单）共用。
  //     返回 { site, title, baseName, content(清理后的正文克隆), pageUrl } 或 null。
  ADL.articlePrepare = (ctx) => {
    const site = ADL.articleSite(ctx.url);
    if (!site) return null;
    const doc = ctx.dom();
    const root = pickFirst(doc, site.root);
    if (!root) return null;
    const title = pickTitle(doc, root, site).replace(TITLE_SUFFIX_RE, "");
    const baseName = ADL.cleanName(title, 50) || "article";
    const baseUrl = ctx.url;

    // 克隆导出范围 → 站点修正 → 通用清理 → 定位正文
    const clone = root.cloneNode(true);
    if (site.transform) {
      try {
        site.transform(clone, doc);
      } catch (_) {}
    }
    cleanup(clone, doc, {
      title,
      baseUrl,
      lazyAttrs: site.lazyAttrs,
      postCleanup: site.postCleanup,
    });
    const content = pickFirst(clone, site.content) || clone;
    return { site, title, baseName, content, root: clone, pageUrl: ctx.url };
  };

  ADL.adapters["articles"] = {
    id: "articles",
    lastSource: "dom",

    extract(ctx) {
      const p = ADL.articlePrepare(ctx);
      if (!p) return [];
      const { site, title, baseName, content, root, pageUrl } = p;
      const baseUrl = pageUrl;

      const out = [];

      // ① Markdown 文档（唯一文档产物；background 走 task.add_content）
      out.push({
        url: pageUrl + "#adl-doc-md",
        kind: "document",
        filename: `${baseName}.md`,
        meta: {
          document: {
            mime: "text/markdown;charset=utf-8",
            content: buildMarkdown({
              title,
              siteLabel: site.label,
              pageUrl,
              contentEl: content,
              baseUrl,
            }),
          },
          extra: { site: site.id, format: "markdown" },
        },
      });

      // ② 文章图片（微信站除外：wechat-article 适配器专管 mmbiz 原图规则）
      if (site.images !== false) {
        const headers = { Referer: pageUrl.split("/").slice(0, 3).join("/") + "/" };
        const seen = new Set();
        let n = 0;
        for (const im of root.querySelectorAll("img")) {
          const u = ADL.toHttps(attr(im, "src") || "");
          if (!u || !/^https?:\/\//i.test(u)) continue;
          // 同图多尺寸去重：剥 query（CSDN ?x-oss-process 缩放参数、微信 ?wx_fmt）
          const key = u.split("#")[0].split("?")[0];
          if (seen.has(key)) continue;
          seen.add(key);
          n += 1;
          const extRaw = (/\.([a-z0-9]+)(?=[?#]|$)/i.exec(u) || [])[1] || "";
          const ext = IMG_EXTS.includes(extRaw.toLowerCase())
            ? extRaw.toLowerCase().replace(/^jpg$/, "jpeg")
            : "jpeg";
          out.push({
            url: u,
            kind: "image",
            filename: `${baseName}_${String(n).padStart(2, "0")}.${ext}`,
            headers,
            meta: { extra: { site: site.id } },
          });
        }
      }
      return out;
    },
  };
})();
