# Aether 插件化架构方案

> 项目形态：**Aether 是一个管理面板 / 下载管理器 Host，所有下载能力都以插件存在。**
> 插件全部由我们（第一方）开发和签名，不是第三方市场；但它们在进程、版本、权限和
> 资源上彼此独立，以便稳定、增量更新和低资源运行。

## 1. 职责边界

| Host（管理面板） | 插件（下载引擎） |
| --- | --- |
| Slint UI、任务列表、详情、设置 | 协议探测、连接、分段/piece 计划 |
| 任务生命周期、状态机、数据库 | 协议特有的续传状态（opaque blob） |
| 全局调度、优先级、队列 | 单任务并发执行与协议级重试 |
| 全局/per-host/per-task 限速预算 | 按 Host 授予的预算执行限速 |
| 插件发现、安装、签名、启停、更新 | 协议元数据、peers/chunks/logs 事件 |
| 任务目录授权、最终路径校验、原子落盘 | 在授权目录内写数据；不越界 |
| 最终完整性校验/用户可见错误 | 协议级校验（piece/hash） |
| 通知、日志聚合、诊断包 | 插件自己的结构化日志 |

**硬约束：Host 不链接 `reqwest`、`librqbit`、`suppaftp`、`m3u8-rs` 等协议库。**
Host 只认识 `PluginId`、任务模型、预算和事件；新增协议 = 新增插件，不改 Host 核心。

## 2. 进程模型

```
┌──────────────────────── Host 进程 ─────────────────────────┐
│ Slint UI ← EngineEvent ← EventBus                          │
│        │ Command                                           │
│   EngineHandle → Scheduler → PluginSupervisor              │
│                              │ spawn/stdio/IPC             │
└──────────────────────────────┼─────────────────────────────┘
                               │
        ┌──────────────────────┼──────────────────────┐
        ▼                      ▼                      ▼
 aether-plugin-http     aether-plugin-bt       aether-plugin-media
 (一个进程，多任务)      (一个进程，多任务)      (一个进程，多任务)
```

- **一个插件 ID 一个进程**，进程内可并行处理多个任务；不做“每任务一个进程”。
- 插件懒启动：默认只启动 HTTP；BT/媒体/FTP 在首次使用或用户在插件页启用时启动。
- 空闲卸载：插件无活动任务 N 分钟后退出，Host 保留任务状态，下次按需重启。
- 插件崩溃：Host 捕获 EOF/心跳超时，受影响任务进入 `RetryWait`；Supervisor 指数退避
  重启插件；连续崩溃进入 `Quarantined`，管理面板提示。
- 插件进程有内存/CPU/文件句柄配额；超出配额先降级，再终止。
- 第一方插件仍进程隔离：协议库崩溃、内存泄漏或死循环不会拖垮 UI/任务库。

## 3. 插件包与清单

每个插件是一个目录/压缩包：

```
aether-plugin-http/
├── plugin.json
├── bin/
│   ├── aether-plugin-http-linux-x86_64
│   ├── aether-plugin-http-linux-aarch64
│   ├── aether-plugin-http-windows-x86_64.exe
│   ├── aether-plugin-http-windows-arm64.exe
│   └── aether-plugin-http-macos-universal
├── assets/                 # 可选
└── LICENSE
```

`plugin.json` 示例：

```json
{
  "id": "http",
  "name": "HTTP / HTTPS Downloader",
  "version": "0.2.0",
  "api": 1,
  "min_host": "0.2.0",
  "protocols": ["http", "https", "metalink"],
  "capabilities": ["network", "filesystem:task", "http3"],
  "executable": {
    "linux-x86_64": "bin/aether-plugin-http-linux-x86_64",
    "linux-aarch64": "bin/aether-plugin-http-linux-aarch64",
    "windows-x86_64": "bin/aether-plugin-http-windows-x86_64.exe",
    "windows-arm64": "bin/aether-plugin-http-windows-arm64.exe",
    "macos-universal": "bin/aether-plugin-http-macos-universal"
  },
  "settings_schema": { "...": "见第 8 节" },
  "detail_schema": { "...": "见第 9 节" },
  "signature": "base64..."
}
```

