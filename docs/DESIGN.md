# Anything Downloader — 设计文档

> 版本：v0.3（2026-09-29，M2/M3 落地后修订）
> v0.1 初稿；v0.2 经三路专项审查（下载引擎 / 浏览器扩展 / 产品·安全·跨平台）修订，吸收全部 P0 修正与 P1 缺口补齐。
> 定位：高性能网页资源下载管理器，性能与体验第一。

## 1. 目标与范围边界

四大核心能力：

1. **网站资源自动嗅探** — 扩展监听页面网络请求，识别可下载资源（视频/音频/图片/文档）。
2. **接管浏览器下载** — 命中规则的下载由本应用接手。
3. **指定链接下载** — 粘贴 URL，自动探测文件信息，多线程下载。
4. **多线程并发下载** — 单任务 Range 分段 + 慢段分裂，全局任务/连接调度。

### v1 明确不支持（边界，UI 明示而非静默失败）

| 项 | 处置 |
|---|---|
| 直播型 HLS（playlist 无 `#EXT-X-ENDLIST`） | 报错「首版不支持直播流」，建议用户停止 |
| DASH/MPD（B站等） | 嗅探面板可见但标注「暂不支持」，二期实现 |
| MSE 分片完整重构 | v1 降级为「清单/直链收集器」：抓 m3u8/直链；完整逆向聚合二期 |
| DRM 内容（Widevine/EME 与 SAMPLE-AES） | 不可能下载/明示不支持，UI 报 drm_unsupported |
| POST 发起的下载、BT/磁力、FTP、Firefox | 不支持，文档声明 |

### 目标市场与语言

首版中文（zh-Hans）+ 英文（en）。i18n 从 M0 贯穿：前端 react-i18next，Rust 侧错误消息全部 key 化。

### 性能指标（v0.2 修订：定义测量边界）

| 指标 | 目标与测量条件 |
|---|---|
| 吞吐 | 单任务、本地不限速源、关闭限速：≥ 测试源上限的 95%（回环测试仅验证「引擎不是瓶颈」，MB 为十进制） |
| 内存 | **主进程（不含 WebView 渲染进程）RSS < 60MB；整进程树 < 120MB**；空载为独立验收项 |
| 任意文件大小 | 流式管道，内存占用恒定 |
| 万级任务列表 | 虚拟滚动 60fps（合成帧直方图采集） |
| 进度上报延迟 | < 100ms |
| 崩溃恢复 | **进程崩溃（kill -9/panic）：已写盘字节零丢失；断电：最多重下 min(2s × 当前速率, 256MB)**，且绝不产生数据空洞 |

## 2. 技术选型

| 项 | 选择 | 说明 |
|---|---|---|
| 引擎语言 | Rust + tokio | |
| 应用壳 | Tauri v2 | 托盘/自启/通知/单实例/updater 均用官方插件 |
| 前端 | React 18 + TypeScript + Vite | |
| 扩展 | MV3 TypeScript | Chrome + Edge 一套代码 |
| HTTP 客户端 | reqwest（**不启用** gzip/brotli/zstd feature；启用 socks） | 见 §5.3 编码强制 identity |
| HLS 解析 | m3u8-rs（v6+，支持 Key/ByteRange/Map/AlternativeMedia） | 不手写解析器 |
| AES-128 | `aes` + `cbc` crate | `aes` 是裸块加密，不可直接用 |
| DNS | hickory-dns（异步 + 缓存） | 避免 DNS 占满 blocking 池 |
| 存储 | SQLite（rusqlite，WAL 模式）+ journal 文件 | |
| 凭据 | OS keychain（Windows DPAPI / macOS Keychain，`keyring` crate） | Cookie/Authorization 不落明文 |
| 日志 | tracing + tracing-appender（轮转） | |
| 崩溃 | panic hook + minidump | |
| HLS 合并 | ffmpeg sidecar（精简构建） | 见 §5.4 |

## 3. 系统架构（v0.2 修订）

```
┌──────────────────┐  Native Messaging(stdio)  ┌─────────────────────────────┐
│  浏览器扩展 (MV3)  │ ◄───────────────────────► │ nm-shim（同二进制 --nm-shim） │
│  · webRequest 嗅探│      每 port 一进程         │        │ 命名管道/UDS        │
│  · downloads 接管 │                            │        ▼                    │
└──────────────────┘                            │  主程序（常驻，Tauri）         │
                                                │  ├─ dl-core   下载引擎(库)    │
┌──────────────────┐  Tauri IPC（进程内）         │  ├─ dl-server 管道服务        │
│  WebView UI      │ ◄───────────────────────►  │  └─ WebView    React UI      │
│  + 托盘 + 剪贴板   │                            │  SQLite + keychain           │
└──────────────────┘                            │  appdata/dl-journals/*.dlctrl│
                                                └─────────────────────────────┘
```

**v0.2 关键修订：取消本地 TCP/WS 监听。** v0.1 的 WS 端口 + token 文件威胁模型自相矛盾（token 明文可被同用户进程读取）且面临 DNS rebinding 攻击面。修订后只有两条通道：

