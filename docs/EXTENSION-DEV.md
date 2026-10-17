# 浏览器扩展开发指南（M1.5）

Chrome/Edge 的 MV3 扩展负责「眼睛和手」：嗅探资源、推送任务给主程序。
本指南覆盖开发环境的完整链路：注册 NM host → 加载扩展 → 启动主程序 → 推送下载。

界面已统一为 Anything Studio「网页收集室」，视觉与交互说明见 [UI-STUDIO.md](UI-STUDIO.md)。扩展无需前端构建；修改后在扩展管理页点击「重新加载」，再刷新已打开的网页，使浮动收集面板和文章操作也使用新版本。打包时请包含 `studio.css`、`popup.css`、`sync.css` 与 `icons/`，不可只复制 HTML 和 JS。

## 组件与职责

| 组件 | 形态 | 职责 |
|---|---|---|
| `extension/` | MV3 扩展（无构建步骤，原生 JS） | webRequest 嗅探 + popup 推送 |
| `dl-server nm-shim` | 同一二进制的 NM 模式 | Chrome 拉起的 stdio ↔ 管道薄桥 |
| `dl-server serve` | 常驻进程 | 持有引擎，执行下载 |
| `scripts/register-nm-host.ps1` | 注册脚本 | 写 HKCU 注册表 + NM manifest |

扩展 ID 由 `manifest.json` 的 `key` 固定为
**`jeigecniniligfiigmmanjeiaooiiomc`**（unpacked 加载 ID 稳定，
NM `allowed_origins` 才能写死）。

## Windows 开发链路

```powershell
# 1. 构建（NM manifest 指向 release 二进制）
cargo build --release

# 2. 注册 NM host（HKCU，无需管理员；Chrome + Edge 一起注册）
powershell -ExecutionPolicy Bypass -File scripts\register-nm-host.ps1
#    取消注册：powershell ... -Unregister

# 3. 加载扩展
#    Chrome/Edge 地址栏打开 chrome://extensions（Edge: edge://extensions）
#    → 开启「开发者模式」→「加载解压缩的扩展」→ 选择 extension/ 目录

# 4. 启动主程序（常驻）
.\target\release\dl-server.exe serve

# 5. 浏览器打开任意含大媒体文件的页面 → 工具栏扩展图标出现角标数字
#    → 点开 popup → 「推送」→ 提示「已创建任务」
#    → 另一终端查看并下载：
.\target\release\dl-cli.exe list
.\target\release\dl-cli.exe download <任务ID> --segments 8
```

NM manifest 写在 `%APPDATA%\anything-downloader\nm\anything_dl.json`，
Chrome **每次连接时**解析——改 manifest/重编二进制后无需重启浏览器。

## macOS 开发链路

```bash
cargo build --release

# NM host manifest（用户级，免管理员）
mkdir -p "$HOME/Library/Application Support/anything-downloader/nm"
cat > "$HOME/Library/Application Support/anything-downloader/nm/anything_dl.json" <<EOF
{
  "name": "anything_dl",
  "description": "Anything Downloader native messaging host",
  "path": "$(pwd)/target/release/dl-server",
  "type": "stdio",
  "allowed_origins": ["chrome-extension://jeigecniniligfiigmmanjeiaooiiomc/"]
}
EOF

# 注册（Chrome 与 Edge 路径分别指向同一 manifest）
for d in "Google/Chrome" "Microsoft Edge"; do
  mkdir -p "$HOME/Library/Application Support/$d/NativeMessagingHosts"
  cat > "$HOME/Library/Application Support/$d/NativeMessagingHosts/anything_dl.json" <<EOF
"$HOME/Library/Application Support/anything-downloader/nm/anything_dl.json"
EOF
done

# 其余同 Windows：加载 unpacked 扩展 → ./target/release/dl-server serve → 推送
```

## 权限与安全边界（当前版本）

- `host_permissions: <all_urls>` + `extraHeaders`：嗅探请求头（Cookie/Referer
  在 Chrome 72+ 默认脱敏，必须显式声明才可见）。这是商店审核的重点项，
  上架时需提供权限用途说明 + 隐私政策（DESIGN §13 风险表）。
