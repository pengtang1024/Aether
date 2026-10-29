# 浏览器集成方案

> 目标：让浏览器下载可以交给 Aether 管理，同时保持本地、显式授权、不做 Cookie/密码窃取。
> **状态：P5 核心已实现并接线**（native host、MV3 扩展、URL handler、daemon 转发、
> Cookie 显式授权、站点过滤、Windows manifest 安装）；商店上架仍待补。

## 0. 当前实现

| 组件 | 位置 | 状态 |
| --- | --- | --- |
| Native Messaging host | `crates/aether-native-host` | 已实现，4 字节 native-endian + JSON，最大 1 MiB；`referrer/headers/cookies/filename` 透传进 `TaskSpec.settings`（集成测试断言） |
| 浏览器扩展 | `browser-extension/` | MV3：右键发送、可选接管、站点过滤、Cookie 显式授权（可选权限，默认关） |
| 桌面 URL handler | `packaging/aether-url-handler.desktop` | `aether/magnet/ed2k/thunder` |
| daemon 连接 | `aether_daemon::Client` | 未运行时自动启动 `aether-daemon`；本地 socket/管道即可信（同用户权限） |
| 安装脚本 | `scripts/install-native-messaging.sh` / `.ps1` | Linux/macOS shell；Windows 注册表版（HKCU，免管理员） |
| UI 检测 | `browser_extension_installed()` | Unix 查 manifest 文件，Windows 查 HKCU 注册表 + Firefox 文件 |

本地安装：

```bash
# 1. 构建/安装 Aether 后
# 2. 浏览器加载 browser-extension/，复制扩展 ID
aether-install-native-messaging <extension-id> /usr/bin/aether-native-host
# Windows：powershell -File scripts/install-native-messaging.ps1 -ExtensionId <id> -HostPath <exe>
# 3. 扩展选项页点击“Test native host”
# 4. 如协议关联未生效，刷新桌面数据库/设置默认处理器
update-desktop-database ~/.local/share/applications 2>/dev/null || true
xdg-mime default aether-url-handler.desktop x-scheme-handler/magnet
```

限制：扩展尚未上架商店；Cookie/站点过滤等扩展侧改动无法自动化测试
（无浏览器测试基建），靠选项页 “Test native host” + 手工冒烟验证。

HTTP 插件已消费这些设置：`referrer` → Referer 头、`user_agent` → User-Agent、
`cookies`（字符串或对象）→ Cookie 头、`headers` 对象逐项附加（Range/Host/
Content-Length/Connection 等受管头会被拒绝）；`max_speed_bps` 做任务级限速。
服务器如果依赖请求头鉴权/防盗链，接管即可生效。

## 1. 组成

```
Chrome/Edge/Firefox 扩展
        │ Native Messaging（stdio）
        ▼
aether-native（随 Host 安装的小型 native host）
        │ 本地 IPC（Unix socket / Windows named pipe）
        ▼
Aether Host（已在运行；未运行则启动）
```

- 不使用 localhost TCP 端口；避免被局域网/网页访问。
- Native host 只做协议转换和本地认证，不解析下载协议。
- Host 负责任务创建、插件探测、UI 通知。

## 2. 扩展能力

- 右键“使用 Aether 下载链接”。
- 接管浏览器下载（可开关、按文件类型/站点过滤、询问模式）。
- 捕获 URL、Referer、User-Agent、必要的 Cookie/Headers（仅用户显式授权）。
- `aether://` 与 `magnet:` 协议关联（由安装器注册）。
- 扩展弹窗：最近任务、暂停/继续、打开 Aether。
- 不读取密码、支付信息、浏览历史；不支持 incognito 除非用户显式开启。

## 3. Native Messaging

- Chrome/Edge：`com.pengtang1024.aether.json` 放入浏览器 NativeMessagingHosts 目录。
- Firefox：`com.pengtang1024.aether.json` 放入 `~/.mozilla/native-messaging-hosts` 或系统目录。
- 消息：`{type:"add", url, referrer, headers?, cookies?, filename?}`；
  返回 `{ok, task_id, error?}`。
- 限制消息大小（如 1 MiB）；校验 scheme（http/https/ftp/magnet）；拒绝 `file://`。
- 本地信任模型：同用户文件权限（Unix socket 0600 / Windows 管道默认 DACL），
  不另设 token。远程场景走 daemon 的 RPC over TCP（token 鉴权，见 `DAEMON.md`）。
- Host 未运行时：native host 启动 Host，等待就绪；超时返回错误。

## 4. 接管浏览器下载

1. 用户在扩展中开启“接管下载”。
2. `chrome.downloads.onCreated` / Firefox `downloads.onCreated` 捕获。
3. 扩展把 URL/headers 交给 native host；Host 创建任务。
4. 扩展调用 `downloads.cancel()` 取消浏览器原生下载。
5. Aether 通知“已接管”，任务进入队列。

失败/不支持时不要取消浏览器下载；先确认 Host 接受任务再取消。

## 5. 隐私与安全

- 默认不接管、不读取 Cookie；逐项授权。
- Cookie/Headers 只在任务请求中使用，不持久化到日志/DB（或加密短期保存）。
- 站点白名单/黑名单；敏感站点排除。
- Native host 校验扩展 ID；Host 校验本地 token；同一用户权限。
- 不在网页注入脚本；不使用远程调试端口。
- 扩展商店审核要求的最小权限原则。

## 6. 打包

- 扩展：Chrome/Edge MV3、Firefox WebExtensions；构建 zip/xpi。
- Chrome Web Store / Edge Add-ons / Firefox Add-ons 上架；本地可侧载开发版。
- Native host 随 Host 安装包分发；安装器注册 manifest 路径。
- 扩展与 Host 版本兼容：native 协议 `api:1`；Host 升级不破坏旧扩展。

## 7. 阶段

- P5：扩展 + native host + 接管/右键/协议关联。
- 后续：RSS、剪贴板监听、批量 URL 导入（Host 功能）。