- `id` 全局唯一，小写；任务里持久化 `plugin_id` + `plugin_version`。
- `api` 是 IPC/SDK 大版本；Host 只加载 `api` 匹配的插件。
- `min_host`/`max_host` 做 Host 版本约束；升级 Host 时先检查插件兼容性。
- 清单和二进制都有 SHA-256；发布包由第一方 release key 签名；Host 默认只加载签名
  有效的插件。开发模式可显式允许本地未签名插件。
- 插件目录只读；用户安装的可选插件放用户数据目录，Host 校验签名后加载。

## 4. IPC 协议

### 4.1 传输

- Host 通过 `stdin/stdout` 与插件通信；stderr 只写日志。
- 帧格式：`u32 big-endian length` + `MessagePack` 负载，最大 8 MiB。
- 不使用 IPC 传文件字节；插件直接写 Host 授权的任务目录。
- 进度事件 4–10 Hz；Host 再合并到 UI 10 Hz。
- 心跳 5 秒；30 秒无响应视为挂起，Host 终止并重启。
- 请求/响应带 `request_id`；事件带 `task_id` + `seq`；重复消息可幂等处理。

### 4.2 握手

```
Plugin → Host: Hello { api, plugin_id, version, capabilities }
Host → Plugin: Init { host_version, data_dir, log_level, proxy, limits }
Plugin → Host: Ready
```

版本不匹配直接拒绝加载，管理面板显示“需要更新插件/Host”。

### 4.3 消息

Host → Plugin：

| 消息 | 作用 |
| --- | --- |
| `Probe { input }` | 判断是否支持 URL/文件，返回元数据和建议文件名 |
| `Start { task, paths, budget }` | 开始任务；`task.plugin_state` 用于恢复 |
| `Pause/Resume/Cancel { task_id }` | 生命周期控制 |
| `SetBudget { task_id, connections, rate }` | 调整并发/限速 |
| `Snapshot { task_id }` | 主动查询状态 |
| `Ping { request_id }` | 心跳探测，插件回 `Pong` |
| `Shutdown` | 优雅退出 |

Plugin → Host：

| 事件 | 作用 |
| --- | --- |
| `ProbeResult` | 支持度、协议、大小、文件名、建议 settings |
| `Metadata` | 最终 URL、ETag、大小、文件列表 |
| `Progress` | downloaded/total、speed、ETA、完成段数 |
| `Detail` | 协议详情（chunks/peers/metrics），由 schema 描述 |
| `Log` | 结构化日志（level、message、fields） |
| `Completed` | 文件列表、校验和、plugin_state 清理 |
| `Failed` | 错误类别、可重试性、诊断 ID |
| `Heartbeat` | 存活 |
| `Pong` | 对 `Ping` 的响应，带 `request_id` |

## 5. 插件 SDK

`aether-plugin-sdk` 是第一方插件共用的 Rust crate：

- `Plugin` trait / `run_stdio()` 消息循环。
- 类型化命令/事件（与 Host 共享）。
- `TaskContext`：task id、授权目录、settings、代理、预算。
- `NetClient`：统一 HTTP 栈、TLS、代理、HTTP/2/3、Happy Eyeballs。
- `StorageWriter`：positioned write、稀疏文件、fsync、路径守卫、校验。
- `RateLimiter`：Host 预算下的 token bucket。
- `Checksum`：SHA-256/BLAKE3 流式。
- `Logger`：结构化日志，自动脱敏。
- 测试工具：fake Host、内存文件系统、范围 HTTP server。

### 5.1 P0 已实现（`crates/`）

| crate | 内容 |
| --- | --- |
| `aether-plugin-sdk` | `PluginManifest`、MessagePack 帧、`HostCommand`/`PluginEvent`、`Plugin` + `run_stdio`、Host 侧 `PluginProcess` |
| `aether-plugin-fake` | 走真实握手/进度/完成的 fake 插件，用于契约测试和后续 UI 接线 |

