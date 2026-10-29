# 无障碍 + macOS 包补齐（第二波）实施定稿

## A. 无障碍（AccessKit）
- slint feature 加 `accessibility`（i-slint-backend-selector/accessibility，
  AccessKit 桥；Linux 走 atspi/zbus，无新增系统库）。
- 关键自定义控件补语义（Slint 1.18 可用属性：accessible-role/label/
  description/checkable/checked/item-*/action-default/value）：
  - RailButton（导航栏图标钮）：button + label（新增 label 属性）+ checked + action
  - NavRailItem：button + label + checked + action
  - FilterPill：button + checkable/checked + label + action
  - RowAction（任务行图标钮）：button + label（新增属性，调用点给
    「打开/重试/继续/暂停」「删除/取消」）+ action
  - TaskItem：list-item + label=任务名 + description=状态 + item-selected +
    action（打开详情）
  - SettingsNav：button + label + checked + action
  - Toast 动作按钮：button + label + action
  - ProgressTrack：progress-indicator + accessible-value 百分比
- 诚实边界：仓库无屏幕阅读器运行环境，验证到「feature 编译 + Xvfb 启动
  渲染无回归 + 语义属性静态审查」为止，真机 NVDA/Orca 验证记为外部步骤。
- 新 Slint 串 3 条（重试/继续/删除）进 en/ja/zh-TW po。

## B. macOS 包补齐（daemon/CLI/native-host/插件）
- package.yml macOS job：`cargo build --release --workspace`（双 arch），
  四个二进制 lipo 成 universal 进 Contents/MacOS；插件用
  `stage-plugins.py --merge-target-dir` 双 arch lipo 后放
  Contents/Resources/plugins。
- `default_plugins_dir`（aether-host）macOS 候选：exe 同级
  `../Resources/plugins`（bundle 布局）→ exe 同级 `plugins`（便携/开发）→
  /usr/lib → build/plugins → plugins。unix 非 mac 顺序不变。
- 验证：本机只能 check（macOS 目标不构建）；package 流水线 release/** 时
  首次实跑——文档如实标注。Info.plist 不变（CFBundleExecutable=aether）。
