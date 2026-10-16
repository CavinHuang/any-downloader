// 抖音纯转换助手 —— content world 与 background（importScripts）共用。
// 提取路径（2025/2026 社区验证）：
//   响应/内嵌 JSON 里的 aweme 对象 → video.play_addr.uri
//   → 拼 https://www.iesdouyin.com/aweme/v1/play/?video_id={uri}（无水印直链，
//      每次请求 302 到新 CDN 地址，video_id 长期有效；需移动端 UA + Referer）
ADL.DOUYIN_UA =
  "Mozilla/5.0 (iPhone; CPU iPhone OS 17_2 like Mac OS X) AppleWebKit/605.1.15 " +
  "(KHTML, like Gecko) Version/17.2 Mobile/15E148 Safari/604.1";
ADL.douyinHeaders = () => ({
  "User-Agent": ADL.DOUYIN_UA,
  Referer: "https://www.douyin.com/",
});

// addr 对象（play_addr / download_addr / bit_rate[].play_addr）→ 可播直链。
// 兜底链（2026 真实 payload 常见残缺形态）：
//   1. url_list 里的 iesdouyin 直链（最稳）
//   2. uri 拼接 iesdouyin 稳定链；uri 偶发就是完整 URL → 直接用
//   3. uri 为空但 url_list 有 douyin.com/amemv 播放链 → 抽 video_id 参数重拼
// 全部落空返回 null（调用方决定是否降级 download_addr）。
ADL.douyinAddrUrl = (pa) => {
  if (!pa || typeof pa !== "object") return null;
  const list = Array.isArray(pa.url_list) ? pa.url_list : [];
  const fromList = list.find((u) => /iesdouyin\.com\/aweme\/v1\/play/i.test(u));
  if (fromList) return fromList;
  if (pa.uri) {
    if (/^https?:\/\//i.test(pa.uri)) return ADL.toHttps(pa.uri);
    return `https://www.iesdouyin.com/aweme/v1/play/?video_id=${encodeURIComponent(pa.uri)}`;
  }
  for (const u of list) {
    try {
      const vid = new URL(u).searchParams.get("video_id");
      if (vid)
        return `https://www.iesdouyin.com/aweme/v1/play/?video_id=${encodeURIComponent(vid)}`;
    } catch (_) {}
  }
  return null;
};

// aweme → 无水印播放地址（优先接口自带的 iesdouyin 直链，缺则按 uri 拼接）
ADL.awemePlayUrl = (a) => ADL.douyinAddrUrl(a && a.video && a.video.play_addr);

// aweme 对象 → MediaResource[]（视频 + 图集 + 清晰度变体）
// 文件名带作者昵称前缀；主资源 = 最高清晰度，其余清晰度标记 optional
// （float 面板默认不勾选，用户可展开选择；popup 一键推送只推非 optional）。
ADL.awemeToResources = (a) => {
  const out = [];
  if (!a || !a.aweme_id) return out;
  const h = ADL.douyinHeaders();
  const author = a.author && a.author.nickname ? ADL.cleanName(a.author.nickname, 20) : "";
  const title =
    ADL.cleanName(a.desc || a.preview_title || "", 60) || `douyin_${a.aweme_id}`;
  const name = author ? `${author}_${title}` : title;
  const seen = new Set();
  const seenVid = new Set(); // play 接口按 video_id 去重（主资源与最高码率常同 uri 异参）
  const vidOf = (u) => {
    try {
      return new URL(u).searchParams.get("video_id");
    } catch (_) {
      return null;
    }
  };
  const playOf = ADL.douyinAddrUrl;
  // 画质标签：gear_name > gear > 分辨率短边（竖屏 720x1280 → "720p"）
  const gearLabel = (r) => {
    if (!r) return undefined;
    if (r.gear_name) return r.gear_name;
    if (r.gear) return r.gear;
    const w = Number(r.play_addr && r.play_addr.width) || 0;
    const h = Number(r.play_addr && r.play_addr.height) || 0;
    return w && h ? `${Math.min(w, h)}p` : undefined;
  };
  const add = (url, kind, filename, opt, quality) => {
    const u = ADL.toHttps(url);
    if (!u || seen.has(u)) return;
    if (kind === "video" || kind === "livephoto") {
      const vid = vidOf(u);
      if (vid) {
        if (seenVid.has(vid)) return;
        seenVid.add(vid);
      }
    }
    seen.add(u);
    out.push({
      url: u,
      kind,
      filename: `${filename}${kind === "image" ? ".jpeg" : ".mp4"}`,
      headers: h,
      optional: !!opt,
      meta: {
        title: a.desc || "",
        quality,
        extra: { aweme_id: a.aweme_id },
      },
    });
  };
  // 主资源：play_addr 优先（无水印）；全残缺时降级 download_addr（带水印但可播）
  const play =
    playOf(a.video && a.video.play_addr) ||
    playOf(a.video && a.video.download_addr);
  if (play) add(play, "video", name, false);
  // 清晰度变体（bit_rate 按码率降序；与主资源重复的 URL 自动去重）
  const rates = (a.video && a.video.bit_rate) || [];
  rates
    .slice()
    .sort((x, y) => (y.bitrate || 0) - (x.bitrate || 0))
    .forEach((r) => {
      const u = playOf(r.play_addr);
      if (u) add(u, "video", name, true, gearLabel(r));
    });
  const imgs = a.images || []; // 图集（无 video.play_addr，仅 images）
  imgs.forEach((im, i) => {
    const list = (im && im.url_list) || [];
    const u = list[list.length - 1] || (im && im.url);
    if (u) add(u, "image", `${name}_${String(i + 1).padStart(2, "0")}`, false);
    // 图集实况（livephoto）：每张图可携带一段 3s 短视频（images[].video.play_addr）
    const lp = playOf(im && im.video && im.video.play_addr);
    if (lp)
      add(lp, "livephoto", `${name}_live_${String(i + 1).padStart(2, "0")}`, false);
  });
  return out;
};

// 从分享口令/文本中提取 URL（"8.88 复制此链接，打开抖音… https://v.douyin.com/xxx …"）。
// 截断集补全角标点；命中后再去掉尾部悬挂标点（"…/xxx。" / "…/xxx）"→ 不带标点）。
ADL.extractLink = (text) => {
  const m = /https?:\/\/[^\s"'<>，。；」』）]+/i.exec(String(text || ""));
  if (!m) return null;
  return m[0].replace(/[.,;:!?)\]】〉」』、。；：？！，…]+$/, "");
};

