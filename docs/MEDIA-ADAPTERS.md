# 站点适配层设计：专用媒体资源下载器

> 状态：**P1+P2 已实现**（扩展 v0.6.0 / app v0.4.0）。
> - **桌面嗅探（app 0.5.0，docs/DESKTOP-SNIFF.md）**：浏览器之外的新战场——
>   微信电脑版视频号。本地 CONNECT 代理（`crates/dl-sniff`）仅对 finder/
>   channels 域定向 MITM（其余 TCP 透传），decode_key 配对解密复用引擎
>   postprocess；PAC 路由 + CurrentUser 证书 + 崩溃还原；主界面「桌面嗅探」
>   列表 + 微信视频窗右上角 ⬇ 悬浮图标（`wechat_overlay.rs` 窗口跟踪）。
>   cargo 18 项新增测试（dl-sniff 10 + dl-app 8）。
> - **全站视频悬浮下载（ext 0.9.0，§4.5）**：`video-overlays.js` 任意站点 `<video>`
>   右上角 ⬇ 浮标，`pickVideoSource` 四级来源决策（适配器/直链/嗅探/yt-dlp），
>   单测 `node scripts/test_video_resolver.js`（20）。
> - P1（纯扩展）：L1 挂钩器、bridge、5 适配器（wechat-article/xhs/douyin/x/channels）、
>   悬浮按钮、popup 站点卡片、平台开关、右键菜单。
>   单测 `node scripts/test_adapters.js`（32）+ `test_channels.js`（8）+ `test_enhancements.js`（24）。
>   **增强（ext 0.6.1）**：xhs 补 API hook（`/api/sns/web/v1/feed`，修 SPA 站内导航盲区，
>   hook+SSR 合并去重）；X/抖音多码率变体（默认最高，低码率 optional——悬浮面板可选、
>   一键推送跳过）；X 详情页只收当前推文；抖音文件名带作者前缀 + video_id 去重；
>   popup 粘贴口令入口（extractLink 从分享文本提链，background resolve 支持
>   xhslink/小红书分享页带 Cookie 解析）。
>   **文章适配层（ext 0.6.4 / app 0.4.1，§11）**：12 站点文章下载
>   （Markdown + 文章图片），`task.add_content` 内容任务通道。
>   **合集模式（ext 0.6.5，§11.5）**：文档+图片统一落 `<下载目录>/<标题>/`、
>   相对引用离线可读、「打开文章」预览、浮标精准化。
>   **0.6.6**：微信跨适配器合集打通（mmbizNormalize 共享 + postCleanup 钩子）。
>   **0.6.7（用户决策：只导出 MD）**：移除 HTML 导出与自包含模板，MD 加
>   YAML front matter、表格列对齐、figure/figcaption。
>   **0.7.0 文章操作菜单（§12）**：对齐 code-box 微信面板 12 项操作
>   （封面/摘要/净化/原始链接/编辑/解析/MD/打包/PDF/Word/图片/海报）。
>   单测 `node scripts/test_articles.js`（87，迷你 DOM 桩零依赖）。
>   **粘贴 X 链接（ext 0.8.3）**：popup 粘贴口令支持 x.com/twitter.com/fxtwitter 等
>   形态——syndication 端点匿名解析（token 为 id 确定性算式，yt-dlp 同款），详见 §3.1；
>   `test_adapters.js` 78 断言（含 token 黄金值）。
>   **文章采集增强（ext 0.8.6，§11）**：+3 站点（知乎回答/B站专栏/少数派，共 15 站）；
>   GFM 任务列表；iframe/video/audio 嵌入转链接（原先整段丢失）；details/summary 原生
>   保留；`<mark>` → `==…==`；front matter 新增 author/published（站点选择器 + 通用
>   meta 兜底，日期归一 YYYY-MM-DD）。`test_articles.js` 158 断言。
>   **P0 能力补全（ext 0.8.6 / app 0.4.6 同批）**：扩展推送 Cookie 兜底（嗅探头
>   无 Cookie 时经 chrome.cookies 取该 URL 全部 Cookie 透传，8KB 上限）；单任务
>   代理（CreateTask.proxy 覆盖全局，http(s)/socks5(h)）；after_done 真执行
>   （原先只有设置无消费方）+ 运行自定义命令（{file}/{dir} 占位符）；yt-dlp
>   选项（字幕 srt / 缩略图+元数据嵌入 / cookies.txt，设置驱动）；**直播 HLS
>   自动转 yt-dlp 托管录制**（HTTP 驱动遇 LiveUnsupported 一次性重路由，
>   托管子进程独占限速槽）。cargo 109 + 扩展 506 断言全绿。> - P2（Rust）：**视频号**——channels 适配器（decode_key 配对）+ 引擎 postprocess
>   （`dl-engine::postprocess`，Isaac64-XOR 解密前 128KB；算法已用官方样本逐位对拍锁定：
>   seed→randrsl[0]、u64 大端拼接、整体 reverse）+ `Decrypting` 任务状态 +
>   `tasks.postprocess_json` 列 + `CreateTask.postprocess`（IPC/CLI `--isaac64-seed` 透传）。
>   单测 `node scripts/test_channels.js`（8 断言）。
>   **YouTube**——`dl-ytdlp` crate（sidecar 驱动：进度行解析/取消/产出定位）+
>   `TaskKind::Ytdlp` 托管模式 + YouTube URL 自动识别 + 设置页 `yt_dlp_path`。
>   **yt-dlp 识别扩展到 X/Bilibili/TikTok/抖音（app v0.4.x，与 ext 0.8.3 同批）**：
>   `is_ytdlp_host` 域名清单 + 各平台结构化单作品 id 命名（x_/bili_/tiktok_/douyin_
>   前缀）+ 主页/空间批量防护，详见 §5.2。
>   **安装器内置 yt-dlp + deno**（`scripts/fetch-ytdlp.ps1` 抓取、缓存于
>   `src-tauri/binaries/*-x86_64-pc-windows-msvc.exe`，tauri externalBin 分发；
>   解析链：设置显式路径 > 应用目录内置 > PATH；子进程 PATH 注入使 yt-dlp
>   能找到同目录 deno 做 nsig 解签）。测试合计：cargo 75 + 扩展 40 全绿。
> 前置调研见会话记录（社区方案分析 2026-10-08）。
> 关联文档：DESIGN.md（引擎/扩展总体架构）、EXTENSION-DEV.md（扩展开发）。

