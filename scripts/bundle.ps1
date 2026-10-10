# 一键打包：构建全部二进制 → 抓取/复制 sidecar → tauri bundle（NSIS 安装器）
# 产物：target/release/bundle/nsis/*.exe
# （安装器含 dl-cli / dl-server / yt-dlp / deno——YouTube 功能开箱即用）

$ErrorActionPreference = "Stop"
$Triple = "x86_64-pc-windows-msvc"
$BinDir = "src-tauri\binaries"

Write-Host "==> cargo build --release --workspace" -ForegroundColor Cyan
cargo build --release --workspace
if ($LASTEXITCODE -ne 0) { Write-Error "cargo build failed" }

Write-Host "==> copy sidecars" -ForegroundColor Cyan
New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
foreach ($n in @("dl-cli", "dl-server")) {
    Copy-Item "target\release\$n.exe" "$BinDir\$n-$Triple.exe" -Force
    Write-Host "  $n-$Triple.exe"
}

# —— 内置 yt-dlp + deno（YouTube 必需：yt-dlp 解析 + deno 做 nsig 解签）。
#    带缓存：binaries 里已有同名文件则跳过下载；删掉对应文件即可强制刷新。
Write-Host "==> fetch bundled yt-dlp / deno" -ForegroundColor Cyan
$Yt = "$BinDir\yt-dlp-$Triple.exe"
$Deno = "$BinDir\deno-$Triple.exe"
$Headers = @{ "User-Agent" = "anything-downloader-bundle" }

if (-not (Test-Path $Yt)) {
    $url = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe"
    Write-Host "  downloading $url"
    Invoke-WebRequest -Uri $url -OutFile $Yt -UseBasicParsing -Headers $Headers
} else {
    Write-Host ("  yt-dlp cached ({0:N1} MB)" -f ((Get-Item $Yt).Length/1MB))
}
if (-not (Test-Path $Deno)) {
    $zip = "$env:TEMP\deno-bundle.zip"
    $url = "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip"
    Write-Host "  downloading $url"
    Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing -Headers $Headers
    $tmp = "$env:TEMP\deno-bundle-extract"
    if (Test-Path $tmp) { Remove-Item $tmp -Recurse -Force }
    Expand-Archive -Path $zip -DestinationPath $tmp -Force
    Copy-Item "$tmp\deno.exe" $Deno -Force
    Remove-Item $zip -Force
} else {
    Write-Host ("  deno cached ({0:N1} MB)" -f ((Get-Item $Deno).Length/1MB))
}
if (-not (Test-Path $Yt) -or -not (Test-Path $Deno)) { Write-Error "bundled sidecars missing" }
Write-Host ("  sizes: yt-dlp {0:N1} MB, deno {1:N1} MB" -f ((Get-Item $Yt).Length/1MB), ((Get-Item $Deno).Length/1MB))

Write-Host "==> cargo tauri build" -ForegroundColor Cyan
cargo tauri build
if ($LASTEXITCODE -ne 0) { Write-Error "tauri build failed" }

Write-Host ""
Write-Host "Done. Installer:" -ForegroundColor Green
Get-ChildItem "target\release\bundle\nsis\*.exe" | ForEach-Object { Write-Host "  $($_.FullName) ($([math]::Round($_.Length/1MB,2)) MB)" }
