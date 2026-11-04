# 平台支持拓展方案（0.5 / 0.6）

> 定位：本文是「把 macOS 从能构建提升为可用桌面端，并补 Linux 分发/BSD/无头边界」的
> 实施方案，落地后并入 `docs/SUPPORT_MATRIX.md` §5 与 `docs/COMPATIBILITY.md`。
> 原则沿用仓库既有约定：**无法验证的能力不宣称支持**；降级必须诚实（日志 + 开关回弹）。

## 0. 结论先说

1. 0.5.0 的目标是 **macOS 运行时对等**（托盘、关闭到托盘、`--silent`、自启驻留、二次启动唤起），
   这是当前唯一「能打包但功能残缺」的一等平台。
2. 托盘底座先做 **3 天时间盒的 spike**：Slint 1.18 自带 `system-tray` 三平台后端
   （macOS AppKit / Linux ksni / Windows Shell_NotifyIcon），优先评估用它统一替换自研托盘；
   不通过则退到「macOS 用 Slint 托盘，Linux/Windows 保留自研」的混合方案。
3. Linux 补 **宿主检测 + 设置页提示** 与 **Flatpak（实验性）**；BSD/无头/WSL 只做
   「可编译 + 显式降级」，不承诺完整桌面体验。
4. 验证体系同步升级：`--self-test` 自测模式、macOS 运行时 CI、Linux 托盘 E2E（stub watcher）。
   签名/公证仍受外部条件限制，维持「未签名/ad-hoc」的诚实标注。

## 1. 现状（事实，带证据）

| 平台 | 构建/打包 | 运行时能力 | CI |
| --- | --- | --- | --- |
| Linux x86_64 / arm64 | `.deb` + `.AppImage` | 完整；托盘 SNI 依赖宿主（GNOME 需 AppIndicator 扩展）；Wayland 聚焦尽力而为 | Linux job：fmt/test/build/deb + license |
| Windows x86_64 / arm64 | `setup.exe` + portable `.zip` | 完整；自研 FFI 托盘 + HKCU 自启 + WinRT toast | Windows job：check + bin 测试（无 GUI 断言） |
| macOS universal | `.dmg`（ad-hoc，未公证） | **无托盘**（`src/tray.rs` 明确未实现；`cfg(unix)` 的 SNI 路径会降级为 None）→ 关闭到托盘/`--silent`/自启驻留全部不成立；LaunchAgent 形状只做了单测；通知走 osascript | macOS job：**仅 `cargo check`** |
| FreeBSD / OpenBSD | 无包 | `src/autostart.rs` 只有 linux/windows/macos 三个 `imp`，其它 unix 无法编译 | 无 |
| 无头 / SSH / WSL | — | 无 display 时 UI 以 backend 错误退出；daemon/CLI 可用 | 无 |

补充事实：

- 自研托盘共 **1473 行**（`tray.rs` 120 / `tray_sni.rs` 700 / `tray_windows.rs` 653）。
  本周修的三处宿主兼容问题（`GetLayout` 返回单节点、子节点 `(ia{sv}av)` 而非 `(vvv)`、
  宿主发 `clicked` 而非 `activate`）说明这套协议维护成本高、且每次只在一个桌面上暴露。
- Slint 1.18 的 `i-slint-core/src/items/system_tray/` 已有 `appkit.rs`（374 行，objc2）、
  `ksni.rs`（333 行）、`windows.rs`（507 行）、`dummy.rs`；`SystemTrayIcon` 暴露
  `icon/tooltip/title/visible/clicked` 与 `Menu`/`MenuItem`，菜单文案走 `@tr`（与现有四语言方案一致）。
  该能力由 `slint` 的 `system-tray` feature 开启，当前 Cargo.toml **未启用**。
- 0.4.0 的 `run_event_loop_until_quit()`、5 秒握手取消、`org.aether.UI` 唤回都是围绕
  「自研托盘不在 Slint 的 keepalive 里」补的；统一托盘底座后可以简化为 Slint 的
  visible-window/tray keepalive。

## 2. 目标 / 非目标

目标（0.5.0）：

- macOS：托盘（显示主窗口/退出）、关闭窗口=隐藏且进程常驻、`--silent` 登录驻留、
  二次启动唤起、自启开关真实反映 LaunchAgent 状态、通知可用。
- Linux：无 SNI 宿主时给出可操作提示（不再只是日志）；Flatpak 作为实验性包。
- 验证：macOS 运行时 job（自测 + DMG 冒烟）、Linux 托盘 E2E、`--self-test` 诊断模式。
- 文档：SUPPORT_MATRIX/COMPATIBILITY/README 的平台矩阵与「已验证到什么程度」同步。

目标（0.6）：

