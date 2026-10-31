# Aether 支持矩阵

> 结论先说：**不能声称覆盖“所有下载情况”。** 插件架构解决的是可扩展性，不等于覆盖率。
> 本文是唯一支持范围来源；矩阵之外视为未支持，新增场景必须通过新插件实现，Host 不改。
>
> 当前：**16 个第一方下载插件 + 1 个 aria2 RPC 兼容服务插件已接入 daemon 与 Slint 插件页**（HTTP/BT/FTP/SFTP/WebDAV/S3/Metalink/HLS/DASH/yt-dlp/RTMP/IPFS/thunder/云盘等）；其余协议仍是目标/计划。

## 1. 支持分级

| 级别 | 含义 |
| --- | --- |
| Core | Host 默认携带、默认启用；基础下载能力 |
| Optional | 第一方签名插件，按需安装/启用 |
| External | 通过受控外部工具（yt-dlp/ffmpeg）提供，不保证所有站点 |
| Not planned | 明确不做（DRM/绕过/私有加速等） |

所有插件均为第一方开发、签名、版本化；不存在第三方插件市场。

## 2. 传输协议

| 场景 | 插件 | 级别 | 阶段 | 备注 |
| --- | --- | --- | --- | --- |
| HTTP/HTTPS 1.1 | http | Core | **已实现** | Range、分段、续传、重定向、代理；鉴权后续 |
| HTTP/2 | http | Core | **已实现** | reqwest 启用 `http2`，TLS ALPN 协商；HTTP/3 未启用 |
| HTTP/3 / QUIC | http | Core | P1/P2 | 可选；UDP 失败回退 h2 |
| FTP / FTPS | ftp | Optional | **已实现** | FTP + 显式 FTPS、REST/SIZE、SHA-256；MDTM 待补 |
| SFTP | sftp | Optional | **已实现** | 密码 / 私钥（`sftp_key` + passphrase）/ SSH agent；seek 续传；known_hosts 校验待补 |
| WebDAV | webdav | Optional | **已实现** | webdav(s) 方案、Basic Auth、Range；PROPFIND Depth:0 取大小/ETag（403/405 回退 HEAD）；PUT/锁待补 |
| S3 / 兼容对象存储 | s3 | Optional | **已实现** | SigV4 GET/HEAD、并发 Range 分片（1–8 连接）、续传；自定义 CA/分片上传待补 |
| Metalink | metalink | Optional | **已实现（HTTP 内置）** | v3/v4、优先级选源、SHA-256 |
| BitTorrent v1 / hybrid | bt | Optional（随包懒启动） | **已实现（基础）** | librqbit、magnet/.torrent、initial peers、多文件选择与完成后做种；DHT 压测待补 |
| BitTorrent v2 / pure v2 | bt | Optional | P3 待验证 | 依赖 BT 内核能力；不支持则明确报错 |
| WebTorrent / WebRTC | — | Not planned | — | 浏览器 P2P 不在核心 |
| ED2K / Kad | ed2k | Optional | P5+（未实现） | 需要 eDonkey 内核；当前不做 |
| thunder:// / QQdl:// / FlashGet | decoder | Optional | **thunder 已实现** | 仅 URL 解码后走 HTTP；不提供私有加速/离线 |
| IPFS / IPNS | ipfs | Optional | **已实现** | HTTP gateway；不内置完整 Kubo |
| RTMP / RTSP | rtmp | Optional | **已实现** | 通过 ffmpeg 子进程；不写实时流核心 |
| Usenet / NNTP | — | Not planned | — | 不在路线图 |
| TFTP / Gopher / SCP | — | Not planned | — | 不在路线图 |
| WebSocket / 长连接消息 | — | Not planned | — | 不是文件下载协议 |
| aria2 JSON-RPC 客户端 | aria2（服务插件） | Optional | **已实现（HTTP/WS）** | 与原生 RPC 共用 6800：POST/GET `/jsonrpc`、CORS、`token:` 鉴权、常用方法与 `system.multicall`；多 URI 与 `out` 文件名覆盖只取/不生效（文件名由探测决定）；列表接口受 daemon `list` 的 100 条上限约束；`addTorrent`/`addMetalink` 通过插件内置回环临时源交给 BT/HTTP 插件（`addTorrent` 的 web seed 列表忽略；内联字节在插件内存中保留 24 小时，daemon/插件重启后尚未开始的排队任务会失效）；WebSocket `/jsonrpc` 支持调用与 `aria2.onDownload*` 通知；逐任务 `changeOption`/`changePosition` 尚未实现 |

