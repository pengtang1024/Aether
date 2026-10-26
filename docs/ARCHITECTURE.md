# Aether 架构与界面重设计契约

> 状态：**方案已定稿，工程尚未实现**。  
> 当前 0.1.0 只有 Host UI mockup；所有下载能力都将以第一方插件形式实现。  
> 本文是产品/架构索引，也是隔壁界面重设计会话的契约；实现细节见链接文档。

## 1. 产品形态

**Aether = 管理面板（Host）+ 第一方下载插件。**

- Host 是用户看到的应用：任务管理、搜索/筛选、新建任务、详情、设置、插件管理、通知、日志。
- 下载协议不在 Host 里：HTTP、BitTorrent、FTP、HLS/DASH、Metalink 等全部是插件。
- 插件全部由我们开发、签名、版本化；插件化是为了独立崩溃域、独立更新、按需加载，
  不是为了第三方市场。
- Host 不链接 `reqwest`/`librqbit`/`suppaftp`/`m3u8-rs` 等协议库。

详细契约：

- [PLUGIN_ARCHITECTURE.md](PLUGIN_ARCHITECTURE.md)：Host/Plugin 职责、进程模型、IPC、
  manifest、生命周期、权限、设置/详情 schema、打包。
- [DOWNLOAD_ENGINE.md](DOWNLOAD_ENGINE.md)：各协议插件内部的算法与实现细节。
- [SUPPORT_MATRIX.md](SUPPORT_MATRIX.md)：支持分级、已覆盖/计划/不做的场景；矩阵外视为未支持。
- [SIGNING.md](SIGNING.md)：Windows Authenticode 与 macOS 公证的证书、secrets 和命令。
- [BROWSER_INTEGRATION.md](BROWSER_INTEGRATION.md)：浏览器扩展 + Native Messaging。
- [CLOUD_PLUGINS.md](CLOUD_PLUGINS.md)：Google Drive/OneDrive/百度/阿里等网盘插件（P6）。
- [LICENSING.md](LICENSING.md)：MIT 与 Slint 许可的边界、署名义务、各分发场景。
- [HOST_RUNTIME.md](HOST_RUNTIME.md)：Host 插件发现/监督/Probe 路由/最终落盘与 CLI。

## 2. 固定决策

| 决策 | 内容 |
| --- | --- |
| 产品形态 | 管理面板 + 第一方插件 |
| 插件隔离 | 一个插件 ID 一个独立进程，进程内多任务 |
| IPC | stdin/stdout + 长度前缀 MessagePack，文件字节不走 IPC |
| 任务状态 | Host 是唯一权威；插件只持有 opaque `plugin_state` |
| 调度/限速 | Host 发放预算；插件在预算内执行 |
| 存储 | Host 创建任务目录并授权 `staging/`；插件写入；Host 校验并原子落盘 |
| UI | Slint 只渲染 Host 数据；插件通过 settings/detail schema 提供配置与详情 |
| 平台 | **Linux 第一优先**（x64/arm64）；Windows x64/arm64、macOS universal 保持可构建，属次优先 |
| 代理 | Host 统一解析 PAC/系统/手动代理；插件只接收最终代理链 |
| 浏览器集成 | P5：扩展 + Native Messaging；用户显式授权，不抓密码/支付信息 |
| 网盘 | P6（最后）：OAuth/官方 API 插件；不做 Cookie 抓取/绕过风控 |
| 签名/公证 | **暂不推进**：Windows 未签名、macOS adhoc；除非获得免费 OSS 签名或赞助，不投入证书费用 |
| 移动端 | Android/iOS/Web 暂缓；不阻塞当前桌面重设计 |
| 发布 | 仅 `release/*` 分支打包；Host 包内置默认插件 |

## 3. 界面重设计契约

界面可以重做视觉、布局和组件，但必须为下面的 Host/插件模型留出位置。

### 3.1 必须出现的界面

