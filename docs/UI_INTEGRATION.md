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

daemon 应由 systemd user unit 启动（`systemctl --user enable --now aether-daemon`）。
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
{"event":"settings_changed","settings":{"proxy":{...}}}
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

// 设置页（代理）
client.settings()?; // {"proxy": null} 或 {"proxy":{"mode":"system|none|custom",…}}
client.set_settings(json!({
    "proxy": {"mode": "custom", "url": "socks5://127.0.0.1:1080"}
}))?; // 部分更新；{"proxy": null} 表示恢复默认；校验失败返回错误

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