// xhs stream 多档位 → 第一个非空数组（h264 > h265 > av1；空数组与缺失都跳过，
// camel/snake 两个转换器与 xhs.js 多档位展开共用，勿各自实现 || 链——
// `h264 || h265` 对空数组（truthy）不回退，会漏采仅有 h265 流的视频）。
ADL.xhsStreamArr = (stream) => {
  for (const k of ["h264", "h265", "av1"]) {
    const a = stream && stream[k];
    if (Array.isArray(a) && a.length) return a;
  }
  return [];
};

// xhs master_url → 直链：仅 https%3A 双重编码形态才整体解码。普通 URL 含合法
// %xx（如预签名 ?sign=ab%2Bcd%3D）时无条件解码会破坏签名 → CDN 403。
// camel/snake 转换器与 xhs.js hook 路径共用本实现。
ADL.xhsStreamUrl = (u) => {
  let s = String(u || "");
  if (/^https?%3A/i.test(s)) {
    try {
      s = decodeURIComponent(s);
    } catch (_) {}
  }
  return ADL.toHttps(s);
};

// xhs __INITIAL_STATE__（对象）→ MediaResource[]。
// 供页面适配器（SSR 数据）与 background 分享页解析共用；API 响应（note_card，
// snake_case）走适配器内的独立解析后与其合并。
ADL.xhsResourcesFromState = (st) => {
  const out = [];
  if (!st || !st.note) return out;
  const note = st.note;
  const map = note.noteDetailMap || {};
  const cur = note.currentNoteId || note.firstNoteId || Object.keys(map)[0];
  const entry = (cur && map[cur]) || Object.values(map)[0];
  const n = entry && (entry.note || entry);
  if (!n) return out;
  return ADL.xhsNoteToResources(n, cur);
};