1. **下载列表**
   - 任务名、来源、进度、实时速度、状态、插件徽标。
   - 搜索、状态筛选、批量操作、排序。
   - 新建任务入口（按钮 + FAB/快捷键）。
   - 空状态区分：无任务 / 无搜索结果 / 没有可用插件 / 插件被禁用 / 插件崩溃。
2. **新建下载**
   - URL/文件、保存路径、备注。
   - Host 探测后显示“将使用哪个插件”，允许手动切换（若多个插件声明支持）。
   - 插件专属选项由 `settings_schema` 生成，不在 Slint 里硬编码 BT/HTTP 字段。
   - 探测中、无法识别、需要安装/启用插件、插件版本不兼容等状态。
3. **任务详情**
   - 固定头部：文件名、插件/协议、状态、总进度。
   - 正文由插件的 `detail_schema` 生成：metrics / chunks / peers / logs / table / text。
   - Host 提供统一渲染器；不要为 BT 单独写死一套、再为 HTTP 另写一套。
   - 操作：暂停/继续/重试/打开目录/移除；失败时显示可诊断错误。
4. **插件管理（新增）**
   - 已安装/已启用/运行中/隔离中/可更新。
   - 版本、API 兼容性、声明的 schemes、资源占用。
   - 启用/禁用、安装/卸载、更新/回滚、查看日志、诊断包。
   - 默认插件与可选插件分组；禁用未使用插件以省资源。
5. **设置**
   - Host 设置与插件设置分组。
   - 插件设置由 schema 生成（bool/int/string/select/path/secret）。
   - 插件更新后 schema 迁移保留旧值。
6. **通知/日志/诊断**
   - 任务完成/失败/插件崩溃/需要更新；可跳转到对应任务或插件页。
   - 日志按任务/插件过滤；敏感信息脱敏。

### 3.2 数据契约（UI 侧）

任务行至少需要：

```
id, plugin_id, plugin_name, protocol,
name, subtitle, progress, progress_text,
speed, speed_label, status, status_text,
selected, actions
```

任务详情是**通用 section 列表**，而不是只有 BT 的固定字段：

```
TaskDetail {
  header: { name, plugin, protocol, status, progress }
  sections: [
    Metrics, Chunks, Peers, Logs, Table, Text, FileInfo
  ]
}
```

插件管理页：

```
PluginRow {
  id, name, version, api, host_compatible,
  enabled, running, health, protocols,
  memory, cpu, last_error, update_available
}
```

### 3.3 不能做的事

- 不要把 HTTP/BT/FTP 的协议逻辑或状态机写进 Slint。
- 不要为某个协议在 Host 里写死详情 UI；用 section schema + 通用渲染器。
- 不要让 UI 直接持有机密（Cookie/Token/代理密码）或文件句柄。
- 不要假设“所有任务都是 HTTP”；任务必须有 `plugin_id`。
- 不要把插件崩溃表现为整个应用崩溃；要有隔离/重试/禁用状态。
- 移动端暂缓：当前窗口仍是桌面优先（900×600 最小），不要为手机牺牲桌面信息密度。

### 3.4 视觉方向

- 继续 Material 3 深色、紧凑桌面工具；可以重做布局/动效/层级，但不要引入 WebView/Electron。
- 插件详情/设置必须能在现有 M3 组件体系内渲染。
- 插件徽标要克制：协议类型是辅助信息，不应压过任务名/进度。

### 3.5 Host 级集成界面

- **设置 > 网络**：代理模式（系统 / 手动 / PAC）、PAC URL 或文件、测试当前 URL 的代理、
  最近解析结果；PAC 由 Host 求值，插件不出现相关设置。
- **设置 > 浏览器**：接管下载开关、按文件类型/站点过滤、询问模式、Native host 状态、
  扩展安装引导；默认关闭，逐项授权 Cookie/Headers。
