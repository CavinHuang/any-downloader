# 抓取内置 yt-dlp + deno sidecar（bundle.ps1 的一步，也可单独跑）
# 用法: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\fetch-ytdlp.ps1
# 缓存：binaries 里已有同名文件则跳过下载；删除对应文件强制刷新。
$ErrorActionPreference = "Stop"
$Triple = "x86_64-pc-windows-msvc"
$Root = Split-Path -Parent $PSScriptRoot
$BinDir = Join-Path $Root "src-tauri\binaries"
New-Item -ItemType Directory -Force -Path $BinDir | Out-Null

$Yt = Join-Path $BinDir "yt-dlp-$Triple.exe"
$Deno = Join-Path $BinDir "deno-$Triple.exe"
$Headers = @{ "User-Agent" = "anything-downloader-bundle" }

if (-not (Test-Path $Yt) -or (Get-Item $Yt).Length -lt 1MB) {
    $url = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe"
    Write-Host "  downloading $url"
    curl.exe -sSL --ssl-no-revoke -L -o $Yt $url
    if ($LASTEXITCODE -ne 0 -or (Get-Item $Yt).Length -lt 1MB) { Write-Error "yt-dlp download failed" }
}
if (-not (Test-Path $Deno) -or (Get-Item $Deno).Length -lt 1MB) {
    $zip = Join-Path $env:TEMP "deno-bundle.zip"
    $url = "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip"
    Write-Host "  downloading $url"
    curl.exe -sSL --ssl-no-revoke -L -o $zip $url
    if ($LASTEXITCODE -ne 0 -or (Get-Item $zip).Length -lt 1MB) { Write-Error "deno download failed" }
    $tmp = Join-Path $env:TEMP "deno-bundle-extract"
    if (Test-Path $tmp) { Remove-Item $tmp -Recurse -Force }
    Expand-Archive -Path $zip -DestinationPath $tmp -Force
    Copy-Item (Join-Path $tmp "deno.exe") $Deno -Force
    Remove-Item $zip -Force
}

Write-Host ("  yt-dlp: {0:N1} MB  ({1})" -f ((Get-Item $Yt).Length/1MB), (& $Yt --version))
$denoVer = (& $Deno --version | Select-Object -First 1)
Write-Host ("  deno:   {0:N1} MB  ({1})" -f ((Get-Item $Deno).Length/1MB), $denoVer)
