// 通用流媒体清单旁路嗅探器 —— 运行在 MAIN world（document_start）。
// 对齐 Mister Download Manager 的首块魔数识别：无 .m3u8/.mpd 后缀、走 fetch/XHR 加载的
// 签名流媒体清单，直接复用页面已拿到的响应文本识别 #EXTM3U / <MPD，零额外请求不消耗一次性 token。
(() => {
  if (window.__adlMediaHooked) return;
  window.__adlMediaHooked = true;

  const MAX_LEN = 512 * 1024;
  const STREAM_URL_RE = /\.(m3u8|mpd)(?=[?#]|$)|m3u8|mpd|playlist|manifest|chunklist|master|hls|dash/i;
  const SKIP_MIME_RE = /(html|javascript|ecmascript|css|json|image\/|font\/|wasm|audio\/|video\/)/i;
  const MANIFEST_MIME_RE = /(mpegurl|dash\+xml|vnd\.apple\.mpegurl|x-mpegurl)/i;

  const abs = (u) => {
    try {
      return new URL(u, location.href).href;
    } catch (_) {
      return String(u || "");
    }
  };

  const inspectAndEmit = (url, text) => {
    try {
      if (!url || !text || text.length > MAX_LEN) return;
      const head = text.slice(0, 512).trimStart();
      let kind = null;
      if (head.startsWith("#EXTM3U")) kind = "hls";
      else if (/<MPD[\s>]/i.test(head)) kind = "dash";
      if (!kind) return;
      window.postMessage(
        { source: "adl-media-manifest", kind, url: abs(url), text },
        "*"
      );
    } catch (_) {}
  };

  const shouldInspectFetch = (url, res) => {
    if (!res || res.status < 200 || res.status >= 300) return false;
    const len = Number(res.headers.get("content-length") || 0);
    if (len > MAX_LEN) return false;
    const ct = (res.headers.get("content-type") || "").toLowerCase();
    if (MANIFEST_MIME_RE.test(ct)) return true;
    if (SKIP_MIME_RE.test(ct)) return false;
    if (!ct || /(text\/plain|octet-stream|xml)/.test(ct)) {
      return STREAM_URL_RE.test(url) || !ct || ct.includes("octet-stream") || ct.includes("text/plain");
    }
    return false;
  };

  const origFetch = window.fetch;
  if (typeof origFetch === "function") {
    window.fetch = function (...args) {
      const p = origFetch.apply(this, args);
      try {
        const first = args[0];
        const rawUrl =
          typeof first === "string"
            ? first
            : first && typeof first.url === "string"
              ? first.url
              : "";
        if (/^https?:/i.test(abs(rawUrl))) {
          p.then((res) => {
            try {
              const targetUrl = (res && res.url) || rawUrl;
              if (!shouldInspectFetch(targetUrl, res)) return;
              res
                .clone()
                .text()
                .then((t) => inspectAndEmit(targetUrl, t))
                .catch(() => {});
            } catch (_) {}
          }).catch(() => {});
        }
      } catch (_) {}
      return p;
    };
  }

  const XHRO = XMLHttpRequest.prototype;
  const origOpen = XHRO.open;
  const origSend = XHRO.send;
  XHRO.open = function (method, url, ...rest) {
    try {
      this.__adlMediaUrl = String(url || "");
    } catch (_) {}
    return origOpen.call(this, method, url, ...rest);
  };
  XHRO.send = function (...args) {
    try {
      const rawUrl = this.__adlMediaUrl;
      if (rawUrl && /^https?:/i.test(abs(rawUrl))) {
        this.addEventListener("load", () => {
          try {
            if (this.status < 200 || this.status >= 300) return;
            if (this.responseType !== "" && this.responseType !== "text") return;
            const text = this.responseText;
            if (!text || text.length > MAX_LEN) return;
            const ct = (this.getResponseHeader("content-type") || "").toLowerCase();
            if (ct && SKIP_MIME_RE.test(ct) && !MANIFEST_MIME_RE.test(ct)) return;
            inspectAndEmit(this.responseURL || rawUrl, text);
          } catch (_) {}
        });
      }
    } catch (_) {}
    return origSend.apply(this, args);
  };
})();