- **UI ↔ 引擎**：Tauri IPC 进程内直调（命令 + 事件），零网络面。
- **扩展 ↔ 主程序**：Native Messaging（Chrome 为每个 port 拉起一个 shim 进程）→ shim 经 **Windows 命名管道 / macOS Unix domain socket** 连主程序。管道 DACL 限当前用户；UDS 置于 0700 目录并校验 peer credential。无任何 TCP 监听。

**运行时纪律（防 panic 路径）：**

- dl-core 暴露 `Engine` 句柄：内部 `tokio::sync::mpsc` 命令通道 + `broadcast` 事件通道。Tauri command **只发命令/订阅事件，永不 `block_on`**（Tauri command 运行于 Tauri runtime，`Handle::block_on` 在 async 上下文直接 panic）。
- 单一 tokio runtime（`tauri::async_runtime`）；若独立 runtime，worker 线程显式设 1–2（IO 密集无需每核一 worker）并显式传递 `Handle`。
- 阻塞 IO 隔离：rusqlite 走专属写线程；`set_len`/`metadata`/`sync_data` 一律 `spawn_blocking`；文件写走每任务单写者 OS 线程（§5.3）；ffmpeg 用 tokio::process。

**单实例：** Windows 命名 mutex / macOS flock 锁文件；第二实例经管道向首实例转发命令行 URL 后退出。`--nm-shim` 模式**绝不**走单例守卫与窗口创建路径。

## 4. 仓库结构（按能力拆分 crate，一能力一 crate）

依赖单向：`engine → {net, io, throttle, filename, journal, segment, persist, model, error}`；
编译器强制模块边界，杜绝能力间反向依赖。

```
anything-downloader/
├── crates/
│   ├── dl-error/       # 错误分类（稳定码表 + i18n key + 可重试判定）
│   ├── dl-model/       # 任务领域模型：Task/状态机迁移表（纯数据与规则，无 IO）
│   ├── dl-persist/     # 任务持久化：SQLite WAL + 专属写线程
│   ├── dl-throttle/    # 层级令牌桶限速（全局→任务）
│   ├── dl-journal/     # 崩溃安全进度 journal（双缓冲 + xxh64 + sequence）
│   ├── dl-segment/     # 分段模型与覆盖不变量（plan/full_coverage）
│   ├── dl-filename/    # 文件名净化/RFC5987/冲突策略
│   ├── dl-net/         # HTTP 客户端构建（identity+代理）与 URL 探测决策表
│   ├── dl-io/          # 单写者落盘管线（定位写 + 检查点顺序）
│   ├── dl-engine/      # 下载驱动编排 + 引擎句柄 + 事件流（含集成测试）
│   ├── dl-ipc/         # 本地 IPC：帧协议 + 管道/UDS 服务 + 客户端（GUI 内嵌托管）
│   ├── dl-testserver/  # Range 静态测试源（吞吐基准 / 崩溃演练）
│   ├── dl-cli/         # 引擎 CLI（验收入口）
│   └── dl-server/      # 本地 IPC 服务（管道 + NM shim 模式，M1.5 扩展）
├── src-tauri/          # Tauri 壳（M3 完成）：GUI 即常驻主程序，内嵌 dl-ipc 托管
├── extension/          # M4：MV3 扩展
├── ui/                 # M3：React 前端
└── docs/
```

## 5. 下载引擎设计

### 5.1 任务状态机

```
Queued → Probing → Downloading ⇄ Paused
                        ↓            ↓
                  WaitingRetry   Merging(HLS) → Verifying → Done
                        ↓            ↓
                     Failed ←────── Canceled（任意态可进）
```

- `Failed` 携带**可分类错误**（§5.6），403 鉴权失败与磁盘满是不同处置。
- **`Verifying` 为强制阶段**（v0.1「可选」是错的）：完成前必须通过 §5.3 的完整校验集。

### 5.2 调度器

- 任务并发默认 3（1–10）；**全局连接池硬上限 64**（HTTP 段连接与 HLS 分片连接共用此池，任务从池租借）。
- HTTP 版本策略：对同 host 优先 **HTTP/2 单连接多路复用**（大初始窗口 + 高并发流），服务端不支持或限流时自动退化为多 h1 连接（每段一连接）。多连接的收益主要在被 per-connection 限速的源；默认策略由实测数据决定（§11）。
- 限速（层级令牌桶）：全局桶 → 任务子桶；**子桶从全局桶领配额**（不是两个独立桶相乘）；每任务最小保底配额防饿死；补令牌按**实际流逝时间**计算（非固定 tick）；**计量点在写盘阶段**（不含重传字节）；允许 1–2 块突发。预留定时限速方案（如夜间全速）。

### 5.3 HTTP 直链下载

**写入模型（v0.2 重写，v0.1 的并发定位写在 Windows 上是数据损坏 bug）：**

多 worker 并发对同一 handle `seek+write` 在 Windows 上会互相移动共享文件指针、静默覆盖（`FileExt::seek_write` 官方文档明示移动游标）；且内核对缓冲写按 inode 串行化，并发定位写本无并行收益。改为**每任务单写者 OS 线程**：

```
worker ×N ──(offset, Bytes)──> 有界 channel（容量 = N×2 块，天然背压）
                                    │
                       单写者线程：相邻块合并 → 定位写
                       （Unix: pwrite；Windows: 单线程内 seek+write 安全）
                                    │
                       写返回 → 推进内存位图 → 周期检查点（§5.5）
```

