# Aether 下载引擎技术方案

> 目标：在 Rust + Slint 的桌面壳之下，做一个**低资源、跨平台、协议覆盖广**的下载核心。
> 本文是引擎设计蓝图；当前 `src/engine.rs` 仍只是模拟器，按本文的 Phase 路线逐层替换。

## 1. 范围与边界

### 1.1 目标协议

| 协议 | 优先级 | 说明 |
| --- | --- | --- |
| HTTP/HTTPS 1.1 / 2 / 3 | P0 | 分段、多连接、断点续传、镜像、鉴权、代理 |
| FTP / FTPS | P1 | REST 续传、SIZE/MDTM、被动模式、TLS |
| BitTorrent / Magnet | P1 | 多文件、DHT、PEX、LSD、Tracker、做种、端口映射 |
| Metalink | P1 | 多镜像 + 分块/整文件校验 |
| HLS / DASH | P2 | 点播与直播、AES-128、分片合并 |
| SFTP / WebDAV / S3 | P3 | 通过协议插件扩展 |
| yt-dlp 等外部提取器 | P3 | 插件调用，不进入核心协议层 |

### 1.2 非目标

- 不绕过 DRM、付费墙、登录/反爬机制。
- 不内置浏览器内核；页面解析交给插件或外部工具。
- 不把“全能”理解为支持所有私有协议；核心是主流开放协议 + 可插拔扩展。
- 首版不做系统级常驻服务/多用户守护进程；先做单用户桌面引擎。

### 1.3 核心指标

- 1000 个任务、其中 32 个活跃时，UI 交互不卡顿；引擎内存可配置。
- 单任务默认 4 连接，可配置 1–16；全局连接上限独立控制。
- 断点续传在进程被杀、系统重启后仍可恢复。
- 每个完成的文件都有可校验的完整性结果（SHA-256 / BLAKE3 / BT piece hash）。
- Linux / Windows / macOS 行为一致；路径、文件锁、sparse file、fsync 均有适配。

## 2. 总体架构

```
┌─────────────────────────────────────────────────────────────┐
│ Slint UI (main thread)                                      │
│  TaskRow / TaskDetail / Settings                            │
└───────────────▲──────────────────────────┬──────────────────┘
                │ EngineEvent (coalesced)  │ Command
┌───────────────┴──────────────────────────▼──────────────────┐
│ EngineHandle (facade, bounded mpsc)                         │
│  add / pause / resume / cancel / retry / limits / snapshot  │
└───────────────▲──────────────────────────┬──────────────────┘
                │                          │
┌───────────────┴──────────────┐  ┌────────▼──────────────────┐
│ Event bus / stats aggregator │  │ Scheduler actor            │
│ 10 Hz snapshots, speed EMA   │  │ priority queue, fairness   │
└───────────────▲──────────────┘  └────────┬──────────────────┘
                │                          │ spawn/cancel
┌───────────────┴──────────────────────────▼──────────────────┐
│ Task supervisor (one per task)                              │
│  state machine · retry · checkpoint · cancellation          │
├───────────────┬───────────────────┬─────────────────────────┤
│ HTTP engine   │ BitTorrent engine │ FTP / HLS / DASH / ...  │
│ segments      │ pieces / peers    │ protocol adapters       │
├───────────────┴───────────────────┴─────────────────────────┤
│ Storage layer: positioned file, sparse/prealloc, checksum   │
│ Persistence: SQLite WAL + per-task checkpoint files         │
│ Network: reqwest/hyper/rustls, quinn, hickory, proxy, rate  │
└─────────────────────────────────────────────────────────────┘
```

### 2.1 Crate 拆分

