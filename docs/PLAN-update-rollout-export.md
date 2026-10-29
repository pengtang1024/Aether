# 更新灰度 + 设置/任务导出导入（第一波）实施定稿

## A. 灰度发布（静态清单，无服务器）
- `UpdateManifest` 新增 `rollout: u8`（serde default 100；>100 视为协议错误）。
- `UpdateClient::check()` 在版本比较后过灰度闸门：
  - bucket = sha256(hostname\nversion)[0] % 100（hostname 取 HOSTNAME/COMPUTERNAME，
    缺省 "aether"）；同一机器同一版本恒定，无随机、无额外依赖。
  - rollout >= 100 → 全量；0 → 无人；其余按 bucket < rollout。
  - 只作用于主程序更新；`check_manifest()`（插件通道）不受影响。
- **运维语义**：清单签名覆盖 rollout，调灰度要重新签名（离线 key 在发布时用），
  UPDATE.md 写明。
- 测试（aether-update 单测）：0/100 边界、确定性、>100 报错、缺省 100 兼容。

## B. 清单生成脚本 + 发布流水线
- `scripts/make-update-manifest.py`：扫 dist 产物 → update.json
  - deb（amd64/arm64 → linux-x86_64/aarch64）、AppImage（linux-*）、
    windows portable zip（windows-x86_64/arm64）、macOS dmg（macos-universal）；
    setup.exe 不进清单（zip 已覆盖同平台，清单每 key 只允许一个资产）。
  - 字段：version/channel/published_at/notes_url/rollout/assets{url,sha256,size,kind}；
    URL 默认 tag 固定地址 `releases/download/v<ver>/<file>`；版本不一致直接报错。
- package.yml `publish` job：SBOM 之后生成清单；有 `AETHER_UPDATE_SECRET_KEY`
  secret 才签名（aether-update-sign），否则 `::warning::` 并**删除未签名清单**
  （未签名清单不得随发布走）；校验和步骤覆盖 update.json(.sig)。

## C. 设置/任务导出导入（aether-cli）
- `aether-cli export <file|-> [--socket]`：JSON
  `{format:"aether-export/1", settings:{daemon settings}, tasks:[{url, plugin_id,
  priority, state, target_dir}]}`；**不导出任务 settings（可能含 Cookie/凭据）**，
  文档明说。
- `aether-cli import` 自动识别：以 `{` 开头 → 导出文档；否则沿用 URL 行列表。
  - 任务逐条 add_with_priority（--dir 覆盖 > 导出 target_dir；强制导出 plugin_id）。
  - `--settings` 才应用导出里的 daemon settings（默认不静默改配置）。
- 测试（cli bin 单测）：导出文档解析/字段、错误 format 拒绝、URL 列表不受影响。

## D. 文档
- UPDATE.md：灰度字段与签名语义、清单生成/签名流水线、setup.exe 不进清单说明。
- README（CLI 用法若有）+ CHANGELOG + ROADMAP §4.9（两件事的落地记录）。
