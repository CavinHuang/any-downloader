# 打包浏览器扩展为可分发的 zip。
# 产物名固定为 anything-downloader-extension.zip（发布资产同名，landing 页经
# releases/latest/download 直链引用，勿改名）。解压后得到
# anything-downloader-extension/ 目录，在 chrome://extensions 「加载已解压的
# 扩展程序」直接选中即可。
# fixtures（adapters/ sync/ 下）仅本地 node 测试用，不入包。
$ErrorActionPreference = "Stop"
$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$Staging = Join-Path $env:TEMP "anything-extension-pack"
$Zip = Join-Path $Root "anything-downloader-extension.zip"

if (Test-Path $Staging) { Remove-Item $Staging -Recurse -Force }
New-Item -ItemType Directory -Force -Path (Join-Path $Staging "anything-downloader-extension") | Out-Null
robocopy (Join-Path $Root "extension") (Join-Path $Staging "anything-downloader-extension") /E /XD fixtures /NFL /NDL /NJH /NJS /NP
if ($LASTEXITCODE -ge 8) { Write-Error "robocopy failed with exit code $LASTEXITCODE" }

if (Test-Path $Zip) { Remove-Item $Zip -Force }
Compress-Archive -Path (Join-Path $Staging "anything-downloader-extension") -DestinationPath $Zip
Remove-Item $Staging -Recurse -Force

Get-Item $Zip | ForEach-Object {
    Write-Host "packed: $($_.FullName) ($([math]::Round($_.Length / 1KB)) KB)" -ForegroundColor Green
}
