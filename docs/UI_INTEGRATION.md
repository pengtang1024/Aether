# Slint UI 接 daemon 的契约

> 目标：GUI 不直接持有插件进程/SQLite；只作为 daemon 的本地客户端。
> 这样 UI 重启不打断下载，daemon 也可以被浏览器/CLI 共用。
>
> 状态：已接入（`src/backend.rs`）。列表/计数/速度/ETA、暂停/继续、取消+删除、新建
> 下载、代理设置、仪表盘统计（`stats`）与插件开关（`plugins`/`set_plugin_enabled`）
> 均已验证；DNS/IP/UA 等设置字段仍是占位。

## 1. 连接

```rust
use aether_daemon::Client;

let socket = aether_daemon::client::default_socket();
let mut client = Client::connect(&socket)?;
```

> `socket` 在 Unix 是 socket 文件路径，在 Windows 是每用户命名管道名
> （`\\.\pipe\aether-<用户名>`）；行协议两平台一致。

daemon 应由 systemd user unit 启动（`systemctl --user enable --now aether-daemon`）。
Windows 上没有 unit：由 UI/CLI 首次连接时自动拉起 `aether-daemon.exe`。
UI 启动时如果连接失败，可以提示“后台服务未启动”，或执行 `aether-daemon`。

## 2. 首屏

```rust
let tasks = client.list_tasks()?;      // Vec<aether_host::StoredTask>
let update = client.update_status()?;  // JSON: disabled/up_to_date/ready/error
```

`StoredTask` 字段直接映射 Slint：

| StoredTask | Slint |
| --- | --- |
| `id` | 任务主键 |
| `url` / `filename` | 标题/副标题 |
| `plugin_id` | 协议标签 |
| `state` | queued/running/paused/completed/failed/canceled |
| `downloaded` / `total` | 进度条、大小 |
| `speed_bps` | 速度 |
| `error_*` | 错误提示/重试按钮 |
| `files` | 文件列表 |
| `created_at` / `updated_at` | 排序/时间 |

`TaskState` 已经 `serde` 序列化；不要自己解析字符串。

## 3. 实时更新

```rust
let subscription = Client::connect(&socket)?.subscribe()?;
```

事件（一行一个 JSON）：

```json
{"event":"task_added","task":{...}}
{"event":"task_state","task_id":"...","state":"paused"}
{"event":"progress","task_id":"...","downloaded":123,"total":456,"speed_bps":789}
{"event":"completed","task_id":"...","files":[...]}
{"event":"failed","task_id":"...","error":{...}}
{"event":"task_deleted","task_id":"..."}
{"event":"settings_changed","settings":{"proxy":{...},"user_agent":null,"ip_mode":null,"dns":null}}
{"event":"retry_scheduled","task_id":"...","attempt":2,"delay_secs":60}
```

完成后文件直接出现在下载目录（不在 `.aether/` 元数据树里）；同名冲突自动
重命名为 `name-1.ext`。

推荐模式：

1. 后台线程阻塞 `subscription.recv_timeout(...)`，把事件发到 `std::sync::mpsc`。
2. Slint 用 `slint::Timer` 每 100–250 ms 取队列，更新 `VecModel<TaskRow>`。
3. 不要在 Slint UI 线程里做 socket/磁盘/插件调用。

## 4. 操作（`aether-cli remote` 是同一套 API 的终端入口）

