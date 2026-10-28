# Anything Downloader · 跨端统一交互规范 v1.0

> 桌面端当前布局与视觉以 [UI-DOWNLOADER.md](UI-DOWNLOADER.md) 和 `ui/src/styles.css` 为准；浏览器扩展保留 [UI-STUDIO.md](UI-STUDIO.md) 中的工作流。本文保留旧版交互约束作为参考。

适用范围：桌面应用（Tauri + React，`ui/src/`）、浏览器扩展（`extension/popup.html`、`extension/sync.html`、`extension/float-button.js`）。
下文为旧版规范，视觉值与布局以各端当前设计文档为准。关于操作反馈、数据保护、键盘可达性及适配器管线的约束继续适用；扩展不增加 manifest 权限。

行号引用基于当前 main 分支（454f4db）。

---

## 0. 优先级定义

| 级别 | 含义 | 判据 |
|---|---|---|
| **P0** | 体验硬伤 | 数据丢失无保护、反馈误导（成败同色/假在线/假死）、核心路径不可用（键盘完全无法操作、字段静默丢弃） |
| **P1** | 重要提升 | 反馈缺失、一致性破坏、可达性补全、明显功能缺口（如速度不显示）、对比度不达标 |
| **P2** | 打磨 | 微文案、微交互、死代码清理、锦上添花 |

同一功能多个入口的问题（如「删除有无确认不一致」）按最高级别处理，且必须一次性对齐所有入口。

---

## 1. 设计原则

**反馈及时** —— 任何点击 100ms 内必须有状态变化（按钮 disabled / 文案加「…」/ 行内反馈）；任何操作 2s 内可见结果（完成后立即 `store.refresh()`，不等 2s 轮询）；失败必须有**人类可读的原因**与重试路径，禁止静默吞异常。现状反例：批量推送结果类未定义导致成败同色（popup.js:388-394）、浮标操作失败只 `console.warn`（float-button.js:145-157）。

**路径最短** —— 高频动作一步直达：下载完 →「打开文件 / 打开文件夹」（`api.ts:115` 的 `revealTask` 已存在，必须接线）；popup 打开 → 粘贴框自动聚焦；筛选/搜索空 → 提供「清除筛选」出口。不为低频功能添加永久入口（克制）。

**状态可见** —— 系统当前状态始终可辨：任务在下载（速度+进度+脉冲圆点）、主程序在线（pill 动态检测而非一次性，popup.js:749-759）、总大小未知（indeterminate 进度条而非 0% 假死，TaskRow.tsx:106-113）。同一状态在所有界面用同一**颜色与形态**；文案在同端内一致，扩展端中文文案与应用端 zh 词典语义对齐（语汇集中复用 popup.js 的 ERROR_TEXT/STATUS_TEXT）；英文界面仅应用端承诺（扩展端不做 i18n 框架，范围见 §6.1）。

**克制的一致性** —— 四处界面（应用/popup/sync/浮标）共用同一套 token、同一套组件形态、同一套四态处理；一致但不扩张：不引入新布局体系、不加重阴影堆叠、不加装饰性动画。系统亮暗跟随 `prefers-color-scheme`，不做手动切换开关（v1 范围外）。

---

## 2. 设计 Tokens（四处界面共用同一套命名）

### 2.1 规范值（唯一事实源）

应用端落在 `ui/src/styles.css:1-33`（替换现有值）；popup 复制进 `popup.html <style>`；sync 复制进 `sync.html <style>` 并删除现第三套调色板；浮标面板以**硬编码副本**内联（内容脚本无法跨 document 共享 CSS 变量，见 §6 约束）。每处粘贴时加注释 `/* ADL-DESIGN-TOKENS v1 — 同步自 docs/UI-DESIGN.md §2 */`。

