# 设置页逐项审查

> 目的：回答「设置页每个控件是不是真的」。每行给出控件、真实落点与判定，
> 并显式记录**做不到**的边界（只读、由启动配置决定、由外部条件决定），
> 避免再出现点了没反应或显示假值的控件。
>
> 最后核对：2026-10-30（main @ `1c6ed4a` 之上，设置页更新波次）。
> 控件清单位于 `ui/pages/settings.slint`（页面壳）与 `ui/pages/settings/panels_*.slint`
> （各分类面板），处理器位于 `src/wiring/settings.rs` 的 `on_settings_changed`、
> `src/wiring/support.rs` 的 `on_support_action` 与 `src/wiring/plugins.rs` 的
> `on_plugin_action`。

状态图例：

- **真实**：控件值送达实际执行方，重启/下次生效等延迟会明确提示。
- **只读**：值来自 daemon 或操作系统，界面只展示，不提供假编辑。
- **外部**：功能真实，但依赖启动配置或系统能力；不满足时界面明确说明。

## 0. 交互约定（2026-11-04 起）

设置页按「零配置可跑」组织，避免把实现细节暴露成输入框：

1. **默认即推荐**：不打开设置也能正常下载。数量类一律给档位下拉
   （同时下载数 3/5/10/16、每任务连接 2/4/8/16、重试 0/3/5/10），
   带宽类给滑杆（全局 0–10240 KB/s、BT 上传 0–4096 KB/s，0 为不限速）。
2. **输入框只出现在高级区**：`SectionCard` 的 `collapsible` / `expanded`
   属性把低频项收进默认收起的「高级」卡片（网络 DNS/UA/请求头、BT 端口/
   屏蔽列表/Tracker、ffmpeg 路径、插件来源策略/固定版本/安装、高级页的
   RPC 与只读路径）。搜索命中时卡片自动展开，收起不影响可发现性。
3. **代理地址按需出现**：只有代理模式选「手动」时才显示地址输入框。
4. **文本输入必须写清格式**：保留的文本框都有 placeholder 示例；提交时
   由 `src/wiring/settings.rs` 的 `validate_text` 做格式校验，错误经
   `SettingsValidation` 全局显示在字段下方（不是静默丢弃），合法值才下发
   daemon。
5. **持久化不变**：仍只写 `settings.json` 既有 key 与 `UiPrefs`，不新增
   键、不改 daemon 读取路径；折叠状态纯 UI 属性，不入盘。
6. **档位外取值不会让下拉框空白**：旧配置（如 `connections=6`、
   `retry_max=7`，或不在预设内的语言/主题/代理模式）会在档位后追加一行
   并自动选中；该行只回显真实值，选中它不写回配置，只有用户主动选择档位
   才下发。数值档（`max_concurrent` / `connections` / `retry_max`）追加的
   是 `自定义（6）`；枚举档（起始页/完成后/强调色/语言/主题/代理模式/IP
   协议）的存储值只是内部编号，追加行写成 `未知选项`，不把 7 或 `fr`
   暴露成界面文案。值回到档位内时该行消失；
   若原值本身越界（如并发 0、连接 >16），在字段下方给出内联提示但不修正。
   实现见 `src/settings_presets.rs`，插件 schema 的 `select` 越界值同样保留。

## 1. 常规（General）

| 控件 | 落点 | 状态 |
| --- | --- | --- |
| 启动页面 | `src/app.rs` `start-page` → `src/prefs.rs` 的 `UiPrefs.start_page`（写入在 `src/wiring/settings/prefs.rs`），启动时 `page = match prefs.start_page` | 真实 |
| 开机自启 | `autostart::set_enabled`（XDG / HKCU Run / LaunchAgent，条目带 `--silent`，启动时 `refresh()` 自愈）；OS 拒绝时开关回弹并提示；无托盘时静默启动回退为显示窗口 | 真实 |
| 关闭窗口行为 | 固定行为说明：有托盘收进托盘，无托盘退出（`tray` 模块决定） | 只读（说明卡） |
| 默认下载目录 | daemon 的 `stats.download_dir`，由 daemon 启动参数 `--dir` 决定 | 只读 |
| 检测下载链接 | `clipboard_control.set_enabled` + `UiPrefs.clipboard_watch`；无后端时禁用开关并说明 | 真实 / 外部 |

## 2. 下载（Download）

| 控件 | 落点 | 状态 |
| --- | --- | --- |
| 同时下载数 | `set_settings {"max_concurrent"}` → daemon 调度器 | 真实 |
| 每任务连接数 | `set_settings {"connections"}` → 插件下载引擎（HTTP 上限 16） | 真实 |
| 重试次数 | `set_settings {"retry_max"}` → daemon 自动重试调度 | 真实 |
| 全局限速开关 | `set_settings {"max_speed_bps"}`，开关=0 / 滑块=KB·1024 | 真实 |
| 限速滑块 | 同上；未开启全局时只记录并提示 | 真实 |
| 完成后动作 | `UiPrefs.after_done`，完成事件里 `open_path` 打开文件夹/文件 | 真实 |
| 完成通知 | `UiPrefs.notify_done` + `send_notification` | 真实 |
| 失败通知 | `UiPrefs.notify_fail` + `send_notification` | 真实 |

