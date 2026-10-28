// 视频悬浮下载来源决策测试（node scripts/test_video_resolver.js）
// 覆盖 core.js ADL.pickVideoSource 五条路线与边界：
//   adapter（非 optional 优先 / 时长就近匹配 / 忽略图片）> direct（http src）
//   > sniff（hls 最近优先、mp4 按体积、segment/dash 排除）> ytdlp（域名后缀
//   匹配）> none。零依赖，与 test_articles.js 同款加载方式。
"use strict";
const path = require("path");

globalThis.self = globalThis;

require(path.join("..", "extension", "adapters", "types.js"));
require(path.join("..", "extension", "adapters", "core.js"));
const ADL = globalThis.self.ADL;

let pass = 0;
let fail = 0;
function ok(cond, label, extra) {
  if (cond) {
    pass++;
    console.log("  ok  " + label);
  } else {
    fail++;
    console.log("  FAIL " + label + (extra !== undefined ? "  => " + JSON.stringify(extra) : ""));
  }
}
function eq(a, b, label) {
  ok(a === b, label + " [" + JSON.stringify(a) + " === " + JSON.stringify(b) + "]");
}

const HOSTS = ["youtube.com", "youtu.be", "x.com", "twitter.com", "bilibili.com", "b23.tv", "tiktok.com", "douyin.com", "iesdouyin.com"];

console.log("pickVideoSource:");

// 1. adapter：无时长信息 → 非 optional 的第一条视频资源
{
  const r = ADL.pickVideoSource({
    srcUrl: "blob:https://www.douyin.com/xxx",
    pageUrl: "https://www.douyin.com/video/7301234567890",
    recs: [],
    adapterResources: [
      { url: "https://img.douyinpic.com/a.webp", kind: "image" },
      { url: "https://www.douyin.com/aweme/v1/play/?video_id=opt", kind: "video", optional: true },
      { url: "https://www.douyin.com/aweme/v1/play/?video_id=main", kind: "video" },
    ],
    duration: 0,
    ytdlpHosts: HOSTS,
  });
  eq(r.via, "adapter", "1a adapter 优先");
  eq(r.urls[0], "https://www.douyin.com/aweme/v1/play/?video_id=main", "1b 取非 optional 视频资源（图片忽略）");
}

// 2. adapter：时长可用 → 就近匹配悬浮中的视频（feed 页多条候选场景）
{
  const r = ADL.pickVideoSource({
    srcUrl: "blob:https://www.douyin.com/xxx",
    pageUrl: "https://www.douyin.com/?modal_id=730999",
    recs: [],
    adapterResources: [
      { url: "u1", kind: "video", meta: { durationSec: 12 } },
      { url: "u2", kind: "video", meta: { durationSec: 61 } },
      { url: "u3", kind: "video", meta: { durationSec: 305 } },
    ],
    duration: 64,
    ytdlpHosts: HOSTS,
  });
  eq(r.via, "adapter", "2a adapter 优先");
  eq(r.urls[0], "u2", "2b 时长 |61-64| 最近");
}

// 3. adapter：全 optional → 取第一条 optional（时长不可用时稳定取先出现者）
{
  const r = ADL.pickVideoSource({
    srcUrl: "",
    pageUrl: "https://x.com/i/status/123",
    recs: [],
    adapterResources: [
      { url: "v1", kind: "video", optional: true },
      { url: "v2", kind: "video", optional: true },
    ],
    duration: 0,
    ytdlpHosts: HOSTS,
  });
  eq(r.urls[0], "v1", "3 全 optional 取先出现者");
}

// 4. direct：<video> 的 http src 本身就是媒体地址
{
  const r = ADL.pickVideoSource({
    srcUrl: "https://cdn.example.com/media/lesson.mp4",
    pageUrl: "https://example.com/lesson",
    recs: [],
    adapterResources: [],
    duration: 100,
    ytdlpHosts: HOSTS,
  });
  eq(r.via, "direct", "4 http src 走 direct");
  eq(r.url, "https://cdn.example.com/media/lesson.mp4", "4b url 透传");
}

