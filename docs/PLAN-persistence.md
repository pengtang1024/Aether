# 持久化与可观测性（第三波）实施定稿

## A. 数据库在线备份 + 完整性检查
- rusqlite 开 `backup` feature（树内已有 crate，无新增依赖）。
- `TaskStore::backup_to(path)`：SQLite online backup API，daemon 运行中可安全
  复制（不阻塞写太久）；`TaskStore::integrity_check()`：PRAGMA integrity_check。
- daemon RPC：`backup {path}` / `db_check`；Client 封装 `backup()`/`db_check()`；
  CLI：`aether-cli remote backup <dest>` / `remote db-check`。
- 测试：内存库建任务 → 备份文件 → 重新打开断言任务与 integrity ok。

## B. 日志保留上限
- `append_log` 同事务剪枝：每任务仅保留最新 `TASK_LOG_LIMIT = 500` 行，
  防止话痨插件无限膨胀 DB；UI/CLI 读同一尾部。
- 测试：写 520 行 → 读回 500 行且最旧被删。

## C. 诊断包（凭据脱敏）
- `aether-cli diag <file|->`：`aether-diagnostics/1`，含版本/平台/daemon 版本/
  settings（脱敏）/任务计数（按状态）/插件清单（id/version/enabled/protocols），
  **不含 URL 与任务级 settings**。
- 脱敏规则：`rpc_secret`、`headers` → `<redacted>`；proxy.url 含 userinfo →
  `<redacted-userinfo>`；其余保留。单测覆盖。

## D. 安装器卸载清理
- Inno `[UninstallDelete]` 删除 `{app}\plugins` 与 `{app}\licenses`（安装器
  自有内容，用户手工加装的插件也一并清）；用户数据（配置/下载）不动。

## 验证
- 本机 e2e：daemon 起 → db-check=ok → backup 文件用 python sqlite3 打开
  （6 表 + integrity ok）→ set rpc_secret 后 diag 显示 `<redacted>`、16 插件。
- 门禁：fmt → workspace test → windows-gnu 交叉 check → push → CI → pin。
