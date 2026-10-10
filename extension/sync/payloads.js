// 文章同步 payload 构造器与响应解析器 —— 零依赖纯函数集（ADL.syncBuild）。
// 逐平台移植自 code-box（GPL-3.0）适配器源码，函数内注释标注证据行号：
//   zhihu.ts / jianshu.ts / cnblogs.ts / segmentfault.ts / oschina.ts /
//   cms/wordpress.ts / cms/metaweblog.ts / extension/engine.ts（登录缓存 TTL）。
// 约束：无 fetch、无 chrome.*、无 DOM —— scripts/test_sync.js 在 node 下
// globalThis.self = globalThis 后 require 即测；JSON 请求体统一返回
// JSON 字符串（与最终 fetch body 完全一致，测试做快照断言）。
self.ADL = self.ADL || {};
(() => {
  // =============================================================== 知乎
  // 创建草稿体（zhihu.ts:108-112）
  const zhihuCreateBody = (title) =>
    JSON.stringify({ title: String(title || ""), content: "", delta_time: 0 });

  // 写正文体（zhihu.ts:165-168；PATCH 可能 204/空体，调用方按 ok 判定）
  const zhihuPatchBody = (title, html) =>
    JSON.stringify({ title: String(title || ""), content: String(html || "") });

  // 知乎 Draft.js 内容变换（zhihu.ts:196-219 逐条照抄）：
  //   表格重写 → img 包 figure → pre lang → 删 data-*（保留 data-draft-*）与 style
  function zhihuTransformContent(content) {
    let result = zhihuTransformTables(String(content || ""));
    // img 包 figure（知乎编辑器要求）
    result = result.replace(
      /<img([^>]+)src="([^"]+)"([^>]*)>/gi,
      '<figure><img$1src="$2"$3></figure>'
    );
    // 代码块语言标注：<pre><code class="language-x"> → <pre lang="x"><code>
    result = result.replace(/<pre><code class="language-(\w+)">/gi, '<pre lang="$1"><code>');
    // 移除 data-* 属性（保留知乎自己的 data-draft-*），移除内联 style
    result = result.replace(/\s*data-(?!draft)[a-z-]+="[^"]*"/gi, "");
    result = result.replace(/\s*style="[^"]*"/gi, "");
    return result;
  }

  // 表格 → 知乎 Draft.js 专用表格（zhihu.ts:224-277 照抄）：
  //   figure 解包 → thead td→th → 统一包 data-draft-* <table><tbody>
  function zhihuTransformTables(html) {
    // 1) 解包 figure 中的 table
    let result = String(html || "").replace(
      /<figure[^>]*>\s*(<table[\s\S]*?<\/table>)\s*<\/figure>/gi,
      "$1"
    );
    // 2) 重写 table 结构
    result = result.replace(
      /<table[^>]*>([\s\S]*?)<\/table>/gi,
      (_match, tableContent) => {
        const theadMatch = tableContent.match(/<thead[^>]*>([\s\S]*?)<\/thead>/i);
        const tbodyMatch = tableContent.match(/<tbody[^>]*>([\s\S]*?)<\/tbody>/i);

        let headerRows = "";
        let bodyRows = "";

        if (theadMatch) {
          // 表头行统一用 <th>
          headerRows = theadMatch[1]
            .replace(/<td([^>]*)>/gi, "<th$1>")
            .replace(/<\/td>/gi, "</th>");
        }

        if (tbodyMatch) {
          bodyRows = tbodyMatch[1];
        } else {
          // 无 tbody：整表内容作为 body（剔除 thead）
          bodyRows = tableContent
            .replace(/<thead[^>]*>[\s\S]*?<\/thead>/gi, "")
            .replace(/<\/?tbody[^>]*>/gi, "");
        }

        // 无 thead 时：首行若全为 th 则视为表头
        if (!theadMatch) {
          const firstRowMatch = bodyRows.match(/<tr[^>]*>([\s\S]*?)<\/tr>/i);
          if (firstRowMatch) {
            const firstRowContent = firstRowMatch[1];
            if (/<th[^>]*>/i.test(firstRowContent) && !/<td[^>]*>/i.test(firstRowContent)) {
              headerRows = firstRowMatch[0];
              bodyRows = bodyRows.replace(firstRowMatch[0], "");
            }
          }
        }

        return (
          '<table data-draft-node="block" data-draft-type="table" data-size="normal" ' +
          'data-row-style="normal"><tbody>' +
          headerRows +
          bodyRows +
          "</tbody></table>"
        );
      }
    );
    return result;
  }

  // 登录检测响应（zhihu.ts:74-88）：{id,name,avatar_url} → 登录态
  const parseZhihuMe = (data) =>
    data && data.id
      ? { ok: true, userId: String(data.id), username: data.name, avatar: data.avatar_url }
      : { ok: false, error: "未登录" };

  // 创建/更新草稿响应（zhihu.ts:124-133）：{id} 必须存在
  const parseZhihuDraft = (data) =>
    data && data.id ? { ok: true, id: String(data.id) } : { ok: false, error: "创建草稿失败: 无效响应" };

  // =============================================================== 简书
  // 创建草稿体（jianshu.ts:106-110）
  const jianshuCreateBody = (notebookId, title) =>
    JSON.stringify({ at_bottom: false, notebook_id: Number(notebookId) || 0, title: String(title || "") });

  // 更新草稿体（jianshu.ts:156-159，content 为 HTML）
  const jianshuUpdateBody = (title, html) =>
    JSON.stringify({ title: String(title || ""), content: String(html || "") });

  // 简书正文清理（jianshu.ts:126-135 + code-adapter.cleanHtml 语义）：
  //   去 iframe/svg 图/mpprofile/qqmusic、去 data-reader-unique-id、
  //   去空 <p> 与尾部 <br>
  function jianshuCleanHtml(content) {
    let result = String(content || "");
    // iframe（成对 + 自闭合）
    result = result.replace(/<iframe[^>]*>[\s\S]*?<\/iframe>/gi, "");
    result = result.replace(/<iframe[^>]*\/>/gi, "");
    // svg 图片
    result = result.replace(/<img[^>]+src="[^"]*\.svg"[^>]*>/gi, "");
    // 指定标签（成对 + 自闭合）
    for (const tag of ["mpprofile", "qqmusic"]) {
      result = result.replace(new RegExp("<" + tag + "[^>]*>[\\s\\S]*?</" + tag + ">", "gi"), "");
      result = result.replace(new RegExp("<" + tag + "[^>]*\\/?>", "gi"), "");
    }
    // 指定属性
    result = result.replace(/\s*data-reader-unique-id="[^"]*"/gi, "");
    // 简书特有：空段落与尾部 br
    result = result.replace(/<p>\s*<\/p>/gi, "");
    result = result.replace(/<br\s*\/?>\s*$/gi, "");
    return result;
  }

  // 登录检测响应（jianshu.ts:36-49）：{data:{nickname,avatar}}
  const parseJianshuMe = (data) =>
    data && data.data && data.data.nickname
      ? { ok: true, username: data.data.nickname, avatar: data.data.avatar }
      : { ok: false, error: "未登录" };

  // 文集列表（jianshu.ts:61-89）：取第一个文集 id
  const parseJianshuNotebooks = (arr) =>
    Array.isArray(arr) && arr.length && arr[0] && arr[0].id != null
      ? { ok: true, notebookId: arr[0].id, name: arr[0].name }
      : { ok: false, error: "没有可用的文集" };

  // 创建/更新响应（jianshu.ts:113-117,163-167）：{id}
  const parseJianshuDraft = (data) =>
    data && data.id ? { ok: true, id: String(data.id) } : { ok: false, error: "草稿响应无效（未返回 id）" };

  // 七牛上传响应（jianshu.ts:249-260）：{url} 或 {key} → 拼完整 URL
  const parseJianshuImageUpload = (data) => {
    if (data && data.url) return { ok: true, url: data.url };
    if (data && data.key) {
      return { ok: true, url: "https://upload-images.jianshu.io/upload_images/" + data.key };
    }
    return { ok: false, error: "图片上传失败" };
  };

  // =============================================================== 博客园
  // 创建草稿请求体（cnblogs.ts:167-210 全字段照抄，42 个键；顺序 = 快照顺序）
  const cnblogsPostBody = (title, markdown, isoDate) =>
    JSON.stringify({
      id: null,
      postType: 2, // 2 = 文章, 1 = 随笔
      accessPermission: 0,
      title: String(title || ""),
      url: null,
      postBody: String(markdown || ""),
      categoryIds: null,
      categories: null,
      collectionIds: [],
      inSiteCandidate: false,
      inSiteHome: false,
      siteCategoryId: null,
      blogTeamIds: null,
      isPublished: false,
      displayOnHomePage: false,
      isAllowComments: true,
      includeInMainSyndication: false,
      isPinned: false,
      showBodyWhenPinned: false,
      isOnlyForRegisterUser: false,
      isUpdateDateAdded: false,
      entryName: null,
      description: null,
      featuredImage: null,
      tags: null,
      password: null,
      publishAt: null,
      datePublished: isoDate,
      dateUpdated: null,
      isMarkdown: true,
      isDraft: true,
      autoDesc: null,
      changePostType: false,
      blogId: 0,
      author: null,
      removeScript: false,
      clientInfo: null,
      changeCreatedTime: false,
      canChangeCreatedTime: false,
      isContributeToImpressiveBugActivity: false,
      usingEditorId: 5,
      sourceUrl: null,
    });

  // 登录检测页解析（cnblogs.ts:100-110）：
  //   <a href="/u/xxx/"><img class="pfs" src="..."></a>
  const parseCnblogsUser = (html) => {
    const s = String(html || "");
    const avatarMatch = s.match(/<img[^>]+class="pfs"[^>]+src="([^"]+)"/);
    const linkMatch = s.match(/href="\/u\/([^/]+)\/"/);
    if (!linkMatch) return { ok: false, error: "未登录" };
    return {
      ok: true,
      userId: linkMatch[1],
      username: linkMatch[1],
      avatar: avatarMatch ? avatarMatch[1] : undefined,
    };
  };

  // 创建草稿响应（cnblogs.ts:225-234）：{id} 或 {error}
  const parseCnblogsPost = (data) => {
    if (data && data.id) return { ok: true, id: String(data.id) };
    return { ok: false, error: (data && data.error) || "创建草稿失败: 无效响应" };
  };

  // 图片上传响应四键兼容（cnblogs.ts:303）：data|url|imageUrl|src
  const parseCnblogsImageUpload = (data) => {
    const url = data && (data.data || data.url || data.imageUrl || data.src);
    return typeof url === "string" && url ? { ok: true, url } : { ok: false, error: "图片上传失败: 无法获取图片 URL" };
  };

  // document.cookie → XSRF-TOKEN 原始值（chrome.cookies 不可用时的内容脚本降级：
  // 规格手测清单第 3 项；httpOnly 时读不到 → 上层明确报错）
  const parseXsrfFromCookie = (cookieStr) => {
    const m = /(?:^|;\s*)XSRF-TOKEN=([^;]+)/.exec(String(cookieStr || ""));
    return m ? m[1] : null;
  };

  // =============================================================== 思否
  // 创建草稿体（segmentfault.ts:175-181）
  const segmentfaultDraftBody = (title, text) =>
    JSON.stringify({
      title: String(title || ""),
      tags: [],
      text: String(text || ""),
      object_id: "",
      type: "article",
    });

  // 异常文本特判（segmentfault.ts:141-144,195-198）：
  //   'Unauthorized' / 含「禁言」「锁定」
  const segmentfaultBlocked = (text) => {
    const s = String(text || "").trim();
    if (s === "Unauthorized") return "未授权";
    if (s.includes("禁言") || s.includes("锁定")) return s;
    return null;
  };

  // 草稿响应双形态（segmentfault.ts:200-239）：
  //   数组 [0,data]=成功取 data.id / [1,msg]=失败；对象含 id 亦成功
  function parseSegmentfaultDraft(text) {
    const blocked = segmentfaultBlocked(text);
    if (blocked) return { ok: false, error: blocked };
    let res;
    try {
      res = JSON.parse(text);
    } catch (_) {
      return { ok: false, error: "发布失败: " + String(text || "").slice(0, 120) };
    }
    if (Array.isArray(res)) {
      if (res[0] === 1) return { ok: false, error: res[1] || "发布失败" };
      const data = res[1];
      if (data && data.id) return { ok: true, id: String(data.id) };
      return { ok: false, error: "发布失败: 响应缺少 id" };
    }
    if (!res || !res.id) {
      const errorMsg =
        (res && (res.message || res.msg || res.error || res.errMsg)) ||
        JSON.stringify(res).slice(0, 200);
      return { ok: false, error: errorMsg };
    }
    return { ok: true, id: String(res.id) };
  }

  // 图片上传响应（segmentfault.ts:153-159）：
  //   新版 {url|result}；旧版 [0,url,id] / [1,msg]
  function parseSegmentfaultImageUpload(text) {
    const blocked = segmentfaultBlocked(text);
    if (blocked) return { ok: false, error: blocked };
    let res;
    try {
      res = JSON.parse(text);
    } catch (_) {
      return { ok: false, error: "图片上传失败: " + String(text || "").slice(0, 120) };
    }
    if (Array.isArray(res)) {
      if (res[0] === 1) return { ok: false, error: res[1] || "图片上传失败" };
      return { ok: true, url: res[1] || "https://image-static.segmentfault.com/" + res[2] };
    }
    const url = res && (res.result || res.url);
    return url ? { ok: true, url } : { ok: false, error: "图片上传失败" };
  }

  // 登录检测页解析（segmentfault.ts:48-58）：href="/u/xxx" + avatar-static 头像
  const parseSegmentfaultUser = (html) => {
    const s = String(html || "");
    const userLinkMatch = s.match(/href="\/u\/([^"]+)"/);
    if (!userLinkMatch) return { ok: false, error: "未登录" };
    const avatarMatch = s.match(/src="(https:\/\/avatar-static\.segmentfault\.com\/[^"]+)"/);
    return {
      ok: true,
      userId: userLinkMatch[1],
      username: userLinkMatch[1],
      avatar: avatarMatch ? avatarMatch[1] : undefined,
    };
  };

  // /write 页 token 提取（segmentfault.ts:74-110，双分支）：
  //   新版 serverData":{"Token":"xxx"；旧版 window.g_initialProps = {...};\n\t</script>
  function parseSegmentfaultToken(html) {
    const s = String(html || "");
    const tokenMatch = s.match(/serverData":\s*\{\s*"Token"\s*:\s*"([^"]+)"/);
    if (tokenMatch) return { ok: true, token: tokenMatch[1] };
    // 旧版格式
    const markStr = "window.g_initialProps = ";
    const authIndex = s.indexOf(markStr);
    if (authIndex === -1) return { ok: false, error: "获取 session token 失败（未登录或页面改版）" };
    const endIndex = s.indexOf(";\n\t</script>", authIndex);
    if (endIndex === -1) return { ok: false, error: "解析 session token 失败" };
    try {
      const config = JSON.parse(s.substring(authIndex + markStr.length, endIndex));
      const token = config && config.global && config.global.sessionInfo && config.global.sessionInfo.key;
      if (!token) return { ok: false, error: "session token 为空" };
      return { ok: true, token };
    } catch (e) {
      return { ok: false, error: "解析 session token 失败: " + (e && e.message) };
    }
  }

  // =============================================================== 开源中国
  // 草稿体（oschina.ts:153-162；contentType 1=markdown 2=html）
  const oschinaDraftBody = (title, userId, content, contentType) =>
    JSON.stringify({
      title: String(title || ""),
      user: Number(userId) || 0,
      content: String(content || ""),
      contentType: contentType === 2 ? 2 : 1,
      catalog: 0,
      originUrl: "",
      privacy: true,
      disableComment: false,
    });

  // 登录检测响应（oschina.ts:46-59）：{success,result:{userId,userVo}}
  const parseOschinaMe = (data) => {
    if (!data || !data.success || !data.result || !data.result.userId) {
      return { ok: false, error: "未登录" };
    }
    return {
      ok: true,
      userId: String(data.result.userId),
      username: (data.result.userVo && data.result.userVo.name) || String(data.result.userId),
      avatar: data.result.userVo && data.result.userVo.portraitUrl,
    };
  };

  // 草稿响应（oschina.ts:166-174）：{success,result:{id}}
  const parseOschinaDraft = (data) => {
    if (!data || !data.success || !data.result || !data.result.id) {
      return { ok: false, error: (data && data.message) || "发布失败" };
    }
    return { ok: true, id: String(data.result.id) };
  };

  // 图片上传响应（oschina.ts:100-108）：{success,result:<url>}
  const parseOschinaImageUpload = (data) => {
    if (!data || !data.success || !data.result) {
      return { ok: false, error: (data && data.message) || "图片上传失败" };
    }
    return { ok: true, url: data.result };
  };

  // =============================================================== CMS（XML-RPC）
  // WordPress 测试连接（wordpress.ts:121-124）：wp.getUsersBlogs [user, pass]
  const wordpressTestBody = (username, password) =>
    ADL.syncXml.build("wp.getUsersBlogs", [String(username || ""), String(password || "")]);

  // WordPress 发布（wordpress.ts:448-468）：wp.newPost [0, user, pass, struct]
  function wordpressNewPostBody(username, password, title, content, draftOnly) {
    const post = {
      post_title: String(title || ""),
      post_content: String(content || ""),
      post_status: draftOnly === false ? "publish" : "draft",
      post_type: "post",
    };
    return ADL.syncXml.build("wp.newPost", [0, String(username || ""), String(password || ""), post]);
  }

  // WordPress 图片上传（wordpress.ts:163-175）：wp.uploadFile [0,user,pass,struct]
  // bits 传 base64 字符串（{__base64} → <base64>）
  function wordpressUploadFileBody(username, password, name, mimeType, base64) {
    const fileData = {
      name: String(name || ""),
      type: String(mimeType || ""),
      bits: { __base64: String(base64 || "") },
      overwrite: true,
    };
    return ADL.syncXml.build("wp.uploadFile", [0, String(username || ""), String(password || ""), fileData]);
  }

  // Typecho 测试连接（metaweblog.ts:618-622）：
  //   metaWeblog.getUsersBlogs ['', user, pass]
  const typechoTestBody = (username, password) =>
    ADL.syncXml.build("metaWeblog.getUsersBlogs", ["", String(username || ""), String(password || "")]);

  // Typecho 发布（metaweblog.ts:674-686）：
  //   metaWeblog.newPost [0(数字), user, pass, {title, description}, false]
  //   publish 固定 false（仅草稿）
  function typechoNewPostBody(username, password, title, content) {
    const post = {
      title: String(title || ""),
      description: String(content || "").trim(),
    };
    return ADL.syncXml.build("metaWeblog.newPost", [0, String(username || ""), String(password || ""), post, false]);
  }

  // Typecho postId=0 兜底（metaweblog.ts:141-146）：
  //   metaWeblog.getRecentPosts [0, user, pass, 1]
  const typechoRecentPostsBody = (username, password) =>
    ADL.syncXml.build("metaWeblog.getRecentPosts", [0, String(username || ""), String(password || ""), 1]);

  // MetaWeblog 通用发布（metaweblog.ts:504-516）：
  //   metaWeblog.newPost ['0'(字符串), user, pass, {title, description, categories:[]}, !draftOnly]
  function metaweblogNewPostBody(username, password, title, content, draftOnly) {
    const post = {
      title: String(title || ""),
      description: String(content || ""),
      categories: [],
    };
    return ADL.syncXml.build("metaWeblog.newPost", [
      "0",
      String(username || ""),
      String(password || ""),
      post,
      !(draftOnly !== false),
    ]);
  }

  // CMS 图片上传（metaweblog.ts:229-247）：bits/bytes 双发提高兼容性
  function metaweblogNewMediaObjectBody(username, password, name, mimeType, base64) {
    const mediaObject = {
      name: String(name || ""),
      type: String(mimeType || ""),
      bits: { __base64: String(base64 || "") },
      bytes: { __base64: String(base64 || "") },
    };
    return ADL.syncXml.build("metaWeblog.newMediaObject", [
      0,
      String(username || ""),
      String(password || ""),
      mediaObject,
    ]);
  }

  // =============================================================== 引擎纯逻辑
  // 登录缓存 TTL 判定（engine.ts:24-28,204-208）：
  //   已登录缓存 5 分钟、未登录 30 秒；force 时全部失效
  const AUTH_TTL_OK = 5 * 60 * 1000;
  const AUTH_TTL_BAD = 30 * 1000;
  function authCacheDecide(cache, platformId, now, force) {
    const cached = cache && cache[platformId];
    if (!cached) return { fresh: false };
    const ttl = cached.ok ? AUTH_TTL_OK : AUTH_TTL_BAD;
    if (!force && now - cached.ts < ttl) return { fresh: true, item: cached };
    return { fresh: false };
  }

  // 历史上限裁剪（engine.ts MAX_HISTORY_ITEMS=30；新的在前）
  const capHistory = (list, max) =>
    (Array.isArray(list) ? list : []).slice(0, max || 30);

  ADL.syncBuild = {
    // 知乎
    zhihuCreateBody,
    zhihuPatchBody,
    zhihuTransformContent,
    zhihuTransformTables,
    parseZhihuMe,
    parseZhihuDraft,
    // 简书
    jianshuCreateBody,
    jianshuUpdateBody,
    jianshuCleanHtml,
    parseJianshuMe,
    parseJianshuNotebooks,
    parseJianshuDraft,
    parseJianshuImageUpload,
    // 博客园
    cnblogsPostBody,
    parseCnblogsUser,
    parseCnblogsPost,
    parseCnblogsImageUpload,
    parseXsrfFromCookie,
    // 思否
    segmentfaultDraftBody,
    parseSegmentfaultUser,
    parseSegmentfaultToken,
    parseSegmentfaultDraft,
    parseSegmentfaultImageUpload,
    segmentfaultBlocked,
    // 开源中国
    oschinaDraftBody,
    parseOschinaMe,
    parseOschinaDraft,
    parseOschinaImageUpload,
    // CMS
    wordpressTestBody,
    wordpressNewPostBody,
    wordpressUploadFileBody,
    typechoTestBody,
    typechoNewPostBody,
    typechoRecentPostsBody,
    metaweblogNewPostBody,
    metaweblogNewMediaObjectBody,
    // 引擎纯逻辑
    authCacheDecide,
    capHistory,
  };
})();