- FreeBSD/OpenBSD：可编译、daemon/CLI 可用；桌面能力按可用性显式禁用。
- 无头/SSH/WSL：清楚说明「只有 daemon/CLI」，不假装有界面。
- Windows arm64 / Linux arm64：已在打包矩阵，补运行时冒烟（runner 允许时）。

非目标：

- Android/iOS/WASM、32-bit、musl 静态包；
- legacy XEmbed 托盘、自动安装 GNOME 扩展、应用商店上架；
- macOS 公证与 Windows 代码签名（外部账号/费用条件，维持现状标注）。

## 3. 决策 D1：托盘底座（先 spike，后定案）

### 方案 A（推荐先验证）：启用 Slint `system-tray`，三平台统一

- 做法：新增 `Tray.slint`（`inherits SystemTrayIcon` + `Menu`），Rust 侧实例化；
  删除 `tray_sni.rs`/`tray_windows.rs` 的协议实现与 `tray.rs` 的循环修补。
- 收益：macOS 立即获得托盘；菜单随 `@tr` 四语言；事件循环由 Slint keepalive 维护；
  SNI/AppKit/Win32 细节由上游维护（ksni 内部处理 watcher 重注册）。
- 缺口/风险（必须 spike 验证）：
  1. **没有「宿主不可用」信号**：GNOME 无 AppIndicator 时组件仍存在、图标不可见。
     需保留一段 D-Bus `StatusNotifierWatcher` 探测，仅用于「关闭=隐藏还是退出」的
     诚实判定（macOS/Windows 恒可用，无需探测）。
  2. **左键语义变化**：macOS 挂载菜单后左键由 AppKit 弹菜单，`clicked` 不触发；
     Linux/Windows 左键仍触发 `clicked`。需确认这与「左键=显示窗口」的产品预期一致，
     或统一改为「左键开菜单、双击/Activate 显示」。
  3. 菜单能力边界：当前只有「显示主窗口/退出」，无子菜单/勾选需求，风险低；
     但 Windows 原生菜单与自研 FFI 菜单的观感差异需确认。
  4. 许可与供应链：ksni（Unlicense/公有领域，已加入 deny.toml allow）、objc2（MIT/Apache-2.0）；
     需要重跑 `THIRD_PARTY_*` 生成脚本与 `cargo-deny`，并确认不与 Slint 锁定的
     objc2 版本冲突（只允许树内一套）。
  5. 二次启动唤回仍需自研（Linux `org.aether.UI` / Windows 命名事件）；macOS 走
     LaunchServices reopen 或 NSDistributedNotification，与本决策解耦。

### 方案 B（回退）：macOS 用 Slint 托盘，Linux/Windows 保留自研

- 收益：改动面最小，macOS 当期可用；Linux 现有「无宿主=无托盘=关闭退出」语义不动。
- 代价：两套托盘实现与两套验证路径；`system-tray` feature 仍会在 Linux 编入 ksni
  （未使用），依赖体积/许可清单照样增加；菜单文案与交互可能分叉。

### 方案 C（不推荐）：自研 macOS AppKit（objc2/raw FFI）

- Slint 已提供 374 行 AppKit 实现；自研等于重写并额外承担「macOS 只能人工验证」的风险。
  除非 A/B 都被证伪，否则不进入排期。

### Spike 退出门禁（时间盒 3 天，全部通过才选 A）

- Linux（Ubuntu GNOME + AppIndicator 扩展，真机）：图标出现；菜单「显示主窗口/退出」
  可用；四语言切换；关闭=隐藏、托盘唤回；宿主扩展重启后图标回来；`--silent` 生效。
- Windows：左键显示、右键菜单、退出；`cargo test --bin aether` 不回归。
- macOS 14 runner：`--self-test=tray` 能创建 `NSStatusItem` 与菜单项并触发回调，
  窗口隐藏时进程存活；DMG 冒烟通过。
- 任一硬阻断（如无法判定宿主、菜单不可用、objc2 版本冲突）→ 选 B。

## 4. 工作流

### P0 macOS 运行时对等（0.5.0，约 2 周）

1. **托盘**：按 D1 落地；菜单「显示主窗口/退出」；图标用模板图（菜单栏单色，
   `NSImage` template）而不是彩色 logo；tooltip/title 按平台约定。
2. **窗口生命周期**：关闭=隐藏并常驻；`Cmd+Q`=退出；`--silent` 启动不显示窗口；
   Dock 图标点击回到窗口（`applicationShouldHandleReopen`，需确认 winit/Slint 是否
   已代理 app delegate，必要时用 objc2 包一层）。
3. **二次启动唤回**：`open -a Aether` 走 LaunchServices reopen（首选）；直接运行
   `Contents/MacOS/aether` 的第二实例走单实例锁 + NSDistributedNotification/命名端口。
   行为与 Linux/Windows 的「唤起而不是只弹提示」对齐。