- 扩展事实持有用户全站登录态（能读到 Cookie 头）——推送的头经 IPC 进主程序时
  只允许白名单字段（Referer/User-Agent/Cookie/Authorization/Accept/Accept-Language），
  其余在 `dl-engine` 入队时被剔除，序列化后 ≤16KB。
- `dl-server nm-shim` 会校验拉起来源必须为 `chrome-extension://` 形态；
  manifest 的 `allowed_origins` 精确到扩展 ID。

## 接管浏览器下载（0.2.0）

popup 中的「接管浏览器下载」开关（默认关，存 `storage.local`）。开启后：

1. `chrome.downloads.onCreated` 命中规则（媒体/压缩包/文档扩展名，且已知大小 ≥1MB）
   → 先 `pause()` 留退路
2. 请求头优先取嗅探缓存（可能含 Cookie），其次近期请求头快照（无 Cookie，匿名下载）
3. 推送主程序成功（task.add + task.start）→ `cancel()` + `erase()` 清下载栏条目
4. **推送失败（主程序未运行/NM 未注册）→ 自动 `resume()` 放行浏览器原生下载**，绝不吞下载

已知限制（DESIGN §7.3 的取舍）：
- 非媒体直链若需登录态（Cookie），匿名下载会 403 失败——任务会以
  `auth_expired`/`url_expired` 显示在 GUI；此类链接请关闭接管或用嗅探推送
- 浏览器已写入 `.crdownload` 的部分不可复用，接管即整体重下
- `downloads.onCreated` 触发时请求已发出，极小文件可能在 cancel 前完成
  （此时 erase 清条目，文件留在原目录）

## 0.2.3 popup 任务控制

popup 双视图：**本页资源** / **主程序任务**。
- 主程序任务页：运行中任务实时进度（1s 轮询）、暂停/取消/开始/重试/打开
  （经 NM `task.list` / `task.pause` / `task.cancel` / `task.start` / `task.retry` /
  `task.open`，后端 `dl-ipc` 全部原生支持）
- 接管记录（最近一条）与「忽略此站点」：忽略后该站点下载不再接管，
  popup 可随时恢复（`storage.local.ignoredHosts`）

## 0.2.2 扩展完善

- **SW 回收不再丢嗅探列表**：列表 500ms 防抖写入 `storage.session`（10MB 配额；
  单页上限 100 条丢最旧；超限降级只保 m3u8 清单），SW 唤醒时合并式恢复
- **NM 长连接 port**：SW 持有 `connectNative` port（此前每条消息 spawn 一个
  shim 进程），断链自动重连、挂起请求 8s 超时兜底
- **接管取消复核**：`cancel()` 后经 `onChanged` 确认 `USER_CANCELED` 才算接管；
  竞态下载已完成（`complete`）则 `removeFile + erase` 清条目
- 推送反馈细分：`已开始下载 / 已创建任务 / 该链接已有进行中的任务 / 失败原因`
- 每条资源带「复制」按钮

## 已知边界（按里程碑解锁）

- SW 回收后嗅探列表丢失（M1.5 接受；M4 用 `storage.session` 治理 + 500ms 防抖）
- `sendNativeMessage` 每条消息拉起一个 shim 进程（M4 换 SW 持有 port 长连接）
- 嗅探大小为「已观测值」：内存缓存命中的请求 webRequest 不可见，实际大小以主程序探测为准
- POST 下载不嗅探（body 不可靠，DESIGN §7.2）

## 无浏览器验证（CI）

`crates/dl-server/tests/nm_shim.rs` 覆盖浏览器之外的全链路：
以 Chrome 的拉起形态（`dl-server <origin>`）spawn shim，经 NM 帧发 `ping` /
`task.add`（带透传头），断言：任务入库（白名单外的头被剔除）→ 引擎下载字节一致
→ 主程序退出后 push 得到明确的 `app_not_running` 错误。
