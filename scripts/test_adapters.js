// 站点适配器单元测试（node scripts/test_adapters.js）
// 适配器 = 纯提取函数 → 用脱敏 fixture 驱动；平台改版回归 = 重新录制 fixture。
"use strict";
const path = require("path");
const fs = require("fs");

globalThis.self = globalThis; // 适配器挂在 self.ADL 命名空间

const A = "extension/adapters";
require(path.join("..", A, "types.js"));
require(path.join("..", A, "core.js"));
require(path.join("..", A, "wechat-article.js"));
require(path.join("..", A, "xhs.js"));
require(path.join("..", A, "douyin.js"));
require(path.join("..", A, "x.js"));

const F = (f) => fs.readFileSync(path.join(A, "fixtures", f), "utf-8");

let pass = 0, fail = 0;
function ok(cond, label, extra) {
  if (cond) { pass++; console.log("  ok  " + label); }
  else { fail++; console.log("  FAIL " + label + (extra !== undefined ? "  => " + JSON.stringify(extra) : "")); }
}

// ---------------------------------------------------------------- 抖音
console.log("douyin:");
{
  const feed = JSON.parse(F("douyin_feed.json"));
  const router = JSON.parse(F("douyin_router.json"));
  const ctx = {
    url: "https://www.douyin.com/video/7301234567890123456",
    captured: (k) => (k === "douyin" ? [{ url: "x", data: feed }] : []),
    inlineJson: () => router,
    dom: () => ({ images: [] }),
  };
  const rs = ADL.adapters.douyin.extract(ctx);
  ok(rs.length === 4, "4 项（1 视频 + 3 图集：feed 2 + 分享页 note 1，aweme 去重）", rs.length);
  ok(rs[0].kind === "video" && rs[0].url.startsWith("https://www.iesdouyin.com/aweme/v1/play/?video_id=v0d00fg10000abcdef1234567890"),
    "当前详情页视频排最前且为 iesdouyin 无水印直链", rs[0].url);
  ok(rs[0].headers["User-Agent"].includes("iPhone") && rs[0].headers.Referer === "https://www.douyin.com/",
    "视频请求带移动端 UA + Referer", rs[0].headers);
  ok(rs[0].filename === "测试视频：海边日落 延时摄影.mp4",
    "文件名 = 标题清洗（/ 等非法字符转空格）+ .mp4", rs[0].filename);
  const imgs = rs.filter((r) => r.kind === "image");
  ok(imgs.length === 3 && imgs.every((r) => r.url.endsWith("_full.jpeg")),
    "图集取 url_list 最后一项（原图）", imgs.map((r) => r.url));
  ok(imgs[0].headers["User-Agent"].includes("iPhone"), "图集同样带移动端 UA");

  // SPA 弹层：modal_id URL 也把当前作品排最前
  const rsModal = ADL.adapters.douyin.extract({ ...ctx, url: "https://www.douyin.com/?modal_id=7399876543210987654" });
  ok(rsModal.length === 4 && rsModal[0].meta.extra.aweme_id === "7399876543210987654",
    "modal_id 弹层作品排最前", rsModal[0] && rsModal[0].meta.extra);

  // 同一 aweme 双副本：hook 残缺（uri 被清空）+ SSR 完整 → 取完整者
  const dead = JSON.parse(JSON.stringify(
    router.loaderData["video_(7301234567890123456)/page"].videoInfoRes.item_list[0]
  ));
  dead.video.play_addr.uri = "";
  const rsDead = ADL.adapters.douyin.extract({
    ...ctx,
    captured: (k) => (k === "douyin" ? [{ url: "x", data: { aweme_list: [dead] } }] : []),
  });
  ok(rsDead.some((r) => r.kind === "video" && r.url.includes("video_id=v0d00fg10000abcdef1234567890")),
    "hook 残缺副本被更完整的 SSR 副本替换", rsDead.map((r) => r.url));

  // 仅 SSR：source 标记为 ssr（popup 数据新鲜度提示）
  const rsSsr = ADL.adapters.douyin.extract({ ...ctx, captured: () => [] });
  ok(rsSsr.length === 2 && ADL.adapters.douyin.lastSource === "ssr",
    "仅 SSR 时 lastSource = ssr 且产出 2 项", [rsSsr.length, ADL.adapters.douyin.lastSource]);

  // 分享页 HTML 解析（background resolveShareLink 路径）
  const routerText = JSON.stringify(router);
  const html = `<!DOCTYPE html><script>window._ROUTER_DATA=${routerText};</script>`;
  const awemes = ADL.douyinShareHtmlAwemes(html);
  ok(awemes.length === 2 && awemes.some((a) => a.aweme_id === "7400000000000000099"),
    "分享页 HTML → videoInfoRes + noteInfoRes（图集页）均解析", awemes.map((a) => a.aweme_id));
  const html2 = `<html><script>self.__ROUTER_DATA__=${routerText};</script></html>`;
  ok(ADL.douyinShareHtmlAwemes(html2).length === 2, "双下划线 __ROUTER_DATA__ 同样解析");
  ok(ADL.douyinShareHtmlAwemes("<html>no data</html>").length === 0, "无数据 HTML 安全返回空");

  // 残缺 payload 边角（douyin_edge.json）
  const edge = JSON.parse(F("douyin_edge.json"));
  const rse = ADL.adapters.douyin.extract({
    url: "https://www.douyin.com/",
    captured: (k) => (k === "douyin" ? [{ url: "e", data: edge }] : []),
    inlineJson: () => null,
    dom: () => ({ images: [] }),
  });
  ok(rse.length === 8, "边角 fixture：5 aweme → 8 资源（空媒体跳过）", rse.length);
  const d1 = rse.find((r) => r.meta.extra.aweme_id === "7400000000000000001");
  ok(d1 && d1.url.includes("video_id=v0d00dl0001"),
    "play_addr 全空 → download_addr 兜底", d1 && d1.url);
  const d2 = rse.find((r) => r.meta.extra.aweme_id === "7400000000000000002");
  ok(d2 && d2.url === "https://www.iesdouyin.com/aweme/v1/play/?video_id=v0d00vid0002",
    "uri 空 → 从 url_list 抽 video_id 重拼 iesdouyin", d2 && d2.url);
  const v3 = rse.filter((r) => r.meta.extra.aweme_id === "7400000000000000003");
  ok(v3.length === 3 && !v3[0].optional && v3[0].meta.quality === undefined,
    "主资源在前非可选（无 gear_name 不影响主资源）", v3.map((r) => r.meta.quality));
  ok(v3[1].optional && v3[1].meta.quality === "1080p" && v3[2].meta.quality === "low_adapt",
    "变体画质标签：gear_name > gear > 分辨率短边", v3.map((r) => r.meta.quality));
  const d4 = rse.filter((r) => r.meta.extra.aweme_id === "7400000000000000004");
  ok(d4.length === 3, "图集：空 url_list 项跳过（3 项而非 4）", d4.length);
  const lp = d4.find((r) => r.kind === "livephoto");
  ok(lp && lp.url.includes("video_id=v0d00lp2") && lp.filename.endsWith("_live_02.mp4"),
    "图集 livephoto 短视频提取（.mp4）", lp && lp.filename);
  ok(!rse.some((r) => r.meta.extra.aweme_id === "7400000000000000005"),
    "全空 aweme 产出 0 资源且不影响其余", null);
}

