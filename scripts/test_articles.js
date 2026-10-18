// 文章适配层单元测试（node scripts/test_articles.js）
// 覆盖：turndown 转换（标题/代码/列表/表格/行内样式）、articles 适配器
// （选择器 fallback / 懒加载图 / 排版 HTML / Markdown / 微信去重）、
// 注册表正则与 ARTICLE_SITES 交叉一致。
// 零依赖：用迷你 DOM 桩复现适配器契约用到的 DOM 子集。
"use strict";
const path = require("path");

globalThis.self = globalThis;

const A = "extension/adapters";
require(path.join("..", A, "types.js"));
require(path.join("..", A, "core.js"));
require(path.join("..", A, "turndown.js"));
require(path.join("..", A, "articles.js"));
require(path.join("..", A, "wechat-article.js"));

let pass = 0, fail = 0;
function ok(cond, label, extra) {
  if (cond) { pass++; console.log("  ok  " + label); }
  else { fail++; console.log("  FAIL " + label + (extra !== undefined ? "  => " + JSON.stringify(extra) : "")); }
}
function eq(a, b, label) {
  if (a === b) { pass++; console.log("  ok  " + label); }
  else { fail++; console.log("  FAIL " + label + "\n    期望: " + JSON.stringify(b) + "\n    实际: " + JSON.stringify(a)); }
}

// ---------------------------------------------------------------- 迷你 DOM 桩
// 支持：createElement/appendChild/cloneNode(true)/remove/文本节点/
//       getAttribute/setAttribute/removeAttribute/classList 不需要/
//       querySelector(All)（简单选择器：tag、.class、#id、组合、后代空格、
//       meta[property="x"] 属性选择器）

const escHtmlStub = (t) =>
  String(t).replace(/&/g, "&amp;").replace(/</g, "&lt;");
function serialize(n) {
  if (n.nodeType === 3) return escHtmlStub(n.text);
  const tag = n.nodeName.toLowerCase();
  const attrs = Object.entries(n.attrs)
    .map(([k, v]) => ` ${k}="${String(v).replace(/"/g, "&quot;")}"`)
    .join("");
  return `<${tag}${attrs}>` + n.childNodes.map(serialize).join("") + `</${tag}>`;
}

function makeNode(tag) {
  const node = {
    nodeType: 1,
    nodeName: tag.toUpperCase(),
    childNodes: [],
    attrs: {},
    style: {},
    get parentNode() { return node._parent || null; },
    setAttribute(k, v) { node.attrs[k.toLowerCase()] = String(v); },
    getAttribute(k) {
      const v = node.attrs[k.toLowerCase()];
      return v === undefined ? null : v;
    },
    removeAttribute(k) { delete node.attrs[k.toLowerCase()]; },
    appendChild(c) {
      if (typeof c.remove === "function") c.remove();
      c._parent = node;
      node.childNodes.push(c);
      return c;
    },
    remove() {
      if (node._parent) {
        const i = node._parent.childNodes.indexOf(node);
        if (i >= 0) node._parent.childNodes.splice(i, 1);
        node._parent = null;
      }
    },
    cloneNode(deep) {
      const c = makeNode(node.nodeName);
      c.attrs = { ...node.attrs };
      if (deep) {
        for (const ch of node.childNodes) c.appendChild(ch.cloneNode(true));
      }
      return addQueryApi(c); // 真实 DOM 克隆自带方法；桩需重挂 query API
    },
    get textContent() {
      return node.childNodes
        .map((n) => (n.nodeType === 3 ? n.text : n.textContent))
        .join("");
    },
    get outerHTML() { return serialize(node); },
    set textContent(v) { node.childNodes = [textNode(v)]; },
  };
  return node;
}

// 文本节点：适配器/转换器一律经 textContent 读文本（真实 DOM 形态）
function textNode(s) {
  return {
    nodeType: 3,
    text: String(s),
    get textContent() { return this.text; },
    cloneNode() { return textNode(this.text); },
  };
}

function el(tag, attrs, ...children) {
  const n = makeNode(tag);
  for (const k in attrs || {}) n.setAttribute(k, attrs[k]);
  for (const c of children) {
    if (typeof c === "string") n.childNodes.push(textNode(c));
    else n.appendChild(c);
  }
  return n;
}

