# 许可证深度审计

> 日期：2026-10-31　范围：工作区 25 个成员、全部 release 目标（Linux x86_64 /
> Windows x86_64 / macOS arm64）、vendored UI、随包 C 代码、浏览器扩展与打包产物。
>
> **不是法律意见。** 供工程整改与法务复核使用。

## 1. 方法与可复现命令

```bash
cargo metadata --format-version 1 --all-features      # 789 个包（含 build/dev）
cargo tree --workspace --target <target> --edges normal # 运行时依赖
python3 scripts/generate-third-party-inventory.py      # 生成 591 行清单
cargo deny --workspace check licenses bans sources     # 全工作区许可/来源
cargo deny --workspace check advisories                # 告警（见 §7）
python3 scripts/check-license-bundles.py --check-recipes
```

- 运行时依赖按目标取并集：**591 个**（Linux 530 / Windows 468 / macOS 500）。
- 许可全部可解析，**没有 UNKNOWN / license_file 缺失**。
- `cargo deny --workspace check licenses bans sources` 通过。

## 2. 许可谱系

按 SPDX 原子统计（OR/AND 表达式拆分，非包数）：

| 许可 | 原子出现次数 | 义务类型 | 本项目处理 |
| --- | ---: | --- | --- |
| MIT | 689 | 保留版权+许可 | 全文 + 清单 |
| Apache-2.0 | 519 | 许可副本 + NOTICE | 全文 + moka NOTICE |
| Zlib | 29 | 保留声明 | 全文 |
| Unicode-3.0 | 28 | Unicode 数据声明 | 全文 |
| BSD-3-Clause | 14 | 保留版权/免责声明 | 全文 |
| GPL-3.0-only | 14 | 仅作为 Slint 三选一未选分支 | 不触发 |
| Slint RF 2.0 | 14 | 署名/禁止单独分发/禁止暴露 API | 见 §4 |
| ISC | 13 | 保留版权/许可 | 全文 + 复合文本 |
| Unlicense | 12 | 公共领域式 | 可选举 MIT |
| LLVM-exception | 9 | Apache 例外 | Apache 文本 |
| BSD-2-Clause | 3 | 保留版权 | 全文 |
| BSL-1.0 | 3 | 保留声明 | 全文 |
| CC0-1.0 / MIT-0 / 0BSD / BSD-1-Clause | 各 1–2 | 公共领域式 | 可选举 MIT/Apache |
| **MPL-2.0** | **1** | 文件级 copyleft + 源码可获取 | 见 §5 |
| CDLA-Permissive-2.0 | 1 | 数据许可 | 全文 |

结论：**没有 GPL/AGPL/SSPL 传染风险**；唯二的“非纯宽松”是 Slint（选择 RF 2.0）
和 MPL-2.0 的 `option-ext`。

## 3. 关键依赖路径

| 组件 | 许可 | 是否随二进制 | 说明 |
| --- | --- | --- | --- |
| `slint` 及 10 个 `i-slint-*` 运行时 crate | GPL-3.0-only OR RF-2.0 OR Software-3.0 | 是 | 选择 RF 2.0；见 §4 |
| `option-ext` ← `dirs-sys` ← `directories` ← `librqbit-core` ← `librqbit` ← BT 插件 | MPL-2.0 | **是，三平台** | 未修改；源码在 crates.io；MPL 全文随包 |
| `r-efi` | MIT OR Apache-2.0 OR LGPL-2.1-or-later | Windows 目标可能 | 选择 MIT/Apache，不触发 LGPL |
| `aws-lc-sys` | ISC AND (Apache-2.0 OR ISC) AND Apache-2.0 AND MIT AND BSD-3-Clause … | 是（HTTP/云盘 TLS） | 复合 C 许可，见 §6 |
| `ring` | Apache-2.0 AND ISC（含 BoringSSL/once_cell/fiat） | 是 | 复合文本，见 §6 |
| `libssh2-sys` | 壳 MIT/Apache，内嵌 libssh2 BSD-3 + 额外 | 是（SFTP） | 见 §6 |
| `rquickjs-sys` | MIT（QuickJS） | 是（daemon） | 见 §6 |
| `rusqlite` / `libsqlite3-sys` | MIT + SQLite public domain | 是 | 见 §6 |
| `moka` | (MIT OR Apache-2.0) AND Apache-2.0 | 是（HTTP 族 DNS 缓存） | 有额外 NOTICE |
| `webpki-roots` | CDLA-Permissive-2.0 | 是 | 证书数据 |

