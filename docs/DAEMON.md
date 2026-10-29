# 守护进程

> 目标：Aether 常驻后台，持有插件运行时和 SQLite，通过本地 socket 接受
> 浏览器扩展、CLI、桌面 UI 和系统协议处理器的下载请求。

## 1. 组成

```text
aether-daemon
  ├─ PluginManager（插件进程 / 可选 cdylib）
  ├─ TaskStore（SQLite）
  ├─ IPC server（Unix socket / Windows named pipe，见 transport.rs）
  └─ 事件循环：进度/完成/失败/最终落盘
```

- 单实例：socket + `flock` 锁；第二个实例直接失败。
- Windows 上端点是每用户命名管道 `\\.\pipe\aether-<用户名>`，由默认 DACL
  限定当前用户；协议行格式与 Unix 完全一致。
- 默认 socket：
  - `$XDG_RUNTIME_DIR/aether/daemon.sock`
  - 回退 `/tmp/aether-<user>.sock`
  - 可用 `AETHER_SOCKET` 覆盖
- 权限：socket `0600`，只允许同一用户连接。
- 默认下载目录：`aether-daemon --download-dir ~/Downloads`
- 插件目录：`--plugins /usr/lib/aether/plugins`（默认查找顺序同 Host）。

## 2. 协议

一行一个 JSON 请求；响应同样一行 JSON。

```json
{"id":1,"method":"ping"}
{"id":1,"ok":true,"result":{"ok":true,"version":"0.1.0"}}
```

方法：

| method | 参数 | 作用 |
| --- | --- | --- |
| `ping` | — | 存活检查 |
| `probe` | `url` | 返回各插件支持度/优先级 |
| `add` | `url`, `plugin?`, `settings?`, `dir?`, `priority?` | 创建任务、Probe、启动插件；`dir` 为目标目录，`priority` 为队列优先级 |
| `list` | — | 最近 100 个任务 |
| `get` | `task_id` | 单任务 + 文件 |
| `logs` | `task_id`, `settings.limit?` | 任务日志（默认 200 条，上限 1000，旧→新） |
| `pause` / `resume` / `cancel` | `task_id` | 生命周期 |
| `delete` | `task_id` | 删除终态任务（保留已下载文件） |
| `resolve_proxy` | `url` | PAC 诊断：返回该 URL 会用的代理与 `pac_error` |
| `get_settings` | — | 返回 `{"settings":{…}}`，调度上限是解析后的生效值 |
| `set_settings` | `settings.*` | 部分更新设置并落盘，广播 `settings_changed`；未知 key 报错 |
| `shutdown` | — | 停止 daemon |

示例：

```bash
printf '%s\n' '{"id":1,"method":"add","url":"https://example.com/file.bin"}' \
  | socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/aether/daemon.sock
```

Rust 客户端：`aether_daemon::Client`
（`ping/add/list/task/logs/pause/resume/cancel/delete/shutdown`）。

## 3. 启动

```bash
# 前台
aether-daemon --download-dir ~/Downloads --plugins /usr/lib/aether/plugins

# systemd user service（.deb 已安装 unit）
systemctl --user enable --now aether-daemon
journalctl --user -u aether-daemon -f
```

unit 文件：`packaging/aether-daemon.service`，安装到
`/usr/lib/systemd/user/aether-daemon.service`。

### 3.1 无 GUI 控制面（`aether-cli remote`）

不依赖桌面 UI 也能完整驱动 daemon（默认连 `$XDG_RUNTIME_DIR/aether/daemon.sock`，
`--socket` 覆盖）：

```bash
aether-cli remote ping
aether-cli remote add https://example.com/file.zip [--dir ~/Downloads/x] [--plugin http]
aether-cli remote list [--json]
aether-cli remote get <task-id> | logs <task-id> [--limit 50] [--json]
aether-cli remote pause|resume|cancel|rm <task-id>
aether-cli remote settings | set-settings '{"proxy":{"mode":"pac","url":"..."}}'
aether-cli remote resolve-proxy https://example.com/file.zip
aether-cli remote watch        # 每行一条 JSON 事件，Ctrl-C 退出
```

- `add` 的 `--settings` 是插件设置 JSON（如 `{"connections":8}`）。
- `rm` 只接受终态任务，和 UI 的删除语义一致。
- `watch` 输出即 `subscribe` 事件流，便于脚本/排错；不要把它当长期日志。
- 本地模式（`tasks`/`logs`/`resume`）直接读 SQLite、自己拉起插件，适合单次下载；
  `remote` 走常驻 daemon，任务在后台继续，推荐日常使用。

## 4. 恢复与状态

- 启动时 `recoverable_tasks()` 读取 `queued/running/paused` 任务；running/queued
  在并发额度内重新启动，超出的降级为 queued。paused 保持 paused，等用户显式恢复。
