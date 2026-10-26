# 盲区审计：还没想到/还没安排的事

> 日期：2026-10-26。基于当前 Host + 第一方插件方案、PC/ARM64 打包与 UI 重设计计划。
> 目的不是否定现有方案，而是把“上线一个真正能用的下载管理器”还缺的东西列全。
> **当前优先级：Linux 第一。** 签名/公证不投入费用；Windows/macOS 保持可构建、未签名/adhoc。

## 0. 先修/已修（本轮）

- **Slint 许可归属**：项目使用 Slint 1.18.1 的 **Royalty-free 2.0** 选项。该许可要求
  AboutSlint 或公网下载页的 “Made with Slint” 徽标；已在 v0.1.0 release notes 补徽标，
  并在 `THIRD_PARTY_NOTICES.md` 明确选择。后续 About 对话框接入 AboutSlint。
- **撤回不合规旧发布**：v0.1.0 的二进制未携带完整 Slint 许可全文，已删除全部
  release 资产；tag 保留用于溯源。记录见 `WITHDRAWN_RELEASES.md`，发布门禁加入
  `scripts/check-license-bundles.py`。
- **PR CI**：新增 `.github/workflows/ci.yml`（Linux fmt/build/test，Windows/macOS check），
  不再只有 release 分支才跑构建。
- **安全披露**：新增 `SECURITY.md`。
- **依赖更新**：新增 Dependabot（cargo + GitHub Actions）。
- **SignPath/隐私页**：新增 `CODE_SIGNING_POLICY.md` 与 `PRIVACY.md`，为免费 Windows OSS
  签名申请和正式隐私说明做准备；macOS 公证仍无免费路径。

## 1. 高优先级盲区

| # | 盲区 | 影响 | 建议 |
| --- | --- | --- | --- |
| 1 | **Slint/第三方许可合规** | 法律风险，可能违反 Royalty-free 署名条件；也可能影响 SignPath 免费签名资格 | 发布页保留 badge / AboutSlint；补全 notices；正式发布前法务 review；向 SignPath 确认 Slint 三许可是否符合条件 |
| 2 | **自动更新** | 用户长期停留在旧版；签名/安全修复无法下发 | 签名更新元数据、增量/全量包、分阶段发布、失败回滚；Windows/macOS/Flatpak 各自通道 |
| 3 | **单实例 / 托盘 / 后台 / 开机自启** | 下载管理器必须能在后台跑；当前 UI 只有窗口 | 单实例锁、关闭到托盘、托盘菜单、开机启动、系统关机/休眠处理、唤醒续传 |
| 4 | **PR CI 与测试** | 没有质量门禁，协议/插件改动容易回归 | 已有基础 CI + P0 IPC/契约测试；继续补 HTTP range/ETag、断点、路径安全、崩溃恢复测试 |
| 5 | **持久化/迁移/备份** | 升级后任务/设置/插件状态丢失 | SQLite v1 已实现任务/文件/设置/日志；备份恢复、设置迁移、导入/导出待补 |
| 6 | **Accessibility** | 屏幕阅读器/键盘用户不可用 | Slint 开启 `accessibility` feature；自定义 TaskItem/FilterChip 补 role/label/focus；高对比/减少动效 |
| 7 | **i18n/本地化** | 目前所有文案硬编码中文 | 字符串资源、英文优先、日期/大小/速度格式化、RTL、错误消息本地化 |
| 8 | **隐私与凭据** | 浏览器 Cookie、云盘 token、代理密码、日志 | 系统 keyring、日志脱敏、隐私政策、诊断包可选、无默认遥测、数据保留/清除 |
| 9 | **安装/升级/卸载/协议关联** | 升级残留、卸载不干净、magnet 打不开 | 每用户/每机器安装、升级迁移、卸载清理、桌面/开始菜单、magnet/aether/thunder 协议注册 |
| 10 | **文件系统边界** | 数据损坏/任务失败 | 跨文件系统 rename 回退 copy+fsync、FAT32 4GiB、网络盘、大小写冲突、长路径、外删/外改、磁盘满 |
| 11 | **BT 的法律与安全** | 做种上传、版权、IP 暴露、恶意种子 | 做种默认策略/比率、版权提示、代理/VPN、IP blocklist、端口映射提示、恶意路径防护 |
| 12 | **插件供应链** | 第一方插件被替换/降级/注入 | 签名 manifest、key 轮换/吊销、版本 pin、回滚、插件更新独立签名、Host/API 兼容矩阵 |
| 13 | **构建供应链** | Actions/依赖被篡改 | Actions pin SHA、`cargo audit`/`cargo deny`、SBOM、可复现构建、release provenance、secret scanning |
| 14 | **性能/低资源预算** | 1000 任务/多插件时内存/句柄爆炸 | 明确内存/CPU/FD 预算；插件进程配额；基准测试；电池/计量网络；磁盘 IO 优先级 |
| 15 | **可观测性与支持** | 用户报障无从下手 | crash dump（opt-in）、日志轮转/脱敏、诊断包、错误 ID、支持流程、隐私说明 |

## 2. 协议/功能层面的盲区

- **HTTP**：Digest/NTLM/Kerberos、OAuth2 刷新、mTLS、PAC 认证、Cloudflare/验证码（不做绕过）、
  签名 URL 过期、HTTP/3 回退、服务器忽略 Range/ETag 的兼容矩阵。
- **BT**：BT v2/pure v2、WebTorrent/WebRTC、web seeds、private、DHT bootstrap、IPv6、
  uTP/加密、端口映射失败、做种比率/时间、代理 DNS 泄漏、IP blocklist、恶意种子。
