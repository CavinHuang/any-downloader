// 适配器：微信视频号（channels.weixin.qq.com）
// 数据源：hook —— 捕获 finder 接口/媒体响应。
// 关键：视频号的视频文件是**加密存储**的——Isaac64 密钥流 XOR 文件前 128KB，
// seed 就是接口响应里的 decode_key（十进制数字符串）。引擎侧 postprocess
// { algo: "isaac64-xor", seed } 完成下载后解密（算法已与官方样本对拍锁定）。
// 响应 schema 平台随时会变 → 深度容错遍历：任何 decode*key 字段配对最近的
// finder.video.qq.com / .mp4 媒体地址；标题取沿途最近的 title/desc。
(() => {
  ADL.adapters["channels"] = {
    id: "channels",
    lastSource: "hook",
    MAX_ITEMS: 20,

    extract(ctx) {
      const out = [];
      const seen = new Set();
      // 每个捕获响应内独立配对：key 取响应内最近出现的 decode_key
      for (const cap of ctx.captured("channels")) {
        let globalKey = null;
        let nearTitle = "视频号视频";
        const stack = []; // 沿途记录 (key, value) 上下文
        const startIdx = out.length; // 本响应新增下界：兜底重试不得跨响应串配 key
        const isMediaUrl = (u) =>
          typeof u === "string" &&
          /finder\.video\.qq\.com|\.mp4(?=[?#]|$)/i.test(u) &&
          !/\.(jpe?g|png|gif|webp)(?=[?#]|$)/i.test(u); // 封面图不入列
        const walk = (o) => {
          if (o == null || typeof o !== "object") return;
          if (Array.isArray(o)) {
            for (const x of o) walk(x);
            return;
          }
          // 先收集本对象上下文（title/key），再处理媒体 URL —— 平台 payload 里
          // URL 常先于 title 出现，顺序处理会错配
          let titleHere = null;
          let keyHere = null;
          const urls = [];
          for (const [k, v] of Object.entries(o)) {
            if (typeof v === "string") {
              if (/^(title|desc|description|nickname)$/.test(k) && v.trim()) {
                titleHere = v;
              } else if (/decode[_-]?key|seed[_-]?key|encrypt[_-]?key/i.test(k) && /^\d{6,20}$/.test(v)) {
                keyHere = v;
              } else if (isMediaUrl(v)) {
                urls.push(v);
              }
            }
          }
          if (titleHere) nearTitle = titleHere;
          if (keyHere && !globalKey) globalKey = keyHere;
          for (const u of urls) {
            this.push(out, seen, u, keyHere || globalKey, titleHere || nearTitle);
          }
          for (const v of Object.values(o)) {
            if (v && typeof v === "object") walk(v);
          }
        };
        walk(cap.data);
        // 首扫中 URL 先于 key 出现的兜底：用响应内 globalKey 重试**本响应**未配对的
        // （decode_key 按视频独立，跨响应串配会用错 seed → 解密出乱码）
        if (globalKey) {
          for (const r of out.slice(startIdx)) {
            if (r.url && !r.meta.decrypt) {
              r.meta.decrypt = { algo: "isaac64-xor", seed: globalKey };
            }
          }
        }
      }
      return out.slice(0, this.MAX_ITEMS);
    },

    push(out, seen, url, key, title) {
      const u = ADL.toHttps(url);
      if (seen.has(u) || out.length >= this.MAX_ITEMS) return;
      seen.add(u);
      out.push({
        url: u,
        kind: "video",
        filename: `${ADL.cleanName(title, 60) || "视频号"}.mp4`,
        headers: { Referer: "https://channels.weixin.qq.com/" },
        meta: key
          ? { title, decrypt: { algo: "isaac64-xor", seed: key } }
          : { title },
      });
    },
  };
})();