// xhs 笔记对象（camelCase，SSR 形态）→ MediaResource[]
ADL.xhsNoteToResources = (n, noteId) => {  const out = [];
  if (!n) return out;
  const title = ADL.cleanName(n.title || n.desc || "", 50) || `xhs_${noteId || "note"}`;
  const headers = { Referer: "https://www.xiaohongshu.com/" };
  for (const [i, im] of (n.imageList || []).entries()) {
    let u =
      im.urlDefault ||
      (im.infoList && im.infoList.length && im.infoList[im.infoList.length - 1].url) ||
      "";
    if (!u) continue;
    u = ADL.stripWatermark(ADL.toHttps(u));
    out.push({
      url: u,
      kind: im.livePhoto ? "livephoto" : "image",
      filename: `${title}_${String(i + 1).padStart(2, "0")}.jpg`,
      headers,
      meta: { extra: { noteId } },
    });
  }
  const stream = n.video && n.video.media && n.video.media.stream;
  if (stream) {
    const pick = ADL.xhsStreamArr(stream)[0];
    if (pick && pick.masterUrl) {
      out.push({
        url: ADL.xhsStreamUrl(pick.masterUrl),
        kind: "video",
        filename: `${title}.mp4`,
        headers,
        meta: {
          title: n.title || n.desc || "",
          quality: pick.masterHeight ? `${pick.masterHeight}p` : undefined,
          extra: { noteId },
        },
      });
    }
  }
  return out;
};

// xhs API note_card（snake_case，/api/sns/web/v1/feed 形态）→ MediaResource[]
ADL.xhsNoteCardToResources = (card, noteId) => {
  const out = [];
  if (!card) return out;
  const title = ADL.cleanName(card.title || card.desc || "", 50) || `xhs_${noteId || "note"}`;
  const headers = { Referer: "https://www.xiaohongshu.com/" };
  for (const [i, im] of (card.image_list || []).entries()) {
    let u =
      im.url_default ||
      (im.info_list && im.info_list.length && im.info_list[im.info_list.length - 1].url) ||
      "";
    if (!u) continue;
    u = ADL.stripWatermark(ADL.toHttps(u));
    out.push({
      url: u,
      kind: im.live_photo ? "livephoto" : "image",
      filename: `${title}_${String(i + 1).padStart(2, "0")}.jpg`,
      headers,
      meta: { extra: { noteId } },
    });
  }
  const stream = card.video && card.video.media && card.video.media.stream;
  if (stream) {
    const pick = ADL.xhsStreamArr(stream)[0];
    if (pick && pick.master_url) {
      out.push({
        url: ADL.xhsStreamUrl(pick.master_url),
        kind: "video",
        filename: `${title}.mp4`,
        headers,
        meta: {
          title: card.title || card.desc || "",
          quality: pick.master_height ? `${pick.master_height}p` : undefined,
          extra: { noteId },
        },
      });
    }
  }
  return out;
};

// 分享页 HTML → xhs 笔记资源（background resolveShareLink 用，无 DOM）。
// 分享页 SSR 同样内嵌 __INITIAL_STATE__。
ADL.xhsShareHtmlResources = (html) => {
  const m = /window\.__INITIAL_STATE__\s*=/.exec(html || "");
  if (!m) return [];
  const start = html.indexOf("{", m.index);
  if (start < 0) return [];
  const json = ADL._balanced(html, start);
  if (json == null) return [];
  const st = ADL.parseLooseJson(json);
  return st ? ADL.xhsResourcesFromState(st) : [];
};

// _ROUTER_DATA JSON → aweme 对象数组（分享页/SSR 首屏）
// loaderData 键形态：video_(id)/page → videoInfoRes.item_list；
// note_(id)/page（图集分享页）→ noteInfoRes.item_list；兜底任意 *InfoRes.item_list。
ADL.douyinRouterAwemes = (router) => {
  const out = [];
  const ld = (router && router.loaderData) || {};
  for (const k of Object.keys(ld)) {
    const branch = ld[k] || {};
    let il =
      (branch.videoInfoRes && branch.videoInfoRes.item_list) ||
      (branch.noteInfoRes && branch.noteInfoRes.item_list) ||
      branch.item_list ||
      [];
    if (!il.length) {
      for (const v of Object.values(branch)) {
        if (v && typeof v === "object" && Array.isArray(v.item_list)) {
          il = v.item_list;
          break;
        }
      }
    }
    for (const a of il) {
      if (a && a.aweme_id) out.push(a);
    }
  }
  return out;
};