附带收益：大块连续写降低 NTFS 碎片与元数据开销、journal 推进天然串行化、HDD 场景受益于合并写。exFAT 不支持稀疏预分配，`set_len` 仅在目标为稀疏友好文件系统且低于阈值时执行，否则跳过预分配。

**传输编码（强制）：** 所有请求 `Accept-Encoding: identity`，reqwest 不启用压缩 feature、禁用自动解压。任何分段/探测响应携带非 `identity` 的 `Content-Encoding` → 立即中止任务（`unsupported_encoding`），绝不拼装（gzip 流的 Content-Range 与解压后字节全部错位）。

**探测决策表（探测请求 `GET + Range: bytes=0-0`，响应体最多读 64KB 即断连，绝不 `resp.bytes()`）：**

| 响应 | 判定 | 动作 |
|---|---|---|
| 206 + `Content-Range: bytes 0-0/<total>` | 支持分段 | 多线程管线 |
| 206 + `bytes 0-0/*` | Range 支持但总长未知 | 单连接顺序，标记不可续传 |
| 200 | 服务器忽略 Range | 丢弃响应体，单连接顺序，标记不可续传 |
| 416 + Content-Length: 0（本地已有完整文件） | 已下载完成 | 校验长度后直接 Done |
| 416 其他 | 状态异常 | 重新探测 |
| HEAD 与 GET 不一致 | 以 GET 为准 | — |

**分段算法与不变量：**

任意时刻 `{已完成区间} ∪ {进行中区间（以各段连续写位置为界）}` 构成 `[0, total)` 的**不交覆盖**（无重叠、无空洞）。此不变量是完成校验的强制前提。

**慢段分裂（v0.2 重写，防分裂风暴）：**

- 观测门槛：任务已下载 ≥32MB 且观测窗口 ≥10s 后才启用（冷启动均值无意义）。
- 判据：段速率 < 目标速率（按剩余字节与期望时限推算，**不用全局均值**——均值注定有一半段低于它）且持续 2 个窗口。
- 动作：中止该响应（已落盘 `[S,P)` 保留），将未下载的 `[P,E)` 二分为二，一段交给刚释放的连接（**随机/轮询选择，不定向**），一段回池。
- 约束：单段分裂 ≤3 次；分裂后最小段长 ≥4MB；同一任务分裂冷却 30s。
- 任何段的重试从**该段最后连续位置**续传（Range 起点 = P），绝不重叠。
- keep-alive 连接复用可能已被服务端关闭：hyper 对幂等请求重试一次，此路径必须有显式测试。

**完成强制校验集（`Verifying` 阶段）：**

1. 覆盖完整性：位图无洞无重叠（不变量断言）；
2. 最终文件长度 == 最后一次探测的 Content-Length（超出则 truncate，不足则失败）；
3. 每段实收字节 == 请求的 Content-Range 长度（不符则 clamp 并把缺口重新入队）；
4. 抽检：随机取 2–3 个 4KB 范围重下比对（挡住 CDN 静默截断，成本可忽略）；
5. 服务器提供 `Content-MD5`/`Digest` 时做 hash 校验（增强项，不是唯一防线）。

**续传语义（v0.2 新增 If-Range）：**

- 每个续传分段请求携带 `If-Range: <强ETag>`（仅无 `W/` 前缀的强 ETag 可用；无强 ETag 用 Last-Modified）。返回 206 = 安全；**返回 200 = 实体已变 → 整任务重下**（提示用户）。任务收尾时对最后一段做条件复核（防长任务中途换版本拼接）。
- 续传入口一律从**用户原始 URL** 重走重定向链（`tasks.url`），不用 `final_url`——签名 URL 过期后 final_url 不可复活。
- 双验证器（ETag/LM）都缺失：续传前取本地文件首 4KB + 尾 4KB 发 Range 比对 + size 一致 → 判可续传；仍无法确认 → UI 二选一「强制续传 / 重新下载」，不替用户默认。
- 重定向跟随：scheme 白名单（http/https）；跳数上限；**跨 host 跳转剥离 Cookie/Authorization**、重算 Referer。

### 5.4 HLS 管线（VOD only）

