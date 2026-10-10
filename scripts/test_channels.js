// 视频号适配器单测（node scripts/test_channels.js）
"use strict";
const path = require("path");
const fs = require("fs");

globalThis.self = globalThis;
const A = "extension/adapters";
require(path.join("..", A, "types.js"));
require(path.join("..", A, "core.js"));
require(path.join("..", A, "channels.js"));

let pass = 0, fail = 0;
const ok = (c, label, extra) => {
  if (c) { pass++; console.log("  ok  " + label); }
  else { fail++; console.log("  FAIL " + label + "  => " + JSON.stringify(extra)); }
};

const feed = JSON.parse(
  fs.readFileSync(path.join(A, "fixtures", "channels_feed.json"), "utf-8")
);
const ctx = {
  url: "https://channels.weixin.qq.com/tab/feed",
  captured: (k) => (k === "channels" ? [{ url: "finder-api", data: feed }] : []),
  inlineJson: () => null,
  dom: () => ({ images: [] }),
};
const rs = ADL.adapters.channels.extract(ctx);

ok(rs.length === 2, "2 个视频（封面 jpg 不入列）", rs.map((r) => r.url));
const first = rs.find((r) => r.url.includes("fake_abc"));
ok(first && first.meta.decrypt && first.meta.decrypt.seed === "2136343393" &&
   first.meta.decrypt.algo === "isaac64-xor",
  "同对象 decode_key 配对 + algo 正确", first && first.meta);
const second = rs.find((r) => r.url.includes("second.mp4"));
ok(second && second.meta.decrypt && second.meta.decrypt.seed === "2136343393",
  "嵌套对象 URL 兜底配到响应级 key", second && second.meta);
ok(first.filename === "旅游vlog第12期：大理的风.mp4",
  "文件名取同对象 title", first && first.filename);
ok(second.filename === "第二条.mp4", "嵌套 URL 用沿途最近 title", second && second.filename);
ok(rs.every((r) => r.headers.Referer === "https://channels.weixin.qq.com/"), "Referer 头");
ok(ADL.matchAdapter("https://channels.weixin.qq.com/tab/profile").id === "channels", "注册表命中");

// 空/无 key 响应：URL 仍入列（无 decrypt），引擎侧会失败并明确报错
const rs2 = ADL.adapters.channels.extract({
  ...ctx,
  captured: () => [{ url: "x", data: { media: [{ url: "https://finder.video.qq.com/a.mp4" }] } }],
});
ok(rs2.length === 1 && !rs2.meta && !rs2[0].meta.decrypt,
  "无 decode_key 时仍收录（不伪造密钥）", rs2[0] && rs2[0].meta);

console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