// ---------------------------------------------------------------- 小红书
console.log("xhs:");
{
  const stateText = F("xhs_state.txt");
  const m = /window\.__INITIAL_STATE__=/.exec(stateText);
  const json = ADL._balanced(stateText, stateText.indexOf("{", m.index));
  const st = ADL.parseLooseJson(json); // 含 undefined 字面量 → 宽松解析
  ok(st && st.note && st.note.noteDetailMap, "宽松解析成功（undefined → null）");
  const ctx = {
    url: "https://www.xiaohongshu.com/explore/6540abc123def4567890abcd",
    captured: () => [],
    inlineJson: () => st,
    dom: () => ({ images: [] }),
  };
  const rs = ADL.adapters.xhs.extract(ctx);
  ok(rs.length === 3, "3 项（2 图 + 1 视频）", rs.length);
  ok(rs[0].url.startsWith("https://sns-webpic-qc.xhscdn.com/20250101/aaa/nd/bbb") && !rs[0].url.includes("!"),
    "图片 https + 无水印后缀", rs[0].url);
  ok(rs[1].kind === "livephoto", "livePhoto 标记保留", rs[1].kind);
  const v = rs.find((r) => r.kind === "video");
  ok(v && v.url === "https://sns-video-qc.xhscdn.com/stream/.mp4" && v.meta.quality === "1920p",
    "视频 h264 优先 + URL 解码 + 清晰度", v && v.url);
  ok(v && v.headers.Referer === "https://www.xiaohongshu.com/", "视频带 Referer");
  ok(rs[0].headers.Referer === "https://www.xiaohongshu.com/", "图片带 Referer");

  // 边界：hook/SSR 全空的页面安全返回空清单（不抛错、source 不残留上次的 hook）
  ADL.adapters.xhs.lastSource = "hook";
  const empty = ADL.adapters.xhs.extract({
    url: "https://www.xiaohongshu.com/explore/6540abc123def4567890abcd",
    captured: () => [],
    inlineJson: () => null,
    dom: () => ({ images: [] }),
  });
  ok(Array.isArray(empty) && empty.length === 0 &&
     ADL.adapters.xhs.lastSource === "ssr",
    "无数据安全返回空且 source 回落 ssr", empty && empty.length);
}

