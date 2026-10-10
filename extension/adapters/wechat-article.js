// 适配器：微信公众号文章图片批量下载（mp.weixin.qq.com）
// 数据源：DOM（img[data-src] 懒加载真实地址，src 为占位/缩放地址）。
// 原图规则（实测 2026-10）：尺寸段不能剥——剥掉整段 mmbiz 直接 400；
// 正确做法是把尾部数字段替换成 /0（0 = 原图，实测 /0=200、无段=400）。
// Referer 非必须（实测无 Referer 200），仍保留以防站点收紧。
(() => {
  // mmbiz 域过滤：钉在主机上。子串匹配（旧实现）会放过
  // https://evil.com/?u=mmbiz.qpic.cn/... 这类把域名藏在 query 里的形态。
  // 协议相对（//mmbiz.qpic.cn/…）先放行，由 normalize 升级 https。
  const isMmbiz = (u) => /^(?:https?:)?\/\/mmbiz\.qpic\.cn\//i.test(u);

  // 剥 tp=webp：微信按浏览器兼容下发 webp 转码字节，带着它引擎会把 webp
  // 数据存成 wx_fmt 声明的扩展名（.jpeg 里装 webp）；去掉即回原格式。
  // wxfrom/wx_lazy/from=appmsg 等只是来源/懒加载标记，不改变字节，保留。
  const stripWebpParam = (u) => {
    const qi = u.indexOf("?");
    if (qi < 0) return u;
    const base = u.slice(0, qi);
    const parts = u
      .slice(qi + 1)
      .split("&")
      .filter((p) => !/^tp=webp$/i.test(p));
    return parts.length ? `${base}?${parts.join("&")}` : base;
  };

  // mmbiz URL 归一化 —— wechat-article 与 articles（导出 HTML 内的图片引用）
  // 两适配器共用：两边产出同形 URL，background 的 rewriteDocRefs 才能按 URL
  // 匹配把微信文章 HTML 重写成相对引用（跨适配器合集模式的前提）。
  ADL.mmbizNormalize = (u) => {
    let s = String(u || "").trim();
    if (s.startsWith("//")) s = "https:" + s; // 协议相对 → https
    s = ADL.toHttps(s);
    // 主机归一小写：大小写变体（HTTP://MMBIZ…）必须与常规形态同去重键；
    // 只动 scheme+host，路径 token 大小写敏感不能碰。
    s = s.replace(/^(https:\/\/[^/?#]+)/i, (m) => m.toLowerCase());
    s = s.replace(/#.*$/, ""); // fragment 不参与请求，只干扰去重键
    // 形态一：路径带尺寸段（/640、/0…）→ 统一替换为 /0（原图）
    s = s.replace(
      /^([a-z][a-z0-9+.-]*:\/\/[^/?#]*\/[^/?#]+\/[^/?#]+)\/\d+(?=[?#]|$)/i,
      "$1/0"
    );
    // 形态二：路径完全没有尺寸段（浏览器渲染后的 src 常见，如
    // /sz_mmbiz_jpg/<hash>?wxfrom=12&tp=webp…）→ 在 query 前补 /0。
    // 实测该形态原样请求 400，补 /0 后 200。
    if (!/\/\d+(?=[?#]|$)/.test(s)) {
      s = s.replace(
        /^([a-z][a-z0-9+.-]*:\/\/[^/?#]*\/[^/?#]+\/[^/?#]+)(?=[?#]|$)/i,
        "$1/0"
      );
    }
    return stripWebpParam(s);
  };

  // 同一元素多候选：data-src（懒加载真实地址）优先；data-src 为占位
  // base64/data URL/空时回退 src。两候选都不是 mmbiz → null（不收）。
  const pickUrl = (im) => {
    const cand = [];
    try {
      const d = im.getAttribute && im.getAttribute("data-src");
      if (d) cand.push(d);
    } catch (_) {}
    if (im.src) cand.push(im.src);
    for (const c of cand) {
      const t = String(c).trim();
      if (t && isMmbiz(t)) return t;
    }
    return null;
  };

  // 同源 iframe 兜底：公众号素材编辑页（UEditor）正文在 iframe 里，
  // 顶层 document.images 收不到。跨域卡片 iframe（音乐/视频）访问
  // contentDocument 会抛 SecurityError，逐个 try/catch 放过。
  const collectImgs = (doc) => {
    const arr = [...((doc && doc.images) || [])];
    if (doc && doc.querySelectorAll) {
      for (const f of doc.querySelectorAll("iframe")) {
        try {
          const idoc = f.contentDocument;
          if (idoc && idoc.images) arr.push(...idoc.images);
        } catch (_) {}
      }
    }
    return arr;
  };

  ADL.adapters["wechat-article"] = {
    id: "wechat-article",
    lastSource: "dom",

    extract(ctx) {
      const doc = ctx.dom();
      const out = [];
      const seen = new Set();
      const h1 =
        doc.querySelector("#activity-name") ||
        doc.querySelector(".rich_media_title");
      const og = doc.querySelector('meta[property="og:title"]');
      const title =
        ADL.cleanName(
          (h1 && h1.textContent) || (og && og.content) || doc.title || "微信文章",
          50
        ) || "wechat";

      for (const im of collectImgs(doc)) {
        if (!im) continue;
        const raw = pickUrl(im);
        if (!raw) continue;
        const u = this.normalize(raw);
        const key = u.split("?")[0];
        if (seen.has(key)) continue; // 同图多尺寸去重（/640 vs /0、tp=webp 差异）
        seen.add(key);
        // 原格式：wx_fmt 优先（值归一小写），模板别名 data-type 兜底
        const fmt =
          (
            (/(&|\?)wx_fmt=(\w+)/i.exec(u) || [])[2] ||
            ADL.cleanName((im.getAttribute && im.getAttribute("data-type")) || "", 10) ||
            "jpeg"
          ).toLowerCase() || "jpeg";
        out.push({
          url: ADL.toHttps(u),
          kind: "image",
          // wx_fmt=svg 长图实为 svg+ts 组合流（单直链还原不了），默认跳过，
          // 悬浮面板仍可手动勾选；见 docs 风险记录
          optional: fmt === "svg" || undefined,
          filename: `${title}_${String(out.length + 1).padStart(2, "0")}.${fmt}`,
          headers: { Referer: "https://mp.weixin.qq.com/" },
          meta: { extra: { article: doc.title || "" } },
        });
      }
      return out;
    },

    // https://mmbiz.qpic.cn/mmbiz_jpg/XXX/640?wx_fmt=jpeg → …/XXX/0?wx_fmt=jpeg
    // /0 = 原图（实测 200）；剥掉尺寸段会 400（微信要求路径带尺寸段）。
    // 主机不钉死：镜像/反代主机同样 /0 化；
    // 但 extract 的 isMmbiz 门仍只放行 mmbiz.qpic.cn，镜像域不会进入管线。
    normalize(u) {
      return ADL.mmbizNormalize(u);
    },
  };
})();
