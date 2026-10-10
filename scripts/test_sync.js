// 文章同步子系统单元测试（node scripts/test_sync.js）
// 覆盖 8 个首批平台（知乎/简书/博客园/思否/开源中国/WordPress/Typecho/MetaWeblog）：
//   ① payload 构造器（fixture 快照断言请求形态）
//   ② 响应解析器（真实形态脱敏样本）
//   ③ HTML→Markdown/图片提取纯函数（含 code-box markdown-images 状态机移植）
//   ④ 内容变换纯函数（知乎 Draft.js 变换 / 简书清理）
//   ⑤ 页面解析（登录检测 HTML / 思否 token / XSRF cookie）
//   ⑥ 引擎纯逻辑（登录缓存 TTL / 历史上限）
// 约束：只 require extension/sync 纯函数模块，无 fetch / chrome.* / DOM。
"use strict";
const path = require("path");
const fs = require("fs");

globalThis.self = globalThis; // 同步模块挂在 self.ADL 命名空间

const S = "extension/sync";
require(path.join("..", S, "xmlrpc.js"));
require(path.join("..", S, "payloads.js"));
require(path.join("..", S, "markdown-images.js"));
require(path.join("..", S, "mdhtml.js"));

const F = (f) => fs.readFileSync(path.join(S, "fixtures", f), "utf-8");

let pass = 0, fail = 0;
function ok(cond, label, extra) {
  if (cond) { pass++; console.log("  ok  " + label); }
  else { fail++; console.log("  FAIL " + label + (extra !== undefined ? "  => " + JSON.stringify(extra) : "")); }
}
const B = () => ADL.syncBuild;
const X = () => ADL.syncXml;

// ---------------------------------------------------------------- XML-RPC 基础
console.log("xmlrpc 基础:");
{
  ok(X().escapeXml('a&b<c>d"e\'f') === "a&amp;b&lt;c&gt;d&quot;e&apos;f",
    "escapeXml 转义五个实体（& 优先，无二次转义）", X().escapeXml('a&b<c>d"e\'f'));
  ok(X().escapeXml("") === "" && X().escapeXml(null) === "", "escapeXml 空/null 安全返回空串");

  const body = X().build("wp.newPost", [0, "u", "p", { t: "x" }]);
  ok(!/\n/.test(body) && !/  /.test(body), "请求体为紧凑格式（无换行/无连续空白）", body);
  ok(body.startsWith('<?xml version="1.0"?><methodCall><methodName>wp.newPost</methodName><params>'),
    "methodCall 骨架紧凑无空白", body);
  ok(body.endsWith("</params></methodCall>"), "methodCall 收尾标签完整");
  ok(body.includes("<param><value><i4>0</i4></value></param>"), "数字编码为 <i4>（兼容性优于 <int>）");
  ok(body.includes("<param><value><string>u</string></value></param>"), "字符串编码为 <string>");
  ok(body.includes("<member><name>t</name><value><string>x</string></value></member>"),
    "struct 编码为 member/name/value");

  ok(X().build("m", [true, false]).includes("<boolean>1</boolean>")
    && X().build("m", [false]).includes("<boolean>0</boolean>"), "布尔编码 1/0");
  ok(X().build("m", [["a", "b"]]).includes("<array><data><value><string>a</string></value><value><string>b</string></value></data></array>"),
    "数组编码为 array/data/value");
  ok(X().build("m", [{ bits: { __base64: "QUJD" } }]).includes("<base64>QUJD</base64>"),
    "{__base64} 对象编码为 <base64>（图片 bits 通道）");
  ok(X().build("m", [{ bits: new Uint8Array([65, 66, 67]) }]).includes("<base64>QUJD</base64>"),
    "Uint8Array 自动 base64 编码");
  ok(X().build("m", ["<&>"]).includes("<string>&lt;&amp;&gt;</string>"),
    "参数值经 XML 转义");
  ok(X().build("m", []).endsWith("<params></params></methodCall>"), "空参数列表合法");
}