```css
:root {
  /* —— 中性色（light）—— */
  --bg: #f6f7f9;            /* 页面底 */
  --panel: #ffffff;         /* 卡片/行/浮层底 */
  --border: #e5e7eb;        /* 常规边框 */
  --border-soft: #f1f5f9;   /* 弱分隔/标签底 */
  --text: #1f2937;          /* 正文 */
  --muted: #6b7280;         /* 次要文字（on #fff 4.83:1 ✓AA） */
  /* —— 品牌色 —— */
  --accent: #2563eb;        /* 品牌蓝：logo/圆点/进度条渐变端/focus ring（图形 ≥3:1） */
  --accent-strong: #2563eb; /* 主按钮填充：白字 5.17:1 ✓AA */
  --accent-ink: #2563eb;    /* 强调文字/选中项文字/链接 */
  --accent-soft: #eff6ff;   /* 选中底/toast 底/hover 底 */
  --violet: #7c3aed;        /* 进度条渐变另一端（与 accent 组成品牌渐变） */
  --violet-soft: #f5f3ff;
  /* —— 语义色（实体 + soft 底 + ink 文字色，三件成套）—— */
  --ok: #16a34a;   --ok-soft: #f0fdf4;   --ok-ink: #15803d;    /* ink on soft 4.79:1 ✓ */
  --err: #dc2626;  --err-soft: #fef2f2;  --err-ink: #b91c1c;   /* 5.91:1 ✓ */
  --warn: #d97706; --warn-soft: #fffbeb; --warn-ink: #b45309;  /* 4.84:1 ✓ */
  /* —— 阴影 / 滚动条 —— */
  --shadow-1: 0 1px 3px rgba(15,23,42,.06);   /* 卡片 */
  --shadow-2: 0 6px 24px rgba(15,23,42,.2);   /* 浮层/浮标面板 */
  --scroll-thumb: #cbd5e1; --scroll-thumb-hover: #94a3b8;
  /* —— 圆角 —— */
  --r-sm: 6px;   /* 行内小按钮、tag */
  --r-md: 8px;   /* 按钮、输入框 */
  --r-lg: 10px;  /* 卡片、站点卡 */
  --r-xl: 12px;  /* 对话框、抽屉 */
  --r-pill: 999px;
  /* —— 间距（4px 栅格）—— */
  --sp-1: 4px; --sp-2: 8px; --sp-3: 12px; --sp-4: 16px; --sp-5: 20px; --sp-6: 24px;
  /* —— 字号阶梯（基准 13px）—— */
  --fs-xs: 11px;  /* 辅助：计数、时间、tag */
  --fs-sm: 12px;  /* 次要：状态、按钮小号 */
  --fs-md: 13px;  /* 正文基准 */
  --fs-lg: 15px;  /* 浮层标题 */
  --fs-xl: 18px;  /* sync 页标题（唯一使用处） */
  /* —— 动效 —— */
  --dur-fast: 120ms;  /* hover/按压 */
  --dur-med: 200ms;   /* 进度条宽度、pill 变色 */
  --dur-slow: 300ms;  /* 浮层出入 */
  --ease-out: cubic-bezier(.2, 0, 0, 1);
  --font-ui: 13px/1.5 "Segoe UI", system-ui, sans-serif;
  --font-mono: 11px/1.5 Consolas, ui-monospace, monospace;
}
@media (prefers-color-scheme: dark) {
  :root {
    --bg: #111318; --panel: #1a1d24; --border: #2a2e37; --border-soft: #1e222b;
    --text: #e5e7eb; --muted: #9ca3af;              /* on #1a1d24 6.64:1 ✓AA */
    --accent: #3b82f6;                              /* 仅图形：进度条/圆点/focus ring */
    --accent-strong: #2563eb;                       /* 主按钮填充：白字 5.17:1 ✓AA（实测） */
    --accent-ink: #93c5fd;                          /* 强调文字：on --accent-soft 8.11:1 ✓AA（实测） */
    --accent-soft: #1e293b;
    --violet: #a78bfa; --violet-soft: #2e1065;
    --ok: #4ade80;  --ok-soft: #052e16;  --ok-ink: #4ade80;    /* ink on soft 8.55:1 ✓ */
    --err: #f87171; --err-soft: #450a0a; --err-ink: #f87171;   /* 5.84:1 ✓ */
    --warn: #fbbf24; --warn-soft: #451a03; --warn-ink: #fbbf24; /* 8.97:1 ✓ */
    --shadow-1: 0 1px 3px rgba(0,0,0,.4);
    --shadow-2: 0 6px 24px rgba(0,0,0,.5);
    --scroll-thumb: #3a3f4b; --scroll-thumb-hover: #4b5563;
  }
}
```

### 2.2 accent 三分规则（本次对比度修复的核心）

`--accent` / `--accent-strong` / `--accent-ink` 三值拆分，解决现状「暗色下一个蓝既当填充又当文字」导致的 AA 不达标（实测 `styles.css:70` 白字 on 暗色 `#3b82f6` = 3.68:1；`styles.css:107/150` accent 文字 on `--accent-soft` = 3.98:1）：

- **`--accent-strong`**：只作 primary 按钮背景。暗色保持 `#2563eb`（白字实测 5.17:1 ✓），亮色同值。
- **`--accent-ink`**：所有蓝色**文字**（选中侧栏项、toast 文字、链接、聚焦边框）。暗色用 `#93c5fd`（on `#1e293b` 实测 8.11:1 ✓）。
- **`--accent`**：图形与渐变（logo 底、进度条渐变、状态圆点、focus ring）。暗色 `#3b82f6`（图形对比 ≥3:1 即可）。

同一规则推广到全部语义色：**实体色（`--ok/--err/--warn`）只用于图形**（状态圆点、边框、进度条、填充型控件底色）；其**文字形态一律用对应的 `--{role}-ink`**——亮色为加深一档的深色值，暗色为提亮一档的浅色值（暗色 ink 与实体色同值）。实测依据：亮色 tag 实体色 on soft 底不达 AA（ok 3.15 / warn 3.07 / err 4.41），换 ink 后 4.79 / 4.84 / 5.91；暗色 8.55 / 5.84 / 8.97。violet 系实测两主题均达标（亮 `#7c3aed` on `#f5f3ff` = 5.20、暗 `#a78bfa` on `#2e1065` = 5.60），**无需** `--violet-ink`，直接用 `--violet` 作文字。

迁移对照（popup 现值 → 统一值）：`#f8fafc→--bg`、`#0f172a→--text`、`#64748b→--muted`、`#e2e8f0→--border`、暗色 `#0f1117/#171a21→--bg/--panel`、`#60a5fa→--accent(-ink 按文字/图形角色分流)`、`#94a3b8→--muted`。sync 页整套硬编码（sync.html:7-44，`color-scheme: light` 必须删除）全部替换为 token；其 `#15803d→--ok`、`#b91c1c→--err`、`#b45309→--warn`、`#94a3b8→--muted`。浮标面板副本同规则（float-button.js:104）。

字体与字号：基准 `--font-ui` 13px（sync 从 14px 降到 13px）；中文字形由系统回退覆盖，不加 PingFang/YaHei 显式项（Chromium/Windows 环境）。数字一律 `font-variant-numeric: tabular-nums`（沿用 styles.css:111/138/208 惯例）。

---

## 3. 组件规范

### 3.1 按钮层级