## 0. 目标与定位

在现有通用下载引擎之上新增**站点适配层**，为六个平台提供"单独的媒体下载入口"：
微信公众号文章图片、小红书、抖音、X (Twitter)、微信视频号、YouTube。

核心原则（两条红线）：

1. **引擎不懂平台**：引擎只认"URL + headers + 后处理指令"，所有平台知识封在适配层。
2. **适配层不懂下载**：适配器只做"提取"——把页面/API 数据转成直链清单，推送后全部复用
   现有管线（多线程分段、断点续传、校验、分类、任务面板）。

社区验证的四条路线与本设计的选择：

| 路线 | 说明 | 本设计采用 |
|---|---|---|
| A. 页面内嵌数据 | 解析 SSR 注入的 `__INITIAL_STATE__` / `_ROUTER_DATA` | 兜底数据源（第 2 优先级） |
| B. 运行时 API 挂钩 | MAIN world 包装 fetch/XHR，抄录页面自己发的签名请求 | **主数据源（第 1 优先级）** |
| C. 专家 sidecar | yt-dlp / gallery-dl 独立进程 | 仅 YouTube（gallery-dl 不支持抖音/小红书） |
| D. 代理抓包+解密 | FiddlerCore/Electron 代理 | 否——用 B 路线替代（无需装证书/改系统代理） |

## 1. 总体架构

```
┌────────────────────────── 浏览器扩展 ──────────────────────────┐
│  L1  runtime-hook（MAIN world）                                 │
│      通用、配置驱动的 fetch/XHR 响应捕获器                       │
│        ↓ window.postMessage → content script → background      │
│  L2  adapters/*（每站一个薄提取器）                              │
│      数据源三级合并：L1 捕获 > 页面内嵌 JSON > DOM               │
│        ↓ MediaResource[]（直链 + headers + 后处理指令）          │
│  L3  入口 UI：popup 站点卡片 / 悬浮按钮 / 右键菜单 / 平台开关    │
└──────────────────────────┬──────────────────────────────────────┘
                           │ Native Messaging（现有管道）
┌──────────────────────────▼──────────────────────────────────────┐
│  dl-app（Rust，现有 + 最小扩展）                                  │
│  L4  下载管线：probe → 分段调度 → 单写者落盘（现有，零改动）       │
│  L5  后处理管道（新增）：decrypt(isaac64-xor) → mux(ffmpeg 已有)  │
│      → verify(校验和已有，针对明文) → done                        │
│  L6  dl-ytdlp（P2）：yt-dlp -J 解析 → 喂引擎；失败降级整任务托管   │
└──────────────────────────────────────────────────────────────────┘
```

模块化收益：平台改版通常只破坏三级数据源中的一级（合并降级保证可用）；
新增平台 = 新增一个 `adapters/xxx.js` 文件 + 一条 hook 规则，引擎零改动。

## 2. L1 runtime-hook（通用挂钩器）

**一个通用组件服务所有平台**，适配器不各自 hook——这是整个设计的架构核心。

- 注入：`chrome.scripting.executeScript({ world: "MAIN", files: ["hooks/main-world-hook.js"] })`，
  content script `runAt: "document_start"`，抢在页面脚本之前包装原生 API。
