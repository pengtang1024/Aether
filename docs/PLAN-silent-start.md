# 静默启动（`--silent`）实施定稿

来源：ROADMAP §4.8 后台形态的后续。单实例、关闭到托盘、开机自启已落地；
本波让「登录自启」不再弹主窗口，而是直接驻留托盘继续下载。

## 1. 语义

- 新增启动参数 `--silent`（别名 `--minimized`）：进程照常启动 daemon、
  后端与托盘，但**不显示主窗口**；托盘菜单「显示主窗口」/左键激活是回来的路。
- 手动启动（无参数）行为不变：显示主窗口。
- 参数只认**精确匹配**：`--silent=false`、`silent`、`--silent-extra` 都不算，
  不会误把窗口藏起来（`src/launch.rs` 单测覆盖）。
- 第二实例仍按既有单实例逻辑：通知「Aether 已在运行」后退出，不唤起窗口。

## 2. 无托盘回退（诚实降级）

静默的前提是「有地方把窗口叫回来」。`tray::start` 返回 `None`（GNOME 无
AppIndicator 扩展、无 StatusNotifier host、Windows 通知区域被拒、macOS 无
托盘实现）时：

- 记一行 `aether: --silent requested but no system tray is available; showing the window`；
- **照常显示主窗口**，绝不把进程藏到用户找不到的地方。

判定集中在 `launch::hide_on_start(silent, tray_up)`，四象限单测覆盖。

## 3. 事件循环

- 可见启动：`ui.show()` + `slint::run_event_loop()`（与原先 `ui.run()` 等价）。
- 静默启动：`slint::run_event_loop_until_quit()`。没有可见窗口时不能用
  「最后一个窗口关闭即退出」的默认语义；循环必须活到托盘菜单调用
  `quit_event_loop()`。窗口被托盘唤出后再关闭，仍是隐藏（`HideWindow`），
  不退出。

## 4. 开机自启集成

- Linux XDG entry：`Exec=<EXE> --silent`（路径含空格仍按 spec 加引号，
  参数在引号外）；Windows HKCU Run：`"<EXE>" --silent`；macOS LaunchAgent：
  `ProgramArguments` 数组追加 `<string>--silent</string>`。
- **自愈**：启动时若自启项存在，调用 `autostart::refresh()` 重写为规范形式。
  原因有两个：AppImage/安装目录更新后可执行文件路径会变；旧版本写下的条目
  没有 `--silent`。条目不存在时**不创建**——删除自启项仍是用户的关闭开关，
  开关继续如实显示 OS 状态。
- 刷新失败只记日志（`aether: could not refresh the autostart entry: …`），
  不阻塞启动；设置页开关仍以 OS 真实状态为准。

## 5. UI / 文案

- 不新增开关：自启即静默是选定语义。设置→常规→启动 的「开机自启」描述改为
  「登录系统时静默启动 Aether（驻留托盘，不弹主窗口；由操作系统管理，删除
  自启项即关闭）」。
- 该串同步 en / ja / zh-TW `aether.po`；无新增 Rust 静态串。

## 6. 验证

- 单测：`launch` 参数精确匹配 / 四象限隐藏判定；Linux entry 含
  `--silent` 与引号规则；macOS plist 含 `--silent`；Windows 注册表往返并
  读回值断言含 `--silent`（CI Windows job 实跑）。
- Xvfb 冒烟（2026-10-31，本机）：
  - 无 watcher：进程存活、日志出现降级行、`xdotool search --onlyvisible`
    找到主窗口 → 回退路径 ✓。
  - 会话总线起一个最小 `org.kde.StatusNotifierWatcher` stub 后再启动
    `--silent`：5 秒内无可见窗口、进程存活；`gdbus` 调
    `org.kde.StatusNotifierItem.Activate` 后窗口出现 → 静默 + 托盘唤回 ✓。
- 门禁：`cargo fmt --all --check`、`cargo test --bin aether`、workspace
  全量测试、Windows 交叉 check。

## 已知边界

- macOS 没有托盘实现（CI 仅 check），因此 macOS 上 `--silent` 必然回退为
  显示窗口；LaunchAgent 仍写入 `--silent`，待托盘落地后自动生效。
- Windows 登录瞬间若通知区域尚未就绪，`Shell_NotifyIconW` 可能拒绝图标，
  此时按同一规则回退显示窗口（宁可可见，不可失联）。
- 旧自启项在下次启动时由 `refresh()` 补上 `--silent`；若用户手改了 Exec 的
  其他参数，会被规范形式覆盖（自启项按「由操作系统管理」的约定视为本应用所有）。