- `resume` 语义：paused 且插件执行仍存活 → 原地恢复；failed（可重试）或
  canceled → 从存储记录重新启动（利用 staging 状态续传）；并发已满 → 回到
  queued 等待提升。任务正在取消中时 resume 返回错误。
- HTTP/FTP/HLS 等利用 staging 中的状态文件或文件长度续传。
- 完成事件由 daemon 执行 `finalize_task`，再写入 `task_files`。
- 失败写入错误类别/消息/可重试标记。

## 5. 自动升级与低消耗

- 可选签名更新：`AETHER_UPDATE_URL` + `AETHER_UPDATE_PUBKEY`，daemon 启动 30 秒后
  检查，之后每 6 小时一次；AppImage 可原子替换，`.deb` 只下载不自行提权。详见
  [UPDATE.md](UPDATE.md)。
- 空闲时阻塞在命令 channel（1 秒超时），不轮询插件；下载中 100 ms 轮询。
- 空闲 5 分钟卸载所有插件进程，保留 registry/SQLite，下次按需启动。
- 不做 busy-loop；DHT/插件进程随任务启动/停止。

## 5.1 并发与限速

| 配置 | 环境变量 | 默认 | 说明 |
| --- | --- | --- | --- |
| 并发上限 | `AETHER_MAX_CONCURRENT` | 3 | 超出上限的任务保持 queued，按优先级 + 创建顺序提升 |
| 全局限速 | `AETHER_MAX_SPEED_BPS` | 无 | 均摊到当前运行任务，随任务启动时生效 |
| 自动重试次数 | `AETHER_RETRY_MAX` | 5 | 可重试失败按指数退避自动重试，0 = 关闭 |
| 重试基础间隔 | `AETHER_RETRY_BASE_SECS` | 30 | 退避 = base × 2^(n-1)，即 30s/1min/2min/… |

- 调度上限同时是 `settings.json` 的键（设置页「下载」面板）：
  `max_concurrent`（≥1）、`max_speed_bps`（0 = 不限）、`retry_max`（0 = 不重试）、
  `connections`（1..=16，仅 HTTP(S)，插件默认 4）。**文件值优先于环境变量**——
  环境变量只在文件没写过该键时生效，而任何一次 `set_settings` 落盘都会把当前
  生效值写进文件（与 `AETHER_PROXY` 的既有行为一致）；`get_settings` /
  `settings_changed` 返回的是解析后的生效值，所以界面不会显示一个和实际不符的
  数字。
- 生效时机：并发上调立刻提升排队任务，下调不打断正在跑的（只挡新任务）；
  限速与连接数随任务启动生效，已在跑的任务保持启动时的预算；重试上限在
  下一次失败判定时读取。非法值（0 并发、连接数越界、未知 key、`null`）会被
  `set_settings` 拒绝，旧设置继续生效；启动时的非法文件则打日志后回退到环境
  变量值，不阻止 daemon 启动。

- 队列优先级：`add` 可带 `priority`（整数，越大越先，clamp 到 `-100..=100`，
  默认 0）。提升顺序是 `priority DESC, created_at ASC`（同优先级 FIFO，同毫秒
  插入按写入顺序）。`StoredTask.priority` 会回显，UI 可做排序/标记。
  注意：严格优先级意味着低优先级任务可能被持续插队（无老化机制）；需要时把
  它们调回 0/正数即可。
- 任务级限速优先：`add` 的 settings 里给 `max_speed_bps`（字节/秒，0 = 不限）。
- 限速在 HTTP 插件下载循环内实现（任务级共享 token bucket，多连接同一桶）；
  委托 HTTP 的插件（baidupan/aliyunpan/ipfs/thunder/cloud/webdav）自动继承。
- daemon 在任务完成/失败/取消后立即提升队列，并广播 `task_state` 事件。

## 5.2 自动重试

- 插件报失败且 `retryable=true` 时，daemon 自动安排重试：30 s 起指数退避，
  最多 `AETHER_RETRY_MAX` 次（默认 5），次数耗尽后保持 failed。
- 到点的重试回到 queued，与其它任务一起按 FIFO 提升运行（不插队）。
- 手动 `resume` 会重置自动重试计数；`cancel` 可以随时打断重试循环（对
  failed 任务也允许取消）。
- 每次调度/耗尽都写入 `task_logs` 并广播 `retry_scheduled` 事件
  （`{attempt, delay_secs}`）。
- 重试计划**持久化**在 `tasks.retry_attempts` / `tasks.retry_at`（schema v3）：
  daemon 重启后未到期的重试按剩余时间恢复，已到期的立即回队列。日志会写明
  `auto-retry N/M resumes in K s after restart`。