- **HLS/DASH**：直播窗口、discontinuity、字幕/多音轨、AES-128、fMP4、DRM（不做）。
- **FTP/SFTP/WebDAV/S3**：主动/被动、编码、REST、主机密钥、WebDAV lock、S3 multipart、
  云厂商专有 API。
- **浏览器集成**：SPA/service worker 下载、blob: URL、HLS 站点、Cookie/Referer 授权、
  商店审核、企业策略、无痕模式。
- **网盘**：OAuth 审核/配额、共享链接权限变化、token 吊销、百度/阿里开放平台可用性；
  不做 Cookie 抓取/绕过风控。
- **PAC**：PAC 是代码（JS）——沙箱、超时、无文件/网络、系统 PAC/WPAD、代理认证、
  网络切换刷新、DNS 泄漏。

## 3. 产品/UX 盲区

- 首次启动引导、默认下载目录、插件安装/启用向导。
- 任务导入/导出、从 Motrix/浏览器/其他下载器迁移。
- 批量 URL、拖拽、剪贴板监听、RSS/自动追番（P5）。
- 任务多选、批量暂停/删除、队列拖拽排序、分组/标签。
- 同名文件策略（覆盖/重命名/跳过）、路径模板、下载前磁盘检查。
- 通知：系统通知权限、点击跳转、免打扰、完成后关机。
- 窗口：多显示器 DPI、记忆窗口位置/大小、最小化到托盘、关闭确认。
- 插件管理：权限解释、更新失败回滚、插件禁用时任务的处理。
- 任务详情 schema 的未知 section 兼容、插件卸载后的历史任务展示。
- 错误文案：可操作建议、复制诊断 ID、重试/换插件。

## 4. 分发/运维盲区

- 自动更新（Host/插件）与更新服务器；灰度/回滚。
- Flatpak/Flathub、AppImage 更新信息、`.deb` apt 仓库、GPG 签名。
- Windows 每用户 vs 每机器安装、ARM64 安装器、SmartScreen 信誉、卸载残留。
- macOS 公证/签名、App Store 政策、universal 插件、Gatekeeper。
- 协议关联（magnet/aether/thunder/ed2k）和默认下载器设置。
- 便携模式、配置目录选择、备份/恢复。
- 版本兼容矩阵：Host/plugin API、插件降级、旧任务恢复。

## 5. 法律/合规盲区

- **Slint Royalty-free 2.0 署名**（badge/AboutSlint）与第三方许可清单。
- ffmpeg/yt-dlp/librqbit/libtorrent/云 API 的许可证与再分发条款。
- BitTorrent 做种/上传的版权与用户责任；DMCA/投诉流程。
- 云盘/浏览器集成的服务条款、OAuth 审核、隐私政策。
- “Aether” 名称/Logo 商标检索；用户提供的 logo 权利确认。
- 隐私政策、GDPR/数据删除、遥测 opt-in、crash report 上传。
- 出口/加密合规（rustls/TLS）一般无需特殊处理，但需确认分发地区。

## 6. 工程/质量盲区

- 单元/集成/模糊/soak/性能/升级/签名验证测试；P0 已有 SDK/IPC/契约测试，协议与 UI 测试仍缺。
- CI 只覆盖构建；需要协议 fixture、代理/PAC、路径 fuzz、插件 crash/restart。
- 依赖审计、SBOM、可复现构建、Actions pin、secret scanning、代码扫描。
- 错误分类/重试/取消安全/背压；panic 边界；插件进程泄漏。
- DB 迁移/备份/损坏恢复；日志轮转；文件句柄泄漏。
- 跨平台运行时测试（不仅交叉编译）：Windows ARM64、Linux ARM64、macOS Intel。
- 安装器测试：升级/卸载/协议关联/权限/无管理员安装。

## 7. 需要用户/外部条件的事

| 事项 | 需要什么 |
| --- | --- |
| Windows 签名（暂缓） | SignPath Foundation 免费 OSS 证书（需审核/确认 Slint 许可）；不买证书 |
| macOS 公证（暂缓） | Apple Developer Program（$99/年）+ Developer ID + notary 凭据；无免费替代 |
| 浏览器扩展上架 | Chrome/Edge/Firefox 开发者账号 + 审核 |
| 云盘 OAuth | 各厂商开发者应用/API 权限/审核 |
| 自动更新服务器 | 更新元数据签名 key、托管/CDN、灰度策略 |
| 法律 review | Slint/第三方许可、BT/云盘/隐私政策 |
| 隐私政策/遥测 | 产品决策：是否收集、收集什么、保留多久 |

## 8. 建议的下一批动作（不依赖外部凭据）

1. 把 AboutSlint/“Made with Slint” 放进 About 对话框和 README（发布页 badge 已补）。
2. 实现单实例、托盘、关闭到托盘、开机自启、休眠/关机处理。
3. 建立 Host/Plugin P0 + HTTP 插件 P1；用 fake plugin 跑通生命周期。
4. 补 PR CI 测试 fixture、路径 fuzz、配置/DB 迁移。
5. 完成设置/任务 schema 与导入/导出。
6. 无障碍与 i18n 基建（Slint accessibility feature、字符串资源）。
7. 自动更新/签名方案与密钥管理设计。
8. 隐私政策、SECURITY.md、威胁模型、SBOM/cargo-deny。

> 结论：协议广度只是“下载器”的一部分；真正上线还缺**许可、更新、后台形态、测试、迁移、
> 无障碍、隐私、供应链和安装升级**这些非功能能力。它们应该和插件开发并行，而不是最后补。
