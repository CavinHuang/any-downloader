# Anything Downloader

高性能网页资源下载管理器。设计文档见 [docs/DESIGN.md](docs/DESIGN.md)。

## 结构（按能力拆分，一能力一 crate）

| crate | 能力 |
|---|---|
| `crates/dl-error` | 错误分类（稳定码表 + i18n key + 可重试判定） |
| `crates/dl-model` | 任务领域模型与状态机迁移表（纯规则，无 IO） |
| `crates/dl-persist` | 任务持久化：SQLite WAL + 专属写线程 |
| `crates/dl-throttle` | 层级令牌桶限速（全局→任务） |
| `crates/dl-journal` | 崩溃安全进度 journal（双缓冲 + xxh64） |
| `crates/dl-segment` | 分段模型与覆盖不变量 |
| `crates/dl-filename` | 文件名净化 / RFC 5987 / 冲突策略 |
| `crates/dl-net` | HTTP 客户端构建（identity + 代理）与探测决策表 |
| `crates/dl-io` | 单写者落盘管线 |
| `crates/dl-engine` | 下载驱动编排 + 引擎句柄 + 事件流 |
| `crates/dl-testserver` | Range 静态测试源（基准 / 崩溃演练） |
| `crates/dl-cli` | 引擎 CLI |
| `crates/dl-server` | 本地 IPC 服务（命名管道 / UDS，NM shim 在 M1.5） |

依赖单向：`dl-engine → 各能力 crate`；边界由编译器强制。`src-tauri` / `ui` / `extension` 按里程碑在 M3/M4 引入。

## 当前进度：v0.2.0 —— 四大核心能力全部落地

- [x] **接管浏览器下载**（0.2.0）：downloads.onCreated → pause → 推送（复用嗅探头缓存）
      → cancel+erase；推送失败自动放行浏览器下载。popup 开关，默认关
- [x] 版本 0.2.0（安装器/便携/GUI/扩展同步）

- [x] 残留清理：删除任务自动清理 journal / .dlpart / HLS 临时目录（绝不碰成品）
- [x] ffmpeg_path 设置（查找链首位）+ GUI 配置项；HLS `.ts` 降级时详情页提示
- [x] 完成行「打开」按钮（文件存在开文件，否则开目录）
- [x] 可执行文件完成通知强提示来源（DESIGN §9）
- [x] 开机自启（autostart 插件 + 设置开关）
- [x] 托盘退出前优雅暂停全部运行任务（journal checkpoint 不丢进度）
- [x] **M8 内存实测：空闲主进程 RSS 46.1MB（目标 <60MB ✓）；进程树 ~187MB（WebView2 底座固定开销，超 120MB 预期，记录在案）**
- [x] 任务栏进度条 + 标题栏总速度
- [x] 失败页签「全部重试」、完成页签「清空记录」
- [x] 失败自动重试（可配次数，仅网络类错误，指数退避）
- [x] 最大并发任务数上限（0 = 不限）
- [x] 设置页「打开日志文件夹」
- [x] 扩展右键菜单：链接/媒体/图片一键推送（badge 反馈），popup UI 精修（深色/彩色标签/状态胶囊）
- [x] 安装器随附 dl-cli / dl-server（externalBin sidecar，注册脚本优先已安装位置）
- [x] 万级任务扩展性：GUI 轮询改增量拉取（updated_at 增量，不全量刷表）
- [x] 重复 URL 去重（task_exists 错误码，GUI 友好提示）
- [x] GUI 设置页显示扩展通道注册状态（winreg 检测）
- [x] 列表排序（名称/速度/大小）、暂停后真实进度显示、删除确认、剪贴板去重、版本号显示
- [x] **修复「暂停后点开始无反应」**：Engine::pause/cancel 对运行中任务走 driver
      权威路径（此前 CLI/IPC 暂停只改 DB 不停 driver，导致后续启动被
      already-running 拒绝且前端吞错）；download 对收尾中的旧实例等待后重开；
      前端操作错误行内提示；CLI 控制命令 IPC 优先（跨进程控制 GUI 任务）
- [x] **HLS 清晰度选择**：详情页列出 master 变体（分辨率/带宽），一键换清晰度重下
- [x] **自动分类目录**：设置开启后按扩展名归类（视频/音乐/压缩包/文档/软件/图片）
- [x] 修复：设置页黑屏（React Hooks 顺序违例 + 空指针初始化）；新增 ErrorBoundary
      （渲染崩溃显示错误面板而非黑屏）；清理重复的目录输入框

### 交付物

| 产物 | 说明 |
|---|---|
| `target/release/bundle/nsis/Anything Downloader_0.2.1_x64-setup.exe` | 安装器（中英双语，随附 dl-cli/dl-server） |
| `target/release/dl-app.exe` | 便携版 GUI（即常驻主程序） |
| `target/release/dl-cli.exe` / `dl-server.exe` | 无头 CLI / NM shim |