- 协议版本 `API_VERSION = 1`；帧上限 8 MiB；长度前缀为 `u32` big-endian。
- 握手：插件 `Hello` → Host `Init` → 插件 `Ready`；id/version/api 不匹配直接拒绝。
- Host 侧 `PluginProcess` 负责 spawn、握手、请求/事件、2 秒优雅退出和强制 kill。
- 运行测试：`cargo test --workspace`；手动跑 fake 插件：`cargo run -p aether-plugin-fake`。

插件可以复用 SDK 的网络/存储/限速，但**协议逻辑仍在插件**；Host 不依赖插件 crate。

## 6. 生命周期与管理

```text
Discovered → Verified → Enabled → Running ⇄ Idle
                    │         │
                    │         ├─ Crashed → Backoff → Running
                    │         └─ Quarantined
                    └─ UpdateAvailable → Updating → Rollback?
```

- 发现：扫描内置插件目录 + 用户插件目录；读取 `plugin.json`。
- 校验：签名、sha256、`api`/Host 版本、平台可执行文件存在。
- 启用/禁用：用户可在插件页禁用不用的协议，Host 不启动、不占内存。
- 更新：签名的增量包；下载到 staging，校验后原子替换；保留上一版回滚。
- 降级/隔离：连续崩溃或心跳失败进入隔离；不影响其他插件/任务。
- 卸载：先停止插件，保留/清理任务数据由用户选择。

## 7. 任务路由

1. 用户输入 URL / 拖入 `.torrent` / 粘贴 magnet。
2. Host 解析 scheme 和内容类型，向已启用插件广播 `Probe`（可并行，带超时）。
3. 插件返回 `supported`、`priority`、`metadata`、`suggested_filename`。
4. Host 按优先级、用户手动选择、插件健康度选一个；创建任务。
5. Host 生成任务目录，发 `Start`；插件返回 `Metadata`/`Progress`。
6. 失败/不支持时允许“用其他插件打开”，不改任务 ID。

`http`/`https` 可能同时被 HTTP、Metalink、Extractor 插件声明；用 file extension、
Content-Type、manifest 探测和优先级消歧。

## 8. 设置 Schema

插件声明设置，Host 渲染，不在插件里写 Slint：

```json
{
  "sections": [{
    "title": "连接",
    "fields": [
      {"key": "connections", "type": "int", "min": 1, "max": 16, "default": 4},
      {"key": "proxy", "type": "select", "options": ["system", "none", "custom"]},
      {"key": "user_agent", "type": "string", "default": "Aether/0.2"}
    ]
  }]
}
```

字段类型：`bool`、`int`、`float`、`string`、`select`、`path`、`secret`、`multiline`。
Host 负责校验、持久化、secret 存 keyring/受限文件；插件只读解析后的值。
插件版本升级时 schema 迁移由 Host 按字段 key 保留旧值。

## 9. 任务详情 Schema

插件不携带 UI 代码；任务抽屉由 Host 按 schema 渲染：

```json
{
  "sections": [
    {"type": "metrics", "items": ["speed", "eta", "size"]},
    {"type": "chunks", "done": 14, "total": 32, "legend": ["done", "active", "pending"]},
    {"type": "peers", "columns": ["address", "speed", "kind"]},
    {"type": "logs", "max_lines": 200}
  ]
}
```

Host 内置 `metrics/chunks/peers/logs/table/text` 渲染器；插件只发数据。这样：
- 所有插件共享同一套 M3 视觉；
- 新协议不需要改 Slint；
- 插件不能注入任意 UI/脚本。

## 10. 调度、预算与限速

- Host 维护全局队列、优先级、aging、活跃任务上限。
- 插件启动时申请 `ConnectionBudget`（连接数、每 host 连接数、上传/下载速率）。
- Host 按全局预算发放/回收 permit；插件不得超发；SDK 的 `RateLimiter` 强制执行。
- 插件报告实际吞吐，Host 汇总总速度；严重超发可降级/暂停插件。
- 暂停/取消通过 IPC + cancellation token；插件必须在 2 秒内确认，否则 Host 终止。

## 11. 存储授权与最终落盘

- Host 为每个任务创建目录：
  ```
  <downloads>/.aether/<task-id>/
  ├── staging/          # 插件唯一可写区
  ├── meta.json
  └── final/            # Host 最终确认后移动
  ```
