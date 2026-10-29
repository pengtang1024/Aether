# 路线图与会话交接

> 面向后续会话/贡献者：现在在哪、下一步做什么、怎么做、别踩哪些坑。
> 最后更新：2026-10-29（main @ `4f9493a`）。历史盲区清单见 `GAP_AUDIT.md`，
> 本文跟踪其中已完成的部分与剩余特性的具体方案。

## 1. 当前状态

- **0.2.0 已发布**（tag v0.2.0）。main 之上已合入五个波次：

| 提交 | 内容 |
| --- | --- |
| `6d6afe0` | 外观主色（6 预设）/动效/流线、启动页（含「上次页面」）、完成后动作、重置偏好；删除约 35 个无机制假控件 |
| `84424ab` | 同时下载数、每任务连接数、重试次数、全局限速 → daemon 调度旋钮（settings.json 覆盖 AETHER_MAX_*） |
| `1c1e8dd` | 默认请求头（HTTP 族）、FTP 主动模式、BT 端口/DHT/上传限速/屏蔽列表/Tracker → 插件引擎 |
| `4710e01` | 计划下载窗口（daemon 调度器，支持跨午夜）+ 完成/失败桌面通知 |
| `4f9493a` | Windows：named pipe transport（无新依赖），daemon/UI/CLI/native-host 全部跨平台，CI Windows job 跑回环测试 |

- **设置页现状**：10 个分类中每个控件要么真实生效、要么是诚实的能力说明卡，
  没有「点了没反应」或显示假值的控件。
- **质量门禁**：`cargo fmt --all --check` + 全仓 86 套件 + CI 三平台
  （Linux 全量测试，Windows 测试 transport 回环 + 全量 check，macOS check）。
  macOS runner 偶发 cancel——`gh run rerun <id> --failed` 重跑即可，不是代码问题。

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

## 4. 下一批特性（按价值排序）

### 4.1 Windows 完整支持（named pipe + 通知 + 插件打包）★

- **已落地（2026-10-29，main CI 全绿）**：`crates/aether-daemon/src/transport.rs`
  抽象了「JSON lines 双向流」——Unix 走 socket 文件（0600），Windows 直接 FFI
  kernel32 实现 named pipe（`\\.\pipe\aether-<user>`，默认 DACL 限同用户，
  无新增依赖）；`server.rs/client.rs` 与 UI `src/backend.rs`、CLI
  remote/import、native-host 的 `#[cfg(unix)]` 门全部移除，Windows 有真实
  后端；`tests/transport.rs` 回环测试在 CI Windows job 实机通过。
- **剩余**：
  - Windows 桌面通知：`send_notification`（src/main.rs）在 Windows 静默。
    优先 `notify-rust`（WinRT toast），依赖可接受就接线，否则保持诚实静默。
  - 发布产物打包全部插件 exe；`default_plugins_dir`（aether-host）目前只有
    Linux/相对路径候选，需加 Windows 安装目录候选。
  - 本机无法交叉验证 Windows 编译（ring/rusqlite 需要 MSVC/mingw 工具链），
    Windows 代码靠 CI 的 windows job check + transport 测试把关。

### 4.2 RPC over TCP（远程控制）

- **现状**：仅本地 socket；高级页 RPC 区已移除（当时无机制）。`Request/
  Response` 协议（proto.rs）与全部 handler 都是现成的。
- **方案**：
  - `--rpc-listen <addr:port>` 或 settings `rpc_listen`；默认关闭。
  - 鉴权：settings `rpc_secret`（写入 settings.json，0600 已有保障）；连接后
    首条消息 `{"method":"auth","settings":{"token":"…"}}`，常数时间比较；
    监听非 127.0.0.1 时强制要求 secret，否则拒绝启动。
  - 复用 4.1 的 transport 抽象（TCP 流）；事件订阅同样走该连接。
  - 不做 TLS——文档写明「仅限可信网络或 SSH 隧道」。
- **验收**：TCP 回环集成测试（正确 token 通、错 token 拒、无 token 拒）。