// ---------------------------------------------------------------- XML-RPC 响应解析
console.log("xmlrpc 响应解析:");
{
  const succ = X().parseResponse(F("wordpress_newpost_success.xml"));
  ok(succ.ok === true && succ.value === "128", "成功响应取 <string> 标量", succ);

  const fault = X().parseResponse(F("cms_fault.xml"));
  ok(fault.ok === false && fault.error === "用户名或密码不正确。",
    "fault 响应取 faultString", fault);

  ok(X().parseResponse("<methodResponse><params><param><value><int>77</int></value></param></params></methodResponse>").value === "77",
    "<int> 标量解析");
  ok(X().parseResponse("<methodResponse><params><param><value><i4>64</i4></value></param></params></methodResponse>").value === "64",
    "<i4> 标量解析");
  ok(X().parseResponse("<methodResponse><params><param><value><array><data/></array></value></param></params></methodResponse>").ok === true,
    "array 返回按成功处理（getUsersBlogs 场景）");
  ok(X().parseResponse("<methodResponse><params><param><value><struct><member><name>x</name><value><string>1</string></value></member></struct></value></param></params></methodResponse>").ok === true,
    "struct 返回按成功处理");

  ok(X().extractLatestPostId(F("typecho_recentposts.xml")) === "95",
    "getRecentPosts 响应提取 postid（Typecho postId=0 兜底）");
  ok(X().extractLatestPostId("<methodResponse><params></params></methodResponse>") === null,
    "无 postid 时安全返回 null");
  ok(X().extractMediaUrl("<methodResponse><params><param><value><struct><member><name>url</name></member></struct></value></param></params></methodResponse>") === null,
    "url member 缺 value 时安全返回 null");
  ok(X().extractMediaUrl(
    "<member><name>url</name><value><string>https://cdn.example/a.png</string></value></member>"
  ) === "https://cdn.example/a.png", "newMediaObject 响应提取图片 URL");
}

// ---------------------------------------------------------------- JSON payload 构造器
console.log("payload 构造器（社区五家）:");
{
  ok(B().zhihuCreateBody("测试标题") === '{"title":"测试标题","content":"","delta_time":0}',
    "知乎创建草稿体（title/content 空串/delta_time 0）", B().zhihuCreateBody("测试标题"));
  ok(B().zhihuPatchBody("标题", "<p>正文</p>") === '{"title":"标题","content":"<p>正文</p>"}',
    "知乎 PATCH 体（title + 处理后 HTML）");

  ok(B().jianshuCreateBody(42, "标题") === '{"at_bottom":false,"notebook_id":42,"title":"标题"}',
    "简书创建体（at_bottom false + 文集 id）", B().jianshuCreateBody(42, "标题"));
  ok(B().jianshuCreateBody("42", "标题").includes('"notebook_id":42'), "简书文集 id 容错字符串入参 → Number");
  ok(B().jianshuUpdateBody("标题", "<p>正文</p>") === '{"title":"标题","content":"<p>正文</p>"}',
    "简书更新体（HTML 正文）");

  ok(B().segmentfaultDraftBody("标题", "MD 正文") ===
    '{"title":"标题","tags":[],"text":"MD 正文","object_id":"","type":"article"}',
    "思否草稿体（tags 空 / type article）", B().segmentfaultDraftBody("标题", "MD 正文"));

  ok(B().oschinaDraftBody("标题", "123", "MD", 1) ===
    '{"title":"标题","user":123,"content":"MD","contentType":1,"catalog":0,"originUrl":"","privacy":true,"disableComment":false}',
    "开源中国草稿体（user 数字化 / privacy true）", B().oschinaDraftBody("标题", "123", "MD", 1));
  ok(B().oschinaDraftBody("标题", 5, "<p>x</p>", 2).includes('"contentType":2'),
    "开源中国 contentType 2=html 通道保留");

  // 博客园：42 键全量快照 + 关键语义断言
  const iso = "2026-10-10T00:00:00.000Z";
  const expectedObj = {
    id: null, postType: 2, accessPermission: 0, title: "T", url: null, postBody: "M",
    categoryIds: null, categories: null, collectionIds: [], inSiteCandidate: false,
    inSiteHome: false, siteCategoryId: null, blogTeamIds: null, isPublished: false,
    displayOnHomePage: false, isAllowComments: true, includeInMainSyndication: false,
    isPinned: false, showBodyWhenPinned: false, isOnlyForRegisterUser: false,
    isUpdateDateAdded: false, entryName: null, description: null, featuredImage: null,
    tags: null, password: null, publishAt: null, datePublished: iso, dateUpdated: null,
    isMarkdown: true, isDraft: true, autoDesc: null, changePostType: false, blogId: 0,
    author: null, removeScript: false, clientInfo: null, changeCreatedTime: false,
    canChangeCreatedTime: false, isContributeToImpressiveBugActivity: false,
    usingEditorId: 5, sourceUrl: null,
  };
  const cnb = B().cnblogsPostBody("T", "M", iso);
  ok(cnb === JSON.stringify(expectedObj), "博客园请求体 42 键全量快照（键序一致）");
  const cnbParsed = JSON.parse(cnb);
  ok(Object.keys(cnbParsed).length === 42, "博客园请求体键数 = 42", Object.keys(cnbParsed).length);
  ok(cnbParsed.postType === 2 && cnbParsed.isMarkdown === true && cnbParsed.isDraft === true
    && cnbParsed.usingEditorId === 5 && cnbParsed.isPublished === false && cnbParsed.datePublished === iso,
    "博客园关键键：postType=2 / isMarkdown / isDraft / usingEditorId=5 / isPublished=false");
  ok(cnbParsed.postBody === "M" && cnbParsed.title === "T", "博客园正文与标题落位");
}