### M5/M7 收口

- [x] 任务重命名（引擎 + CLI + GUI 详情抽屉；非活跃态、自动净化）
- [x] 任务列表导入/导出（JSON，不含敏感头）
- [x] 完成后动作：无 / 打开文件 / 打开文件夹（设置可选）
- [x] 剪贴板监听：新直链 → 顶栏提示一键下载（设置可关）
- [x] 设置页：目录浏览选择器
- [x] M5 稳定性：GUI 日志按天轮转落盘（数据目录/logs）、panic 崩溃日志（crash/）
- [x] M6 分发：NSIS 安装器（`cargo tauri build` 产出 bundle/nsis/*.exe）

### M2（HLS 管线）→ 全功能可用

- [x] HLS VOD：m3u8 解析（master 自动选最高码率）、BYTERANGE、EXT-X-MAP、音轨延续
- [x] AES-128 解密（显式 IV / 序列号推导，PKCS7 + 原始回退）
- [x] 分片 16 并发池 + 分片级断点（文件即 journal，续传零成本）
- [x] 签名失效自动重拉清单；直播流明确拒绝（live_unsupported）
- [x] 合并：fMP4 原始拼接（无需 ffmpeg）；TS → ffmpeg remux mp4，缺失降级 .ts
- [x] ffmpeg 查找：ANYTHING_DL_FFMPEG > 应用目录 > PATH
- [x] `.m3u8` 直链自动识别为 HLS 任务
- [x] GUI：完成系统通知、打开文件夹、任务搜索、HLS 分片进度显示

**真实站点验证**：mux 公开测试流（x36xhzz，master→64 分片 TS）完整下载，
中途强杀后断点续传至完成，产物 ffprobe 识别为合法 mpegts（时长 634s）。

### M3（桌面 GUI）

- [x] Tauri v2 壳：进程内直调引擎（命令 + 100ms 事件快照，无 WS、无 block_on）
- [x] GUI 即常驻主程序：内嵌 IPC 管道，浏览器扩展推送直达 GUI 进程
- [x] 任务列表：虚拟滚动（固定行高窗口化）、实时进度/速度、状态过滤页签
- [x] 新建任务：多 URL 粘贴 + 识别（探测文件名/大小/MIME/是否可多线程）+ 批量添加
- [x] 任务操作：开始/暂停（协作 checkpoint）/取消/重试/删除 + 详情抽屉（错误分类）
- [x] 设置：默认目录、分段数、单任务/全局限速、代理、冲突策略（引擎设置 KV）
- [x] 托盘常驻：关窗隐藏、左键/菜单恢复、退出；单实例（二次启动聚焦首实例）
- [x] i18n：zh-Hans / en

运行：

```bash
cd ui && npm install && npm run build   # 产物 ui/dist
cargo build --release -p dl-app
./target/release/dl-app.exe             # GUI 即主程序；扩展推送直达
```

### M1.5（最小扩展通道）

- [x] 请求头端到端：扩展嗅探头 → 任务（白名单过滤 + 16KB 上限）→ 下载请求携带
- [x] `dl-server nm-shim`：Chrome 拉起形态的 NM stdio ↔ 管道薄桥（stdout 纯净、断链自动重连、主程序不在线返回 `app_not_running`）
- [x] MV3 扩展最小版：URL 预筛 → `extraHeaders` 抓头 → popup 推送（固定扩展 ID）
- [x] NM host 注册脚本（HKCU，Chrome+Edge）+ macOS 手册
- [x] 全链路集成测试（浏览器之外：serve + shim + NM 帧 → 引擎下载字节一致）

开发链路见 [docs/EXTENSION-DEV.md](docs/EXTENSION-DEV.md)。发布与自动更新见 [docs/RELEASE.md](docs/RELEASE.md)。

## 站点适配（v0.4.0 / 扩展 v0.6.0）

通用引擎之上的专用媒体下载入口（设计详见 [docs/MEDIA-ADAPTERS.md](docs/MEDIA-ADAPTERS.md)）：

| 平台 | 能力 | 实现 |
|---|---|---|
| 抖音 | 无水印视频（多清晰度）/图集、作者名文件名；**粘贴链接解析**；app 侧直链经 yt-dlp | hook 抓取 API 响应 → `iesdouyin` 直链（移动端 UA，video_id 去重） |
| 小红书 | 笔记图片（无水印）/视频/Live 图 | API hook + `__INITIAL_STATE__` 双源合并（SPA 站内导航也新鲜） |
| X (Twitter) | 照片原图/视频多码率（默认最高）；**粘贴链接免登录解析**（syndication 端点） | GraphQL 响应直链 mp4 + `name=orig`；详情页只收当前推文 |
| 微信文章 | 文章全部原图批量 | DOM 懒加载 `data-src` + 参数重写（Referer 防盗链） |
| 微信视频号 | **加密视频自动解密** | hook 抓 URL + decode_key → 引擎下载后 Isaac64-XOR 解密前 128KB（算法与官方样本逐位对拍） |
| YouTube / **X / Bilibili / TikTok / 抖音** | 视频/音频下载 | yt-dlp sidecar 托管模式（`TaskKind::Ytdlp`，URL 自动识别覆盖五平台；**安装器已内置 yt-dlp + deno，开箱即用**，设置里可覆盖路径） |
| **文章站点 ×12** | **文章下载（Markdown）+ 文章图片 + 操作菜单** | 微信公众号/知乎专栏/掘金/CSDN/简书/博客园/51CTO/脚本之家/php中文网/SegmentFault/开源中国/Medium；自研 GFM 转换器（YAML front matter / 代码块语言探测 / 表格列对齐 / figure 图注）；**文章合集模式**：`下载目录/标题/` 统一存放 .md 与原图，相对引用离线完整可读；**文章操作菜单**（对齐 code-box）：获取封面/文章摘要/净化链接/原始链接/编辑 Markdown/解析 Markdown/打包下载/下载 PDF/下载 Word/下载所有图片/生成海报；文档经 `task.add_content` 直达已完成任务，图片走常规下载管线 |

入口：popup 站点卡片、**粘贴分享口令**（抖音/小红书/X 的"复制链接"直接粘）、页面悬浮 ⬇ 按钮、右键"下载本页媒体"、设置页按平台开关。
仅供个人备份学习使用，请遵守各平台服务条款。

## 已完成里程碑

### M1（HTTP 引擎核心）

- [x] M0：workspace 脚手架、状态机、SQLite、管道 IPC、CI
- [x] M1：URL 探测决策表（206/200/416/总长未知）
- [x] M1：单写者线程管线（规避 Windows 共享文件指针竞态；块合并顺序定位写）
- [x] M1：崩溃安全 journal（双缓冲 A/B + xxh64 + sequence，恢复取最大合法序；
      检查点顺序 sync(数据)→写 journal→sync(journal)，崩溃零丢失、断电最多重下 2s 窗口）
- [x] M1：分段下载 + 慢段分裂（护栏：≥32MB/10s 起评、最小半段 4MB、单段 ≤3 次、5s 窗口）
- [x] M1：If-Range 强 ETag 续传；内容变更自动检测并整体重下
- [x] M1：`.dlpart` 原子 rename 收尾 + 冲突策略（rename/overwrite/skip/ask）
- [x] M1：文件名 sanitize（Windows 保留名/非法字符/路径剥离/RFC 5987）
- [x] M1：完成强制校验（覆盖完整性 + 长度 + 随机 4KB 抽检）
- [x] M1：层级令牌桶限速（全局→任务，按流逝时间补充）
- [x] M1：代理（http/socks5/socks5h，未配置时跟随系统）
- [x] M1：错误分类（DNS/超时/429/403 鉴权/磁盘满…）+ Retry-After 退避
- [x] 集成测试 6 项 + 单元测试 35 个，clippy `-D warnings` 全绿

**验收结果（本机实测）**：本地源 2GB / 8 段 ≈ **118 MB/s**（千兆线速）；
kill -9 中途强杀 → journal 恢复续传 → sha256 与源一致。

### M1 验收命令

```bash
cargo build --release
./target/release/dl-cli.exe test-server big.bin 18080          # 终端 A：Range 静态源
./target/release/dl-cli.exe add http://127.0.0.1:18080/file --dir ./out
./target/release/dl-cli.exe download <id> --segments 8         # 终端 B：下载
# 中途 taskkill /F /IM dl-cli.exe 再重新 download，即演练崩溃恢复

## 快速开始

```bash
# CLI（直接读写 SQLite）
cargo run -p dl-cli -- add https://example.com/file.zip
cargo run -p dl-cli -- list
cargo run -p dl-cli -- show <id>
cargo run -p dl-cli -- cancel <id>
cargo run -p dl-cli -- retry <id>

# IPC 端到端（终端 A 起服务，终端 B 调用）
cargo run -p dl-server -- serve
cargo run -p dl-server -- call ping
cargo run -p dl-server -- call task.list
cargo run -p dl-server -- call task.add '{"url":"https://example.com/a.zip"}'

# 测试
cargo test
```

数据目录：`%APPDATA%\anything-downloader`（Windows）/ `~/Library/Application Support/anything-downloader`（macOS），可用 `ANYTHING_DL_DATA_DIR` 覆盖。
