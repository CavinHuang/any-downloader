# 注册 Native Messaging Host（开发环境，Windows）。
# 用户级注册（HKCU），无需管理员；Chrome 与 Edge 一起注册。
#
# 用法：
#   powershell -ExecutionPolicy Bypass -File scripts\register-nm-host.ps1
#   powershell ... -Unregister   # 取消注册
#
# 前置：cargo build --release（需要 target\release\dl-server.exe 已存在）
# 扩展 ID 由 manifest.json 的 key 固定为 jeigecniniligfiigmmanjeiaooiiomc。

param(
    [switch]$Unregister
)

$ErrorActionPreference = "Stop"
$HostName = "anything_dl"
$ExtensionId = "jeigecniniligfiigmmanjeiaooiiomc"
$Exe = Join-Path $PSScriptRoot "..\target\release\dl-server.exe"

if ($Unregister) {
    foreach ($k in @(
        "HKCU:\Software\Google\Chrome\NativeMessagingHosts\$HostName",
        "HKCU:\Software\Microsoft\Edge\NativeMessagingHosts\$HostName"
    )) {
        if (Test-Path $k) { Remove-Item -Path $k -Recurse; Write-Host "removed $k" }
    }
    exit 0
}

if (-not (Test-Path $Exe)) {
    Write-Error "dl-server.exe not found. Run: cargo build --release"
}
$Exe = (Resolve-Path $Exe).Path

$Dir = Join-Path $env:APPDATA "anything-downloader\nm"
New-Item -ItemType Directory -Force -Path $Dir | Out-Null

# NM manifest：Chrome 每次连接时解析（改完无需重启浏览器）
$Manifest = @{
    name            = $HostName
    description     = "Anything Downloader native messaging host"
    path            = $Exe
    type            = "stdio"
    allowed_origins = @("chrome-extension://$ExtensionId/")
}
$ManifestPath = Join-Path $Dir "$HostName.json"
$Manifest | ConvertTo-Json | Set-Content -Encoding ASCII -Path $ManifestPath

# HKCU 注册（Edge 按 HKCU Edge -> Chromium -> Chrome 顺序回退查找，两处都显式写）
foreach ($k in @(
    "HKCU:\Software\Google\Chrome\NativeMessagingHosts\$HostName",
    "HKCU:\Software\Microsoft\Edge\NativeMessagingHosts\$HostName"
)) {
    New-Item -Path $k -Force | Out-Null
    Set-ItemProperty -Path $k -Name "(default)" -Value $ManifestPath
    Write-Host "registered $k -> $ManifestPath"
}

Write-Host ""
Write-Host "NM host registered for extension $ExtensionId"
Write-Host "Next: chrome://extensions -> Load unpacked -> select extension/"
Write-Host "      start app: target\release\dl-server.exe serve"