// ---------------------------------------------------------------- CMS XML 端到端
console.log("payload 构造器（CMS 三件套）:");
{
  const wpDraft = B().wordpressNewPostBody("user1", "pw1", "标题", "<p>正文</p>", true);
  ok(wpDraft.includes("<methodName>wp.newPost</methodName>"), "WordPress 发布方法名 wp.newPost");
  ok(wpDraft.includes("<name>post_status</name><value><string>draft</string></value>"),
    "WordPress 草稿模式 post_status=draft");
  ok(wpDraft.includes("<name>post_title</name><value><string>标题</string></value></member><member><name>post_content</name><value><string>&lt;p&gt;正文&lt;/p&gt;</string></value>"),
    "WordPress 正文 HTML 经 XML 转义");
  ok(B().wordpressNewPostBody("u", "p", "T", "C", false).includes("<name>post_status</name><value><string>publish</string></value>"),
    "WordPress 非草稿 post_status=publish");
  ok(B().wordpressNewPostBody("u", "p", "T", "C", true).indexOf("<param><value><i4>0</i4></value></param><param><value><string>u</string></value></param>") > 0,
    "WordPress 参数序 = [blogId 0, user, pass, struct]");
  ok(!/\n/.test(wpDraft), "WordPress 请求体紧凑无换行");

  ok(B().wordpressTestBody("user1", "pw1").includes("<methodName>wp.getUsersBlogs</methodName>"),
    "WordPress 测试连接 wp.getUsersBlogs");

  const wpUpload = B().wordpressUploadFileBody("u", "p", "a.png", "image/png", "QUJD");
  ok(wpUpload.includes("<methodName>wp.uploadFile</methodName>")
    && wpUpload.includes("<name>bits</name><value><base64>QUJD</base64></value>")
    && wpUpload.includes("<name>overwrite</name><value><boolean>1</boolean></value>"),
    "WordPress uploadFile：bits base64 + overwrite true");

  const tc = B().typechoNewPostBody("u", "p", "标题", "  <p>正文</p>  ");
  ok(tc.includes("<methodName>metaWeblog.newPost</methodName>")
    && tc.includes("<name>description</name><value><string>&lt;p&gt;正文&lt;/p&gt;</string></value>"),
    "Typecho 发布 metaWeblog.newPost 且 description 去首尾空白（metaweblog.ts:676 trim）");
  ok(tc.includes("<param><value><i4>0</i4></value></param><param><value><string>u</string></value></param><param><value><string>p</string></value></param>")
    && tc.endsWith("<value><boolean>0</boolean></value></param></params></methodCall>"),
    "Typecho 参数序 = [0(数字), user, pass, struct, false 固定草稿]", tc.slice(-80));

  ok(B().typechoTestBody("u", "p").includes("<methodName>metaWeblog.getUsersBlogs</methodName>")
    && B().typechoTestBody("u", "p").includes('<param><value><string></string></value></param>'),
    "Typecho 测试连接 getUsersBlogs（appKey 空串）");
  ok(B().typechoRecentPostsBody("u", "p").includes("<methodName>metaWeblog.getRecentPosts</methodName>")
    && B().typechoRecentPostsBody("u", "p").endsWith("<param><value><i4>1</i4></value></param></params></methodCall>"),
    "Typecho 兜底 getRecentPosts（只取最新 1 篇）");

  const mwDraft = B().metaweblogNewPostBody("u", "p", "标题", "内容", true);
  ok(mwDraft.indexOf("<param><value><string>0</string></value></param>") > 0
    && mwDraft.indexOf('<param><value><string>0</string></value></param>')
      < mwDraft.indexOf("<param><value><string>u</string></value></param>"),
    "MetaWeblog blogId 为字符串 '0'（与 Typecho 数字 0 有意区分，且位于 user 之前）");
  ok(mwDraft.includes("<name>categories</name><value><array><data></data></array></value>"),
    "MetaWeblog struct 携带空 categories 数组");
  ok(mwDraft.endsWith("<value><boolean>0</boolean></value></param></params></methodCall>"),
    "MetaWeblog draftOnly=true → publish 标志 false");
  ok(B().metaweblogNewPostBody("u", "p", "T", "C", false).endsWith("<value><boolean>1</boolean></value></param></params></methodCall>"),
    "MetaWeblog draftOnly=false → publish 标志 true（!draftOnly 翻转）");

  const media = B().metaweblogNewMediaObjectBody("u", "p", "a.png", "image/png", "QUJD");
  ok(media.includes("<methodName>metaWeblog.newMediaObject</methodName>")
    && media.indexOf("<name>bits</name>") < media.indexOf("<name>bytes</name>")
    && (media.match(/<base64>QUJD<\/base64>/g) || []).length === 2,
    "MetaWeblog newMediaObject：bits/bytes 双发 base64（兼容 Typecho）");
}

