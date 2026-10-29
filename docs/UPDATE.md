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

# 插件包签名（manifest 内嵌 signature，同把 release key）：
# 重算 files 哈希并回填 canonical 签名；文件改动后重跑即可
AETHER_UPDATE_SECRET_KEY=... cargo run -p aether-update --bin aether-plugin-sign -- <package-dir>
```

插件 manifest 的签名载荷是 canonical bytes（去掉 `signature` 键后的
`serde_json::Value` 序列化），`files` 映射在载荷内 → 包内文件被一并覆盖。

## 2. 清单

> 插件条目（4.6b）：清单可含 `plugins` 映射（插件 id → `{version, url,
> sha256, size?}`），与主程序资产同处一签——下载按 sha256 校验，解包后再
> 验包内 manifest 签名（`aether-plugin-sign` 生成）。

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

配置（systemd 用户单元读取 `~/.config/aether/daemon.env`，模板见
`packaging/daemon.env.example`）：

```bash
AETHER_UPDATE_URL=https://github.com/pengtang1024/Aether/releases/latest/download/update.json
AETHER_UPDATE_PUBKEY=gmTgGxsoxi9dyYiWQ+mlqeqx4gDNnMwQ+P0jwNbsa+w=
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

### 4.1 密钥保管

正式发布密钥（2026-10 生成）：

| 项 | 位置 |
| --- | --- |
| 私钥（base64 32 字节 seed） | `~/.ssh/aether-update-signing.key`，权限 `0600` |
| 公钥 | `~/.ssh/aether-update-signing.pub`，同时写进 `packaging/daemon.env.example` |
| 公钥值 | `gmTgGxsoxi9dyYiWQ+mlqeqx4gDNnMwQ+P0jwNbsa+w=` |

- 私钥**不进仓库、不进 CI、不贴进聊天/日志**。备份到离线介质或密码管理器，
  丢失后无法为已发布用户签名新版本（只能发一个带新公钥的版本）。
- 换机器时把私钥文件拷到新机器的 `~/.ssh/`（`chmod 600`）即可；公钥由签名
  工具从私钥推导，可随时核对是否一致。
- 签名前先确认 `~/.ssh/aether-update-signing.pub` 与工具输出的 `public key:`
  相同，再上传签名。

### 4.2 发布步骤

1. 构建平台产物（AppImage/deb/zip/dmg），`release/**` 分支由 CI 打包成 draft release。
2. 从 draft release 里取 Linux AppImage 的 **下载 URL、字节大小、SHA-256**。
3. 生成 `update.json`（见 §2），`assets` 的 key 用
   `aether_plugin_sdk::current_platform_key()`（如 `linux-x86_64`、
   `linux-aarch64`）。
4. 本机签名（私钥只在这一步读取）：

   ```bash
   AETHER_UPDATE_SECRET_KEY="$(cat ~/.ssh/aether-update-signing.key)" \
     cargo run -q -p aether-update --bin aether-update-sign -- update.json
   # 打印的 public key 必须等于上面的公钥
   ```

5. 把 `update.json`、`update.json.sig` 上传到同一个 release；清单固定地址
   `https://github.com/pengtang1024/Aether/releases/latest/download/update.json`
   （draft 期间该地址不可用，先上传附件，发布后再验证）。
6. 用旧版本执行一次 `check → download → apply` 验证；AppImage 替换后重启确认版本：
   `aether-daemon` 的 `update_status` 应为 `ready`，替换后 `aether --version` 为新版本。
7. 保留上一版产物和签名，支持回滚（`<target>.old`）。

## 5. 边界

- 不自动执行未经签名的脚本，不走 `curl | sh`。
- `.deb` 自动安装涉及 root/包管理器策略，首版不静默提权。
- Windows/macOS 的自动替换/签名/公证仍待后续实现。
- 更新服务器/镜像可用 `AETHER_UPDATE_URL` 切换；清单签名是信任根。
- 失去私钥时不能签新版本；公钥轮换需要发布一个带新公钥的版本。

## 6. 发布流水线与灰度

发布分支的 `publish` job 在 SBOM 之后自动生成清单：

```bash
scripts/make-update-manifest.py --dist artifacts --version "$VERSION" --out artifacts/update.json
```

- 扫描产物：`.deb`（amd64/arm64）、AppImage、Windows 便携 zip、macOS dmg；
  **每个平台 key 只收一个资产**，同平台 AppImage 优先于 `.deb`（可原地替换），
  `setup.exe` 不进清单（便携 zip 已覆盖同 key，安装器仍是发布资产）。
- 产物文件名里的版本必须等于 `--version`，否则脚本直接失败。
- 清单 URL 用 tag 固定地址（`releases/download/v<ver>/<file>`）；清单本体发布在
  `releases/latest/download/update.json`。
- **签名**：只有配置了仓库 secret `AETHER_UPDATE_SECRET_KEY` 才签名；没配置时
  打 `::warning::` 并**删除未签名清单**——未签名清单不得随发布流出。
- 灰度：清单新增 `rollout`（0-100，缺省 100）。daemon 用
  `sha256(hostname + version) % 100` 做稳定分桶，同一机器同一版本恒定；
  `rollout=0` 无人、`100` 全量。**签名覆盖 rollout**，调灰度需要重新签名
  （发布时用离线 key 重跑 `aether-update-sign` 并替换附件）。
- 插件更新走同一清单的 `plugins` 字段（4.6b），灰度只作用于主程序。

## 7. 低消耗策略

- 更新检查：启动后 30 秒 + 每 6 小时一次，失败不重试风暴。
- daemon 空闲：阻塞在命令 channel（1 秒超时），不轮询插件进程。
- 下载中：100 ms 事件轮询。
- 空闲 5 分钟：卸载所有插件进程（registry/SQLite 保留），下次任务按需重启。
