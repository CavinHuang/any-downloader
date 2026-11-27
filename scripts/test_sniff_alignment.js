// 浏览器嗅探与接管对齐单元测试（node scripts/test_sniff_alignment.js）
"use strict";
const assert = require("node:assert/strict");
const path = require("node:path");

globalThis.self = globalThis;
require(path.join("..", "extension", "adapters", "types.js"));
require(path.join("..", "extension", "adapters", "core.js"));
const ADL = globalThis.self.ADL;

// 1. HLS master 清单解析：提取分辨率、按清晰度降序、相对路径补全与去重
{
  const master = [
    "#EXTM3U",
    '#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=854x480,NAME="480P"',
    "low/index.m3u8",
    '#EXT-X-STREAM-INF:BANDWIDTH=4200000,RESOLUTION=1920x1080,NAME="1080P"',
    "https://cdn.example.com/1080/index.m3u8",
    '#EXT-X-STREAM-INF:BANDWIDTH=2100000,RESOLUTION=1280x720',
    "/hd/720.m3u8?token=abc",
  ].join("\n");
  const r = ADL.parseHlsManifest(master, "https://cdn.example.com/v/master.m3u8");
  assert.equal(r.isMaster, true);
  assert.equal(r.variants.length, 3);
  assert.equal(r.variants[0].label, "1080p");
  assert.equal(r.variants[0].url, "https://cdn.example.com/1080/index.m3u8");
  assert.equal(r.variants[1].label, "720p");
  assert.equal(r.variants[1].url, "https://cdn.example.com/hd/720.m3u8?token=abc");
  assert.equal(r.variants[2].url, "https://cdn.example.com/v/low/index.m3u8");
  console.log("  ok  parseHlsManifest master variants & sorting");
}

// 2. HLS 分片媒体清单解析：统计总时长与分片数
{
  const media = [
    "#EXTM3U",
    "#EXT-X-TARGETDURATION:10",
    "#EXTINF:9.5,",
    "seg-1.ts",
    "#EXTINF:10.5,",
    "seg-2.ts",
    "#EXT-X-ENDLIST",
  ].join("\n");
  const r = ADL.parseHlsManifest(media, "https://cdn.example.com/v/index.m3u8");
  assert.equal(r.isMaster, false);
  assert.equal(r.segmentCount, 2);
  assert.equal(r.durationSec, 20);
  console.log("  ok  parseHlsManifest media duration & segment count");
}

// 3. DASH MPD 清单解析：提取 ISO8601 时长与分辨率档位
{
  const mpd = `<MPD mediaPresentationDuration="PT1M30.5S"><Period><AdaptationSet><Representation width="1280" height="720" bandwidth="1500000"/><Representation width="1920" height="1080" bandwidth="3800000"/></AdaptationSet></Period></MPD>`;
  const r = ADL.parseDashManifest(mpd, "https://cdn.example.com/manifest.mpd");
  assert.equal(r.isMaster, true);
  assert.equal(r.durationSec, 90.5);
  assert.equal(r.variants[0].label, "1080p");
  assert.equal(r.variants[1].label, "720p");
  console.log("  ok  parseDashManifest duration & representations");
}

// 4. 网页标题命名规则：通用文件名用标题替换；具名文件保留并带清晰度标签
{
  assert.equal(
    ADL.buildMediaFilename("极客课程：第一讲", "https://cdn.example.com/play/index.m3u8", "hls", "1080p", true),
    "极客课程：第一讲 - 1080p.mp4"
  );
  assert.equal(
    ADL.buildMediaFilename("发布会直播回放", "https://cdn.example.com/keynote.mp4", "video", null, true),
    "发布会直播回放 - keynote.mp4"
  );
  assert.equal(
    ADL.buildMediaFilename("发布会直播回放", "https://cdn.example.com/play/index.m3u8", "hls", null, false),
    "index.mp4"
  );
  console.log("  ok  buildMediaFilename page title prefixing");
}

// 5. 请求头大小写归一与 Cookie 兜底（防止 cookie + Cookie 双键重复）
{
  const normalized = ADL.normalizeHeadersWithCookie(
    { Referer: "https://a.example/", cookie: "sid=1", Cookie: "sid=2", "X-Other": "drop" },
    "sid=fallback"
  );
  assert.deepEqual(normalized, { referer: "https://a.example/", cookie: "sid=2" });
  const injected = ADL.normalizeHeadersWithCookie({ Authorization: "Bearer tok" }, "k=v");
  assert.deepEqual(injected, { authorization: "Bearer tok", cookie: "k=v" });
  console.log("  ok  normalizeHeadersWithCookie deduplication");
}

// 6. 下载接管判断：无后缀附件按 attachment 接管，HTML/忽略站点/过小文件放行
{
  const RE = /\.(zip|mp4|exe)(?=[?#]|$)/i;
  assert.equal(
    ADL.shouldTakeoverDownload({
      url: "https://release.example.com/download?id=99",
      filename: "installer-v2",
      mime: "application/octet-stream",
      fileSize: 20 * 1024 * 1024,
      isAttachment: true,
      ignoredHosts: [],
      minBytes: 1024 * 1024,
      takeoverRe: RE,
    }),
    true
  );
  assert.equal(
    ADL.shouldTakeoverDownload({
      url: "https://ignore.example.com/app.zip",
      filename: "app.zip",
      mime: "application/zip",
      fileSize: 10 * 1024 * 1024,
      isAttachment: true,
      ignoredHosts: ["ignore.example.com"],
      minBytes: 1024 * 1024,
      takeoverRe: RE,
    }),
    false
  );
  assert.equal(
    ADL.shouldTakeoverDownload({
      url: "https://example.com/tiny.zip",
      filename: "tiny.zip",
      mime: "application/zip",
      fileSize: 1024,
      isAttachment: false,
      ignoredHosts: [],
      minBytes: 1024 * 1024,
      takeoverRe: RE,
    }),
    false
  );
  console.log("  ok  shouldTakeoverDownload rules");
}