## 3. 网络（Network）

| 控件 | 落点 | 状态 |
| --- | --- | --- |
| 代理模式（系统/直连/手动/PAC） | `Backend::set_proxy` → `set_settings {"proxy"}`，插件侧 `aether_host::proxy` | 真实 |
| 代理地址 | 手动模式下编辑即下发；空地址拒绝切到手动 | 真实 |
| PAC | 需要 daemon 启动配置 `AETHER_PAC_URL`；选择时回弹到实际模式并说明 | 外部 |
| DNS | `set_settings {"dns"}` → 插件 `NetworkConfig`，空闲插件重启后生效 | 真实（延迟生效） |
| IP 协议 | `set_settings {"ip_mode"}`，同上 | 真实（延迟生效） |
| User-Agent | `set_settings {"user_agent"}`，空值恢复默认 | 真实（延迟生效） |
| 默认请求头 | `set_settings {"default_headers"}`，HTTP 族插件读取 | 真实（延迟生效） |
| FTP 主动模式 | `set_settings {"ftp_passive"}`，FTP 插件读取 | 真实（延迟生效） |

## 4. BT

| 控件 | 落点 | 状态 |
| --- | --- | --- |
| 监听端口 | `set_settings {"bt_listen_port"}`，1–65535 校验 | 真实（空闲重启） |
| DHT | `set_settings {"bt_dht"}` | 真实（空闲重启） |
| 上传限速 | `set_settings {"bt_upload_bps"}`（滑块 KB/s） | 真实 |
| 屏蔽列表 | `set_settings {"bt_blocklist"}` | 真实（空闲重启） |
| Tracker | `set_settings {"bt_trackers"}`，附加到之后的任务 | 真实 |

BT 键由 `aether-plugin-bt` 消费（`crates/aether-plugin-bt/src/plugin.rs` 与
`.../bt.rs` 的 `NetworkConfig` 字段），不是仅存盘的占位值。

## 5. 媒体与浏览器（Media）

| 控件 | 落点 | 状态 |
| --- | --- | --- |
| HLS / DASH 直链 | 能力说明，指向 `aether-plugin-hls` | 只读（说明卡） |
| 任务转封装 | 能力说明，`transcode_to` → `aether-plugin-media`（`-c copy`） | 只读（说明卡） |
| ffmpeg 路径 | `set_settings {"ffmpeg_path"}` → daemon 注入插件，之后开始的任务生效 | 真实 |

## 6. 插件（Plugin）

| 控件 | 落点 | 状态 |
| --- | --- | --- |
| 启用/停用 | `Backend::set_plugin_enabled` → `set_plugin_enabled` RPC；有运行任务时 daemon 拒绝并提示 | 真实 |
| 更新全部 | `plugin_update_all` RPC（签名校验 + tar.gz 解包防护） | 真实 |
| 自动更新插件 | `set_settings {"plugin_auto_update"}` → daemon 每 6 小时 pass | 真实 |
| 安装插件包 | `install_plugin` RPC；签名/哈希/占用结果经 notice 回传 | 真实 |
| 卸载 | `uninstall_plugin` RPC；运行任务时拒绝，删除包与回滚备份并清禁用标记 | 真实 |
| 回滚 | `rollback_plugin` RPC；上一版重新走验签/哈希，当前版成为新回滚点 | 真实 |
| 权限行 | `plugins` RPC 的 `capabilities` + `blocked`；握手强制与 manifest 一致 | 真实（展示/策略） |
| 固定插件版本 | `set_settings {"plugin_pins"}`；更新/自动更新跳过，手动安装不受限 | 真实 |
| 外部进程闸门 | `set_settings {"plugin_external_process"}`；拒绝 spawn 声明 external-process 的插件，空闲实例立即停止 | 真实 |
| 重新扫描 | `rescan_plugins` RPC | 真实 |

## 7. 计划（Schedule）

| 控件 | 落点 | 状态 |
| --- | --- | --- |
| 计划开关 | `set_settings {"schedule_on"}`；未填时间时回弹开关 | 真实 |
| 开始/结束时间 | `set_settings {"schedule_start"|"schedule_end"}`，daemon 每 15s tick（支持跨午夜） | 真实 |

## 8. 外观（Appearance）

| 控件 | 落点 | 状态 |
| --- | --- | --- |
| 语言 | `UiPrefs.lang` + `i18n::set_language` + `select_bundled_translation`（即时切换） | 真实 |
| 主题 | `save_theme_mode` + `UiPrefs`，Slint 即时换色 | 真实 |
| 主色预设 | `UiPrefs.accent` + `apply-appearance()`（6 预设） | 真实 |
| 动效 | `UiPrefs.motion` → `Aether.motion-* = 0ms` | 真实 |
| 流线背景 | `UiPrefs.flow` → `flow-enabled` | 真实 |

## 9. 高级（Advanced）