1. **范围**：仅 VOD（playlist 含 `#EXT-X-ENDLIST`）；EVENT 类型按 VOD 处理；无 ENDLIST → `live_unsupported` 明确报错。m3u8 内容探测（拉前 16 字节找 `#EXTM3U`）在**主程序**做——扩展 SW 有 30s fetch 硬限制。
2. **解析**：m3u8-rs。master playlist：嗅探阶段主程序预取 variant 列表并缓存时间戳；任务创建**默认自动选最高码率立即开下**（不让签名 URL 在用户挑选时过期），UI 提供「换清晰度重下」；创建超 5 分钟出队时重拉 playlist 刷新。
3. **`#EXT-X-BYTERANGE`**：支持（同 URI 不同 range 的分片各自独立请求，解析器忽略此 tag 会把同一 URL 重复下 N 遍）。
4. **`#EXT-X-MAP`**：fMP4 必须先取 init segment 置于合并列表最前（init 本身可能也是 byterange）。
5. **音视频分离**：`#EXT-X-MEDIA TYPE=AUDIO GROUP-ID=...` 场景默认选主 variant + 同组默认音频 rendition，ffmpeg 合流 mux；UI 可只选单轨。
6. **AES-128**：IV = `#EXT-X-KEY` 的 IV 属性（hex 128-bit），**缺失时 = 媒体序列号的大端 128-bit 表示**（v0.1「key 复用」说法错误，每分片 IV 不同）；key 按 URI 缓存、`EXT-X-KEY` 变更即轮换、`METHOD=NONE` 清空；key 请求带任务 headers，其 403 与分片 403 分开报错；playlist 重拉后 key 缓存重新校验；解密用 `aes`+`cbc` crate；`SAMPLE-AES` → 报错不支持。
7. **分片并发（动态）**：在飞字节目标 ~64MB 且并发 ≥16，分片完成立即补位（固定 8 并发在「7 小片 + 1 个 40MB 大片」下尾延迟不可接受）。
8. **journal**：记录 playlist 原文哈希 + 分片列表 + 完成位图；续传时重拉 playlist 校验列表未变。分片批量 403 → 自动刷新 playlist 重试一次，仍失败 → `url_expired` 提示重新嗅探。
9. **落盘与合并**：分片写 `appdata/tmp/<task>/`；全部完成 → ffmpeg 合并输出 `.dlpart` → 原子 rename。ffmpeg 不可用时降级为 TS 二进制拼接输出 `.ts`（标注播放器兼容性问题，不作默认）。**磁盘峰值 ≈ 分片总和 + 成片 ≈ 2×，计入空间预检。**
10. **ffmpeg sidecar 管理**：Tauri sidecar 精简构建（保留 demux/mux/stream-copy/aac_adtstoasc，裁剪编解码器——注意 `-c copy` 仍需解析器，裁剪过猛跑不起来）；concat 用文件列表；参数基线 `-c copy -fflags +genpts -avoid_negative_ts make_zero -movflags +faststart`（ADTS AAC 加 `-bsf:a aac_adtstoasc`；`DISCONTINUITY` 分段 remux）；看门狗：无进度 N 秒 kill 重试一次，再失败 → `Failed(ffmpeg)` 附脱敏后的 stderr 尾部；启动时扫描清理孤儿临时目录。

### 5.5 持久化与崩溃安全（v0.2 重写）

**崩溃安全不变量：任意时刻 `journal 记录的完成集合 ⊆ 已成功写入 OS 的字节集合`（journal 是保守下界）。数据先行，journal 滞后推进；反向绝不允许。**

**journal 文件**（`appdata/dl-journals/<task_id>.dlctrl`，不放目标目录——目标可能是 exFAT U 盘/网络盘/只读，且崩溃后不该在 Downloads 留垃圾、签名 URL 不该明文暴露）：

- 固定大小双缓冲 A/B：每缓冲 = magic + 64 位递增 sequence + 任务元数据（total/etag/分段布局）+ 完成位图（4MB/块粒度）+ xxh64 校验。
- 写入 = **单次覆盖写完整缓冲**，恢复时取 sequence 最大且 checksum 通过者。不依赖 rename 原子性（rename 只保证目录项切换，且 Windows 上目标被杀软/索引器占用时会 `ACCESS_DENIED`）。
- 检查点顺序（单写者线程内）：`sync_data(数据文件) → 覆盖写 journal → sync_data(journal)`。此顺序保证断电后 journal 绝不超前于数据落盘。
- 刷新时机：内存位图实时推进；journal 落盘合并周期 200ms；**强制 flush** 于暂停/取消/完成/应用优雅退出；`sync_data` 每 256MB 或 2s 一次——**绝不分块 fsync**（机械盘 fsync 上限 100–200 次/s，会打死 110MB/s 目标）。
- 非活跃任务关闭 journal FD（万级任务下 Windows 默认 FD 上限 512、macOS 256）。

**`.dlpart` 临时名 + 原子收尾：**

- 下载中写 `<name>.<ext>.dlpart`，完成时校验长度 → 原子 rename 到最终名（Windows `MoveFileExW(REPLACE_EXISTING)` + 共享冲突重试；POSIX `rename`）。
- **冲突策略在此步生效**：`rename`（默认，`name (1).ext` 递增）/ `overwrite` / `skip` / `ask`，可记忆化。**`set_len` 预分配只作用于 `.dlpart`，绝不触碰用户已有文件**（v0.1 会对已存在文件静默截断清零）。

**文件名 sanitize（确定性算法，非「校验」二字）：**