// 5. sniff：hls 清单最近一条优先（master 与子清单同为 hls，后请求的是当前画质）
{
  const r = ADL.pickVideoSource({
    srcUrl: "blob:https://live.example.com/x",
    pageUrl: "https://live.example.com/",
    recs: [
      { url: "https://c/lv/master.m3u8", type: "hls", bytes: 1024, at: 1000 },
      { url: "https://c/lv/v2.m3u8", type: "hls", bytes: 2048, at: 2000 },
      { url: "https://c/lv/big.mp4", type: "video", bytes: 9000000, at: 3000 },
    ],
    adapterResources: [],
    duration: 0,
    ytdlpHosts: HOSTS,
  });
  eq(r.via, "sniff", "5a blob src 落嗅探");
  eq(r.url, "https://c/lv/v2.m3u8", "5b hls 取最近请求");
}

// 6. sniff：无 hls 时 mp4 按体积降序；segment/dash 排除（引擎无 DASH、分片不可单推）
{
  const r = ADL.pickVideoSource({
    srcUrl: "blob:https://v.example.com/x",
    pageUrl: "https://v.example.com/",
    recs: [
      { url: "https://c/a.m4s", type: "segment", bytes: 99999999, at: 9000 },
      { url: "https://c/a.mpd", type: "dash", bytes: 88888888, at: 9000 },
      { url: "https://c/small.mp4", type: "video", bytes: 3000000, at: 4000 },
      { url: "https://c/big.mp4", type: "video", bytes: 8000000, at: 5000 },
    ],
    adapterResources: [],
    duration: 0,
    ytdlpHosts: HOSTS,
  });
  eq(r.via, "sniff", "6a 落嗅探");
  eq(r.url, "https://c/big.mp4", "6b mp4 按体积最大");
}

// 7. ytdlp：MSE 站点（blob src、无可直推嗅探记录）推页面 URL 交引擎托管
{
  for (const page of ["https://www.youtube.com/watch?v=dQw4w9WgXcQ", "https://m.douyin.com/share/video/1", "https://www.bilibili.com/video/BV1xx411c7mD"]) {
    const r = ADL.pickVideoSource({
      srcUrl: "blob:https://x/y",
      pageUrl: page,
      recs: [],
      adapterResources: [],
      duration: 0,
      ytdlpHosts: HOSTS,
    });
    eq(r.via, "ytdlp", "7 ytdlp 兜底 " + page);
    eq(r.url, page, "7b url=页面 " + page);
  }
}

// 8. none：普通站点 blob src 且无嗅探记录
{
  const r = ADL.pickVideoSource({
    srcUrl: "blob:https://random.example.org/v",
    pageUrl: "https://random.example.org/watch",
    recs: [],
    adapterResources: [],
    duration: 0,
    ytdlpHosts: HOSTS,
  });
  eq(r.via, "none", "8 无来源");
}

// 9. 后缀匹配不误伤：netflix.com 不因含 "x.com" 命中
{
  const r = ADL.pickVideoSource({
    srcUrl: "blob:https://www.netflix.com/v",
    pageUrl: "https://www.netflix.com/watch/1",
    recs: [],
    adapterResources: [],
    duration: 0,
    ytdlpHosts: HOSTS,
  });
  eq(r.via, "none", "9 子域后缀精确匹配");
}

// 10. direct 优先于 sniff（src 是该视频的权威地址，嗅探缓存可能是广告）
{
  const r = ADL.pickVideoSource({
    srcUrl: "https://cdn.example.com/v/current.mp4",
    pageUrl: "https://example.com/",
    recs: [{ url: "https://ad.example.net/ad.mp4", type: "video", bytes: 9e9, at: 1 }],
    adapterResources: [],
    duration: 0,
    ytdlpHosts: HOSTS,
  });
  eq(r.url, "https://cdn.example.com/v/current.mp4", "10 direct 先于 sniff");
}

console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
