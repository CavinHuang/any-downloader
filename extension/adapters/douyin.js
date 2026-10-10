// 适配器：抖音（www.douyin.com）
// 数据源（三级降级）：
//   1. hook —— L1 捕获的 /aweme/v1/web/* 响应（浏览信息流/主页/详情）
//   2. ssr —— self/window._ROUTER_DATA（分享页/首屏直连详情；www 页为 __ROUTER_DATA__）
// 当前详情页/弹层视频优先排最前；去重按 aweme_id（同 id 副本取数据更全者），上限 30 个作品。
(() => {
  ADL.adapters["douyin"] = {
    id: "douyin",
    lastSource: "hook",
    MAX_AWEMES: 30,

    extract(ctx) {
      const awemes = [];
      // aweme_id -> 下标；hook 副本可能被风控削字段（play_addr.uri 清空等），
      // 同一 aweme 后到且更完整的副本（通常来自 SSR）替换前者
      const idxOf = new Map();
      const score = (a) =>
        (ADL.awemePlayUrl(a) ? 1 : 0) +
        ((a.video && a.video.bit_rate && a.video.bit_rate.length) ? 1 : 0) +
        ((a.images || []).some((im) => im && ((im.url_list || []).length || im.url))
          ? 1
          : 0);
      const add = (a) => {
        if (!a || !a.aweme_id) return;
        const i = idxOf.get(a.aweme_id);
        if (i == null) {
          idxOf.set(a.aweme_id, awemes.length);
          awemes.push(a);
        } else if (score(a) > score(awemes[i])) {
          awemes[i] = a;
        }
      };
      const walk = (o) => {
        if (!o || typeof o !== "object") return;
        if (Array.isArray(o)) {
          for (const x of o) walk(x);
          return;
        }
        if (o.aweme_id && (o.video || o.images)) add(o);
        for (const k of Object.keys(o)) walk(o[k]);
      };
      for (const cap of ctx.captured("douyin")) walk(cap.data);

      const hookCount = awemes.length;
      // SSR 兜底：分享页 _ROUTER_DATA；www 页（双下划线 __ROUTER_DATA__）两套名字都试
      const router =
        ctx.inlineJson("_ROUTER_DATA") || ctx.inlineJson("__ROUTER_DATA__");
      if (router) {
        for (const a of ADL.douyinRouterAwemes(router)) add(a);
      }
      this.lastSource = hookCount ? "hook" : "ssr";
      if (!awemes.length) return [];

      // 当前详情页/弹层视频排最前（SPA 弹层 URL 用 modal_id，无独立路径）
      const m = /\/(video|note)\/(\d+)/.exec(ctx.url);
      const q = m ? null : /[?&]modal_id=(\d+)/.exec(ctx.url);
      const cur = m ? m[2] : q && q[1];
      if (cur) {
        awemes.sort((a, b) => (b.aweme_id === cur) - (a.aweme_id === cur));
      }
      return awemes
        .slice(0, this.MAX_AWEMES)
        .flatMap((a) => ADL.awemeToResources(a));
    },
  };
})();