### 4.3 浏览器扩展接管（依赖 4.2）

- **现状**：`scripts/install-native-messaging.sh` 装 native-host manifest；
  dashboard 显示是否已安装（`browser_extension_installed()`）；但没有常驻
  通道，扩展无处投递任务。
- **方案**：扩展 → native host → daemon。先 Unix socket 版（权限位即可信），
  4.2 落地后扩展直连 TCP+token。抓取的 `referrer/cookies/headers` 直接进
  `TaskSpec.settings`（http 插件已支持，无需引擎改动）。细节已有
  `docs/BROWSER_INTEGRATION.md`。

### 4.4 媒体转码（ffmpeg 管线）

- **现状**：media 插件是纯 Rust 的 HLS/DASH 下载+解密合并（不用 ffmpeg）；
  rtmp 插件已有 `ffmpeg_path` settings key + `AETHER_FFMPEG` env；
  UI 媒体页是能力说明卡。
- **方案**：
  - media 插件下载完成后按任务级 key `transcode_to`（容器名）spawn ffmpeg
    做 remux（`-c copy`）或转码；进度从 ffmpeg `-progress pipe:1` 解析进
    SharedProgress；失败保留原文件并在日志说明。
  - 全局 `ffmpeg_path` 与 rtmp 的 key 语义对齐（daemon settings → 注入任务
    settings 或 NetworkConfig，选后者需给 media 插件加 Init 读取）。
  - UI 恢复 ffmpeg 路径行（rtmp/media 共用）+ 完成后转码开关（任务级语义要
    如实描述）。
- **验收**：假 ffmpeg 脚本 e2e（记录 argv、退出 0 → 任务完成、退出 1 →
  保留原文件并 failed）。

### 4.5 托盘 / 剪贴板 / i18n

- 托盘：Slint 无内置托盘。Linux 走 StatusNotifier（dbus，无重依赖）；
  「关闭到托盘」的语义其实很轻——下载由 daemon 常驻负责，UI 关闭即可，
  托盘主要为快速唤起。优先级低于 4.1-4.4。
- 剪贴板监听：`arboard` + 轮询线程 + 「检测到链接」通知动作。
- i18n：全仓文案硬编码中文。Slint 侧 `tr()` + 资源文件、Rust 侧字符串表；
  建议先英文后多语言，且放在 UI 大改之前做（改动面大）。

### 4.6 插件安装/更新接 daemon + manifest 签名

- **现状**：插件页「安装」「更新全部」是诚实 toast；插件从 `plugins_dir`
  扫描注册（SQLite 持久化启用状态）。自动更新通道（`aether_update` +
  `SIGNING.md` 的签名体系）已存在但只覆盖主程序。
- **方案**：daemon `install_plugin(path)`（校验 manifest 签名 → 复制进
  plugins_dir → 注册表刷新）+ `update_check`（复用 update_url 清单，插件条目
  带独立签名）；UI 插件页两个按钮接线。签名 key 轮换策略见 `SIGNING.md`。

### 4.7 发布与供应链

- `cargo-deny`（license/advisory/sources）+ SBOM 生成进 release 资产；
  GitHub Actions pin 到 SHA（现 Dependabot 会跟进）。
- `.deb` 升级通道：appstream 元数据 + 每用户 daemon 的自更新提示
  （`update_status` RPC 已有）。
- 小件：任务优先级老化（防高优饿死）、批量导入（URL 列表文件 / 多 magnet）。

## 5. 发布流程摘要

- 发布仅在 `release/*` 分支触发，**分支名必须等于 `Cargo.toml` 版本**
  （如 `release/0.3.0`）；完整流程见 `RELEASING.md`。
- 更新签名密钥在 `~/.ssh/aether-update-signing.key/.pub`——**绝不入库、
  不贴日志**。
- 教训（`WITHDRAWN_RELEASES.md`）：发布资产缺第三方许可文件会立即撤回；
  `scripts/check-license-bundles.py` 是门禁，别绕过。
