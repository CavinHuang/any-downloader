// 极简 Markdown → HTML 渲染器（零依赖）—— 文章同步专用降级路径。
// 场景：提取产物自带「净化 HTML」（articlePrepare 产出，保真优先）；当用户在
// 同步页编辑了 Markdown 后，HTML 平台（知乎/简书/CMS）无法再复用原 HTML，
// 用本渲染器从编辑后的 Markdown 生成基础 HTML。
// 覆盖（GFM 子集）：围栏代码块/ATX 标题/分割线/引用/无序有序列表/GFM 表格/
//   段落；行内：转义/行内代码/图片/链接/加粗/斜体/删除线。
// 明确不支持（超出降级定位）：嵌套列表、任务列表、脚注、引用块内多级结构。
self.ADL = self.ADL || {};
(() => {
  // —— HTML 转义（文本节点与属性值共用）
  const esc = (s) =>
    String(s == null ? "" : s)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;");

  // —— 行内渲染：先转义，再依次替换结构标记（URL 中已转义的 &quot; 兼容标题形态）
  function inline(text) {
    let s = esc(text);
    // 行内代码（内容已转义，避免被后续规则二次处理 → 先挖出占位）
    const codes = [];
    s = s.replace(/`([^`]+)`/g, (_m, code) => {
      codes.push("<code>" + code + "</code>");
      return "\u0000" + (codes.length - 1) + "\u0000";
    });
    // 图片：![alt](src) 或 ![alt](src "title")
    s = s.replace(
      /!\[([^\]]*)\]\(([^)\s]+)(?:\s+&quot;([^&]*)&quot;)?\)/g,
      (_m, alt, src) => '<img src="' + src + '" alt="' + alt + '" />'
    );
    // 链接：[text](href)
    s = s.replace(
      /\[([^\]]+)\]\(([^)\s]+)(?:\s+&quot;([^&]*)&quot;)?\)/g,
      (_m, label, href) => '<a href="' + href + '">' + label + "</a>"
    );
    // 加粗（** 与词边界的 __）/ 斜体 / 删除线
    s = s.replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>");
    s = s.replace(/(^|[^\w])__([^_]+)__(?=[^\w]|$)/g, "$1<strong>$2</strong>");
    s = s.replace(/\*([^*\n]+)\*/g, "<em>$1</em>");
    s = s.replace(/~~([^~]+)~~/g, "<del>$1</del>");
    // 还原行内代码占位
    s = s.replace(/\u0000(\d+)\u0000/g, (_m, idx) => codes[Number(idx)]);
    return s;
  }

  // —— GFM 表格：| a | b | 行序列，第二行分隔行（---/:--:）判定对齐
  function buildTable(rows) {
    const cells = (line) =>
      line
        .replace(/^\s*\|/, "")
        .replace(/\|\s*$/, "")
        .split("|")
        .map((c) => c.trim());
    const header = cells(rows[0]);
    const aligns = rows[1] ? cells(rows[1]) : [];
    const alignOf = (i) => {
      const a = aligns[i] || "";
      if (/^:.*:$/.test(a)) return ' style="text-align:center"';
      if (/:$/.test(a)) return ' style="text-align:right"';
      return "";
    };
    let html = "<table><thead><tr>";
    for (const [i, h] of header.entries()) {
      html += "<th" + alignOf(i) + ">" + inline(h) + "</th>";
    }
    html += "</tr></thead><tbody>";
    for (const row of rows.slice(2)) {
      html += "<tr>";
      for (const [i, c] of cells(row).entries()) {
        html += "<td" + alignOf(i) + ">" + inline(c) + "</td>";
      }
      html += "</tr>";
    }
    html += "</tbody></table>";
    return html;
  }

  // —— 主流程：逐行状态机
  function mdToHtml(markdown) {
    const lines = String(markdown || "").replace(/\r\n?/g, "\n").split("\n");
    const out = [];
    let para = []; // 段落行缓冲
    let quote = []; // 引用行缓冲
    let list = null; // null | {tag:"ul"|"ol", items:[]}
    let code = null; // null | {lang, lines:[]}
    let table = null; // 表格行缓冲

    const flushPara = () => {
      if (para.length) {
        out.push("<p>" + inline(para.join("\n")) + "</p>");
        para = [];
      }
    };
    const flushQuote = () => {
      if (quote.length) {
        out.push("<blockquote><p>" + inline(quote.join("\n")) + "</p></blockquote>");
        quote = [];
      }
    };
    const flushList = () => {
      if (list) {
        out.push(
          "<" + list.tag + ">" + list.items.map((it) => "<li>" + inline(it) + "</li>").join("") + "</" + list.tag + ">"
        );
        list = null;
      }
    };
    const flushTable = () => {
      if (table && table.length >= 2) out.push(buildTable(table));
      table = null;
    };
    const flushAll = () => {
      flushPara();
      flushQuote();
      flushList();
      flushTable();
    };

    for (const raw of lines) {
      const line = raw;

      // 围栏代码块优先（内部不做任何行内处理）
      const fence = /^\s*```+\s*(\S*)\s*$/.exec(line);
      if (code) {
        if (fence) {
          out.push(
            '<pre><code' + (code.lang ? ' class="language-' + esc(code.lang) + '"' : "") + ">" +
              esc(code.lines.join("\n")) +
              "</code></pre>"
          );
          code = null;
        } else {
          code.lines.push(raw);
        }
        continue;
      }
      if (fence) {
        flushAll();
        code = { lang: fence[1] || "", lines: [] };
        continue;
      }

      // 空行 = 块结束
      if (!line.trim()) {
        flushAll();
        continue;
      }

      // 表格行（成组处理，第二行需为分隔行才算表格）
      if (/^\s*\|.*\|\s*$/.test(line)) {
        flushPara();
        flushQuote();
        flushList();
        if (!table) table = [];
        table.push(line);
        continue;
      }
      flushTable();

      // ATX 标题
      const h = /^(#{1,6})\s+(.*?)\s*#*\s*$/.exec(line);
      if (h) {
        flushAll();
        const lv = h[1].length;
        out.push("<h" + lv + ">" + inline(h[2]) + "</h" + lv + ">");
        continue;
      }

      // 分割线
      if (/^\s*(-{3,}|\*{3,}|_{3,})\s*$/.test(line)) {
        flushAll();
        out.push("<hr />");
        continue;
      }

      // 引用
      if (/^\s*>\s?/.test(line)) {
        flushPara();
        flushList();
        quote.push(line.replace(/^\s*>\s?/, ""));
        continue;
      }
      flushQuote();

      // 列表（同组同级；嵌套拍平 —— 降级定位）
      const ul = /^\s*[-*+]\s+(.*)$/.exec(line);
      const ol = /^\s*\d+[.)]\s+(.*)$/.exec(line);
      if (ul || ol) {
        flushPara();
        const tag = ul ? "ul" : "ol";
        if (!list || list.tag !== tag) {
          flushList();
          list = { tag, items: [] };
        }
        list.items.push((ul || ol)[1]);
        continue;
      }
      flushList();

      // 普通文本 → 段落
      para.push(line);
    }

    // 收尾：未闭合的围栏代码按代码块输出
    if (code) {
      out.push(
        '<pre><code' + (code.lang ? ' class="language-' + esc(code.lang) + '"' : "") + ">" +
          esc(code.lines.join("\n")) +
          "</code></pre>"
      );
    }
    flushAll();

    return out.join("\n");
  }

  ADL.syncMdHtml = { mdToHtml, inline, esc };
})();