- Windows：剔除路径成分 → 替换 `< > : " / \ | ? *` 及控制字符 → 保留名（CON/PRN/AUX/NUL/COM1-9/LPT1-9，含上标变体）加 `_` 前缀 → 去尾部空格与点 → 长路径走 `\\?\` 封装并把 `ERROR_FILENAME_EXCED_RANGE` 映射为可读错误。
- macOS：替换 `:` 与 `/`，处理开头点号。
- 文件名来源优先级：RFC 5987 `filename*=UTF-8''%xx` > `filename` > URL path 末段（query 不进文件名）；扩展名与 Content-Type 不符时按 MIME 补扩展名。

**SQLite：** WAL 模式 + busy_timeout + **专属写线程**（rusqlite `Connection` 是 `!Sync`）。`tasks` 表存 `url`（原始）、`final_url`、filename、dir、size、etag、last_modified、mime、kind、status、priority、created_at、updated_at、**headers_ref（keychain 引用，敏感头不进库）**。历史查询建索引（filename/url/host/created_at）。

### 5.6 错误分类与恢复（v0.2 新增）

```
Network(dns_failed | connect_timeout | tls | connection_reset)
Http(status, retry_after)          → 429/503 读 Retry-After 按其退避
AuthExpired(401/403 且带凭据)      → UI「从浏览器刷新 Cookie 后重试」：扩展经 chrome.cookies 重新取，任务原地续传
UrlExpired(403/410 签名失效)       → 「重新嗅探」引导
RangeUnsupported / LengthUnknown
DiskFull / FileAccessDenied / FileNameInvalid / PathTooLong
UnsupportedEncoding / LiveUnsupported | DrmUnsupported | DashUnsupported
ProxyAuthRequired(407) / ProxyConnectFailed
Internal(...)
```

每类错误 → i18n key → 用户可读文案 + 处置动作。重试矩阵：网络类指数退避（1s/2s/4s…上限 60s）+ 连续失败自动降连接数；4xx（除 429）不盲目重试，升级为重新探测；意外 EOF/被按字节数掐断 → 从段内最后连续位置续传。任务详情面板展示：错误分类、HTTP 状态码、重定向链、重试历史时间线、当前/最终 URL，附一键重试（重置退避 + 重新探测）。

### 5.7 多镜像源（aria2 multi-URI，分期）

任务模型支持 `uris: Vec<String>`；跨镜像一致性校验（强 ETag 或 size+首 4KB 比对）后才允许混拼分段。v1 落地：模型 + 手动粘贴多 URL；自动加权选择与失效切换放 v1.x。单源被 per-connection 限速时这是跑满带宽的主要杠杆，接口从 M1 就位。

### 5.8 代理（v0.2 新增，M1 而非 M7）

- 模式：**跟随系统**（Windows WinINET / macOS SCDynamicStore，含 PAC）/ 手动 HTTP / HTTPS(CONNECT) / SOCKS5 / **SOCKS5h（推荐默认，远端 DNS——`socks5` 本地解析在 DNS 污染场景直接失败）** / 不使用。
- per-task 覆盖；`no_proxy` 内网直连列表；407 → `ProxyAuthRequired` 明确报错（不报成「下载失败」），凭据存 keychain；代理切换后重试。
- 「代理下全功能」进测试矩阵；DNS 走 hickory 异步解析 + 缓存。

## 6. 本地服务与 IPC（v0.2 重写）

### 6.1 通道与安全

| 通道 | 用途 |
|---|---|
| Tauri IPC（进程内） | UI ↔ 引擎：命令 + 100ms 聚合事件快照 |
| NM（stdio，每 port 一 shim 进程） | 扩展 ↔ shim：小消息 |
| 命名管道 / UDS | shim ↔ 主程序 |

- 命名管道 `\\.\pipe\anything-dl-<suffix>`，DACL 仅当前用户 + SYSTEM；UDS 置于 0700 目录 + `LOCAL_PEERPID` 校验。
- 连接握手：shim 上报 NM origin（`argv[1]`）+ `protocol_version`，主程序校验 origin 在允许清单。
- 入站校验：URL 仅 http/https；仅透传白名单头（Referer/UA/Cookie/Authorization/Accept）；NM 消息带协议版本，不兼容返回明确错误 + 升级引导（扩展商店审核天然滞后，版本兼容是常态而非异常）。
- **NM 消息大小硬限：host→扩展单条 ≤1MB。** 大载荷（完整资源列表、任务列表）设计为「主程序持有数据 + 扩展游标分页请求」，不做 host→扩展大批量推送；UI 进度不回显扩展（badge 仅计数摘要）。

### 6.2 shim 生命周期

- **port 只由 background SW 持有**（popup 经 `chrome.runtime.sendMessage` 转发，避免双 shim）；port 关闭即 shim 退出，启动预算 <100ms。
- SW 被回收断链 → 事件唤醒后重连（指数退避）；断链期间嗅探数据缓存于 storage.session，重连后补推。
- 主程序被用户退出 → shim 默认**不**自动拉起；badge/popup 显示「主程序未运行」+ 用户显式拉起按钮。
- 多 shim 并发拉起竞态 → §3 单实例守卫兜底。
- stdout 协议纯净化：任何日志只写 stderr；Windows 自测含 `0x0A` 的消息帧。

## 7. 浏览器扩展

### 7.1 权限与监听配置（v0.2 新增——缺了嗅探不到任何东西）

```
permissions:        ["webRequest", "downloads", "downloads.ui", "nativeMessaging",
                     "storage", "cookies", "alarms", "tabs"(可选)]
