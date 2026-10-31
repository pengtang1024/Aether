# 下载链路硬化设计（9.75 → 10）

> 状态：**Phase A/B/C 已实施；Phase C 补了 Windows 卷信息缺失时的 centralized 回退、Nightly soak workflow（aria2 HTTP/WS + BT serial）；真实 Windows 跨卷硬件环境仍不可在 CI 模拟，Phase D 可选**
> 当前基线：下载正确性 9.8 / 可靠性 9.7 / 性能 9.5 / 可维护性 8.8，综合约 **9.75/10**
> 目标：把剩余的架构、跨平台和长期运行风险收敛到可验收状态。

## 1. 目标与非目标

### 目标

1. 统一 aria2 service bridge 与下载插件的生命周期管理，消除两套 spawn/stop/health 逻辑。
2. 把 daemon 已经产出的 `task_detail`（BT peers、ratio、seeding、HTTP 细节）接入任务抽屉。
3. 给跨盘 staging 加运行时回退，降低 `volume_serial_number` 误判和权限问题的影响。
4. 增加可重复的 soak / 故障注入测试，覆盖长时间轮询、并发 WS、进程崩溃和恢复。
5. 明确 librqbit 能力边界，不在 Host 侧伪造 BT 能力。

### 非目标

- 不在本轮实现 BT 逐任务限速：librqbit 只有会话级 rate limit。
- 不实现 BT web seed：librqbit 不提供该能力。
- 不重写 `getPeers` 的 peerId/实时速度/seeder：公开 snapshot 不含这些字段。
- 不把下载引擎换成 aria2；兼容层只做协议适配。
- 不在本轮实现下载中镜像 failover（见 Phase D，单独评估）。

## 2. 现状与问题

### 2.1 Service bridge 生命周期是独立实现

- `crates/aether-daemon/src/aria2.rs`：
  - `PluginBridge` 自己持有 `PluginProcess`、`Mutex`、`listen`、重启逻辑。
- `crates/aether-daemon/src/server.rs`：
  - `spawn_aria2_bridge` / `stop_aria2_bridge` / `restart_aria2_bridge` 自成一套。
  - `set_plugin_enabled`、`rescan_registry`、`install_package` 分别调用这套逻辑。
- `crates/aether-host/src/manager.rs`：
  - `PluginManager` 只管理下载插件，不知道 service 插件。
- 结果：
  - 崩溃恢复、连接上限、重启策略在 bridge 内实现，manager 无感知。
  - 插件页没有 service 的 running/last_error 状态。
  - 未来新增第二个 service 插件（例如 aria2 的另一种协议、同步桥）会复制整套逻辑。

### 2.2 `task_detail` 已产出但 UI 未消费

- BT 插件在下载/做种循环周期性发送 `PluginEvent::Detail`：
  - `seeding`、`ratio`、`uploaded_bytes`、`seed_seconds`、`peers`。
- daemon `handle_event` 已缓存到 `State.details` 并广播 `task_detail`。
- `src/backend.rs::apply_event` 没有处理 `task_detail`；`Snapshot` 没有 detail 字段。
- 结果：AriaNg 能通过 `getPeers` 看到 peers，但 Aether 自己的任务抽屉看不到 BT 做种/peer 信息。

### 2.3 跨盘 staging 依赖平台元数据

- `same_filesystem`：
  - Unix：`MetadataExt::dev()`。
  - Windows：`MetadataExt::volume_serial_number()`。
- 已经实现：
  - 同盘 centralized staging；
  - 跨盘目标 `.aether-staging`；
  - 启动清理、完成/删除清理、失败保留。
- 缺口：
  - Windows 未实测；`volume_serial_number` 失败/未知时默认 centralized，可能出现跨盘复制或目标 staging 不可写。
  - 目标盘 staging 创建失败时没有回退到 centralized。

### 2.4 缺少长期运行验证

- 现有 E2E 覆盖：HTTP/WS、addTorrent/addMetalink、checksum、pause、changePosition、session、kill -9 插件、keep-alive。
- 缺少：
  - 多 WS 客户端 + 高频 HTTP 轮询 + 大历史库的 soak；
  - daemon 重启中途恢复的真实场景；
  - 磁盘写满/目标盘只读的故障注入。

