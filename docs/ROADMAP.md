# 路线图与会话交接

> 面向后续会话/贡献者：现在在哪、下一步做什么、怎么做、别踩哪些坑。
> 最后更新：2026-10-30（main @ `582faa8`）。历史盲区清单见 `GAP_AUDIT.md`，
> 本文跟踪其中已完成的部分与剩余特性的具体方案。

## 1. 当前状态

- **0.2.0 已发布**（tag v0.2.0）。main 之上已合入十二个波次：

| 提交 | 内容 |
| --- | --- |
| `6d6afe0` | 外观主色（6 预设）/动效/流线、启动页（含「上次页面」）、完成后动作、重置偏好；删除约 35 个无机制假控件 |
| `84424ab` | 同时下载数、每任务连接数、重试次数、全局限速 → daemon 调度旋钮（settings.json 覆盖 AETHER_MAX_*） |
| `1c1e8dd` | 默认请求头（HTTP 族）、FTP 主动模式、BT 端口/DHT/上传限速/屏蔽列表/Tracker → 插件引擎 |
| `4710e01` | 计划下载窗口（daemon 调度器，支持跨午夜）+ 完成/失败桌面通知 |
| `4f9493a` | Windows：named pipe transport（无新依赖），daemon/UI/CLI/native-host 全部跨平台，CI Windows job 跑回环测试 |
| `1353da9` | RPC over TCP：`rpc_listen`/`rpc_secret` settings + `--rpc-listen`，token 鉴权（常数时间，热轮换），CLI `--socket ip:port`，`tests/rpc.rs` 6 用例 |
| `cb3ef23` | 浏览器接管：Cookie 显式授权（optional permission）、站点过滤、Windows manifest 安装（注册表）、native-host 透传测试 |
| `f8aefa0` | 媒体转码：`transcode_to` 任务级容器转封装（ffmpeg `-c copy`，失败保留原件）、全局 `ffmpeg_path` 注入、媒体页路径行、假 ffmpeg e2e |
| `4c84ac2` | 剪贴板链接检测：arboard 轮询（默认关、变化触发、scheme 白名单）、toast 动作按钮 → 新建对话框预填、设置页开关；托盘/i18n 仍待做 |
| `3ed8af5` | 插件安装+签名：`files` 哈希映射 + canonical Ed25519 签名、`aether-plugin-sign`、install/rescan RPC（staging swap 回滚、禁用状态保留）、UI 路径安装行、假开关改说明文本 |
| `b0f8a26` | 插件更新下载链：清单 `plugins` 条目、plugin_updates/update_all RPC（tar.gz 解包防护）、`plugin_auto_update` 键+6h pass、UI 更新全部/自动更新开关接真 |
| `6fad6ef`+`bfd5ae1` | 供应链：Actions pin SHA、cargo-deny+CI job、SBOM 进 release、AppStream 更新、优先级老化、UI 多 URL 导入 |
| `3cc8d1c` | 托盘（Linux）：zbus StatusNotifierItem + dbusmenu 菜单（显示/退出），私有 bus 全链路实测含退出；无 bus/host 优雅降级 |
| `79c2cdb` | i18n（中文优先）：全量 202 条 UI 串 @tr + en.po、select_bundled_translation 即时切换、设置→外观语言项；ComboBox 功能值不译 |
| `f6ef273` | Rust 侧字符串表化：i18n::tr(zh,en) 内联对 + 全局语言位，toast/hint/notice/时间格式/插件描述/通知全迁；功能值保留 |
| `c1d1bcd` | ja + zh-TW 全量（po 250 条 + Rust 单表多语言行）、语言下拉 4 项、扩展商店打包脚本+文案 |
| `8406446` | 在线 DB 备份（rusqlite backup feature）+ PRAGMA integrity_check、`aether-cli diag` 脱敏诊断包（rpc_secret/headers/proxy userinfo 掩码）、每任务日志上限 500 行、Inno 卸载清理 plugins/licenses |
| `6481b84` | 清单 `rollout` 稳定分桶灰度（签名覆盖，需重签调档）、make-update-manifest.py 自动生成清单（AppImage 优先/版本校验/未签名即丢弃）、`aether-cli export/import` 设置+任务迁移（不导出凭据，`--settings` 显式应用） |
| `ee2ab81` | 单实例锁（fs2/配置目录锁文件，第二实例通知后退出）、关闭到托盘（仅托盘真实可用时；Linux 托盘无 watcher 改为诚实返回 None）、开机自启（Linux XDG / Windows HKCU Run / macOS LaunchAgent，状态以 OS 为准） |
| `1d75cd5` | Windows 桌面通知（notify-rust toast + HKCU AUMID 一次注册，失败回退内置归属）+ Windows 发布产物补齐（daemon/CLI/native-host + 16 插件 exe 进 zip/安装器 `{app}\plugins`）+ default_plugins_dir exe 同级候选 |
| `716896e` | 托盘平台分发（tray.rs → tray_sni.rs + tray_windows.rs 零依赖 Shell_NotifyIconW FFI）、zbus 移 unix-only、CI Windows 跑 bin 测试 |
| `582faa8` | 设置页逐项审查：`update_check` RPC + `app_auto_update` 键（自动下载开关成真）、关于页真实打开文档/社区/反馈/版本说明/许可、PAC 回弹、重置补语言与主题；新增 `docs/SETTINGS_AUDIT.md` |

- **设置页现状**：10 个分类中每个控件要么真实生效、要么是诚实的能力说明卡，
  没有「点了没反应」或显示假值的控件。逐项落点与边界见
  `docs/SETTINGS_AUDIT.md`（每项控件 → 处理函数 → 真实/只读/外部）。