## 3. 内容与来源

| 场景 | 方案 | 级别 | 备注 |
| --- | --- | --- | --- |
| 普通文件 URL | http | Core（已实现插件） | 文件名/大小/校验 |
| 多镜像 / Metalink | metalink/http | Optional | 源评分、分段分配 |
| 种子 / magnet | bt | Optional | 多文件、做种、顺序模式 |
| HLS 点播 | media | Optional | **已实现插件** | AES-128、EXT-X-MAP、BYTERANGE、合并 |
| HLS 直播 | media | Optional | **已实现** | 滚动 playlist 刷新、按媒体序列去重、ENDLIST 收尾；断点续传不做 |
| DASH 点播 | media | Optional | **已实现插件** | SegmentTemplate/List/BaseURL、首个 Period；直播待补 |
| YouTube/Bilibili 等站点 | extractor | External | **已实现插件** | yt-dlp 子进程；站点覆盖取决于 yt-dlp，不保证 |
| Google Drive / OneDrive / Dropbox | gdrive / onedrive / dropbox | Optional | **已实现** | 公开链接转直链；私有文件支持现成 access_token 或 OAuth2 refresh_token（用户自建应用，插件不内置 client_id）；Dropbox 临时直链过期后需重开任务 |
| 百度网盘 / 阿里云盘等 | baidupan / aliyunpan | Optional | **官方 API 基础** | 文件 ID + access_token；共享链接 OAuth 待补；不做绕过 |
| 浏览器正在下载的文件 | browser-extension | Optional | P5 | 扩展 + Native Messaging；用户显式授权 |
| RSS / 自动追番 | Host 功能 | Optional | P5 | 后续 Host 功能，不是协议插件 |
| 剪贴板监听 | Host 功能 | Optional | P5 | 平台集成层，默认关闭 |
| 批量 URL 导入 | Host 功能 | 未实现 | 不属于协议插件 |

## 4. 网络、鉴权与调度

| 能力 | 状态 | 备注 |
| --- | --- | --- |
| 全局/单任务/单 host 限速 | 设计完成 | Host 发放预算，插件执行 |
| 优先级/队列/暂停/恢复 | 设计完成 | Host 权威状态 |
| 代理 HTTP CONNECT / SOCKS5 | 设计完成 | 全局/任务级；PAC 见下一行 |
| PAC / 自动代理（URL、文件、WPAD） | 设计完成 | P2；Host 内置 JS 沙箱求值、缓存、刷新、系统 PAC |
| Basic/Bearer/自定义 Header | 设计完成 | 插件设置 |
| Cookie 手动导入 | 设计完成 | 不自动读取浏览器 Cookie |
| Digest/NTLM/Kerberos | 未纳入 | 如有需求作为插件/SDK 扩展 |
| OAuth2 刷新 | **已实现** | 云端插件用 refresh_token 换 access_token（用户自建应用）；PKCE/设备码与 keyring 存储待补 |
| 客户端证书/mTLS | 未纳入 | 可后续加 |
| DNS/DoH/自定义 CA | 部分设计 | 系统 DNS 默认；DoH 可选 |
| 自定义 UA/Referer | 设计完成 | 插件设置 |
| IPv6/Happy Eyeballs | 设计完成 | 连接层 |
| UPnP/NAT-PMP | 设计完成（BT） | 失败不阻塞 |
| 浏览器 Cookie/请求头导入 | 计划 | P5；仅用户显式授权，不采集密码/支付信息 |
| Cloudflare/验证码/反爬 | Not planned | 不做绕过 |
| DRM（Widevine/PlayReady/FairPlay） | Not planned | 明确不做 |

