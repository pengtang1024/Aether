# 法律法规合规审查（工程视角）

> 日期：2026-10-31　范围：Aether 0.3.0 开发树（Host、16 个第一方插件、浏览器扩展、
> 打包与更新）。
>
> **免责声明**：本文是工程团队按公开资料做的合规自查，不是法律意见，也不构成对
> 任何司法辖区的合规保证。正式商用、上架应用商店、进入 Linux 发行版或大规模
> 再分发前，应由具备相应资质的律师复核。审查假设项目同时面向欧盟（GDPR/EAA）、
> 美国（DMCA/出口管制）和中国大陆用户；其他辖区需另行评估。

## 1. 审查方法

- 读许可与隐私文档：`LICENSE`、`THIRD_PARTY_NOTICES.md`、`docs/LICENSING.md`、
  `PRIVACY.md`、`SECURITY.md`、`CODE_SIGNING_POLICY.md`、`deny.toml`。
- 用 `cargo metadata` 扫描 789 个依赖的 SPDX 许可表达式；检查 vendored Slint
  文件的 SPDX 头；检查打包脚本实际携带的许可文件。
- 检查 SQLite/设置文件的权限与持久化字段（凭据、Cookie、OAuth token）。
- 检查浏览器扩展的 manifest 权限、Cookie 授权与商店隐私声明。
- 核对 yt-dlp/ffmpeg/云盘 API/BT 做种/DRM 等高风险内容场景的文案与默认值。
- 检查发布流程的署名、SBOM、更新签名与撤回机制。

## 2. 结论摘要

| 级别 | 数量 | 说明 |
| --- | --- | --- |
| 🔴 高 | 4 | 隐私声明与实现不符、凭据数据库权限过宽、第三方许可全文缺失、Slint 署名缺口 |
| 🟡 中 | 7 | MPL 未入白名单、扩展隐私政策缺失、“sandbox”表述、BT/外部工具/商标提示、Debian copyright、CRA 准备 |
| 🟢 低/信息 | 5 | 出口管制、商标检索、EAA、CLA/DCO、依赖许可清单维护 |

本次已修复高风险的文档与权限问题（见 §4）；第三方许可全文已补齐到 27 份
（含 AWS-LC/ring/libssh2/QuickJS 复合文本），逐包版权行、OS keyring 仍未完成，
列为发布前待办。

## 3. 详细发现

### 🔴 H1. 隐私声明与实现严重不符（已修复）

`PRIVACY.md` 原文仍写着“0.1.0 是 UI mockup、不做下载网络请求”，并声称：

- OAuth token 存放在 OS keyring；
- 日志保留期可配置；
- 用户可以“撤销云盘 token”。

实际实现是：真实下载网络、token 存在任务设置/SQLite、日志固定 500 行、没有 token
管理界面。这属于对数据处理的虚假陈述，在欧盟 GDPR 透明度义务和消费者保护法下都有
风险。

**处理**：重写 `PRIVACY.md`，逐项描述本地数据、网络端点、扩展 Cookie、诊断脱敏、
删除方式与“未加密/无 keyring”的真实边界；同步修正 `SECURITY.md` 的过期安全姿态。

### 🔴 H2. 凭据数据库权限过宽（已修复）

`TaskStore::open` 直接 `Connection::open`，没有设置文件/目录权限；任务表的
`settings` 列保存 cookies、`Authorization`、代理密码、OAuth refresh token 等。
在常见 umask 022 下 `aether.db` 为 0644，同机其他用户可读取。

**处理**：

- 在 Unix 上创建/打开数据库前以 `0600` 建文件，打开后再次收紧，并把 `-wal`/
  `-shm`/`-journal` 一并设为 `0600`；
- 数据目录设为 `0700`；
- 新增 `task_database_and_data_dir_are_owner_only` 测试；
- 在 `PRIVACY.md` 明确“未加密、无 keyring”，不再做错误承诺。