- **质量门禁**：`cargo fmt --all --check` + 全仓 86 套件 + CI 三平台
  （Linux 全量测试，Windows 测 transport + rpc 回环 + bin 测试（托盘 FFI/i18n）
  + 全量 check，macOS check）。macOS runner 偶发 cancel——`gh run rerun <id> --failed` 重跑即可，
  不是代码问题。Linux 上 `system_proxy` stub 测试族偶发失败（runner 环境），
  单跑必过，重跑即可。

## 2. 架构速查（新会话必读）

```text
UI (src/backend.rs 后台线程 + src/main.rs)
  │  JSON lines over unix socket（契约：docs/UI_INTEGRATION.md）
  ▼
daemon (crates/aether-daemon/src/server.rs，单线程拥有 State)
  │  HostCommand::Init / Start / Pause…（docs/PLUGIN_ABI.md）
  ▼
插件进程（16 个第一方，SDK：crates/aether-plugin-sdk）
```

配置有四条通道，别混用：

1. **daemon settings.json**（`<download_dir>/.aether/settings.json`）：
   `KEYS` 白名单 + whole-patch 校验（坏补丁整体拒绝，旧设置继续跑）+
   `resolve_limits` 启动写回（get_settings 回报生效值）+ `settings_changed`
   事件广播。改这里要同步：`Settings` 字段、`KEYS`、`validate()`、
   `set_settings` 分支、`every_serialized_field_is_a_known_key` 测试。
2. **NetworkConfig**（SDK `protocol.rs`，经 `Init.network` 下发进程级）：
   `user_agent/ip_mode/dns/default_headers/ftp_passive/bt_*`。
   **进程级配置在 spawn 时固定**——daemon 在这些键变化时 `restart_idle_plugins`
   （只停非 busy 插件；运行中任务保持原进程，UI 提示「空闲插件重启后生效」）。
3. **TaskSpec.settings**（任务级，随 add 下发）：
   `headers/referrer/cookies/user_agent/max_speed_bps/checksum/initial_peers/…`，
   任务级同名键优先于全局。
4. **UI 本地偏好**（`~/.config/aether/ui.json`，`src/main.rs` 的 `UiPrefs`）：
   主色/动效/流线/启动页/完成后动作/通知开关——无 daemon 对应物。

调度入口在 `server.rs`：`try_launch` 门控（max_concurrent、全局限速按
`active+1` 均摊、`schedule_in_window`），`tick_schedule` 每 15s 强制计划窗口
（`schedule_paused` 集合只恢复调度器自己暂停的任务）。

## 3. 开发工作流与已知坑

- **跑测试**：`cargo test --workspace`（86 套件基线）；BT 契约测试需要
  `--test-threads=1`；`cargo fmt --all --check`。
- **daemon e2e**（`crates/aether-daemon/tests/daemon.rs`）：
  - 先 `cargo build -p aether-plugin-http`（等被 stage 的插件）——`stage_http_plugin`
    从 target 拷二进制，改源码不重编会跑旧代码。
  - 一律 `bind` 端口 0；本机 8088 被 QEMU 占用。
  - DNS 测试用 **`.test` 域名**：hickory 对 `.invalid/localhost/local/onion` 按
    RFC 6761 本地判 NxDomain，永不联网。
  - DNS stub 应答 **RDLENGTH 写 2 字节**，AAAA 按 qtype 返回 ::1，否则整次查询失败。
  - **probe 失败被宽容放行**（supported:true）：`add` 成功 ≠ 网络通，断言要落在
    任务终态。
- **Xvfb 冒烟**（配方见 `scripts/shot.sh`）：必须
  `env -u WAYLAND_DISPLAY WINIT_UNIX_BACKEND=x11 LIBGL_ALWAYS_SOFTWARE=1`，
  否则 winit 连到宿主 Wayland，Xvfb 里只有 1×1 残窗。无窗口管理器：
  点击后要 `xdotool windowfocus <winid>` 再 `type`。清理进程用 `killall` 按
  comm 名（`pkill -f` 会自匹配 shell 命令行；symlink 启动的 daemon 进程名是
  链接名）。冒烟脚本样例：`/tmp/opencode/aether-smoke2/smoke*.sh`（临时目录，
  丢了按本文重写即可）。
- **Slint**：`SettingRow` 描述文字 `wrap: word-wrap`；Material 组件强调色走
  `MaterialPalette.brand-primary ← Aether.accent-base`；动效 token 可被
  Rust 置零。提交前留意 `slint_build` 报错里的文件:行号。