// ---------------------------------------------------------------- 响应解析器（社区）
console.log("响应解析器:");
{
  ok(B().parseZhihuMe({ id: "1", name: "张三", avatar_url: "https://pic.zhimg.com/a.png" }).ok === true,
    "知乎登录检测解析 {id,name,avatar_url} 命中");
  const zme = B().parseZhihuMe({ id: "1", name: "张三", avatar_url: "https://pic.zhimg.com/a.png" });
  ok(zme.username === "张三" && zme.avatar === "https://pic.zhimg.com/a.png",
    "知乎登录检测字段透传 username/avatar");
  ok(B().parseZhihuMe({ error: "401" }).ok === false, "知乎未登录（无 id）→ 未登录");
  ok(B().parseZhihuDraft({ id: 610000 }).ok === true
    && B().parseZhihuDraft({}).ok === false, "知乎草稿响应必须含 id");

  ok(B().parseJianshuMe({ data: { nickname: "李四", avatar: "https://a" } }).ok === true
    && B().parseJianshuMe({ data: {} }).ok === false, "简书登录检测解析 data.nickname");
  ok(B().parseJianshuNotebooks([{ id: 7, name: "默认文集" }, { id: 8, name: "第二" }]).notebookId === 7,
    "简书文集取第一个");
  ok(B().parseJianshuNotebooks([]).ok === false, "简书空文集列表报错");
  ok(B().parseJianshuDraft({ id: 9 }).ok === true && B().parseJianshuDraft({}).error.length > 0,
    "简书草稿响应校验 id");
  ok(B().parseJianshuImageUpload({ url: "https://u/x.png" }).url === "https://u/x.png",
    "简书七牛上传响应 url 形态");
  ok(B().parseJianshuImageUpload({ key: "abc/def" }).url === "https://upload-images.jianshu.io/upload_images/abc/def",
    "简书七牛 key 形态拼完整 URL");
  ok(B().parseJianshuImageUpload({}).ok === false, "简书上传响应无 url/key 报错");

  const cnbPost = B().parseCnblogsPost({ id: 128 });
  ok(cnbPost.ok === true && cnbPost.id === "128", "博客园草稿响应取 id");
  ok(B().parseCnblogsPost({ error: "无权限" }).error === "无权限", "博客园错误字段透传");
  for (const [k, v] of [["data", "u1"], ["url", "u2"], ["imageUrl", "u3"], ["src", "u4"]]) {
    ok(B().parseCnblogsImageUpload({ [k]: v }).url === v, `博客园图片响应 ${k} 键兼容`);
  }
  ok(B().parseCnblogsImageUpload({ data: 123 }).ok === false, "博客园图片响应非字符串 URL 报错");

  ok(B().parseOschinaMe({ success: true, result: { userId: 9, userVo: { name: "王五", portraitUrl: "https://p" } } }).username === "王五"
    && B().parseOschinaMe({ success: true }).ok === false
    && B().parseOschinaMe({ success: false, message: "请登录" }).ok === false, "开源中国登录检测解析");
  ok(B().parseOschinaDraft({ success: true, result: { id: 55 } }).id === "55", "开源中国草稿响应取 result.id");
  ok(B().parseOschinaDraft({ success: false, message: "失败原因" }).error === "失败原因", "开源中国失败 message 透传");
  ok(B().parseOschinaImageUpload({ success: true, result: "https://cdn/x.png" }).url === "https://cdn/x.png",
    "开源中国图片上传 result 即 URL");

  const sfOk = B().parseSegmentfaultDraft('[0,{"id":"draft-9"}]');
  ok(sfOk.ok === true && sfOk.id === "draft-9", "思否数组成功形态 [0,data]");
  ok(B().parseSegmentfaultDraft('[1,"发布失败：无权限"]').error === "发布失败：无权限",
    "思否数组失败形态 [1,msg]");
  ok(B().parseSegmentfaultDraft('{"id":77}').id === "77", "思否对象成功形态含 id");
  ok(B().parseSegmentfaultDraft('{"message":"请先登录"}').error === "请先登录", "思否对象错误字段 message");
  ok(B().parseSegmentfaultDraft("Unauthorized").error === "未授权", "思否纯文本 Unauthorized 特判");
  ok(B().parseSegmentfaultDraft("你已被禁言").error === "你已被禁言", "思否「禁言」特判");
  ok(B().parseSegmentfaultDraft("账号被锁定，请联系管理员").error.includes("锁定"), "思否「锁定」特判");
  ok(B().parseSegmentfaultDraft("<html>not json</html>").error.includes("发布失败"), "思否非 JSON 响应明确报错");
  ok(B().parseSegmentfaultImageUpload('{"result":"https://cdn.sf/a.png"}').url === "https://cdn.sf/a.png",
    "思否图片上传 result 形态");
  ok(B().parseSegmentfaultImageUpload('{"url":"/img/x"}').url === "/img/x", "思否图片上传 url 形态");
  ok(B().parseSegmentfaultImageUpload('[0,"https://cdn.sf/b.png","aid"]').url === "https://cdn.sf/b.png",
    "思否图片上传旧版数组 [0,url,id]");
  ok(B().parseSegmentfaultImageUpload('[1,"大小超限"]').error === "大小超限", "思否图片上传数组失败形态");
}

