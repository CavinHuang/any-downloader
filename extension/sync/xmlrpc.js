// XML-RPC 请求构建/响应解析 —— 零依赖纯函数（文章同步 CMS 三件套共用）。
// 移植自 code-box sync/adapters/cms/metaweblog.ts:22-131（GPL-3.0）：
//   - 请求体必须是紧凑格式（不能有多余空白/换行），否则部分 XML-RPC 实现（Typecho）
//     会解析失败 —— 测试里有 /\n\s/ 不匹配断言护栏。
//   - 响应只做正则级解析（fault/string/int/i4/struct/array），不做完整 XML DOM。
// 挂载：ADL.syncXml；无 fetch、无 chrome.*、无 DOM，node 直接 require 即测。
self.ADL = self.ADL || {};
(() => {
  // —— XML 转义：五个实体，& 必须最先替换（否则二次转义）
  function escapeXml(str) {
    return String(str == null ? "" : str)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;")
      .replace(/'/g, "&apos;");
  }

  // —— Uint8Array → base64（图片上传 bits 参数用；node 22 与浏览器均有 btoa，
  //     兜底 Buffer 防极端环境）
  function toBase64(bytes) {
    let binary = "";
    const len = bytes.byteLength || bytes.length;
    for (let i = 0; i < len; i++) binary += String.fromCharCode(bytes[i]);
    if (typeof btoa === "function") return btoa(binary);
    if (typeof Buffer === "function") {
      return Buffer.from(bytes).toString("base64");
    }
    throw new Error("当前环境无 btoa/Buffer，无法 base64 编码");
  }

  // —— 单值 → <value> XML。类型规则（与 metaweblog.ts:48-66 语义一致）：
  //     string→<string>、number→<i4>（兼容性优于 <int>）、boolean→<boolean>、
  //     {__base64:"..."} 或 Uint8Array→<base64>、Array→<array>、对象→<struct>
  function valueXml(v) {
    if (typeof v === "string") return "<string>" + escapeXml(v) + "</string>";
    if (typeof v === "number") return "<i4>" + v + "</i4>";
    if (typeof v === "boolean") return "<boolean>" + (v ? 1 : 0) + "</boolean>";
    if (v instanceof Uint8Array) return "<base64>" + toBase64(v) + "</base64>";
    if (v && typeof v === "object" && typeof v.__base64 === "string") {
      return "<base64>" + v.__base64 + "</base64>";
    }
    if (Array.isArray(v)) {
      const items = v.map((x) => "<value>" + valueXml(x) + "</value>").join("");
      return "<array><data>" + items + "</data></array>";
    }
    if (v && typeof v === "object") return "<struct>" + structXml(v) + "</struct>";
    return "<string>" + escapeXml(String(v)) + "</string>";
  }

  // —— 对象 → <struct>（member 顺序 = Object.entries 顺序；key 一并转义更稳）
  function structXml(obj) {
    return Object.entries(obj)
      .map(
        ([key, value]) =>
          "<member><name>" + escapeXml(key) + "</name><value>" + valueXml(value) + "</value></member>"
      )
      .join("");
  }

  // —— 构建完整请求体（紧凑格式，禁止空白 —— metaweblog.ts:26-46 注释同款约束）
  function build(method, params) {
    const paramXml = (params || [])
      .map((p) => "<param><value>" + valueXml(p) + "</value></param>")
      .join("");
    return (
      '<?xml version="1.0"?><methodCall><methodName>' +
      escapeXml(method) +
      "</methodName><params>" +
      paramXml +
      "</params></methodCall>"
    );
  }

  // —— 解析响应（metaweblog.ts:92-122 语义）：
  //     {ok:true, value} | {ok:false, error}
  //     fault → 取 fault 内首个 <string> 作为错误信息；
  //     否则依次尝试 string/int/i4 标量；struct/array → value = {}（够用：
  //     调用方只关心 getUsersBlogs 是否成功）。
  function parseResponse(xml) {
    const s = String(xml || "");
    if (s.includes("<fault>")) {
      const faultMatch = s.match(/<string>([^<]+)<\/string>/);
      return { ok: false, error: faultMatch ? faultMatch[1] : "XML-RPC 错误" };
    }
    const stringMatch = s.match(/<string>([^<]*)<\/string>/);
    if (stringMatch) return { ok: true, value: stringMatch[1] };
    const intMatch = s.match(/<int>([^<]*)<\/int>/);
    if (intMatch) return { ok: true, value: intMatch[1] };
    const i4Match = s.match(/<i4>([^<]*)<\/i4>/);
    if (i4Match) return { ok: true, value: i4Match[1] };
    if (s.includes("<array>") || s.includes("<struct>")) return { ok: true, value: {} };
    return { ok: true, value: null };
  }

  // —— 从 getRecentPosts 响应提取 postid（Typecho newPost 返回 0 时的兜底：
  //     metaweblog.ts:127-131）
  function extractLatestPostId(xml) {
    const m = String(xml || "").match(
      /<name>postid<\/name>\s*<value>(?:<string>)?([^<]+)(?:<\/string>)?<\/value>/
    );
    return m ? m[1] : null;
  }

  // —— 从 newMediaObject / uploadFile 响应提取图片 URL（metaweblog.ts:265、
  //     wordpress.ts:192）
  function extractMediaUrl(xml) {
    const m = String(xml || "").match(
      /<name>url<\/name>\s*<value>(?:<string>)?([^<]+)(?:<\/string>)?<\/value>/
    );
    return m ? m[1] : null;
  }

  ADL.syncXml = {
    escapeXml,
    toBase64,
    build,
    parseResponse,
    extractLatestPostId,
    extractMediaUrl,
  };
})();