```
crates/
├── aether-core/          # 纯引擎，不依赖 Slint
│   ├── engine/           # scheduler, task actor, snapshot, commands
│   ├── protocol/         # http, ftp, bt, hls, dash, metalink
│   ├── storage/          # positioned file, segment writer, path safety
│   ├── persist/          # SQLite schema, migrations, checkpoints
│   ├── net/              # client builder, proxy, DNS, rate limiting
│   └── event.rs
├── aether-app/           # 当前 Slint 壳，只依赖 aether-core 的 facade
└── aether-cli/           # 可选：无 UI 的引擎验证工具
```

先保持单 crate 也能做 Phase 1；当协议层开始膨胀时再拆 workspace。核心约束是
**UI 不直接持有网络/文件句柄，引擎不依赖 Slint 类型**。

## 3. 核心对象与状态机

### 3.1 对象

```rust
struct Task {
    id: TaskId,                 // UUID v7，按时间可排序
    url: String,
    kind: TaskKind,             // Http | Ftp | Torrent | Hls | Dash | Metalink
    state: TaskState,
    dir: PathBuf,
    filename: Option<String>,
    total_bytes: Option<u64>,
    downloaded_bytes: u64,
    priority: i8,               // -10..10
    connections: u8,
    speed_limit: Option<u64>,   // B/s
    created_at: i64,
    error: Option<ErrorInfo>,
    checksum: Option<ChecksumSpec>,
    sources: Vec<Source>,
}

struct Source {
    url: String,
    kind: SourceKind,           // HttpMirror | Tracker | Peer | Metalink
    healthy: bool,
    score: i32,
    last_error: Option<String>,
}

struct Segment {
    file_index: usize,
    start: u64,
    end: u64,                   // exclusive
    state: SegmentState,        // Pending | InFlight | Done | Failed
    attempts: u8,
    hash: Option<[u8; 32]>,
}
```

### 3.2 状态机

```
Queued ──start──► Probing ──► Downloading ──► Verifying ──► Completed
  ▲                  │             │               │
  │                  │             ├─pause────────► Paused
  │                  │             ├─error────────► RetryWait ──► Queued/Downloading
  │                  │             └─cancel───────► Canceled
  │                  └─unsupported/error──────────► Failed
  └──────────────── retry / resume ─────────────────┘
Seeding（BT 完成后，可选）──► Completed / Paused
```

规则：

- 状态迁移只能由 Task supervisor 执行；UI 只发 Command。
- 每次迁移写入持久化层；崩溃后根据 `state + checkpoint` 恢复。
- `Paused` 保留部分文件与 segment 状态；`Canceled` 默认删除临时文件，可选保留。
- `Completed` 在最终校验和原子 rename 成功后才进入。

## 4. 调度与资源控制

### 4.1 两级调度

1. **全局调度器**：维护 Queued 任务的最小堆（优先级 + aging，避免饥饿）。
2. **任务内调度器**：为每个任务维护 segment 队列与连接 worker。

全局限制：

- `max_active_tasks`（默认 3）
- `max_total_connections`（默认 32）
- `max_connections_per_host`（默认 8）
- `max_connections_per_task`（默认 4）
- 下载/上传 token bucket（全局 + 单任务）
- 磁盘写入背压：写队列高水位时暂停读取 socket

### 4.2 队列与公平性

- 优先级 `-10..10`；每等待 30 秒提升一级有效优先级。
- 同一 host 使用信号量限制；`Semaphore` permit 从全局和 per-host 两层获取。
- 暂停/取消使用 `CancellationToken` 向下传播，不等待所有 socket 自然超时。
- 任务完成后释放 permit；失败重试进入 `RetryWait`，不占用活跃槽。

### 4.3 低资源模式

- `max_active_tasks=1`、每任务 2 连接、64 KiB 读写缓冲、BT 磁盘缓存 16 MiB。
- 可在设置中开启“仅 Wi-Fi/有线”或“电池供电时暂停”（平台能力探测后）。
- 事件合并到 10 Hz，避免每个 chunk 触发 UI 重绘。

## 5. HTTP/HTTPS 引擎

### 5.1 探测