## 3. Phase A：ServiceSupervisor（架构）

### 3.1 目标

用**一个共享的 supervisor** 管理所有 `kind: "service"` 插件，替代 `PluginBridge` + `spawn/stop/restart_aria2_bridge`。

### 3.2 模块与结构

新增 `crates/aether-daemon/src/service.rs`：

```rust
pub struct ServiceSupervisor {
    socket: PathBuf,
    data_dir: PathBuf,
    host_version: String,
    listen: Mutex<Option<String>>,
    services: Mutex<HashMap<String, Arc<Mutex<ServiceSlot>>>>,
}

struct ServiceSlot {
    service: String,
    manifest: PluginManifest,
    executable: PathBuf,
    process: Option<PluginProcess>,
    status: ServiceStatus,
    last_error: Option<String>,
    starts: u32,
}

#[derive(Clone, Debug)]
pub enum ServiceStatus {
    Stopped,
    Running,
    Error(String),
}
```

- 外层 `Mutex<HashMap>` 只做查找，内层 `Mutex<ServiceSlot>` 保证一个慢 service 不阻塞另一个。
- `listen` 是 daemon 绑定 RPC 后交给插件的 `ip:port`。

### 3.3 方法

```rust
impl ServiceSupervisor {
    pub fn new(socket: PathBuf, data_dir: PathBuf, host_version: String) -> Arc<Self>;
    pub fn set_listen(&self, listen: Option<String>);
    /// 启动所有 enabled 的 service 插件；单个失败只记录状态，不影响 daemon。
    pub fn start_enabled(&self, registry: &PluginRegistry);
    pub fn call(&self, service: &str, payload: Value) -> Result<Value, String>;
    pub fn stop(&self, service: &str);
    pub fn restart(&self, service: &str);
    pub fn stop_all(&self);
    pub fn statuses(&self) -> Vec<ServiceStatusView>;
}
```

- `start_enabled`：遍历 `entry.manifest.kind == Service && entry.enabled`，spawn `PluginProcess`，发送 `ServiceInit { socket }` 和 `ServiceAddress { listen }`，状态置 Running。
- `call`：
  1. 查找 slot；
  2. 如果 process 为 None 或 `has_exited()`，spawn（保留 `starts` 计数）；
  3. 发送 `BridgeCall`，等待 `BridgeResult`；
  4. 失败时记录 `last_error`，但不自动无限重启（下一次调用再试）。
- `restart`：stop + spawn。
- `statuses`：给 `plugins` RPC / 插件页使用。

### 3.4 调用路径

- `aria2.rs` 保留 `Bridge` trait，新增适配器：

```rust
struct SupervisorBridge {
    supervisor: Arc<ServiceSupervisor>,
    service: String,
}

impl Bridge for SupervisorBridge {
    fn call(&self, payload: Value) -> Result<Value, String> {
        self.supervisor.call(&self.service, payload)
    }
}
```

- `serve_http` 继续接收 `Option<&dyn Bridge>`；listener 启动时用 `SupervisorBridge`。
- `State` 把 `aria2: Arc<Mutex<Option<Arc<PluginBridge>>>>` 替换为 `services: Arc<ServiceSupervisor>`。
- `set_plugin_enabled` / `rescan` / `install` / `uninstall`：
  - 根据 manifest `kind == Service` 调用 `supervisor.restart/stop`，不再有 aria2 专用分支。

### 3.5 迁移步骤

1. 新建 `service.rs`，先把 `PluginBridge` 逻辑平移，保持 `serve_http` E2E 通过。
2. 替换 `State.aria2` 为 `services`；`spawn_aria2_bridge`/`stop`/`restart` 删除或改成 supervisor 包装。
3. `set_plugin_enabled`、`rescan_registry`、`install_package` 改为 supervisor 统一处理。
4. `plugins` RPC 对 service 条目附加 `running` / `last_error`。
5. 更新 `docs/PLUGIN_ARCHITECTURE.md` 的 service 生命周期说明。

### 3.6 测试