| 层级 | 形态 | 用途 | 每屏数量 |
|---|---|---|---|
| primary | `--accent-strong` 底 + 白字 + 600 | 当前上下文唯一主操作 | 每个视图容器 ≤1；横幅/浮层内的动作一律降级为 default 或 danger（见 A16） |
| default | `--panel` 底 + `--border` 边框 | 常规操作 | 不限 |
| ghost | 无边框透明，hover `--accent-soft` 底 | 关闭 ×、弱化操作 | 不限 |
| danger | 文字 `--err`、hover `--err-soft` 底 | 删除/清空类 | 不限 |
| mini | `--fs-sm` + padding 2px 10px | 表格行内/紧凑区 | — |

尺寸：默认高 28px（padding 5px 12px，圆角 `--r-md`）；行内 mini 高 24px。hover：边框/文字转 `--accent-ink`（`--dur-fast`）；active：`opacity:.85`（克制，不做位移缩放）；disabled：`opacity:.5` + `cursor:default`，**并配合文案反馈**（见加载态）。loading：文案后缀「…」（沿用「推送中…」「识别中…」既有惯例）+ disabled，不引入 spinner。

现状修正：popup 的 `.icon-btn`（popup.html:192）、设置面板全套 `.settings/.set-title/.set-note`（popup.html:229-247）**必须补样式定义**——齿轮按钮用 default 层级 + 28×28 方形，active 态（popup.js:654 切换的 `.active`）给 `--accent-soft` 底 + `--accent-ink` 字；设置面板按 fieldset 卡片化（`--panel` 底 + `--r-lg` + `--shadow-1`，label `--fs-sm --muted`）。

**填充型选中控件的组件级映射**（popup 的 `.tab2.active`、`.chip-f.active`，popup.html:82-93）：填充色一律用 `--accent-strong` + 白字——两主题均实测 5.17:1；**禁止用 `--accent` 作白字填充底**（暗色 `#3b82f6` 仅 3.68:1；popup 现状暗色 `#60a5fa` 仅 2.54:1，正是 §2.2 判死的组合且字号更小）。

### 3.2 输入框

沿用现有形态（`--panel` 底、`--border`、`--r-md`、padding 6px 10px）。focus：`border-color: var(--accent-ink); box-shadow: 0 0 0 2px var(--accent-soft)`，**禁止 `outline:none` 只换边框色**（修 popup.html:71）。数值输入（分段数/限速/并发）：blur 与保存时 clamp 到标注范围，非法时输入框边框转 `--err-ink` 并在其下显示 `--fs-xs --err-ink` 错误行；不依赖 HTML min/max 兜底（修 SettingsDialog.tsx:83-146 键入 999 直接入库的问题）。

### 3.3 卡片 / 列表行

- 应用表格行：56px 固定行高不变（虚拟滚动前提，App.tsx:59、TaskRow.tsx:8 合并为单一常量导出），hover `--accent-soft`，底线 `--border`。行内容溢出一律 `ellipsis`，任何行内元素不得撑破 56px（修 TaskRow.tsx:118-122 错误提示挤压邻行——行级错误出口统一为 error toast + 出错行闪红，唯一口径见 §4.1/A18）。
- 扩展卡片（popup `.item`）：`--r-lg` + `--shadow-1` + hover 边框 `--accent-ink`，选中态 `--accent-soft` 底（沿用现状结构）。
- 行内主操作 ≤3 个，更多操作收进「详情」；完成态固定为 [打开][文件夹][详情]（「删除」统一收进详情抽屉，见 A1 与 §4.5）。

### 3.4 标签 chip / tag

沿用 popup 色谱并成为唯一标准；**tag 文字一律用对应 `--*-ink`**（实体色只作图形/边框，实测数据见 §2.2）：视频=`--accent-ink` on `--accent-soft`、HLS/音频=`--violet` on `--violet-soft`（violet 实测达标，无 ink）、压缩包=`--warn-soft` 底 `--warn-ink` 字、软件=`--err-soft` 底 `--err-ink` 字、文档=`--ok-soft` 底 `--ok-ink` 字、其他=`--border-soft/--muted`；任务状态四态 tag：进行中 `s-active`、完成 `s-done`、失败 `s-failed`、等待 `s-warn`（popup.js:34-43 已实现，浮标面板复用同一映射）。筛选 chip：未选=描边幽灵，选中=`--accent-strong` 底白字（§3.1 填充型选中控件映射）。

### 3.5 进度条

高度 5-6px、`--r-sm`、底 `--border`；填充渐变 `linear-gradient(90deg, var(--accent), var(--violet))` —— 应用端 `styles.css:201` 的硬编码 `#7c3aed` 改为 `var(--violet)`，与 popup.html:147 天然统一。done 转 `--ok`、failed 转 `--err`（两端已有）。百分比列显示「—」时同时给 `title="总大小未知"`。

**indeterminate 形态**：总大小未知且状态为进行中时使用。此时引擎无法产出分段定位（`TaskRow.tsx:77-97` 的分段定位全部依赖 `total`），**multi 分段条不渲染，一律单条**——两形态并存规则：`total` 已知 → 现有单段/多段；`total` 未知且进行中 → indeterminate。动画规格（唯一实现，keyframes 命名带前缀防宿主冲突）：

```css
.bar.indet { position: relative; overflow: hidden; }
.bar.indet::after {
  content: ""; position: absolute; top: 0; bottom: 0;
  left: -30%; width: 30%; border-radius: inherit;
  background: linear-gradient(90deg, var(--accent), var(--violet));
  animation: adl-indet 1.4s linear infinite;   /* 单向循环，非 alternate */
}
@keyframes adl-indet { from { left: -30%; } to { left: 100%; } }
```

`prefers-reduced-motion: reduce` 下由 §4.4 全局 `animation: none` 停住动画，再用同选择器**只补定位属性**（与全局块无属性交集，不产生 `!important` 特异性裁决）：

```css
@media (prefers-reduced-motion: reduce) {
  .bar.indet::after { left: 35%; }   /* 静态 30% 灰条停在中位 */
}
```