**残留风险**：数据库仍是明文；拥有该用户账户/磁盘访问权的人可读取。建议后续提供
OS keyring 或对敏感字段做应用层加密，并在设置页提供“清除已保存凭据”。

### 🔴 H3. 二进制分发缺少第三方许可全文与逐包声明（部分修复）

发布包此前只带 `LICENSE`、`THIRD_PARTY_NOTICES.md` 和 Slint 许可全文。依赖树包含
Apache-2.0、BSD-2/3、ISC、BSL-1.0、Unicode-3.0、Zlib、MPL-2.0 等；Apache-2.0
第 4 条要求向接收者提供许可证副本（及上游 NOTICE，如有），MIT 要求保留版权与许可
声明。只给一份项目 MIT 文本不足以覆盖这些依赖。

**处理**：

- 新增 `LICENSES/` 下的 Apache-2.0、BSD-2-Clause、BSD-3-Clause、ISC、BSL-1.0、
  CDLA-Permissive-2.0、MPL-2.0、Unicode-3.0、Zlib 全文；
- Makefile / AppImage / macOS 打包改为复制整个 `LICENSES/`；
- `scripts/check-license-bundles.py` 现在要求每个产物包含全部许可文本；
- `THIRD_PARTY_NOTICES.md` 增加许可文本索引与 MPL 说明。

**已闭环**：`THIRD_PARTY_LICENSES.md` 由
`scripts/generate-third-party-licenses.py` 生成（591 个运行时包、379 份去重文本，
含每个 crate 的 LICENSE/COPYING/NOTICE 原文）；生成器 `--check` 与打包门禁会阻止
漂移。详见 [`LICENSE_AUDIT.md`](LICENSE_AUDIT.md) §8。

### 🔴 H4. Slint 署名缺口（已修复）

`docs/LICENSING.md` 选择 Slint Royalty-free 2.0 的 (b) 方案：在公开下载页展示
“Made with Slint”徽标。0.1.0/0.2.0 的 release notes 有徽标，但 **0.2.1、0.2.2 和
0.3.0 的 release notes 没有**；这两个版本已发布，存在署名义务缺口。

**处理**：给 0.2.1、0.2.2、0.3.0 的 release notes 补徽标；发布检查应把“release
notes 含徽标”纳入门禁，或改用应用内 `AboutSlint`（尚未实现）。

### 🟡 M1. MPL-2.0 未进入许可白名单（已修复）

`cargo metadata --all-features` 显示 `option-ext 0.2.0`（MPL-2.0，经
`directories`/`dirs-sys`，部分目标/特性）不在 `deny.toml` 的 allow 列表中。
MPL 是文件级弱 copyleft：未修改、保留许可文本并提供源码即可，但必须显式允许并
说明，否则 CI 的 cargo-deny 可能失败或漏检。

**处理**：`deny.toml` 允许 `MPL-2.0` 并注明义务；`THIRD_PARTY_NOTICES` 和
`LICENSES/MPL-2.0.txt` 记录来源与“未修改、源码在 crates.io”的说明。另行补齐
`i-slint-backend-linuxkms` / `i-slint-backend-testing` 的 Slint 例外。

**待确认**：本机未安装 `cargo-deny`，未能实跑；CI 的 `cargo-deny` job 是权威判定。
Windows 目标是否实际拉入 `option-ext` 需在 CI/`cargo tree --target` 复核。

### 🟡 M2. 浏览器扩展缺少商店要求的隐私政策（已修复）

AMO 要求隐私政策，Chrome Web Store 要求 Limited Use 披露。原 `store-listing.md`
只写“以仓库 PRIVACY.md 为起点”，没有可访问 URL，也没有 Limited Use 段落。

**处理**：新增 `browser-extension/PRIVACY.md`，在商店文案中给出 GitHub URL 和
Limited Use 声明；扩展仍然无远程代码、Cookie 权限可选且默认关闭。

### 🟡 M3. “sandboxed plugins” 表述不准确（已修复）

