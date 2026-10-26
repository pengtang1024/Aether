# 守护进程

> 目标：Aether 常驻后台，持有插件运行时和 SQLite，通过本地 socket 接受
> 浏览器扩展、CLI、桌面 UI 和系统协议处理器的下载请求。

## 1. 组成

```text
aether-daemon
  ├─ PluginManager（插件进程 / 可选 cdylib）
  ├─ TaskStore（SQLite）
  ├─ Unix socket server
  └─ 事件循环：进度/完成/失败/最终落盘
```

- 单实例：socket + `flock` 锁；第二个实例直接失败。
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
| `add` | `url`, `plugin?`, `settings?` | 创建任务、Probe、启动插件 |
| `list` | — | 最近 100 个任务 |
| `get` | `task_id` | 单任务 + 文件 |
| `pause` / `resume` / `cancel` | `task_id` | 生命周期 |
| `shutdown` | — | 停止 daemon |

示例：

```bash
printf '%s\n' '{"id":1,"method":"add","url":"https://example.com/file.bin"}' \
  | socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/aether/daemon.sock
```

Rust 客户端：`aether_daemon::Client`（`ping/add/list/task/pause/resume/cancel/shutdown`）。

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

## 4. 恢复与状态

- 启动时 `recoverable_tasks()` 读取 `queued/running/paused` 任务并重新启动插件。
- HTTP/FTP/HLS 等利用 staging 中的状态文件或文件长度续传。
- 完成事件由 daemon 执行 `finalize_task`，再写入 `task_files`。
- 失败写入错误类别/消息/可重试标记。

## 5. 接管系统下载还差什么

daemon 是常驻入口，但“接管所有下载”还需要：

1. **浏览器扩展 + Native Messaging**：扩展把下载 URL/Headers 交给 native host，
   native host 再通过本地 socket 调 daemon。见 `BROWSER_INTEGRATION.md`。
2. **系统默认下载器/协议关联**：`aether://`、`magnet:`、`ed2k:`、`thunder:` 的
   desktop MIME/URL handler；由安装器注册。
3. **剪贴板监听/RSS**：Host 功能，默认关闭。
4. **桌面 UI 连接 daemon**：Slint Host 作为 socket 客户端，不再直接持有插件进程。
5. **权限与自动启动策略**：systemd user unit、首次启动授权、单用户 session。
6. **多用户/远程**：当前只支持本机同用户 Unix socket；不做局域网 TCP。

## 6. 安全边界

- 不做 localhost TCP，避免网页/局域网访问。
- socket 权限 0600；请求只能由同用户发起。
- `add` 的 URL/settings 会传给第一方插件；插件仍受 Host 最终落盘路径校验。
- cdylib 插件在 Host 进程内运行，daemon 默认只加载 `kind: "process"` 的插件；
  启用 cdylib 需显式配置并接受崩溃风险。