### 3.6 对话框（modal）

- 遮罩 `rgba(0,0,0,.4)`；容器 `--r-xl`、560px、max-width 92vw、padding 18px 20px（沿用 styles.css:213-224）。
- 标题 `--fs-lg` 600；底部操作区 `modal-actions`：左侧放次要（识别/删除），右侧 spacer + 取消 + primary。
- **必须提供「取消」按钮**（设置对话框现状缺失，SettingsDialog.tsx:235-241）。
- 关闭途径三选全备：Esc、遮罩点击、取消/关闭按钮。表单类浮层（新建/设置）在**有未提交修改**时，Esc 与遮罩点击需先 `window.confirm("放弃未保存的修改？")`（修 SettingsDialog.tsx:60 静默丢弃）。
- 保存行为：保存失败 → 停留原地 + 框内 `--err-ink` 错误行（带原因），**不关闭**；保存成功 → 显示「已保存」并在 800ms 后自动关闭（修 SettingsDialog.tsx:56 的 500ms）。**取舍说明**：保留自动关闭是有意为之——「改一项 → 存 → 关」保持 1 次点击，符合路径最短；原缺陷的主风险（失败静默、遮罩误触丢改动）已由错误行不关闭与 dirty 确认消除；800ms 较 500ms 略缓以保证「已保存」可读。「取消」/Esc/遮罩恢复到基线。
- **dirty 判定**（「有未提交修改」的唯一定义）：当前表单值 ≠ 基线。新建对话框（无保存概念）基线 = 打开时初始值，dirty 范围 = `urls/dir/filename/checksum/mirrorText` 五项中任一非初始；设置对话框基线 = 载入值，**保存成功后基线重置为已保存值**（此后取消/Esc 不再提示）。
- 焦点：打开时 autoFocus 首字段（AddDialog.tsx:86 已有，设为标准）；Tab 循环困于浮层内（焦点陷阱）；关闭后焦点归还触发元素。

### 3.7 抽屉（详情）

520px，**从右侧滑入**（`transform: translateX(100%) → 0`，`--dur-slow --ease-out`；reduced-motion 退化为淡入）。现状 DOM 居中（styles.css:213-222 与 modal 共用类），改造时拆出 `.drawer` 独立定位。内容沿用 kv 表；底部操作区固定：重试（主）+ 打开文件夹 + 关闭，删除按 danger 层级放左侧。

### 3.8 toast / 横幅

沿用顶部 banner 条形态（不做浮动 toast，克制）。四语义，一律成对 token：

| 语义 | 底 | 字 | 用途 |
|---|---|---|---|
| info | `--accent-soft` | `--accent-ink` | 中性提示（现状 toast） |
| success | `--ok-soft` | `--ok` | 导入/导出/批量完成（带计数） |
| error | `--err-soft` | `--err` | 操作失败（带原因，5s 或手动关） |
| warn | `--warn-soft` | `--warn` | 剪贴板捕获、降级提示 |

无障碍：容器 `role="status" aria-live="polite"`（error 用 `role="alert"`）；「×」关闭按钮必须有 `aria-label="关闭"`。时长：成功 2.5s、错误 5s，均可手动关。

### 3.9 空状态（三分法）

`filtered.length===0` 必须区分三种（修 App.tsx:436-437 一律渲染仓库空文案）：

1. **真空**（`tasks.length===0`）：「暂无任务 — 点新建下载或用浏览器扩展推送」+ [新建下载] 按钮。
2. **筛选空**：「该筛选下暂无任务」+ [清除筛选]（重置 filter/catFilter）。
3. **搜索空**：「无匹配结果」+ [清除搜索]。

扩展端同理：popup 已区分「本页暂未嗅探到资源 / 该类型无资源」（popup.js:222/228），保持。

### 3.10 加载态

- 首屏列表未到达：3 行灰条骨架（`--border-soft` 底圆角块，P2）。**触发条件（唯一实现口径，见 A23/E9）**：仅「首次加载 + 列表为空」时显示，延迟 300ms 才出现（避免毫秒级 IPC 闪烁），数据到达即撤；轮询与局部刷新绝不显示骨架。
- 按钮级：disabled + 文案「…」。
- 轮询失败 ≠ 加载态：主程序离线显示空态 + 离线解释（见 §5 popup）。

---

## 4. 交互模式

### 4.1 四态统一处理

| 态 | 处理 |
|---|---|
| Loading | 按钮「…」/骨架行；已有数据时局部更新不闪空态 |
| Empty | §3.9 三分法，每态必带一个出口动作 |
| Error | **就近原则 + 固定行高优先**：行级操作 → error toast + 出错行 `.err-flash` 闪红 2s（`--err-soft` 底渐隐）——行内**不渲染错误文本**（56px 固定行高是虚拟滚动硬约束，文本通道不可靠；TaskRow.tsx:15-24 guard 的捕获与 3s 自动清除逻辑保留，仅反馈出口改为 toast，此为唯一权威口径，见 A18）；对话框/浮层内操作 → 容器内错误行；全局操作（导入/导出/剪贴板下载）→ error toast。所有 Promise 必须 catch，错误文案优先取 ERROR_TEXT 映射（popup.js:2-7） |
| Success | toast 带计数（「已导出 N 个任务」格式沿用）；批量操作汇总「成功 n · 重复 m · 失败 k」 |

### 4.2 键盘导航与 :focus-visible