`packaging/debian/control.in` 的 Description 写 “sandboxed first-party plugins”。
当前只有进程隔离、路径校验和能力声明，没有 Landlock/seccomp/AppContainer 等 OS
沙箱；这是可能被认定为误导的宣传表述。

**处理**：改为 “process-isolated first-party plugins”。`SECURITY.md` 同步说明
“no OS-level sandbox yet”。README/发布文案如出现“sandbox/绝对安全”应同样改写。

### 🟡 M4. BT 做种与 IP 暴露提示不足（已补文档）

BT 会连接 tracker/DHT/peer 并暴露 IP；做种上传在部分辖区受版权法约束。代码默认
`bt_seed_ratio=0`（不做种）是正确方向，但用户手动开启时缺少显著提示；DHT 默认
开启也会扩大暴露面。

**处理**：新增 `docs/LEGAL_NOTICES.md` 的 BT 章节（IP 可见、做种授权、代理/屏蔽
列表不是法律豁免、恶意种子风险），并从 README 链接。

**待办**：考虑首次开启做种时的确认弹窗/提示；DHT 默认值是否需要改为关闭由产品
决定；补充 DMCA/侵权投诉联系人说明（当前项目不托管内容）。

### 🟡 M5. 外部工具与平台 ToS 提示（已补文档）

`extractor` 调 yt-dlp、`rtmp`/`media` 调 ffmpeg，但工具不随包分发。yt-dlp 是
GPL-3.0-or-later；ffmpeg 的许可以构建选项而定；YouTube 等平台条款通常禁止未授权
下载。Aether 不捆绑它们，规避了 GPL 传染与编解码器专利分发问题，但用户仍需自行
取得并遵守。

**处理**：`docs/LEGAL_NOTICES.md` 明确外部工具、平台条款、录制/肖像权与编解码器
专利责任；README 链接。

### ✅ M6. Debian copyright（已闭环）

新增 `packaging/debian/copyright`（DEP-5）：项目 MIT、vendored Slint 三选一、
第三方编译依赖的许可文本位置与 SBOM 指引；`make deb` 安装该文件。正式进
Debian/Ubuntu 前仍建议 ftp-master 预审。

### ✅ M7. EU CRA / 安全更新义务（工程侧已补）

欧盟《Cyber Resilience Act》对含数字元素产品分阶段生效（2026-09 漏洞报告，
2027-12 全面适用）。已补：

- `SECURITY.md` 增加支持周期（当前 + 上一个 minor）与 CRA 24h 早期预警 /
  72h 最终报告的目标流程；
- CI 增加每周 `schedule` 的 advisory/licence 扫描；
- 保留 SBOM、`cargo-deny --workspace check` 与构建溯源；
- 依赖升级后 `cargo deny --workspace check` 四项全绿。

正式合规仍需法务确认 CRA 适用性与报告主体。

### 🟢 L1. 出口管制与加密

使用 TLS、Ed25519、SHA-256 等标准密码学，依赖 ring/aws-lc 等公开库；作为公开
源代码/大众市场软件通常可适用豁免，但用户/分发者仍需遵守所在地出口管制与制裁
法律。当前无加密出口分类（ECCN）声明；如进入美国商业分发，建议法务确认
EAR 分类与公开源代码例外。**不是法律意见。**

### 🟢 L2. 商标与名义使用

插件名/描述使用 Google Drive、OneDrive、Dropbox、BitTorrent、YouTube、百度网盘、
阿里云盘、迅雷、Slint、Material 等商标，属于指示性/兼容性使用；未使用官方 Logo
暗示合作。已在 `docs/LEGAL_NOTICES.md` 加“无隶属/赞助/背书”声明。

**待办**：正式商业发布前对 “Aether” 名称做商标检索；如收到权利人异议，准备
改名/文案调整预案。

### 🟢 L3. 无障碍（EAA）与消费者保护