- 通信：MAIN world 无 `chrome.*` → `window.postMessage({ source: "adl-hook", ... })`
  → ISOLATED content script 转发 background。
- Hook 点：
  - `window.fetch`：包装后 clone response（`response.clone().json()`），按 URL 规则匹配；
  - `XMLHttpRequest.prototype.open/send`：记录 method/url，`onload` 时按 `responseType`
    取 `responseText`/`response`。
- 规则注册（background，声明式）：

```js
const HOOK_RULES = {
  douyin:   [{ urlMatch: /aweme\/v1\/web\/aweme\/(post|item|feed)/,   keep: 20 }],
  x:        [{ urlMatch: /\/i\/api\/graphql\/.*(TweetDetail|UserTweets|HomeTimeline)/, keep: 30 }],
  channels: [{ urlMatch: /channels\/finder|finder\.video\.qq\.com/,   keep: 50 }],
};
```

- 存储：按 tabId 隔离的环形缓冲（每规则 `keep` 条上限，新的顶掉最旧的）；
  同一 URL 只留最新响应；单响应 > 2MB 丢弃（时间线 JSON 通常 < 1MB）。
- 生命周期：真实导航（hard navigation）清空该 tab 缓冲；SPA 内部路由保留。
- 页面 CSP 防护：本组件是纯包装、不 eval 外部代码，不受页面 CSP 影响。

## 3. L2 适配器契约

适配器 = 纯提取函数：**输入页面上下文 + 捕获数据，输出资源清单**。不发下载、不碰推送。

```ts
// extension/adapters/types.ts
interface MediaResource {
  url: string;                       // 直链或清单（m3u8）
  kind: "video" | "audio" | "image" | "livephoto";
  filename: string;                  // 适配器建议名（已按 dl-filename 规则清洗）
  headers?: Record<string, string>;  // Referer / User-Agent / Cookie
  meta?: {
    decrypt?: { algo: "isaac64-xor"; seed: string };  // 视频号专用
    quality?: string;                // "1080p" / "原画" / 码率
    title?: string;
    coverUrl?: string;
    durationSec?: number;
    extra?: Record<string, unknown>; // 平台自有字段（作品 id、作者等）
  };
}

interface SiteAdapter {
  id: string;                        // "douyin" | "xhs" | "x" | "wechat-article" | "channels"
  matchHost: RegExp;
  /** 探测当前页面：popup 卡片与悬浮按钮的数据源 */
  probe(ctx: PageContext): Promise<{ resources: MediaResource[]; source: "hook" | "ssr" | "dom" }>;
  /** 可选：解析分享链接/短链（粘贴链接场景，background fetch，可带 Cookie） */
  resolveShareLink?(link: string): Promise<MediaResource[]>;
}

interface PageContext {
  tabId: number;
  url: string;
  captured(ruleKey: string): unknown[];   // L1 捕获的响应（环形缓冲）
  inlineJson(name: string): unknown;      // 页面 <script> 内嵌 JSON（A 路线）
  dom(): Document;                        // DOM 兜底（C 路线）
}
```

**三级数据源合并去重**（按资源 URL / 平台作品 id）：

1. `hook` —— L1 捕获的 API 响应：信息最全（全部清晰度、图集、livephoto、作者）；
2. `ssr` —— 页面内嵌 JSON：首屏兜底（用户直达详情页、hook 注入晚于首屏请求时）；
3. `dom` —— 标签直读：最后防线。

`probe()` 返回 `source` 字段供 UI 提示数据新鲜度。任一级失效自动降级，**这是平台对抗
（改版）下保持可用性的关键设计**。

### 3.1 各适配器提取要点

| 适配器 | 主数据源 | 提取路径 | 特殊处理 |
|---|---|---|---|
| `wechat-article` | DOM | `img[data-src]` / `img[src*=mmbiz.qpic.cn]` | 参数重写原图（`wx_fmt` 保留原格式、去 `/640` 缩放段）；**headers 必须带 Referer**（防盗链） |
| `xhs` | ssr | `__INITIAL_STATE__.note.noteDetailMap[id]` | 图片 `imageList[*].urlDefault` 去 `!nd_dft_...` 水印后缀；视频 `video.media.stream` h264 优先取 `masterUrl`（需 URL 解码）；livephoto = 图+视频成对 |
| `douyin` | hook | `/aweme/v1/web/*` 响应 → `video.play_addr.uri` | 拼 `https://www.iesdouyin.com/aweme/v1/play/?video_id={uri}`（无水印）；headers 带移动端 UA；302 跟随由引擎处理；图集走 `images[]` |
| `x` | hook | GraphQL 响应 → `extended_entities.video_info.variants[]` | 直链 mp4 按码率列出（quality 选择）；图片 DOM `pbs.twimg.com` → `name=orig`；兜底 `cdn.syndication.twimg.com/tweet-result` |
| `channels` | hook | finder 接口响应 → 媒体 URL + **seed（decode_key）** | `meta.decrypt = { algo: "isaac64-xor", seed }`；签名短时效 → **推送即下**（引擎已有即建即下） |
| `youtube`（P2，app 侧） | dl-ytdlp | `yt-dlp -J` | 解析模式直链喂引擎；PO Token 失败降级 yt-dlp 整任务 + 进度桥接 |

