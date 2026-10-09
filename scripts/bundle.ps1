# 一键打包：构建全部二进制 → 复制 sidecar → tauri bundle（NSIS 安装器）
# 产物：target/release/bundle/nsis/*.exe（安装器含 dl-cli / dl-server）

$ErrorActionPreference = "Stop"
$Triple = "x86_64-pc-windows-msvc"

Write-Host "==> cargo build --release --workspace" -ForegroundColor Cyan
cargo build --release --workspace
if ($LASTEXITCODE -ne 0) { Write-Error "cargo build failed" }

Write-Host "==> copy sidecars" -ForegroundColor Cyan
New-Item -ItemType Directory -Force -Path "src-tauri\binaries" | Out-Null
foreach ($n in @("dl-cli", "dl-server")) {
    Copy-Item "target\release\$n.exe" "src-tauri\binaries\$n-$Triple.exe" -Force
    Write-Host "  $n-$Triple.exe"
}

Write-Host "==> cargo tauri build" -ForegroundColor Cyan
cargo tauri build
if ($LASTEXITCODE -ne 0) { Write-Error "tauri build failed" }

Write-Host ""
Write-Host "Done. Installer:" -ForegroundColor Green
Get-ChildItem "target\release\bundle\nsis\*.exe" | ForEach-Object { Write-Host "  $($_.FullName) ($([math]::Round($_.Length/1MB,2)) MB)" }
