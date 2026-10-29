# §4.1 剩余两项实施定稿（Windows 通知 + 发布产物打包）

## A. Windows 桌面通知
- 依赖：`[target.'cfg(windows)'.dependencies] notify-rust = "4"`（4.18.2，
  MIT/Apache-2.0；Windows 后端 tauri-winrt-notification 0.8 同许可；unix/mac
  后端依赖仅进 Cargo.lock 不参与 Windows 编译；cargo-deny licenses 全绿已核）。
- notify-rust Windows 默认 app_id = `Toast::POWERSHELL_APP_ID`（上游为未安装
  程序准备的兜底，toast 可显示但归属「Windows PowerShell」——不诚实）。
- 我方做法：首次通知时 `reg add` 注册
  `HKCU\Software\Classes\AppUserModelId\com.pengtang1024.Aether`
  （DisplayName=Aether；IconUri=exe 旁 logo.ico 若存在）——HKCU 免管理员，
  与 notify-send/osascript 同为「外部命令」哲学；进程内 AtomicU8 只试一次。
  - 注册成功 → `notification.app_id(AUMID)`，toast 归属 Aether。
  - 注册失败 → 不设 app_id，回退 notify-rust 默认（可显示、归属诚实降级）。
  - `show()` Err → `eprintln` 记**一次**（AtomicBool），不 panic 不重试。
- 测试（CI Windows job `cargo test --bin aether` 已覆盖）：调用
  `send_notification` 不 panic + 断言 HKCU 键真实写入（`reg query` 可在
  无桌面会话验证）；toast 显示本身 headless 无法断言 → best-effort。
- Linux/macOS 路径一行不动（notify-send/osascript）。

## B. default_plugins_dir Windows 候选
- 抽 `plugin_dir_candidates() -> Vec<PathBuf>`：
  - windows：**exe 同级 `plugins\`** 打头（安装器 `{app}\aether-daemon.exe`
    → `{app}\plugins`；便携 zip 同理），不再列 /usr/lib；
  - unix：顺序原样（/usr/lib/aether/plugins → build/plugins → plugins）。
  - 两平台尾部保持 `build/plugins`、`plugins`（dev 树）。
- 测试 `plugin_dir_candidates_…`：unix 断言首项 /usr/lib；windows 断言首项
  绝对且以 plugins 结尾；尾部两项顺序一致。CI Windows job 加
  `cargo test -p aether-host plugin_dir`（按名过滤，避免无关测试风险）。

## C. 发布产物打包（zip + Inno）
- 现状缺口：package.yml Windows 只 `cargo build --release`（default-members
  = ["."] → 只有 aether.exe），zip/iss 均不含 daemon/cli/native-host/插件 →
  UI 的 start_daemon 找 exe 旁 aether-daemon.exe 必失败（README 已自述缺口）。
- package.yml Windows job：
  1. `cargo build --release --workspace`（含全部插件；manifest 的
     windows-x86_64 / windows-arm64 key 已逐一核过 16/16 全有）；
  2. `py -3 scripts/stage-plugins.py`（脚本本就支持 windows key，含 .exe 后缀）；
  3. zip `$files` 增加 aether-daemon.exe / aether-cli.exe /
     aether-native-host.exe / `build/plugins`（Compress-Archive 存目录叶名
     `plugins\`，与现有 LICENSES 同机制）。
- aether.iss：三个 exe → `{app}`；`build\plugins\*` → `{app}\plugins`
  （recursesubdirs createallsubdirs）。
- 安装布局与代码解析闭环：daemon exe 与 plugins 在 `{app}`；浏览器扩展的
  native-host 装机路径为 `{app}\aether-native-host.exe`（ps1 `-HostPath`
  覆盖默认 dev 路径——文档补一句）。
- arm64 runner 首次做 --workspace 构建（此前只编根包）：Windows CI 的
  x64 check --all-targets 已证明全 workspace 可编译；arm64 的 aws-lc-sys
  走 C 编译无需 nasm，release 流水线到时验证（release/** 才触发）。

## D. 文档
- README 已知缺口段（347-351）：删「Windows 暂无桌面通知」「Windows 安装包
  尚未随附插件 exe…通知保持静默」→ 改为已落地状态。
- ROADMAP §4.1：两项标已落地；删已被 mingw 交叉 check 推翻的第 3 条剩余。
- CHANGELOG：Windows toast + Windows 包含 daemon/插件两段。

## 门禁
fmt → cargo test --workspace → windows-gnu 交叉 check（整仓 --all-targets）
→ cargo-deny（本地有则跑，无则 CI 权威）→ push → CI watch → pin hash → 记忆。
