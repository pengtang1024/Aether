# 代码签名与 macOS 公证

> 目标：Windows 安装包不再触发 SmartScreen 的“未知发布者”，macOS 通过 Gatekeeper/公证。
> **两者都需要外部凭据**，没有证书/Apple 账号时只能产出未签名/adhoc 包；v0.1.0 目前就是
> 这种状态。本文给出流水线与 secrets，凭据到位即可接入。

## 1. Windows Authenticode

### 1.1 需要的材料

- 代码签名证书：OV 或 EV Code Signing Certificate（PFX），或 Azure Trusted Signing /
  云 HSM。
- 时间戳服务（如 DigiCert/GlobalSign RFC 3161）。
- 证书私钥不能进仓库；只放 GitHub Actions Encrypted Secrets。

### 1.2 Secrets

| Secret | 内容 |
| --- | --- |
| `WINDOWS_CERT_PFX_BASE64` | PFX 的 base64 |
| `WINDOWS_CERT_PASSWORD` | PFX 密码 |
| `WINDOWS_TIMESTAMP_URL` | 可选，默认 `http://timestamp.digicert.com` |

### 1.3 签名顺序

1. `cargo build --release` 生成 `aether.exe`。
2. 签名 exe。
3. 制作 portable zip（zip 内的 exe 已签名）。
4. Inno Setup 生成 setup.exe。
5. 再签名 setup.exe（安装器本身也要签）。

```powershell
$pfx = [Convert]::FromBase64String($env:WINDOWS_CERT_PFX_BASE64)
[IO.File]::WriteAllBytes("cert.pfx", $pfx)
$signtool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" |
  Sort-Object -Descending | Select-Object -First 1
& $signtool.FullName sign /fd SHA256 /f cert.pfx /p $env:WINDOWS_CERT_PASSWORD `
  /tr $env:WINDOWS_TIMESTAMP_URL /td SHA256 target\release\aether.exe
& $signtool.FullName sign /fd SHA256 /f cert.pfx /p $env:WINDOWS_CERT_PASSWORD `
  /tr $env:WINDOWS_TIMESTAMP_URL /td SHA256 dist\*_setup.exe
& $signtool.FullName verify /pa /v target\release\aether.exe
Remove-Item cert.pfx
```

- 证书到期前轮换；EV 证书/Trusted Signing 对 SmartScreen 信誉更友好。
- x64 与 arm64 的 exe/setup 都要签。
- 签名失败应让 release job 失败，而不是继续产出“看似签名”的包。

## 2. macOS Developer ID + 公证

### 2.1 需要的材料

- Apple Developer Program 账号。
- Developer ID Application 证书（导出 `.p12`）。
- Apple ID app-specific password，或 App Store Connect API key。
- Team ID。

### 2.2 Secrets

| Secret | 内容 |
| --- | --- |
| `MACOS_CERT_P12_BASE64` | Developer ID Application 证书 p12 的 base64 |
| `MACOS_CERT_PASSWORD` | p12 密码 |
| `MACOS_KEYCHAIN_PASSWORD` | CI 临时 keychain 密码 |
| `MACOS_SIGN_IDENTITY` | 如 `Developer ID Application: Name (TEAMID)` |
| `APPLE_ID` | 公证账号 |
| `APPLE_APP_PASSWORD` | app-specific password |
| `APPLE_TEAM_ID` | Team ID |

### 2.3 流水线

```bash
# 临时 keychain
security create-keychain -p "$MACOS_KEYCHAIN_PASSWORD" build.keychain
security default-keychain -s build.keychain
security unlock-keychain -p "$MACOS_KEYCHAIN_PASSWORD" build.keychain
echo "$MACOS_CERT_P12_BASE64" | base64 --decode > cert.p12
security import cert.p12 -k build.keychain -P "$MACOS_CERT_PASSWORD" \
  -T /usr/bin/codesign
security set-key-partition-list -S apple-tool:,apple:,codesign: \
  -s -k "$MACOS_KEYCHAIN_PASSWORD" build.keychain

# universal app 已由 lipo 生成
codesign --force --deep --options runtime --timestamp \
  --sign "$MACOS_SIGN_IDENTITY" Aether.app

# DMG
hdiutil create -volname "Aether" -srcfolder Aether.app -ov -format UDZO Aether.dmg
codesign --force --timestamp --sign "$MACOS_SIGN_IDENTITY" Aether.dmg

# 公证 + staple
xcrun notarytool submit Aether.dmg \
  --apple-id "$APPLE_ID" --password "$APPLE_APP_PASSWORD" --team-id "$APPLE_TEAM_ID" --wait
xcrun stapler staple Aether.dmg
spctl --assess --type execute --verbose Aether.app
```

- 必须启用 **hardened runtime**；当前应用无 JIT/特殊 entitlement，先不需要额外 entitlements。
- 公证失败要阻断 release；不要发布“已签名但未公证”的 DMG 当作通过。
- 证书/账号轮换写进 release checklist。

## 3. Linux（可选）

- `.deb`/AppImage 可加 GPG detached signature 或仓库签名；当前未启用。
- 至少发布 `SHA256SUMS`（已实现）。

## 4. CI 接入方式

- 签名步骤通过 secrets 是否为空决定：无凭据时保持现有未签名/adhoc 构建，并在 release
  notes 明确提示；有凭据时签名失败必须失败。
- 建议把 Windows 签名放在 exe 构建后、zip/Inno 之前/之后；macOS 放在 lipo/icns 之后、
  DMG 创建之后。
- Secret 只在 step env 中注入，禁止 `echo`/日志打印；临时证书文件在 job 结束前删除。

## 5. 当前状态

- v0.1.0 Windows：未签名。
- v0.1.0 macOS：adhoc 签名、未公证。
- 签名/公证流水线目标：P1.5（HTTP 插件完成后即可接入）。
