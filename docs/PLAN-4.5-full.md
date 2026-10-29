# 4.5 托盘 + i18n 全量（设计定稿，中文优先）

## A. 托盘（Linux StatusNotifier via zbus）
- zbus 已在树内（slint→i-slint-backend-winit→zbus 5.19），直接依赖零新增树。
- 新 `src/tray.rs`：session bus 上注册 StatusNotifierItem（/StatusNotifierItem,
  org.kde.StatusNotifierItem）+ 向 StatusNotifierWatcher 注册；菜单 org.kde.DBusMenu
  （显示主窗口/隐藏主窗口/退出）；Icon 主题名或 pixmap；Activate → 显示窗口。
- 平台策略：Linux 走 zbus；Windows/macOS 托盘留诚实说明（Roadmap 本就 Linux 第一）。
- 窗口控制：Slint Window set_visible?（编译期验证）；隐藏=快速唤起语义。
  关闭窗口仍退出进程（不做 close-to-tray，Roadmap 说语义很轻）。
- 失败降级：DBus 不可用（headless）→ 托盘不出现，日志说明，主程序照常。

## B. i18n（中文第一语言）
- 语言存 UiPrefs.lang: "zh"(默认) | "en"；设置→外观或通用面板加语言切换。
- Slint 侧：源串保持中文不动 → `@tr("…")` 包裹；build.rs 改
  `slint_build::compile_with_translations`，translations/zh-Hans.ftl 与
  en.ftl（en.ftl = 中文键→英文译文；zh 源即默认，zh.ftl 可为空）。
  未迁移的串仍走源串（中文）→ 混合状态可编译、可分批。
- Rust 侧：`src/i18n.rs` 字符串表（key → {zh, en}）+ `tr(key)` 按
  UiPrefs.lang 选择；迁移 toast/notice/设置提示等到表。
- 范围：全部 UI slint 页面 + Rust 提示串（以 grep 中文全量清点）。
- 诚实验收：`@tr` 覆盖率脚本（grep 中文无 tr 包裹的残留=0）+ Xvfb 截图
  双语言对比 + 设置里语言切换即时生效。

## 提交顺序
1. 托盘波次（zbus tray + 设置无新控件，行为级：菜单唤起/隐藏/退出）
2. i18n 框架波次（build 翻译管道 + UiPrefs.lang + 设置项 + 迁移全量串）