- 计数语义：`retry_attempts` 是「上次手动操作以来失败的次数」，决定退避档位。
  重试真正开跑时只清 `retry_at`、保留计数，所以一个重试周期不会把自己重置成
  「第一次尝试」；达到上限后保留计数（UI 可显示「已重试 N 次后放弃」），
  `retry_at = NULL` 表示没有待执行的重试。
- 手动 `resume` / `cancel` / 完成都会清零计数并清掉待执行的重试；`resume` 之后
  的再次失败从「第 1 次重试」重新开始。
- 事件 `retry_scheduled` 带 `attempt` 与 `delay_secs`；`StoredTask` 也暴露
  `retry_attempts` / `retry_at`（epoch ms），UI 可做倒计时。

## 5.3 任务日志

daemon 自己写入的日志行（`task_logs` 表，`level` 为 `info`/`warn`/`error`）：

| 时机 | 内容 |
| --- | --- |
| 启动插件执行 | `running with plugin <id>` |
| 并发已满 | `queued: N task(s) already running (max M)` |
| 完成 | `completed: a.bin, b.bin`（`fields.files` 为文件名数组） |
| 插件失败 | 插件给的错误消息（`fields` 含 `kind`/`retryable`） |
| finalize 失败 | `finalize failed: ...` |
| 自动重试 | `auto-retry 2/5 in 60 s`；用尽时 `auto-retry exhausted after 6 attempts: ...` |
| 用户操作 | `paused by user` / `resumed by user` / `canceled by user` |

`logs` 返回 `{"task_id": "...", "logs": [{"level","message","created_at","fields?"}]}`，
按时间升序。任务不存在时返回错误（不返回空数组），已删除任务的日志随记录级联删除。

CLI 对等命令：`aether-cli logs <task-id> --dir <dir> [--limit <n>]`（直接读 SQLite，
时间戳按 UTC 打印）。

## 5.4 删除任务

- `delete` 只接受终态任务（completed/failed/canceled）；运行中任务需先 cancel。
- 删除会清掉 `.aether/<task_id>/`（staging 与续传状态）和数据库记录；已完成的
  文件位于下载目录本体，删除任务不会动它们。
- 广播 `task_deleted` 事件，UI 用来移除列表项。

## 5.5 目标目录（另存为）

- `add` 的 `dir` 决定最终文件落在哪：绝对路径原样使用；相对路径相对下载根目录，
  且不允许含 `..`（防止意外写出下载树）。省略时用 daemon 的 `--download-dir`。
- 目录在 `add` 时就会创建，权限问题立即报错，而不是下载完才失败。
- 非默认目录会写入 `tasks.target_dir`（schema v2），重启/resume 后仍回到原目录。
  默认目录不落库，改 `--download-dir` 时既有任务跟随新根目录。
- `add` 的响带 `dir` 字段，UI 可直接显示保存位置。
- staging 始终在 `.aether/<task_id>/staging`；跨文件系统 finalize 通过复制+删除完成。
- 同名冲突自动 `name-1.ext`，不会覆盖已有文件。
- **空间预检**：`add` 拿到探测大小时先查目标目录可用空间（`statvfs`），不足则
  直接报错、不建任务；同一目录下正在下载的任务按剩余字节预留，避免并发把盘写满。
  预检只是提前失败，下载中仍可能被其他进程写满——此时 HTTP 插件把 `ENOSPC`
  映射为 `ErrorKind::DiskFull` 且标记不可重试，不会烧掉重试次数。
  探测拿不到大小（chunked/直播）时跳过预检。

## 5.6 代理

插件侧的 `Init.proxy` 早就有三种模式，但 daemon 以前从不填它；现在两端接通：

| 模式 | settings JSON | 说明 |
| --- | --- | --- |
| `system` | `{"proxy":{"mode":"system"}}` | 跟随系统：环境变量优先，其次 GNOME/KDE 桌面设置（见下） |
| `none` | `{"proxy":{"mode":"none"}}` | 永不使用代理（显式忽略上面这些环境变量） |
| `custom` | `{"proxy":{"mode":"custom","url":"socks5://127.0.0.1:1080","username":…,"password":…}}` | 只用指定代理；`http`/`https`/`socks5`/`socks5h` |

- `system`（以及未配置时的默认值）会读桌面设置，否则 GNOME 里配了代理、浏览器
  走了代理，Aether 却直连：
  - 只要 `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY`（大小写皆可）任一存在，就交给
    reqwest 处理（`NO_PROXY` 也由它生效），不再覆盖；
  - 否则 `gsettings get org.gnome.system.proxy`（manual → http/https/socks，
    auto → `autoconfig-url` 当 PAC 用）→ KDE `~/.config/kioslaverc`
    （`ProxyType=1` 手动 / `=2` PAC 脚本）；
  - 检测结果缓存 60 秒，`resolve_proxy` 可随时查看实际生效的代理；
  - GNOME “自动”但没填脚本 URL 意味着 WPAD 自动发现，**不做**（见 §5.7 末尾）。
  - 环境变量 `AETHER_GSETTINGS` / `AETHER_KDE_PROXY_CONFIG` 可替换检测源
    （测试/调试用）。
