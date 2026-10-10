// 适配器增强单测：xhs API hook / 可选画质变体 / 粘贴口令解析
// （node scripts/test_enhancements.js）
"use strict";
const path = require("path");
const fs = require("fs");

globalThis.self = globalThis;
const A = "extension/adapters";
require(path.join("..", A, "types.js"));
require(path.join("..", A, "core.js"));
require(path.join("..", A, "wechat-article.js"));
require(path.join("..", A, "xhs.js"));
require(path.join("..", A, "douyin.js"));
require(path.join("..", A, "x.js"));
require(path.join("..", A, "channels.js"));

const F = (f) => fs.readFileSync(path.join(A, "fixtures", f), "utf-8");
let pass = 0, fail = 0;
const ok = (c, label, extra) => {
  if (c) { pass++; console.log("  ok  " + label); }
  else { fail++; console.log("  FAIL " + label + "  => " + JSON.stringify(extra)); }
};

// ---------------------------------------------------------------- xhs API hook
console.log("xhs (API hook):");
{
  const feed = JSON.parse(F("xhs_feed.json"));
  const ctx = {
    url: "https://www.xiaohongshu.com/explore/6540abc123def4567890ffff",
    captured: (k) => (k === "xhs" ? [{ url: "feed", data: feed }] : []),
    inlineJson: () => null,
    dom: () => ({ images: [] }),
  };
  const rs = ADL.adapters.xhs.extract(ctx);
  // ffff：2 图（1 张 live 静帧）+ live 视频 + 主视频(1080) + 720 变体；
  // bbbb：1 图；cccc：空壳卡片（无标题/无 url/空 master_url）0 项
  ok(rs.length === 6, "6 项（当前笔记 5 + 时间线 1；空壳卡片不计）",
    rs.map((r) => r.filename));
  ok(rs[0].url === "https://sns-webpic-qc.xhscdn.com/20250103/cover/nd/img1" &&
     !rs[0].url.includes("!"),
    "url_default 无水印 + https", rs[0] && rs[0].url);
  const live = rs.find((r) => r.kind === "livephoto");
  ok(!!live, "live_photo 静帧标记保留", live && live.filename);
  const liveVid = rs.find(
    (r) => r.kind === "video" && r.meta.extra && r.meta.extra.livePhoto
  );
  ok(liveVid && liveVid.url === "https://sns-video.xhscdn.com/stream/live_img2.mp4" &&
     liveVid.optional === false &&
     liveVid.filename === "苏州两天一晚攻略_live_02.mp4",
    "livephoto 视频流补齐（live_photo_url，非可选）", liveVid && liveVid.filename);
  const v = rs.find(
    (r) => r.kind === "video" && !r.optional && !(r.meta.extra && r.meta.extra.livePhoto)
  );
  ok(v && v.url === "https://sns-video.xhscdn.com/stream/suzhou_1080.mp4" &&
     v.meta.quality === "1080p",
    "视频主资源 = master_height 最高档（h264 数组乱序也取对）", v && v.url);
  const variants = rs.filter((r) => r.kind === "video" && r.optional);
  ok(variants.length === 1 && variants[0].url.endsWith("suzhou_720.mp4") &&
     variants[0].meta.quality === "720p",
    "低档高度降为 optional 变体", variants.map((r) => r.url));
  ok(v && v.filename === "苏州两天一晚攻略.mp4", "当前笔记（URL 匹配）排最前", v && v.filename);
  ok(rs[rs.length - 1].url.includes("/20250102/old/"), "时间线旧笔记排最后", rs.length);

  // /user/profile/{uid}/{nid} 形态：当前笔记 id 是第二段 hex（第一段是用户 id）
  const rp = ADL.adapters.xhs.extract({
    ...ctx,
    url: "https://www.xiaohongshu.com/user/profile/5ff0e6410000000001008400/6540abc123def4567890bbbb",
  });
  ok(rp.length === 6 && rp[0].filename === "旧时间线笔记_01.jpg",
    "user/profile/{uid}/{nid} 命中当前笔记排最前", rp[0] && rp[0].filename);

  // 同笔记双捕获（/feed + /note 端点，视频签名 URL 不同）：按 id 去重保留最新
  const noteCap = JSON.parse(JSON.stringify(feed));
  const cur = noteCap.data.items.find((it) => it.id === "6540abc123def4567890ffff");
  cur.note_card.video.media.stream.h264[1].master_url =
    "https%3A%2F%2Fsns-video.xhscdn.com%2Fstream%2Fsuzhou_1080_v2.mp4";
  const rd = ADL.adapters.xhs.extract({
    ...ctx,
    captured: (k) => (k === "xhs" ? [{ url: "feed", data: feed }, { url: "note", data: noteCap }] : []),
  });
  const dVids = rd.filter(
    (r) => r.kind === "video" && !r.optional && !(r.meta.extra && r.meta.extra.livePhoto)
  );
  ok(dVids.length === 1 && dVids[0].url.endsWith("suzhou_1080_v2.mp4"),
    "同笔记双捕获按 id 去重且保留最新签名", dVids.map((r) => r.url));
  ok(!rd.some((r) => r.url.endsWith("suzhou_1080.mp4")), "旧签名不残留", null);

  // hook + ssr 合并（非重叠首屏）：SSR 当前笔记被补充
  const stText = F("xhs_state.txt");
  const sm = /window\.__INITIAL_STATE__=/.exec(stText);
  const st = ADL.parseLooseJson(ADL._balanced(stText, stText.indexOf("{", sm.index)));
  const ctx2 = { ...ctx, inlineJson: () => st };
  const rs2 = ADL.adapters.xhs.extract(ctx2);
  const urls2 = rs2.map((r) => r.url);
  ok(new Set(urls2).size === rs2.length, "hook+ssr 合并无重复 URL", rs2.length);
  ok(rs2.length === 9, "SSR 首屏笔记（abcd 3 项）被补充：6 hook + 3 ssr", rs2.length);
  ok(urls2.includes("https://sns-video-qc.xhscdn.com/stream/.mp4"),
    "SSR abcd 视频入列", urls2);

  // hook + ssr 重叠：SSR 同笔记给出不同签名/尺寸的等价直链时让位 hook
  // （xhsResourcesFromState 只取 currentNoteId 一条，故把首屏换成重叠笔记 ffff）
  const st2 = JSON.parse(JSON.stringify(st));
  st2.note.currentNoteId = "6540abc123def4567890ffff";
  st2.note.noteDetailMap["6540abc123def4567890ffff"] = {
    note: {
      noteId: "6540abc123def4567890ffff",
      title: "苏州两天一晚攻略",
      desc: "附路线图",
      imageList: [
        // 与 hook 相同 URL（去水印后）→ URL 级去重
        { urlDefault: "https://sns-webpic-qc.xhscdn.com/20250103/cover/nd/img1!nd_dft_wgtehjqv10hmmm", livePhoto: false, infoList: [] },
        // 不同尺寸直链 → noteId|filename 二级去重（hook 优先）
        { urlDefault: "https://sns-webpic-qc.xhscdn.com/20250103/live/nd/img2_alt!nd_dft_bbb", livePhoto: false, infoList: [] },
      ],
      video: { media: { stream: { h264: [
        { masterUrl: "https://sns-video.xhscdn.com/stream/suzhou_ssr.mp4", masterHeight: 1080 },
      ] } } },
    },
  };
  const rs3 = ADL.adapters.xhs.extract({ ...ctx, inlineJson: () => st2 });
  const urls3 = rs3.map((r) => r.url);
  ok(new Set(urls3).size === rs3.length, "重叠合并无重复 URL", rs3.length);
  ok(rs3.length === 6, "SSR 同笔记等价直链全部让位（总数不涨）", rs3.length);
  ok(!urls3.some((u) => u.includes("suzhou_ssr") || u.includes("img2_alt")),
    "同笔记等价直链（不同签名/尺寸）不重复计数", urls3);
  ok(rs3.filter((r) => r.kind === "video" && !r.optional &&
      !(r.meta.extra && r.meta.extra.livePhoto)).length === 1,
    "同笔记主视频只保留 hook 一份", urls3);
  ok(ADL.adapters.xhs.lastSource === "hook", "source = hook");
}