抖音"粘贴分享口令"场景走 `resolveShareLink`：background fetch 短链（v.douyin.com）跟随
重定向 → 分享页 HTML 正则取 `window._ROUTER_DATA` → `loaderData → video_(id)/page →
videoInfoRes → item_list[0] → video.play_addr.uri`。

**粘贴 X 链接（ext 0.8.3）**：X 无 SSR、GraphQL 需登录态，background 匿名 fetch 拿不到
`TweetDetail`——社区与 yt-dlp 同款方案改走 **syndication 端点**（`cdn.syndication.twimg.com/
tweet-result?id=…&token=…`，无需任何鉴权）。token 为推文 id 的确定性算式（X 官方 embed JS
同源、yt-dlp `_generate_syndication_token` 逐字移植）：`((id/1e15)*π).toString(36)` 去
`0` 与 `.`——19 位 id 转 double 丢的低位精度 X 侧同样在丢，同一算式即同一 token（测试有
黄金值锁定）。链接形态接受 x.com/twitter.com（含 mobile、/statuses/、/i/web/status/）与
fxtwitter/vxtwitter/fixupx 短链；响应无 `extended_entities`，媒体取顶层 `mediaDetails[]`
（photo 升级 name=orig，video/gif 取 mp4 按码率降序、m3u8/webm 不收），命名与码率约定与
页面路径（`x.js`）一致。本机网络到不了 twimg 未能录制真实响应件（`x_share.json` 为按
yt-dlp 重组来源构造的形态样本），接入真实环境后建议重录。

## 4. L3 入口 UI

1. **popup 站点卡片**：当前 tab 命中适配器时顶部显示卡片（平台图标 + "检测到 1 个视频 /
   4 张图片" + 质量选择 + 一键推送/全选）。
2. **悬浮按钮**：详情页注入 ⬇ 浮标（douyin/xhs/x/channels），单击 = probe + 推送 + 角标反馈。
3. **右键菜单**：按适配器能力注册——"下载全部图片"（微信文章页）、"下载此视频"（douyin/xhs/x）。
4. **设置页平台开关**：每适配器独立启停（复用 `sniffCfg` 的 storage.local 模式，
   `adaptersCfg: { douyin: true, xhs: true, ... }`），关闭时 L1 不注入、菜单不注册。
5. **视频悬浮下载按钮（ext 0.9.0，全站）**：`video-overlays.js` 在任意站点的
   `<video>` 上浮现右上角 ⬇ 浮标（鼠标移入触发，全屏/shadow DOM 内同样生效）。
   单击由 `ADL.pickVideoSource` 决策来源：命中适配器的页面取当前条目最优资源
   （时长就近匹配 feed 多候选）→ 走 adapterPush（保无水印/视频号解密）；
   `<video>` 的 http src 直推；否则用本页嗅探记录（hls 最近优先/mp4 按体积，
   segment/dash 不推）；MSE 站点（YouTube/B站）落页面 URL 交引擎 yt-dlp 托管。
   开关：popup 设置 04（`sniffCfg.videoOverlay`，默认开）。

## 5. 引擎侧改动（Rust，最小集）

### 5.1 后处理管道（P2，视频号解密）

任务元数据新增：

```json
{ "postprocess": [ { "algo": "isaac64-xor", "seed": "..." } ] }
```

- `dl-model::CreateTask` 加 `postprocess: Vec<PostProcess>`；`tasks` 表加
  `postprocess_json` 列（跟随现有迁移模式）。
- 管线位置：`download → decrypt → mux(HLS 已有) → verify → done`。
  **decrypt 在 verify 之前**（校验和永远针对明文）；HLS 分段已在内存/临时文件，
  解密发生在合成阶段之后、verify 之前。
- 实现：`dl-engine::postprocess` 模块。Isaac64 PRNG 密钥流按**文件偏移**生成
  （`skip_to(offset)` 后流式 XOR），天然兼容断点续传（续传部分从中断偏移继续解密）。
  自实现约 200 行（算法已由 Evil0ctal/WeChat-Channels-Video-File-Decryption 公开并
  有测试向量，实现后与已知样本对拍）。
- 状态机：下载完成 → `Decrypting`（新状态，面板显示解密进度）→ `Merging/Verifying` → `Done`。

### 5.2 dl-ytdlp（P2，YouTube；ext 0.8.3 起扩展 X/Bilibili/TikTok）