## 4. Slint Royalty-free 2.0 合规

选择依据：`docs/LICENSING.md`、`THIRD_PARTY_NOTICES.md`，按 RF 2.0（非 GPL 分支）。

| 条款 | 要求 | 现状 |
| --- | --- | --- |
| §2 署名 | AboutSlint 或公开下载页徽标 | 0.1.0/0.2.0 有；0.2.1/0.2.2/0.3.0 已补“Made with Slint” |
| §1 集成 | 只能作为 Application 一部分分发 | 桌面应用，未单独分发 Slint |
| §3 禁止 | 不得用于 Embedded System、不得暴露 Slint API、不得单独分发 | 桌面应用；未暴露 API |
| §3 通知 | 不得删除/修改版权、许可、免责声明 | `ui/material` 45/45 保留 SPDX 与版权头（44 个 SixtyFPS + 1 个 KDAB） |
| 修改 | 允许修改，但保留通知 | 有本地色板/控件补丁，头未动 |

风险与待办：

- **发布页是唯一署名载体**：GitHub Release 正文必须来自含徽标的
  `docs/releases/<version>.md`；0.2.1/0.2.2 已发布版本若 Release 正文未更新，
  仍应补上或改用应用内 `AboutSlint`。
- RF 2.0 不是 OSI 许可：进入 Debian/Ubuntu 官方仓库大概率不符合 DFSG；Flathub/
  AUR 可逐案审核。
- SignPath 免费签名资格需确认 RF 2.0 是否满足其 OSS 条件。

## 5. MPL-2.0（`option-ext`）

- 路径：`librqbit-core -> directories -> dirs-sys -> option-ext`，**Linux/Windows/
  macOS 三个运行时图都包含**（不是仅 Windows）。
- 义务：MPL 是文件级 copyleft；只要不改 MPL 文件、保留许可文本、并提供源码获取
  方式，即可与 MIT 应用一起分发。
- 现状：未修改 `option-ext`；`LICENSES/MPL-2.0.txt` 随包；
  `THIRD_PARTY_NOTICES.md` 写明版本与 crates.io 源码位置。
- 残余：逐 crate 的版权行未单独列出（见 §8）。

## 6. 随二进制编译的 C 代码

这是本次审计最重要的补充：`Cargo.toml` 的 SPDX 字段无法表达这些内嵌代码的
完整许可链。

| 组件 | 内嵌来源 | 已加入的全文 |
| --- | --- | --- |
| `aws-lc-sys` | AWS-LC + BoringSSL + OpenSSL + mlkem-native + mldsa-native + 其它 | `AWS-LC-LICENSE.txt`（20 KB 复合文本） |
| `ring` | BoringSSL、once_cell、fiat | `ring-LICENSE*.txt`、`ring-once_cell-*.txt`、`ring-fiat-LICENSE.txt` |
| `libssh2-sys` | libssh2 + curl/FSFULLR/SAX-PD 片段 | `libssh2-COPYING.txt` + 6 个 `libssh2-*.txt` |
| `rquickjs-sys` | QuickJS（Fabrice Bellard 等 4 位版权人） | `QuickJS-LICENSE.txt` |
| `rusqlite` | SQLite（public domain blessing） | `SQLite-blessing.txt` |
| `moka` | 两个文件仅 Apache-2.0 | `moka-NOTICE.txt` |

`rquickjs-sys` 的 `vendor/wasi-libc` 只用于 wasm 目标，当前 release 目标不编译，
未纳入二进制；如未来发布 wasm，需要补 wasi-libc 的 NOTICE。

## 7. 分发产物矩阵