- **Windows named pipe**（transport.rs）：
  - **bind 只存名不算绑定**——实例存在管道才存在，必须在 bind 时预创建
    第一个实例，否则 bind~accept 之间 connect 报 `ERROR_FILE_NOT_FOUND`
    （CI 回环测试抓到过）。
  - `std::os::windows::raw` 在 stable 不导出 `HANDLE`（私有）/`BOOL`（无），
    本地声明 `type HANDLE = *mut c_void; type BOOL = i32`；raw handle 跨
    线程要 `unsafe impl Send`；`ConnectNamedPipe` 失败后先取
    `last_os_error()` 再 CloseHandle（否则错误码被覆盖）。
  - 本机 Linux 无法交叉验证 Windows 编译（ring/rusqlite 需 MSVC/mingw），
    **Windows 侧只有 CI 是权威判定**，别只看本地 `cargo check`。
  - **CI 并发抢占**：`ci.yml` 的 concurrency 组 `cancel-in-progress: true`——
    在新 run 进行中时 `gh run rerun --failed` 会把进行中的 run 打成
    "cancelled"（曾把 4.6a 的 macOS job 误杀出假失败）。重跑失败 job 前先
    确认组里没有更新的 run 在跑/排队。
  - **ETXTBSY（os error 26）**：exec 一个正被写入的二进制时报此错——本质
    瞬态。media transcode 的 spawn 已带 3×250ms 有界重试（CI 假 ffmpeg
    fixture 曾被它咬过）；写测试脚本再立即执行的模式都要留心。
  - **Windows 上集成测试的 daemon 端点必须是 `\\.\pipe\` 名**，不能拿
    `root/daemon.sock` 当 socket（`Listener::bind` 报错、daemon 起不来、
    用例全灭——rpc.rs 踩过，见 `2d9fa72`）。

## 4. 下一批特性（按价值排序）

### 4.1 Windows 完整支持（named pipe + 通知 + 插件打包）★

- **已落地（2026-10-29，main CI 全绿）**：`crates/aether-daemon/src/transport.rs`
  抽象了「JSON lines 双向流」——Unix 走 socket 文件（0600），Windows 直接 FFI
  kernel32 实现 named pipe（`\\.\pipe\aether-<user>`，默认 DACL 限同用户，
  无新增依赖）；`server.rs/client.rs` 与 UI `src/backend.rs`、CLI
  remote/import、native-host 的 `#[cfg(unix)]` 门全部移除，Windows 有真实
  后端；`tests/transport.rs` 回环测试在 CI Windows job 实机通过。
- **剩余两项已落地（2026-10-30，`1d75cd5` CI 三平台全绿）**：
  - **Windows 桌面通知**：`send_notification` 接 notify-rust（4.x，MIT/Apache；
    Windows 后端 tauri-winrt-notification 同许可，unix/mac 后端依赖仅进
    Cargo.lock 不参与 Windows 编译）。toast 按 AUMID 归属：首次通知时
    `reg add` 注册 `HKCU\Software\Classes\AppUserModelId\com.pengtang1024.Aether`
    （DisplayName/IconUri，免管理员、进程内只试一次）；注册被拒则不设
    app_id、回退 notify-rust 内置兜底（可显示、归属降级，诚实优于缺失）。
    `show()` 失败记一次日志不 panic。Linux/macOS 仍是 notify-send/osascript。
    测试进 CI Windows job：AUMID 键真实写入（reg query 断言）+ toast
    best-effort 不 panic。
  - **发布产物打包插件 exe + 全套二进制**：package.yml Windows job 改
    `cargo build --release --workspace` + `stage-plugins.py`（manifest 的
    windows-x86_64/arm64 key 16/16 核过全有）；zip 与 Inno 安装器收录
    aether-daemon / aether-cli / aether-native-host 与 `plugins\*`
    （→ `{app}\plugins`）。闭环：UI 自启 daemon 找 exe 旁
    `aether-daemon.exe` ✓；`default_plugins_dir` 加 **exe 同级 `plugins\`**
    候选（aether-host `plugin_dir_candidates()`，unix 顺序不变）→ 安装/便携
    布局都能解析；CI Windows job 加 `cargo test -p aether-host plugin_dir`。
    macOS 包已补齐（2026-10-30，第二波）：`cargo build --workspace` 双 arch +
    lipo 四个二进制进 Contents/MacOS + 插件双 arch lipo 进
    Contents/Resources/plugins；`default_plugins_dir` 的 macOS 候选按 bundle
    布局解析（见 §4.10）。
  - 交叉编译认知更新：装 `gcc-mingw-w64-x86-64`+`nasm` 后本机即可
    `cargo check --workspace --all-targets --target x86_64-pc-windows-gnu`
    （见 §4.5 Windows 托盘条），Windows 编译错不再只能等 CI。

### 4.2 RPC over TCP（远程控制）★

- **已落地（2026-10-29）**：
  - transport 抽象加了 TCP 分支（`Listener::bind_tcp` / `Conn::connect_tcp`），
    行协议与本地 socket 完全一致。
  - daemon settings 新增 `rpc_listen`（`ip:port`，默认关闭）与 `rpc_secret`；
    `--rpc-listen` / `AETHER_RPC_LISTEN` 覆盖 `rpc_listen`。非 loopback 地址
    必须有 `rpc_secret`，否则**拒绝启动**（boot 路径和 `set_settings` 都校验）。
  - 每个 TCP 连接的首条消息必须是
    `{"method":"auth","settings":{"token":"…"}}`，常数时间比较；错 token/
    无 auth 直接拒绝。`set_settings` 轮换 `rpc_secret` 即时生效（新连接用
    新 token，无需重启监听）。
  - CLI：`aether-cli remote … --socket ip:port [--token …]`（token 也可用
    `AETHER_RPC_TOKEN`）走 RPC；`Client::connect_rpc(addr, token)` 是客户端
    入口。
  - 验收：`tests/rpc.rs` 6 个用例（正确 token 通/错 token 拒/无 auth 拒/
    authed 会话含 subscribe+shutdown/非 loopback 无 secret 拒启/token 轮换），
    CI Windows job 与 transport 回环一起实跑。
- **明确不做**：TLS——文档写明「仅限可信网络或 SSH 隧道」。

### 4.3 浏览器扩展接管

- **已落地（2026-10-29）**：
  - **Cookie 显式授权**：manifest 把 `cookies` 放进 `optional_permissions`；
    选项页开关触发浏览器自己的权限请求，默认关。开启后扩展用
    `chrome.cookies.getAll` 组装 Cookie 头随任务下发（http 插件已消费）。
  - **站点过滤**：选项页按 host 匹配（`example.com` / `*.example.com`，
    空 = 全部），只作用于接管模式；右键菜单不受限。
  - **Windows 安装**：`scripts/install-native-messaging.ps1`——HKCU 注册表
    （Chrome/Edge/Brave）+ Firefox `%APPDATA%` 文件，免管理员；
    UI 的 `browser_extension_installed()` 在 Windows 查注册表 + Firefox 文件。
  - **透传有测试**：`aether-native-host/tests/native.rs` 断言
    referrer/cookies/headers/filename 一路进 `add` 的 `TaskSpec.settings`；
    另有 `file://` 拒绝用例。