// ---------------------------------------------------------------- 页面解析
console.log("页面解析（登录态/token/cookie）:");
{
  const user = B().parseCnblogsUser(F("cnblogs_user.html"));
  ok(user.ok === true && user.userId === "kzzz" && user.avatar === "https://pic.cnblogs.com/avatar/simple_user.png",
    "博客园 CurrentUserInfo 提取 uid + 头像", user);
  ok(B().parseCnblogsUser(F("cnblogs_user_anon.html")).ok === false, "博客园未登录页 → 未登录");

  const sfUser = B().parseSegmentfaultUser(F("segmentfault_settings.html"));
  ok(sfUser.ok === true && sfUser.userId === "testuser-1"
    && sfUser.avatar === "https://avatar-static.segmentfault.com/200/344/200344000_1699000000_avatar",
    "思否设置页提取 uid + avatar-static 头像", sfUser);

  const tok = B().parseSegmentfaultToken(F("segmentfault_write.html"));
  ok(tok.ok === true && tok.token === "tok_new_format_9f8e7d6c", "思否 /write 新版 serverData.Token 提取", tok);

  const legacy = 'window.g_initialProps = {"global":{"sessionInfo":{"key":"tok_legacy"}}};\n\t</script>';
  const tok2 = B().parseSegmentfaultToken("<html><script>" + legacy + "</script></html>");
  ok(tok2.ok === true && tok2.token === "tok_legacy", "思否旧版 g_initialProps 分支提取", tok2);
  ok(B().parseSegmentfaultToken("<html>登录后才能写文章</html>").error.includes("获取 session token 失败"),
    "思否 token 缺失 → 明确报错（未登录或改版）");
  const badLegacy = 'window.g_initialProps = {broken json};\n\t</script>';
  ok(B().parseSegmentfaultToken("<script>" + badLegacy + "</script>").error.includes("解析 session token 失败"),
    "思否旧版分支 JSON 损坏 → 明确报错");

  ok(B().parseXsrfFromCookie("other=1; XSRF-TOKEN=abc%2Fdef.123|t; more=2") === "abc%2Fdef.123|t",
    "document.cookie 提取 XSRF-TOKEN 原始值（不做 URL 解码，与 Cookie 头一致）");
  ok(B().parseXsrfFromCookie("XSRF-TOKEN=first") === "first", "XSRF-TOKEN 位于串首同样命中");
  ok(B().parseXsrfFromCookie("foo=bar") === null, "无 XSRF-TOKEN → null");
}

