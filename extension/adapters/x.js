// 适配器：X (Twitter)（x.com / twitter.com）
// 数据源（三级降级）：
//   1. hook —— L1 捕获的 GraphQL 响应：extended_entities.media[]，
//      视频 variants[] 里就是各码率直链 mp4（零解签），照片 media_url_https
//   2. dom —— 时间线里的 pbs.twimg.com 图片 → name=orig 原图
// 当前推文页（/status/<id>）的媒体优先。
(() => {
  ADL.adapters["x"] = {
    id: "x",
    lastSource: "hook",
    headers: { Referer: "https://x.com/" },

    extract(ctx) {
      // 深度遍历 GraphQL 响应，收集 media；沿途记住最近的 user / tweet id。
      // walk 返回更新后的上下文——子树发现的信息要能传给后续兄弟子树。
      const found = []; // { statusId, user, media }
      const walk = (o, c) => {
        if (!o || typeof o !== "object") return c;
        if (Array.isArray(o)) {
          let cur = c;
          for (const x of o) cur = walk(x, cur);
          return cur;
        }
        let cur = c;
        if (o.screen_name) cur = { ...cur, user: o.screen_name };
        if (
          (o.id_str || o.rest_id) &&
          (o.full_text || o.text || o.entities || o.extended_entities || o.legacy)
        ) {
          cur = { ...cur, statusId: o.id_str || o.rest_id };
        }
        const ee = o.extended_entities || o.entities;
        if (ee && Array.isArray(ee.media) && ee.media.length) {
          found.push({ statusId: cur.statusId, user: cur.user, media: ee.media });
        }
        for (const k of Object.keys(o)) cur = walk(o[k], cur);
        return cur;
      };
      for (const cap of ctx.captured("x")) walk(cap.data, {});

      const m = /\/status\/(\d+)/.exec(ctx.url);
      const curId = m && m[1];
      // 推文详情页只收当前推文；时间线（无 status 上下文）收全部
      const scoped = curId
        ? found.filter((f) => f.statusId === curId)
        : found;
      const list = scoped.length ? scoped : found;

      const out = [];
      const seen = new Set();
      for (const f of list) {
        for (const md of f.media) {
          if (md.type === "photo" && md.media_url_https) {
            const ext =
              (/\.(jpe?g|png|gif|webp)$/i.exec(md.media_url_https) || [])[1] || "jpg";
            const url = `${md.media_url_https}?format=${ext}&name=orig`; // 原图
            const key = url.split("?")[0];
            if (seen.has(key)) continue;
            seen.add(key);
            out.push({
              url,
              kind: "image",
              filename: `x_${f.statusId || f.user || "tw"}_${String(out.length + 1).padStart(2, "0")}.${ext}`,
              headers: this.headers,
            });
          } else if (md.video_info && md.type !== "photo") {
            const mp4 = (md.video_info.variants || [])
              .filter((v) => v.content_type === "video/mp4")
              .sort((a, b) => (b.bitrate || 0) - (a.bitrate || 0));
            const who = f.user || f.statusId || "tw";
            const sid = f.statusId || "";
            mp4.forEach((v, vi) => {
              if (!v.url || seen.has(v.url)) return;
              seen.add(v.url);
              out.push({
                url: v.url,
                kind: md.type === "animated_gif" ? "video" : "video",
                filename: `x_${who}${sid ? `_${sid}` : ""}${vi ? `_${v.bitrate || ""}` : ""}.mp4`,
                headers: this.headers,
                optional: vi > 0, // 默认只推最高码率，低码率可选
                meta: {
                  quality: v.bitrate ? `${Math.round(v.bitrate / 1000)}kbps` : undefined,
                },
              });
            });
          }
        }
      }
      this.lastSource = out.length ? "hook" : "dom";

      // DOM 兜底：页面已渲染的推文图片 → 原图参数
      if (!out.length || !curId) {
        for (const im of ctx.dom().images) {
          const s = im.src || "";
          if (!/pbs\.twimg\.com\/media\//.test(s)) continue;
          let url = /[?&]name=/.test(s)
            ? s.replace(/([?&])name=[^&]*/, "$1name=orig")
            : `${s}${s.includes("?") ? "&" : "?"}name=orig`;
          url = url.replace(/:(small|medium|large)$/, "");
          const key = url.split(/[?&]format=/)[0];
          if (seen.has(key)) continue;
          seen.add(key);
          const ext = (/\.(jpe?g|png|gif|webp)$/i.exec(s) || [])[1] || "jpg";
          out.push({
            url,
            kind: "image",
            filename: `x_img_${String(out.length + 1).padStart(2, "0")}.${ext}`,
            headers: this.headers,
          });
        }
      }
      return out.slice(0, 60);
    },
  };
})();
