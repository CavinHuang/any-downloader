# 桌面嗅探（微信视频号）设计与运维

> 状态：**v1 已实现**（app 0.5.0）。浏览器之外的桌面应用（首期：微信电脑版
> 视频号）没有扩展钩子可用，本方案用「本地代理 + 定向解密」补齐同款体验：
> 微信里刷视频号 → 视频窗口右上角出现 ⬇ 悬浮图标 / 主界面「桌面嗅探」列表
> 出现条目 → 一键下载（自动 isaac64-xor 解密，与浏览器 channels 适配器共用
> 引擎 postprocess 管线）。

## 1. 为什么是「本地代理 + 证书」

- 微信桌面端的视频号由内置 Chromium 进程（`WeChatAppEx.exe`）渲染，遵循
  系统代理/PAC——这是唯一不注入进程就能看到其流量的通道。
- 当年浏览器侧否决代理方案（docs/MEDIA-ADAPTERS.md 路线表 D）的前提是
  「浏览器有 hook 可用」；桌面端没有，方案在此重新成立。
- 与生态内同类工具（Charles/Fiddler 抓视频号、各类"视频号助手"）同一原理，
  差异在本方案把解密范围与路由范围都压到最小（见 §3）。

## 2. 架构与数据流

```
微信(WeChatAppEx, Chromium)
   │  HTTPS（PAC：仅 video.qq.com / channels|finder.weixin.qq.com 走代理）
   ▼
dl-sniff 代理（127.0.0.1:9463，crates/dl-sniff）
   ├─ CONNECT → 预读 ClientHello 解析 SNI
   ├─ SNI 命中白名单 → 本地 CA 签叶子证书 MITM：
   │     finder API JSON ──► decode_key / 直链 / 标题
   │     finder CDN mp4   ──► 直链 + 请求头 + Content-Length
   │     （capture.rs：key 120s TTL 与最近媒体配对 → SniffedItem）
   └─ SNI 未命中 ──► 纯 TCP 透传，不解密任何字节
   ▼
SniffState（src-tauri/src/sniff.rs，条目环形 50 条）
   ├─ broadcast → "sniff-event" → 主窗口 SniffPanel + sniff-overlay 悬浮窗
   ├─ sniff_download / sniff_overlay_download → CreateTask(postprocess=isaac64-xor)
   │     → 引擎常规管线（多线程下载 → Decrypting → Done）
   └─ wechat_overlay.rs：250ms 轮询 EnumWindows 找视频号宿主窗 →
      sniff-overlay 窗口钉在右上角（right-52/top+12，不抢焦点）
```

代码地图：`crates/dl-sniff`（sni/ca/proxy/capture）· `src-tauri/src/{sniff,sysproxy,cert,wechat_overlay}.rs` · `ui/src/components/SniffPanel.tsx` + `ui/src/sniff-overlay.ts`。

## 3. 安全边界（设计不变量）

1. **解密白名单最小化**：仅 `channels.weixin.qq.com`、`finder.video.qq.com`、
   `*.video.qq.com`（`SniffConfig::new`，M1 实测校准）。其余域名一律 TCP
   透传，不解密任何字节——代理对非白名单流量只是"更近一点的网线"。
2. **PAC 而非全局代理**：只把上述域名路由进本代理，其余网站 DIRECT。
   reqwest/引擎不读 AutoConfigURL，天然无回环；上游客户端显式 `no_proxy`。
3. **证书 CurrentUser 级**：安装/卸载免管理员（Crypt32，mkcert 同款），
   仅影响当前用户；设置页一键卸载 + 删除 `%APPDATA%/anything-downloader/sniff/`
   即完全撤销。
4. **崩溃安全**：启用时把原 AutoConfigURL 备份进 `sniff/pac-active.json`
   flag 文件；启动见 flag 即还原，再按设置决定是否重新启用；托盘退出亦还原。
5. **默认关闭**：`sniff_desktop` 默认 off；开启前置检查证书已装。

## 4. 启用流程（用户视角）

设置 → 05 桌面嗅探（视频号）→ 「安装证书」（一次性）→ 打开开关。之后在
微信电脑版播放视频号视频：主界面 masthead 雷达图标亮角标，「桌面嗅探」列表
出现条目；视频窗口右上角出现 ⬇ 悬浮图标，单击下载最近一条。

## 5. 故障排查（M1 校准手册）

微信版本更新后若捕获失效，按序检查：

1. **流量有没有进代理**：日志（`data_dir/logs/dl.log`）过滤 `CONNECT`——
   每条隧道都记录 `host/sni/decrypt`。刷视频号时若无任何 qq.com 相关行 →
   PAC 未生效（重启微信；或检查 `HKCU\...\Internet Settings\AutoConfigURL`）。
2. **SNI 白名单是否需要扩**：看 CONNECT 行里出现的新域名（如媒体 CDN 换域），
   加进 `SniffConfig` 的 `decrypt_exact/decrypt_suffix`。
3. **直链形态是否变化**：`is_media_url`（`.mp4/.m3u8` 后缀）与
   `scan_api_json`（`decode_key`/标题/URL 字段名）按真实响应调整。
4. **微信硬编码不走系统代理**（Chromium 被定制裁剪的最坏情况）：PAC 与全局
   模式都试过仍无流量 → 该微信版本不适用本方案，回退到浏览器
   channels.weixin.qq.com 适配，并在本文档记录版本号。

证书排障：`certmgr.msc` → 受信任的根证书颁发机构 → `Anything Downloader
Sniff CA`。

## 6. 明确不做 / 后续

- macOS 桌面嗅探（Windows-only，代码 cfg 隔离）。
- 全局系统代理模式（PAC 已够；全局会引入回环治理成本）。
- 微信内嵌浏览器（公众号文章）的桌面捕获——浏览器扩展已覆盖。
- 捕获条目持久化（当前内存环形 50 条，重启即清）。