1. `GET` + `Range: bytes=0-0`，`Accept-Encoding: identity`。
2. 解析：
   - `206` + `Content-Range: bytes 0-0/N` → 支持分段，总长 N。
   - `200` + `Content-Length` → 服务器忽略 Range，单流下载。
   - `416` → 可能是空文件或已满足；回退 HEAD/普通 GET。
3. 记录 `ETag`、`Last-Modified`、`Content-Disposition`、`Content-Type`、最终 URL。
4. HEAD 不可靠（403/405）时，只用 Range 探测，不发 HEAD。

### 5.2 分段策略

- 初始 segment 大小：4 MiB（可配置 1–16 MiB），最后一段取余。
- 连接数：min(配置, ceil(size / 2 MiB), per-host/global permit)。
- 慢段拆分：若剩余 > 2×最小段且该段速度低于中位数 50%，拆成两段并交给空闲 worker。
- 相邻完成段合并，减少元数据；不合并正在请求的段。
- 每个 worker 循环领取 segment，直到队列空或取消。
- 失败段进入任务内重试队列；连续失败触发 source 切换。

### 5.3 Range 请求细节

```http
GET /file.iso HTTP/1.1
Range: bytes=START-END
If-Range: "<etag>"           # 有 ETag 时优先
Accept-Encoding: identity
```

- 必须校验响应为 `206` 且 `Content-Range` 的 start/end 与请求一致。
- 若服务器返回 `200`：放弃分段，切换单流；已写数据只有在偏移 0 连续时才可复用。
- 若 ETag/Last-Modified 改变：默认作废旧分段并从 0 开始；可选“重新校验后保留同前缀”。
- `Retry-After`、`429`、`503` 做指数退避 + jitter；`401/403` 不盲目重试。
- 重定向策略：最多 10 跳；跨 scheme 需重新校验（https→http 默认拒绝）。
- 代理：HTTP CONNECT / SOCKS5；代理认证只存于内存/系统 keyring，不写日志。
- 鉴权：Basic/Bearer/自定义 Header；Cookie 可由调用方提供；不自动读取浏览器 Cookie（后续插件）。
- 证书：rustls + 系统根证书；默认严格校验，提供“仅本次任务忽略证书”开关并显式警告。

### 5.4 多连接与 HTTP/2/3

- HTTP/1.1：每连接一个 Range 请求，连接池按 host 复用。
- HTTP/2：一个 TCP/TLS 连接上多路复用多个 Range stream；per-host 并发按 stream 计。
- HTTP/3：启用 quinn/h3 时同样多路复用；UDP 被封时快速回退 HTTP/2。
- 是否启用 h2/h3 由客户端能力 + 服务器 ALPN 决定；不在代码里假设。
- 下载请求禁用透明压缩，避免 Content-Length 与实际字节不一致。

### 5.5 续传

- checkpoint 记录：最终 URL、ETag、Last-Modified、总大小、segment 表、已完成字节、校验和。
- 重启后先做轻量探测；ETag/大小一致才续传，否则标记 `RemoteChanged` 并提示。
- `If-Range` 不匹配时，服务器返回 200；此时必须丢弃旧 segment 并重新规划。
- 写入采用 `pwrite`/positioned write；每个 segment 完成后记录完成位图。
- 进程崩溃最多回退一个 checkpoint 周期（默认 1 秒）内的 segment。

### 5.6 多镜像 / Metalink

- Metalink 解析出 URL 列表、整文件 hash、piece hash、大小、媒体类型。
- 镜像评分：最近成功率、RTT、吞吐、失败次数、协议优先级。
- 每个 segment 可绑定不同镜像；响应必须与主源大小/ETag 兼容（或使用 piece hash 校验）。
- 镜像全部失败时回到主 URL；不无限重试。
- 同一 segment 不允许并发写同一偏移；以 segment lease 保证。

## 6. BitTorrent 引擎

### 6.1 实现策略