- 插件只收到 `TaskPaths`，不能访问其他任务/用户目录。
- 插件写入 `staging/`；完成后返回 `FileList { relative_path, size, checksum }`。
- Host 对每个路径 `canonicalize`，拒绝 `..`、绝对路径、symlink、Windows 保留名；
  校验大小/哈希后移动到最终目录，原子 rename。
- 多文件种子：插件返回文件清单；Host 逐文件校验/移动；失败可回滚。
- 大文件不经过 IPC；插件直接 positioned write 到 `staging`。

## 12. 安全与权限

- 第一方插件仍最小权限：`network`、`filesystem:task`、`external-process`、
  `clipboard`（如需要）分别声明。
- 默认只加载签名插件；release key 离线保存；开发模式可显式允许未签名。
- 进程隔离 + OS 级沙箱（后续）：Linux Landlock/seccomp/bubblewrap，Windows Job Object/
  AppContainer，macOS sandbox profile；首版至少进程隔离 + 目录授权。
- 外部工具（ffmpeg/yt-dlp）必须声明 `external-process`，由 Host 提供受控执行器：
  参数数组、超时、输出上限、工作目录，不经过 shell。
- 插件日志/错误统一脱敏 URL 凭据、Cookie、Token。
- 插件安装/更新必须校验签名与 sha256；禁止从任意 URL 静默安装。

## 13. 管理面板

Host 的“插件”页面（后续 UI）展示：

- 已安装/已启用/运行中/隔离中
- 版本、API 兼容性、协议 scheme、资源占用
- 启用/禁用、卸载、更新、回滚
- 插件设置（由 settings schema 生成）
- 最近日志/崩溃次数/诊断包
- 默认插件与可选插件分组
- 当前任务使用的插件徽标/来源

禁用插件后，对应 URL 的“新建下载”会提示安装/启用插件，而不是显示一个假进度条。

## 13.5 Host 级集成（不是下载插件）

- **PAC/自动代理**：Host 网络层统一求值（JS 沙箱、缓存、系统 PAC），插件通过
  `TaskContext.proxy_chain` 接收最终代理链。见 DOWNLOAD_ENGINE.md 11.1。
- **浏览器集成**：扩展 + Native Messaging + 本地 IPC，Host 接管下载/右键发送；
  见 [BROWSER_INTEGRATION.md](BROWSER_INTEGRATION.md)。P5。
- **签名/公证**：**暂缓**；Windows Authenticode、macOS Developer ID + notarytool 的
  方案保留在 [SIGNING.md](SIGNING.md)，Linux 优先，不买证书。

## 14. 打包与分发

