# 后台形态（单实例 / 关闭到托盘 / 开机自启）实施定稿

来源：GAP_AUDIT #3「单实例 / 托盘 / 后台 / 开机自启」。托盘已有（Linux/Windows），
本波补其余三件。

## 1. 单实例锁（src/instance.rs）
- fs2 的 try_lock_exclusive（flock / LockFileEx），锁文件放配置目录
  `aether/instance.lock`（与 ui.json 同目录）；进程退出/崩溃由 OS 自动释放，
  不会留 stale lock。
- 结果三态：Primary(Guard)（持有到进程结束）/ AlreadyRunning / Unlocked
  （配置目录不可用等环境问题 → 照常启动，双开好过不开）。
- 第二实例：`send_notification("Aether 已在运行", "请使用已经打开的窗口")`
  后 `return Ok(())`。**不做窗口唤起**（需 UI 级 IPC/窗口枚举）——诚实记录。
- 锁冲突判定：unix WouldBlock；Windows fs2 报 ERROR_LOCK_VIOLATION(33)。
- 测试：同进程两次 acquire_at 第二次 AlreadyRunning，drop 后可再取；
  CI Windows bin 测试也跑（LockFileEx 跨句柄冲突）。

## 2. 关闭到托盘（main.rs + tray_sni.rs 诚实化）
- Slint 1.18 的 CloseRequestResponse 只有 HideWindow / KeepWindowShown
  （无 Quit）→ **只有托盘真实可用时才注册** on_close_requested→HideWindow；
  无托盘保持默认（关闭=退出），绝不把应用藏进看不见的地方。
- **Linux 托盘诚实化**：tray_sni 原来无论有没有 StatusNotifierWatcher 都返回
  Some（图标其实不可见）。改为 register_with_watcher 失败 → 返回 None + 一行
  日志（文档本来就写「无 host 优雅降级」，此前名不副实）。副作用：GNOME 无
  AppIndicator 扩展时 tray_up=false → 关闭=退出 ✓；宿主后出现需重启应用。
- 托盘菜单「退出」→ quit_event_loop ✓（既有）。

## 3. 开机自启（src/autostart.rs）
- 状态源是**操作系统**，不进 ui.json：开关启动时读真实状态，用户手删也如实反映。
- Linux：`$XDG_CONFIG_HOME/autostart/aether.desktop`（Exec 含空格时按 desktop
  entry 规则加引号转义）。
- Windows：`HKCU\...\CurrentVersion\Run` /v Aether（reg add/delete/query，
  值带引号防空格路径），免管理员。
- macOS：`~/Library/LaunchAgents/com.pengtang1024.Aether.plist`（RunAtLoad；
  XML 转义路径）。诚实标注：macOS 仅 check-only CI，未做运行时验证。
- 失败时 UI 开关回弹 + 错误提示（不假装成功）。

## 4. UI
- 设置→常规→启动 卡片：新增「开机自启」Switch（changed("autostart")）+
  「关闭窗口」说明行（有托盘收进托盘，无托盘直接退出）。
- 属性链：GeneralPanel.autostart-on ↔ SettingsPage.autostart-on ↔
  AppWindow.settings-autostart（Rust set_settings_autostart）。
- 新增 Slint 串 4 条 → en/ja/zh-TW po 全补；Rust 静态串 4 条 → i18n/table.rs
  行（动态 format! 串按既有规则回退中文）。

## 5. 验证
- 单测：instance 双开/释放、autostart 内容与临时目录往返、Windows 注册表往返。
- Xvfb 冒烟：双开第二实例快速退出且第一实例存活；设置页新行渲染截图。
- 门禁：fmt → workspace test → windows-gnu 交叉 check → cargo-deny（无新依赖，
  fs2 已在树内）→ push → CI → pin → 记忆。