优先使用成熟的 Rust 实现（候选 `librqbit`）作为 BT 内核，Aether 只做：

- 任务模型/状态机、目录与文件选择、限速、调度、事件映射；
- 与 HTTP/FTP 共用的存储、校验、路径安全、设置；
- 需要自定义 piece picker/顺序下载时再评估 libtorrent FFI，不一开始重写协议。

BT 内核接口用 trait 隔离，避免 UI/引擎绑死某个库。

### 6.2 功能面

- `.torrent` 与 magnet；多文件/单文件。
- Tracker：HTTP/HTTPS/UDP；`announce-list` 回退。
- DHT、PEX、LSD（可配置关闭）。
- 连接：TCP + uTP（如库支持）；加密优先/强制/关闭；proxy。
- Piece picker：rarest-first 默认；顺序/流式模式；endgame；优先文件/边界。
- Choking/unchoking、上传槽、做种比率/时间限制。
- 端口映射：UPnP/NAT-PMP；失败不阻塞下载。
- 私有种子：禁用 DHT/PEX/LSD，遵守 `private` 标志。
- 断点续传：piece 完成位图 + 磁盘校验；重启后只重算未完成/可疑 piece。
- 路径安全：拒绝绝对路径、`..`、symlink、Windows 保留名；多文件落盘前统一 sanitize。
- 限速：全局/任务上传下载；连接数/peer 数上限；IP blocklist（可选）。
- 事件：peers、pieces、ratio、seeders、tracker 状态，映射到 UI 的 BT 节点/分片面板。

### 6.3 存储

- 多文件种子按文件布局写入；临时文件 `.part` 与最终文件同目录。
- Piece 校验使用 torrent 的 SHA-1；校验失败重下该 piece。
- 顺序模式用于边下边播；非顺序模式优先稀有 piece。
- 磁盘缓存有上限；写放大通过合并写与对齐 piece 边界控制。

## 7. FTP / FTPS / SFTP

### 7.1 FTP/FTPS

- 被动模式（EPSV/PASV），必要时回退主动模式。
- `SIZE` 获取大小；`MDTM` 获取时间；`REST` 断点续传；`TYPE I` 二进制。
- FTPS：显式 AUTH TLS 优先；证书严格校验；不支持时明确失败。
- 单连接为主；若服务器支持 `REST` 且允许并发，可选有限分段（默认关闭，兼容性优先）。
- 目录列表/文件名解析使用 UTF-8，失败回退服务器编码并提示。

### 7.2 SFTP（P3 插件）

- `russh-sftp` 或等价库；密码/密钥/agent 认证。
- 断点续传按 offset；权限 0600；主机密钥严格校验。
- 与 HTTP 共用 task/segment/checksum 抽象。

## 8. HLS / DASH

### 8.1 HLS

- 解析 master/variant playlist；按带宽/分辨率选择 variant（可手动）。
- 下载 init segment + media segments；支持 `EXT-X-BYTERANGE`、`EXT-X-MAP`。
- AES-128：拉取 key，解密后写入；key URI 鉴权头可配置。
- 直播：按 `EXT-X-MEDIA-SEQUENCE` 滚动刷新；保留窗口；停止条件（时长/用户取消）。
- 输出：分片目录 + 合并后的 `.ts`/`.m4s`（可选 ffmpeg 插件 remux 为 mp4）。

### 8.2 DASH

- 解析 MPD；选择 Representation；SegmentTemplate/List/Base。
- 支持 `SegmentBase`（单文件 Range）、多 Period、字幕/音频轨选择。
- 直播的 `availabilityStartTime`/`timeShiftBufferDepth` 用于窗口计算。

### 8.3 媒体边界

- 核心只负责“按 manifest 下载分片并校验”，不内置转码器。
- remux/转码通过可选插件调用 ffmpeg；插件缺失时保留分片并提示。
- DRM（Widevine/PlayReady/FairPlay）不在范围内。

## 9. 存储层