// ---------------------------------------------------------------- X (Twitter)
console.log("x:");
{
  const g = JSON.parse(F("x_graphql.json"));
  const ctx = {
    url: "https://x.com/testuser/status/1700000000000000001",
    captured: (k) => (k === "x" ? [{ url: "gql", data: g }] : []),
    inlineJson: () => null,
    dom: () => ({ images: [] }),
  };
  const rs = ADL.adapters.x.extract(ctx);
  ok(rs.length === 4, "详情页只收当前推文：1 图 + 3 码率变体", rs.length);
  const ph = rs.find((r) => r.kind === "image");
  ok(ph && ph.url === "https://pbs.twimg.com/media/abc123.jpg?format=jpg&name=orig",
    "照片升级 name=orig 原图", ph && ph.url);
  const vid = rs.find((r) => r.kind === "video" && !r.optional);
  ok(vid && vid.url.endsWith("vid2.mp4") && vid.meta.quality === "2176kbps",
    "视频默认最高码率（跳过 m3u8，低码率 optional）", vid && vid.url);

  // 无 /status 上下文时：时间线全部媒体 + DOM 兜底
  const ctx2 = {
    url: "https://x.com/home",
    captured: (k) => (k === "x" ? [{ url: "gql", data: g }] : []),
    inlineJson: () => null,
    dom: () => ({
      images: [
        { src: "https://pbs.twimg.com/media/xyz.jpg?format=jpg&name=900x900", },
        { src: "https://pbs.twimg.com/profile_images/me_normal.jpg" },
      ],
    }),
  };
  const rs2 = ADL.adapters.x.extract(ctx2);
  ok(rs2.some((r) => r.url === "https://pbs.twimg.com/media/other.jpg?format=jpg&name=orig"),
    "无 status 上下文：时间线推文也收集", rs2.map((r) => r.url));
  ok(rs2.some((r) => r.url === "https://pbs.twimg.com/media/xyz.jpg?format=jpg&name=orig"),
    "DOM 图片兜底 → name=orig", rs2.map((r) => r.url));
  ok(!rs2.some((r) => r.url.includes("profile_images")), "头像不入列");
}