- 一切可点元素用 `<button>`（或带 `role="button"` + `tabIndex=0` + Enter/Space 处理）：浮标按钮与面板 ✕ 必须从 div/span 改为 button（float-button.js:42-44、112-116）；应用排序表头从 `span onClick` 改 `<button>`（App.tsx:401-424）。
- 全局规则：`:focus-visible { outline: 2px solid var(--accent-ink); outline-offset: 2px; }`；禁止无替代的 `outline:none`。
- 浮层（modal/drawer/浮标面板）：Esc 关闭、Tab 焦点循环困于层内、关闭后焦点归还触发元素、打开时焦点落首交互元素。
- 排序表头用 `<button aria-sort="ascending|descending|none">`。
- 屏幕阅读器：动态结果区 `aria-live`；图标按钮必须有 `aria-label`；浮标面板容器 `role="dialog" aria-label="Anything Downloader 下载面板"`。

### 4.3 悬停 / 按压

hover：边框或底色转 accent 系（`--dur-fast`）；行 hover 底色 `--accent-soft`；按压 `opacity:.85`。不使用位移/缩放/新阴影层级。

### 4.4 过渡动效

只允许四类动效：进度条宽度（`--dur-med`）、浮层出入（`--dur-slow`）、hover 反馈（`--dur-fast`）、状态点脉冲（现有 `pulse 1.2s`，styles.css:183）。新增 indeterminate 动画同属进度类。全局兜底：

```css
@media (prefers-reduced-motion: reduce) {
  *, *::before, *::after {
    transition-duration: .01ms !important;
    animation: none !important;   /* 停动画必须用 none：.01ms 会让 infinite 动画（脉冲/indeterminate）每帧重启，等效高频闪烁 */
  }
}
```

`animation: none` 停掉 pulse/indeterminate 后由各自的专用静态规则补形态（§3.5 的 `.bar.indet::after` 只写定位属性，与全局块无属性交集；圆点脉冲停后状态色本身已表意，无需额外处理）。

### 4.5 危险操作的确认

| 级别 | 操作 | 保护 |
|---|---|---|
| 破坏性-批量 | 清空记录（App.tsx:380-397）、清空历史（sync.js:389-392） | **必须确认**，文案含数量与后果：「清空 N 条已完成记录？文件保留，记录不可恢复」 |
| 破坏性-单条 | 删除任务/账号/历史项 | 确认（应用端沿用 `window.confirm`，含对象名；sync 页用 `confirm()`）。**所有入口一致**——详情抽屉删除当前无确认（DetailDrawer.tsx:195-203），必须补齐 |
| 可恢复/幂等 | 暂停、取消、重试、推送 | 不确认，但必须给结果反馈（toast 计数） |

exe/apk/dmg/msi 完成态打开前：行内 sub 显示 `t("exeWarn")`（i18n.ts:103 已有，从未渲染），且「打开」前 `window.confirm(t("exeWarn"))`。

### 4.6 反馈时限约定

点击 → 100ms 内视觉响应；操作 → 立即 `refresh()` 不等轮询；错误 → 至少停留 3s（行内）/ 5s（toast），禁止「闪一下就没了」（修浮标失败提示 1.6s 抢关，float-button.js:231-236）。

---

## 5. 逐界面改造清单

### 5.1 应用端（桌面）

#### 全局（App.tsx / styles.css）