### 9.1 文件布局

```
<download-dir>/.aether/<task-id>/
├── meta.json          # 任务快照（可读，便于恢复/排障）
├── data.part          # 单文件任务
├── files/             # 多文件/种子
└── segments.json      # 可选：大任务的 segment 快照
```

最终文件通过原子 rename 从 `.part` 移到目标目录；多文件任务逐文件完成、
逐文件 rename，最后删除任务临时目录。

### 9.2 Positioned I/O

- Unix：`FileExt::write_all_at` / `read_at`。
- Windows：`seek_write` / `seek_read` 或 Overlapped I/O 封装。
- 统一 `PositionedFile` trait；上层 segment worker 不关心平台。
- 写缓冲 64–256 KiB；按 segment 批量写，避免每 4 KiB syscall。
- `fsync` 策略：checkpoint 时 fsync 数据文件 + 元数据；完成后 fsync 文件与目录，再 rename。
- 预分配：`set_len` 创建稀疏文件；Linux 可选 `fallocate`；Windows/macOS 用平台等价或跳过。
- 磁盘空间：开始前检查可用空间 + 预留 5%；写入 `ENOSPC` 时暂停任务并保留 checkpoint。

### 9.3 路径安全

对所有远端提供的文件名/种子路径执行：

1. 拒绝绝对路径、盘符、UNC、`..`、空组件。
2. Windows 保留名（CON/PRN/AUX/NUL/COM1..9/LPT1..9）、结尾空格/点、控制字符。
3. Unicode NFC 规范化；大小写冲突检测（Windows/macOS 默认不区分大小写）。
4. 拒绝 symlink/hardlink 目标逃逸；创建文件使用 `O_NOFOLLOW`（平台支持时）。
5. 路径长度限制；Windows 长路径前缀按需处理。
6. 最终路径必须位于任务目录的 canonicalize 后前缀内。

## 10. 持久化与崩溃恢复

### 10.1 SQLite（WAL）

```sql
CREATE TABLE tasks (
  id TEXT PRIMARY KEY, url TEXT NOT NULL, kind INTEGER NOT NULL,
  state INTEGER NOT NULL, dir TEXT NOT NULL, filename TEXT,
  total_bytes INTEGER, downloaded_bytes INTEGER NOT NULL DEFAULT 0,
  priority INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL,
  error_json TEXT, checksum_json TEXT, meta_json TEXT
);
CREATE TABLE files (
  task_id TEXT NOT NULL, idx INTEGER NOT NULL, path TEXT NOT NULL,
  size INTEGER NOT NULL, done INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (task_id, idx)
);
CREATE TABLE segments (
  task_id TEXT NOT NULL, file_idx INTEGER NOT NULL,
  start INTEGER NOT NULL, end INTEGER NOT NULL,
  state INTEGER NOT NULL, attempts INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (task_id, file_idx, start)
);
CREATE INDEX segments_pending ON segments (task_id, state);
```

- DB 操作走独立 DB actor（`rusqlite` + 单线程/`spawn_blocking`），避免 async 线程阻塞。
- 事务批量提交；checkpoint 1 秒或 4 MiB 触发一次，崩溃最多丢一个周期。
- 迁移用 `user_version` + 顺序 SQL；启动时自动迁移并备份旧库。
- `meta.json` 作为只读快照，DB 损坏时可人工恢复。

### 10.2 恢复流程

1. 加载 DB 中非终态任务。
2. 校验临时文件存在、大小、已完成 segment 的边界。
3. HTTP/FTP 探测远端 ETag/大小；BT 读取 piece 位图。
4. 重建 segment 队列；已完成且校验通过的段不重下。
5. 进入 Queued，由调度器决定何时启动。

## 11. 网络层