- Host 包内置默认插件集；安装包必须包含插件二进制和清单。
- Linux：`.deb`/AppImage 内置 `plugins/`；可选插件装到用户数据目录。
- Windows：安装器把 `plugins\` 放到应用目录；便携 zip 同样包含。
- macOS：`Aether.app/Contents/PlugIns/`；universal 插件二进制。
- 每平台/架构构建插件二进制；CI 矩阵与 Host 相同（Linux x64/arm64、Windows x64/arm64、
  macOS universal）。
- 插件独立版本；应用包可以带一组默认版本，用户可单独更新。
- 第一方插件签名；更新通道与 Host 发布分离；失败可回滚上一版。
- 可选插件（SFTP/WebDAV/S3、ED2K、thunder、IPFS、RTMP、yt-dlp）以独立签名包分发，
  默认不装，保持低资源。

## 15. 低资源策略

- 默认只启动 HTTP 插件；BT/媒体/FTP 首次使用才启动。
- 一个插件进程多任务复用；空闲 5 分钟卸载。
- 插件进程内存/句柄配额；超限先降并发，再重启。
- UI 事件 4–10 Hz；无每字节 IPC。
- 禁用未使用插件不占内存/句柄；插件页可见资源。
- 移动端/嵌入式后续可只带 HTTP 插件子集。

## 16. 测试

- **契约测试**：fake Host ↔ fake plugin；所有消息/错误/超时。
- **IPC fuzz**：截断帧、超大帧、乱序、重复 request_id、未知消息。
- **崩溃恢复**：kill 插件进程/宿主；任务重试、插件重启、状态恢复。
- **版本矩阵**：api 不匹配、min_host、schema 迁移、插件降级。
- **安全**：路径逃逸、签名篡改、未授权 external-process、secret 泄漏。
- **协议一致性**：HTTP Range/ETag、BT piece、FTP REST、HLS key 等。
- **打包**：每个平台安装包必须包含 host + 默认插件；启动后插件可发现。
- **跨平台**：Linux x64/arm64、Windows x64/arm64、macOS universal。

## 17. 路线图

| Phase | 内容 | 验收 |
| --- | --- | --- |
| P0 | Host/PluginSupervisor/SDK/IPC/fake 插件 | SDK/IPC/fake + 契约测试已完成；UI 接线待重设计 |
| P1 | HTTP 插件（单流+分段+续传+校验） | 1 GB 文件 8 连接；kill 后恢复 |
| P2 | PAC/调度/限速 + 插件管理页 | PAC/预算/插件启停生效 |
| P3 | BitTorrent 插件 | magnet/多文件/DHT/做种；进程崩溃不影响 Host |
| P4 | FTP/FTPS、HLS/DASH、Metalink 插件 | 各协议完成/续传/校验 |
| P5 | 浏览器集成 + 可选插件：SFTP/WebDAV/S3、yt-dlp、ED2K、thunder、IPFS、RTMP | 扩展接管/独立安装/回滚 |
| P6 | 网盘插件：Google Drive/OneDrive/百度/阿里（最后） | OAuth/刷新/共享链接 |
| P7 | 沙箱、自动更新、长稳、模糊测试 | 安全审计 + soak |

## 18. 插件矩阵

完整支持分级与缺口见 [SUPPORT_MATRIX.md](SUPPORT_MATRIX.md)；矩阵外视为未支持。

| 插件 | 默认携带 | 阶段 | 说明 |
| --- | --- | --- | --- |
| `http` | 是 | P1 | HTTP/HTTPS/Metalink/多镜像 |
| `bt` | 是（懒启动） | P3 | BitTorrent/Magnet |
| `ftp` | 可选 | P4 | FTP/FTPS |
| `media` | 可选 | P4 | HLS/DASH |
| `metalink` | 可选 | P4 | 单独的多源/校验插件（也可由 HTTP 插件内置） |
| `extractor` | 可选 | P5 | yt-dlp 等外部提取器 |
| `sftp` / `webdav` / `s3` | 可选 | P5 | 云盘/远程文件 |
| `ed2k` / `thunder` / `ipfs` / `rtmp` | 否 | P5+ | 独立签名插件，默认不装 |
| `gdrive` / `onedrive` / `dropbox` / `baidupan` / `aliyunpan` | 否 | P6（最后） | OAuth/官方 API；见 CLOUD_PLUGINS.md |

默认安装只保证 HTTP 可用；BT/媒体等即使随包携带也不启动，用户首次使用或启用时
才加载。所有插件均为第一方开发、签名、版本化，不是第三方市场。

## 19. 当前状态

**P0 骨架已落地，但还没有接入 Host UI，也没有真实下载协议。**

- `crates/aether-plugin-sdk`：manifest、MessagePack IPC、协议类型、插件 runner、Host 侧 supervisor。
- `crates/aether-plugin-fake`：fake 插件 + 真实进程契约测试（握手、Probe、Start、Progress、Completed、id 不匹配拒绝）。
- Host UI 仍由 `src/engine.rs` 提供模拟数据；接线要等隔壁 UI 重设计完成，避免冲突。
- 真实协议插件仍是 0 个；下一个是 P1 HTTP 插件。

## 20. 结论

Aether 的产品形态是：

> **一个原生管理面板 + 一组第一方协议插件。**

插件化不是为了第三方生态，而是为了：

- 每个协议独立崩溃域、独立版本、独立权限；
- 默认只加载 HTTP，BT/媒体等按需启动，保持低资源；
- 新增协议不修改 Host/UI；
- 设置/详情由 schema 驱动，M3 视觉统一；
- 打包时 Host 与默认插件一起分发，安装后即用。

这比“单体全能引擎”更符合现在这个项目的长期形态，也更适合桌面 + ARM64 的低资源定位。