- **诚实的限制**：扩展是 JS，仓库没有浏览器测试基建——Cookie/过滤等扩展侧
  行为靠选项页 “Test native host” + 手工冒烟；商店上架（签名、审核）未做。
- 通道维持「扩展 → native host（一次性进程）→ 本地 socket/管道」：同用户
  权限即信任，不需要额外 token；远程投递用 4.2 的 RPC。

### 4.4 媒体转码（ffmpeg 管线）

- **已落地（2026-10-29）**：
  - media 插件新增 `transcode` 模块：HLS/DASH 下载完成后，任务级
    `transcode_to`（容器名）触发 `ffmpeg -c copy` 转封装（不重编码）。
    `-progress pipe:1` 的 `out_time` 解析进进度事件（UI 显示为处理中的
    计数；没有字节总量可映射）。
  - **失败永不毁文件**：ffmpeg 缺失/非零退出/无输出 → 保留原文件、任务
    正常完成，原因（含 stderr 尾行）写进 `plugin_state.transcode.note`；
    半写输出被清理。容器名做了白名单校验（≤8 字符字母数字），不进 shell。
  - daemon settings 新增全局 `ffmpeg_path`（KEYS 第 21 键）：`add` 时注入
    任务 settings（`entry().or_insert`——任务自带的不被覆盖），rtmp 与
    media 共用同一语义（task.settings.ffmpeg_path > AETHER_FFMPEG > PATH）。
  - UI 媒体页：ffmpeg 路径行（回车提交，提示「对之后开始的任务生效」），
    HLS/DASH 描述更新为已支持转封装。
  - 验收：`tests/transcode_e2e.rs` 假 ffmpeg e2e（4 用例：argv 断言
    `-c copy`/`-progress pipe:1`/真实路径、退出 0 换名删原件、退出 1 保留
    原件且 note 含 stderr、缺失二进制保留原件、无 `transcode_to` 不碰文件）
    + daemon e2e 注入/覆盖用例 + 容器名校止单测。
- **诚实的边界**：只做 `-c copy` 转封装（容器不兼容时 ffmpeg 会失败→保留
  原件）；不做音视频分离合并、不重编码。直播 HLS 仍不支持。

### 4.5 托盘 / 剪贴板 / i18n

- **剪贴板监听已落地（2026-10-29）**：
  - `arboard`（`default-features = false` + `wayland-data-control`，避免拖入
    image/tiff/fax 重依赖）+ 独立轮询线程（1.2s，只在**链接变化**时上报）。
  - 默认关（`UiPrefs.clipboard_watch`，剪贴板访问是显式授权的事）；开启时
    基线重播种，不会对已有内容误触发。
  - 检测规则：**首 token** 匹配 http/https/ftp/ftps/magnet/ed2k/thunder 才报
    （`file://`、`javascript:`、纯文本路径、句中 URL 都拒绝——plugin probe
    反正也会拒，不让用户白点一次）。
  - UI：新 toast 动作按钮（「新建任务」）→ 点击打开新建对话框并**预填 URL**；
    设置页常规面板加「剪贴板」分区（开关 + 诚实描述）。
  - 无剪贴板后端（远程/无图形会话）时 `clipboard::start()` 返回 None：
    开关禁用 + 描述说明原因，`clipboard-watch` 事件被拒并提示，不假装可用。
  - 冒烟验证（Xvfb + xclip 端到端）：复制 URL → toast 出现 → 点击 →
    对话框预填成功；设置页开关/描述渲染正确。
- **托盘已落地（2026-10-29，Linux）**：`src/tray.rs` 用 zbus（树内已有，
  零新增依赖树）实现 StatusNotifierItem + com.canonical.dbusmenu 菜单
  （显示主窗口/退出，`/Menu`+`/MenuBar` 双路径）；无 session bus 或无
  StatusNotifier host 时优雅降级（一行日志，主程序照常——GNOME 需
  AppIndicator 扩展才显示）。**私有 session bus 全链路实测**：bus name、
  Title 属性、GetLayout（label 键值正确）、Activate 唤起、Event→退出
  （app 实际退出）。当时 macOS/Windows 托盘未做（Linux 第一）——Windows 见下条，macOS 延后见说明。
  2026-10-30 诚实化：无 StatusNotifierWatcher 时 `start` 返回 None（原来
  返回 Some 但图标不可见）——「图标不可见就不假装有托盘」，关闭窗口因此
  保持退出语义（见 §4.8）。
  坑（都踩过）：zbus 5 **没有** `#[zbus(method)]`（普通方法自动识别，
  method 级合法键仅 name/signal/property/out_args/proxy）；zvariant 5 的
  `Signature` 是解析树 + `Type::SIGNATURE` 常量，derive 用
  `#[zvariant(signature = "...")]` 显式覆盖（const 内临时 Signature 会
  E0493）；**serde 序列化 `Value` 会包成 variant(v)**——derived 结构体的
  typed 字段必须用 Dict/Array（按声明签名序列化），否则签名静默错配；
  冒烟三坑：`pkill -f` 会匹配自己 heredoc 正文里的裸字符串（字符串拼接
  避开）、busctl/gdbus 必须与 app 在同一 `dbus-run-session` 内否则查的
  是另一条 bus、私有 bus 的 portal 激活刷屏需过滤；gdbus 给 `v` 参数传
  空串 variant 写 `"<''>"`。