- 现有 10 个 aria2 E2E 全部继续通过。
- 新增：
  - service 禁用 → HTTP 返回兼容错误 → 启用 → 恢复；
  - kill 服务进程 → 下一次请求自动重启；
  - `plugins` RPC 能读到 service 的 running/error；
  - 两个 service 同时运行互不阻塞（用一个假 service 插件或延迟测试）。

### 3.7 实施记录

- 新增 `crates/aether-daemon/src/service.rs`：`ServiceSupervisor`、`ServiceBridge`、`ServiceStatus`。
- `State.aria2` 已替换为 `State.services: Arc<ServiceSupervisor>`。
- 删除 `spawn_aria2_bridge` / `stop_aria2_bridge` / `restart_aria2_bridge`；
  启停/重装/回滚统一走 `services.sync(registry)`。
- `plugins` RPC 新增 `running` / `last_error`，UI 可感知 service 状态。
- 新增 E2E：service 启停状态、kill -9 服务进程后下一次请求自动重启。
- 已知限制：`sync` 遇到正在处理请求的 slot 会跳过，下一次 sync 再更新；这是为了避免
  与插件回调 daemon 主循环形成死锁。

### 3.8 风险与回滚

- 风险：锁顺序、重新引入 service 调用阻塞主循环。
- 约束：`call` 只在 HTTP 线程调用；主循环只做 start/stop/restart，不做同步 bridge 调用。
- 回滚：保留 `PluginBridge` 到 Phase A E2E 全绿后再删除；出现回归直接切回旧路径。

## 4. Phase B：UI task_detail

### 4.1 后端

- `src/backend.rs::apply_event` 增加：

```rust
"task_detail" => {
    if let Some(detail) = event.get("detail") {
        self.details.insert(task_id, detail.clone());
    }
}
```

- `BackendState` / `Snapshot` 增加 `details: HashMap<String, serde_json::Value>`。
- 在 `completed` / `failed` / `task_deleted` 时移除对应 detail，避免无限增长。

### 4.2 模型（实施记录）

Phase B 已落地，实际实现比草案更轻：没有单独引入 `TaskDetailExtra`，而是在
`refresh_detail` 里直接读 `Snapshot.details[i32]` 并拼成一条 `bt-text`。

- `Snapshot.details`：`HashMap<i32, serde_json::Value>`，在 `publish` 时按 UI id 映射。
- `ui/model.slint::TaskDetail` 新增 `torrent: bool`、`bt-text: string`。
- `refresh_detail` 读取：
  - `seeding`：`detail.seeding == true`
  - `ratio`：`format!("{:.2}", ratio)`
  - `uploaded`：`format_size(uploaded as f64)`
  - `seed_seconds`：`format_eta(Some(seed_seconds as f64))`
  - `peers`：`peers.as_array().len()`
- 任务抽屉新增一行 `BT 状态`，仅 `torrent == true` 时显示。

### 4.3 Slint（实施记录）

- `ui/model.slint::TaskDetail` 增加 `torrent: bool`、`bt-text: string`。
- `ui/pages/task-drawer.slint` 在分片/信息区之间增加一行：
  `if root.detail.torrent: Text { text: @tr("BT 状态  ") + root.detail.bt-text; ... }`
- 非 BT 任务不显示；HTTP 任务保持现状。
- 翻译已补 en/ja/zh-TW 的 `BT 状态` 条目。

### 4.4 测试

- backend 单测：注入 `task_detail` 事件，断言 Snapshot 里的 detail。
- Slint 编译检查 + 截图（如果有截图基线）。
- 手动/soak：BT 做种任务在 1 秒内显示 ratio/peers。

## 5. Phase C：跨盘回退与 soak

### 5.1 staging 回退

- 新增 `create_staging_root`：

```rust
fn create_staging_root(state, task_id, final_dir) -> Result<PathBuf, DaemonError> {
    let preferred = task_staging_root(state, task_id, final_dir);
    if fs::create_dir_all(preferred.join("staging")).is_ok() {
        return Ok(preferred);
    }
    // volume detection was wrong, target is read-only, etc.
    let fallback = state.download_dir.join(".aether").join(task_id);
    fs::create_dir_all(fallback.join("staging"))?;
    Ok(fallback)
}
```

