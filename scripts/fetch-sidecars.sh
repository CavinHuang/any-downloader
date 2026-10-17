#!/usr/bin/env bash
# 抓取当前平台的 yt-dlp / deno sidecar 到 src-tauri/binaries/<target-triple> 命名。
# 供 macOS/Linux CI（release.yml）使用；Windows 走 scripts/fetch-ytdlp.ps1。
# 用法: bash scripts/fetch-sidecars.sh
set -euo pipefail

TRIPLE=$(rustc -vV | sed -n 's/^host: //p')
BIN="src-tauri/binaries"
SUFFIX=""
case "$TRIPLE" in
  *windows-msvc) SUFFIX=".exe" ;;
esac

YTDLP_ASSET=""
DENO_ASSET=""
case "$TRIPLE" in
  x86_64-pc-windows-msvc)        YTDLP_ASSET="yt-dlp.exe";           DENO_ASSET="deno-x86_64-pc-windows-msvc.zip" ;;
  aarch64-apple-darwin)          YTDLP_ASSET="yt-dlp_macos";         DENO_ASSET="deno-aarch64-apple-darwin.zip" ;;
  x86_64-apple-darwin)           YTDLP_ASSET="yt-dlp_macos";         DENO_ASSET="deno-x86_64-apple-darwin.zip" ;;
  x86_64-unknown-linux-gnu)      YTDLP_ASSET="yt-dlp_linux";         DENO_ASSET="deno-x86_64-unknown-linux-gnu.zip" ;;
  aarch64-unknown-linux-gnu)     YTDLP_ASSET="yt-dlp_linux_aarch64"; DENO_ASSET="deno-aarch64-unknown-linux-gnu.zip" ;;
  *) echo "unsupported target triple: $TRIPLE" >&2; exit 1 ;;
esac

mkdir -p "$BIN"
fetch() { curl -sSL --retry 3 --fail -o "$2" "$1"; }

echo "==> yt-dlp ($TRIPLE)"
fetch "https://github.com/yt-dlp/yt-dlp/releases/latest/download/$YTDLP_ASSET" "$BIN/yt-dlp-$TRIPLE$SUFFIX"
chmod +x "$BIN/yt-dlp-$TRIPLE$SUFFIX"

echo "==> deno ($TRIPLE)"
TMP=$(mktemp -d)
fetch "https://github.com/denoland/deno/releases/latest/download/$DENO_ASSET" "$TMP/deno.zip"
unzip -oq "$TMP/deno.zip" -d "$TMP/deno-x"
# zip 内是 deno / deno.exe，统一改名为 <triple> 命名（tauri externalBin 约定）
mv "$TMP/deno-x/deno$SUFFIX" "$BIN/deno-$TRIPLE$SUFFIX"
chmod +x "$BIN/deno-$TRIPLE$SUFFIX"
rm -rf "$TMP"

echo "==> sizes"
ls -la "$BIN" | grep "$TRIPLE"
echo "sidecars ready for $TRIPLE"
