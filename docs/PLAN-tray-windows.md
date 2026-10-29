# Windows 托盘（4.5 扩展）设计定稿

- 与 transport.rs 同哲学：**零依赖 raw FFI**（shell32/user32/kernel32）。
- `src/tray_windows.rs`：`start(Hooks{on_show,on_quit}) -> Option<()>`
  - 注册窗口类 + message-only 窗口（HWND_MESSAGE）
  - `Shell_NotifyIconW(NIM_ADD)`，`cbSize = size_of(NotifyIconDataW)`（#[repr(C)] 现代结构 976B/x64，手算对齐）
  - uCallbackMessage = WM_APP+1；v0 事件：LOWORD(lParam) WM_LBUTTONUP→show、
    WM_RBUTTONUP→popup menu（CreatePopupMenu：显示主窗口/退出 + TrackPopupMenu）
  - WM_COMMAND → id 分发；quit → DestroyWindow → 消息循环退出（NIM_DELETE 兜底）
  - 图标：exe 旁 logo.ico（LoadImage LR_LOADFROMFILE）→ 失败回退 IDI_APPLICATION（诚实记 note）
- Hooks 解耦窗口：tray::start(windows 分支) 从 Weak<AppWindow> 构造回调 → 测试无需显示
- **测试**（CI Windows job 跑 `cargo test --bin aether`）：start + no-op hooks，
  Some/None 都过（None=会话拒绝图标，服务会话已文档化的跳过——验证 FFI 布局与窗口创建路径）
- Cargo.toml：zbus 移到 target.'cfg(unix)'.dependencies（保护 Windows CI）
- macOS 托盘不做：需 objc2 且无任何运行时验证路径——诚实延后（ROADMAP 注明）

## 实施结果（2026-10-30）

- 文件布局与计划的一处偏差：`src/tray.rs` 不是「给现有文件加 cfg」而是
  重构为**薄分发器**，Linux 实现整体挪到 `src/tray_sni.rs`（`git mv`，
  逻辑零改动）——避免在同一文件里给 15 个 item 逐个打 cfg。
- 按计划完成：Hooks 解耦、message-only 窗口、NotifyIconData 976B 手算
  对齐（测试钉 size+offset）、v0 回调、菜单（含 SetForegroundWindow+WM_NULL
  坑）、logo.ico→IDI_APPLICATION 回退、zbus 移 unix-only、CI 加
  `cargo test --bin aether`、start Some/None 双过契约。
- **本地交叉预检**（超出计划的收获）：`apt install gcc-mingw-w64-x86-64
  nasm` 后 `cargo check --workspace --all-targets --target
  x86_64-pc-windows-gnu` 在 Linux 上即可整仓类型检查 Windows 目标
  （不链接，无需完整工具链）。推送前抓掉 4 个编译错：
  LoadImageW `null`→`null_mut`（HINSTANCE 是 *mut）、with_hooks 解引用
  出 `&State` 需再取 `.hooks`、dispatcher 缺 `ComponentHandle` import。
  nasm 是 aws-lc-sys 的硬前置，缺了 build script 直接 panic。
- 测试（Windows CI 执行）：SDK 布局钉死（976/960 + hIcon/szTip/guid
  offset）、message-only 窗口创建/销毁、start 5s 握手 Some|None。
  本机 Linux 只跑得到 dispatcher + sni；Windows 目标下 test 代码也经
  `--all-targets` check 类型检查通过。
