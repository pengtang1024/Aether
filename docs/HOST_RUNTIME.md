# Host 运行时与 CLI

> 状态：已实现 Host 侧插件运行时和一个无 UI 的 CLI，用真实插件跑通
> Probe → Start → Progress → Completed → 最终落盘。Slint UI 经 `src/backend.rs` 接入
> （含 `stats` 统计与插件启停）。

## 1. 代码结构

| crate | 职责 |
| --- | --- |
| `aether-host` | 插件发现、按需启动、Probe 路由、事件分发、最终落盘校验 |
| `aether-cli` | 无 UI 命令行：`list` / `probe` / `download` |
| `aether-plugin-sdk` | manifest、IPC、协议类型、进程 supervisor |

`aether-host` 是纯 Rust 库，不依赖 Slint；GUI 接线时直接复用，不需要把插件逻辑写进
`src/main.rs`。

## 2. 插件目录

`PluginRegistry` 扫描：

```text
<plugins-dir>/<plugin-id>/plugin.json
<plugins-dir>/<plugin-id>/bin/<platform-binary>
```

查找顺序（`default_plugins_dir`）：

1. 环境变量 `AETHER_PLUGINS_DIR`
2. `/usr/lib/aether/plugins`（`.deb` 安装位置）
3. `build/plugins`（本地开发）
4. `plugins`

`plugin.json` 的 `executable` 使用平台键（`linux-x86_64`、`linux-aarch64`、
`windows-x86_64`、`windows-arm64`、`macos-universal`）；`resolve_executable`
拒绝绝对路径、`..` 和逃出包目录的符号链接。

## 3. 运行时行为

- 插件进程**按需启动**：第一次 Probe/Start 时才 spawn；禁用插件不会占内存。
- 一个插件进程多任务；每个任务由插件内部 worker 处理。
- Probe 按插件声明顺序执行；结果按 `priority` 从高到低排序。
- `PluginManager::recv_event` 轮询所有进程，返回 `{ plugin_id, event }`。
- Host 只信任 `TaskPaths.staging`；插件完成后按 `FileInfo` 清单执行最终落盘。

## 4. 最终落盘

`aether_host::finalize_task(staging, final_dir, files)`：

1. 相对路径只允许普通组件（拒绝绝对路径、`..`、Windows 前缀）。
2. `canonicalize` 后必须仍在 staging 内。
3. 校验文件大小；有 SHA-256 时必须匹配。
4. 目标已存在则报错，不静默覆盖。
5. 同文件系统 `rename`；跨文件系统 `copy + fsync + remove`。

插件永远不能直接写最终目录，也不能通过路径逃出 staging。

## 5. CLI

```bash
# 列出插件
aether-cli list --plugins build/plugins

# 探测 URL
aether-cli probe https://example.com/file.bin --plugins build/plugins

# 下载并最终落盘
aether-cli download https://example.com/file.bin \
  --dir ./downloads --plugins build/plugins

# 强制指定插件
aether-cli download magnet:?xt=... --dir ./downloads \
  --plugins build/plugins --plugin bt

# 查看持久化任务 / 继续未完成任务
aether-cli tasks --dir ./downloads
aether-cli resume <task-id> --dir ./downloads --plugins build/plugins
```

已验证：用本地 HTTP 服务器和 `build/plugins/http` 下载 256 KiB 文件，CLI 输出最终路径，
`cmp` 与源文件一致。

## 6. SQLite 持久化

`aether_host::TaskStore` 使用 bundled SQLite，默认库：

```text
<output>/.aether/aether.db
```

表：

| 表 | 内容 |
| --- | --- |
| `tasks` | URL、插件、状态、进度、错误、settings/plugin_state、时间戳 |
| `task_files` | 完成文件清单、大小、SHA-256 |
| `plugin_settings` | 每个插件的设置 JSON |
| `task_logs` | 任务结构化日志 |

- `PRAGMA user_version` 做迁移；当前 schema v1。
- 状态：`queued/running/paused/completed/failed/canceled`。
- `recoverable_tasks()` 返回 queued/running/paused，供 Host 重启恢复。
- 完成时写入文件清单并把进度置为 100%。

CLI：

```bash
aether-cli tasks --dir ./downloads
aether-cli resume <task-id> --dir ./downloads --plugins build/plugins
```

`resume` 重新启动同一插件；HTTP/FTP/HLS 等利用 staging 中的状态文件或文件长度续传，
BT 目前会重新检查种子数据。

## 7. 打包

Linux `.deb` 安装插件到：

```text
/usr/lib/aether/plugins/<id>/plugin.json
/usr/lib/aether/plugins/<id>/bin/<binary>
/usr/lib/aether/plugins/<id>/LICENSE
```

`make plugins` 用 `scripts/stage-plugins.py` 按当前平台 staging（当前 16 个）；`make deb` 自动包含。
AppImage 也把 `build/plugins` 放进 `usr/lib/aether/plugins`。

`.deb` 同时安装 `/usr/bin/aether-cli`，可直接使用 `tasks`/`resume` 和系统插件目录
`/usr/lib/aether/plugins`。

## 8. 尚未实现

- Slint UI：已接线（任务列表/控制/新建/代理/统计/插件启停）。
- PAC 支持、队列持久化策略之外的调度公平性（并发上限 + FIFO 已实现，见
  `DAEMON.md` §5.1）。
- 插件签名验证、更新/回滚、Linux 沙箱。
- Windows/macOS 安装包携带插件（Linux 优先，当前只保证 Linux 包）。
- 百度/阿里共享链接 OAuth、ED2K 内核。