- **Windows 托盘已落地（2026-10-30）**：
  - `src/tray.rs` 重构为平台分发器（Linux 实现原样挪进 `src/tray_sni.rs`，
    一行逻辑没动）；Windows 走 `src/tray_windows.rs`——**零依赖 raw FFI**
    （shell32/user32/kernel32，与 transport.rs 同哲学）：message-only 窗口
    （HWND_MESSAGE，不显示不绘制）+ `Shell_NotifyIconW` + WM_APP+1 v0 回调
    （左键/双击→唤起、右键→显示主窗口/退出 菜单，SetForegroundWindow+WM_NULL
    处理菜单吞首击的经典坑）；图标取 exe 旁 `logo.ico`（Inno 打包本就放置），
    加载失败回退 IDI_APPLICATION（诚实降级，不装死）。
  - `Hooks{on_show,on_quit}` 解耦 Slint：dispatcher 从 `Weak<AppWindow>` 适配，
    测试用 no-op 起图标，无需窗口/显示环境。`zbus` 移入
    `[target.'cfg(unix)'.dependencies]`——Windows 构建不再编译 zbus。
  - **CI Windows job 新增 `cargo test --bin aether`**：FFI 布局测试
    （`NotifyIconData` 976B/x64、960B/x86 + hIcon/szTip/guid 等 offset 断言，
    防 cbSize 协商被手滑破坏）、message-only 窗口创建/销毁回环、start 契约
    （会话有 shell→Some、无 shell 拒绝图标→None，**两者都过**；硬性要求是
    5s 内回报且不 panic——CI runner 无桌面，按 best-effort 验收）。
  - **本地交叉预检配方**（Linux 上先于 CI 抓 Windows 编译错）：
    `apt install gcc-mingw-w64-x86-64 nasm`（nasm 是 aws-lc-sys 的汇编器，
    缺了整仓 check 直接挂）→
    `cargo check --workspace --all-targets --target x86_64-pc-windows-gnu`。
    本波 4 个编译错（LoadImageW 参数 `null`→`null_mut` 可变性、with_hooks
    借用出 `&State` 而非 `&Hooks`、缺 `ComponentHandle` import）推送前即抓掉。
- **macOS 托盘：诚实延后**：需要 objc2/NSStatusItem，而本仓 macOS CI 仅
  `cargo check`，没有任何运行时验证路径——**无法验证的托盘不做**（Linux
  （2026-10-29）/Windows（2026-10-30）先落地；macOS 记为未来项：先补 macOS
  运行时冒烟路径再谈实现）。
- **i18n 已落地（Slint 侧，中文优先，2026-10-29）**：
  - 源串保持中文（第一语言，编译器 index0 = 源语言槽，天然默认）；
    `build.rs` 启用 `with_bundled_translations("translations")` +
    `DefaultTranslationContext::None`（免 msgctxt）+ `rerun-if-changed`。
  - `translations/en/LC_MESSAGES/aether.po`：**全量 202 条** UI 串翻译
    （text/title/subtitle/description/placeholder-text/label 两轮脚本扫除
    含行中出现与跨行三元表达式）。运行时
    `slint::select_bundled_translation("en"/"")`（空串=中文源）即时切换，
    设置→外观→界面语言；UiPrefs.lang 持久化，启动先选后渲染（不闪）。
  - **故意不译**：ComboBox 的 model 值字符串同时是传给 Rust 的功能值
    （`changed(key, self.current-value)`），翻译会破坏匹配——要译需先改成
    index 语义（记为未来重构项）。
  - Xvfb 双语言截图核验：中文默认 ✓ / en 全英文（含导航、条件描述两分支）✓。
  - **Rust 侧也已落地（同波）**：`src/i18n.rs` 的 `tr(zh, en)` 内联双语对
    （不做 key 表——表会与译文漂移）+ 进程级语言位（启动按 UiPrefs.lang
    设、切换时更新；后台 worker 的 notice 走同一语言位）。清扫范围：toast、
    设置 hint、backend notice、相对时间/ETA（rel_time/format_eta）、插件
    描述与分类、仪表盘统计串、桌面通知、support 提示；**功能值不译**
    （ComboBox 值同时是 Rust 匹配的 wire value，如 跟随系统/直连/浅色）。
    @tr 残留扫描脚本验收：Slint 与 Rust 的用户可见中文串均已被 tr 覆盖
    （多行 tr 参数需逐行复核，扫描器会误报）。Xvfb 双语言截图核验
    英文主页 + 中文默认设置页。
  - 已知小瑕疵：英文 "Medium" 被限速药丸宽度裁切（en 比 zh 宽→已改译 "Med"）。
  - **多语言已扩展（2026-10-30）**：日语（ja）+ 繁體中文（zh-TW）全量上线。
    - Slint 侧：`translations/{ja,zh-TW}/LC_MESSAGES/aether.po`（250 条 msgid，
      构建期打包），语言下拉扩为 4 项（经 `i18n::LANGUAGES` 单一来源，
      display 名↔code 映射，防 UI 与运行时漂移）。
    - Rust 侧：`src/i18n/table.rs` 单表多语言行 `(zh, ja, zhT)`——`tr(zh,en)`
      签名不变，按 zh 源串查表，**空单元格/缺行回退中文**（不留空串）。
      **范围规则：仅静态串入表**；`format!()` 先格式化再进 tr，key 是动态的
      无法查表 → ja/zhT 下这些动态串（带值的提示/通知）回退中文——
      要覆盖需改调用点结构（记为未来项）。
    - Xvfb 三语截图核验：日语设置页+主页、繁中设置页（剪貼簿/資料夾/
      儲存等台湾用语）✓。加语言配方：po 一个 + table 一列 + LANGUAGES 一项。
  - 扩展商店打包：`scripts/build-extension.sh`（store 上传 zip，剔除内部
    文档）+ `browser-extension/store-listing.md`（Chrome/Edge/AMO 文案与
    权限理由）；上架本身是人工步骤（需商店账号，AMO 还需隐私政策）。

