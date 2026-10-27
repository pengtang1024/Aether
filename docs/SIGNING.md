# 代码签名与 macOS 公证

> **当前决策（2026-10）：暂不推进签名/公证，Linux 第一优先。** 不购买 Windows 证书，
> 也不购买 Apple Developer；Windows 保持未签名、macOS 保持 adhoc。本文保留完整方案，
> 等免费 OSS 签名（SignPath Foundation）通过或获得赞助后再接。
>
> 目标：Windows 安装包不再触发 SmartScreen 的“未知发布者”，macOS 通过 Gatekeeper/公证。
> **Windows 有免费 OSS 路径；macOS 公证没有免费方案**。没有证书/Apple 账号时只能产出
> 未签名/adhoc 包；v0.1.0 目前就是这种状态。

> **自动升级密钥不属于本文范围**：更新清单使用独立的 Ed25519 密钥，不属于
> 代码签名/公证。私钥在 `~/.ssh/aether-update-signing.key`（`0600`），公钥与
> 保管/轮换流程见 [UPDATE.md](UPDATE.md) §4.1。

## 0. 开源项目的现实路径（先看这里）

### Windows

| 方案 | 成本 | 说明 |
| --- | --- | --- |
| **SignPath Foundation** | 免费 | 证书签发给 SignPath Foundation，按 OSS 条件审核；需公开代码、OSI 许可、维护记录、代码签名政策/隐私页；publisher 显示为 SignPath Foundation |
| Certum Open Source | 约 €70–100 + 硬件/运费 | 低成本的 OSS 证书；不是免费；需身份验证 |
| Microsoft Store | 开发者账号费用 | 商店会为 MSIX 包签名；需审核；不是“直接下载 exe” |
| 未签名 + SHA256/winget | 免费 | 仍可能触发 SmartScreen；提供哈希、源码和构建说明 |
| 自签名证书 | 免费 | 不能解决 SmartScreen；只适合内部测试 |

**SignPath 的坑**：其 OSS 条件要求所有组件使用 OSI 许可且没有商业双许可。Slint 1.18.1 是
GPL-3.0 / Royalty-free 2.0 / 商业三选一；Aether 目前选择 Royalty-free 2.0（非 OSI），
可能不符合 SignPath Foundation 的免费证书条件。申请前应先向 SignPath 确认，或改走
Certum/Microsoft Store/自有证书/未签名路径。不要为了签名擅自把 Slint 改成 GPL 而忽略
整个项目的许可证影响。

### macOS

| 方案 | 成本 | 说明 |
| --- | --- | --- |
| Apple Developer Program + Developer ID + notarytool | **$99/年** | 真正公证的唯一正规路径；没有免费替代 |
| ad-hoc 签名（`codesign -s -`） | 免费 | Apple Silicon 必需；**不等于公证**，Gatekeeper 仍会拦截 |
| 源码构建 / Homebrew 本地编译 | 免费 | 用户本地构建，通常不带 quarantine；体验较差 |
| 非营利/教育/政府费用豁免 | 视资格 | Apple 对特定组织提供费用豁免；个人 OSS 通常不符合 |
| 赞助方/基金会代签 | 赞助 | 有 Apple Developer 账号的组织可代为签名/提交公证；需信任与合规安排 |
| Mac Admins Open Source（MAOS） | 免费 | 面向 Mac 管理工具；Aether 这种通用下载器通常不符合范围 |

**没有“免费公证”的公共 CA 或服务**。macOS 的 notarization 绑定 Apple Developer
身份；不接受“像 Windows 一样找个 OSS 证书”的路径。

### Aether 的建议

- Windows：先申请 SignPath Foundation；若因 Slint 许可/项目资历被拒，保持未签名 +
  SHA256 + winget，或使用低成本 Certum；不买 EV。
- macOS：保持 ad-hoc 签名 + 明确 Gatekeeper 指引；等赞助/组织账号再做公证。
- 不要声称“已签名/已公证”，除非 secrets 真实产生并通过 `signtool verify` / `spctl`。



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
- 当前 CI 不注入签名 secrets；在没有 secrets 时保持未签名/adhoc，且不把签名步骤设为必需。

## 5. 当前状态

- v0.1.0 Windows：未签名。
- v0.1.0 macOS：adhoc 签名、未公证；较新 macOS 上右键 Open 可能不再足够，
  需“系统设置 → 隐私与安全性 → 仍要打开”，或从源码构建。
- 签名/公证：**暂缓，不投入费用**。Linux 优先；Windows 只有 SignPath Foundation
  免费 OSS 路径值得尝试，macOS 需赞助/组织账号后再议。
- 发布说明必须明确“未签名/未公证”，不得暗示已通过 SmartScreen/Gatekeeper。
