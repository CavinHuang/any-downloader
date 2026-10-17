# 浏览器扩展安装教程（Chrome / Edge）

Anything Downloader 的浏览器扩展负责「浏览时顺手收下」：嗅探网页里的视频、音频、图片与直链，一键推送到桌面端开始下载；还提供右键推送、接管浏览器下载、文章收藏室（微信/知乎/CSDN 等文章转 Markdown）。

扩展未上架应用商店，采用「**本地文件 → 开发者模式加载**」的方式安装，全程约 2 分钟，无需命令行、无需管理员权限。**扩展文件已随桌面端安装在软件目录**——装好桌面端后，在应用内点设置里的「安装浏览器扩展」按钮，按弹窗指引操作即可；也可以手动下载扩展包（见文末备选方式）。

```
浏览器扩展 ──(Native Messaging 本机通道)──> Anything Downloader 桌面端 ──> 下载引擎
```

扩展与桌面端通过本机通道（Native Messaging）通信，**桌面端必须处于运行状态**（关窗后缩到托盘即算运行）才能接收推送。

## 准备

| 项 | 要求 |
| --- | --- |
| 桌面端 | **v0.4.3 及以上**（该版本起扩展通道自动注册）。旧版请先在 设置 → 关于与更新 里「检查更新」，或到 [Releases](https://github.com/CavinHuang/any-downloader/releases/latest) 重新下载安装包 |
| 浏览器 | Chrome 或 Edge（其他 Chromium 内核浏览器见文末附录）。Firefox 暂不支持 |
| 系统 | Windows 10/11 · macOS(Apple Silicon) · Linux x64 |

## 第 1 步：安装桌面端，并启动一次

1. 从 [Releases](https://github.com/CavinHuang/any-downloader/releases/latest) 下载并安装桌面端（Windows 为 `x64-setup.exe`；macOS 未签名，首次启动需右键 → 打开）。
   **扩展文件会随桌面端一起安装进软件目录**（Windows 安装版位于安装目录下的 `extension\` 文件夹，macOS 在 `Anything Downloader.app` 内），无需单独下载。
2. **启动一次** Anything Downloader。启动时会自动完成扩展通道注册（Chrome 与 Edge 同时注册，用户级注册，无需管理员）。
3. 验证：应用内打开 **设置**，找到「浏览器扩展通道」，显示 **已注册 ✓** 即成功。

> 如果显示「未注册」：确认桌面端已升级到 v0.4.3+ 并重启一次；仍不行通常是安装目录里的 `dl-server.exe` 缺失（被杀毒软件误删等），重装安装包即可修复。

## 第 2 步：在桌面端打开「安装浏览器扩展」指引

桌面端内置了安装向导弹窗，跟着走即可：

1. 桌面端打开 **设置**，在「浏览器扩展通道」一行点 **「安装浏览器扩展」**。
2. 弹窗里列出了完整步骤和扩展文件夹的实际位置；点 **「打开文件夹」** 直达该目录，点 **「复制路径」** 方便稍后粘贴。
3. 按弹窗指引在浏览器中完成加载（下一步），或点 **「完整教程与常见问题」** 打开本文档。

## 第 3 步：在浏览器中加载扩展

**Chrome：**

1. 地址栏输入 `chrome://extensions` 回车。
2. 打开右上角的 **开发者模式** 开关。
3. 点击左上角 **加载已解压的扩展程序**，选中第 2 步弹窗给出的扩展文件夹（选到含 `manifest.json` 的那层；Windows 安装版即安装目录下的 `extension` 文件夹）。
4. 列表中出现「Anything Downloader」即安装成功。

**Edge：**

1. 地址栏输入 `edge://extensions` 回车。
2. 打开左下角 **开发人员模式** 开关。
3. 点击 **加载解压缩的扩展**，同样选中该扩展文件夹。

首次加载时浏览器可能提示「新增扩展权限」（读取网站内容等）——扩展需要这些权限来嗅探媒体与透传下载请求头，点击允许即可。所有数据只在本机流转，无账号、无云端。

## 第 4 步：验证可用

1. 工具栏出现 Anything 图标（若被折叠，点拼图图标把它固定住）。
2. 打开任意含视频/大文件的网页，扩展图标出现**角标数字**（嗅探到的资源数）。
3. 点开 popup：右上角状态应为 **已连接**；「本页资源」列表中点某条资源的 **推送**，桌面端随即出现任务并开始下载。
4. 也可以在网页里**右键**图片/链接/视频，选择「推送到 Anything Downloader」。

> popup 显示「未运行」？→ 确认桌面端在运行（托盘里有图标）；再确认设置里通道为「已注册 ✓」；然后重启浏览器再试。详见下方常见问题。

## 安装后怎么用（速览）

- **popup**：「本页资源」逐条推送或全部推送；「主程序任务」实时查看运行中任务，可暂停/继续/打开。
- **右键菜单**：链接 / 图片 / 视频/音频 一键推送，角标反馈结果。
- **接管浏览器下载**（popup 内开关，默认关）：开启后浏览器里触发的视频、压缩包等大文件下载会转交桌面端（推送失败会自动放行浏览器原生下载，不会吞下载）。
- **文章收藏室**：在微信公众号、知乎、CSDN、掘金、简书等文章页，浮动按钮可把文章转成 Markdown 收藏/同步。
- 复制的下载直链也会被桌面端安静识别，顶栏提示一键下载（可在设置关闭）。

## 常见问题

**popup 一直显示「未运行 / 主程序未连接」**
按顺序检查：① 桌面端是否在运行（关窗 ≠ 退出，托盘里有图标才算在运行；完全退出过就再启动一次）；② 设置 →「浏览器扩展通道」是否「已注册 ✓」；③ 都正常仍连不上时**重启浏览器**（让扩展重建通道）。

**设置里显示「未注册」**
桌面端 v0.4.3+ 启动一次即自动注册。若已是新版本仍未注册：重装桌面端安装包修复（安装目录 `dl-server.exe` 缺失时通道无法注册，多为杀毒软件误删，可将其加入白名单）。

**chrome://extensions 里扩展显示「错误」或加载失败**
- 确认选中的文件夹里有 `manifest.json`（是 `extension` 目录本身，不是它的上级）。
- 扩展文件夹被移动/重命名/删除后原路径失效：重新在桌面端打开「安装浏览器扩展」弹窗核对路径，再「加载已解压」一次。

**怎么更新扩展到新版本？**
桌面端通过应用内自动更新升级后，软件目录里的扩展文件会同步更新——只需在 `chrome://extensions` 里点该扩展卡片上的「重新加载」↻。一般与桌面端一起更新即可。

**Chrome 和 Edge 都能用吗？装几次？**
通道注册对 Chrome 与 Edge 同时生效；但扩展需要在**每个浏览器里各加载一次**（都指向软件目录里同一个 `extension` 文件夹即可）。

**支持 Firefox 吗？**
暂不支持（扩展背景页使用了 MV3 service worker，Firefox 尚未完整支持）。

**想卸载扩展 / 桌面端？**
扩展：`chrome://extensions` 里「移除」。桌面端：正常卸载即可（软件目录里的扩展文件一并清除）；残留的通道注册键无害（找不到主程序时推送自动失败并提示），介意的话可删除注册表 `HKCU\Software\Google\Chrome\NativeMessagingHosts\anything_dl` 与 `HKCU\Software\Microsoft\Edge\NativeMessagingHosts\anything_dl`（macOS/Linux 为对应浏览器目录下的 `anything_dl.json`）。

**隐私与安全**
扩展持有 `<all_urls>` 权限仅用于嗅探媒体直链与透传下载所需请求头（Cookie/Referer 等），推送数据只进入本机桌面端，不上传任何服务器；详见 [EXTENSION-DEV.md](EXTENSION-DEV.md) 的「权限与安全边界」。

## 附录：非 Chrome/Edge 浏览器与其他系统

通道注册覆盖以下浏览器的用户级目录（桌面端每次启动自动刷新）：

| 系统 | 已自动注册 | 其他 Chromium 内核浏览器 |
| --- | --- | --- |
| Windows | Chrome、Edge | 手动把 `%APPDATA%\anything-downloader\nm\anything_dl.json` 路径写入对应浏览器注册表 `NativeMessagingHosts\anything_dl` 默认值 |
| macOS | Chrome、Edge | 把同一 manifest 复制到该浏览器 `~/Library/Application Support/<浏览器>/NativeMessagingHosts/` |
| Linux | Chrome、Edge、Chromium | 把 manifest 复制到 `~/.config/<浏览器>/NativeMessagingHosts/` |

macOS / Linux 用户注意：桌面端安装形态不同，扩展目录与注册的 `dl-server` 路径也不同（dmg 安装在 `/Applications` 下应用包内，AppImage 在挂载点内）——都由桌面端启动时自动处理，「安装浏览器扩展」弹窗给出的就是当前实际路径，无需手动干预。

## 备选方式：手动下载扩展包

不方便用应用内向导（旧版桌面端、便携版、或想把扩展放在自选目录）时，也可以手动下载：

1. 下载扩展包（固定取最新版）：**[anything-downloader-extension.zip](https://github.com/CavinHuang/any-downloader/releases/latest/download/anything-downloader-extension.zip)**（也在对应版本的 Release 资产列表里）。
2. 解压到一个**长期保留的目录**（例如 `D:\Anything\extension`）。⚠️ 不要随手删——浏览器加载的是这个目录本身，目录被删/移动扩展就会失效。
3. 其余步骤同上（开发者模式 → 加载已解压的扩展程序 → 选中解压出的文件夹）。

---

开发者（从源码加载、改代码、跑测试）请看 [EXTENSION-DEV.md](EXTENSION-DEV.md)。