- **插件设置 > 网盘账号**（P6）：OAuth 登录、账号列表、配额、退出；token 不显示在 UI。
- **通知**：浏览器接管成功、插件崩溃/被隔离、插件需要更新、签名/公证状态按 release notes。


## 4. 当前实现状态

| 部分 | 状态 |
| --- | --- |
| Host UI（Slint） | 已有 mockup；隔壁会话正在重做视觉 |
| 模拟引擎 `src/engine.rs` | 仅演示；不是真实下载 |
| 插件管理器 / SDK / IPC | 已实现：`aether-plugin-sdk` + `aether-host` + `aether-cli`；尚未接 Slint UI |
| 协议插件 | HTTP、BT、FTP/FTPS、HLS/DASH、WebDAV、S3、SFTP、yt-dlp、thunder、IPFS、RTMP/RTSP、Google Drive/OneDrive/Dropbox、百度/阿里官方 API 共 16 个（均未接 UI）+ fake；ED2K 见 SUPPORT_MATRIX.md |
| 打包/CI/图标/发布 | Linux `.deb`/AppImage 已包含 16 个插件；Windows/macOS 仍只打包 Host |
| v0.1.0 Release | 已发布；内容仍是 UI mockup |

## 5. 实施顺序

1. **界面系统重设计**（隔壁会话）：先定 Host 的导航、列表、详情、插件页、设置页。
2. **P0 Host/Plugin 骨架**：PluginSupervisor、SDK、IPC、fake plugin，UI 用 fake 数据走通。
3. **P1 HTTP 插件**：真实分段/续传/校验；替换模拟进度。
4. **P2**：调度/限速/PAC/代理/插件管理页接线。
5. **P3**：BitTorrent 插件。
6. **P4**：FTP/HLS/DASH/Metalink 插件。
7. **P5**：浏览器集成、SFTP/WebDAV/S3、yt-dlp、ED2K、thunder、IPFS、RTMP。
8. **P6**：Google Drive/OneDrive/百度/阿里等网盘插件（最后）。

Linux 优先的具体含义：P0/P1 在 Linux 上先跑通并作为发布门禁；Windows/macOS 只保证
构建不回归，不因签名/公证阻塞发布。签名/公证的代码与文档保留，等免费/赞助条件满足再接。

## 6. 打包与发布

- Host 包必须内置默认插件（至少 HTTP；BT/媒体可随包懒启动）。
- 每个平台/架构构建 Host + 插件二进制；CI 矩阵与现有兼容性矩阵一致。
- 插件独立版本、签名、更新/回滚；Host 版本与插件 API 做兼容检查。
- **Linux `.deb`/AppImage 是发布门禁**；Windows/macOS 保持未签名/adhoc，不阻塞 Linux 发布。
- Linux 包必须包含 `LICENSE`、`THIRD_PARTY_NOTICES.md`、`LICENSES/Slint-Royalty-free-2.0.md`。
- 签名/公证暂缓；如以后获得免费 OSS 签名或赞助，再按 [SIGNING.md](SIGNING.md) 接入。
- 发布仍只在 `release/*` 分支触发；[RELEASING.md](RELEASING.md)。
- 平台基线见 [COMPATIBILITY.md](COMPATIBILITY.md)。

## 7. 待定问题

- Linux 发行版打包（Flatpak/apt 仓库/GPG 签名）的优先级与维护成本。
- 浏览器扩展商店账号与审核周期（Chrome/Edge/Firefox）。
- 云盘 OAuth 应用注册/审核与 API 配额（P6）。
- 插件签名 release key 的保管与轮换。
- 插件更新通道：随 Host 包更新，还是独立签名包增量更新（当前方案：两者都支持）。
- 插件沙箱强度：首版进程隔离 + 目录授权；后续 Linux Landlock/Windows Job Object/macOS sandbox。
- 移动端时机：等 Host/插件 API 稳定后再做响应式 UI + 移动打包。