function matchesCompound(node, compound) {
  // compound: h1 / .cls / #id / tag.class / meta[property="og:title"]
  const attrM = /^(.*?)\[(\w+)="([^"]*)"\]$/.exec(compound);
  let base = compound;
  if (attrM) {
    base = attrM[1];
    if (String(node.getAttribute(attrM[2]) || "") !== attrM[3]) return false;
    if (!base) return true;
  }
  const tagM = /^([a-z0-9]*)?((?:[.#][\w-]+)*)$/i.exec(base);
  if (!tagM) return false;
  const [, tag, clsPart] = tagM;
  if (tag && node.nodeName.toLowerCase() !== tag.toLowerCase()) return false;
  const classes = [];
  let id = null;
  for (const part of (clsPart || "").split(/(?=[.#])/)) {
    if (!part) continue;
    if (part[0] === ".") classes.push(part.slice(1));
    else if (part[0] === "#") id = part.slice(1);
  }
  if (id && node.getAttribute("id") !== id) return false;
  for (const c of classes) {
    const cls = String(node.getAttribute("class") || "");
    if (!new RegExp("(?:^|\\s)" + c + "(?:\\s|$)").test(cls)) return false;
  }
  return true;
}

function querySelectorAll(root, selector) {
  // 逗号多选择器（articles 的 TOC 用 "h2,h3,h4"）：各部分独立匹配后按
  // DFS 文档序合并去重（真实 querySelectorAll 语义）
  const dfsSeq = new Map();
  let seq = 0;
  const number = (n) => {
    for (const ch of n.childNodes || []) {
      if (ch.nodeType === 1) {
        dfsSeq.set(ch, seq++);
        number(ch);
      }
    }
  };
  number(root);
  dfsSeq.set(root, -1); // root 自身命中（适配器在 clone 上用 content/root 选择器）
  const out = new Set();
  for (const part of selector.split(",")) {
    const compounds = part.trim().split(/\s+/);
    if (!compounds[0]) continue;
    if (matchesCompound(root, compounds[0])) out.add(root);
    const walk = (node, depth) => {
      for (const ch of node.childNodes || []) {
        if (ch.nodeType !== 1) continue;
        if (matchesCompound(ch, compounds[depth])) {
          if (depth === compounds.length - 1) out.add(ch);
          else walk(ch, depth + 1);
        }
        // 后代语义：无论当前节点是否命中，子树继续按当前层查找
        //（单层选择器也必须下钻，否则非命中中间层会截断整棵子树）
        walk(ch, depth);
      }
    };
    walk(root, 0);
  }
  return [...out].sort((a, b) => dfsSeq.get(a) - dfsSeq.get(b));
}

function addQueryApi(node) {
  node.querySelector = (s) => querySelectorAll(node, s)[0] || null;
  node.querySelectorAll = (s) => querySelectorAll(node, s);
  for (const ch of node.childNodes) if (ch.nodeType === 1) addQueryApi(ch);
  return node;
}

// 建文档：addQueryApi(document-like)
function doc(rootEl) {
  const d = {
    nodeType: 9,
    childNodes: [rootEl],
    title: "",
    get images() { return querySelectorAll(rootEl, "img"); },
    querySelector(s) { return querySelectorAll(rootEl, s)[0] || null; },
    querySelectorAll(s) { return querySelectorAll(rootEl, s); },
  };
  return d;
}

// ---------------------------------------------------------------- turndown

console.log("turndown: 标题与段落");
{
  const root = el("div", {},
    el("h1", {}, "一级标题"),
    el("h3", {}, "三级标题"),
    el("p", {}, "第一段文本。"),
    el("p", {}, "第二段，含", el("br"), "硬换行。")
  );
  const md = ADL.turndown(addQueryApi(root));
  ok(md.includes("# 一级标题"), "h1 → # 前缀", md);
  ok(md.includes("### 三级标题"), "h3 → ### 前缀", md);
  ok(/\n\n/.test(md), "块间空行分隔", md);
}

console.log("turndown: 行内样式");
{
  const root = el("p", {},
    "普通", el("strong", {}, "加粗"), "和", el("em", {}, "斜体"),
    "与", el("del", {}, "删除"), "及", el("code", {}, "code_here"), "结束"
  );
  const md = ADL.turndown(addQueryApi(root)).trim();
  eq(md, "普通**加粗**和*斜体*与~~删除~~及`code_here`结束", "strong/em/del/code 定界符");
}
{
  // 强调内容首尾空白移出定界符：`** bar**` 不渲染
  const root = el("p", {}, "前 ", el("strong", {}, " 空白加粗 "), " 后");
  const md = ADL.turndown(addQueryApi(root)).trim();
  eq(md, "前 **空白加粗** 后", "强调定界符外侧空白保留", md);
}

console.log("turndown: 链接与图片");
{
  const root = el("div", {},
    el("p", {}, el("a", { href: "/post/1" }, "站内链接")),
    el("p", {}, el("img", { src: "//cdn.example.com/a.png", alt: "图" }))
  );
  const md = ADL.turndown(addQueryApi(root), { baseUrl: "https://blog.example.com/x" });
  ok(md.includes("[站内链接](https://blog.example.com/post/1)"), "相对链接按 baseUrl 绝对化", md);
  ok(md.includes('![图](https://cdn.example.com/a.png)'), "协议相对图片绝对化", md);
}

console.log("turndown: 代码块");
{
  const root = el("div", {},
    el("pre", {}, el("code", { class: "language-rust hljs" }, "fn main() {\n    println!(\"hi\");\n}")),
    el("pre", { class: "brush: js;" }, "let x = 1;")
  );
  const md = ADL.turndown(addQueryApi(root));
  ok(md.includes("```rust\nfn main() {"), "围栏代码块 + language- 语言探测", md);
  ok(md.includes("```js\nlet x = 1;"), "brush: 语法高亮语言探测", md);
}

console.log("turndown: 列表（嵌套）");
{
  const root = el("ul", {},
    el("li", {}, "第一项"),
    el("li", {},
      "第二项",
      el("ul", {}, el("li", {}, "子项 A"), el("li", {}, "子项 B"))
    ),
    el("li", {}, "第三项")
  );
  const md = ADL.turndown(addQueryApi(root));
  const lines = md.split("\n");
  eq(lines[0], "- 第一项", "无序列表标记");
  eq(lines[1], "- 第二项", "第二项");
  eq(lines[2], "  - 子项 A", "嵌套列表缩进 2 空格", lines);
  eq(lines[3], "  - 子项 B", "嵌套第二项");
  eq(lines[4], "- 第三项", "嵌套后回到外层");
}
{
  const root = el("ol", { start: "2" }, el("li", {}, "乙"), el("li", {}, "丙"));
  const md = ADL.turndown(addQueryApi(root));
  ok(md.includes("2. 乙") && md.includes("3. 丙"), "有序列表 start 属性生效", md);
}

console.log("turndown: GFM 表格");
{
  const root = el("table", {},
    el("thead", {}, el("tr", {}, el("th", {}, "列A"), el("th", {}, "列B"))),
    el("tbody", {},
      el("tr", {}, el("td", {}, "a1"), el("td", {}, el("strong", {}, "b1"))),
      el("tr", {}, el("td", {}, "a|2"), el("td", {}, "b2"))
    )
  );
  const md = ADL.turndown(addQueryApi(root));
  const lines = md.split("\n");
  eq(lines[0], "| 列A | 列B |", "表头行");
  eq(lines[1], "| --- | --- |", "分隔行");
  ok(lines[2].includes("**b1**"), "单元格行内样式保留", lines);
  ok(lines[3].includes("a\\|2"), "单元格竖线转义", lines);
}

console.log("turndown: 表格列对齐与 figure/figcaption");
{
  const root = el("table", {},
    el("thead", {}, el("tr", {},
      el("th", { style: "text-align:center" }, "居中"),
      el("th", { style: "text-align:right" }, "右对齐"),
      el("th", { align: "left" }, "左对齐"),
      el("th", {}, "默认")
    )),
    el("tbody", {}, el("tr", {},
      el("td", {}, "a"), el("td", {}, "b"), el("td", {}, "c"), el("td", {}, "d")
    ))
  );
  const md = ADL.turndown(addQueryApi(root));
  const lines = md.split("\n");
  eq(lines[1], "| :---: | ---: | :--- | --- |", "th 对齐 → GFM 分隔行标记", lines);
}
{
  const root = el("div", {},
    el("figure", {},
      el("img", { src: "https://cdn.example.com/f.png", alt: "架构图" }),
      el("figcaption", {}, "图 1：系统架构")
    ),
    el("p", {}, "正文")
  );
  const md = ADL.turndown(addQueryApi(root), { baseUrl: "https://blog.example.com/" });
  ok(md.includes("![架构图](https://cdn.example.com/f.png)\n\n*图 1：系统架构*\n\n正文"), "figure 图片 + figcaption 斜体说明", md);
}

console.log("turndown: 引用与分割线与行首转义");
{
  const root = el("div", {},
    el("blockquote", {}, el("p", {}, "引用内容"), el("p", {}, "第二行")),
    el("hr"),
    el("p", {}, "- 不是列表项")
  );
  const md = ADL.turndown(addQueryApi(root));
  ok(md.includes("> 引用内容\n> \n> 第二行"), "多段引用逐行 > 前缀", md);
  ok(md.includes("\n\n---\n\n"), "hr → ---", md);
  ok(md.includes("\\- 不是列表项"), "行首列表符转义", md);
}

console.log("turndown: 脚本样式剔除与空输出");
{
  const root = el("div", {},
    el("script", {}, "var x = 1;"),
    el("style", {}, ".a{}"),
    el("p", {}, "正文")
  );
  const md = ADL.turndown(addQueryApi(root)).trim();
  eq(md, "正文", "script/style 不进 Markdown");
  eq(ADL.turndown(null), "", "null 输入安全");
}

// ---------------------------------------------------------------- articles 适配器

function articleCtx(url, documentLike) {
  return {
    url,
    captured: () => [],
    inlineJson: () => null,
    dom: () => documentLike,
  };
}

console.log("articles: 注册表与站点表交叉一致");
{
  for (const s of ADL.ARTICLE_SITES) {
    const probe = "https://" + (s.re.source.includes("mp\\.weixin")
      ? "mp.weixin.qq.com/s/abc"
      : s.re.source.includes("csdn")
      ? "blog.csdn.net/x/article/1"
      : s.re.source.includes("zhuanlan")
      ? "zhuanlan.zhihu.com/p/1"
      : s.re.source.includes("zhihu")
      ? "www.zhihu.com/question/123/answer/456"
      : s.re.source.includes("bilibili")
      ? "www.bilibili.com/read/cv123"
      : s.re.source.includes("sspai")
      ? "sspai.com/post/123"
      : s.re.source.includes("jianshu")
      ? "www.jianshu.com/p/abc"
      : s.re.source.includes("juejin")
      ? "juejin.cn/post/1"
      : s.re.source.includes("cnblogs")
      ? "www.cnblogs.com/blog/p/1.html"
      : s.re.source.includes("51cto")
      ? "blog.51cto.com/u_1/1"
      : s.re.source.includes("jb51")
      ? "www.jb51.net/article/1.htm"
      : s.re.source.includes("php")
      ? "www.php.cn/faq/1.html"
      : s.re.source.includes("segmentfault")
      ? "segmentfault.com/a/119000001"
      : s.re.source.includes("oschina")
      ? "my.oschina.net/u/1/blog/1"
      : "medium.com/@u/xxx-123");
    ok(
      s.re.test(probe) && ADL.matchAdapters(probe).some((a) => a.id === "articles"),
      `注册表 re 命中 ${s.id}（${probe}）`,
      probe
    );
  }
}

console.log("articles: CSDN（MD 文档 + 图片 + 清理）");
{
  const content = el("div", { id: "content_views" },
    el("p", {}, "正文段落 ", el("strong", {}, "重点")),
    el("pre", {}, el("code", { class: "language-python hljs" }, "print(1)")),
    el("p", {},
      el("img", { src: "https://i-blog.csdnimg.cn/a.png" }),
      el("img", { "data-src": "https://i-blog.csdnimg.cn/lazy.jpg", src: "data:image/gif;base64,xxx" }),
      el("img", { src: "https://i-blog.csdnimg.cn/a.png?x=1" })
    ),
    el("span", { class: "hljs-button" }, "复制")
  );
  const page = doc(el("div", { class: "blog-content-box" },
    el("h1", { class: "title-article", id: "activity-name" }, "CSDN 测试文章"),
    content
  ));
  addQueryApi(page.childNodes[0]);
  const rs = ADL.adapters["articles"].extract(articleCtx("https://blog.csdn.net/u/article/details/123", page));
  const docs = rs.filter((r) => r.kind === "document");
  const imgs = rs.filter((r) => r.kind === "image");
  eq(docs.length, 1, "唯一文档资源（Markdown）", docs.map((d) => d.filename));
  eq(imgs.length, 2, "图片：直链 + 懒加载 data-src（同图 query 变体去重）", imgs.map((i) => i.url));
  ok(imgs.every((i) => /^https:/.test(i.url) && i.filename.startsWith("CSDN 测试文章_")), "图片文件名带标题前缀", imgs.map((i) => i.filename));

  const md = docs[0].meta.document.content;
  eq(docs[0].filename, "CSDN 测试文章.md", "MD 文件名");
  // YAML front matter 元数据
  ok(md.startsWith("---\ntitle: \"CSDN 测试文章\"\nsource: \"https://blog.csdn.net/u/article/details/123\"\nsite: \"CSDN\"\n"), "front matter（title/source/site）", md.split("\n").slice(0, 6));
  ok(md.includes('exported: "'), "front matter 导出日期");
  ok(md.includes("# CSDN 测试文章"), "H1 标题");
  ok(md.includes("**重点**"), "MD 行内样式");
  ok(md.includes("```python\nprint(1)"), "MD 围栏代码块", md);
  ok(/!\[\]\(https:\/\/i-blog\.csdnimg\.cn\/a\.png/.test(md), "MD 图片外链");
  ok(!md.includes("data:image/gif"), "懒加载图落地为真实地址", md.match(/!\[\]\([^)]*\)/g));
  ok(!md.includes("复制"), "复制按钮不进 MD");
  ok(!docs.some((d) => d.filename.endsWith(".html")), "HTML 导出已移除（仅 MD）");
}

console.log("articles: 微信公众号（懒加载 + 隐藏样式 + 不重复收图）");
{
  const jsContent = el("div", { id: "js_content", style: "visibility: hidden; opacity: 0;" },
    el("p", {}, "微信正文"),
    el("img", { "data-src": "https://mmbiz.qpic.cn/abc/640?wx_fmt=jpeg", src: "data:image/svg+xml,placeholder" })
  );
  const page = doc(el("div", { id: "img-content" },
    el("h1", { class: "rich_media_title", id: "activity-name" }, "公众号文章标题"),
    jsContent
  ));
  addQueryApi(page.childNodes[0]);
  const rs = ADL.adapters["articles"].extract(articleCtx("https://mp.weixin.qq.com/s/abc123", page));
  ok(rs.every((r) => r.kind === "document"), "微信站图片由 wechat-article 适配器专管，articles 不收图", rs.map((r) => r.kind));
  const md = rs.find((r) => r.filename.endsWith(".md")).meta.document.content;
  // 微信排版依赖内联样式，MD 化后样式丢弃是预期；隐藏样式不影响文本提取
  ok(md.includes("微信正文"), "正文文本提取");
  ok(md.includes("![ ]];") === false && /!\[\]/.test(md), "懒加载图进 MD 图片引用", md.match(/!\[[^\]]*\]\([^)]*\)/g));
  const rsWx = ADL.adapters["wechat-article"].extract({
    ...articleCtx("https://mp.weixin.qq.com/s/abc123", page),
  });
  ok(rsWx.some((r) => r.kind === "image" && r.url.includes("mmbiz")), "wechat-article 适配器仍独立收图（双适配器分工）");
}

console.log("articles: 知乎懒加载 data-actualsrc + 掘金选择器 fallback");
{
  const z = doc(el("article", { class: "Post-Main" },
    el("h1", { class: "Post-Title" }, "知乎专栏文章"),
    el("div", { class: "Post-RichTextContainer" },
      el("p", {}, "内容"),
      el("img", { "data-actualsrc": "https://pic1.zhimg.com/80/v2-a.jpg", src: "https://picx.zhimg.com/80/v2-a_b.jpg" })
    )
  ));
  addQueryApi(z.childNodes[0]);
  const rs = ADL.adapters["articles"].extract(articleCtx("https://zhuanlan.zhihu.com/p/100", z));
  const imgs = rs.filter((r) => r.kind === "image");
  ok(imgs.some((i) => i.url.includes("pic1.zhimg.com/80/v2-a.jpg")), "知乎 data-actualsrc 优先于 src 缩放图", imgs.map((i) => i.url));
  eq(rs.filter((r) => r.kind === "image").length, 1, "同图双源去重为一张", imgs.map((i) => i.url));

  const jj = doc(el("div", { class: "article-main" },
    el("h1", { class: "article-title" }, "掘金文章"),
    el("div", { class: "article-content" },
      el("p", {}, "掘金正文"),
      el("pre", {}, el("code", { class: "hljs javascript" }, "const a = 1;"))
    )
  ));
  addQueryApi(jj.childNodes[0]);
  const rsJ = ADL.adapters["articles"].extract(articleCtx("https://juejin.cn/post/7123", jj));
  const mdJ = rsJ.find((r) => r.filename.endsWith(".md")).meta.document.content;
  ok(mdJ.includes("```javascript"), "掘金 hljs javascript 语言探测", mdJ);
  ok(rsJ.some((r) => r.kind === "document" && r.filename === "掘金文章.md"), "fallback root（article-main）命中");
}

console.log("articles: 标题清洗与非法字符");
{
  const page = doc(el("div", { class: "blog-content-box" },
    el("h1", { class: "title-article" }, "标题: 含*非法/字符?  - CSDN博客"),
    el("div", { id: "content_views" }, el("p", {}, "x"))
  ));
  addQueryApi(page.childNodes[0]);
  const rs = ADL.adapters["articles"].extract(articleCtx("https://blog.csdn.net/u/a/1", page));
  const mdDoc = rs.find((r) => r.filename.endsWith(".md"));
  ok(!/[\\/:*?"<>|]/.test(mdDoc.filename.replace(/\.md$/, "")), "文件名非法字符已清洗", mdDoc.filename);
  ok(!mdDoc.filename.includes("CSDN博客"), "站点后缀已剥", mdDoc.filename);
  // front matter 内 title 已做 YAML 转义（引号/反斜杠不破坏结构）
  const fm = mdDoc.meta.document.content.split("---")[1] || "";
  ok(/^title: "/.test(fm.trim()), "front matter title 双引号包裹", fm);
}

console.log("articles: 未命中/无正文安全");
{
  eq(ADL.articleSite("https://example.com/x"), null, "非文章站不命中");
  eq(ADL.adapters["articles"].extract(articleCtx("https://example.com/x", doc(el("div")))).length, 0, "未命中返回空");
  // root 缺失（非文章页）→ 空；root 存在但 content 选择器缺失 → 降级导出 root
  const page0 = doc(el("div", { class: "whatever" }));
  addQueryApi(page0.childNodes[0]);
  eq(ADL.adapters["articles"].extract(articleCtx("https://blog.csdn.net/x/1", page0)).length, 0, "无 root 返回空");
}

console.log("rewriteDocRefs: 文章合集相对引用重写（MD）");
{
  const byUrl = {
    "https://i-blog.csdnimg.cn/a.png": "标题_01.png",
    "https://mmbiz.qpic.cn/abc/0": "标题_02.jpeg",
  };
  // 普通文件名直写；未命中 URL 原样保留
  const md =
    "![图1](https://i-blog.csdnimg.cn/a.png?x=1)\n\n![图2](https://none.example/b.png)";
  const mdOut = ADL.rewriteDocRefs(md, byUrl);
  ok(mdOut.includes("![图1](标题_01.png)"), "MD 相对引用重写", mdOut);
  ok(mdOut.includes("![图2](https://none.example/b.png)"), "MD 未命中保留", mdOut);
  ok(!mdOut.includes("标题_02.jpeg"), "内容里不存在的 URL 不误插", mdOut);
  // 空格的真实形态是 %20 编码（turndown 输出经 absUrl）：命中 → 角链接防截断；
  // 裸空格 URL（不合法，正则不吞）安全跳过
  const md2 = ADL.rewriteDocRefs(
    "![](https://x.example/p%201.png)\n\n![](https://y.example/raw 1.png)",
    { "https://x.example/p%201.png": "标题 (1).jpeg" }
  );
  ok(md2.includes("](<标题 (1).jpeg>)"), "含空格/括号文件名用角链接防截断", md2);
  ok(md2.includes("](https://y.example/raw 1.png)"), "裸空格 URL 原样跳过", md2);

  // 防御：空入参原样返回
  eq(ADL.rewriteDocRefs("", byUrl), "", "空内容安全");
  eq(ADL.rewriteDocRefs("x", null), "x", "空映射安全");
}

console.log("articles: 微信跨适配器合集（mmbiz URL 一致性端到端，MD 路径）");
{
  // articles 的 postCleanup 把 mmbiz src 归一化为 /0 形态 → turndown 读到的
  // src 与 wechat-article 适配器输出的图片 URL 同形 → rewriteDocRefs 按剥
  // query 键命中（MD 图片链接重写）
  const jsContent = el("div", { id: "js_content" },
    el("p", {}, "正文"),
    el("img", { "data-src": "https://mmbiz.qpic.cn/mmbiz_jpg/ABC/640?wx_fmt=jpeg&tp=webp&wx_lazy=1", src: "data:image/svg+xml,ph" })
  );
  const page = doc(el("div", { id: "img-content" },
    el("h1", { class: "rich_media_title" }, "微信合集测试"),
    jsContent
  ));
  addQueryApi(page.childNodes[0]);
  const rsDocs = ADL.adapters["articles"].extract(articleCtx("https://mp.weixin.qq.com/s/xyz", page));
  const md = rsDocs.find((r) => r.filename.endsWith(".md")).meta.document.content;
  // 归一化语义与 wechat-article 相同：路径 /0 化 + 剥 tp=webp；wx_lazy 等无害标记保留
  ok(md.includes("](https://mmbiz.qpic.cn/mmbiz_jpg/ABC/0?wx_fmt=jpeg&wx_lazy=1)"), "MD 图片链接已归一化（/0、tp=webp 已剥）", md.match(/!\[\]\([^)]*\)/g));

  const rsImgs = ADL.adapters["wechat-article"].extract(articleCtx("https://mp.weixin.qq.com/s/xyz", page));
  const img = rsImgs.find((r) => r.kind === "image");
  ok(!!img, "wechat-article 输出图片资源");
  // 跨适配器一致性：两边剥 query 后同键 → background 合集重写必命中
  const relMap = {};
  relMap[img.url.split("#")[0].split("?")[0]] = img.filename;
  const rewritten = ADL.rewriteDocRefs(md, relMap);
  ok(rewritten.includes(`](${img.filename})`), "微信 MD 相对引用重写命中（合集模式打通）", rewritten.match(/!\[[^\]]*\]\([^)]*\)/g));
  ok(img.filename.endsWith(".jpeg"), "wx_fmt 推断扩展名", img.filename);
}

console.log("actions: 净化链接 / 原始链接 / 时间戳·指令·命名·海报 / PDF·Word 模板");
{
  require(path.join("..", A, "..", "..", "extension", "actions.js"));
  // 净化链接（code-box weixinPanel.tsx:63-99 白名单制）：微信 /s? 文章 URL 保留
  // 白名单参数，且按规则数组顺序重拼（__biz, idx, mid, sn, src…），非白名单剔除
  eq(
    ADL.actionCleanUrl("https://mp.weixin.qq.com/s?__biz=MzA=&mid=100&sn=abc&idx=2&chksm=x1&scene=126&src=3"),
    "https://mp.weixin.qq.com/s?__biz=MzA=&idx=2&mid=100&sn=abc&src=3",
    "微信按规则数组序重拼白名单参数"
  );
  // 通用站白名单制：id/q 命中保留，utm_*/spm 白名单外剔除
  eq(
    ADL.actionCleanUrl("https://blog.example.com/p/1?id=5&utm_source=x&spm=y&q=%E4%B8%AD"),
    "https://blog.example.com/p/1?id=5&q=%E4%B8%AD",
    "通用站白名单命中保留、追踪参数剔除"
  );
  // 通用站白名单制：非追踪的普通参数（p/foo）同样删除（白名单制 ≠ 黑名单制）
  eq(
    ADL.actionCleanUrl("https://blog.example.com/p/1?p=123&foo=bar"),
    "https://blog.example.com/p/1",
    "通用站白名单外参数一律删除"
  );
  // 微信规则仅命中 /s? 文章 URL：其余微信页面走通用白名单（不做全站豁免）
  eq(
    ADL.actionCleanUrl("https://mp.weixin.qq.com/mp/profile_ext?action=home&abc=x"),
    "https://mp.weixin.qq.com/mp/profile_ext",
    "微信非 /s? 页走通用白名单规则"
  );
  // 无 query 无 hash：两条规则都不命中 → 原 URL
  eq(ADL.actionCleanUrl("https://blog.example.com/p/1"), "https://blog.example.com/p/1", "无 query 无 hash 原样");
  // 仅 hash：通用规则命中但白名单为空 → base + 原 hash
  eq(
    ADL.actionCleanUrl("https://blog.example.com/p/1#sec-2"),
    "https://blog.example.com/p/1#sec-2",
    "仅 hash 原样保留"
  );
  // 白名单命中 + 末尾拼回原 hash
  eq(
    ADL.actionCleanUrl("https://blog.example.com/p/1?utm_source=x&id=5#top"),
    "https://blog.example.com/p/1?id=5#top",
    "白名单命中并拼回 hash"
  );
  // 非法 URL 原样返回
  eq(ADL.actionCleanUrl("not a url"), "not a url", "非法 URL 安全");

  // 原始链接：微信 __biz/mid/sn 重构（idx 缺省 1）——code-box 为空占位，
  // 此处为有意保留的真实重构实现
  eq(
    ADL.actionOriginalUrl("https://mp.weixin.qq.com/s?__biz=MzA=&mid=100&sn=abc&chksm=x&scene=1", doc(el("div"))),
    "https://mp.weixin.qq.com/s?__biz=MzA=&mid=100&idx=1&sn=abc",
    "微信原始链接重构"
  );
  // 兜底 og:url
  const pageOg = doc(el("head", {},
    el("meta", { property: "og:url", content: "https://blog.example.com/canonical" })
  ));
  eq(
    ADL.actionOriginalUrl("https://blog.example.com/p/1?utm_source=x", pageOg),
    "https://blog.example.com/canonical",
    "og:url 兜底"
  );

  console.log("actions: 时间戳与 md 文件名（tools.ts:64 格式）");
  eq(ADL.formatTimestamp(new Date(2026, 9, 10, 15, 4, 5)), "2026-10-10 15:04:05", "YYYY-MM-DD HH:mm:ss（dayjs 同款）");
  eq(ADL.formatTimestamp(new Date(2026, 0, 2, 3, 4, 5)), "2026-01-02 03:04:05", "月/日/时/分/秒补零");
  ok(
    /^标题-\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.md$/.test(ADL.mdFileName("标题")),
    "md 文件名 = 标题-时间戳.md",
    ADL.mdFileName("标题")
  );

  console.log("actions: 解析 Markdown AI 指令原文（parseMarkdownHook.ts:20-21）");
  eq(
    ADL.AI_INSTRUCTION,
    "\n    将上面的文字,翻译成中文并生成markdown",
    "换行 + 4 空格缩进指令原文"
  );
  ok(!/\n$/.test(ADL.AI_INSTRUCTION), "指令无结尾换行");

  console.log("actions: 下载所有图片命名（weixin.tsx:199-215 / download.ts:14,23）");
  eq(ADL.weixinImageExt("https://mmbiz.qpic.cn/a/640?wx_fmt=gif"), ".gif", "wx_fmt=gif → .gif");
  eq(ADL.weixinImageExt("https://mmbiz.qpic.cn/mmbiz_gif/ABC/640"), ".gif", "mmbiz_gif → .gif");
  eq(ADL.weixinImageExt("https://mmbiz.qpic.cn/a/640?wx_fmt=png"), ".png", "wx_fmt=png → .png");
  eq(ADL.weixinImageExt("https://mmbiz.qpic.cn/mmbiz_png/ABC/640"), ".png", "mmbiz_png → .png");
  eq(ADL.weixinImageExt("https://mmbiz.qpic.cn/a/640?wx_fmt=bmp"), ".bmp", "wx_fmt=bmp → .bmp");
  eq(ADL.weixinImageExt("https://mmbiz.qpic.cn/mmbiz_bmp/ABC/640"), ".bmp", "mmbiz_bmp → .bmp");
  eq(ADL.weixinImageExt("https://mmbiz.qpic.cn/a/640?wx_fmt=jpeg"), ".jpg", "wx_fmt=jpeg → 其余一律 .jpg");
  eq(ADL.weixinImageExt("https://mmbiz.qpic.cn/a/640?wx_fmt=webp"), ".jpg", "webp → .jpg（weixin.tsx 无 webp 分支）");
  eq(ADL.genericImageName("https://cdn.example.com/img/a.png?v=2", 3), "a.png", "通用条目名 = 路径末段去 query");
  eq(ADL.genericImageName("https://cdn.example.com/img/", 3), "image-3.jpg", "空段兜底 image-<index>.jpg");

  console.log("actions: 海报排版纯函数（makerQRPost.ts:60-81,133-138）");
  {
    // measureText 桩：每字符 100px 宽，350px 宽 → 3 字/行（越界字符换行起笔）
    const stubCtx = { font: "", measureText: (s) => ({ width: s.length * 100 }) };
    eq(
      ADL.posterWrapText("一二三四五六", "18px", 350, stubCtx).join("|"),
      "一二三|四五六",
      "逐字符 measureText 折行"
    );
    eq(ADL.posterWrapText("   ", "18px", 350, stubCtx).length, 0, "纯空白不折行（跳过摘要区）");
    eq(
      ADL.posterCardHeight(1, 0),
      36 * 2.5 + 18 * 1.6 + 300,
      "无摘要高度公式（条件项为 0）"
    );
    eq(
      ADL.posterCardHeight(2, 3),
      36 * 2.5 + 18 * 2 * 1.6 + 16 * 3 * 1.6 + 18 + 300,
      "有摘要高度公式（含 margin/2 条件项）"
    );
  }

  // PDF/Word 模板：标题转义 + 正文 outerHTML + Word 命名空间
  const env = {
    title: '标题 " 引号<测>',
    site: { label: "CSDN" },
    pageUrl: "https://blog.csdn.net/u/a/1",
    contentEl: (() => {
      const c = el("div", { id: "content_views" }, el("p", {}, "段落"));
      return addQueryApi(c);
    })(),
  };
  const printHtml = ADL.actionBuildPrintHtml(env);
  ok(printHtml.includes("<title>标题 &quot; 引号&lt;测&gt;</title>"), "打印模板标题转义", printHtml.match(/<title>.*<\/title>/));
  ok(printHtml.includes("https://blog.csdn.net/u/a/1"), "打印模板带原文链接");
  ok(printHtml.includes("段落"), "打印模板含正文");
  const wordHtml = ADL.actionBuildWordHtml(env);
  ok(wordHtml.includes("urn:schemas-microsoft-com:office:word"), "Word 模板 office 命名空间");
  ok(wordHtml.includes("宋体"), "Word 模板中文正文样式");
  ok(wordHtml.includes("段落"), "Word 模板含正文");
}

// ---------------------------------------------------------------- 收图任务（actions collectImageJobs）
// 「下载所有图片」的前置：字节抓取在 MV3 service worker（background zipDownload），
// data: 与协议相对 //host 两类 URL 在 SW 上下文必失败，收集侧须先归一化
console.log("actions: 收图任务（data: 过滤 + 协议相对补全，SW 抓取前置）");
{
  // isWeixinPage → ADL.articleSite(location.href)：Node 无 location/document，注入桩
  globalThis.location = { href: "https://mp.weixin.qq.com/s?__biz=MzA=&mid=1&idx=1&sn=abc" };
  const img = (ds, src) => ({ dataset: { src: ds || "" }, src: src || "" });
  const jsContent = {
    getElementsByTagName: () => [
      img("//img01.mmbiz.qpic.cn/mmbiz_jpg/A1/640?wx_fmt=jpeg", "data:image/svg+xml,ph"), // // 开头 mmbiz：须补全
      img("data:image/svg+xml,placeholder", ""), // data: 占位：须剔除
      img("", "https://mmbiz.qpic.cn/mmbiz_png/B2/640?wx_fmt=png"), // 绝对 https：原样（dataset 空 → src）
      img("//res.wx.qq.com/mmbizwap/zh_CN/htmledition/images/icon.png", ""), // mmbizwap 特例
    ],
  };
  globalThis.document = {
    title: "微信合集测试",
    querySelector: (sel) => (sel === "#js_content" ? jsContent : null),
    images: [],
  };
  const jobs = ADL.collectImageJobs();
  eq(jobs.length, 3, "微信 4 张源图 → 3 个任务（data: 剔除）");
  eq(jobs[0].url, "https://img01.mmbiz.qpic.cn/mmbiz_jpg/A1/640?wx_fmt=jpeg",
    "协议相对 //mmbiz 补全 https（SW 内按 worker 基址解析成 chrome-extension://host 必失败）");
  eq(jobs[0].name, "微信合集测试-0.jpg", "补全后条目名 wx_fmt=jpeg → .jpg");
  eq(jobs[1].url, "https://mmbiz.qpic.cn/mmbiz_png/B2/640?wx_fmt=png", "绝对 https URL 原样保留");
  eq(jobs[1].name, "微信合集测试-1.png", "wx_fmt=png → .png");
  eq(jobs[2].url, "https://res.wx.qq.com/mmbizwap/zh_CN/htmledition/images/icon.png",
    "mmbizwap // 补全（weixin.tsx:176-188 特例被统一规则覆盖）");
  ok(jobs.every((j) => /^https:/i.test(j.url)), "所有任务 URL 均为绝对 https（SW fetch 前置满足）",
    jobs.map((j) => j.url));

  // 通用站：document.images 的 src 由浏览器保证绝对，http(s) 去重、非 http(s) 剔除
  globalThis.location = { href: "https://juejin.cn/post/1" };
  globalThis.document = {
    title: "通用页",
    querySelector: () => null,
    images: [
      { src: "https://cdn.example.com/a.png?v=2" },
      { src: "https://cdn.example.com/a.png?v=2" }, // 去重
      { src: "data:image/gif;base64,x" }, // 非 http(s) 剔除（原行为）
      { src: "" },
    ],
  };
  const gjobs = ADL.collectImageJobs();
  eq(gjobs.length, 1, "通用站去重后 1 个任务");
  eq(gjobs[0].name, "a.png", "通用条目名 = 路径末段去 query");
  delete globalThis.document;
  delete globalThis.location;
}

console.log("turndown: 真实 DOM NodeList 形态（回归：childNodes 无 filter/map）");
{
  // 真实 DOM 的 childNodes 是 NodeList：可迭代但没有 filter/map 等数组方法。
  // 曾因 (n.childNodes||[]).filter 直接 TypeError —— 文章菜单/MD 导出在真实页面
  // 全部静默失效（node 测试桩用普通数组故未发现）。
  const nodelist = (arr) => {
    // 用自有属性遮蔽原型上的数组方法（delete 删不掉原型链，桩会失真），
    // 效果与真实 NodeList 一致：可迭代、有 length、调用 .filter/.map 即 TypeError
    const nl = arr.slice();
    for (const m of ["filter", "map", "forEach", "slice", "concat", "join"]) {
      Object.defineProperty(nl, m, { value: undefined });
    }
    return nl;
  };
  const toNodeList = (n) => {
    n.childNodes = nodelist(n.childNodes);
    for (const c of [...n.childNodes]) if (c.nodeType === 1) toNodeList(c);
  };
  const inner = el("div", {}, el("p", {}, "段落一"), el("strong", {}, "加粗"));
  const root = el("div", {}, inner);
  toNodeList(root);
  addQueryApi(root);
  const md = ADL.turndown(root);
  ok(md.includes("段落一") && md.includes("**加粗**"), "NodeList 形态下 turndown 正常输出", md);
  ok(typeof inner.childNodes.filter === "undefined", "确认测试对象确实无 filter（防桩失真）");
}

// ---------------------------------------------------------------- 能力增强（ext 0.8.6）
console.log("turndown: GFM 任务列表（li 内 checkbox → - [ ]/[x]）");
{
  const root = el("ul", {},
    el("li", {}, el("input", { type: "checkbox", checked: "" }), "已完成项"),
    el("li", {}, el("input", { type: "checkbox" }), "待办项"),
    el("li", {}, "普通项")
  );
  const md = ADL.turndown(root);
  ok(md.includes("- [x] 已完成项"), "已勾选 → [x]", md);
  ok(md.includes("- [ ] 待办项"), "未勾选 → [ ]", md);
  ok(md.includes("- 普通项"), "无 checkbox 仍是普通列表项", md);
}

console.log("turndown: 嵌入媒体 → 链接（iframe/video 不再丢失）");
{
  const root = el("div", {},
    el("p", {}, "看视频："),
    el("iframe", { src: "https://player.bilibili.com/player.html?bvid=BV1xx" }),
    el("video", { "data-src": "https://cdn.example.com/a.mp4" }),
    el("audio", { src: "https://cdn.example.com/a.mp3" }),
    el("video", {}, el("source", { src: "https://cdn.example.com/b.mp4" }))
  );
  const md = ADL.turndown(root);
  ok(md.includes("[▶ 视频](https://player.bilibili.com/player.html?bvid=BV1xx)"), "iframe → 视频链接", md);
  ok(md.includes("[▶ 视频](https://cdn.example.com/a.mp4)"), "video data-src 懒加载", md);
  ok(md.includes("[♪ 音频](https://cdn.example.com/a.mp3)"), "audio → 音频链接", md);
  ok(md.includes("[▶ 视频](https://cdn.example.com/b.mp4)"), "video 无 src 取 source 子元素", md);
  ok(!/IFRAME|<video/i.test(md), "不残留原始标签", md);
}

console.log("turndown: <details>/<summary> 原生保留 + <mark> ==高亮==");
{
  const root = el("div", {},
    el("p", {}, "重点", el("mark", {}, "高亮词"), "结束"),
    el("details", {},
      el("summary", {}, "展开看代码"),
      el("pre", {}, el("code", { class: "language-js" }, "let a = 1;"))
    )
  );
  const md = ADL.turndown(root);
  ok(md.includes("==高亮词=="), "mark → ==…==", md);
  ok(md.includes("<details>") && md.includes("<summary>展开看代码</summary>"), "details/summary 原生保留", md);
  ok(md.includes("```js\nlet a = 1;") && md.includes("</details>"), "details 内正文仍走 Markdown", md);
}

console.log("articles: front matter 作者与发布时间（有值写入、无值省略）");
{
  const page = doc(el("div", { class: "blog-content-box" },
    el("h1", { class: "title-article" }, "元数据测试"),
    el("div", { id: "content_views" }, el("p", {}, "正文")),
    el("meta", { name: "author", content: "李四" }),
    el("meta", { property: "article:published_time", content: "2026-03-05 10:00" })
  ));
  const p = ADL.articlePrepare({ url: "https://blog.csdn.net/x/a/1", dom: () => page, captured: () => [], inlineJson: () => null });
  const out = ADL.adapters.articles.extract({ url: "https://blog.csdn.net/x/a/1", dom: () => page, captured: () => [], inlineJson: () => null });
  const md = out[0].meta.document.content;
  ok(p.meta.author === "李四", "meta author 提取", p.meta);
  ok(p.meta.published === "2026-03-05", "发布时间归一 YYYY-MM-DD", p.meta);
  ok(md.includes('author: "李四"') && md.includes('published: "2026-03-05"'), "front matter 写入 author/published", md);
  // 无元数据：字段整体省略
  const page2 = doc(el("div", { class: "blog-content-box" },
    el("h1", { class: "title-article" }, "无元数据文章"),
    el("div", { id: "content_views" }, el("p", {}, "正文"))
  ));
  const out2 = ADL.adapters.articles.extract({ url: "https://blog.csdn.net/x/b/2", dom: () => page2, captured: () => [], inlineJson: () => null });
  const md2 = out2[0].meta.document.content;
  ok(!md2.includes("author:") && !md2.includes("published:"), "缺省时省略 author/published 字段", md2.split("\n").slice(0, 8).join("\n"));
  ok(md2.includes('title: "无元数据文章"') && md2.includes("exported:"), "基础字段仍在", md2);
}

console.log("articles: normalizeDate 中式日期与非法值");
{
  const page = doc(el("div", { class: "blog-content-box" },
    el("h1", { class: "title-article" }, "日期格式"),
    el("div", { id: "content_views" }, el("p", {}, "正文"))
  ));
  // 直接走内部逻辑等价路径：不同的 meta 日期形态
  const mk = (content) => {
    const pg = doc(el("div", { class: "blog-content-box" },
      el("h1", { class: "title-article" }, "日期格式"),
      el("div", { id: "content_views" }, el("p", {}, "正文")),
      el("meta", { property: "article:published_time", content })
    ));
    return ADL.articlePrepare({ url: "https://blog.csdn.net/x/c/3", dom: () => pg, captured: () => [], inlineJson: () => null }).meta.published;
  };
  eq(mk("2026年3月5日"), "2026-03-05", "中式日期归一");
  eq(mk("2026/03/15"), "2026-03-15", "斜杠日期归一");
  eq(mk("not-a-date"), null, "非法日期 → null");
}

console.log("articles: 知乎回答页提取（新站点）");
{
  const page = doc(el("div", { class: "AnswerItem" },
    el("h1", { class: "QuestionHeader-title" }, "如何评价 Anything？"),
    el("div", { class: "AuthorInfo-name" }, el("a", {}, "张三")),
    el("meta", { itemprop: "datePublished", content: "2026-05-06" }),
    el("div", { class: "RichContent-inner" },
      el("p", {}, "回答正文 ", el("b", {}, "很有料")),
      el("img", { "data-actualsrc": "https://picx.zhimg.com/answer.png", src: "data:image/gif;base64,x" })
    )
  ));
  const url = "https://www.zhihu.com/question/123/answer/456";
  ok(ADL.articleSite(url) && ADL.articleSite(url).id === "zhihu-answer", "回答页命中 zhihu-answer 适配器");
  ok(ADL.ARTICLES_RE.test(url), "ARTICLES_RE 命中回答页");
  const out = ADL.adapters.articles.extract({ url, dom: () => page, captured: () => [], inlineJson: () => null });
  ok(out.length >= 2, "输出 MD + 图片", out.length);
  const md = out.find((r) => r.kind === "document").meta.document.content;
  ok(md.includes('title: "如何评价 Anything？"'), "回答页标题", md);
  ok(md.includes('author: "张三"'), "作者取自 AuthorInfo-name", md);
  ok(md.includes('site: "知乎回答"'), "站点标签", md);
  ok(md.includes("回答正文 **很有料**"), "正文转换", md);
  ok(out.some((r) => r.kind === "image" && r.url === "https://picx.zhimg.com/answer.png"), "data-actualsrc 懒加载原图入清单");
}

console.log("articles: B站专栏 / 少数派 URL 命中");
{
  ok(ADL.ARTICLES_RE.test("https://www.bilibili.com/read/cv25576166"), "B站专栏命中 ARTICLES_RE");
  ok(ADL.ARTICLES_RE.test("https://sspai.com/post/71500"), "少数派命中 ARTICLES_RE");
  ok(ADL.articleSite("https://www.bilibili.com/read/cv25576166").id === "bilibili-read", "B站专栏 site id");
  ok(ADL.articleSite("https://sspai.com/post/71500").id === "sspai", "少数派 site id");
  ok(!ADL.ARTICLES_RE.test("https://www.bilibili.com/video/BV1xx"), "视频页不误命中");
  ok(!ADL.ARTICLES_RE.test("https://www.zhihu.com/question/123"), "问题页（无回答 id）不命中");
}

// ---------------------------------------------------------------- 汇总
console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