## 5. 平台与分发

| 平台/形态 | 状态 |
| --- | --- |
| Linux x86_64 | **第一优先**；打包已通，插件待实现 |
| Linux arm64 | **第一优先**；打包已通，插件待实现 |
| Windows x86_64 | 次优先；打包已通，未签名 |
| Windows arm64 | 次优先；打包已通，未签名 |
| macOS universal | 次优先；打包已通，adhoc 未公证 |
| Android / iOS | 不做（当前） |
| Web / WASM | 不做（当前） |
| Linux 32-bit / Windows 32-bit | 不做 |
| Flatpak | 未做；当前 `.deb`/AppImage |
| 自动更新 | 未做；插件更新设计已包含 |
| Windows 代码签名 | **暂不推进**（Linux 优先；不买证书） |
| macOS 公证 | **暂不推进**（无免费路径；需 Apple Developer/赞助） |
| PAC 自动代理 | P2 Host 网络层 |
| 浏览器集成 | P5 扩展 + Native Messaging |
| 网盘插件 | P6 最后（Google Drive/OneDrive/百度/阿里） |

## 6. 存储与文件系统

| 场景 | 状态 |
| --- | --- |
| 单文件/多文件 | 设计完成 |
| 稀疏文件/预分配 | 设计完成 |
| 原子 rename/fsync | 设计完成 |
| 断点续传 | 设计完成 |
| FAT32 4 GiB 限制 | 需在 Host 检查并提示 |
| 网络盘/NFS/SMB | 尽力支持；不保证锁/原子性 |
| 磁盘满/只读/权限 | 设计完成（错误分类） |
| Windows 长路径/保留名/大小写 | 设计完成（路径守卫） |
| S3 multipart / WebDAV 属性 | 插件级；不是 Host 核心 |

## 6.5 计划内，但需要外部条件

| 能力 | 外部条件 | 计划 |
| --- | --- | --- |
| Windows 代码签名 | SignPath Foundation 免费 OSS 证书（需确认 Slint 许可/项目资历）；不买证书 | 暂缓（条件满足再说） |
| macOS 公证 | Apple Developer Program（$99/年）、Developer ID、notarytool；无免费公证 | 暂缓（需赞助/组织账号） |
| 浏览器集成 | Chrome/Edge/Firefox 扩展商店账号；本地扩展可先侧载 | P5 |
| Google Drive / OneDrive | OAuth 应用注册/审核、API 配额 | P6（最后） |
| 百度网盘 / 阿里云盘 | 开放平台/API 权限；遵守 ToS | P6（最后） |
| PAC / 自动代理 | 无外部条件 | P2 |

## 7. 明确不做

- 绕过 DRM、付费墙、登录、验证码、反爬。
- 私有客户端的专有加速/离线下载协议（如迅雷私有通道）。
- 未经用户明确同意抓取/导入浏览器 Cookie、密码或支付信息。
- 自动执行下载内容。
- 保证所有站点/所有 CDN/所有服务器的兼容性。
- 使用私有加速通道或绕过云盘风控/登录限制。

## 7.5 交付顺序

1. Linux 上的 Host/Plugin 骨架 + HTTP 插件（P0/P1）
2. Linux `.deb`/AppImage 真实下载联调与发布门禁
3. PAC/调度/限速（P2）
4. BitTorrent（P3）
5. FTP/HLS/DASH/Metalink（P4）
6. 浏览器集成 + 可选插件（P5）
7. 网盘插件（P6，最后）

> 签名/公证不在交付顺序里；Linux 优先，Windows/macOS 保持可构建、未签名/adhoc。

## 8. 如何判断“支持”

一个场景只有同时满足以下条件才算支持：

1. 有已签名的插件实现；
2. Host 能通过 `Probe` 识别并路由；
3. 有集成测试（真实本地服务器/种子/分片）；
4. 有明确的错误与恢复路径；
5. 在支持矩阵中标记为 Core/Optional/External，而不是“应该可以”。

矩阵之外 = 未支持。新增协议/站点 = 新增插件 + 测试 + 矩阵条目，不改 Host 核心。