- 检测到的代理按任务下发（`TaskSpec.proxy`），探测同样使用它；手动代理优先
  http → https → socks5。
- 设置持久化在 `<download>/.aether/settings.json`（`0600`，可能含代理密码）。
  坏文件不会阻止 daemon 启动：打日志后按默认值运行。
- `AETHER_PROXY` 可给单次运行覆盖，接受一行规格：
  `system`、`none`、`http://user:pass@host:3128`、`socks5://host:1080`。
  环境变量**不落盘**——不会因为一次 `export` 永久改掉用户设置。
- 生效范围：设置变更只影响之后启动的插件进程；正在跑的任务保持原代理。
  空闲 5 分钟插件会被卸载，所以最迟下次启动就会用新设置。
- `custom` 但缺 `url`、未知 `mode`、非 `host:port` 的 URL 都会被拒绝（错误返回给
  UI，旧设置保持不变）。
- 凭据只存在本机 settings/SQLite 和进程 `Init` 里，不写日志；HTTP 插件只把
  `username/password` 交给 reqwest 的 `Proxy::basic_auth`。

## 5.7 自动代理（PAC）

`mode: "pac"` 时 Host 用 `rquickjs` 求值 `FindProxyForURL(url, host)`：

```json
{"proxy":{"mode":"pac","url":"http://wpad.corp/wpad.dat"}}
{"proxy":{"mode":"pac","script":"function FindProxyForURL(u,h){return h.endsWith('.corp') ? 'PROXY proxy.corp:3128' : 'DIRECT';}"}}
```

- `url`（`http(s)://`、`file://` 或本地路径）与 `script` 二选一。
- 沙箱：16 MiB 堆、300 ms 墙钟、无文件/网络/主环境 API；`dnsResolve` 返回 null、
  `isResolvable` 为 false，脚本无法让 daemon 做任意 DNS。
- 脚本缓存 30 分钟；刷新失败保留旧脚本并记录 `pac_error`，不会静默直连。
- 只取返回链的**第一条**（`PROXY a; DIRECT` → a）；reqwest 不支持代理间自动切换。
- 每任务求值：`add` 时把结果写进 `TaskSpec.proxy`（探测也用同一代理）；重启、重试、
  队列提升时重新求值但不触发网络 I/O（用缓存脚本）。
- 生效范围：HTTP 与 media 插件按任务代理建客户端；其余插件仍用进程级 `Init.proxy`
  （PAC 模式下该值设为 `system`，作为解析失败的回退）。
- `AETHER_PAC_URL` 单次运行覆盖，不落盘；`--pac-url` 同义。
- 诊断：`{"method":"resolve_proxy","url":"https://x"}` →
  `{"proxy":{…}|null,"pac":true,"pac_error":null}`。
- **不做 WPAD 自动发现**：`http://wpad.<domain>/wpad.dat` 与 DHCP 252 都需要
  额外的信任假设（WPAD 劫持是已知攻击面，主流浏览器已在移除），且难以跨平台
  正确实现。需要自动代理时请填 PAC URL/脚本；GNOME/KDE 里已经配好的 PAC URL
  会被 `system` 模式自动采用。

## 6. 接管系统下载还差什么

daemon 是常驻入口，但“接管所有下载”还需要：

1. **浏览器扩展 + Native Messaging**：扩展把下载 URL/Headers 交给 native host，
   native host 再通过本地 socket 调 daemon。见 `BROWSER_INTEGRATION.md`。
2. **系统默认下载器/协议关联**：`aether://`、`magnet:`、`ed2k:`、`thunder:` 的
   desktop MIME/URL handler；由安装器注册。
3. **剪贴板监听/RSS**：Host 功能，默认关闭。
4. **桌面 UI 连接 daemon**：Slint Host 作为 socket 客户端，不再直接持有插件进程。
5. **权限与自动启动策略**：systemd user unit、首次启动授权、单用户 session。
6. **多用户/远程**：当前只支持本机同用户（Unix socket 或 Windows 命名管道）；
   不做局域网 TCP。

## 7. 安全边界

- 不做 localhost TCP，避免网页/局域网访问。
- socket 权限 0600；Windows 管道走默认 DACL（仅同用户）；请求只能由同用户发起。
- `add` 的 URL/settings 会传给第一方插件；插件仍受 Host 最终落盘路径校验。
- cdylib 插件在 Host 进程内运行，daemon 默认只加载 `kind: "process"` 的插件；
  启用 cdylib 需显式配置并接受崩溃风险。