```rust
client.add_task(url, None, None, json!({}))?; // -> AddResult { task_id, plugin_id, queued, dir }
// “另存为”：传目标目录（绝对路径，或相对下载根目录的路径）
client.add_task(url, Some(Path::new("/home/me/Videos")), None, json!({}))?;
// 队列优先级：越大越先；同优先级 FIFO
client.add_task_with_priority(url, None, None, 10, json!({}))?;
client.pause(task_id)?;   // running/queued -> paused
client.resume(task_id)?;  // paused -> 原地恢复或重新启动；failed/canceled -> 重新排队
                          // 并发已满时返回 state: "queued"，稍后自动提升
client.cancel(task_id)?;  // -> canceled（终态，可再 resume 重新下载）
client.delete(task_id)?;  // 仅终态可删；清记录/staging，已下载文件保留
client.task_record(task_id)?;
client.logs(task_id, 200)?; // Vec<TaskLog> { level, message, created_at, fields? }，旧→新
// 任务列表里每项都有 priority（整数，默认 0）；可据此显示“高优先级”标签或自排序。

// 设置页（网络）
client.settings()?; // {"proxy": …, "user_agent": …, "ip_mode": …, "dns": …}
//   缺席的 key = 该设置未启用（None）。字段：
//   proxy      null | {"mode":"system|none|custom|pac", …}
//   user_agent 自定义 UA 字符串；未设置时插件发内置 Aether/<version>
//   ip_mode    "ipv4_only" | "ipv6_only"；未设置 = 双栈按系统顺序
//   dns        逗号/空格分隔的服务器列表字符串；未设置 = 系统 DNS
client.set_settings(json!({
    "proxy": {"mode": "custom", "url": "socks5://127.0.0.1:1080"}
}))?; // 部分更新；{"proxy": null} 表示恢复默认；校验失败返回错误

// 网络三键同样是部分更新：key 存在才改，null 清空，空白串等价于清空。
client.set_settings(json!({
    "user_agent": "MyClient/1.0",
    "ip_mode": "ipv6_only",        // "ipv4_only" | "ipv6_only" | null
    "dns": "223.5.5.5, 1.1.1.1"    // 非法地址列表会被整体拒绝（返回错误）
}))?;
// 语义（诚实 UI 必读）：
// - dns 指定后「仅用这些服务器」，解析失败即报错，不回退系统 DNS；
// - dns/ip_mode/user_agent 只作用于 HTTP 族插件（http/metalink/webdav/
//   pan/cloud/ipfs/thunder/media）；FTP 与 BT 不覆盖；
// - 网络配置实际变化时空闲插件进程会被重启（运行中任务保持原进程），
//   UI 提示「空闲插件重启后生效」即对应此行为；
// - 每次 set_settings 成功后广播 settings_changed 事件（完整 settings
//   对象），UI 应以事件值回填控件而不是本地猜测归一化结果。

// 调度上限同样是部分更新的键，但必须是具体数字（`null` 会被拒绝）：
client.set_settings(json!({
    "max_concurrent": 5,            // ≥1；上调立即提升排队任务，下调只挡新任务
    "max_speed_bps": 2048 * 1024,    // 全局限速（字节/秒），0 = 关闭；随任务启动生效
    "retry_max": 3,                  // 0 = 不自动重试；下次失败判定时生效
    "connections": 8                 // 1..=16，仅 HTTP(S)；新任务生效
}))?;
// get_settings 回报的是解析后的生效值（文件没写过的键 = 启动配置 AETHER_MAX_*），
// 所以界面显示的就是 daemon 实际在用的值；未知 key、0 并发、越界连接数都会
// 返回错误，旧设置原样保留。

// 请求头与传输插件旋钮同样是部分更新，null 清空：
client.set_settings(json!({
    "default_headers": "Referer: https://example.com; Origin: https://example.com",
    //   换行或分号分隔的 "Key: Value"；非法格式整体拒绝。HTTP 族插件的每个
    //   请求都带上，任务级同名头优先。仅 HTTP 族插件生效。
    "ftp_passive": true,            // false = 主动模式；空闲 FTP 插件重启后生效
    "bt_listen_port": 6881,         // 1..=65535；空闲 BT 插件重启后生效
    "bt_dht": true,                 // false = 关 DHT
    "bt_upload_bps": 1024 * 512,    // 上传限速（字节/秒），0 = 不限
    "bt_blocklist": "/path/to/list.dat", // dat 格式路径或 http(s)/file URL
    "bt_trackers": "udp://t1:80/announce; udp://t2:80/announce" // 附加到所有种子
}))?;
// BT 旋钮在插件会话创建时读取：运行中的种子保持原配置，空闲插件重启后
// 对新任务生效（daemon 在这些键变化时停掉空闲插件进程）。

// 计划窗口（本机时间 HH:MM；结束早于开始 = 跨午夜）。启用需要两个时间。
client.set_settings(json!({
    "schedule_on": true,
    "schedule_start": "01:00",
    "schedule_end": "07:00"
}))?;
// 时段外：新任务不启动（try_launch 被门控），运行/排队任务被「paused by
// schedule」暂停；窗口开启时只有调度器暂停过的任务被恢复（用户手动暂停
// 的不受影响）。窗口判断每次 set_settings 立即生效，另有 15s 周期 tick。
// 时间格式非法、或启用时缺时间，返回错误且旧设置保持不变。

// PAC：url 或 script 二选一
client.set_settings(json!({"proxy": {"mode": "pac", "url": "http://wpad/wpad.dat"}}))?;
// “测试这个 URL 会走哪个代理”
let resolved = client.resolve_proxy("https://example.com/file.zip")?;
// {"proxy": {"mode":"custom","url":"http://p:3128"} | {"mode":"none"} | null,
//  "pac": true, "pac_error": null}

```