4. **开机自启**：现有 LaunchAgent + `--silent`；新增运行时自测（launchd 载入后进程
   存在、UI 端点可唤醒、无窗口），设置开关只读 OS 真实状态。
5. **通知**：保留 osascript；打包后用 bundle 名称验证一次。
6. **打包**：universal DMG 不变；Info.plist 复核（`LSUIElement` 保持 false、保留
   Dock 图标）；ad-hoc 签名 + 诚实说明；公证仍为外部门禁。
7. **CI**：macOS job 从 `cargo check` 升级为 build + `--self-test=tray` + DMG 冒烟
   （`hdiutil attach`、`codesign -dv`、bundle 结构断言）；菜单栏视觉/点击仍为人工清单，
   在文档中明确「CI 验证到进程内创建，不验证屏幕像素」。

### P1 Linux 桌面覆盖（0.5.x，约 1 周）

1. **宿主检测与提示**：启动时探测 `StatusNotifierWatcher`（现有逻辑）→ 无宿主时
   设置页显示一行可操作说明（GNOME 需 AppIndicator 扩展，附 extensions.gnome.org
   链接），关闭=退出语义不变；不自动安装扩展。
2. **Wayland 聚焦**：验证 winit `focus_window` 是否走 xdg-activation；不支持则文档化
   「后台唤起可能不抢焦点」，不引入自研 compositor 依赖。
3. **Flatpak（实验性）**：manifest + finish-args；自启走
   `org.freedesktop.portal.Background`；通知走 portal（或保留 notify-send 并标注
   沙箱差异）；下载目录走 portal/`xdg-download`；托盘 SNI 走 session bus；
   关闭应用内自动更新。CI 只做 manifest 校验，真机 VM 冒烟后标「实验性」。
4. `.deb`/AppImage 不变；`COMPATIBILITY.md` 的 glibc 基线保持。

### P2 BSD / 无头（0.6，约 1 周）

1. **FreeBSD/OpenBSD**：`autostart.rs` 补 `imp` 桩（开关禁用 + 说明）；通知检测
   `notify-send` 缺失时降级为日志；托盘在 D1=A 时随 ksni 可用，否则明确「无托盘」；
   提供源码构建/ports 说明；CI 用交叉 check（`x86_64-unknown-freebsd` 工具链）或
   QEMU FreeBSD runner，至少保证编译不回归。
2. **无头/SSH/WSL**：检测到无 display/无 session bus 时打印一条明确错误并退出
   （附「daemon/CLI 用法」提示），不尝试启动半残 UI；文档补矩阵行。

### P3 验证与发布（持续）

1. **`--self-test=tray|ui|notify`**：进程内创建托盘/窗口/通知，输出 JSON（是否创建、
   菜单项数、回调是否触发、耗时），退出码表达成败；CI 与用户报障都用它。
2. **Linux 托盘 E2E**：Xvfb + `dbus-run-session` + stub StatusNotifierWatcher
   （本周已验证的配方）→ 断言注册、`GetLayout`/`GetGroupProperties`、`clicked`→
   窗口显示、退出码 0；纳入 Linux CI（快）。
3. **macOS 运行时 job**（见 P0-7）；Windows 已有 bin 测试，补 `--self-test=tray`。
4. **供应链**：`THIRD_PARTY_INVENTORY`/`LICENSES` 重新生成（ksni/objc2），
   `cargo-deny --workspace` 门禁；若 A 落地，删除自研托盘的许可说明不再需要。
5. **文档**：SUPPORT_MATRIX §5、COMPATIBILITY、README 平台表、ROADMAP、CHANGELOG
   同步「已验证/未验证」边界。

## 5. 里程碑与排期

| 里程碑 | 时长 | 交付物 | 门禁 |
| --- | --- | --- | --- |
| M0 托盘 spike | 3 天 | D1 结论（A/B）+ 可行性记录 | 三平台门禁全过或明确阻断点 |
| M1 macOS 对等 | 2 周 | 托盘、生命周期、唤回、自启自测、DMG/CI | macOS job：self-test + DMG 冒烟；人工清单签字 |
| M2 Linux 覆盖 | 1 周 | 宿主提示、Wayland 结论、Flatpak 实验包 | Linux 托盘 E2E + Flatpak VM 冒烟 |
| M3 BSD/无头 | 1 周（0.6） | autostart 桩、交叉 check、无头提示 | 交叉编译 + 文档矩阵 |
| M4 验证/发布 | 持续 | self-test、CI 扩展、许可清单、文档 | fmt/test/deny/package 全绿 |

0.5.0 = M0 + M1 + M2 + M4；0.6 = M3。M1 依赖 M0 结论；M3 的托盘行依赖 M0 选 A。

## 6. 风险与回退