host_permissions:  ["<all_urls>"]
```

监听管线（性能分层）：

- `onBeforeRequest`（无头、低成本）全量预筛 URL/类型线索 → 仅候选请求进入下一步；
- 候选才在 `onBeforeSendHeaders`（extraInfoSpec: `requestHeaders` + **`extraHeaders`**——Chrome 72+ 不加则 Cookie/Referer 等被脱敏）抓请求头；
- `onCompleted`（`responseHeaders`）抓响应头/MIME/Content-Length；
- `onErrorOccurred` 处理网络层失败（4xx/5xx 仍走 onCompleted，两类都要处理）；
- `onBeforeRedirect` 记录完整重定向链（finalUrl 未定时 cookie 域以此为准）。

### 7.2 嗅探

- 按 tab 聚合；m3u8/master 命中即建资源组，后续同源分片归组。
- **大小阈值类型感知**（v0.1 的 1MB 一刀切会把 HLS 分片全滤掉）：直链媒体 ≥1MB；**分组内分片不受阈值**，按组累计字节判断价值；无 Content-Length（gzip/chunked）标「大小未知」。
- 内存缓存命中的请求对 webRequest 不可见（官方明示）→ UI 标注「分片数 = 已观测值」，以主程序实际拉取为准。
- 多视频/广告页歧义：按累计字节最大组定主资源 + content script 读 `video.src` 作线索（需 `storage.session.setAccessLevel('trusted_contexts')`）+ 用户手选。
- POST 发起的下载排除（body 不可靠）。
- 持久化：嗅探结果异步推主程序 SQLite（浏览器重启后可查）；扩展侧只保内存快照。

### 7.3 接管（v0.2 重写——`cancel()` 不是静默成功）

Chromium 源码事实：下载已完成时 `cancel()` 不报错且什么都不做；`onCreated` 触发时请求已发出，小文件可能在 cancel 到达前完成 → 双下载。修正流程：

1. 规则确定模式：`onCreated` 命中 → `setUiOptions({enabled:false})` 压制下载栏 → `cancel(id)` → **用 `onChanged` 复核结局**：
   - `interrupted` + `error: USER_CANCELED` = 接管成功，推送主程序；
   - `complete` = 已落盘：`removeFile()` + `erase()`（erase 不删文件，须先 removeFile）后仍推送主程序重下；
   - 其他 `interrupted` = 原下载本就失败，按失败处理。
2. 「每次询问」模式：**`pause()` → 通知/popup 询问 → `cancel()` 或 `resume()`**（cancel 不可逆，先 pause 留退路）。`action.openPopup()` 需 Chrome 127+，低版本降级 `chrome.notifications` 点击打开。
3. `onDeterminingFilename` **不用于接管**（它只能 suggest 文件名，不能取消）。
4. 兜底回落 `chrome.downloads.download()` 时打标记并在 `onCreated` 跳过，防自触发死循环。
5. 浏览器已写入 `.crdownload` 的字节不可复用，明示整体重下。

### 7.4 Cookie 双通道

- 主：webRequest 请求头快照（`extraHeaders`）；兜底：`chrome.cookies.getAll({url: 最终URL})` 补齐（跨站时浏览器可能因 SameSite 未发 cookie）。
- HttpOnly 不挡 `chrome.cookies` 与 webRequest 观测（设计如此，用于转交）。
- 安全注记：**扩展事实持有用户全站登录态，是本产品最大攻击面**。最小化原则：嗅探可按站点/类型关闭；不声明 `externally_connectable`（网页与第三方扩展不可直达 NM）；headers 不进任何日志。

### 7.5 NM 注册与安装引导

- 四个注册位置（全用户级，**禁 HKLM / /Library**，免管理员）：Win Chrome `HKCU\Software\Google\Chrome\NativeMessagingHosts\`、Win Edge `HKCU\SOFTWARE\Microsoft\Edge\NativeMessagingHosts\`（Edge 按 HKCU Edge→Chromium→Chrome 回退且只读第一个 manifest）、macOS `~/Library/Application Support/Google/Chrome/NativeMessagingHosts/` 与 `.../Microsoft Edge/NativeMessagingHosts/`。
- `allowed_origins` 收录三 ID：开发期固定 ID（manifest `"key"` 钉住，unpacked 路径推导的 ID 不稳定）+ Chrome 商店 ID + Edge 商店 ID；显式写两份注册不依赖 Edge 回退。
- 先装扩展后装主程序：识别 NM 的 "host not found" / "forbidden" 错误 → 引导页「请先安装主程序」；NM manifest 在每次 `connectNative` 时解析，通常无需重启浏览器（M4 实测确认）。
- 探活：popup 打开时经 port ping/pong，**不轮询**（SW 唤醒有成本）。

### 7.6 storage.session 治理（v0.2 纠偏）

- 定位：**内存快照——跨 SW 回收存活、不跨浏览器重启/扩展更新**（官方：存内存不落盘）；「重启后恢复」由主程序 SQLite 负责。
- 写入：500ms 防抖批量 flush（不逐请求写，避免 IPC 风暴）。
- 配额：固定 10MB 不可提升。`getBytesInUse()` 监控 + tab 关闭 TTL 清理 + 近上限降级（丢分片明细、保 manifest）+ `set` 的 `QUOTA_BYTES` 错误必须处理。

## 8. UI / UX

- **任务列表**：分类页签（全部/下载中/已完成/失败/嗅探收藏）；行内进度/速度/ETA/连接数；虚拟滚动 + 行级 memo + 仅活跃任务增量推送（payload 用 MessagePack）；任务详情面板见 §5.6。
- **新建任务**：多行粘贴批量解析（自动去重、识别 m3u8）→ 进程内探测 → 确认（目录/线程数/冲突策略）→「记住我的选择」。
- **批量与导入导出**：扩展 popup「本页全部资源」勾选批量；任务列表导出/导入（URL、文件名、目录；**不含 Cookie**）。
- **通知策略**：接管通知默认关闭（高频会轰炸）；完成/失败通知聚合（N 秒窗口合并「3 个任务已完成」）；免打扰时段；便携版降级为应用内提示；macOS 首次通知前请求授权、被拒降级。
- **剪贴板监听**：仅本会话新出现且未提示过的 URL 提示一次；气泡带「忽略此站点/类型」；macOS 为轮询，明确间隔与功耗策略。
- **完成后动作**：打开文件 / 关机 / 休眠 / 退出 / 运行命令（仅用户自配 + 强警示），任务级与全局级队列。
- **历史与规则**：历史搜索（按文件名/URL/站点/时间，走 SQLite 索引）；下载中可重命名（扩展名锁定）；自动目录规则引擎 `条件(类型/域名/时间) → 模板(%date/%domain/%type)`。
- **平台惯例**：macOS Cmd+W 关窗 / Cmd+Q 退出（「关窗即隐藏」违反惯例，改为默认询问）；任务栏进度条；暗色模式跟随系统；Windows 125–150% 缩放 HiDPI 走查；中文字体渲染专项。

## 9. 安全设计（v0.2 重写）

**威胁模型（诚实边界）：** 防护 = 远程网络、其他用户会话、浏览器内恶意网页（无 TCP 监听面 + NM origin 校验 + scheme 白名单）；**不防护 = 同用户态恶意进程**——那等价于本机已失陷，任何纯软件方案只能提高门槛，不为它设计复杂机制。

- **凭据存储**：Cookie/Authorization → OS keychain（DPAPI/Keychain）按 task id；SQLite 只存引用。数据目录 = Win `%APPDATA%` / macOS `~/Library/Application Support`，文档提醒勿放同步盘，卸载清理（NM 注册、数据目录、sidecar 缓存）。
- **日志与脱敏**：tracing 分级 + 轮转；**URL 脱敏函数**（strip query 中 token/sig/key/sign/expires 等参数）应用于一切日志、错误文案、UI、上报、诊断包；panic hook 脱敏后写盘；「导出诊断包」= 日志 + 环境 + 任务摘要，导出前二次脱敏。
- **内容校验规则表**：魔数与扩展名不符 → 告警继续 + UI 标记；已知可执行类型（exe/msi/bat/ps1/dmg/app）完成时强提示来源。zip 炸弹职责声明：只写流不解压，不检测压缩比（不假装能防）。
- **资源上限**：m3u8 分片数上限（默认 10000 可配）、variant 递归深度上限、预估总大小上限；任务开始前磁盘预检（剩余空间 ≥ 预估 + 余量，HLS 按 2× 计；目标目录存在/可写检查）；`ENOSPC` → 停写、flush journal（保住已下部分）、`Failed(disk_full)`，绝不在 ENOSPC 后继续标记完成。
- **Mark-of-the-Web（我们是下载方，负责打标）**：Windows 写 `Zone.Identifier` ADS；macOS 写 `com.apple.quarantine`。缺失即绕过 SmartScreen/Gatekeeper（BT 客户端是公认反面案例），不做等于自拆安全门。
- **单实例与唤醒**：第二实例转发后退出；系统休眠/唤醒后统一重连 + 重新探测（TCP 状态可能已失效）。

## 10. 跨平台

**Windows：**
- Authenticode 签名是首版阻塞项：未签名 → SmartScreen「Windows 已保护你的电脑」拦截，直接杀转化率；OV 证书需信誉积累，EV 即时信誉但贵——M0 前决策。
- 安装器形态（NSIS / MSI / 便携版）M0 前定：便携版与 NM 注册表路径稳定性、updater 配合都是连带决策。
- 长路径：`LongPathsEnabled` + 应用清单声明 + 引擎 IO 层 `\\?\` 封装。
- 电源事件（`WM_POWERBROADCAST`）优雅 flush journal。

**macOS：**
- **App Nap**：常驻无窗口 + 静默下载是 App Nap 教科书候选（IO/定时器节流直接打击千兆卖点）。活动下载期间持有 `NSActivityUserInitiated | NSActivityLatencyCritical` assertion，全部暂停/结束释放；`Info.plist` 设 `NSAppSleepDisabled`。
- **公证链**：Developer ID 签名 + hardened runtime + **ffmpeg sidecar 同 Team ID 签名一并公证**（否则公证失败）；Apple Silicon 为主流 → universal（aarch64 + x86_64）构建；不上 MAS（免沙盒），仅 Developer ID 外部分发。
- quarantine 落标见 §9。

**自动更新（v0.2 新增，常驻产品必需）：** tauri-plugin-updater（签名强制、pubkey 内嵌 tauri.conf.json、私钥只进 CI secrets 且**丢失即永远无法向存量用户推送**——备份与轮换流程入文档）；更新源走 TLS；用户确认后安装；与下载中任务的冲突处理（暂停或排队）；Windows 安装期自动退出的 `on_before_exit` 与引擎独立 runtime 联动验证。

**签名密钥资产清单**（三者都是「丢了就完了」）：Windows 代码签名证书、macOS Developer ID + Apple 账号、updater 签名密钥对。保管位置、备份、轮换、泄露应急（吊销/撤销更新通道）写入运维文档。**Apple Developer 账号（$99/年）与 Windows 证书在 M0 前启动申请。**

## 11. 质量保障（v0.2 新增）

**测试策略：**
- m3u8 解析器真实语料集（BOM、`\r\n`、未知 tag、相对 URI、BYTERANGE/MAP/DISCONTINUITY）。
- 分段覆盖不变量的**属性测试**：随机注入分裂/失败/重试序列，断言任意时刻无重叠无空洞。
- journal 崩溃恢复**故障注入**：在任意系统调用点 kill -9 / 模拟断电（丢弃未 sync 的写入），断言恢复后保守下界成立、文件无空洞。
- 代理矩阵（系统/PAC/HTTP/SOCKS5h × 失败/切换）；HLS 公开测试源端到端；接管复核流（complete 竞态用例）。
- CI 性能回归门槛（吞吐、内存基线）。

**性能测试方法学：**
- 本地测试源：nginx/caddy + 可配限速 + 10GB 测试文件（`python -m http.server` 单线程自身就是瓶颈，不可用；公共测速源不可能给单客户端 110MB/s）。
- 声明回环局限：仅验证「引擎不是瓶颈」，不反映真实网卡。
- 指标采集与判定脚本化；内存测量明确边界（主进程 RSS 与进程树分开，工具与采样方法固定）；万任务帧率用合成帧直方图。

**崩溃收集：** panic hook（human-panic 类）+ 本地 minidump 留存 + 可选上报（脱敏后）+ 用户反馈附诊断包（§9）。

## 12. 里程碑（v0.2 重排：M0–M8，2 人量级估算，单人约翻倍）

**M0 前置（立即启动，长周期依赖）：** Apple Developer 账号申请；Windows 签名证书决策（OV/EV）；ffmpeg 精简构建流水线；本地性能测试源搭建。

| 阶段 | 内容 | 验收 |
|---|---|---|
| M0（2–3 周） | workspace、状态机、SQLite、管道 IPC 骨架、CI | 编译通过，CLI 建任务 |
| M1（6–10 周） | HTTP 引擎核心：探测决策表、单写者管线、分段+分裂、崩溃安全 journal、`.dlpart`+原子 rename、冲突策略、If-Range、错误分类、限速、**代理** | 本地源 ≥95%；kill -9 续传零丢失；断电语义故障注入通过 |
| M1.5（2–3 周） | 最小扩展通道：NM shim + 管道 + webRequest 头捕获（CLI 验证）——为 M2 提供真实签名 URL/Cookie 测试面 | 扩展推送 URL → CLI 下载成功 |
| M2（4–6 周） | HLS VOD：m3u8-rs、BYTERANGE/MAP/音轨、AES-128、动态并发、ffmpeg sidecar + 看门狗、时效刷新 | 公开测试源全通 |
| M3（4–6 周） | Tauri UI：列表/新建/详情/设置/托盘/通知策略/剪贴板 | 空载主进程 <60MB；万任务 60fps |
| M4（4–8 周） | 扩展完整：嗅探聚合 + popup、接管复核流、NM 注册安装引导、商店审核提交（预留审核与隐私政策时间） | Chrome/Edge 实测接管与嗅探 |
| M5（3–4 周） | 稳定性：日志/脱敏、崩溃收集、诊断包、重试矩阵、单实例、唤醒重连 | 故障注入全套通过 |
| M6（3–4 周） | 分发：Win 签名+安装器、macOS 公证+sidecar 签名+universal、updater、卸载清理 | 双平台全新机器安装即用 |
| M7（3–4 周） | UX 补齐：批量、导入导出、目录规则、完成后动作、i18n、历史搜索 | 验收清单过 |
| M8（2–3 周） | 性能与打磨：方法学执行、回归门槛、虚拟滚动调优、边界场景 | §1 指标全达标 |

**总量级：2 人约 7–9 个月**（v0.1 的「一个季度」暗示不现实）。

## 13. 风险与对策（v0.2 更新）

| 风险 | 对策 |
|---|---|
| MV3 SW 回收 / NM 生命周期 | §6.2/§7.6 全套生命周期设计 + M1.5 提前验证 |
| 视频站反爬（签名时效、加密变体） | 即建即下 + playlist 刷新 + `url_expired` 明确分类；DRM/DASH 明示边界 |
| ffmpeg 精简构建裁剪过猛跑不起来 | 构建流水线进 M0 前置，用真实语料回归 |
| Chrome/Edge 商店审核（`<all_urls>` + extraHeaders + cookies 敏感组合） | 权限 justification + 隐私政策，M4 预留审核周期 |
| Windows SmartScreen / macOS 公证 | 证书与账号 M0 前启动；签名进 M6 验收 |
| updater 私钥丢失 = 存量用户失联 | 密钥资产清单 + 备份轮换流程（§10） |
| 服务器掐多连接 | 失败自动降连接数 + h2 多路复用默认策略 + 站点规则库 |
| 内存/吞吐指标不达 | 指标已定义测量边界；M1/M3 设回归门槛早发现 |
