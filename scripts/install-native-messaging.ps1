# Install the Aether Native Messaging manifest for the current Windows user.
# Usage: install-native-messaging.ps1 -ExtensionId <chrome-extension-id> [-HostPath <exe>]
#
# Chrome/Edge/Brave read the manifest path from HKCU; Firefox reads the JSON
# from %APPDATA%\Mozilla\NativeMessagingHosts. No admin rights needed.
param(
    [Parameter(Mandatory = $true)]
    [string]$ExtensionId,
    [string]$HostPath = (Join-Path $PSScriptRoot "..\target\release\aether-native-host.exe")
)

$ErrorActionPreference = "Stop"

$HostPath = (Resolve-Path $HostPath).Path
if (-not (Test-Path $HostPath)) {
    Write-Error "native host not found: $HostPath"
    exit 1
}

$manifestDir = Join-Path $env:LOCALAPPDATA "Aether\NativeMessagingHosts"
New-Item -ItemType Directory -Force -Path $manifestDir | Out-Null
$manifestPath = Join-Path $manifestDir "com.pengtang1024.aether.json"

$manifest = @{
    name           = "com.pengtang1024.aether"
    description    = "Aether native messaging host"
    path           = $HostPath
    type           = "stdio"
    allowed_origins = @("chrome-extension://$ExtensionId/")
} | ConvertTo-Json
Set-Content -Path $manifestPath -Value $manifest -Encoding UTF8

# Registry keys per browser; a missing browser just leaves its key in place.
$chromeBrowsers = @{
    "HKCU:\Software\Google\Chrome\NativeMessagingHosts\com.pengtang1024.aether"              = $true
    "HKCU:\Software\Microsoft\Edge\NativeMessagingHosts\com.pengtang1024.aether"             = $true
    "HKCU:\Software\BraveSoftware\Brave-Browser\NativeMessagingHosts\com.pengtang1024.aether" = $true
}
foreach ($key in $chromeBrowsers.Keys) {
    New-Item -Path $key -Force | Out-Null
    Set-Item -Path $key -Value $manifestPath
    Write-Host "registered $key"
}

# Firefox: manifest in the user profile, allowed_extensions instead of origins.
$firefoxDir = Join-Path $env:APPDATA "Mozilla\NativeMessagingHosts"
New-Item -ItemType Directory -Force -Path $firefoxDir | Out-Null
$firefoxManifest = @{
    name              = "com.pengtang1024.aether"
    description       = "Aether native messaging host"
    path              = $HostPath
    type              = "stdio"
    allowed_extensions = @("aether@pengtang1024.github.io")
} | ConvertTo-Json
Set-Content -Path (Join-Path $firefoxDir "com.pengtang1024.aether.json") -Value $firefoxManifest -Encoding UTF8
Write-Host "installed Firefox native manifest"

Write-Host "done. Load browser-extension/ as an unpacked extension and use its ID above."