| 风险 | 影响 | 应对/回退 |
| --- | --- | --- |
| Slint 托盘无「宿主可用」信号 | GNOME 无扩展时无法诚实判定关闭语义 | 保留 watcher 探测；不行则 D1 选 B |
| macOS 菜单栏 CI 不可见 | 无法机器验证视觉/点击 | `--self-test`（进程内）+ 人工清单；文档只声明验证到的范围 |
| objc2/ksni 版本或许可冲突 | 构建/deny 失败 | 只用 Slint 锁定的版本；必要时回退 B |
| Wayland 拒绝后台激活 | 唤起不聚焦 | 文档化，best-effort；不引入新协议栈 |
| Flatpak 沙箱破坏自启/通知/托盘 | 功能缺失 | 先发「文件下载可用」的实验包，缺的能力显式禁用 |
| 无 Apple 开发者账号 | 公证/公证后的 Gatekeeper 体验 | 保持 ad-hoc + 文档说明，不阻塞功能 |

## 7. 验收门禁（每个里程碑都要过）

- `cargo fmt --all --check`、`cargo test --workspace`、`make check-licenses`、`cargo-deny`。
- Linux：托盘 E2E（stub host）+ 无宿主回退日志断言。
- Windows：`cargo test --bin aether`；托盘 self-test（CI 尽力）。
- macOS：build + `--self-test=tray` + DMG 结构断言；人工清单记录在 release notes。
- 文档矩阵与实际能力一致；未验证项必须写「未验证」，不得写成「支持」。

## 8. 明确不做

- Android/iOS/WASM/32-bit/musl 静态；legacy XEmbed 托盘；自动安装 GNOME 扩展；
  App Store/微软商店；macOS 公证与 Windows 代码签名（外部条件）；自研 compositor 级
  Wayland 激活。

## 9. 待确认（影响排期）

1. D1 默认按 **A（3 天 spike）**推进，若你倾向零风险可直接选 B（macOS 用 Slint 托盘，
   Linux/Windows 不动）。
2. 0.5.0 是否接受「macOS 优先、Flatpak 实验性」的排序；若 Flatpak 必须一等支持，
   M2 从 1 周扩到 2 周。
3. macOS 公证是否列入预算（当前按「不做」排期）。

## 10. M0 实施记录（2026-11-02）

- **决策**：选 A。`slint` 开启 `system-tray`，新增 `ui/tray.slint`
  （`inherits SystemTrayIcon` + `Menu`），`src/tray.rs` 只保留窗口唤起、
  宿主探测与二次启动端点；删除此前拆出的 `tray_sni` / `tray_windows`
  两个平台模块（约 1.4k 行自研协议，已不存在）。
- **Linux 实测（Ubuntu GNOME + AppIndicator，真实 session bus）**：
  ksni 注册 `org.kde.StatusNotifierItem-<pid>-1`；`GetLayout` 返回
  显示主窗口/退出；`Event(clicked)` 触发显示与退出；`--silent` 下无窗口
  且进程常驻（托盘 keepalive）；`org.aether.UI` 唤起可用；`lang=en` 启动时
  菜单为 "Show main window"/"Quit"。
- **Windows**：`x86_64-pc-windows-gnu` 交叉 check 通过；`available()` 用
  `FindWindowW("Shell_TrayWnd")` 判断通知区域是否存在，无 shell 时不隐藏。
- **macOS**：CI 仅 check（本机无 macOS）；AppKit 后端由 Slint 提供，
  菜单栏/关闭到托盘待真机验证。模板图标（单色）与 LaunchServices reopen
  唤起仍属 P0 后续。
- **许可/门禁**：`cargo-deny` 增补 Unlicense（ksni）；三方清单 
  `THIRD_PARTY_LICENSES.md`/`docs/THIRD_PARTY_INVENTORY.md` 已重新生成，
  `make check-licenses` 通过。
- **Wayland 唤起补丁（同日，实机验证）**：`--silent` 启动的窗口是 0x0 表面，
  首次 `show()` 会复用它而不映射；现在先按模型尺寸 `set_size` 再重建表面，
  并做有界的 show/focus 重试。GNOME 忽略无输入序列的 xdg-activation，
  因此当 Window Calls 扩展存在时用它的 `MakeAbove` 把窗口置顶直到用户聚焦
  （2 分钟后兜底取消）；无扩展时层级由合成器决定。X11 关闭改为最小化，
  绕开 winit 在隐藏聚焦窗口时的 `BadWindow` panic。**上游缺口**：Slint 的
  `SystemTrayIcon` 没有把 `ProvideXdgActivationToken` 的 token 交给窗口，
  winit 也无法对既有窗口应用 token——这是后续应向 Slint 反馈的点。
- **遗留（P0/P1）**：macOS 模板图标、`applicationShouldHandleReopen`、
  公证外部门禁；Flatpak/BSD/无头按原里程碑继续。
