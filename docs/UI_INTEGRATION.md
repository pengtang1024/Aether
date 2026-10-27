# Slint UI 接 daemon 的契约

> 目标：GUI 不直接持有插件进程/SQLite；只作为 daemon 的本地客户端。
> 这样 UI 重启不打断下载，daemon 也可以被浏览器/CLI 共用。

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
```

推荐模式：

1. 后台线程阻塞 `subscription.recv_timeout(...)`，把事件发到 `std::sync::mpsc`。
2. Slint 用 `slint::Timer` 每 100–250 ms 取队列，更新 `VecModel<TaskRow>`。
3. 不要在 Slint UI 线程里做 socket/磁盘/插件调用。

## 4. 操作

```rust
client.add_task(url, None, None, json!({}))?; // -> AddResult { task_id, plugin_id }
client.pause(task_id)?;   // running/queued -> paused
client.resume(task_id)?;  // paused -> 原地恢复或重新启动；failed/canceled -> 重新排队
                          // 并发已满时返回 state: "queued"，稍后自动提升
client.cancel(task_id)?;  // -> canceled（终态，可再 resume 重新下载）
client.task_record(task_id)?;
```

`add_task` 会 Probe 后选择插件；想强制插件用 `plugin: Some("http")`。
浏览器传过来的 `referrer`/`headers`/`cookies` 在 `settings` 里，daemon 负责持久化/脱敏策略。

## 5. 错误/状态

- `daemon` 不在：连接错误；UI 显示“后台服务未启动”。
- 插件失败：`failed` 事件 + `StoredTask.error_message`；可提供“重试”。
- 升级：`update_status` 显示 `ready`，由 daemon/包管理器处理；UI 只做提示。
- 不要把 socket 暴露到 TCP；不要接受非本用户连接。

## 6. 迁移步骤

1. UI 启动创建 `Client`，读取 `list_tasks` 填充列表。
2. 新建/暂停/恢复/删除改为调用 daemon。
3. 订阅事件替换 `src/engine.rs` 的模拟进度。
4. 保留 `engine.rs` 作为离线演示 fallback，或删除。
5. 打包时确保 `aether-daemon` + 插件目录存在。