// ---------------------------------------------------------------- 内容变换
console.log("内容变换:");
{
  const table = '<table><thead><tr><td>表头1</td><td>表头2</td></tr></thead>'
    + '<tbody><tr><td>a</td><td>b</td></tr></tbody></table>';
  ok(B().zhihuTransformTables(table) ===
    '<table data-draft-node="block" data-draft-type="table" data-size="normal" data-row-style="normal">'
    + '<tbody><tr><th>表头1</th><th>表头2</th></tr><tr><td>a</td><td>b</td></tr></tbody></table>',
    "知乎表格：thead td→th + data-draft-* 四属性包裹", B().zhihuTransformTables(table));
  ok(B().zhihuTransformTables('<figure><table><tbody><tr><td>x</td></tr></tbody></table></figure>')
    .startsWith('<table data-draft-node="block"'),
    "知乎表格：figure 包裹先解包再重写");
  const noThead = B().zhihuTransformTables('<table><tr><th>h</th></tr><tr><td>v</td></tr></table>');
  ok(noThead.includes("<th>h</th>") && noThead.indexOf("<th>h</th>") < noThead.indexOf("<td>v</td>"),
    "知乎表格：无 thead 时全 th 首行提升为表头");

  const zc = B().zhihuTransformContent('<img src="https://pic.example/a.png" data-src="s" style="width:1px">'
    + '<pre><code class="language-js">let x = 1;</code></pre>');
  ok(zc.includes("<figure><img src=") && zc.includes("</figure>"), "知乎正文：img 包裹 figure");
  ok(!zc.includes("data-src=") && !zc.includes("style="), "知乎正文：data-*/style 属性清除");
  ok(zc.includes('<pre lang="js"><code>'), "知乎正文：代码块 language-x → pre lang");

  const zKeep = B().zhihuTransformContent('<div data-draft-type="table" data-track="1">x</div>');
  ok(zKeep.includes("data-draft-type=") && !zKeep.includes("data-track="),
    "知乎正文：data-draft-* 保留、其余 data-* 清除");

  const jc = B().jianshuCleanHtml(
    '<div><iframe src="//e.com/x"></iframe><p> </p><p>段落</p><br><img src="a.svg">'
    + '<mpprofile>卡片</mpprofile><span data-reader-unique-id="9">t</span></div><br>'
  );
  ok(jc === "<div><p>段落</p><br><span>t</span></div>", "简书清理：iframe/svg图/空p/mpprofile/data-reader-unique-id/尾部br", jc);
  ok(B().jianshuCleanHtml('<iframe class="x" src="y" />尾巴') === "尾巴", "简书清理：自闭合 iframe 移除");
}