- HTTP 客户端：`reqwest`（hyper + rustls）为主；HTTP/3 可选 `quinn`/h3 feature。
- DNS：系统解析器默认；可选 `hickory-resolver` 做缓存/DoH；Happy Eyeballs 连接。
- TLS：rustls；系统根证书；可选自定义 CA；默认不跳过校验。
- 代理：HTTP/HTTPS CONNECT、SOCKS5；全局/任务级；PAC 暂不做。
- 超时：连接 15s、首字节 30s、空闲读 60s（可配置）；下载大文件不设总超时。
- Keep-alive：HTTP/1.1 连接池；HTTP/2/3 多路复用；按 host 隔离。
- 速率：分层 token bucket（全局→host→task）；上传/下载独立。
- IPv6：优先 Happy Eyeballs，失败回退 IPv4；绑定接口可选（P3）。

## 12. 错误模型与重试

```rust
enum ErrorKind {
    Dns, Connect, Tls, Timeout, HttpStatus(u16), Auth,
    RangeMismatch, Checksum, DiskFull, Permission,
    Protocol, Canceled, RemoteChanged,
}
```

- 可重试：DNS 临时失败、连接重置、超时、408/425/429/5xx、range 临时错误。
- 不可重试：401/403（除非凭据刷新）、404、410、校验和不匹配（重下一次后仍失败）、路径非法。
- 退避：`min(60s, 500ms * 2^attempt) * (0.5..1.5 jitter)`；尊重 `Retry-After`。
- 每个 host 一个轻量 circuit breaker：连续失败过多时短暂降级，避免雪崩。
- 用户可见错误包含：阶段、URL（脱敏）、HTTP 状态、建议动作、可复制的诊断 ID。

## 13. UI 桥接

当前 Slint 层已有 `TaskRow` / `TaskDetail` / `VecModel`。接入真实引擎后：

- `EngineHandle` 暴露 `send(Command)` 与 `subscribe()`。
- 引擎在 Tokio runtime 运行；不阻塞 Slint 主线程。
- `EngineEvent` 通过 `slint::invoke_from_event_loop` 回到 UI；每 100 ms 合并一次。
- 行更新继续用 `set_row_data` 差分；详情抽屉的 peers/logs 只在打开/切换任务时重建，
  避免每 tick 重置 repeater（此前的滚动 bug 根因）。
- 速度图用固定长度 ring buffer（60 个样本），引擎侧计算 EMA/峰值，UI 只画路径。
- 命令：`AddTask`、`Pause`、`Resume`、`Cancel`、`Retry`、`Remove`、`SetLimits`、`Reorder`、
  `OpenFolder`；事件：`TaskUpdated`、`TaskRemoved`、`GlobalStats`、`Log`、`Toast`。

## 14. 安全

- 默认严格 TLS；忽略证书必须每次显式确认。
- URL/凭据不写日志；错误信息脱敏 `user:pass@`、query token。
- 下载内容不自动执行；完成后不自动打开可执行文件。
- 路径穿越/种子路径/文件名 sanitize（见 9.3）。
- HLS/DASH manifest、torrent、Metalink 都视为不可信输入；限制 XML/JSON 大小与嵌套深度。
- 校验和是完成条件之一；BT 用 piece hash，HTTP 用服务端 hash/Metalink/BLAKE3。
- 插件（yt-dlp/SFTP/ffmpeg）以子进程或受控接口运行，不把任意字符串拼进 shell。
- 代理/认证信息优先放系统 keyring；不写入 SQLite 明文（至少加密/受限权限 0600）。

## 15. 性能与低资源

- 连接数、内存、磁盘写队列均可配置；默认面向低资源设备。
- 单任务内存目标：< 16 MiB（不含 BT piece cache）；全局缓冲池上限 64 MiB。
- 不把整个文件读入内存；所有下载流式落盘。
- HTTP/2/3 复用连接，减少握手；HTTP/1.1 用连接池。
- 合并 UI 事件到 10 Hz；速度/ETA 用 EMA，不每包计算。
- BT piece cache 有硬上限；顺序模式优先写连续区间。
- Release：LTO + strip；ARM64 原生；Windows 静态 CRT；无 WebView。
- 可选 `io_uring`（Linux）作为后续优化，不作为首版依赖。