| 产物 | LICENSE | Notices | 逐包文本 | LICENSES/ | 插件包 |
| --- | --- | --- | --- | --- | --- |
| `.deb` | DEP-5 `copyright` | 是 | `THIRD_PARTY_LICENSES.md` | `usr/share/doc/aether/licenses/*` | 每个插件带全套 |
| AppImage | 是 | 是 | 是 | `usr/share/doc/aether/licenses/*` | 同上 |
| Windows zip | 是 | 是 | 是 | `LICENSES/` | 同上 |
| Windows 安装器 | 是 | 是 | 是 | `LICENSES\*` | 同上 |
| macOS DMG | 是 | 是 | 是 | `Contents/Resources/licenses/*` | 同上 |
| 浏览器扩展 zip | 是 | n/a | n/a | n/a | n/a |
| 插件独立更新包 | 是 | 是 | 是 | 是 | n/a |

`scripts/check-license-bundles.py` 现在：

- 校验 `LICENSES/` 每个文本非空、`THIRD_PARTY_LICENSES.md` 存在；
- 用 `--check` 模式验证两个生成器产物（inventory + per-crate texts）没有漂移；
- 校验每个 `docs/releases/*.md` 带 “Made with Slint”（发布正文门禁）；
- 打开 `.deb`/AppImage/zip/DMG，要求 LICENSE、Notices、逐包文本和每个许可文本都存在且非空；
- 校验插件 staging 脚本携带 LICENSE/Notices/逐包文本/LICENSES。

## 8. 发现与整改

### 🔴 H1. 之前的 `cargo deny` 只审计根包（已修复 CI 拆分）

`Cargo.toml` 的 `default-members = ["."]` 让 `cargo deny check` 只以根包为图根；
16 个插件（librqbit/MPL、libssh2、aws-lc 等）的依赖树完全没有进入许可检查。
本地实测：`cargo deny check licenses` 报 `MPL-2.0` “未遇到”，加上 `--workspace`
才通过。

**修复**：CI 的 `cargo-deny` job 现在以 `--workspace --all-features` 运行
`check`（advisories/bans/licenses/sources 四项），覆盖全部 16 个插件依赖树；
本地已实测四项全绿。

### 🔴 H2. 复合 C 许可缺失（已修复）

只放通用 Apache/ISC/BSD 文本，不能覆盖 AWS-LC（BoringSSL/OpenSSL）、ring
（BoringSSL/once_cell/fiat）、libssh2（curl/FSFULLR/SAX-PD-2.0）、QuickJS、
moka NOTICE。现已按上游原文加入 14 份额外文本。

### 🔴 H3. 插件独立更新包缺许可（已修复）

`stage-plugins.py` 只复制 `LICENSE`，独立更新某个插件时不带 Notices 和
third-party 文本。现已复制 `THIRD_PARTY_NOTICES.md` 与整个 `LICENSES/`。

### 🔴 H4. 浏览器扩展 zip 缺 MIT 声明（已修复）

`build-extension.sh` 打出的 zip 没有 LICENSE；MIT 要求副本随附版权与许可声明。
已在 staging 时复制根 `LICENSE`。

### ✅ M1. 逐 crate 版权/NOTICE（已闭环）

`scripts/generate-third-party-licenses.py` 生成
[`THIRD_PARTY_LICENSES.md`](../THIRD_PARTY_LICENSES.md)：591 个运行时包、
382 份去重文本，包含每个 crate 的 LICENSE/COPYING/NOTICE 原文与版权行；
没有内嵌许可文件的 crate 会明确标注 SPDX 表达式并指向 `LICENSES/` 标准全文。
生成器支持 `--check`，`check-license-bundles.py` 会阻止产物漂移；应用、插件更新包、
浏览器扩展均携带对应声明。

### ✅ M2. Debian `copyright`（已闭环）

新增 `packaging/debian/copyright`（DEP-5）：项目 MIT、vendored Slint 三选一、
第三方编译依赖的许可文本位置与 SBOM 指引；`make deb` 安装该文件而不是单份 MIT
文本。正式进 Debian 前仍建议 ftp-master 预审。

### 🟡 M3. `project_license` 只写 MIT（说明）

AppStream `project_license` / macOS `NSHumanReadableCopyright` 描述的是应用自身
许可；依赖的 Slint RF/MPL 不改变应用许可。但对外文案不应说“纯 MIT 二进制”，
`THIRD_PARTY_NOTICES.md` 已明确“not wholly MIT”。

### 🟢 L1. 资产、图标、扩展 JS