// ---------------------------------------------------------------- 图片链路
console.log("图片链路（markdown-images 移植）:");
{
  const P = (md) => ADL.syncImages.parseMarkdownImages(md);
  ok(JSON.stringify(P("前 ![alt 文本](https://a.example/1.png) 后")) ===
    '[{"full":"![alt 文本](https://a.example/1.png)","alt":"alt 文本","src":"https://a.example/1.png"}]',
    "基础形态：![alt](src) full/alt/src 三元组");
  ok(P('![t](https://a.example/2.png "标题")')[0].src === "https://a.example/2.png",
    "可选双引号标题形态不破坏 src");
  ok(P('![t](https://a.example/3.png \'单引号\')')[0].src === "https://a.example/3.png",
    "可选单引号标题形态");
  ok(P("![t](<https://a.example/4(b).png>)")[0].src === "https://a.example/4(b).png",
    "<角括号 URL> 形态支持含括号地址");
  ok(P("![t](https://a.example/5(b).png)")[0].src === "https://a.example/5(b).png",
    "普通形态 URL 内成对括号（深度感知）");
  ok(P("![a](https://x.example/p\\(q\\).png)")[0].src === "https://x.example/p\\(q\\).png",
    "反斜杠转义括号不提前截断");
  ok(P("这不是图片 [文字](https://a.example/x.png)").length === 0, "非图片链接（无 !）不匹配");
  ok(P("![未闭合](https://a.example").length === 0, "未闭合括号安全跳过");
  ok(P("a ![1](u1) b ![2](<u2(x)>) c").length === 2, "一段多图全部命中");

  const H = ADL.syncImages.extractHtmlImages;
  ok(H('<div><img src="https://i.example/1.png" alt="a"></div>').length === 1
    && H('<img src="https://i.example/2.png">')[0].full === '<img src="https://i.example/2.png">',
    "HTML <img src> 提取（full = 整标签）");
  ok(H("<img class=x>").length === 0, "无 src 的 img 不误收");

  const S_ = ADL.syncImages.shouldSkip;
  ok(S_("https://pic.zhimg.com/a.png", ["zhimg.com"]) === true, "skipPatterns 子串命中跳过");
  ok(S_("data:image/png;base64,xxx", ["zhimg.com"]) === false, "data URI 永不跳过");
  ok(S_("https://a.example/x.png", []) === false, "无 skipPatterns 不跳过");
  ok(S_("", []) === true, "空 src 跳过");

  // processImages：HTML+MD 双格式 / 同 URL 去重 / skipPatterns / 失败保留原 URL / 注入 delay
  (async () => {
    const content = '<p><img src="https://a.example/1.png" alt=""></p>\n\n'
      + "![图](https://a.example/1.png)\n\n"
      + "![跳过](https://pic.zhimg.com/x.png)\n\n"
      + "![失败](https://a.example/fail.png)\n";
    let calls = 0, delays = 0;
    const out = await ADL.syncImages.processImages(
      content,
      async (src) => {
        calls++;
        if (src.includes("fail")) throw new Error("上传被拒");
        return { url: "https://cdn.example/new.png" };
      },
      {
        skipPatterns: ["zhimg.com"],
        delayFn: async () => { delays++; },
      }
    );
    ok(calls === 2, "同 URL 去重（1.png 一次）+ 失败图独立尝试，共 2 次上传", calls);
    ok(delays === 3, "每张处理过的图注入一次 delayFn（3 处：html 1.png + md 1.png + fail）", delays);
    ok(out.includes('<img src="https://cdn.example/new.png"'), "HTML img 替换为上传后 URL", out);
    ok(out.includes("![图](https://cdn.example/new.png)"), "Markdown 图替换为上传后 URL（保留 alt 文本）");
    ok(out.includes("https://pic.zhimg.com/x.png"), "skipPatterns 命中的图保留原 URL");
    ok(out.includes("https://a.example/fail.png"), "上传失败的图保留原 URL 不中断");
    const outNone = await ADL.syncImages.processImages("无图片文本", async () => ({ url: "x" }), { delayFn: async () => {} });
    ok(outNone === "无图片文本", "无图片内容原样返回");
  })().then(runEngineSection).catch((e) => { console.log("  FAIL 图片链路异步段 => " + e.message); fail++; runEngineSection(); });
}