// xhs 分享页 HTML（background 粘贴口令路径）
console.log("xhs (share html):");
{
  const stText = F("xhs_state.txt");
  const html = `<!DOCTYPE html><script>window.__INITIAL_STATE__=${stText.match(/=\s*(\{.*\});?/)[1]};</script>`;
  const rs = ADL.xhsShareHtmlResources(html);
  ok(rs.length === 3, "分享页 HTML → 3 项（2 图 + 1 视频）", rs.length);
  ok(rs.every((r) => r.headers.Referer === "https://www.xiaohongshu.com/"), "Referer 头");
  ok(ADL.xhsShareHtmlResources("<html>nope</html>").length === 0, "无数据安全返回空");
}

// ---------------------------------------------------------------- 抖音变体
console.log("douyin (quality variants):");
{
  const aweme = {
    aweme_id: "7301234567890123456",
    desc: "做饭教程",
    author: { nickname: "小厨/娘" },
    video: {
      play_addr: {
        uri: "v0d00abc",
        url_list: ["https://www.iesdouyin.com/aweme/v1/play/?video_id=v0d00abc"],
      },
      bit_rate: [
        { bitrate: 2000000, gear_name: "1080p", play_addr: { uri: "v0d00abc", url_list: ["https://www.iesdouyin.com/aweme/v1/play/?video_id=v0d00abc&ratio=1080p"] } },
        { bitrate: 900000, gear_name: "540p", play_addr: { uri: "v0d00abc_540", url_list: ["https://www.iesdouyin.com/aweme/v1/play/?video_id=v0d00abc_540"] } },
      ],
    },
  };
  const rs = ADL.awemeToResources(aweme);
  const vids = rs.filter((r) => r.kind === "video");
  ok(vids.length === 2, "2 个清晰度（主 + 540p；1080p 与主同 video_id 去重）", vids.length);
  ok(!vids[0].optional && vids[0].meta.quality === undefined,
    "主资源在前且非可选", vids[0] && vids[0].meta);
  ok(vids[1].optional === true && vids[1].meta.quality === "540p" &&
     vids[1].url.includes("v0d00abc_540"),
    "变体标记 optional + gear_name", vids[1] && vids[1].optional);
  ok(vids[0].filename === "小厨 娘_做饭教程.mp4",
    "作者昵称前缀（/ 清洗为空格）", vids[0] && vids[0].filename);
  ok(vids.every((v) => v.headers["User-Agent"].includes("iPhone")), "变体同样带移动端 UA");
}

