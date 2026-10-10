// 适配器：小红书笔记（www.xiaohongshu.com）
// 数据源（三级降级/合并）：
//   1. hook —— L1 捕获的 /api/sns/web/v1/(feed|note) 响应（SPA 站内导航时的新鲜数据，
//      snake_case note_card 形态；SSR 的 __INITIAL_STATE__ 只反映首屏）
//   2. ssr —— window.__INITIAL_STATE__（首屏直连详情页）
// 图片：core.js 转换器去 !nd_dft_... 水印后缀 = 无水印原图。
// hook 侧补强（本文件）：
//   a. 视频多档高度 —— 转换器取首个非空档位数组的 [0]（依赖平台排序）；这里改为按
//      master_height 取最高档为主资源（顺序无关），其余档位降为 optional 变体；
//   b. livephoto —— 转换器只出静帧；image_list[i].live_photo_url 的视频流在这里
//      补成非可选 video 资源（否则动图只下到图、动图视频丢失）；
//   c. master_url 仅在形如 https%3A.. 时解码（URL 本身含 %xx 时无条件解码会破坏
//      签名）—— 该规则已下沉为共享 ADL.xhsStreamUrl（core.js，camel/snake 转换器
//      同一实现）。
// 合并去重两级：URL 精确去重 + 同笔记内非可选资源按 noteId|filename 去重 ——
// hook 与 SSR 对同一笔记可能给出不同签名/尺寸的等价直链，只保留 hook（更新鲜）。
// 同一笔记会在 /feed 与 /note 两次捕获里重复出现（视频签名 URL 还不同），按 id
// 去重保留最新一次，避免重复资源挤占 MAX_NOTES 名额。
// CDN 下载需 Referer（Cookie 非必须：地址为预签名）。
(() => {
  // master_url → 直链：解码规则收敛到共享 ADL.xhsStreamUrl（仅 https%3A 双重
  // 编码形态才解码，普通 URL 保留合法 %xx 签名；与 core.js 两转换器同一实现）
  const streamUrl = (s) => (s && s.master_url ? ADL.xhsStreamUrl(s.master_url) : null);

  // hook 卡片 → 转换器资源 + hook 侧补强（livephoto 视频 / 视频多档位展开）
  const cardResources = (card, id) => {
    const out = ADL.xhsNoteCardToResources(card, id);
    if (!card) return out;
    // 与 core.js 转换器同名规则保持一致（仅用于派生 live 视频文件名）
    const title =
      ADL.cleanName(card.title || card.desc || "", 50) || `xhs_${id || "note"}`;
    const headers = { Referer: "https://www.xiaohongshu.com/" };

    // livephoto：静帧之外补 live_photo_url 视频流（非可选，默认推送包含）
    (card.image_list || []).forEach((im, i) => {
      if (!im || !im.live_photo) return;
      const u = im.live_photo_url || im.livePhotoUrl;
      if (!u) return;
      out.push({
        url: ADL.toHttps(String(u)),
        kind: "video",
        filename: `${title}_live_${String(i + 1).padStart(2, "0")}.mp4`,
        headers,
        optional: false,
        meta: {
          title: card.title || card.desc || "",
          extra: { noteId: id, livePhoto: true },
        },
      });
    });

    // 视频多档位：主资源换成 master_height 最高档（稳定排序 → 高度缺失时保持
    // 转换器的 [0] 选择），其余高度追加为 optional 变体（同 douyin/X 变体模式）
    const vi = out.findIndex((r) => r.kind === "video" && !r.optional);
    const stream = card.video && card.video.media && card.video.media.stream;
    if (vi >= 0 && stream) {
      const arr = ADL.xhsStreamArr(stream)
        .slice()
        .sort((a, b) => (b.master_height || 0) - (a.master_height || 0));
      const topUrl = streamUrl(arr[0]);
      if (topUrl) {
        if (topUrl !== out[vi].url) {
          out[vi] = {
            ...out[vi],
            url: topUrl,
            meta: {
              ...out[vi].meta,
              quality: arr[0].master_height
                ? `${arr[0].master_height}p`
                : out[vi].meta.quality,
            },
          };
        }
        for (const s of arr.slice(1)) {
          const u = streamUrl(s);
          if (!u) continue;
          out.push({
            url: u,
            kind: "video",
            filename: out[vi].filename,
            headers: out[vi].headers,
            optional: true,
            meta: {
              title: out[vi].meta.title,
              quality: s.master_height ? `${s.master_height}p` : undefined,
              extra: { noteId: id },
            },
          });
        }
      }
    }
    return out;
  };

  ADL.adapters["xhs"] = {
    id: "xhs",
    lastSource: "ssr",
    MAX_NOTES: 12,

    extract(ctx) {
      // 每次探测重置数据源标记：两级全空时不得残留上一次 probe 的 "hook"
      this.lastSource = "ssr";
      const out = [];
      const seenUrl = new Set();
      const seenKey = new Set(); // 非可选资源 noteId|filename：跨数据源等价直链去重
      const keyOf = (r) =>
        !r.optional && r.meta && r.meta.extra && r.meta.extra.noteId != null
          ? `${r.meta.extra.noteId}|${r.filename}`
          : null;
      const addAll = (rs) => {
        for (const r of rs) {
          if (seenUrl.has(r.url)) continue;
          const k = keyOf(r);
          if (k && seenKey.has(k)) continue;
          seenUrl.add(r.url);
          if (k) seenKey.add(k);
          out.push(r);
        }
      };

      // 1) hook：API 响应里的 note_card（站内导航的新鲜数据，优先）
      const cards = []; // { id, card }
      const walk = (o) => {
        if (!o || typeof o !== "object") return;
        if (Array.isArray(o)) {
          for (const x of o) walk(x);
          return;
        }
        if (o.note_card) cards.push({ id: o.id, card: o.note_card });
        for (const k of Object.keys(o)) walk(o[k]);
      };
      for (const cap of ctx.captured("xhs") || []) walk(cap.data);

      // 同笔记多捕获按 id 去重：原位替换（位置稳定）+ 内容取最新一次。
      // byId 记 uniq 下标（而非对象引用）——原位替换后引用会过期，
      // 用旧引用 indexOf 得 -1 会把第 3 次及以后的捕获静默丢掉。
      const byId = new Map(); // id -> uniq 下标
      const uniq = [];
      for (const c of cards) {
        if (c.id == null) {
          uniq.push(c);
          continue;
        }
        const i = byId.get(c.id);
        if (i == null) {
          byId.set(c.id, uniq.length);
          uniq.push(c);
        } else {
          uniq[i] = c;
        }
      }

      // 当前笔记 id：详情页形态 /explore/{nid}、/discovery/item/{nid}；
      // 主页笔记弹层 /user/profile/{uid}/{nid} —— 笔记 id 是第二段 24 位 hex
      // （第一段是用户 id，取错会把当前笔记排序完全废掉）
      const m =
        /\/(?:explore|discovery\/item)\/([0-9a-f]{24})/i.exec(ctx.url) ||
        /\/user\/profile\/[0-9a-f]{24}\/([0-9a-f]{24})/i.exec(ctx.url);
      const curId = m && m[1];
      if (uniq.length) {
        if (curId) uniq.sort((a, b) => (b.id === curId) - (a.id === curId));
        addAll(
          uniq.slice(0, this.MAX_NOTES).flatMap((c) => cardResources(c.card, c.id))
        );
        this.lastSource = "hook";
      }

      // 2) ssr：首屏直连的兜底（合并去重；同笔记的等价直链让位 hook）
      const st = ctx.inlineJson("__INITIAL_STATE__");
      if (st) {
        addAll(ADL.xhsResourcesFromState(st));
        if (!uniq.length) this.lastSource = "ssr";
      }
      return out;
    },
  };
})();
