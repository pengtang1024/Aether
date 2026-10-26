# 自动升级

> 目标：daemon 后台低频检查签名更新，校验产物后下载；AppImage 可原子替换并保留
> 回滚，`.deb` 交给包管理器/用户确认。升级清单和产物必须签名，不能只信任 HTTPS。

## 1. 签名模型

- 算法：Ed25519。
- 清单 `update.json` 原样字节 + detached `update.json.sig`（base64 原始签名）。
- 公钥内置/配置在 Host；私钥离线保存，绝不进仓库/CI 日志。
- 下载后校验 `sha256` 和清单签名，任一失败都不落盘为可执行更新。

签名工具：

```bash
# 私钥是 32 字节 seed 的 base64，只在本机/离线环境设置
AETHER_UPDATE_SECRET_KEY=... cargo run -p aether-update --bin aether-update-sign -- update.json
# 输出 update.json.sig 和 public key
```

## 2. 清单

```json
{
  "version": "0.2.0",
  "channel": "stable",
  "published_at": "2026-10-26T00:00:00Z",
  "notes_url": "https://github.com/pengtang1024/Aether/releases/tag/v0.2.0",
  "assets": {
    "linux-x86_64": {
      "url": "https://github.com/.../aether-0.2.0-linux-x86_64.AppImage",
      "sha256": "...",
      "size": 12345678,
      "kind": "appimage"
    },
    "linux-aarch64": {
      "url": "https://github.com/.../aether-0.2.0-linux-aarch64.AppImage",
      "sha256": "...",
      "kind": "appimage"
    }
  }
}
```

- `assets` 的 key 与 `aether_plugin_sdk::current_platform_key()` 一致。
- `kind`：`appimage` / `deb` / `zip` / `dmg`。
- 只接受 semver 大于当前版本的清单。
- 更新清单和 `.sig` 放在 release 的固定下载地址，例如
  `releases/latest/download/update.json`。

## 3. daemon 行为

配置：

```bash
# 环境变量或 systemd EnvironmentFile
AETHER_UPDATE_URL=https://.../update.json
AETHER_UPDATE_PUBKEY=<base64 32-byte public key>
AETHER_UPDATE_APPIMAGE=/home/user/Applications/Aether.AppImage
```

- daemon 启动后 30 秒做第一次检查，之后每 6 小时一次（不在下载/任务循环里轮询）。
- 先验签，再下载到 `<downloads>/.aether/artifacts/<version>/`，校验 size/SHA-256。
- `kind=appimage` 且配置了 `AETHER_UPDATE_APPIMAGE`：原子替换目标，旧文件保留为
  `<target>.old`；下次启动生效。失败可手工把 `.old` 复制回去。
- `kind=deb`：只下载并报告 `ready`，由用户/包管理器安装；daemon 不自行提权。
- 状态查询：

```bash
printf '%s\n' '{"id":1,"method":"update_status"}' \
  | socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/aether/daemon.sock
```

返回 `disabled / idle / up_to_date / ready / error`。

## 4. 发布接入

1. 构建平台产物（AppImage/deb/zip/dmg）。
2. 生成 `update.json`，填 URL/SHA-256/size/kind。
3. 离线/CI secret 中签名，得到 `update.json.sig`。
4. 把 `update.json`、`.sig` 和产物上传到同一 release。
5. 用旧版本执行一次 `check → download → apply` 验证；AppImage 替换后重启确认版本。
6. 保留上一版产物和签名，支持回滚。

## 5. 边界

- 不自动执行未经签名的脚本，不走 `curl | sh`。
- `.deb` 自动安装涉及 root/包管理器策略，首版不静默提权。
- Windows/macOS 的自动替换/签名/公证仍待后续实现。
- 更新服务器/镜像可用 `AETHER_UPDATE_URL` 切换；清单签名是信任根。
- 失去私钥时不能签新版本；公钥轮换需要发布一个带新公钥的版本。

## 6. 低消耗策略

- 更新检查：启动后 30 秒 + 每 6 小时一次，失败不重试风暴。
- daemon 空闲：阻塞在命令 channel（1 秒超时），不轮询插件进程。
- 下载中：100 ms 事件轮询。
- 空闲 5 分钟：卸载所有插件进程（registry/SQLite 保留），下次任务按需重启。