// ---------------------------------------------------------------- 微信文章
console.log("wechat-article:");
{
  const fakeDoc = {
    title: "深度好文：如何优雅地下载图片",
    querySelector: () => null,
    images: [
      { src: "data:image/svg+xml;base64,xxx", getAttribute: () => null },
      { src: "https://mmbiz.qpic.cn/mmbiz_jpg/AAA/640?wx_fmt=jpeg", getAttribute: (n) => (n === "data-src" ? "http://mmbiz.qpic.cn/mmbiz_jpg/BBB/0?wx_fmt=png" : null) },
      { src: "https://mmbiz.qpic.cn/mmbiz_jpg/AAA/640?wx_fmt=jpeg", getAttribute: () => null },
      { src: "https://mmbiz.qpic.cn/mmbiz_gif/CCC/640?wx_fmt=gif", getAttribute: () => null },
      { src: "https://example.com/not-mmbiz.jpg", getAttribute: () => null },
    ],
  };
  const ctx = { url: "https://mp.weixin.qq.com/s/abc", captured: () => [], inlineJson: () => null, dom: () => fakeDoc };
  const rs = ADL.adapters["wechat-article"].extract(ctx);
  ok(rs.length === 3, "3 张（懒加载优先/非 mmbiz 不收/dataURL 不收）", rs.length);
  ok(rs[0].url === "https://mmbiz.qpic.cn/mmbiz_jpg/BBB/0?wx_fmt=png",
    "data-src 优先且宽度段去除（/0）+ https 升级", rs[0] && rs[0].url);
  ok(rs[1].url === "https://mmbiz.qpic.cn/mmbiz_jpg/AAA/0?wx_fmt=jpeg",
    "src 兜底同样规格化（/640 去除）", rs[1] && rs[1].url);
  ok(rs[0].filename.endsWith(".png") && rs[0].filename.startsWith("深度好文：如何优雅地下载图片"),
    "文件名 = 文章标题 + wx_fmt 扩展名", rs[0].filename);
  ok(rs.every((r) => r.headers.Referer === "https://mp.weixin.qq.com/"), "防盗链 Referer");

  // —— normalize 直测：URL 形态覆盖（镜像主机、无宽度段、fragment、域名藏在 query）
  const N = ADL.adapters["wechat-article"].normalize;
  ok(N("https://mmbiz.qpic.mmirror/mmbiz_qq/XXX/640?wx_fmt=jpeg") === "https://mmbiz.qpic.mmirror/mmbiz_qq/XXX/0?wx_fmt=jpeg",
    "normalize：镜像主机形态（mmbiz_qq.mmirror）同剥 /640", N("https://mmbiz.qpic.mmirror/mmbiz_qq/XXX/640?wx_fmt=jpeg"));
  // 形态二（实测 2026-10）：路径无尺寸段原样请求 400 → 补 /0 后 200
  ok(N("http://mmbiz.qpic.cn/mmbiz_png/YYY?wx_fmt=png") === "https://mmbiz.qpic.cn/mmbiz_png/YYY/0?wx_fmt=png",
    "normalize：无尺寸段补 /0 + http→https", N("http://mmbiz.qpic.cn/mmbiz_png/YYY?wx_fmt=png"));
  ok(N("https://mmbiz.qpic.cn/mmbiz_jpg/Z/640?wx_fmt=jpeg#imgindex") === "https://mmbiz.qpic.cn/mmbiz_jpg/Z/0?wx_fmt=jpeg",
    "normalize：fragment 剥离（不污染去重键）");
  ok(N("https://mmbiz.qpic.cn/sz_mmbiz_jpg/LONGTOKEN/640?wx_fmt=jpeg&wxfrom=5") === "https://mmbiz.qpic.cn/sz_mmbiz_jpg/LONGTOKEN/0?wx_fmt=jpeg&wxfrom=5",
    "normalize：sz_ 前缀段 + /640?wx_fmt=jpeg&wxfrom=5 形态", );
  ok(N("https://mmbiz.qpic.cn/sz_mmbiz_jpg/HASH?wxfrom=12&wx_fmt=jpg&tp=webp&usePicPrefetch=1") ===
     "https://mmbiz.qpic.cn/sz_mmbiz_jpg/HASH/0?wxfrom=12&wx_fmt=jpg&usePicPrefetch=1",
    "normalize：query-only 形态补 /0 且剥 tp=webp（真实失败样本）");

  // —— 真实形态组合：占位 data-src 回退 / tp=webp / 协议相对 / data-type 别名 /
  //    svg optional / 同图多尺寸去重 / 标题降级
  const fakeDoc2 = {
    title: "", // 无 #activity-name、无 og:title、无 doc.title → “微信文章”降级
    querySelector: () => null,
    images: [
      // data-src 是占位 base64 → 应回退 src（旧实现整图丢弃）
      { src: "https://mmbiz.qpic.cn/mmbiz_png/DDD/640?wx_fmt=png&from=appmsg",
        getAttribute: (n) => (n === "data-src" ? "data:image/svg+xml;base64,PLACEHOLDER" : null) },
      // tp=webp：不剥离引擎会下到 webp 字节却存 .jpeg；顺带测首尾空白 trim
      { src: "",
        getAttribute: (n) => (n === "data-src" ? " https://mmbiz.qpic.cn/mmbiz_jpg/EEE/640?wx_fmt=jpeg&tp=webp&wxfrom=5&wx_lazy=1 " : null) },
      // 协议相对 URL
      { src: "//mmbiz.qpic.cn/mmbiz_gif/FFF/640?wx_fmt=gif", getAttribute: () => null },
      // 无宽度段 + wx_fmt 缺失 → data-type 别名出扩展名
      { src: "https://mmbiz.qpic.cn/mmbiz_gif/GGG?wxfrom=5",
        getAttribute: (n) => (n === "data-type" ? "gif" : null) },
      // svg 长图（svg+ts 组合流）→ optional
      { src: "https://mmbiz.qpic.cn/mmbiz_svg/HHH/0?wx_fmt=svg", getAttribute: () => null },
      // 与第 2 张同图异尺寸（/640+tp=webp vs /0）→ 规格化后同键去重
      { src: "https://mmbiz.qpic.cn/mmbiz_jpg/EEE/0?wx_fmt=jpeg", getAttribute: () => null },
    ],
  };
  const rs2 = ADL.adapters["wechat-article"].extract({
    url: "https://mp.weixin.qq.com/s/x", captured: () => [], inlineJson: () => null, dom: () => fakeDoc2,
  });
  ok(rs2.length === 5, "6 img → 5 项（EEE /640 与 /0 同图去重）", rs2.length);
  ok(rs2[0].url === "https://mmbiz.qpic.cn/mmbiz_png/DDD/0?wx_fmt=png&from=appmsg",
    "data-src 为占位 base64 时回退 src", rs2[0] && rs2[0].url);
  ok(rs2[1].url === "https://mmbiz.qpic.cn/mmbiz_jpg/EEE/0?wx_fmt=jpeg&wxfrom=5&wx_lazy=1",
    "tp=webp 剥离 + 首尾空白 trim", rs2[1] && rs2[1].url);
  ok(rs2[1].filename.endsWith(".jpeg"), "剥离 webp 后扩展名与实际字节一致", rs2[1].filename);
  ok(rs2[2].url === "https://mmbiz.qpic.cn/mmbiz_gif/FFF/0?wx_fmt=gif",
    "协议相对 URL 升级 https", rs2[2] && rs2[2].url);
  ok(rs2[3].url === "https://mmbiz.qpic.cn/mmbiz_gif/GGG/0?wxfrom=5" && rs2[3].filename.endsWith(".gif"),
    "无宽度段补 /0 + wx_fmt 缺失时 data-type 别名", rs2[3] && rs2[3].url);
  ok(rs2[4].optional === true && rs2[4].filename.endsWith(".svg"),
    "svg 长图标记 optional（组合流单直链还原不了）", rs2[4]);
  ok(rs2[0].filename.startsWith("微信文章_"),
    "标题降级链：无 #activity-name/og/doc.title → 微信文章", rs2[0].filename);
  ok(rs2.every((r) => r.headers.Referer === "https://mp.weixin.qq.com/"), "Referer 保持");

  // —— 同源 iframe（素材编辑页 UEditor 正文）兜底；跨域 iframe 抛错不致死
  const iframeImg = { src: "https://mmbiz.qpic.cn/mmbiz_jpg/III/640?wx_fmt=jpeg", getAttribute: () => null };
  const fakeDoc3 = {
    title: "编辑器预览",
    querySelector: () => null,
    images: [],
    querySelectorAll: (sel) => (sel === "iframe"
      ? [
          { get contentDocument() { throw new Error("SecurityError"); } }, // 跨域卡片
          { contentDocument: { images: [iframeImg] } },                    // 同源正文
        ]
      : []),
  };
  const rs3 = ADL.adapters["wechat-article"].extract({
    url: "https://mp.weixin.qq.com/s/x", captured: () => [], inlineJson: () => null, dom: () => fakeDoc3,
  });
  ok(rs3.length === 1 && rs3[0].url === "https://mmbiz.qpic.cn/mmbiz_jpg/III/0?wx_fmt=jpeg",
    "同源 iframe 内图片（编辑页）收进清单，跨域 iframe 安全跳过", rs3.map((r) => r.url));

  // —— 门禁与归一：域名藏在 query 的重定向链不收；主机大小写归一
  const rs4 = ADL.adapters["wechat-article"].extract({
    url: "https://mp.weixin.qq.com/s/x", captured: () => [], inlineJson: () => null,
    dom: () => ({
      title: "t", querySelector: () => null,
      images: [
        // 旧子串门禁会误收：mmbiz.qpic.cn 出现在 query 里，真正主机是 evil.com
        { src: "https://evil.com/redirect?u=https://mmbiz.qpic.cn/mmbiz_jpg/VVV/640?wx_fmt=jpeg", getAttribute: () => null },
        { src: "HTTP://MMBIZ.QPIC.CN/mmbiz_png/UUU/640?wx_fmt=PNG", getAttribute: () => null },
      ],
    }),
  });
  ok(rs4.length === 1 && rs4[0].url === "https://mmbiz.qpic.cn/mmbiz_png/UUU/0?wx_fmt=PNG",
    "域名藏 query 的重定向链不收；大写主机/扩展名归一小写", rs4.map((r) => r.url));
  ok(rs4[0] && rs4[0].filename.endsWith(".png"), "wx_fmt 值大小写归一 → .png", rs4[0] && rs4[0].filename);
  ok(ADL.adapters["wechat-article"].normalize("HTTP://MMBIZ.QPIC.CN/mmbiz_png/UUU/640?wx_fmt=png") ===
     ADL.adapters["wechat-article"].normalize("https://mmbiz.qpic.cn/mmbiz_png/UUU/640?wx_fmt=png"),
    "大小写变体与常规形态同去重键（同图不重复入列）");
}

// ---------------------------------------------------------------- 注册表
console.log("registry:");
ok(ADL.matchAdapter("https://www.douyin.com/video/1").id === "douyin", "douyin 命中");
ok(ADL.matchAdapter("https://x.com/a/status/1").id === "x", "x 命中");
ok(ADL.matchAdapter("https://twitter.com/a/status/1").id === "x", "twitter.com 命中");
ok(ADL.matchAdapter("https://www.xiaohongshu.com/explore/1").id === "xhs", "xhs 命中");
ok(ADL.matchAdapter("https://mp.weixin.qq.com/s/1").id === "wechat-article", "wechat 命中");
ok(ADL.matchAdapter("https://example.com/") === null, "其他站不命中");

console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