## 16. 可观测性

- `tracing` 结构化日志：task_id、protocol、host、segment、attempt、error kind。
- 指标：活跃任务、连接数、吞吐、队列长度、重试次数、校验失败、磁盘写入延迟。
- 每任务保留最近 200 条日志与 60 个速度样本；UI 抽屉直接消费。
- 诊断包：任务元数据 + 最近日志 + 配置（脱敏）+ 平台信息；用户可导出。
- 错误 ID 短哈希，便于用户报障时定位。

## 17. 测试策略

### 17.1 单元/属性

- segment 切分/合并/边界、剩余字节、空文件、1 字节文件。
- 路径 sanitize：`..`、绝对路径、Windows 保留名、Unicode、大小写冲突。
- 退避/jitter/优先级 aging；token bucket 精度。
- 校验和/断点元数据序列化。

### 17.2 集成

- 本地 hyper 测试服务器：
  - 支持/不支持 Range、206/200/416、ETag 变化、重定向、401/403、429 + Retry-After、
    慢速/断流/半包、Content-Disposition、gzip 干扰。
- 本地 FTP 服务器：REST/SIZE/MDTM/TLS。
- 本地 BT：生成小种子、tracker/peer、magnet、piece 校验、做种。
- HLS/DASH：静态 fixture + AES-128 + 直播滚动窗口。
- 代理：本地 HTTP CONNECT/SOCKS5。
- 崩溃恢复：下载中 SIGKILL，重启后校验续传；重复 100 次。
- 跨平台 CI：Linux x64/arm64、Windows x64/arm64、macOS universal。

### 17.3 模糊测试

- torrent 文件、m3u8/mpd、Metalink XML、Content-Disposition、路径字符串。
- 网络输入截断/乱序/超大 header；限制内存与递归。

## 18. 依赖选型（候选）

| 能力 | 选型 | 备注 |
| --- | --- | --- |
| async runtime | tokio | 多线程；限流/取消/信号量 |
| HTTP | reqwest + hyper + rustls | h2 默认；h3 可选 feature |
| HTTP/3 | quinn + h3 | 可选；UDP 失败回退 |
| DNS | hickory-resolver | 可选缓存/DoH |
| TLS | rustls + webpki-roots/系统根 | 严格校验 |
| SQLite | rusqlite（bundled） | DB actor，WAL |
| 校验 | sha2、blake3、crc32fast | 流式 |
| 限速 | governor 或自研 token bucket | 分层桶需要自研封装 |
| BT | librqbit（优先评估） | 接口隔离；必要时 libtorrent FFI |
| FTP | suppaftp / async-ftp | Phase 4 评估异步支持 |
| SFTP | russh-sftp | 插件 |
| HLS | m3u8-rs | AES-128 另配 aes/cbc |
| DASH | dash-mpd | MPD 解析 |
| Metalink | quick-xml + serde | 限制 XML 大小 |
| 端口映射 | igd-next / portmapper | UPnP/NAT-PMP |
| 通知 | notify-rust | 可选 |
| 日志 | tracing + tracing-subscriber | 结构化 |

依赖以当前稳定版为准；协议层用 trait 隔离，避免单库锁定。

## 19. 分阶段路线图

### Phase 0：核心骨架（1–2 周）

- `aether-core` crate、Command/Event、任务状态机、SQLite schema、路径安全。
- 用现有模拟引擎适配新接口，UI 行为不变。
- 验收：1000 个任务快照无阻塞；重启恢复空任务。

### Phase 1：HTTP 单流 + 分段 + 续传（2–3 周）