- 新 crate `dl-ytdlp`：sidecar 二进制（与 dl-cli/dl-server 同打包链，bundle.ps1 加 copy）。
- 模式一（解析）：`yt-dlp -J <url>` → 抽 `formats` 直链 → 构造视频+音频两个引擎任务 +
  ffmpeg 混流（复用 HLS 混流能力）；保留多线程/断点/日志核心价值。
- 模式二（托管，降级）：直接 `yt-dlp --newline --progress-template` 整任务托管，
  stdout 解析桥接为 EngineEvent::Progress。
- **托管平台清单（`ytdlp_task::is_ytdlp_host`）**：YouTube 之外纳入 X/Twitter
  （x.com/twitter.com 及子域）、Bilibili（bilibili.com 及子域、b23.tv 短链）、
  TikTok（tiktok.com 及子域）、抖音（douyin.com 及子域、iesdouyin.com 分享页）。
  建任务 URL 命中域名即自动 `TaskKind::Ytdlp`；命名按平台结构化单作品 id
  （`youtube_/x_/bili_/tiktok_/douyin_` 前缀，短链无 id 退时间戳）；主页形态
  （X 用户页、B 站用户空间、TikTok @user、抖音 /user/）与 YouTube 合集一样
  快速失败报"无法作为单文件任务"。抖音走 yt-dlp 的分享页 SSR 提取路线，公开
  视频可用；浏览器扩展在场时 hook 路线仍是首选（信息更全、含图集/livephoto），
  两条路线并存。
- 设置页：yt-dlp 路径、跟随 nightly 自动更新开关；未安装时 GUI 引导下载。

## 6. 消息协议扩展（extension ⇄）

```
{ type: "adapter-probe", tabId }            → { ok, adapterId, source, resources[] }
{ type: "adapter-push", tabId, urls?[] }    → { ok, pushed, taskIds[] }   // 缺省推送全部
{ type: "adapter-resolve", link }           → { ok, adapterId, resources[] } // 粘贴链接
```

推送落点 = 现有 `pushToApp(rec)`（task.add + task.start 即建即下），`MediaResource` →
`CreateTask` 映射：`url/filename/headers` 直传，`meta.decrypt` → `postprocess`。

## 7. 文件布局

```
extension/
  hooks/main-world-hook.js        # L1 通用挂钩（新增，~150 行）
  adapters/
    types.js                      # 契约（JSDoc 注释型接口）
    index.js                      # 注册表：matchHost 路由 + HOOK_RULES 导出
    wechat-article.js  xhs.js  douyin.js  x.js  channels.js
  float-button.js                 # 悬浮按钮 content script
  popup.{js,html}                 # 站点卡片区间
crates/
  dl-model/                       # + PostProcess, CreateTask.postprocess
  dl-engine/src/postprocess.rs    # isaac64-xor 流式解密（P2）
  dl-ytdlp/                       # yt-dlp sidecar 驱动（P2）
docs/MEDIA-ADAPTERS.md            # 本文档
```

## 8. 测试策略

| 层 | 方法 |
|---|---|
| 适配器提取（纯函数） | **录制的真实响应 JSON 做脱敏 fixture**，每适配器 `__tests__/fixtures/`；改版回归 = 重新录制 |
| L1 挂钩器 | jsdom 模拟 fetch/XHR 包装、环形缓冲淘汰、postMessage 转发 |
| isaac64-xor | 已知明文-密文-种子向量对拍；续传偏移处解密一致性测试 |
| 端到端 | dl-testserver 加 `xor` 模式：服务端输出 XOR 密文 → 引擎下载+解密 → 与明文 sha256 比对 |

## 9. 分期

- **P1（纯扩展，引擎零改动）**：L1 挂钩器 → wechat-article → xhs → douyin → x；
  四个入口（站点卡片/浮标/右键/平台开关）随适配器一起上。
- **P2（Rust）**：channels（挂钩 + 引擎解密）+ youtube（dl-ytdlp）+ Decrypting 状态 + 设置页。
- **P3**：GUI 粘贴分享口令一站式（`dl-sites` crate 公开页解析，不依赖浏览器）；
  博主主页批量明确不做（抖音 a_bogus 签名成本不成比例）。

## 10. 风险与对策

| 风险 | 对策 |
|---|---|
| 平台改版破坏提取 | 三级数据源降级 + fixture 回归测试 + `probe.source` 透出健康度 |
| 风控 | 用用户自己已登录会话、用户点击触发（非批量爬取），风险与手动下载等同 |
| 视频号解密算法变更 | 测试向量对拍失败即明确报错"算法已更新"，不产出损坏文件 |
| yt-dlp 落后于 YouTube | nightly 自动更新开关；解析失败自动降级托管模式 |
| 签名/URL 短时效（抖音 302、视频号 TTL） | 即建即下（已有）；失败任务一键重试（提取器重跑） |
| 合规 | 设置页与 README 注明"仅供个人备份学习"；不提供批量账号采集 |

## 11. 文章适配层（ext 0.6.4 / app 0.4.1）—— 文章与排版下载

