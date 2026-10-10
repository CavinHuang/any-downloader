// 平台发布适配器（扩展页 sync.html 上下文运行）—— 8 个首批平台：
//   社区：知乎 / 简书 / 博客园 / 思否 / 开源中国（Cookie 登录态，fetch credentials:'include'）
//   自建站：WordPress / Typecho / MetaWeblog（账密 + XML-RPC，cmsPlatform 工厂按账号生成）
// 移植自 code-box 平台适配器（GPL-3.0）；请求规格证据见各函数注释行号。
// 纯函数部分（请求体/响应解析）在 ADL.syncBuild（payloads.js），此处只编排：
//   fetch（30s AbortController 超时）→ 调 payload 构造器 → 解析响应 → 草稿链接。
// 权限说明：扩展页 fetch 自带 Cookie（host_permissions <all_urls>）；
//   Origin/Referer 无法手工设置（浏览器禁用头）——code-box 用 declarativeNetRequest
//   动态规则兜底，本扩展未申请该权限（规格手测清单第 2 项），被目标站拒绝时
//   会以明确报错呈现，不静默失败。
// 图片上传：默认关闭（保留原始图片链接）；勾选「上传图片」后启用，上传失败
//   一律保留原 URL 继续同步（code-adapter.ts:322-326 语义）。
self.ADL = self.ADL || {};
(() => {
  const B = () => ADL.syncBuild; // payload 构造器（payloads.js）
  const IMG = () => ADL.syncImages; // 图片处理（markdown-images.js）

  // ============================================================ 请求基件

  // fetch + 30s AbortController 超时（code-box sync/runtime/extension.ts:21-41 形态）
  function fetchTimeout(url, init, timeoutMs, signal) {
    const ctrl = new AbortController();
    const timer = setTimeout(() => ctrl.abort(new Error("请求超时（30 秒）")), timeoutMs || 30000);
    const onOuterAbort = () => ctrl.abort(new Error("已取消"));
    if (signal) {
      if (signal.aborted) onOuterAbort();
      else signal.addEventListener("abort", onOuterAbort, { once: true });
    }
    return fetch(url, { credentials: "include", ...init, signal: ctrl.signal }).finally(() => {
      clearTimeout(timer);
      if (signal) signal.removeEventListener("abort", onOuterAbort);
    });
  }

  const errSnippet = (t) => (t ? " - " + String(t).slice(0, 200) : "");

  // JSON 请求：非 2xx 或非 JSON 均明确报错（规格 d：JSON 失败回退 text 报错）
  async function fetchJson(url, init, what) {
    const resp = await fetchTimeout(url, init);
    const text = await resp.text();
    if (!resp.ok) throw new Error((what || "请求") + "失败: HTTP " + resp.status + errSnippet(text));
    try {
      return JSON.parse(text);
    } catch (_) {
      throw new Error((what || "请求") + "失败: 响应不是有效 JSON - " + String(text).slice(0, 120));
    }
  }

  // 宽松版：2xx 但非 JSON（204 空体等）→ 返回原文（code-adapter.ts:219-232 语义）
  async function fetchJsonLenient(url, init, what) {
    const resp = await fetchTimeout(url, init);
    const text = await resp.text();
    if (!resp.ok) throw new Error((what || "请求") + "失败: HTTP " + resp.status + errSnippet(text));
    try {
      return JSON.parse(text);
    } catch (_) {
      return text;
    }
  }

  const fetchText = async (url, init, signal) => {
    const resp = await fetchTimeout(url, init, 30000, signal);
    const text = await resp.text();
    if (!resp.ok) throw new Error("请求失败: HTTP " + resp.status + errSnippet(text));
    return text;
  };

  async function downloadBlob(src, signal) {
    const resp = await fetchTimeout(src, {}, 30000, signal);
    if (!resp.ok) throw new Error("图片下载失败: HTTP " + resp.status + " - " + src.slice(0, 80));
    return resp.blob();
  }

  const delay = (ms) => new Promise((r) => setTimeout(r, ms));

  // 通用错误包装：publish 抛错 → {ok:false, error}
  const guard = (fn) => async (...args) => {
    try {
      return await fn(...args);
    } catch (e) {
      return { ok: false, error: String((e && e.message) || e) };
    }
  };

  // 检查登录的通用包装：任何异常 → 未登录 + 原因（engine 会展示）
  const authGuard = (fn) => async () => {
    try {
      return await fn();
    } catch (e) {
      return { ok: false, error: String((e && e.message) || e) };
    }
  };

  // ============================================================ 知乎
  // 登录检测（zhihu.ts:64-94）：GET /api/v4/me + x-requested-with:fetch
  async function zhihuCheckAuth() {
    const data = await fetchJson(
      "https://www.zhihu.com/api/v4/me",
      { headers: { "x-requested-with": "fetch" } },
      "登录检测"
    );
    return B().parseZhihuMe(data);
  }

  // 图片 URL 直传（zhihu.ts:300-320）：POST /api/uploaded_images
  //   application/x-www-form-urlencoded，url=<原URL>&source=article → {src}
  async function zhihuUploadByUrl(src, signal) {
    if (src.startsWith("data:")) throw new Error("data URI 不支持 URL 直传");
    const data = await fetchJson(
      "https://zhuanlan.zhihu.com/api/uploaded_images",
      {
        method: "POST",
        headers: { "x-requested-with": "fetch", "Content-Type": "application/x-www-form-urlencoded" },
        body: new URLSearchParams({ url: src, source: "article" }).toString(),
        signal,
      },
      "图片上传"
    );
    if (!data || !data.src) throw new Error("图片上传失败");
    return { url: data.src };
  }

  // 发布（zhihu.ts:96-191）：POST drafts 创建 → PATCH /{id}/draft 写正文
  const zhihuPublish = guard(async (article, opts) => {
    const signal = opts.signal;
    let content = article.html || "";
    if (opts.enableImageUpload && content) {
      content = await IMG().processImages(content, (src) => zhihuUploadByUrl(src, signal), {
        skipPatterns: ["zhimg.com"], // 平台自家 CDN 跳过（zhihu.ts:147）
        delayFn: delay,
      });
    }
    content = B().zhihuTransformContent(content);

    const created = await fetchJson(
      "https://zhuanlan.zhihu.com/api/articles/drafts",
      {
        method: "POST",
        headers: { "Content-Type": "application/json", "x-requested-with": "fetch" },
        body: B().zhihuCreateBody(article.title),
        signal,
      },
      "创建草稿"
    );
    const d1 = B().parseZhihuDraft(created);
    if (!d1.ok) throw new Error(d1.error);

    const resp = await fetchTimeout("https://zhuanlan.zhihu.com/api/articles/" + d1.id + "/draft", {
      method: "PATCH",
      headers: { "Content-Type": "application/json", "x-requested-with": "fetch" },
      body: B().zhihuPatchBody(article.title, content),
      signal,
    });
    if (!resp.ok) {
      const text = await resp.text().catch(() => "");
      throw new Error("更新草稿失败: HTTP " + resp.status + errSnippet(text));
    }
    return { ok: true, id: d1.id, draftUrl: "https://zhuanlan.zhihu.com/p/" + d1.id + "/edit" };
  });

  // ============================================================ 简书
  // 登录检测（jianshu.ts:29-56）：GET /settings/basic.json
  async function jianshuCheckAuth() {
    const data = await fetchJson("https://www.jianshu.com/settings/basic.json", {}, "登录检测");
    return B().parseJianshuMe(data);
  }

  // 七牛上传（jianshu.ts:188-261）：token.json → multipart(file+token) → {url|key}
  async function jianshuUploadByUrl(src, signal) {
    if (src.startsWith("data:")) throw new Error("data URI 不支持上传");
    const blob = await downloadBlob(src, signal);
    const tok = await fetchJson(
      "https://www.jianshu.com/upload_images/token.json",
      { headers: { Accept: "application/json" }, signal },
      "获取上传凭证"
    );
    const ext = (blob.type && blob.type.split("/")[1]) || "png";
    const fd = new FormData();
    fd.append("file", blob, Date.now() + "." + ext);
    fd.append("token", tok.token);
    const resp = await fetchTimeout(tok.url, { method: "POST", body: fd, signal });
    const data = await resp.json();
    const r = B().parseJianshuImageUpload(data);
    if (!r.ok) throw new Error(r.error);
    return { url: r.url };
  }

  // 发布（jianshu.ts:91-183）：取第一个文集 → 创建 → 清理 HTML → 更新
  const jianshuPublish = guard(async (article, opts) => {
    const signal = opts.signal;
    const notebooks = await fetchJson(
      "https://www.jianshu.com/author/notebooks",
      { headers: { Accept: "application/json" }, signal },
      "获取文集"
    );
    const nb = B().parseJianshuNotebooks(notebooks);
    if (!nb.ok) throw new Error(nb.error);

    const created = await fetchJson(
      "https://www.jianshu.com/author/notes",
      {
        method: "POST",
        headers: { "Content-Type": "application/json", Accept: "application/json" },
        body: B().jianshuCreateBody(nb.notebookId, article.title),
        signal,
      },
      "创建草稿"
    );
    const d1 = B().parseJianshuDraft(created);
    if (!d1.ok) throw new Error(d1.error);

    let content = B().jianshuCleanHtml(article.html || article.markdown || "");
    if (opts.enableImageUpload && content) {
      content = await IMG().processImages(content, (src) => jianshuUploadByUrl(src, signal), {
        skipPatterns: ["jianshu.com", "jianshuapi.com", "upload-images.jianshu.io"], // jianshu.ts:142
        delayFn: delay,
      });
    }

    await fetchJson(
      "https://www.jianshu.com/author/notes/" + d1.id,
      {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: B().jianshuUpdateBody(article.title, content),
        signal,
      },
      "更新草稿"
    );
    return {
      ok: true,
      id: d1.id,
      draftUrl: "https://www.jianshu.com/writer#/notebooks/" + nb.notebookId + "/notes/" + d1.id,
    };
  });

  // ============================================================ 博客园
  // XSRF-TOKEN 降级链（规格手测清单第 3 项；cnblogs.ts:52-89 的 chrome.cookies
  // 路线因 manifest 无 cookies 权限不可用）：
  //   ① 从任意已打开博客园标签页的内容脚本读 document.cookie（httpOnly 时读不到）
  //   ② GET i.cnblogs.com/posts/edit 触发 Set-Cookie 后重试一次
  //   ③ 仍无 → 明确报错（提示补 cookies 权限的替代路线）
  async function cnblogsXsrfViaTabs() {
    let tabs = [];
    try {
      tabs = await chrome.tabs.query({ url: ["*://*.cnblogs.com/*"] });
    } catch (_) {}
    for (const t of tabs) {
      try {
        const r = await chrome.tabs.sendMessage(t.id, { type: "adl-sync-cookies" });
        const v = B().parseXsrfFromCookie(r && r.cookie);
        if (v) return v;
      } catch (_) {}
    }
    return null;
  }

  async function cnblogsXsrfToken(signal) {
    let token = await cnblogsXsrfViaTabs();
    if (token) return token;
    // 先访问编辑页触发 Set-Cookie（cnblogs.ts:59-62）后重试
    await fetchTimeout("https://i.cnblogs.com/posts/edit", { signal }).catch(() => {});
    token = await cnblogsXsrfViaTabs();
    if (!token) {
      throw new Error(
        "获取 XSRF-TOKEN 失败：请先登录博客园，并保持任意一个博客园页面处于打开状态" +
          "（其 Cookie 非 httpOnly 时可读取；否则需为扩展添加 cookies 权限走 chrome.cookies 直读）"
      );
    }
    return token;
  }

  // 登录检测（cnblogs.ts:91-122）：home.cnblogs.com/user/CurrentUserInfo HTML 解析
  const cnblogsCheckAuth = authGuard(async () => {
    const html = await fetchText("https://home.cnblogs.com/user/CurrentUserInfo", {});
    return B().parseCnblogsUser(html);
  });

  // CORS 上传（cnblogs.ts:255-312）：multipart(image/app/uploadType) + x-xsrf-token
  async function cnblogsUploadByUrl(src, xsrf, signal) {
    if (src.startsWith("data:")) throw new Error("data URI 不支持上传");
    const blob = await downloadBlob(src, signal);
    const fd = new FormData();
    fd.append("image", blob, "image.png");
    fd.append("app", "blog");
    fd.append("uploadType", "Select");
    const resp = await fetchTimeout(
      "https://upload.cnblogs.com/v2/images/cors-upload",
      { method: "POST", headers: { "x-xsrf-token": xsrf }, body: fd, signal }
    );
    const text = await resp.text();
    if (!resp.ok) throw new Error("图片上传失败: HTTP " + resp.status + errSnippet(text));
    let data;
    try {
      data = JSON.parse(text);
    } catch (_) {
      throw new Error("图片上传失败: 响应不是 JSON - " + text.slice(0, 100));
    }
    const r = B().parseCnblogsImageUpload(data);
    if (!r.ok) throw new Error(r.error);
    return { url: r.url };
  }

  // 发布（cnblogs.ts:124-249）：XSRF → POST i.cnblogs.com/api/posts（Markdown）
  const cnblogsPublish = guard(async (article, opts) => {
    const signal = opts.signal;
    const xsrf = await cnblogsXsrfToken(signal);
    let markdown = article.markdown || "";
    if (opts.enableImageUpload && markdown) {
      markdown = await IMG().processImages(markdown, (src) => cnblogsUploadByUrl(src, xsrf, signal), {
        skipPatterns: ["cnblogs.com", "img2024.cnblogs.com", "img2023.cnblogs.com"], // cnblogs.ts:146
        delayFn: delay,
      });
    }
    const resp = await fetchTimeout("https://i.cnblogs.com/api/posts", {
      method: "POST",
      headers: { "Content-Type": "application/json", "x-xsrf-token": xsrf },
      body: B().cnblogsPostBody(article.title, markdown, new Date().toISOString()),
      signal,
    });
    if (resp.status === 401 || resp.status === 403) {
      throw new Error("未登录或登录已过期，请重新登录博客园"); // cnblogs.ts:219-221 文案
    }
    const text = await resp.text();
    if (!resp.ok) throw new Error("创建草稿失败: HTTP " + resp.status + errSnippet(text));
    let data;
    try {
      data = JSON.parse(text);
    } catch (_) {
      throw new Error("创建草稿失败: 响应不是有效 JSON - " + text.slice(0, 100));
    }
    const r = B().parseCnblogsPost(data);
    if (!r.ok) throw new Error(r.error);
    return { ok: true, id: r.id, draftUrl: "https://i.cnblogs.com/articles/edit;postId=" + r.id };
  });

  // ============================================================ 思否
  // 登录检测（segmentfault.ts:41-69）：/user/settings HTML 解析
  const segmentfaultCheckAuth = authGuard(async () => {
    const html = await fetchText("https://segmentfault.com/user/settings", {});
    return B().parseSegmentfaultUser(html);
  });

  // /write 页取 token（segmentfault.ts:74-110）
  async function segmentfaultToken(signal) {
    const html = await fetchText("https://segmentfault.com/write", { signal });
    const r = B().parseSegmentfaultToken(html);
    if (!r.ok) throw new Error(r.error);
    return r.token;
  }

  // 图片上传（segmentfault.ts:115-160）：multipart image + token 头
  async function segmentfaultUploadByUrl(src, token, signal) {
    if (src.startsWith("data:")) throw new Error("data URI 不支持上传");
    const blob = await downloadBlob(src, signal);
    const fd = new FormData();
    fd.append("image", blob);
    const resp = await fetchTimeout("https://segmentfault.com/gateway/image", {
      method: "POST",
      headers: { token },
      body: fd,
      signal,
    });
    const text = await resp.text();
    const r = B().parseSegmentfaultImageUpload(text);
    if (!r.ok) throw new Error(r.error);
    return { url: r.url };
  }

  // 发布（segmentfault.ts:165-246）：token → POST /gateway/draft（Markdown）
  const segmentfaultPublish = guard(async (article, opts) => {
    const signal = opts.signal;
    const token = await segmentfaultToken(signal);
    let content = article.markdown || article.html || "";
    if (opts.enableImageUpload && content) {
      content = await IMG().processImages(content, (src) => segmentfaultUploadByUrl(src, token, signal), {
        delayFn: delay,
      });
    }
    const resp = await fetchTimeout("https://segmentfault.com/gateway/draft", {
      method: "POST",
      headers: { "Content-Type": "application/json", token, accept: "*/*" },
      body: B().segmentfaultDraftBody(article.title, content),
      signal,
    });
    const text = await resp.text();
    if (!resp.ok && !text) throw new Error("发布失败: HTTP " + resp.status);
    const r = B().parseSegmentfaultDraft(text);
    if (!r.ok) throw new Error(r.error);
    return { ok: true, id: r.id, draftUrl: "https://segmentfault.com/write?draftId=" + r.id };
  });

  // ============================================================ 开源中国
  // 登录检测（oschina.ts:41-72）：GET user/myDetails → {success,result}
  const oschinaCheckAuth = authGuard(async () => {
    const data = await fetchJson("https://apiv1.oschina.net/oschinapi/user/myDetails", {}, "登录检测");
    return B().parseOschinaMe(data);
  });

  // 图片上传（oschina.ts:77-111）：multipart file → {success,result:<url>}
  async function oschinaUploadByUrl(src, signal) {
    if (src.startsWith("data:")) throw new Error("data URI 不支持上传");
    const blob = await downloadBlob(src, signal);
    let filename = "image";
    try {
      const name = new URL(src).pathname.split("/").pop();
      if (name && name.trim()) filename = name;
    } catch (_) {}
    const fd = new FormData();
    fd.append("file", blob, filename);
    const data = await fetchJson(
      "https://apiv1.oschina.net/oschinapi/ai/creation/project/uploadDetail",
      { method: "POST", body: fd, signal },
      "图片上传"
    );
    const r = B().parseOschinaImageUpload(data);
    if (!r.ok) throw new Error(r.error);
    return { url: r.url };
  }

  // 发布（oschina.ts:126-193）：myDetails 拿 userId → save_draft（Markdown）
  const oschinaPublish = guard(async (article, opts) => {
    const signal = opts.signal;
    const me = await oschinaCheckAuth();
    if (!me.ok) throw new Error(me.error || "未登录");
    const userId = me.userId;
    const useMarkdown = String(article.markdown || "").trim().length > 0;
    let content = useMarkdown ? article.markdown : article.html || "";
    if (opts.enableImageUpload && content) {
      content = await IMG().processImages(content, (src) => oschinaUploadByUrl(src, signal), {
        delayFn: delay,
      });
    }
    const data = await fetchJson(
      "https://apiv1.oschina.net/oschinapi/api/draft/save_draft",
      {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: B().oschinaDraftBody(article.title, userId, content, useMarkdown ? 1 : 2),
        signal,
      },
      "创建草稿"
    );
    const r = B().parseOschinaDraft(data);
    if (!r.ok) throw new Error(r.error);
    return {
      ok: true,
      id: r.id,
      draftUrl: "https://my.oschina.net/u/" + userId + "/blog/write/draft/" + r.id,
    };
  });

  // ============================================================ 自建站（CMS 三件套）
  const trimUrl = (u) => String(u || "").replace(/\/+$/, "");

  // XML-RPC 端点（metaweblog.ts:170-176 + typecho 固定 /action/xmlrpc）
  function cmsEndpoint(account) {
    if (account.type === "typecho") return trimUrl(account.url) + "/action/xmlrpc";
    if (account.endpoint) return account.endpoint;
    return trimUrl(account.url) + "/xmlrpc.php";
  }

  const MIME_EXT = {
    "image/jpeg": "jpg", "image/jpg": "jpg", "image/png": "png",
    "image/gif": "gif", "image/webp": "webp", "image/bmp": "bmp", "image/svg+xml": "svg",
  };

  const safeHost = (u) => {
    try {
      return new URL(u).hostname;
    } catch (_) {
      return null;
    }
  };

  // CMS 图片上传（metaweblog.ts:309-348 重试 10 次递增延迟 + 440-449 同域跳过；
  // wordpress 走 wp.uploadFile，Typecho/MetaWeblog 走 newMediaObject bits/bytes 双发）
  async function cmsUploadByUrl(account, password, src, signal) {
    if (!src || src.startsWith("data:")) return null;
    const siteHost = safeHost(account.url);
    if (siteHost && safeHost(src) === siteHost) return null; // 同域图跳过
    for (let attempt = 1; attempt <= 10; attempt++) {
      if (signal && signal.aborted) return null;
      try {
        const blob = await downloadBlob(src, signal);
        const b64 = ADL.syncXml.toBase64(new Uint8Array(await blob.arrayBuffer()));
        const mime = blob.type || "image/jpeg";
        const name = "image_" + Date.now() + "." + (MIME_EXT[mime] || "png");
        const body =
          account.type === "wordpress"
            ? B().wordpressUploadFileBody(account.username, password, name, mime, b64)
            : B().metaweblogNewMediaObjectBody(account.username, password, name, mime, b64);
        const resp = await fetchTimeout(cmsEndpoint(account), {
          method: "POST",
          headers: { "Content-Type": "text/xml" },
          body,
          signal,
        });
        const xml = await resp.text();
        const url = ADL.syncXml.extractMediaUrl(xml);
        if (url) return { url };
        const parsed = ADL.syncXml.parseResponse(xml);
        throw new Error(parsed.ok ? "无法解析上传结果" : parsed.error);
      } catch (_) {
        // 重试（递增延迟 1s/2s/.../9s）
      }
      if (attempt < 10) await delay(1000 * attempt);
    }
    return null; // 全部失败 → 上层保留原 URL
  }

  // CMS 登录检测 = 测试连接：
  //   WordPress wp.getUsersBlogs（wordpress.ts:121-124）；
  //   Typecho/MetaWeblog metaWeblog.getUsersBlogs（metaweblog.ts:618-622）
  function cmsCheckAuth(account, getPassword) {
    return authGuard(async () => {
      const password = await getPassword(account.id);
      if (!password) return { ok: false, error: "未保存密码，请重新编辑账号" };
      const body =
        account.type === "wordpress"
          ? B().wordpressTestBody(account.username, password)
          : B().typechoTestBody(account.username, password);
      const resp = await fetchTimeout(cmsEndpoint(account), {
        method: "POST",
        headers: { "Content-Type": "text/xml" },
        body,
      });
      const parsed = ADL.syncXml.parseResponse(await resp.text());
      if (!resp.ok) return { ok: false, error: "HTTP " + resp.status };
      if (!parsed.ok) return { ok: false, error: parsed.error || "XML-RPC 错误" };
      return { ok: true, username: account.username };
    });
  }

  // CMS 发布：按账号类型分派（wordpress.ts:420-491 / metaweblog.ts:652-734,486-548）
  function cmsPublish(account, getPassword) {
    return guard(async (article, opts) => {
      const signal = opts.signal;
      const password = await getPassword(account.id);
      if (!password) throw new Error("未保存密码，请重新编辑账号");

      // HTML 内容（用户编辑过 Markdown 时由页面层先转 HTML 再进来）
      let content = article.html || "";
      if (opts.enableImageUpload && content) {
        content = await IMG().processImages(
          content,
          (src) =>
            cmsUploadByUrl(account, password, src, signal).then((r) => {
              if (!r) throw new Error("图片上传失败/跳过");
              return r;
            }),
          { delayFn: delay }
        );
      }

      const post = async (body) => {
        const resp = await fetchTimeout(cmsEndpoint(account), {
          method: "POST",
          headers: { "Content-Type": "text/xml" },
          body,
          signal,
        });
        const xml = await resp.text();
        if (!resp.ok) throw new Error("HTTP " + resp.status);
        const parsed = ADL.syncXml.parseResponse(xml);
        parsed.raw = xml; // Typecho postId=0 兜底需要原始响应提取 postid
        return parsed;
      };

      const base = trimUrl(account.url);
      if (account.type === "wordpress") {
        const r = await post(B().wordpressNewPostBody(account.username, password, article.title, content, opts.draftOnly));
        if (!r.ok) throw new Error(r.error || "XML-RPC 错误");
        const id = String(r.value);
        return {
          ok: true,
          id,
          draftUrl: base + "/wp-admin/post.php?post=" + id + "&action=edit",
        };
      }

      if (account.type === "typecho") {
        const r = await post(B().typechoNewPostBody(account.username, password, article.title, content));
        if (!r.ok) throw new Error(r.error || "XML-RPC 错误");
        let id = String(r.value == null ? "" : r.value);
        // Typecho newPost 可能返回 0 → getRecentPosts 取最新 postid 兜底（metaweblog.ts:707-718）
        if (!id || id === "0") {
          const r2 = await post(B().typechoRecentPostsBody(account.username, password));
          const latest = r2.ok ? ADL.syncXml.extractLatestPostId(r2.raw || "") : null;
          if (latest) id = latest;
        }
        return {
          ok: true,
          id,
          draftUrl:
            id && id !== "0"
              ? base + "/admin/write-post.php?cid=" + id
              : base + "/admin/manage-posts.php",
        };
      }

      // metaweblog 通用（metaweblog.ts:504-548）
      const r = await post(
        B().metaweblogNewPostBody(account.username, password, article.title, content, opts.draftOnly)
      );
      if (!r.ok) throw new Error(r.error || "XML-RPC 错误");
      const id = String(r.value == null ? "" : r.value);
      return {
        ok: true,
        id,
        draftUrl: opts.draftOnly === false ? base + "/archives/" + id + "/" : base + "/admin/manage-posts.php?cid=" + id,
      };
    });
  }

  // ============================================================ 平台注册表
  // 社区五家（contentFormat 仅供 UI 展示：知乎/简书吃 HTML，其余吃 Markdown）
  const ALL = [
    {
      id: "zhihu",
      name: "知乎",
      group: "社区",
      contentFormat: "HTML",
      checkAuth: () => zhihuCheckAuth(),
      publish: zhihuPublish,
    },
    {
      id: "jianshu",
      name: "简书",
      group: "社区",
      contentFormat: "HTML",
      checkAuth: () => jianshuCheckAuth(),
      publish: jianshuPublish,
    },
    {
      id: "cnblogs",
      name: "博客园",
      group: "社区",
      contentFormat: "Markdown",
      checkAuth: cnblogsCheckAuth,
      publish: cnblogsPublish,
    },
    {
      id: "segmentfault",
      name: "思否",
      group: "社区",
      contentFormat: "Markdown",
      checkAuth: segmentfaultCheckAuth,
      publish: segmentfaultPublish,
    },
    {
      id: "oschina",
      name: "开源中国",
      group: "社区",
      contentFormat: "Markdown",
      checkAuth: oschinaCheckAuth,
      publish: oschinaPublish,
    },
  ];

  // CMS 账号 → 平台对象（id 形如 cms_wordpress_1699...，与 engine 存储键对应）
  function cmsPlatform(account, getPassword) {
    return {
      id: account.id,
      name: account.name || account.type + "：" + account.username,
      group: "自建站",
      contentFormat: "HTML",
      cms: true,
      account,
      checkAuth: cmsCheckAuth(account, getPassword),
      publish: cmsPublish(account, getPassword),
    };
  }

  ADL.syncPlatforms = { ALL, cmsPlatform, cmsEndpoint };
})();