- 调用点：`add_task`、`start_from_record`；保留 `existing_staging_root` 优先复用。
- 失败回退写日志，不打断 add。

### 5.2 Windows 验证

- 在 CI 增加 Windows 单元测试：
  - 同一临时卷 `same_filesystem == true`；
  - 权限拒绝/路径不存在时返回安全默认；
  - 跨卷无法在 CI 模拟，至少覆盖元数据失败分支。
- 如果 `volume_serial_number` 不可用，回退到 centralized staging + 日志。

### 5.3 Soak / 故障注入

新增 `crates/aether-daemon/tests/soak.rs`（`#[ignore]`，nightly）：

- 10 个 WS 客户端，每秒 `tellActive`；
- 1 条 HTTP keep-alive 连接，每秒 `getGlobalStat`；
- 100 个 fake 下载任务（短 duration），反复 add/pause/resume/remove；
- 1 个 BT 任务（可选，使用本地种子）；
- 运行 10–30 分钟，断言：
  - 没有任务卡在 running 超过 N 秒；
  - 没有 service 重启风暴；
  - 进程线程数/内存有界。
- 故障注入：
  - 已在 Linux 覆盖 `kill -9` 插件；
  - 增加 daemon 重启中途恢复（复用现有 `spawn_daemon_at`）；
  - 目标盘只读/磁盘满（Linux tmpfs 或 mock）：已加 finalize 失败不销毁
    staging 的回归测试、`ensure_space` 短缺/探测失败单测，以及
    `create_dir_with_fallback` 在 preferred 路径被文件占用时回退 centralized
    的单测。
- CI：nightly job 跑 `--ignored`，PR 只跑普通 E2E。

## 6. Phase D（可选）：下载中镜像 failover

> 只有在真实用户需求出现时再做，默认不进入主线。

- ResumeState 记录每个 range 的 `source_index`。
- 规则：
  - `done == 0` 的 range 可切换镜像；
  - `done > 0` 时必须 ETag + total 一致，否则不切换；
  - 切换只影响该 range，不重置整个任务；
  - 全部镜像失败才返回错误。
- 测试：双本地服务器，主源中途断流，镜像同 ETag；断言续传成功。
- 风险：不同源的 Range 语义、ETag 缺失、文件内容不一致；没有校验时宁可不切换。

## 7. 兼容与发布

- `API_VERSION = 2` 已经是 breaking change：发布前必须重新构建/签名全部插件包；旧 `api=1` 包会被拒绝。
- ServiceSupervisor 不改变 IPC 协议，只改变 Host 内部生命周期；插件无需改代码。
- UI detail 是纯增量，旧 daemon/新 UI 或新 daemon/旧 UI 都不应崩溃（detail 缺失时隐藏区块）。
- 文档同步：
  - `docs/PLUGIN_ARCHITECTURE.md`（service supervisor）
  - `docs/DAEMON.md`（service 状态/重启）
  - `docs/SUPPORT_MATRIX.md`（BT detail/限制）
  - `CHANGELOG.md`

## 8. 验收清单

- [x] Phase A：ServiceSupervisor 替换 PluginBridge；aria2 E2E 全绿；kill 服务进程自动恢复；service 状态可查。
- [ ] Phase B：BT 任务抽屉显示 ratio/uploaded/seed time/peers；非 BT 不显示；backend 单测通过。
- [x] Phase C：目标盘 staging 创建失败自动回退 centralized；Windows
  `volume_serial_number` 缺失时回退 centralized；并发 HTTP 测试通过；
  `soak_http_clients`（`#[ignore]`，`AETHER_SOAK_SECS`）可重复跑；
  `.github/workflows/soak.yml` 每日跑 aria2 HTTP/WS soak 与 BT 串行契约测试。
  真实 Windows 跨卷硬件仍无法在 CI 模拟（已用反射/回退逻辑覆盖）。
- [ ] Phase D（可选）：主源中途失败可切换到同 ETag 镜像，不影响已下载分片。
- [ ] 全 workspace check、fmt、clippy 通过；CHANGELOG/文档同步。