| # | 项 | 改法 | 级别 |
|---|---|---|---|
| A1 | 接通「打开所在文件夹」 | 完成态行内改为 [打开][文件夹][详情] 三按钮（文件夹 = mini 按钮 `t("openFolder")` → `api.revealTask(task.id)`，`api.ts:115` 现零调用）；**「删除」从行内移除、统一收进详情抽屉**（危险操作远离误触区，§4.5；「清空记录」批量入口保留在工具条且带确认）——守住 §3.3 行内 ≤3 约束 | **P0** |
| A2 | 「清空记录」加确认 | `window.confirm(\`${t("clearDone")} ${counts.done}? …\`)`，文案含数量与「文件保留、记录不可恢复」（§4.5） | **P0** |
| A3 | 新建对话框校验/镜像接线 | `doAdd` 把 `checksum`/`mirrors` 传入 `api.addTask`（`api.ts:96-103` 已支持）；单 URL 才启用两字段，多 URL 时禁用并注明「仅单链接生效」；若决定不做校验则删字段——**不允许保留摆设字段** | **P0** |
| A4 | 未捕获 Promise 全部接 guard→toast | 剪贴板「下载」（App.tsx:338-344，`task_exists` 用 `t("duplicate")`）、导出（224-226）、导入（238-241）catch 后 error toast；失败不出现成功 toast | **P1** |
| A5 | 列头语义修正 | 「操作」列头还原纯文本（去掉 onClick/title，App.tsx:418-424）；大小排序移到「进度」列头（409）；三个可排序列头改 `<button aria-sort>`，指示符用 `t("name"/"size"/"speed")`+▲▼，tooltip 用 i18n（现硬编码「按大小排序」「↕」） | **P1** |
| A6 | i18n 硬编码清零 | 「全部暂停/全部恢复」（App.tsx:294/308）、抽屉「优先级/高（30）/Dir/Size」（DetailDrawer.tsx:87/98-100/117/121）、设置「自动分类…」（SettingsDialog.tsx:203）、ErrorBoundary（22/27）、AddDialog「校验值/镜像链接/multi-thread」（88/94/116）全部回填 `t()`；**zh 与 en 同步新增键**（§6）；`document.documentElement.lang` 随语言设置（index.html:2 固定 zh-CN） | **P1** |
| A7 | exeWarn 接线 | 完成态且文件名匹配 `/\.(exe|msi|apk|dmg)$/i` 时行内显示 `t("exeWarn")`，「打开」前确认（§4.5） | **P1** |
| A8 | 浮层键盘/a11y 基线 | 三个浮层 Esc 关闭 + 焦点陷阱 + 归还；toast/banner `role="status"`；「×」加 `aria-label`（App.tsx:349-351）；排序头 `<button>`（同 A5） | **P1** |
| A9 | 暗色对比度 | 按 §2.1 替换 token：primary 按钮用 `--accent-strong`、选中侧栏/toast 文字用 `--accent-ink`（实测 5.17:1 / 8.11:1 达 AA） | **P1** |
| A10 | 空态三分 | 按 §3.9 改 App.tsx:436-437，加「清除筛选/清除搜索」按钮 | **P1** |
| A11 | indeterminate 进度 | 总大小未知且进行中 → `.bar.indet`（§3.5）；「—」加 title | **P1** |
| A12 | 详情抽屉实时化 | App.tsx:68 存 `detailId` 而非快照，渲染时从 `useSyncExternalStore` 快照按 id 取；store 事件已持续到达（store.ts:84-127） | **P1** |
| A13 | 重试语义统一与可见性裁剪 | 抽屉底部动作**按状态裁剪**：failed/canceled → [重试(主)]（`retryTask + startDownload`，与行内 TaskRow.tsx:136-141、批量 App.tsx:364-375 一致）；queued/paused → [开始(主)]；done → [打开文件][打开文件夹]；active → 无状态动作仅关闭。**所有动作按钮必须 catch → 容器内错误行**（DetailDrawer.tsx:187-193 现无 catch，失败静默） | **P1** |
| A14 | 批量添加 allSettled | AddDialog.tsx:52-72 改 `Promise.allSettled` 逐条尝试（单条失败不中断），结束后汇总「成功 n · 重复 m · 失败 k」+ `store.refresh()` + `onAdded()`；filename/checksum/mirrors 仅单 URL 启用（同 A3） | **P1** |
| A15 | 工具栏响应式 | `.toolbar` 加 `flex-wrap: wrap`（styles.css:124-129）；搜索框 `max-width: 220px; flex: 1 1 160px`；表格 grid 改 `minmax(120px,1fr) minmax(140px,300px) 80px 190px`（无条件生效，不挂断点）；**≤1024px 侧栏收窄至 48px 图标列**（title 提示全文）；**≤920px 隐藏速度列**（速度保留在行 title 与详情抽屉）。**断点硬约束：窗口 `minWidth: 860`（src-tauri/tauri.conf.json:17）——一切断点必须 >860，≤860 的媒体查询是死代码；若未来放宽 minWidth 须同步复核本组断点** | **P1** |
| A16 | 剪贴板横幅 | 渲染 `t("clipboardFound")` 作标题（i18n.ts:72 现闲置）；去掉与「忽略」重复的「×」（349-351）或反之，二留一；改 warn 语义配色（§3.8）；**「下载」按钮从 primary 降级为 default**（横幅出现时与工具栏「＋新建下载」同屏，primary 每容器 ≤1，§3.1） | **P2** |
| A17 | 批量操作反馈 | 全部暂停/恢复/重试（App.tsx:283-307/362-379）改 `Promise.allSettled` 并行，按钮期间 disabled+「…」，完成 toast「已暂停 N 个」 | **P2** |
| A18 | 行级错误显示 | opErr 不再渲染进 90px 速度列（TaskRow.tsx:118-122），消除撑破 56px 行高；反馈出口统一改为 **error toast + 出错行 `.err-flash` 闪红 2s**（§4.1 权威口径），TaskRow guard 保留捕获/3s 清除逻辑仅换出口 | **P2** |
| A19 | 双击行为统一 | 双击一律开详情（TaskRow.tsx:40-46 去掉 done 态分叉，打开文件走行内「打开/文件夹」按钮）；行 `title` 提示「双击查看详情」 | **P2** |
| A20 | 死代码清理 | 删 `.tabs/.tab` 样式（styles.css:140-147）、合并 ROW_H 双定义（App.tsx:59 / TaskRow.tsx:8）、删恒等三元（TaskRow.tsx:26）、删未用 i18n 键或按 A1/A6/A7 接线 | **P2** |
| A21 | 数值输入校验 | 设置数值项 blur/保存时 clamp（segments 1-32，限速 ≥0），非法红字（§3.2） | **P2** |
| A22 | nmMissing 文案 | 改用户视角：「未注册 — 请在桌面端重新安装/修复，或查看帮助文档」（zh+en 同步改 i18n.ts:111/221） | **P2** |
| A23 | 首屏骨架 | 仅在**首次加载且列表为空**时显示 3 行灰条：`store.refresh()` 首次完成前（`tasks.length===0`）挂载骨架，**延迟 300ms 才渲染**（`setTimeout` 守卫，避免毫秒级 IPC 闪烁），数据到达即取消；轮询与局部刷新绝不显示骨架（§3.10 同口径） | **P2** |

#### 侧栏（App.tsx:258-328）

- P1：A6 覆盖「全部暂停/全部恢复」硬编码；选中项文字色改 `--accent-ink`（A9）。
- P2：侧栏按钮补 `:focus-visible`（§4.2）。

#### 工具栏（App.tsx:208-256）

- P1：A4（导入/导出错误 toast）；A15（换行）。
- P2：搜索框 Esc 清空；总速度列加 `aria-live="off"`（高频变化不播报）。

#### 任务列表行（TaskRow.tsx）

- P0：A1（打开文件夹按钮）；A2 在 App 层。
- P1：A7（exeWarn）；A5/A8 表头部分。
- P2：A18；A19；done 态 sub 行追加完成时间（`updated_at`，P2 顺手项，可选）。

#### 新建对话框（AddDialog.tsx）

- P0：A3；A14。
- P1：Esc + 遮罩 dirty 确认（§3.6）；A6 文案。
- P2：识别结果改 chip 排布（文件名/大小/类型/分段各一枚）。

#### 详情抽屉（DetailDrawer.tsx）