// gear_name 缺失时的画质标签兜底（douyin_edge.json 第 3 个 aweme）
console.log("douyin (gear label fallback):");
{
  const edge = JSON.parse(F("douyin_edge.json"));
  const vids = ADL.awemeToResources(edge.aweme_list[2]).filter((r) => r.kind === "video");
  ok(vids.length === 3, "主资源 + 2 变体全部保留", vids.length);
  ok(vids[1].optional === true && vids[1].meta.quality === "1080p",
    "无 gear_name → 按 play_addr 分辨率短边标注（1080x1920 → 1080p）",
    vids[1] && vids[1].meta.quality);
  ok(vids[2].meta.quality === "low_adapt",
    "gear_name 缺失 → gear 字段兜底", vids[2] && vids[2].meta.quality);
}

// ---------------------------------------------------------------- X 变体
console.log("x (bitrate variants):");
{
  const g = {
    data: {
      tweet_result: {
        result: {
          rest_id: "1700000000000000001",
          core: { user_results: { result: { legacy: { screen_name: "testuser" } } } },
          legacy: {
            full_text: "hi",
            extended_entities: {
              media: [
                {
                  type: "video",
                  video_info: {
                    variants: [
                      { content_type: "video/mp4", url: "https://video.twimg.com/v1.mp4", bitrate: 320000 },
                      { content_type: "video/mp4", url: "https://video.twimg.com/v2.mp4", bitrate: 2176000 },
                      { content_type: "video/mp4", url: "https://video.twimg.com/v3.mp4", bitrate: 1280000 },
                      { content_type: "application/x-mpegURL", url: "https://video.twimg.com/pl.m3u8" },
                    ],
                  },
                },
              ],
            },
          },
        },
      },
    },
  };
  const ctx = {
    url: "https://x.com/testuser/status/1700000000000000001",
    captured: (k) => (k === "x" ? [{ url: "gql", data: g }] : []),
    inlineJson: () => null,
    dom: () => ({ images: [] }),
  };
  const rs = ADL.adapters.x.extract(ctx);
  ok(rs.length === 3, "3 个码率变体（m3u8 跳过）", rs.length);
  ok(!rs[0].optional && rs[0].meta.quality === "2176kbps" &&
     rs[0].url.endsWith("v2.mp4"),
    "默认 = 最高码率非可选", rs[0]);
  ok(rs.slice(1).every((r) => r.optional), "低码率全部 optional",
    rs.map((r) => r.optional));
  ok(rs[0].filename.startsWith("x_testuser_1700000000000000001"),
    "文件名带 screen_name + statusId", rs[0] && rs[0].filename);
}

// ---------------------------------------------------------------- 粘贴口令
console.log("extractLink:");
ok(ADL.extractLink("8.88 复制此链接，打开抖音搜索 https://v.douyin.com/iAbCdEf/ 看视频") ===
   "https://v.douyin.com/iAbCdEf/", "口令文本提取抖音短链");
ok(ADL.extractLink("看看这个 http://xhslink.com/a1B2c3 哈哈") ===
   "http://xhslink.com/a1B2c3", "xhslink 短链");
ok(ADL.extractLink("没有链接的文本") === null, "无链接返回 null");
ok(ADL.extractLink("https://www.xiaohongshu.com/explore/6540abc") ===
   "https://www.xiaohongshu.com/explore/6540abc", "完整 URL 原样提取");
ok(ADL.extractLink("打开抖音 https://v.douyin.com/iAbCdEf/。看看") ===
   "https://v.douyin.com/iAbCdEf/", "尾部全角句号剥离");
ok(ADL.extractLink("链接是 https://v.douyin.com/iAbCdEf/！") ===
   "https://v.douyin.com/iAbCdEf/", "尾部全角叹号剥离");
ok(ADL.extractLink("see https://v.douyin.com/iAbCdEf/.") ===
   "https://v.douyin.com/iAbCdEf/", "尾部 ASCII 句点剥离");

console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
