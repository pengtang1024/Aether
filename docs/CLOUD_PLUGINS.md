# 网盘插件方案（P6，最后）

> Google Drive、OneDrive、Dropbox、百度网盘、阿里云盘等以第一方插件接入。
> 需要 OAuth/官方 API/开放平台权限；不做 Cookie 抓取、不绕过风控/付费限制。
> 计划阶段：P6（最后）。

## 1. 插件形态

- 插件 ID：`gdrive`、`onedrive`、`dropbox`、`baidupan`、`aliyunpan`。
- 每个插件独立进程、独立签名/版本；Host 只负责任务、账号 UI、token keyring。
- 插件通过 SDK 的 `NetClient` 调 API，通过 Host 授权目录写文件。

## 2. OAuth 与账号

- Google Drive：OAuth2 + PKCE；scope `drive.readonly`（按需最小化）；Drive API v3。
- OneDrive：Microsoft identity platform + Graph；`Files.Read`。
- Dropbox：OAuth2 + PKCE；`files.content.read`。
- 百度/阿里：以官方开放平台/授权 API 为准；没有合规 API 的场景不做。
- token 存 Host keyring（Windows Credential Manager / macOS Keychain / Linux Secret
  Service），不写 SQLite 明文、不写日志。
- 多账号、刷新、撤销、退出；过期/撤销时任务暂停并提示重新授权。

## 3. 下载流程

1. 用户粘贴共享链接或选择文件。
2. 插件解析链接/ID，获取元数据（文件名/大小/校验和/直链）。
3. 分块/多连接下载；遵守 API rate limit；支持 Range 时续传。
4. Host 校验/原子落盘；插件上报进度/错误。
5. 共享链接权限变化、限流、配额不足时给出明确错误。

## 4. Host UI

- 插件设置页内“账号”区块：登录/授权、账号列表、空间/配额、退出。
- 新建任务时可选择“从网盘选择文件”（插件提供文件浏览 schema）。
- 任务详情显示来源/账号/共享链接；不显示 token。

## 5. 安全与合规

- 只使用官方/授权 API；不抓取浏览器 Cookie、不存密码。
- 最小 scope；PKCE；token 加密存储；日志脱敏。
- 遵守各服务 ToS/API 配额；不提供绕过会员/限速/风控的功能。
- OAuth client ID 为公开客户端；不把 client secret 嵌入桌面二进制。

## 6. 测试

- Mock OAuth/Graph/Drive API；token 过期/刷新/撤销；429/配额；共享链接权限变化。
- 大文件分块/续传；文件名/路径安全；多账号隔离。
- 插件崩溃不影响 Host；重启后 token/任务恢复。

## 7. 阶段

- P6（最后）：先 Google Drive/OneDrive（API 文档完整），再评估百度/阿里开放平台。
- 没有合规 API 的网盘不进入支持矩阵；不支持“非官方破解接口”。