- P1：A12（实时化）；A13（重试语义）；A6（「优先级/Dir/Size」回填）；状态行改「状态｜下载中」（首列固定 `t("状态标签")`——新增 `statusField` 键，zh+en；不再用状态译名当字段名，DetailDrawer.tsx:52-57）；删除加确认（§4.5）。
- P0：随 A1 加「打开文件夹」。
- P2：变体按钮换清晰度保持现状。

#### 设置对话框（SettingsDialog.tsx）

- P1：补「取消」按钮；保存 try/catch → 失败框内错误行不关闭、成功提示后 800ms 自动关闭（§3.6，含取舍说明）；遮罩/Esc dirty 确认；A21；A6（自动分类文案）。
- P2：A22。

### 5.2 扩展端

#### popup（popup.html / popup.js）

| # | 项 | 改法 | 级别 |
|---|---|---|---|
| E1 | 补齐缺失样式 | 定义 `.result-ok{color:var(--ok)} .result-skip{color:var(--err)}`（popup.js:389 现引用不存在的类，成败同色）；补 `.icon-btn`、`.settings/.set-title/.set-note/.active`（popup.html:192/229-247，形态见 §3.1） | **P0** |
| E2 | 轮询容错 + 离线态与自动恢复 | `renderTasks` 内 `send` 包 try/catch（popup.js:499-511）；连续失败 ≥2 次进入离线态：任务区显示「主程序未运行——启动后自动恢复连接」，暂停 1s 任务轮询，改以 **5s 间隔发 `type:"ping"` 探活**，任一次成功即恢复 1s 轮询、更新 pill 为「已连接」并立即 `renderTasks()`（此为文案「自动恢复」的兑现机制）；状态 pill 随每次轮询同步更新（修 popup.js:749-759 仅开窗检测一次，「已连接」永挂） | **P0** |
| E3 | 单任务速度修复 | popup.js:520 与 531 对同一任务调两次 `computeSpeed`（403-413），第二次 prev 已更新、dt≈0 恒 0——改为轮询循环内**每任务算一次**存 `Map<id,bps>`，总速求和与 `renderTask` 均读该 Map | **P1** |
| E4 | 批量计数修正 | popup.js:392：`ok` 已含 dup（373-379），`ok - fail + dup` 再加一次 dup——改为显式 `成功 ${ok}（含重复 ${dup}）· 失败 ${fail}` | **P2** |
| E5 | token 迁移 | `<style>` 换 §2.1 统一 token（对照表见 §2.2）；**填充型选中控件组件级映射**：`.tab2.active/.chip-f.active` 填充色改用 `--accent-strong`（白字两主题均 5.17:1；现状暗色 `#60a5fa` 仅 2.54:1，直接换 `--accent` 也只 3.68:1，见 §3.1）；body 408px 上限不动（popup.html:26） | **P1** |
| E6 | 粘贴框 autofocus | popup 打开即聚焦 `#paste-input`（HTML 加 `autofocus` 或 init 里 `focus()`，popup.html:204） | **P1** |
| E7 | 未运行引导 | pill「未运行」可点击 → 展开内联指引（启动桌面端；`app_not_registered` 文案改「在桌面端完成浏览器连接授权」口吻，替换 PowerShell 路径，popup.js:2-7） | **P1** |
| E8 | focus-visible / aria-live | 补 §4.2 全局 focus 规则（替换 popup.html:71 的 outline:none）；`#paste-result/#scan-result` 加 `aria-live="polite"` | **P2** |
| E9 | 首屏骨架 | 列表/任务区加载占位：仅首次加载且无数据时显示，延迟 300ms 出现、到达即撤（§3.10/A23 同口径） | **P2** |
| E10 | 30 条截断出口 | popup.js:516 硬截最近 30 条且无提示：`tasks-stats` 追加「共 N 条」；发生截断时列表尾部加弱化行「仅显示最近 30 条 · 全部任务请在主程序窗口查看」（不新建打开主程序的通道，克制） | **P2** |

#### sync 页（sync.html / sync/sync.js）

| # | 项 | 改法 | 级别 |
|---|---|---|---|
| S1 | 清空历史确认 | sync.js:389-392 加 `confirm("清空全部 N 条同步历史？不可恢复")` | **P0** |
| S2 | 暗色 + token 统一 | 删 `color-scheme: light`（sync.html:7）；整页换 §2.1 token（删第三套调色板 7-44）；字号 14px→13px 基准 | **P1** |
| S3 | 失败行样式 | sync.js:166-172 失败信息不再挂 `.lnk`（无 href 的假链接、hover 有下划线），改 `.st.failed` 红字 + `title` 全文；草稿链接保留 `.lnk` | **P1** |
| S4 | 平台勾选持久化 | 勾选变化写 `chrome.storage.local`（如 `syncTargets`），boot 渲染时恢复（sync.js:23-62 渲染、181 读取） | **P2** |
| S5 | toast a11y | `#toast` 加 `role="status" aria-live="polite"`（sync.html:170）；错误 toast 用 `role="alert"` | **P2** |
| S6 | focus-visible | 补 §4.2 全局规则（sync.html 现无任何 focus 规则） | **P2** |
| S7 | CMS 账号删除确认 | sync.js:312-316「删除」现直接 `removeCmsAccount` 无确认——按 §4.5 破坏性-单条补 `confirm("删除账号 …（站点地址）？不可恢复")`，与 S1、应用端删除口径一致（所有入口统一） | **P1** |

#### 浮标面板（float-button.js）

