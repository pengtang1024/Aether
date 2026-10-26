<p align="center"><img src="assets/logo-icon-128.png" width="64" alt="Aether logo"></p>

# Aether

**原生 Material 3 下载管理器 · Rust + Slint · 无 WebView**

Aether 是一个桌面下载管理器的界面原型（UI mockup），交互与视觉参考 Motrix，但完全用
Rust 与 [Slint](https://slint.dev) 绘制：没有 Electron、没有 Chromium。除品牌 logo 外，
窗口里的图标、进度条、折线图和背景流线全部是矢量渲染。

界面严格遵循 Material 3：深色主题、`#6750A4` 品牌主色、M3 形状与字号阶梯、柔和的环境
阴影，以及适合桌面工具的紧凑留白。

---

## 界面预览

### 下载列表

虚拟化任务表：文件名与来源、进度、实时速率、状态标签、行内暂停/继续与取消操作。
顶栏包含搜索、新建下载按钮与状态筛选 chip；右下角是新建任务 FAB。

![下载列表](docs/screenshots/home.png)

### 新建下载

URL、保存路径（带下拉选择）、分片数、限速滑块与备注。打开后自动聚焦地址栏，Enter
提交、Esc 取消；URL 为空时主按钮保持 M3 禁用态。确认后任务会立即插入列表并开始推进。

![新建下载](docs/screenshots/dialog.png)

### 任务详情抽屉

固定头部展示文件徽标、状态与整宽进度条；下方依次是最近 60 秒实时速度折线图、分片进度
网格与图例、紧凑的文件信息卡、日志预览和 BT 节点列表。点击任务行任意空白处打开，点击
遮罩或右上角关闭。

![任务详情](docs/screenshots/drawer.png)

### 设置

通用、网络限速、BT 配置、外观、通知五组 M3 卡片，包含开关、输入框、下拉框与滑块。

![设置](docs/screenshots/settings.png)

---

## 运行

前置条件：

- Rust 1.92 或更新版本
- Linux 桌面（X11 或 Wayland）
- OpenGL 驱动；无 GPU 时 Slint 会回退到软件渲染

```bash
cargo run --release
```

首次构建需要编译 Slint 与整套 Material 组件，耗时略长；之后的增量构建在数秒内完成。

### 开发用启动参数

调试某个界面状态时，不必手动点击：

```bash
AETHER_VIEW=settings cargo run   # 直接打开设置页
AETHER_VIEW=dialog   cargo run   # 直接打开新建下载对话框
AETHER_VIEW=drawer   cargo run   # 打开一个正在做种/下载的任务详情
```

### 截图脚本

`scripts/shot.sh` 会在私有 Xvfb 显示（默认 `:97`）上启动应用并抓图，不打扰当前桌面：

```bash
./scripts/shot.sh /tmp/aether.png 6          # 输出路径、等待秒数
./scripts/shot.sh /tmp/aether.png 6 1200 800 # 还可指定画布尺寸
AETHER_VIEW=drawer ./scripts/shot.sh /tmp/drawer.png
```

依赖 `Xvfb` 与 ImageMagick 的 `import`。窗口默认 1040×700，可在 900×600 以上自由缩放；
脚本默认画布与窗口一致。

### 打包与安装

发布包由 GitHub Actions 生成，**只有 `release/*` 分支会触发打包**；分支名必须与
`Cargo.toml` 的版本一致。

```bash
git switch -c release/0.1.0
git push -u origin release/0.1.0
```

同一套源码在 CI 中构建全平台产物：

| 平台 | 产物 | 说明 |
| --- | --- | --- |
| Linux x86_64 | `.deb` + `.AppImage` | `.deb` 安装到 `/usr`；AppImage 可移动 |
| Windows x86_64 | `setup.exe` + portable `.zip` | 安装器含开始菜单/桌面快捷方式 |
| macOS universal | `.dmg` | Apple Silicon + Intel；未签名，需右键打开 |

所有产物会附加到一个 **draft GitHub Release**（tag `v<version>`），确认后发布即可。构建只在
release 分支进行，`main`/feature 提交不会产生包。每个包都必须携带 `LICENSE`、
`THIRD_PARTY_NOTICES.md` 与 `LICENSES/Slint-Royalty-free-2.0.md`；发布前由
`make check-licenses` 和 `scripts/check-license-bundles.py` 检查实际产物。

Linux 本地验证：

```bash
make check   # desktop-file-validate + AppStream 校验
make deb     # dist/aether_0.1.0_amd64.deb
sudo apt install ./dist/aether_0.1.0_amd64.deb
```

`.deb` 在 Ubuntu 24.04 上构建，要求 glibc 2.39+；macOS 包为 universal（arm64 + x86_64），
但未做 Apple notarization。Windows 安装器由 Inno Setup 生成，便携版解压即用。

### 应用图标规范

- 画布：GNOME HIG 的 128×128 名义尺寸；仓库导出 32/48/64/128/256/512，安装到 hicolor。
- 底板：Ubuntu Yaru 方形图标按 224/256（87.5%）绘制，256 画布外缘 2px 描边；对应 512
  画布即 448px 底板、4px 描边。100% 铺满会明显大于其他 Dock 图标，75% 又会偏小。
- Dock 使用 48px 资产；桌面、Dock 和窗口图标使用浅色半透明磨砂底，导航栏使用去底版本。
- 图标名与 `.desktop` 的 `Icon` 都使用 app id `aether`，并安装到 `hicolor/{32,48,64,128,256,512}x…/apps/`。
- 发布包由 `.deb` 安装到 `/usr`；AppStream 组件 ID 使用反向域名
  `io.github.pengtang1024.Aether`。
- GNOME HIG 通用模板更保守（104/128 底板）；Ubuntu Yaru 的方形底板更大，本应用在 Ubuntu
  Dock 中按 Yaru 的比例对齐。

---

## 项目结构

```
.
├── build.rs                 # 注册 "material" 样式、include 路径
├── src/
│   ├── main.rs              # UI 胶水层：过滤/搜索、行 diff、回调、200 ms 定时器
│   ├── engine.rs            # 模拟下载引擎：种子数据、速率抖动、完成/做种迁移、图表路径
│   └── model.rs             # Task / Status 领域模型与格式化
├── ui/
│   ├── app.slint            # 窗口外壳、导航栏、顶栏、筛选 chip、FAB、覆盖层
│   ├── theme.slint          # Aether 设计令牌（surface / brand / 字号 / 圆角）
│   ├── icons.slint          # SVG 图标注册表（33 个）
│   ├── widgets.slint        # 自有组件：chip、导航项、搜索框、速度图、分片网格、流线背景
│   ├── model.slint          # 与 Rust 共享的结构体
│   ├── pages/
│   │   ├── downloads.slint  # 任务表与空状态
│   │   ├── settings.slint   # 五组设置卡片
│   │   ├── new-download.slint
│   │   └── task-drawer.slint
│   └── material/            # vendored Slint 1.18.1 Material 3 组件库
├── assets/
│   ├── icons/               # 单色描边 SVG（33 个），使用时 colorize
│   ├── logo.png                 # 原始 logo（带浅色底板）
│   ├── logo-transparent-128.png # 导航栏使用（去背景）
│   ├── logo-transparent-512.png # 去背景高分辨率版本
│   ├── logo-icon-*.png          # hicolor 32–512 图标（半透明磨砂底）
│   ├── logo.ico                 # Windows 安装器/快捷方式图标
├── packaging/                # desktop / AppStream / Debian / Windows / macOS
├── .github/workflows/        # release 分支全平台打包
├── Makefile                  # make check / make deb
├── docs/screenshots/
└── scripts/shot.sh
```

### 数据流

```
Slint 回调  →  main.rs 意图处理  →  Engine 状态变更
    ↑                                    │
    └──  VecModel / 属性更新  ←  行级 diff ┘
```

UI 不直接修改任务。暂停、继续、取消、新建、筛选和搜索都先变成 `main.rs` 里的意图，由
模拟引擎算出新状态，再以最小 diff 写回 `VecModel`。定时器每 200 ms 推进一次引擎，因此
速率、进度、剩余时间和详情图表都会连续变化。

引擎内置 12 个演示任务（HTTP / BT、下载中 / 等待 / 暂停 / 完成 / 失败 / 做种），覆盖了
列表与详情抽屉的所有视觉状态。

---

## 设计说明

**Material 3。** 直接使用 Slint 官方 Material 3 组件库，而不是手写一套仿 M3 控件。
`build.rs` 通过 `with_style("material")` 把 vendored 的 Slint 1.18.1 组件注册为样式；
Aether 只替换品牌色板，并补充上游没有的桌面业务组件。设置项、对话框和详情抽屉中的
按钮、开关、滑块、下拉框与单行输入框全部来自上游实现；自绘组件复用同一套 M3 令牌与官方
`StateLayer`（涟漪、悬停、焦点）。

| 层级 | 组件 | 来源 |
| --- | --- | --- |
| 上游 M3 | `Button`、`TextButton`、`IconButton`、`Switch`、`Slider`、`ComboBox`、`LineEdit`、`ScrollView`、`ListView`、`StateLayer` | `ui/material/`（Slint 1.18.1） |
| 自绘业务组件 | 导航栏、筛选 chip、搜索框、FAB、状态标签、进度轨、速度图、分片网格、文件徽标、设置卡片 | 基于 Aether 令牌 / `StateLayer` |

上游 Slint 目前没有 App Bar、Navigation Rail、Chip、FAB 这类模式组件，对应部分按 M3
规范自绘；设置页、对话框、表格与抽屉中的按钮、下拉框、开关与滑块均来自上游实现，文本
输入使用上游 M3 `LineEdit`（搜索框为自绘的胶囊组合）。

**深色优先。** `MaterialPalette` 的深色角色被重定向到 Aether 的 surface 阶梯
（`#141218` → `#211F26` → `#2B2930` → `#36343B`）。主色 `#6750A4` 驱动 accent 角色，
`ColorSchemeSelector` 固定为 dark，避免跟随系统切到浅色而破坏整屏对比。

**留白与层级。** 导航栏 84 px，内容区 20 px 外边距，表格行高 60 px。卡片的“悬浮感”
来自低透明度、大模糊半径的阴影，而不是粗边框或高对比描边。

**矢量图表。** Slint 1.18 还没有 `Canvas`。速度折线图因此用 `Path` 的 SVG `commands`
绘制，坐标系归一化到 0..1，再通过 `ImageFit.fill` 拉伸到目标尺寸；分片网格、进度轨和
背景流线同理。图标是单色描边 SVG 资源，由 `colorize` 在使用处着色，因此一套资源可以适配任意前景色角色。

**虚拟化与命中测试。** 表格用 `ListView`，只实例化视口内的行；行背景的
`TouchArea` 放在内容**之后**，这样行内按钮优先接收点击，而空白区域仍能打开详情抽屉。

**键盘与空状态。** 新建对话框自动聚焦 URL，Enter 提交、Esc 关闭；详情抽屉也支持 Esc
关闭。搜索或筛选无结果时，空状态提供“清除筛选条件”入口，一次恢复全部任务。

### 对 vendored 组件的改动

`ui/material/` 来自 Slint 1.18.1 的 `widgets/{material,common,interfaces}`，做了几处
小而局部的补丁，均保留上游版权头：

| 文件 | 改动 |
| --- | --- |
| `styling.slint` | 品牌色板：`#6750A4` 及 Aether 深色 surface 阶梯 |
| `color-scheme.slint` | 固定深色方案（上游从平台读取） |
| `lineedit.slint` | 新增 `show-clear-button`，路径输入框不需要清除按钮 |
| `scrollview.slint` | 1 px 描边滑块改为 6 px 实心圆角滑块，深色下更清晰 |
| `switch.slint` | 选中态滑块改用 `control-foreground`，与深紫轨道拉开对比 |

---

## 范围与限制

这是一个**界面原型**，不是可用的下载器：

- 下载过程完全由 `engine.rs` 模拟，不发起网络请求、不写文件
- 数据每次启动重新生成，没有持久化、断点续传或真实校验
- 没有系统托盘、通知、剪贴板监听、协议关联与多语言
- 窗口默认 1040×700，最小 900×600；没有做手机/超窄窗口布局
- 设置项只改变界面状态，不产生实际效果

## 许可

Aether 自有代码以 MIT 发布。`ui/material/` 为 Slint 上游 vendored 代码，版权归
SixtyFPS GmbH 所有，保留原始许可头，采用三选一许可：GPL-3.0-only、
Slint Royalty-free 2.0 或 Slint Software 3.0。再分发时请遵守相应条款。
缺少第三方许可文件的旧发布会立即下架；v0.1.0 的二进制因此已撤回，记录见
[`docs/WITHDRAWN_RELEASES.md`](docs/WITHDRAWN_RELEASES.md)。