- `assets/icons/*.svg` 为项目自绘（无 Material Icons 复制）；logo 为项目自有。
- 浏览器扩展为原生 JS，无 npm 依赖、无远程代码；图标为项目 logo。
- 翻译 `.po` 为项目贡献，无第三方许可负担。

### 🟢 L2. 供应链告警（非许可，单独跟进）

`cargo deny --workspace check advisories` 目前 **FAILED**，因为根-only 检查此前
未覆盖插件：

| ID | 组件 | 状态 |
| --- | --- | --- |
| RUSTSEC-2025-0012 | `backoff`（librqbit） | 无修复版本（unmaintained） |
| RUSTSEC-2025-0141 | `bincode`（librqbit ×2） | 无修复版本 |
| RUSTSEC-2024-0384 | `instant`（backoff） | 无修复版本 |
| RUSTSEC-2026-0118 | `hickory-proto` NSEC3 循环 | 无安全升级 |
| RUSTSEC-2026-0119 | `hickory-proto` O(n²) 编码 | 升级 `hickory-resolver`/`hickory-proto` |
| RUSTSEC-2026-0418 | `libz-sys` 构建脚本泄漏环境变量 | 升级 ≥1.1.31 |
| RUSTSEC-2026-0194/0195 | `quick-xml`（librqbit-upnp 0.37） | 升级 ≥0.41 |
| RUSTSEC-2026-0271 | `suppaftp` CRLF 命令注入 | 升级 ≥10.0.2 |

这些是**安全**问题，不是许可问题。本轮已完成可升级项：

- `libz-sys` 1.1.30 → 1.1.31（修复构建脚本泄漏）；
- `suppaftp` 6.3 → 10.0.2（修复 CRLF 命令注入；API 兼容，FTP 契约测试通过）；
- `hickory-resolver` 0.25 → 0.26.3 / `hickory-proto` 0.26.3（修复 O(n²) 与
  NSEC3 告警；自定义 DNS 代码已适配 0.26 的 `ConnectionConfig`/`ResolverConfig` API）；
- `librqbit` 8.1 → 9.1（`librqbit-upnp` 改用 quick-xml 0.41，移除 unmaintained
  backoff/bincode/instant；BT 插件与契约测试已适配 9.x API）；
- `ringbuf` 0.4.8 由 `librqbit-utp 0.7`（最新）固定，无法升到 0.5.2；该告警需要
  Drop panic 才触发，而 Aether 的 BT listener 是 TCP-only，未驱动该路径。已在
  `deny.toml` 记录原因并保留复查条件。

`cargo deny --workspace check` 现在四项全绿。

## 9. 发布前检查清单

- [x] `cargo deny --workspace check`（advisories/bans/licenses/sources）
- [x] `python3 scripts/check-license-bundles.py --check-recipes`
- [x] 生成逐 crate `THIRD_PARTY_LICENSES.md` 并纳入门禁（591 包 / 382 文本）
- [x] Debian DEP-5 `copyright`
- [x] 每个 Release 正文含 “Made with Slint”（脚本门禁）
- [x] 插件更新包携带 LICENSE/Notices/逐包文本/LICENSES
- [x] 浏览器扩展 zip 携带 LICENSE
- [ ] 对实际产物运行 `check-license-bundles.py dist`（需构建产物）
- [ ] 法务复核（尤其 Slint RF 入发行版、MPL 义务、商标）

## 10. 证据文件

- [`docs/THIRD_PARTY_INVENTORY.md`](THIRD_PARTY_INVENTORY.md) — 591 个运行时包
- [`THIRD_PARTY_LICENSES.md`](../THIRD_PARTY_LICENSES.md) — 591 包 / 382 份逐 crate 许可与版权原文
- [`scripts/generate-third-party-licenses.py`](../scripts/generate-third-party-licenses.py) — 生成器（`--check` 门禁）
- [`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md) — 组件与许可范围
- [`LICENSES/`](../LICENSES/) — 27 份全文（含复合 C 许可）
- [`deny.toml`](../deny.toml)、[`.github/workflows/ci.yml`](../.github/workflows/ci.yml)
- [`scripts/generate-third-party-inventory.py`](../scripts/generate-third-party-inventory.py)
- [`scripts/check-license-bundles.py`](../scripts/check-license-bundles.py)
- [`scripts/stage-plugins.py`](../scripts/stage-plugins.py)
