# 浏览器集成方案

> 目标：让浏览器下载可以交给 Aether 管理，同时保持本地、显式授权、不做 Cookie/密码窃取。
> **状态：P5 基础已实现**（native host、MV3 扩展骨架、URL handler、daemon 转发）；
> 商店上架、Windows named pipe、站点过滤/Cookie 实际使用仍待补。

## 0. 当前实现

| 组件 | 位置 | 状态 |
| --- | --- | --- |
| Native Messaging host | `crates/aether-native-host` | 已实现，4 字节 native-endian + JSON，最大 1 MiB |
| 浏览器扩展 | `browser-extension/` | MV3 骨架，右键发送/可选接管/选项页 |
| 桌面 URL handler | `packaging/aether-url-handler.desktop` | `aether/magnet/ed2k/thunder` |
| daemon 连接 | `aether_daemon::Client` | 未运行时自动启动 `aether-daemon` |
| 安装脚本 | `scripts/install-native-messaging.sh` | Chrome/Edge/Chromium/Brave/Firefox |

本地安装：

```bash
# 1. 构建/安装 Aether 后
# 2. 浏览器加载 browser-extension/，复制扩展 ID
aether-install-native-messaging <extension-id> /usr/bin/aether-native-host
# 3. 扩展选项页点击“Test native host”
# 4. 如协议关联未生效，刷新桌面数据库/设置默认处理器
update-desktop-database ~/.local/share/applications 2>/dev/null || true
xdg-mime default aether-url-handler.desktop x-scheme-handler/magnet
```

限制：扩展尚未上架商店；native manifest 需要扩展 ID；Windows named pipe 未实现；
Cookie/Headers 只透传，HTTP 插件尚未使用；站点白名单/询问模式未实现。

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
- 本地 IPC token：Host 生成随机 token，native host 读取用户级配置文件；仅同用户可读。
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