// ---------------------------------------------------------------- mdhtml
console.log("基础 Markdown→HTML 渲染:");
{
  const M = ADL.syncMdHtml.mdToHtml;
  const html = M(
    "# 标题\n\n段落 **加粗** 与 [链接](https://a.b/c) 与 `code` 与 <script>\n\n"
    + "- 甲\n- 乙\n\n1. 一\n2. 二\n\n> 引用内容\n\n---\n\n"
    + "```js\nlet a = \"<b>\";\n```\n\n"
    + "| 列A | 列B |\n| --- | :--: |\n| 1 | 2 |\n\n"
    + "![图](https://x.example/y.png)\n"
  );
  ok(html.includes("<h1>标题</h1>"), "ATX 标题渲染");
  ok(html.includes("<strong>加粗</strong>"), "加粗渲染");
  ok(html.includes('<a href="https://a.b/c">链接</a>'), "链接渲染");
  ok(html.includes("<code>code</code>"), "行内代码渲染");
  ok(html.includes("&lt;script&gt;"), "HTML 文本转义防注入");
  ok(html.includes("<ul><li>甲</li><li>乙</li></ul>"), "无序列表渲染");
  ok(html.includes("<ol><li>一</li><li>二</li></ol>"), "有序列表渲染");
  ok(html.includes("<blockquote><p>引用内容</p></blockquote>"), "引用渲染");
  ok(html.includes("<hr />"), "分割线渲染");
  ok(html.includes('<pre><code class="language-js">let a = &quot;&lt;b&gt;&quot;;</code></pre>'),
    "围栏代码块：语言标注 + 内容转义", html);
  ok(html.includes("<table>") && html.includes("<th>列A</th>")
    && html.includes('<th style="text-align:center">列B</th>')
    && html.includes("<td>1</td>") && html.includes('<td style="text-align:center">2</td>'),
    "GFM 表格：表头/居中对齐（th 与 td 同列）/表体", html);
  ok(html.includes('<img src="https://x.example/y.png" alt="图" />'), "图片渲染");
  ok(M("普通段落") === "<p>普通段落</p>", "纯段落渲染");
  ok(M('带 **星号** 与 __下划线加粗__ 与 ~~删除~~ 与 *斜体*').includes("<strong>下划线加粗</strong>")
    && M("~~删除~~").includes("<del>删除</del>")
    && M("*斜体*").includes("<em>斜体</em>"), "行内强调族渲染");
  ok(ADL.syncMdHtml.esc('a<b>&"') === "a&lt;b&gt;&amp;&quot;", "esc 基础转义");
}

// ---------------------------------------------------------------- 引擎纯逻辑
function runEngineSection() {
  console.log("引擎纯逻辑:");
  {
    const now = 1_800_000_000_000;
    const MIN = 60 * 1000;
    ok(B().authCacheDecide({ a: { ok: true, ts: now - 4 * MIN } }, "a", now).fresh === true,
      "已登录缓存 4 分钟 < TTL 5 分钟 → 命中");
    ok(B().authCacheDecide({ a: { ok: true, ts: now - 6 * MIN } }, "a", now).fresh === false,
      "已登录缓存超 5 分钟 → 过期重检");
    ok(B().authCacheDecide({ a: { ok: false, ts: now - 20 * 1000 } }, "a", now).fresh === true,
      "未登录缓存 20 秒 < TTL 30 秒 → 命中");
    ok(B().authCacheDecide({ a: { ok: false, ts: now - 40 * 1000 } }, "a", now).fresh === false,
      "未登录缓存超 30 秒 → 过期重检");
    ok(B().authCacheDecide({}, "a", now).fresh === false, "无缓存条目 → 需检测");
    ok(B().authCacheDecide({ a: { ok: true, ts: now } }, "a", now, true).fresh === false,
      "force 刷新时缓存一律失效");

    const long = Array.from({ length: 35 }, (_, i) => ({ i }));
    ok(B().capHistory(long, 30).length === 30 && B().capHistory(long, 30)[0].i === 0,
      "历史上限裁剪到 30 条且保留最新在前");
    ok(B().capHistory(null, 30).length === 0, "历史空值安全返回空数组");
  }

  // ---------------------------------------------------------------- 汇总
  console.log(`\n${pass} passed, ${fail} failed`);
  process.exit(fail ? 1 : 0);
}