### 4.6 插件安装/更新接 daemon + manifest 签名

- **4.6a 已落地（安装 + 签名）**：
  - **签名契约**：`plugin.json` 新增 `files`（包内相对路径 → sha256，不含
    plugin.json 自身）；`signature` = base64 Ed25519 over **canonical bytes**
    = 移除 `signature` 键后的 `serde_json::Value` 序列化（workspace 未启用
    preserve_order → BTreeMap 有序，与文件格式/键序无关）。签名覆盖 files →
    篡改任何包内文件都会被抓住。信任根复用 update 的 release key
    （`AETHER_UPDATE_PUBKEY` / `--update-key`）。
  - **策略**：配置了公钥 → 签名必填 + 逐条验 files；无公钥 → 拒绝，
    除非 `AETHER_ALLOW_UNSIGNED_PLUGINS=1`（显式开发逃生门，按次请求检查）。
  - **工具**：`aether-plugin-sign <package_dir>`（aether-update 新 bin，
    复用 `AETHER_UPDATE_SECRET_KEY`）——算 files、canonical 签名、回填。
  - **daemon**：`install_plugin` RPC（validate + api/host 版本 + 可执行路径
    安全 + 验签 → staging 拷贝 → swap 失败回滚旧版 → 重扫 registry 保留禁用
    列表；运行中任务占用的插件拒绝覆盖，空闲旧进程退役）+ `rescan_plugins`
    RPC（重扫 + 重放禁用列表，供「重新加载」按钮）。
  - **UI**：安装行改为「包目录路径 LineEdit + 安装按钮」（结果经 notice
    toast 反馈）；「重新加载」接 rescan；三个假开关（自动更新/外部插件/
    开发者模式）改为诚实能力说明文本。
  - **验收**：`aether-update/tests/plugin_sign.rs`（签名工具 roundtrip：重排
    重格式化仍验过、篡改二进制被 files 抓、篡改 manifest 被签名抓、错 key/
    无 files/缺文件/路径穿越全拒）+ `aether-daemon/tests/install.rs` 3 用例
    （未签名策略开关、签名/错 key/篡改/覆盖安装、rescan 保留禁用状态）。
- **4.6b 已落地（更新下载链）**：
  - `UpdateManifest.plugins`：`{version, url, sha256, size?}` 按插件 id 索引，
    serde default → 旧清单照常解析。信任链：清单整体 detached 签名（已有）
    覆盖条目 → 下载按 sha256 校验 → 包内 4.6a manifest 签名在 install
    时再验一道（纵深）。
  - 包格式 `.tar.gz`（plugin.json 在归档根）；`unpack_plugin_archive` 防护：
    仅普通文件/目录、相对路径（拒 `..`/绝对路径/符号链接）、总量上限
    256 MiB。
  - daemon：`plugin_updates`（未配置 update_url+公钥时诚实返回
    configured:false）对比已装版本；`plugin_update_all` 逐条下载→验哈希→
    解包→复用 install_package（验签+staging swap）；单条失败（忙碌/坏包/
    签名）不中断其余。`check_manifest()` 从 `check()` 拆出（插件条目不
    受主程序版本比较影响）。
  - 自动更新：settings 第 22 键 `plugin_auto_update`；update loop 每 6h
    以 daemon client 身份执行 `plugin_auto_update_pass`（开关关=静默跳过）。
  - UI：「更新全部」接真调用（toast 结果摘要）；「自动更新插件」恢复为
    真开关（镜像 settings，提示 6h 周期与前置条件）。
  - 验收：`aether-update/tests/update_schema.rs`（旧清单兼容、解包防护
    5 例：穿越/绝对路径/符号链接/炸弹/非 gzip，恶意条目手工构造 ustar 头
    ——tar Builder 本身拒绝 `..`）+ `plugin_update.rs` 4 用例（未配置诚实
    返回+设置往返、check 报差异、update_all 装新+跳过已最新+坏包/哈希错
    各自独立失败且不落盘、auto pass 开关门控）。

### 4.7 发布与供应链