`add_task` 会 Probe 后选择插件；想强制插件用 `plugin: Some("http")`。
浏览器传过来的 `referrer`/`headers`/`cookies` 在 `settings` 里，daemon 负责持久化/脱敏策略。

## 5. 错误/状态

- `daemon` 不在：连接错误；UI 显示“后台服务未启动”。
- 插件失败：`failed` 事件 + `StoredTask.error_message`；可提供“重试”。
  自动重试中：`retry_scheduled` 事件，`StoredTask.retry_at` 为下次尝试时间
  （epoch ms，NULL 表示没有待执行的重试），`retry_attempts` 为已失败次数；
  列表页可显示“N 秒后重试”。
- 升级：`update_status` 显示 `ready`，由 daemon/包管理器处理；UI 只做提示。
- 代理设置变更不影响已经在跑的任务（插件进程持有启动时的配置）；设置页保存后
  提示“对之后开始的任务生效”。
- “跟随系统”= `system`：环境变量优先，其次是 GNOME/KDE 的代理设置（含桌面里
  配置的 PAC URL）。UI 不需要自己读 gsettings；用 `resolve_proxy` 展示实际生效值。
- 不要把 socket 暴露到 TCP；不要接受非本用户连接。

## 6. 迁移步骤

1. UI 启动创建 `Client`，读取 `list_tasks` 填充列表。
2. 新建/暂停/恢复/删除改为调用 daemon。
3. 订阅事件替换 `src/engine.rs` 的模拟进度。
4. 保留 `engine.rs` 作为离线演示 fallback，或删除。
5. 打包时确保 `aether-daemon` + 插件目录存在。

## 8. stats（仪表盘统计）

```
→ {"id":1,"method":"stats","settings":{}}
← {"ok":true,"result":{
     "version":"0.2.0","download_dir":"/…","plugins_dir":"/…","data_dir":"/…/aether.db",
     "disk":{"total":N,"free":N},
     "today":{"bytes":N,"files":N},"week":{"bytes":N,"files":N},
     "tasks":{"completed":N,"failed":N,"active":N,"queued":N},
     "plugins":{"total":N,"enabled":N}}}
```

- `today` 按本地时区的当日 00:00 起算（daemon 侧用 chrono Local）；`week` 为近 7 天。
- 数据库无记录时各项为 0。客户端 `Client::stats()` 返回扁平化的 `Stats` 结构。

## 9. plugins（注册表与启停）

```
→ {"id":2,"method":"plugins","settings":{}}
← {"ok":true,"result":{"plugins":[{"id":"bt","name":"…","version":"0.1.0",
     "protocols":["bt","magnet"],"capabilities":[…],"enabled":true},…]}}
→ {"id":3,"method":"set_plugin_enabled","plugin":"bt","settings":{"enabled":false}}
← {"ok":true,"result":{"plugin":{"id":"bt","enabled":false}}}
```

- 禁用列表存 SQLite `disabled_plugins` 表（schema v5），重启后仍生效。
- 有运行中任务的插件拒绝停用（报 `plugin X still has running tasks`）；已禁用的
  插件不参与 probe，因此 `add` 会报 `no plugin supports this URL`。
- UI 行 id 即 `plugins` 返回顺序的数组下标（registry 按 id 排序，顺序稳定）。