| 控件 | 落点 | 状态 |
| --- | --- | --- |
| 数据库路径 | `stats.data_dir`（daemon 真实数据目录） | 只读 |
| 插件目录 | `stats.plugins_dir` | 只读 |
| RPC 监听地址 | `set_settings {"rpc_listen"}` → `Settings.rpc_listen`，daemon 启动时 bind；首次启动默认 `127.0.0.1:6800`，空字符串关闭 | 真实（重启生效） |
| RPC 访问令牌 | `set_settings {"rpc_secret"}` → `rpc_secret`（0600 文件），首次启动自动生成 32 位令牌；热轮换立即生效 | 真实 |
| 随机令牌 | 设置页按钮调用 `generate_rpc_secret()` 填入字段，点「应用」写入 daemon（不自动保存） | 真实 |
| 关闭远程控制 | `set_settings {"rpc_secret": "", "rpc_listen": ""}` 原子提交；不会留下无鉴权监听，已绑定监听保留旧令牌直到重启 | 真实 |
| RPC 状态 | 由 `rpc_listen` + 是否设置令牌生成，明确写「重启 daemon 后按此地址监听」 | 只读 |
| 重置界面偏好 | `UiPrefs::default()` + 立即恢复剪贴板开关、语言（zh）、主题（跟随系统）、主色/动效/流线/启动页/完成后动作/通知 | 真实 |

## 10. 关于与支持（About）

| 控件 | 落点 | 状态 |
| --- | --- | --- |
| 检查更新 | `Backend::check_update` → daemon `update_check`（后台线程，不阻塞下载主循环） | 真实 / 外部 |
| 更新源 | 只读说明：`--update-url` + 更新公钥；无源时显示「未配置更新源」 | 只读 |
| 自动下载更新 | `set_settings {"app_auto_update"}`；关闭后只报告 `available`，不下载 | 真实 / 外部 |
| 更新状态行 | `stats.update` → `update_status_text`（idle/checking/up_to_date/available/ready/error） | 真实 |
| 版本说明 | `open_url` → `CHANGELOG.md`；打开器失败时提示 URL | 真实 |
| 文档 | `open_url` → 仓库 `docs/` | 真实 |
| 社区 | `open_url` → GitHub Discussions | 真实 |
| 问题反馈 | `open_url` → GitHub Issues | 真实 |
| 许可 | `open_url` → `LICENSE` | 真实 |

## 11. 明确做不到的边界

1. **更新源与签名公钥**只来自 daemon 启动参数
   （`--update-url` / `AETHER_UPDATE_URL`、`--update-key` / `AETHER_UPDATE_PUBKEY`）。
   界面不提供「稳定版/测试版」切换：后端没有通道概念，做一个假 ComboBox
   只会骗人。无源时 `update_check` 返回 `disabled`，About 面板照实显示。
2. **下载目录**由 daemon 启动参数决定；`Settings` 里没有 `download_dir`
   键，界面因此只展示真实路径，不提供无效输入框。
3. **PAC** 需要 `AETHER_PAC_URL`/`--pac-url`；界面选择时回弹到 daemon
   实际使用的模式，并说明用启动参数切换，避免界面与 daemon 不一致。
4. **DNS / IP 协议 / User-Agent / 请求头 / FTP / BT 端口等**写入
   `settings.json` 后由插件在**下一次启动**读取；正在运行的插件不会热改，
   提示文案明确写了「空闲插件重启后生效」。
5. **语言**：UI 串四种语言齐全；Rust 侧字符串表只有中/英完整，ja/zh-TW
   缺行回退中文（`i18n::tr` 的既有契约），切换提示里已说明。
6. **签名/公证、商店上架、云盘 OAuth、屏幕阅读器真机、macOS `.app` 实跑**
   依旧需要外部账号或硬件，不在设置页伪装成可用。

## 12. 本波修掉的名不副实项

| 原控件 | 问题 | 修复 |
| --- | --- | --- |
| 更新通道（稳定版/测试版） | 无后端概念，纯假下拉 | 改为只读「更新源」说明 |
| 自动下载更新 | 开关永远 `checked: true`，点击只弹提示 | 新增 `app_auto_update` 设置，更新循环每轮重读 |
| 检查更新 | 固定 toast「已是最新 0.1.0」 | 新增 `update_check` RPC，后台执行，状态经 `stats.update` 回传 |
| 文档/社区/反馈/版本说明/许可 | 只弹「原型」toast | 真正 `open_url`，失败时给出 URL |
| PAC 选项 | 只提示不回弹，界面与 daemon 不一致 | 回弹到实际模式并说明启动配置决定 |
| 重置界面偏好 | 不重置语言与主题 | 同时恢复 zh 与跟随系统主题 |
| 计划/开机自启等开关 | `checked:` 单向绑定，Rust 回弹或镜像到属性时开关不跟着变（计划任务未填时间仍显示「已开启」） | 全部改为 `checked <=>` 双向绑定；实测计划开关会回弹、开机自启失败会回弹 |
| 远程控制（RPC） | 设置页完全没有入口，只有 CLI/设置文件可用 | 高级页新增监听地址、访问令牌、关闭远程控制、应用与实时状态行；实测令牌错误被拒、正确令牌可 `remote stats` |