- **已落地（2026-10-29）**：
  - **Actions pin SHA**：ci.yml/package.yml 全部 6 个 actions 钉死真实
    40 位 commit SHA + 版本注释；SHA 全部经 `gh api` 取得（annotated tag
    peel、dtolnay 分支解析），版本注释经 tag 反查校正（不能凭记忆写）。
    Dependabot 已为全部 pinned actions 自动开 SHA 更新 PR——pin 写法
    （`# vX.Y.Z` 注释）正是它识别的格式。
  - **cargo-deny**：`deny.toml`（宽松许可 allow 集、slint 三许可 exception
    指向真实 crate 名——以 `cargo deny` 实测拒绝清单为准，猜的名字全错；
    4 条带理由的 unmaintained ignore（zbus 家族/ttf-parser，slint 传递且
    公告写明无升级路径）；未知 registry/git 拒绝）。本机四项全绿；CI 新增
    pin SHA 的 cargo-deny job（已绿）。
  - **SBOM**：package.yml release job 在 SHA256SUMS **之前**跑
    cargo-cyclonedx（`--format json --override-filename aether-$V.cdx`，
    注意 workspace 模式每成员一文件、工具会追加 `.json` 后缀——已本地实测
    后才写进 workflow）→ CycloneDX 随 release 资产附带且被校验和覆盖。
  - **AppStream**：metainfo 去掉 "mockup/no networking" 谎言，补 0.2.0，
    `appstreamcli validate` 通过（`make check` 一环）。`.deb` 升级提示走
    既有 `update_status` RPC + dashboard。
  - **优先级老化**：`next_queued` 排序 `priority + MIN(100, wait_min/300)`
    ——**等待奖励**而非惩罚（惩罚等待者会反向饿死低优先级），+100 头部
    优势约 8.3h 后被公平竞争取代，平局回到最老优先；store 测试 3 断言。
  - **批量导入**：新建对话框 URL 字段按空白切分（`backend::split_urls`
    单测：多链接/多磁力/制表符/空输入），一次提交排队多个、toast 报数量；
    CLI `import` 文件列表为既有能力。

### 4.8 后台形态（单实例 / 关闭到托盘 / 开机自启）

- **已落地（2026-10-30，`ee2ab81` CI 三平台全绿）**：来自 GAP_AUDIT #3。
  - **单实例**：`src/instance.rs` 用 fs2 的 `try_lock_exclusive`（flock /
    LockFileEx）锁配置目录 `aether/instance.lock`；进程退出/崩溃由 OS 释放，
    无 stale lock。三态 Primary(Guard) / AlreadyRunning / Unlocked（配置目录
    不可用等环境问题照常启动——双开好过不开）。第二实例发系统通知
    「Aether 已在运行」后退出。**已知限制：不唤起已有窗口**（需 UI 级 IPC），
    通知文案即说明「请使用已经打开的窗口」。冲突判定 unix WouldBlock /
    Windows ERROR_LOCK_VIOLATION(33)。
  - **关闭到托盘**：Slint 1.18 的 CloseRequestResponse 只有 HideWindow /
    KeepWindowShown（**无 Quit**）→ 仅当托盘真实可用才注册关闭→隐藏，托盘
    菜单「退出」走 quit_event_loop；无托盘保持关闭=退出（绝不把应用藏进
    看不见的地方）。Linux 托盘随之诚实化（见 §4.5 注）。
  - **开机自启**：`src/autostart.rs`，状态源是**操作系统**而非 ui.json（启动
    时读真实状态，手删也如实反映）：Linux XDG autostart desktop entry（Exec
    含空格按 spec 加引号）；Windows HKCU `...\CurrentVersion\Run`（reg
    add/delete，免管理员，值带引号）；macOS LaunchAgent plist（RunAtLoad，
    XML 转义；**未做运行时验证**——macOS CI 仅 check）。写失败时开关回弹 +
    错误提示，不假装成功。
  - UI：设置→常规→启动 新增「开机自启」开关 + 「关闭窗口」说明行；属性链
    GeneralPanel ↔ SettingsPage ↔ AppWindow；新增 Slint 串 4 条进
    en/ja/zh-TW po，Rust 静态串 4 条进 `i18n/table.rs`。
  - 验证：单实例双开 Xvfb 冒烟（第二实例 0 退出、第一实例存活、锁文件真实
    存在）；开机自启点击 e2e（desktop entry 写入/删除、开关状态、toast）；
    单测覆盖双开/释放、entry 内容与引号、临时目录往返、Windows 注册表往返
    （CI Windows bin 测试实跑）。

### 4.9 更新灰度与迁移导出

- **已落地（2026-10-30，`6481b84`）**：
  - **灰度发布（无服务器）**：`UpdateManifest.rollout: u8`（serde default 100，
    >100 报协议错）。`UpdateClient::check()` 版本比较后过闸门：
    bucket = `sha256(hostname\nversion)[0] % 100`（hostname 取
    HOSTNAME/COMPUTERNAME，缺省 aether）——同机同版本恒定、无随机、无新依赖；
    `rollout=0` 无人 / `100` 全量 / 其余 `bucket < rollout`。只作用于主程序，
    `check_manifest()`（插件通道）不受影响。**签名覆盖 rollout**：调档要重签，
    已在 UPDATE.md 写明。
  - **清单生成**：`scripts/make-update-manifest.py` 扫发布产物生成 update.json
    （deb/AppImage/Windows zip/macOS dmg；同 key AppImage 优先于 deb；setup.exe
    不进清单；文件名版本必须等于 --version；sha256+size+tag 固定 URL）。
    package.yml `publish` job 在 SBOM 后生成；有 `AETHER_UPDATE_SECRET_KEY`
    secret 才签名，否则 `::warning::` + 删除未签名清单（未签名不得流出）。
  - **设置/任务迁移**：`aether-cli export <file|->` 输出
    `aether-export/1`（settings + tasks[url/plugin_id/priority/state/target_dir]）；
    **不导出任务级 settings**（Cookie/token 隐私边界）。`import` 自动识别导出
    JSON 与 URL 行列表；`--settings` 才应用导出配置，`--dir` 覆盖 target_dir。
    集成测试：daemon e2e 导出→改 URL→导入（含 --settings）→ 任务数断言。
  - 剩余（需外部条件）：真实灰度调档要离线私钥重签；更新服务器/CDN 用
    GitHub Releases 固定地址即可（`releases/latest/download/update.json`）。