- Range 探测、分段 worker、checkpoint、SHA-256/BLAKE3、原子 finalize。
- 本地测试服务器覆盖 Range/ETag/重定向/429。
- UI 接入真实进度、速度、ETA。
- 验收：1 GB 文件 8 连接下载；kill -9 后恢复；ETag 变化正确重启；校验失败可重试。

### Phase 2：调度/限速/代理/镜像（2 周）

- 全局/per-host/per-task 限制、优先级 aging、token bucket、代理、鉴权、Metalink 多源。
- 验收：多任务公平；限速误差 < 5%；代理/镜像切换不丢进度。

### Phase 3：BitTorrent（3–4 周）

- 接入 BT 内核；magnet/多文件/DHT/Tracker/做种/端口映射/路径安全。
- UI 的 peers/chunks 使用真实数据。
- 验收：公开种子下载/做种；私有种子禁用 DHT；重启校验；多文件路径安全。

### Phase 4：FTP/HLS/DASH（2–3 周）

- FTP/FTPS 续传；HLS/DASH 点播 + AES-128；直播窗口；可选 ffmpeg remux。
- 验收：点播分片合并可播放；直播停止/恢复；FTP REST 续传。

### Phase 5：插件与高级能力（持续）

- SFTP/WebDAV/S3、yt-dlp 插件、浏览器集成、通知、计划任务、代理 PAC。
- 验收：插件崩溃不影响核心；外部工具超时/输出限制。

### Phase 6：硬化（持续）

- 模糊测试、长时间 soak、跨平台 CI、内存/句柄泄漏、磁盘满/只读、休眠唤醒、网络切换。

## 20. 主要风险与对策

| 风险 | 对策 |
| --- | --- |
| 服务器 Range/ETag 行为不一致 | 探测 + 单流回退 + If-Range + 明确 RemoteChanged |
| BT 库能力/维护风险 | trait 隔离；librqbit 与 libtorrent FFI 可替换 |
| HLS/DASH 直播窗口复杂 | 先点播，直播单独 feature；窗口边界测试 |
| 路径穿越/种子恶意路径 | sanitize + canonicalize + 拒绝 symlink/保留名 |
| 低资源设备内存/句柄 | 硬上限、背压、缓冲池、连接 semaphore |
| 跨平台文件 I/O 差异 | PositionedFile trait + 平台 CI + 崩溃测试 |
| UI 被高频事件拖慢 | 10 Hz 合并 + 行 diff + 详情模型稳定 |
| 签名/分发限制 | macOS 未公证/Windows 未签名在 release notes 明示 |

## 21. Rust API 草图

```rust
pub struct EngineHandle { tx: mpsc::Sender<Command> }

pub enum Command {
    Add(AddTask),
    Pause(TaskId),
    Resume(TaskId),
    Cancel { id: TaskId, remove_files: bool },
    Retry(TaskId),
    Remove { id: TaskId, remove_files: bool },
    SetLimits(Limits),
    Reorder { id: TaskId, priority: i8 },
    Snapshot { reply: oneshot::Sender<Vec<TaskSnapshot>> },
}

pub enum EngineEvent {
    TaskUpdated(TaskSnapshot),
    TaskRemoved(TaskId),
    GlobalStats(GlobalStats),
    Log(LogEntry),
    Toast(String),
}
```

`TaskSnapshot` 是 UI 的只读投影，不暴露 socket/file/peer 内部句柄；这样 Slint 层
可以安全地在主线程消费，也方便未来做 CLI/daemon。

## 22. 结论

“技术全能”不等于首版把所有协议写完。正确的顺序是：

1. 先把 **任务状态机、调度、存储、续传、校验、UI 桥接** 做扎实；
2. HTTP 分段是核心中的核心，先做到生产级；
3. BT 用成熟内核 + 适配层，避免重写协议；
4. FTP/HLS/DASH/Metalink 作为独立协议适配器逐步接入；
5. 插件和外部提取器放最后，保持核心轻量。

这样既能覆盖主流下载场景，又能维持 Aether 的低资源、原生、跨平台定位。
