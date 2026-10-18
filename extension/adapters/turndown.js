// 轻量 HTML → Markdown（GFM）转换器 —— 零依赖（code-box 用的 @joplin/turndown
// 等价子集，为文章导出场景实现，docs/MEDIA-ADAPTERS.md §11）。
// 覆盖：标题/段落/分割线/围栏代码块（语言探测）/引用/列表（嵌套）/GFM 表格/
//       加粗斜体删除线/行内代码/链接/图片/换行。
// DOM 遍历只用 nodeType/childNodes/textContent/getAttribute/n.nodeName ——
// 单测用迷你 DOM 桩即可完整复现（scripts/test_articles.js），不依赖 jsdom。
(() => {
  // 块级元素集合（行内上下文遇到时拍平为其行内内容；导出时 NAV/ASIDE 等直接丢弃）
  const BLOCK = new Set([
    "ADDRESS", "ARTICLE", "ASIDE", "BLOCKQUOTE", "CAPTION", "DD", "DETAILS",
    "DIR", "DIV", "DL", "DT", "FIELDSET", "FIGCAPTION", "FIGURE", "FOOTER",
    "FORM", "H1", "H2", "H3", "H4", "H5", "H6", "HEADER", "HR", "LI", "MAIN",
    "MENU", "NAV", "OL", "P", "PRE", "SECTION", "SUMMARY", "TABLE", "TBODY",
    "TD", "TFOOT", "TH", "THEAD", "TR", "UL",
  ]);
  // 不可离线化的元素（交互控件/脚本）：Markdown 输出里一律丢弃。
  // IFRAME/VIDEO/AUDIO 不在列——嵌入的在线视频/音频转为链接保留（mdInline/mdBlock），
  // OBJECT/EMBED 无可靠 src 语义仍丢弃。
  const DROP = new Set([
    "SCRIPT", "STYLE", "NOSCRIPT", "TEMPLATE", "LINK", "META", "BUTTON",
    "INPUT", "SELECT", "TEXTAREA", "OBJECT", "EMBED", "SVG",
    "CANVAS", "MAP", "AREA", "DATALIST", "DIALOG",
  ]);

  const isElem = (n) => !!n && n.nodeType === 1;
  const isText = (n) => !!n && n.nodeType === 3;
  const tagOf = (n) => String((n && n.nodeName) || "").toUpperCase();
  // childNodes 在真实 DOM 是 NodeList（无 filter/map）——必须先转数组；
  // 单测桩此前用普通数组导致该缺陷漏检（scripts/test_articles.js 有 NodeList 形态回归）
  const childNodesArr = (n) => Array.from((n && n.childNodes) || []);
  const kids = (n) => childNodesArr(n).filter(isElem);

  // —— 文本规整：连续空白折叠为单空格；换行收敛为段落分隔
  const tidy = (s) =>
    String(s || "")
      .replace(/[ \t\r\f]+/g, " ")
      .replace(/ *\n */g, "\n")
      .replace(/\n{3,}/g, "\n\n")
      .trim();

  // 行内文本 Markdown 转义（\ ` * _ [ ]）；块级装配时另做行首转义
  const escInline = (s) => s.replace(/([\\`*_\[\]])/g, "\\$1");
  // 行首 Markdown 结构符转义（标题/引用/列表/分割线），防正文被解析为结构
  const escLineStart = (s) =>
    s
      .split("\n")
      .map((l) =>
        /^([#>]|[-+]\s|\*\s|\d+[.)]\s)/.test(l) || /^(-{3,}|\*{3,}|_{3,})\s*$/.test(l)
          ? "\\" + l
          : l
      )
      .join("\n");

  // —— URL 解析：相对 → 绝对；data:image 保留（罕见，导出仍有意义）；
  //    blob:/javascript: 与其他 data: 无离线意义 → null（调用方跳过）
  const absUrl = (u, base) => {
    const t = String(u || "").trim();
    if (!t || /^javascript:/i.test(t)) return null;
    if (/^data:/i.test(t)) return /^data:image\//i.test(t) ? t : null;
    if (/^blob:/i.test(t)) return null;
    try {
      return new URL(t, base || undefined).href;
    } catch (_) {
      return t;
    }
  };

  // —— 行内上下文。强调类元素把内容首尾空白移出定界符（`** bar**` 不渲染）
  function emphasis(node, base, mk) {
    const inner0 = mdInlineChildren(node, base);
    const lead = (/^\s+/.exec(inner0) || [""])[0];
    const trail = (/\s+$/.exec(inner0) || [""])[0];
    const inner = inner0.trim();
    return inner ? lead + mk(inner) + trail : "";
  }

  function mdInline(node, base) {
    if (isText(node)) return escInline(collapseWs(node.textContent));
    if (!isElem(node)) return "";
    const tag = tagOf(node);
    switch (tag) {
      case "BR":
        return "\n";
      case "IMG": {
        const src = absUrl(node.getAttribute("src") || node.getAttribute("data-src"), base);
        if (!src) return "";
        const alt = String(node.getAttribute("alt") || "").replace(/[\[\]]/g, "");
        return `![${alt}](${src})`;
      }
      case "A": {
        const inner = mdInlineChildren(node, base);
        const href = absUrl(node.getAttribute("href"), base);
        if (!href) return inner; // 命名锚点/占位链接：退化为纯文本
        const text = inner.trim() || href;
        return `[${text}](${href})`;
      }
      case "STRONG":
      case "B":
        return emphasis(node, base, (s) => `**${s}**`);
      case "EM":
      case "I":
        return emphasis(node, base, (s) => `*${s}*`);
      case "DEL":
      case "S":
      case "STRIKE":
        return emphasis(node, base, (s) => `~~${s}~~`);
      case "MARK":
        // ==高亮==：GFM 生态事实标准（Obsidian/Typora 渲染；不支持的查看器显示等号）
        return emphasis(node, base, (s) => `==${s}==`);
      case "IFRAME":
      case "VIDEO":
      case "AUDIO":
        return mediaLink(node, base);
      case "CODE": {
        const text = collapseWs(node.textContent).trim();
        if (!text) return "";
        return /`/.test(text) ? "`` " + text + " ``" : "`" + text + "`";
      }
      default:
        if (DROP.has(tag)) return "";
        return mdInlineChildren(node, base); // span/font/u/sup… 透明递归
    }
  }

  const mdInlineChildren = (node, base) =>
    childNodesArr(node).map((n) => mdInline(n, base)).join("");

  // —— 嵌入媒体 → 链接（B 站/YouTube 播放器 iframe、<video>/<audio>）。
  //    src 懒加载形态取 data-src；<video> 无 src 时取 <source> 子元素。
  function mediaSrc(node, base) {
    let u = absUrl(node.getAttribute("src") || node.getAttribute("data-src"), base);
    if (!u && tagOf(node) === "VIDEO") {
      const srcEl = kids(node).find((k) => tagOf(k) === "SOURCE");
      if (srcEl) u = absUrl(srcEl.getAttribute("src"), base);
    }
    return u;
  }
  function mediaLink(node, base) {
    const u = mediaSrc(node, base);
    if (!u) return "";
    const kind = tagOf(node) === "AUDIO" ? "♪ 音频" : "▶ 视频";
    return `[${kind}](${u})`;
  }

  // 行内文本节点的空白折叠：保留首尾单空格避免中英文词粘连；
  // 不可见空白（纯换行缩进）折叠后为空串
  function collapseWs(s) {
    return String(s || "").replace(/[ \t\r\n\f]+/g, " ");
  }

  // —— 代码块语言探测：language-xxx / lang-xxx / hljs xxx / brush:xxx;（SyntaxHighlighter）
  function codeLang(...classes) {
    for (const cs of classes) {
      const m =
        /(?:^|\s)(?:language|lang|hljs)[- ]([\w+#.-]+)/i.exec(cs || "") ||
        /(?:^|\s)brush:\s*([\w+#.-]+)/i.exec(cs || "");
      if (m) return m[1].toLowerCase();
    }
    return "";
  }

  function mdPre(pre, base) {
    const codeEl = kids(pre).find((k) => tagOf(k) === "CODE") || pre;
    const lang = codeLang(
      codeEl.getAttribute("class"),
      pre.getAttribute("class"),
      pre.getAttribute("data-lang")
    );
    const text = String(codeEl.textContent || "").replace(/^\n+|\n+$/g, "");
    // 内容含 ``` 时升级围栏长度
    const fence = /```/.test(text) ? "````" : "```";
    return fence + lang + "\n" + text + "\n" + fence;
  }

  function mdList(list, base, depth) {
    const ordered = tagOf(list) === "OL";
    const startN = Number(list.getAttribute("start"));
    let idx = Number.isFinite(startN) && startN > 0 ? startN : 1;
    const out = [];
    for (const li of kids(list)) {
      if (tagOf(li) !== "LI") continue;
      // GFM 任务列表：li 直属 checkbox（articles.cleanup 会保留并禁用它）
      const cb = childNodesArr(li).find(
        (n) => isElem(n) && tagOf(n) === "INPUT" && (n.getAttribute("type") || "").toLowerCase() === "checkbox"
      );
      const task = cb
        ? cb.getAttribute("checked") != null || cb.getAttribute("data-checked") === "true"
          ? "[x] "
          : "[ ] "
        : "";
      let inline = "";
      const nested = [];
      for (const n of li.childNodes || []) {
        const tag = tagOf(n);
        if (isElem(n) && (tag === "UL" || tag === "OL")) {
          nested.push(mdList(n, base, depth + 1));
        } else if (isElem(n) && (tag === "PRE" || tag === "BLOCKQUOTE" || tag === "TABLE")) {
          nested.push(mdBlock(n, base));
        } else {
          inline += mdInline(n, base);
        }
      }
      const lines = tidy(inline).split("\n").filter((l, i) => i === 0 || l);
      const marker = (ordered ? idx + ". " : "- ") + task;
      const indent = " ".repeat((ordered ? String(idx).length + 2 : 2) + task.length);
      let item = marker + (lines.shift() || "");
      for (const l of lines) item += "\n" + indent + l;
      for (const nb of nested) {
        item +=
          "\n" +
          nb
            .split("\n")
            .map((l) => (l ? indent + l : ""))
            .join("\n");
      }
      out.push(item);
      idx += 1;
    }
    return out.join("\n");
  }

  // 列对齐：th 的 style text-align / align 属性 → GFM 分隔行标记
  function alignOf(el) {
    const m = /text-align\s*:\s*(left|center|right)/i.exec(
      String(el.getAttribute("style") || "")
    );
    const a = ((m && m[1]) || el.getAttribute("align") || "").toLowerCase();
    return a === "center" ? ":---:" : a === "right" ? "---:" : a === "left" ? ":---" : "";
  }

  function mdTable(table, base) {
    const rows = [];
    for (const k of kids(table)) {
      const tag = tagOf(k);
      if (tag === "TR") rows.push(k);
      else if (tag === "THEAD" || tag === "TBODY" || tag === "TFOOT")
        for (const tr of kids(k)) if (tagOf(tr) === "TR") rows.push(tr);
    }
    if (!rows.length) return "";
    const cellEls = (tr) =>
      kids(tr).filter((c) => tagOf(c) === "TD" || tagOf(c) === "TH");
    const cellText = (c) =>
      tidy(mdInlineChildren(c, base)).replace(/\|/g, "\\|").replace(/\n/g, " ");
    const n = Math.max(...rows.map((r) => cellEls(r).length));
    const fix = (arr) => {
      const a = arr.slice();
      while (a.length < n) a.push("");
      return a;
    };
    const headerEls = cellEls(rows[0]);
    const header = fix(headerEls.map(cellText));
    if (!header.some((c) => c)) return ""; // 首行无内容：不是可用的表头
    // 对齐标记跟随表头列（body 行不参与对齐判定）
    const aligns = fix(headerEls.map(alignOf));
    const lines = [
      "| " + header.join(" | ") + " |",
      "| " + (aligns.map((a) => a || "---")).join(" | ") + " |",
    ];
    for (const r of rows.slice(1)) {
      lines.push("| " + fix(cellEls(r).map(cellText)).join(" | ") + " |");
    }
    return lines.join("\n");
  }

  // —— 块级上下文：返回不含尾空行的块文本
  function mdBlock(node, base) {
    if (isText(node)) {
      const t = tidy(escInline(collapseWs(node.textContent)));
      return t ? escLineStart(t) : "";
    }
    if (!isElem(node)) return "";
    const tag = tagOf(node);
    if (DROP.has(tag)) return "";
    switch (tag) {
      case "H1":
      case "H2":
      case "H3":
      case "H4":
      case "H5":
      case "H6": {
        const inner = mdInlineChildren(node, base).trim();
        return inner ? "#".repeat(Number(tag[1])) + " " + inner : "";
      }
      case "P":
      case "DT":
      case "DD": {
        const t = tidy(mdInlineChildren(node, base));
        return t ? escLineStart(t) : "";
      }
      case "HR":
        return "---";
      case "PRE":
        return mdPre(node, base);
      case "BLOCKQUOTE": {
        const inner = mdBlockChildren(node, base);
        return inner
          ? inner
              .split("\n")
              .map((l) => "> " + l)
              .join("\n")
          : "";
      }
      case "UL":
      case "OL":
        return mdList(node, base, 0);
      case "TABLE":
        return mdTable(node, base);
      case "FIGURE": {
        // 图片 + 可选说明（figcaption → 斜体说明行，紧跟图片）
        const parts = [];
        for (const n of node.childNodes || []) {
          if (!isElem(n)) continue;
          if (tagOf(n) === "FIGCAPTION") {
            const cap = tidy(mdInlineChildren(n, base));
            if (cap) parts.push(`*${cap}*`);
          } else {
            const b = mdBlock(n, base);
            if (b) parts.push(b);
          }
        }
        return parts.join("\n\n");
      }
      case "FIGCAPTION": {
        const cap = tidy(mdInlineChildren(node, base));
        return cap ? `*${cap}*` : "";
      }
      case "DETAILS": {
        // 折叠块 → 原生 HTML details（GFM 允许内嵌 HTML；内部正文仍走 Markdown）
        let summary = "";
        const parts = [];
        for (const n of childNodesArr(node)) {
          if (!isElem(n)) continue;
          if (tagOf(n) === "SUMMARY") {
            summary = tidy(mdInlineChildren(n, base));
            continue;
          }
          const b = mdBlock(n, base);
          if (b) parts.push(b);
        }
        const head = `<details>\n<summary>${summary || "详情"}</summary>\n`;
        const body = parts.join("\n\n");
        return body ? `${head}\n${body}\n</details>` : `${head}</details>`;
      }
      case "IFRAME":
      case "VIDEO":
      case "AUDIO": {
        const s = mediaLink(node, base);
        return s || "";
      }
      case "IMG": {
        const s = mdInline(node, base);
        return s || "";
      }
      case "BR":
        return ""; // 块级游离 br
      default: {
        // 容器：有块级子元素 → 递归为块序列；纯行内容器 → 视作段落。
        // 段落分支用 mdInline(node)（而非 mdInlineChildren）：行内格式元素
        // （strong/em 等）直接落在块级位置时也要保留定界符。
        if (kids(node).some((k) => BLOCK.has(tagOf(k)))) {
          return mdBlockChildren(node, base);
        }
        const t = tidy(mdInline(node, base));
        return t ? escLineStart(t) : "";
      }
    }
  }

  function mdBlockChildren(node, base) {
    return childNodesArr(node)
      .map((n) => mdBlock(n, base))
      .filter(Boolean)
      .join("\n\n");
  }

  /**
   * HTML → Markdown。
   * @param {Element} root 文章内容根节点
   * @param {{baseUrl?: string}} [opts] 相对链接/图片的解析基准
   * @returns {string} markdown 文本（尾随单个换行）
   */
  ADL.turndown = (root, opts) => {
    if (!root) return "";
    const base = (opts && opts.baseUrl) || "";
    // 根节点本身是结构块（表格/列表/代码块等）→ 整体走对应块规则，
    // 不能拆子节点（否则 thead/tbody/tr 会被当普通容器拍平成段落）
    const ROOT_BLOCK = ["PRE", "UL", "OL", "TABLE", "BLOCKQUOTE", "HR"];
    const isRootBlock = isElem(root) && ROOT_BLOCK.includes(tagOf(root));
    const md = isRootBlock
      ? mdBlock(root, base)
      : kids(root).some((k) => BLOCK.has(tagOf(k)))
      ? mdBlockChildren(root, base)
      : // 纯行内根（p/span/强调容器）：整体作为一段，拆开会丢行内上下文
        (() => {
          const t = tidy(mdInline(root, base));
          return t ? escLineStart(t) : "";
        })();
    return md ? md + "\n" : "";
  };
})();