集成 [code-box](https://github.com/027xiguapi/code-box) 的文章下载能力：**Markdown 导出**（唯一文档格式，见 §11.2 决策）+ **文章图片批量**。选择器表来源为 code-box `contents/*.tsx`
（2026-10 校对），每站 fallback 链防改版。

### 11.1 站点覆盖与提取

| 站点 | root / content 选择器 | 特殊处理 |
|---|---|---|
| 微信公众号 | `#img-content` / `#js_content` | `data-src` 懒加载落地；剥 `visibility:hidden` 初始样式；**图片由 wechat-article 适配器专管**（mmbiz 原图规则），articles 只出文档 |
| CSDN | `.blog-content-box` / `#content_views` | 剥 `.hljs-button` 复制按钮 |
| 知乎专栏 | `article.Post-Main` / `.Post-RichTextContainer` | `data-actualsrc` 原图优先于缩放 src |
| 知乎回答（ext 0.8.6） | `.AnswerItem` / `.RichContent-inner` | 问题标题作标题、回答者作 author；视频页/无回答 id 不误命中 |
| B站专栏（ext 0.8.6） | `.article-container` / `.article-content` | `up-name` 作 author |
| 少数派（ext 0.8.6） | `.article-body` / `.article-body .content` | `time`/meta 作发布时间 |
| 掘金 | `article.article` / `.article-content` | hljs 语言探测（fenced code lang） |
| 简书 | `article` | 随机 hash class，直接取 article 容器 |
| 博客园 | `#post_detail` / `#cnblogs_post_body` | — |
| 51CTO | `article` / `.article-detail` | 剥行号列 `.pre-numbering` |
| 脚本之家 | `#article` / `#content` | — |
| php中文网 | `.phpscMain` / `.php-article` | — |
| SegmentFault | `.article-wrap` / `.article-content` | — |
| 开源中国 | `.blog-detail-container` | — |
| Medium | `article` | — |

提取管线（`adapters/articles.js`，纯提取函数，契约同 §3）：

```
root = 站点表选择器命中 → cloneNode(true)
  → site.transform（站点特有：懒加载/初始隐藏）
  → cleanup（script/style/表单控件/iframe/复制按钮删除；
             懒加载属性 data-src 族 > src；a/img 绝对化；去 srcset；
             与标题重复的正文 h1 去除）
  → content = content 选择器命中（缺省 = root）
  → 输出 document×1（Markdown）+ image×N（微信除外）
```

图片去重键 = 剥 query 的 URL（同图多尺寸只收一张，DOM 序先到先得）；
headers 带 `Referer: <站点 origin>`。

### 11.2 Markdown 导出（唯一文档格式）

> 决策（0.6.7，用户明确）：**只导出 MD，不做 HTML/Word**。此前 0.6.4-0.6.6 的
> 自包含 HTML 模板（TOC/复制按钮/暗色）已随 HTML 导出一并移除。

`ADL.buildMarkdown` 产物结构：

```
---                          ← YAML front matter（Obsidian/Hugo/Hexo 直接识别；
title: "文章标题"               值双引号包裹，内部 " 与 \ 转义）
source: "https://原文链接"
site: "CSDN"
exported: "2026-10-10"
---

# 文章标题

正文（ADL.turndown 转换，见 §11.3）
```

转换器（`adapters/turndown.js`，`ADL.turndown(root, {baseUrl})`，零依赖——code-box 用的
@joplin/turndown 是 npm 包，本扩展无构建链，等价自研子集）：
标题/段落/`<br>`/分割线/围栏代码块（`language-x`/`hljs x`/`brush:x;` 语言探测，含
```` 升级）/引用/列表（嵌套缩进按 marker 宽度对齐 + `start` 属性）/
**GFM 表格（th 的 `text-align`/`align` → `:---:`/`---:` 列对齐标记，竖线转义）**/
**`figure` → 图片 + `figcaption` → 斜体说明行**/
加粗斜体删除线/行内代码（反引号定界升级）/链接图片（相对 → 绝对，`blob:`/非 image
`data:` 丢弃）/行首结构符转义。DOM 遍历只用 `nodeType/childNodes/textContent/getAttribute`，
单测用迷你 DOM 桩完整复现（`scripts/test_articles.js`）。

**ext 0.8.6 转换与元数据增强**：

- **GFM 任务列表**：li 直属 checkbox 保留（cleanup 加 disabled，其余 input 仍删），
  `checked` 属性 → `- [x]` / `- [ ]`；
- **嵌入媒体不再丢失**：iframe（B 站/YouTube 播放器）/`<video>`/`<audio>` →
  `[▶ 视频](src)` / `[♪ 音频](src)` 链接（src 懒加载 data-src、video 取 source 子元素；
  原先在 DROP 集合被整段丢弃）；
- **`<details>/<summary>`** → 原生 HTML details 块（GFM 允许内嵌 HTML，内部正文仍走
  Markdown；原先折叠内容会被拍平）；
- **`<mark>` → `==高亮==`**（Obsidian/Typora 事实标准）；
- **front matter 新增 `author` / `published`**：站点专属选择器（微信 `#js_name`/
  `#publish_time`、博客园 `#post-date`、知乎回答 `.AuthorInfo-name` 等）→ 通用 meta
  兜底（`meta[name=author]`、`article:published_time`/`pubdate`/`itemprop=datePublished`）；
  日期归一 `YYYY-MM-DD`（支持 `2026年3月5日` / `2026/03/15` 形态），取不到省略字段。
  `ARTICLES_RE`（types.js 静态并集）与 manifest content_scripts matches 同步扩了三个新站。

### 11.4 内容任务通道（Rust）

文档是**生成内容**而非 URL，下载管线的 probe/分段/校验全不适用 → 新通道：

```
adapter-push（background）
  └─ document 资源 → nmCall("task.add_content",
        { filename, mime, content, dir?, source_url? })   ← 单帧，32 MiB 上限
     └─ dl-ipc dispatch → engine.create_content_task
          → 目录解析（显式 dir > default_dir 设置 > 系统下载目录；
                     无显式 dir 且 auto_categorize=on → 「文档」）
          → sanitize + resolve_conflict(Rename)
          → 落盘 + Task{ kind: content, status: done } 直插入库 + StateChanged 事件
```

- **帧上限分方向**（`dl-ipc`）：`read_frame` 1 MiB（host→扩展，NM 协议硬限制对齐），
  `read_request` 32 MiB（扩展→宿主；Chrome NM 该方向协议上限 4 GB）。NM shim stdin
  与管道服务端读请求均走 `read_request`；`Engine::MAX_CONTENT_BYTES` 兜底同值。
- `TaskKind::Content`：任务无下载阶段，直达 Done；`source_url`（原文页链接）记录进
  `Task.url` 供 UI 展示/跳转，缺省记 `content://<落盘路径>`（内容任务不参与续传/去重）。
- 同文重复推送 = 冲突重命名 `(1)`，不报错（文章改版后重导出常见）。
- `settings.get`：白名单暴露 `default_dir`/`auto_categorize` 给扩展（文章合集目录
  拼接用）；代理/限速/密钥类设置不经 NM 通道外泄。
- 防护：`dir` 含 `..` 段、`source_url` 非 http(s) → `InvalidParams` 拒绝。

### 11.5 文章合集模式（ext 0.6.5 体验优化）

**痛点**：auto_categorize 开启时 HTML 进「文档/」、文章图片进「图片/」——用户拿到
裂图的 HTML，"排版下载"不完整。

**方案**：文档与图片同批推送时（`adapterPush`），统一落到
`<default_dir>/<文章标题>/`：

1. `settings.get` 取 `default_dir`（失败/只推文档 → 降级普通模式，行为同 v0.6.4）；
2. `ADL.rewriteDocRefs(content, urlToName, isMarkdown)`：文档内同批图片的 URL 重写为
   **相对文件名**（HTML 兼容 `&amp;` 转义形态；Markdown 文件名含空格/括号用 `<...>`
   角链接防截断；未勾选/未命中的图片保持外链）；
3. 图片任务与文档任务都带 `dir`（`task.add` 的 `CreateTask.dir` 本就支持）；
4. **图片先推**（即建即下），文档瞬时完成殿后——用户打开文章时图片基本就绪。

**微信跨适配器一致性（0.6.6）**：微信的文档（articles）与图片（wechat-article）出自
不同适配器——HTML 内 src 是 `/640?wx_fmt=jpeg&tp=webp` 原始形态，图片任务 URL 经
normalize 为 `/0` 无 webp，按 URL 匹配重写必然 miss。修复：normalize 提为共享
`ADL.mmbizNormalize`（wechat-article 与 articles 共用），articles 侧新增站点级
`postCleanup(clone)` 钩子在懒加载落地后归一化 HTML 内 mmbiz src → 两边同形，
微信合集模式打通（端到端断言锁定）。

效果：一个文件夹 = 完整离线文章（HTML/MD + 原图），任何编辑器/浏览器打开排版如原页。
重复导出同一文章：图片 `task_exists`（任务已完成不重下）、文档 `(1)` 副本，引用仍指向
首批图片文件，合集保持可读。

**推送反馈**：完成文案带落点目录（`已保存 N 项 → 标题/`）；float 面板追加
「打开文章」按钮 = `task.open` 打开 .md（系统默认 Markdown 应用，如 Typora/VSCode），
面板延时至 8s 供点击。

**浮标精准化**：文章站点（articles 命中）在 boot 时按站点 root 选择器同步探测，
无正文节点（列表页/首页/改版失效）不注入浮标。

### 11.6 多适配器命中与入口

微信文章页同时命中 `wechat-article`（图片）与 `articles`（文档）：
`ADL.matchAdapters(url)` 返回全部命中 → bridge 按 `adapterIds[]` 逐个提取
（单个失败不拖垮整页）→ background 按 URL 合并去重（保序，wechat-article 优先）。

入口沿用四件套：popup 站点卡片（计数含"文章 n"）、悬浮按钮面板（文档行带"文档"标签，
默认勾选）、右键"下载本页媒体"（documentUrlPatterns 已扩展 12 站）、
设置页"文章"平台开关（`adaptersCfg.articles`）。

### 11.7 测试

| 层 | 方法 |
|---|---|
| turndown 转换 | 迷你 DOM 桩（makeNode/serialize/querySelector 子集，~120 行）驱动全部规则分支 |
| articles 提取 | 桩 DOM 页面（CSDN/微信/知乎/掘金）→ 断言文档内容/图片清单/清理效果 |
| 注册表一致性 | `ADL.ARTICLES_RE` 对 ARTICLE_SITES 每站 URL 交叉命中断言（防两处正则漂移） |
| 引擎内容任务 | 落盘/直 Done/冲突重命名/超限拒绝/合集目录与原文链接/`..` 穿越拒绝；**必须显式设 default_dir 或显式 dir**（缺省会写用户下载目录——教训已写进测试注释） |
| IPC | task.add_content dispatch（含 dir/source_url 透传）+ settings.get 白名单 + 帧上限方向语义（32 MiB 请求 / 1 MiB 响应） |
| 引用重写 | rewriteDocRefs：HTML `&amp;` 形态/未命中保留/MD 角链接防截断/裸空格跳过 |
| 微信跨适配器合集 | 端到端：articles HTML src 归一化 ↔ wechat-article 图片 URL 同形 → 重写命中 |

合计扩展 `test_articles.js` 87 断言；cargo 新增 9 测试（engine 5 + ipc 3 + 帧上限）。

## 12. 文章操作菜单（ext 0.7.0，对齐 code-box 微信面板）

悬浮按钮面板新增「文章操作」区（仅 articles 命中站点渲染；抖音/小红书等视频站面板不变），
`extension/actions.js` 实现 12 项操作。提取管线经 `ADL.articlePrepare` 与适配器共享
（clone/清理/正文定位只跑一次）。与 code-box 的实现差异（有意为之）：

| 菜单项 | 本实现 | code-box 原实现 |
|---|---|---|
| 获取封面 | og:image → 推送应用下载（`push` 消息，任务面板可见） | window.open 打开图片 |
| 文章摘要 | meta description（兜底首段 150 字）→ 弹窗复制 | prompt 展示 |
| 净化链接 | 微信白名单参数（__biz/idx/mid/sn/src/timestamp/ver/signature），其他站剥追踪参数（utm_*/spm/share_* 等）；**手工拆 query 保留原始编码**（URL.searchParams 会把 `MzA=` 重写为 `MzA%3D`）；弹窗复制 + 可跳转 | 同款白名单 + execCommand 复制 + 导航 |
| 原始链接 | 微信由 `__biz/mid/sn` 重构规范 URL（idx 缺省 1）；其他站 og:url/canonical 兜底 | 未实现（面板里是空占位） |
| 编辑 Markdown | 页内模态编辑器（可编辑 + 复制 + 浏览器本地下载） | 存 storage 后打开 md.randbox.top 第三方编辑器 |
| 下载 Markdown | 推送应用（task.add_content，合集目录） | Blob 保存 |
| 解析 Markdown | MD + 翻译指令 → 复制并打开 Kimi/ChatGPT/DeepSeek | 同款（storage 中转） |
| 打包下载(MD+图片) | adapter-push 全量推送 → 合集目录 `<下载目录>/<标题>/`（相对引用离线可读） | JSZip 图片 + base64 内嵌 MD |
| 下载 PDF | 干净排版 HTML 隐藏 iframe 调起打印（浏览器「另存为 PDF」） | 同款原理（Print.print） |
| 下载 Word | Word XML 命名空间包装 HTML → `task.add_content`（mime application/msword，入应用任务） | Blob 存 .doc |
| 下载所有图片 | 推送应用（引擎多线程/断点/分类） | JSZip 打包 |
| 生成海报 | **本地 canvas** 绘制分享卡（渐变底 + 封面[匿名跨域，无 CORS 自动跳过] + 标题换行 + 摘要 + 日期/品牌）→ PNG 下载 | 打开 code-box.fun 外部服务生成 |
| 文章同步 | **不实现**（发布到第三方平台超出下载器范畴） | 29 平台同步 |

通信只用既有消息（`push` / `nm` / `adapter-push` / `adapter-probe`），background 零新增；
`nm` 消息超时改为调用方可指定（大文章 add_content 5s 不够，上限 30s）。
