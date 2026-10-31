# 许可与授权范围

> **结论：Aether 自有代码是 MIT；Slint 运行时和 `ui/material/**` 不是。**
> 仓库不是“纯 MIT”，但 **MIT 代码 + Slint Royalty-free 2.0** 的组合是合规的。
> 本文不是法律意见；正式商用、进发行版仓库或大规模再分发前，建议法务复核。

## 1. 一览

| 部分 | 许可 | 能否改成 MIT |
| --- | --- | --- |
| `src/**`、`crates/**`、Aether 自己写的 `.slint`、脚本、文档 | **MIT** | 是（本来就是） |
| `ui/material/**`（vendored Slint 组件） | `GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0` | **否** |
| Slint 运行时 / `slint-build`（Cargo 依赖） | 同上，本项目选择 **Royalty-free 2.0** | 否（作为依赖） |
| `assets/` 中的品牌 logo / 图标 | 项目自有，随 MIT | 是（第三方素材需单独确认） |

- MIT 全文：[`LICENSE`](../LICENSE)
- 第三方声明：[`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md)
- Slint Royalty-free 2.0 全文：[`LICENSES/Slint-Royalty-free-2.0.md`](../LICENSES/Slint-Royalty-free-2.0.md)
- 其它随二进制分发的许可全文（Apache-2.0 / BSD / ISC / BSL / CDLA / MPL / Unicode / Zlib）：
  [`LICENSES/`](../LICENSES/)

## 2. 自有代码：MIT

`src/**`、`crates/**`（插件 SDK/fake 插件）、Aether 自己的 `.slint` 页面与主题、构建脚本和文档都按 MIT 发布。MIT 允许：

- 商用、闭源分发、收费；
- 修改后不公开源码；
- 再许可（sublicense）和双许可；
- 静态/动态链接 Slint 后分发二进制。

MIT 只要求保留版权与许可声明。**MIT 不会因为链接了 Slint 而变成 GPL**——前提是 Slint
按 Royalty-free 2.0 使用，而不是按 GPL-3.0 使用。

## 3. Slint 的三许可

Slint 1.18.1 的每个文件和 crate 都提供三个选项：

| 选项 | 对应用的影响 | 本项目 |
| --- | --- | --- |
| `GPL-3.0-only` | 整个组合作品需按 GPL-3.0 分发，源码必须可用 | 未选择 |
| `LicenseRef-Slint-Royalty-free-2.0` | 应用可保持 MIT/闭源；需署名；有使用范围限制 | **已选择** |
| `LicenseRef-Slint-Software-3.0`（商业） | 付费；可免署名；适合闭源商业产品 | 未购买 |

Royalty-free 2.0 不是 copyleft。它第 1 条只授予“把 Slint 作为 Application 的一部分
使用、修改、分发”的权利，不要求 Application 的源码改用 GPL。因此：

- Aether 可以继续用 MIT；
- 可以收费、闭源、双许可；
- 修改 vendored 组件也可以，但必须保留版权/许可头。

如果选择 GPL-3.0 选项，则相反：整个应用分发要满足 GPL，MIT 代码虽然兼容 GPL，
但不能再以“纯 MIT 二进制”宣传。

## 4. `ui/material/**` 的边界

`ui/material/` 来自 Slint 1.18.1 的 `widgets/{material,common,interfaces}`，共 60+ 个文件。
其中 45 个文件头带有：

```
SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0
```

因此：

1. **不能**把这些文件重新标成 MIT，也不能删除或改写版权/许可头（Royalty-free 第 3 条）。
2. 这些文件是“作为 Aether 的一部分”分发，不是单独分发 Slint，符合许可。
3. Aether 对它们的本地补丁（色板、滚动条、开关等）允许保留，但补丁文件仍受 Slint 许可约束。
4. `THIRD_PARTY_NOTICES.md` 必须继续随源码和安装包分发。

这也是“仓库不是纯 MIT”的唯一实质原因：不是 Slint 运行时污染了 MIT，而是**你把 Slint 的
源码文件 vendored 进了仓库**。

## 5. 署名义务

Royalty-free 2.0 第 2 条要求二选一：

- **(a)** 在顶层菜单可进入的 About 对话框里显示 `AboutSlint` 组件；或
- **(b)** 在可下载二进制的公开网页上显示 “Made with Slint” 徽标，位置要容易被访问者找到。

当前状态：

- 已采用 (b)：v0.1.0 的 GitHub Release 页面带徽标（该版本的二进制已因许可
  文件不全而下架，见第 10 节；撤销说明仍保留在发布页）。
- 应用内 **还没有** About 对话框，因此 (a) 尚未满足；不影响 (b) 的合规性。
- README 也建议放一份徽标，但 README 正被 UI 重设计会话修改，等其落地后再加。

后续接 About 对话框时，`ui/material/about-slint.slint` 已经 vendored，`build.rs` 也把
`ui/material` 加进了 include path，直接：

```slint
import { AboutSlint } from "about-slint.slint";

// 在 About 对话框内容区
AboutSlint { }
```

## 6. 分发场景

| 场景 | 是否可行 | 注意事项 |
| --- | --- | --- |
| GitHub Releases 发 `.deb` / AppImage | 可行 | 保留 `LICENSE`、`THIRD_PARTY_NOTICES.md`、Slint 许可全文；发布页带徽标 |
| AUR / Snap Store / 自建仓库 | 可行 | 包内保留许可文件；不要声称“纯 MIT” |
| Debian / Ubuntu 官方仓库 | **大概率不可行** | Royalty-free 2.0 含使用范围限制，可能不符合 DFSG；提交前确认。需要时改用 GPL-3.0 构建或商业许可 |
| Flathub | 通常可行 | 可再分发的非自由许可一般可上架，但需在 metainfo 正确标注许可并逐案审核 |
| 闭源商用 / 收费 | 可行 | 自有代码保持 MIT 或改为闭源均可；Slint 仍按 Royalty-free 署名 |
| 公司内部使用 | 可行 | 无额外义务；保留声明即可 |
| 源码再分发 | 可行 | 必须保留 `ui/material/**` 的上游头与 `LICENSES/` |
| 把 Aether 做成 SDK/组件卖 | 需注意 | Royalty-free 不允许“单独分发 Slint”或“暴露 Slint API”；你的产品不能变成 Slint 封装 |

## 7. 常见误区

- **“用了 Slint 就必须 GPL”**：只有选 GPL-3.0 选项时才成立；选 Royalty-free 不会。
- **“MIT 覆盖整个仓库”**：不覆盖 `ui/material/**`；`LICENSE` 是自有代码许可，第三方文件另有许可。
- **“依赖是 GPL，所以我的代码也 GPL”**：本项目没有选 GPL 选项；且依赖许可与自有代码许可是两件事。
- **“买了商业许可就能把 vendored 文件改成 MIT”**：不能；商业许可只是换掉 Slint 的授权条件，版权仍属 SixtyFPS。
- **“release 页有徽标就万事大吉”**：还要保留许可声明、不单独分发 Slint、不删除文件头。
- **“AppImage 是自包含的，不用带许可”**：二进制里包含 Slint，仍应随包带 `LICENSES/` 与声明。

## 8. 如果以后想要“纯 MIT 仓库”

只有两条现实路径：

1. **不 vendored 组件源码**：删除 `ui/material/`，改用 Slint 内置的 `material` 样式。
   代价是失去当前的品牌色板、滚动条、开关等局部补丁，UI 需要重新适配。
2. **更换 UI 工具包**：成本最高，不建议为了许可证做。

即使采用路径 1，二进制仍包含 Slint 运行时（Royalty-free），只是仓库源码层面不再含
Slint 源文件；对外仍应保留 `THIRD_PARTY_NOTICES.md` 与署名。

## 9. 贡献与再许可

- 外部贡献默认按 MIT 接收（与仓库自有代码一致）；提交 PR 即表示同意。
- 不要接受来自第三方的、带其它许可的 Slint 组件补丁而不更新 `THIRD_PARTY_NOTICES.md`。
- 如果未来需要把项目整体改为 GPL 或商业双许可，需要所有贡献者的授权；当前没有 CLA，
  建议在引入大规模外部贡献前补一个 DCO/CLA 决策。

## 10. 已撤回的发布

发布包一旦发现缺少第三方许可全文、第三方声明或署名，立即下架，不在旧 tag 上
补挂新文件，也不静默替换资产。

- **v0.1.0（2026-10-26 撤回）**：二进制包缺少
  `LICENSES/Slint-Royalty-free-2.0.md`，且当时的 `THIRD_PARTY_NOTICES.md`
  未明确记录所选许可。所有 release 资产已删除；tag 保留用于源码溯源，但不得
  用于重新分发旧包。完整资产哈希与处理记录见
  [`WITHDRAWN_RELEASES.md`](WITHDRAWN_RELEASES.md)。
- 撤回后不得移动或复用该 tag；修复必须通过新版本发布。
- 不得把已撤回的版本号重新用于新构建。
- 每个新发布在 publish 前必须通过
  `python3 scripts/check-license-bundles.py --check-recipes`，并对实际产物运行
  `python3 scripts/check-license-bundles.py dist`（`.deb`、AppImage、ZIP、DMG；
  Windows 安装器由 Inno Setup recipe 检查覆盖）。

## 11. 相关文件

- [`LICENSE`](../LICENSE) — MIT 全文
- [`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md) — 第三方组件与许可范围
- [`docs/THIRD_PARTY_INVENTORY.md`](THIRD_PARTY_INVENTORY.md) — 591 个随二进制分发的依赖（SPDX + 目标平台）
- [`docs/LICENSE_AUDIT.md`](LICENSE_AUDIT.md) — 许可专项深度审计（复合 C 许可、Slint/MPL、产物矩阵）
- [`THIRD_PARTY_LICENSES.md`](../THIRD_PARTY_LICENSES.md) — 逐 crate 许可/版权原文（591 包）
- [`LICENSES/Slint-Royalty-free-2.0.md`](../LICENSES/Slint-Royalty-free-2.0.md) — Slint 许可全文
- [`LICENSES/`](../LICENSES/) — 其它第三方许可全文（打包时全部携带）
- [`docs/WITHDRAWN_RELEASES.md`](WITHDRAWN_RELEASES.md) — 已撤回的发布、资产哈希与撤回规则
- [`docs/GAP_AUDIT.md`](GAP_AUDIT.md) — 许可、签名、分发相关的待办
- [`docs/RELEASING.md`](RELEASING.md) — 发布时需携带的许可文件
- [`scripts/check-license-bundles.py`](../scripts/check-license-bundles.py) — 发布产物许可文件检查