// 分享页 HTML → aweme 对象数组（background resolveShareLink 用，无 DOM）。
// 兼容 window./self. 与 _ROUTER_DATA/__ROUTER_DATA__（www 页用双下划线形态）。
ADL.douyinShareHtmlAwemes = (html) => {
  const m =
    /(?:window|self)\._ROUTER_DATA\s*=/.exec(html || "") ||
    /(?:window|self)\.__ROUTER_DATA__\s*=/.exec(html || "");
  if (!m) return [];
  const start = html.indexOf("{", m.index);
  if (start < 0) return [];
  const json = ADL._balanced(html, start);
  if (json == null) return [];
  const router = ADL.parseLooseJson(json);
  return router ? ADL.douyinRouterAwemes(router) : [];
};

// ---------------------------------------------------------------- X 分享链接
// 粘贴解析走 syndication 端点（cdn.syndication.twimg.com/tweet-result，无需登录，
// 社区与 yt-dlp 同款方案）。token 为推文 id 的确定性算式，逐字移植 yt-dlp
// _generate_syndication_token（与 X 官方 embed JS 同源）：
//   ((id/1e15)*π).toString(36) 去 0 与 .
// 注意 Number(19 位 id) 会丢低位精度，但 X 自己的 embed JS 同样跑在 JS double 上，
// 同一算式得到同一 token——不要"修复"成 BigInt。
ADL.xSyndicationToken = (id) =>
  ((Number(id) / 1e15) * Math.PI).toString(36).replace(/(0+|\.)/g, "");

// 分享文本/链接 → status id。接受 x.com / twitter.com（含 mobile）/ fxtwitter /
// vxtwitter / fixupx，路径 /status/、/statuses/、/i/web/status/；非推文链接 null。
// 前向锚点防止子串误判（netflix.com 含 "x.com"）。
ADL.xStatusId = (link) => {
  const m =
    /(?:^|\/\/|\.)(?:mobile\.)?(?:x|twitter|fxtwitter|vxtwitter|fixupx)\.com\/(?:i\/web\/)?(?:[^/?]+\/)?status(?:es)?\/(\d+)/i.exec(
      String(link || "")
    );
  return m && m[1];
};

// syndication tweet-result JSON → MediaResource[]（与 x.js 页面路径同一命名/码率约定）。
// 响应无 extended_entities，媒体在顶层 mediaDetails[]（yt-dlp 同样重组该字段）：
//   photo → media_url_https 升级 ?format=<ext>&name=orig；
//   video/animated_gif → video_info.variants 取 mp4 按码率降序（m3u8/webm 不收），
//   最高档为主资源，低码率 optional。
ADL.xSyndicationResources = (d, statusId) => {
  const out = [];
  if (!d || !Array.isArray(d.mediaDetails)) return out;
  const who = (d.user && (d.user.screen_name || d.user.name)) || statusId || "tw";
  const headers = { Referer: "https://x.com/" };
  const seen = new Set();
  for (const md of d.mediaDetails) {
    if (!md) continue;
    if (md.type === "photo" && md.media_url_https) {
      const ext =
        (/\.(jpe?g|png|gif|webp)$/i.exec(md.media_url_https) || [])[1] || "jpg";
      const url = `${md.media_url_https}?format=${ext}&name=orig`;
      const key = url.split("?")[0];
      if (seen.has(key)) continue;
      seen.add(key);
      out.push({
        url,
        kind: "image",
        filename: `x_${who}_${statusId || "tw"}_${String(out.length + 1).padStart(2, "0")}.${ext}`,
        headers,
      });
    } else if (md.video_info && md.type !== "photo") {
      const mp4 = (md.video_info.variants || [])
        .filter((v) => v && v.content_type === "video/mp4" && v.url)
        .sort((a, b) => (b.bitrate || 0) - (a.bitrate || 0));
      mp4.forEach((v, vi) => {
        if (seen.has(v.url)) return;
        seen.add(v.url);
        out.push({
          url: v.url,
          kind: "video",
          filename: `x_${who}${statusId ? `_${statusId}` : ""}${vi ? `_${v.bitrate || ""}` : ""}.mp4`,
          headers,
          optional: vi > 0,
          meta: {
            quality: v.bitrate ? `${Math.round(v.bitrate / 1000)}kbps` : undefined,
          },
        });
      });
    }
  }
  return out;
};
