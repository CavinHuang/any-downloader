// L1 运行时挂钩器 —— 运行在 MAIN world（document_start，抢在页面脚本之前）。
// 包装 window.fetch / XMLHttpRequest，按规则抄录页面自己发出的签名 API 响应，
// 通过 window.postMessage 交给 ISOLATED world 的 bridge.js 转发到 background。
// 设计约束（docs/MEDIA-ADAPTERS.md §2）：只读旁路，绝不改变页面行为；不 eval 任何代码。
(() => {
  if (window.__adlHooked) return;
  window.__adlHooked = true;

  // —— 监听规则（唯一事实来源；新增平台在这里加一条）
  const RULES = [
    { key: "douyin", re: /aweme\/v1\/web\/aweme\/(post|item|feed)|aweme\/v1\/feed|\/share\/video\//i },
    { key: "x", re: /\/i\/api\/graphql\/.*(TweetDetail|TweetResultByRestId|UserTweets|HomeTimeline|HomeLatestTimeline|UserMedia)/i },
    { key: "channels", re: /finder\.video\.qq\.com|mmfindermedia|channels\/finder/i },
    { key: "xhs", re: /\/api\/sns\/web\/v1\/(feed|note)/i },
  ];
  const MAX_TEXT = 2 * 1024 * 1024; // 单响应 > 2MB 不处理（时间线 JSON 通常 < 1MB）

  const abs = (u) => {
    try {
      return new URL(u, location.href).href;
    } catch (_) {
      return String(u || "");
    }
  };
  const matchRule = (u) => {
    if (!u) return null;
    const full = abs(u);
    for (const r of RULES) {
      if (r.re.test(full) || r.re.test(u)) return r;
    }
    return null;
  };

  const emit = (key, url, text) => {
    try {
      if (!text || text.length > MAX_TEXT) return;
      const data = JSON.parse(text);
      window.postMessage({ source: "adl-hook", key, url: abs(url), data }, "*");
    } catch (_) {
      // 非 JSON 响应（流式/protobuf/错误页）——忽略
    }
  };

  // —— fetch 包装：clone 后异步读取，原始响应原样返回给页面
  const origFetch = window.fetch;
  if (typeof origFetch === "function") {
    window.fetch = function (...args) {
      const p = origFetch.apply(this, args);
      try {
        const first = args[0];
        const url =
          typeof first === "string"
            ? first
            : first && typeof first.url === "string"
              ? first.url
              : "";
        const rule = matchRule(url);
        if (rule) {
          p.then((res) => {
            try {
              if (!res || res.status >= 400) return;
              res.clone().text().then((t) => emit(rule.key, url, t)).catch(() => {});
            } catch (_) {}
          }).catch(() => {});
        }
      } catch (_) {}
      return p;
    };
  }

  // —— XHR 包装：open 记 url，send 挂 load 监听（不覆盖页面自己的 onload）
  const XHRO = XMLHttpRequest.prototype;
  const origOpen = XHRO.open;
  const origSend = XHRO.send;
  XHRO.open = function (method, url, ...rest) {
    try {
      this.__adlUrl = String(url || "");
    } catch (_) {}
    return origOpen.call(this, method, url, ...rest);
  };
  XHRO.send = function (...args) {
    try {
      const rule = matchRule(this.__adlUrl);
      if (rule) {
        this.addEventListener("load", () => {
          try {
            let text = null;
            if (this.responseType === "" || this.responseType === "text") {
              text = this.responseText;
            } else if (this.responseType === "json") {
              text = JSON.stringify(this.response);
            }
            if (text) emit(rule.key, this.__adlUrl, text);
          } catch (_) {}
        });
      }
    } catch (_) {}
    return origSend.apply(this, args);
  };
})();