已做 AccessKit 语义标注，但未完成 NVDA/Orca 真机验证。若在欧盟作为消费者产品
销售，需评估 EAA/WCAG 2.1 AA 义务；发布文案避免“完全无障碍”“支持所有网站”等
绝对化表述。

### 🟢 L4. 贡献者许可（DCO）

新增 `CONTRIBUTING.md`：入站贡献按 MIT 接收，要求每个 commit 使用 DCO 1.1
sign-off（`git commit -s`），并说明第三方代码/依赖的许可登记流程。项目仍无 CLA；
若未来需要双许可/商业授权，应在此之前补 CLA 并取得历史贡献者授权。

### 🟢 L5. 依赖许可清单维护

依赖数量大（789 packages），许可表达式多样；`deny.toml` 已覆盖主流宽松许可，
但新增依赖时仍可能引入 MPL/EPL/CDDL 等弱 copyleft。建议：

- CI 固定跑 `cargo-deny check licenses advisories bans sources`；
- PR 模板要求新增依赖说明许可；
- 发布前生成完整第三方许可清单（见 H3）。

## 4. 本次已落地的修复

| 文件 | 修复 |
| --- | --- |
| `PRIVACY.md` | 按真实数据流重写；删除 keyring/可配置保留期等不实陈述 |
| `SECURITY.md` | 更新支持版本与“无 OS 沙箱/明文凭据”现状 |
| `crates/aether-host/src/store.rs` | 数据库/数据目录 0600/0700 + 测试 |
| `LICENSES/*` | 补齐 Apache/BSD/ISC/BSL/CDLA/MPL/Unicode/Zlib 全文 |
| `Makefile`、`.github/workflows/package.yml` | 打包复制全部许可文本 |
| `scripts/check-license-bundles.py` | 产物门禁要求全部许可文本 |
| `deny.toml` | 允许 MPL-2.0、补 Slint 例外 |
| `THIRD_PARTY_NOTICES.md` | 许可文本索引、MPL 说明、依赖清单说明 |
| `docs/releases/0.2.1|0.2.2|0.3.0.md` | 补 “Made with Slint” 署名 |
| `packaging/debian/control.in` | “sandboxed” → “process-isolated” |
| `docs/LEGAL_NOTICES.md`（新增） | 内容/BT/外部工具/云盘/商标/出口/责任提示 |
| `browser-extension/PRIVACY.md`（新增） | AMO/Chrome 隐私政策与 Limited Use |
| `browser-extension/store-listing.md` | 隐私政策 URL + Limited Use 披露 |

## 5. 发布前必须完成（按优先级）

1. ~~生成完整第三方许可包~~：已完成（`THIRD_PARTY_LICENSES.md`，591 包/379 文本，
   纳入全部产物与门禁）。
2. **凭据保护**：OS keyring 或应用层加密；至少提供“清除已保存凭据”入口（H2 残留）。
3. **Slint 署名门禁**：release notes 必须含徽标，或在应用内接 `AboutSlint`（H4）。
4. ~~Debian DEP-5 copyright~~：已完成。
5. **CRA/安全更新声明**：支持周期、漏洞报告 SLA、advisories 定时扫描（M7）。
6. **商标检索与名称预案**（L2）。
7. **法务复核**：以上均为工程自查，商用/上架前请律师按目标辖区复核。

## 6. 相关文件

- [`LICENSE`](../LICENSE)、[`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md)、
  [`LICENSES/`](../LICENSES/)
- [`docs/LICENSING.md`](LICENSING.md)、[`docs/RELEASING.md`](RELEASING.md)
- [`PRIVACY.md`](../PRIVACY.md)、[`SECURITY.md`](../SECURITY.md)
- [`docs/LEGAL_NOTICES.md`](LEGAL_NOTICES.md)
- [`browser-extension/PRIVACY.md`](../browser-extension/PRIVACY.md)
- [`deny.toml`](../deny.toml)、[`scripts/check-license-bundles.py`](../scripts/check-license-bundles.py)