### 4.10 无障碍与 macOS 包补齐

- **已落地（2026-10-30，`8e69f67` CI 三平台全绿）**：
  - **无障碍**：slint 开启 `accessibility`（AccessKit 桥）。语义补齐：
    RailButton/NavRailItem/SettingsNav/FilterPill/RowAction/Toast 动作按钮
    为 button（label + checked + action-default），TaskItem 为 list-item
    （label=任务名、description=状态、item-selected），ProgressTrack 为
    progress-indicator（accessible-value 百分比）。新增 Slint 串
    「重试/继续/删除」进三语 po。**诚实边界**：仓库无屏幕阅读器运行环境，
    验证到 feature 编译 + Xvfb 启动渲染无回归 + 语义静态审查；真机
    NVDA/Orca 验证记为外部步骤。
  - **macOS 包补齐**：package.yml macOS job 改 `cargo build --release
    --workspace`（双 arch），aether/aether-daemon/aether-cli/
    aether-native-host 逐个 lipo 进 Contents/MacOS；插件经
    `stage-plugins.py --merge-target-dir` 双 arch lipo 后进
    Contents/Resources/plugins。`default_plugins_dir` 加 macOS 候选：
    `../Resources/plugins`（bundle）→ `plugins`（便携）→ /usr/lib →
    build/plugins → plugins；unix 非 mac 顺序不变。release 流水线首次
    实跑在下一个 release 分支（macOS 无本地运行环境，如实标注）。

### 4.11 持久化与可观测性

- **已落地（2026-10-30，`8406446` CI 三平台全绿）**：来自 GAP_AUDIT #5/#15 中不依赖外部
  条件的两项。
  - **在线备份 + 完整性检查**：rusqlite `backup` feature（树内 crate，无新增
    依赖）；`TaskStore::backup_to`（SQLite backup API，daemon 运行中安全复制）
    与 `integrity_check`（PRAGMA）；RPC `backup {path}` / `db_check`；CLI
    `remote backup <dest>` / `remote db-check`。恢复 = 停 daemon 换
    `aether.db`。测试：内存库→备份→重开→任务数与 integrity 断言。
  - **日志保留上限**：`append_log` 同事务剪枝，每任务仅留最新
    `TASK_LOG_LIMIT=500` 行，话痨插件无法撑爆 DB；UI/CLI 读同一尾部；测试
    520→500 且最旧被删。
  - **诊断包**：`aether-cli diag <file|->` 输出 `aether-diagnostics/1`（版本/
    平台/daemon 版本/settings 脱敏/任务计数/插件清单）；脱敏 `rpc_secret`、
    `headers`、proxy.url userinfo；**不含任务 URL 与任务级 settings**；单测
    覆盖脱敏，e2e 验证 16 插件与 `<redacted>`。
  - **卸载清理**：Inno `[UninstallDelete]` 删除 `{app}\plugins` 与
    `{app}\licenses`（安装器自有），用户配置/下载不动。

### 4.12 设置页诚实化（逐项审查）

- **已完成**：逐个控件核对落点，修掉最后几处名不副实：
  - 「自动下载更新」接入新设置键 `app_auto_update`，更新循环每轮重读
    `settings.json`；关闭后只把状态置为 `available`，不下载。
  - 「检查更新」新增 `update_check` RPC：后台线程执行清单拉取/校验/下载，
    不阻塞下载主循环，结果经 `stats.update` 回传 About 面板。
  - 「更新通道」删除（后端无此概念），改为只读「更新源」说明；无源时
    `update_check` 返回 `disabled`，界面照实显示「未配置更新源」。
  - 文档/社区/反馈/版本说明/许可改为真实 `open_url`（GitHub 仓库、
    Discussions、Issues、CHANGELOG、LICENSE），打开器失败时提示 URL。
  - 关于面板的状态行覆盖 idle/checking/up_to_date/available/ready/error。
  - 重置界面偏好补齐语言（zh）与主题（跟随系统）。
  - PAC 选择回弹到 daemon 实际模式，并说明由 `AETHER_PAC_URL` 决定。
- **测试**：`tests/rpc.rs` 覆盖无源 `update_check=disabled`、
  `app_auto_update` 往返/持久化/坏类型拒绝；`src/main.rs` 覆盖
  `update_status_text` 全状态分支；三条新 @tr 文案补齐 en/ja/zh-TW。
- **不变边界**：签名/公证、商店上架、云盘 OAuth、屏幕阅读器真机、
  macOS `.app` 实跑需要外部账号或硬件，设置页不提供假开关。

## 5. 发布流程摘要

- 发布仅在 `release/*` 分支触发，**分支名必须等于 `Cargo.toml` 版本**
  （如 `release/0.3.0`）；完整流程见 `RELEASING.md`。
- 更新签名密钥在 `~/.ssh/aether-update-signing.key/.pub`——**绝不入库、
  不贴日志**。
- 教训（`WITHDRAWN_RELEASES.md`）：发布资产缺第三方许可文件会立即撤回；
  `scripts/check-license-bundles.py` 是门禁，别绕过。
