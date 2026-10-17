# 发布与自动更新（GitHub Releases 分发）

桌面端基于 `tauri-plugin-updater` 实现自动更新，更新包以 GitHub Release 为分发源。

## 分发链路

```
main 合并版本号变更（或推送 tag v*）→ GitHub Actions（.github/workflows/release.yml）
  → tauri build（NSIS 安装包 + updater 签名产物 *.nsis.zip / *.nsis.zip.sig）
  → 创建 GitHub Release，上传安装包与 latest.json
  → 已安装客户端按 tauri.conf.json 的端点轮询：
    https://github.com/CavinHuang/any-downloader/releases/latest/download/latest.json
  → 版本更新 → 验签（内嵌公钥）→ 下载 → passive 静默安装 → 重启生效
```

客户端入口：

- 启动 4 秒后静默检查一次（失败不打扰），发现新版本在收藏列表上方挂横幅：下载 → 进度 → 「立即重启」。
- 设置 → 「05 / 关于与更新」可手动「检查更新」，同一状态机（`ui/src/updater.ts`）。

## 一次性配置（Repo Secrets）

更新包必须签名，客户端内嵌公钥验签（`tauri.conf.json → plugins.updater.pubkey`）。
签名密钥对已于 2026-10-17 生成（空密码）：

| 项 | 值 |
| --- | --- |
| 私钥文件 | `C:\Users\A\.tauri\any-downloader.key` |
| 私钥内容 | 配置到 GitHub Secrets：`TAURI_SIGNING_PRIVATE_KEY` |
| 私钥密码 | 空（`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` 留空或设空串） |
| 公钥 | 已内嵌 `tauri.conf.json` |

```bash
# 查看私钥内容，复制到 GitHub → Settings → Secrets and variables → Actions
cat ~/.tauri/any-downloader.key
```

**务必备份私钥**（建议离线保存一份）。私钥丢失后无法签发后续更新，
已装用户将收不到任何自动更新，只能手动卸载重装；更换密钥同理。

## 发布步骤

1. 在 PR 中将 `src-tauri/tauri.conf.json` 的 `version` 更新到新版本（如 `0.4.1`）。
2. 确认 CI 通过后将 PR 合并到 `main`。工作流只在配置文件版本变更时自动发布，`tauri-action` 根据配置创建相应的 `v__VERSION__` tag 和 GitHub Release。
3. 也可手动推送与配置版本相同的 `v*` tag，沿用原有发布流程。**切勿用不匹配的版本 tag 发布。**
4. Release 页核对安装包及签名、`latest.json`，再核对客户端自动更新端点。工作流采用 `releaseDraft: false`，成功后直接发布。

根 `Cargo.toml` workspace 版本可能独立于桌面产品版本，并非 updater 的判定依据。

## 更新包产物

每次发布会产出：

- Windows：`Anything.Downloader_<ver>_x64-setup.exe`（安装包）+ updater 签名产物
- macOS arm64：`.dmg` / `.app`（未签名，首启需右键 → 打开）+ `.app.tar.gz` + `.sig`
- Linux x64：`.AppImage` + `.AppImage.sig`
- `latest.json` — 多平台版本清单（windows-x86_64 / darwin-aarch64 / linux-x86_64）

三平台并行构建（release.yml 矩阵）。macOS 应用未签名——如需消除 Gatekeeper
提示，后续接入 Apple Developer ID 签名与公证（TAURI_APPLE 名下 Secrets）。

## 本地验证

```bash
# 一键打包（构建 dl-cli/dl-server + 抓取 yt-dlp/deno + NSIS 安装器；
# 需签名密钥在 TAURI_SIGNING_PRIVATE_KEY，或临时 export 后再跑）
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\bundle.ps1

# 仅验证 UI 与 Rust 编译
npm run build --prefix ui
cargo check -p dl-app
```

注意：`src-tauri/binaries/` 在 .gitignore 里，**不入库**。CI（release.yml）
按同样顺序自行准备 sidecar；本机 0 字节占位文件会被 fetch 脚本按
「<1MB 视为缺失」强制重新下载。

本地跑低版本 → 高版本升级链路：`npm run tauri build` 不够（需真实
Release），可直接推 `v0.0.0-test` 类预发布 tag 验证整条流水线后删除。
