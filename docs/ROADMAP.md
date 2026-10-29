# 路线图与会话交接

> 面向后续会话/贡献者：现在在哪、下一步做什么、怎么做、别踩哪些坑。
> 最后更新：2026-10-29（main @ `bfd5ae1`）。历史盲区清单见 `GAP_AUDIT.md`，
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

- **设置页现状**：10 个分类中每个控件要么真实生效、要么是诚实的能力说明卡，
  没有「点了没反应」或显示假值的控件。
- **质量门禁**：`cargo fmt --all --check` + 全仓 86 套件 + CI 三平台
  （Linux 全量测试，Windows 测 transport + rpc 回环 + 全量 check，macOS
  check）。macOS runner 偶发 cancel——`gh run rerun <id> --failed` 重跑即可，
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
- **剩余**：
  - Windows 桌面通知：`send_notification`（src/main.rs）在 Windows 静默。
    优先 `notify-rust`（WinRT toast），依赖可接受就接线，否则保持诚实静默。
  - 发布产物打包全部插件 exe；`default_plugins_dir`（aether-host）目前只有
    Linux/相对路径候选，需加 Windows 安装目录候选。
  - 本机无法交叉验证 Windows 编译（ring/rusqlite 需要 MSVC/mingw 工具链），
    Windows 代码靠 CI 的 windows job check + transport 测试把关。

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
  （app 实际退出）。macOS/Windows 托盘未做（Linux 第一）。
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
- **i18n 未做（用户已定方向：中文优先）**：源串保持中文作为第一语言，
  走 Slint `@tr()` + gettext .po（zh 恒等/en 翻译）+ `select_bundled_translation`
  运行时切换 + UiPrefs.lang + Rust 字符串表；全仓文案改动面大，单独成波。

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

## 5. 发布流程摘要

- 发布仅在 `release/*` 分支触发，**分支名必须等于 `Cargo.toml` 版本**
  （如 `release/0.3.0`）；完整流程见 `RELEASING.md`。
- 更新签名密钥在 `~/.ssh/aether-update-signing.key/.pub`——**绝不入库、
  不贴日志**。
- 教训（`WITHDRAWN_RELEASES.md`）：发布资产缺第三方许可文件会立即撤回；
  `scripts/check-license-bundles.py` 是门禁，别绕过。