| # | 项 | 改法 | 级别 |
|---|---|---|---|
| F1 | 键盘可达 | 浮标 div → `<button aria-label="Anything Downloader">`（float-button.js:42-44），**内联样式必须补 button 复位**：`padding:0; border:none; box-sizing:border-box; line-height:1` 并显式声明 `background` 与 `font`——宿主页样式（如 Tailwind preflight 会重置 button 背景/边距/字号）会把 40px 圆形撑变形；面板 ✕ span → `<button aria-label="关闭">` 命中区 ≥28×28（112-116，同样复位）；面板 `role="dialog"`；Esc 关闭面板；Tab 在面板内循环 | **P0** |
| F2 | 样式隔离 + 双主题 | 面板容器改 `attachShadow({mode:"closed"})`；样式用**构造式样式表**注入：`const sheet = new CSSStyleSheet(); sheet.replaceSync(css); shadow.adoptedStyleSheets = [sheet]`——shadow 内 `<style>` 元素属于向文档注入样式节点，**受宿主页 CSP `style-src` 约束**（抖音/小红书/X 等站点可能禁内联样式，整块失效退回无样式比现状更脆），而 `new CSSStyleSheet()` 不经元素注入、不受 CSP 限制，Chromium 73+ 全量支持（本产品运行环境 Chromium 系，§6.5，无需 fallback）。token 值以硬编码副本写入并注释来源；按 `matchMedia("(prefers-color-scheme: dark)")` 切亮暗两组值（修 float-button.js:104 硬编码白底刺眼）。浮标按钮本体保持 inline style（不进 shadow，复位清单见 F1） | **P1** |
| F3 | 推送反馈修正 | 按钮计数随勾选实时更新（补 change 监听，修 196 固定总数）；0 项时按钮 disabled + title 说明（替代 202-204 静默 return）；失败文案带原因并**停留至用户关闭或 ≥6s**（修 231-236 的 1.6s 抢关 + 裸「推送失败」）；成功已有 8s（227）保留 | **P1** |
| F4 | 文章菜单失败反馈 | float-button.js:145-157 的 catch 在面板内显示错误行（复用 F2 的 err 色值，3s 自动消失），不再只 `console.warn` | **P1** |
| F5 | 探测超时 | 点击后 probe 加 5s race（对齐 popup.js:24-32 的 `send`），超时浮标显示「⌛」并 1.5s 复位（修 85-90 永久卡「…」）。**超时场景行为收敛**：探测期间浮标为 disabled 态（再次点击无效，不重复发 probe）；超时后**迟到的 probe 结果直接丢弃、不得应用**（不允许超时后突然弹面板）；用户再次点击 = 发起全新 probe | **P1** |
| F6 | 面板点击外部关闭 | document 上加一次性 click 监听（面板外点击关闭，配合 Esc） | **P2** |

---

## 6. 约束（必须遵守）

1. **i18n 双语同步（仅应用端）**：应用端任何新增/修改文案，`ui/src/i18n.ts` 的 zh 与 en 两本词典必须同时补齐（现 107/107 成对是基线，不得破坏）；键名沿用现有 camelCase 风格。本规范涉及的新键：`pauseAll`、`resumeAll`、`checksumLabel`、`mirrorLabel`、`clearFilter`、`clearSearch`、`statusField`、`sizeUnknown` 等，落地时统一补。
   **扩展端范围（v1 明确不做 i18n 框架）**：popup/sync/浮标维持中文文案——因此 §1 的「同一文案」跨端承诺收敛为「颜色、形态、状态语义」一致，英文界面仅应用端保证。扩展端新增文案必须集中到既有映射（popup.js 的 `ERROR_TEXT`/`STATUS_TEXT`/`ADAPTER_LABELS`），禁止新增散落字面量；若未来扩展端引入 i18n，键需与应用端词典同源对齐。
2. **popup 宽度上限 408px**（popup.html:26）：任何 popup 改版不得加宽；新增内容用纵向空间。
3. **浮标运行在任意网页**：样式必须内联（inline style 或 shadow 内 `<style>`），不得向宿主 document 注入全局样式/元素选择器规则；z-index 维持 `2147483646`；不得依赖宿主页的 CSS 变量或字体。
4. **sync.html 需补暗色模式**：随 §2.1 token 的 `prefers-color-scheme` 自动生效，不得保留 `color-scheme: light` 强制。
5. **运行环境为 Chromium 系**（Tauri WebView2 / Chrome 扩展 MV3）：可放心使用 `:focus-visible`、`color-mix()`、`overlay` 滚动条、`structuredClone`、Shadow DOM、`Promise.allSettled` 等现代特性；无需为 Safari/Firefox 兼容妥协。
6. **不重写业务逻辑**：虚拟滚动（App.tsx:179-184）、store 事件+轮询架构（store.ts）、适配器/同步引擎（extension/adapters、extension/sync/engine.js）、manifest 权限与注入清单（`<all_urls>` 的安装警告是通用嗅探的已知代价，本文档不改）一律不动。性能相关只在反馈层做（如 E2 失败后停止无谓轮询）。
7. **动效尊重 `prefers-reduced-motion`**（§4.4 兜底块为必选项，不是建议）。
8. **对比度下限**：正文文字 ≥4.5:1（AA），大号/图形 ≥3:1；所有新配色组合落地前按 §2.2 的实测方法（WCAG 相对亮度公式）验算。

---

## 7. 落地顺序建议

1. **第一批（token + P0）**：§2.1 token 四处落位（顺带解决 A9 对比度）→ A1/A2/A3、E1/E2、S1、F1。
2. **第二批（P1）**：应用端浮层键盘基线 A8 + 设置对话框行为（§3.6）→ A5/A6/A7/A10-A15 → E3/E5/E6/E7、F2-F5、S2/S3。
3. **第三批（P2）**：各端打磨项按清单执行，A20 死代码清理收尾。

每批完成后跑既有核验脚本（i18n 键配对、未用键 grep、对比度计算）回归。
