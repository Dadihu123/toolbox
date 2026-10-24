# Toolbox（个人工具集）v1.1 设计方案

> 目标读者：你自己（唯一开发者 + 主要用户）。
> 输入：现有 `ImageOverlayTool`（Electron + React 19 + TS）与 `tools/sprite-align`（单文件零依赖 HTML 工具）。
> 本文只谈设计与选型，不含实现。

## 0. 决策记录

| # | 问题 | 决定 | 影响 |
| --- | --- | --- | --- |
| 1 | 平台 | **仅 Windows** | 可直接调 Win32/DWM，不做跨平台抽象；Tauri 的最弱项（Linux WebKitGTK）不存在了 |
| 2 | 悬浮窗形态 | **圆球 + 全局面板** | 放弃"悬停展开图标网格"；面板改为搜索驱动，热键是主入口 |
| 3 | WebView2 依赖 | 接受（见 §2.5） | 本机已装 155.0.4283.62，零安装成本 |
| 4 | 快捷键粒度 | **绑到工具内动作** | 需要 Action Registry + 动态注册 + 窗口未开时的延迟派发 |
| 5 | 工具来源 | **要能装别人的** | 架构从"iframe 同源增强"改为"**每工具独立 webview + 能力沙箱 + 安装授权**"，这是 v1.1 最大的改动 |
| 6 | 环境（0.2.0） | **宿主做单一环境服务**：主题三态 `system/light/dark`、语言 `system/zh-CN/en-US`；Windows 注册表/`GetUserPreferredUILanguages` 解析，`WM_SETTINGCHANGE` 实时跟随；受控范围内只覆盖宿主窗口与工具标题栏，球与快捷扇不动 | `config.json` v2；所有窗口创建时必须显式传 `Some(resolved)`（WebView2 profile 全 app 共享，后创建者会把主题盖上去，见 §1 的结构说明）；工具通过 Bridge v2 的 `theme`/`locale` 事件获知变化 |
| 7 | 宿主 UI 构建（0.2.0） | **Vite 多页 + pnpm**，`ui/` 仍是源码根，产物 `ui/dist`，`frontendDist` 指向它；不引 React/TS，不迁主窗口框架 | 面板保持零框架与首屏预算；`make-portable.ps1` 先 `pnpm build` 再 `cargo build`；**不设 `devUrl`**（会让 release 构建嵌入空资产表） |
| 8 | i18n（0.2.0） | 前端 **i18next**（命名空间 common/panel/main，仅加载本页所需）+ Rust **rust-i18n 4**（`native.*`）；词表所有权分离；`pnpm i18n:check` 与 `cargo test` 各自校验 | zh-CN 为源语言与回退；en-US 全量对齐；`pnpm dev?lng=en-XA` 伪语言抓硬编码 |
| 9 | 视觉收尾（0.2.0） | 面板做**真磨砂**（窗口级 Acrylic + 圆角裁剪），并把饱和描边色/遮罩/提示阴影提升为共享 token | 见 §4.2；`ui/shared/theme.css` 的 `--*-line`、`--scrim`、`--shadow-toast`；小写字母 .02–.04 的透明度差异被合并，暗色视觉不变 |

因 #5，原方案的"单 Tool Host + iframe"被**整体废弃**：同源 iframe 不构成安全边界，第三方工具能碰宿主特权。现在每个工具一个窗口、一个独立 renderer、一套最小能力白名单。

因 #2 + 低资源要求，**圆球改为原生窗口**（Rust + Win32 分层窗口），不常驻 webview：空闲内存从 ~60MB 降到 ~20MB。面板仍是 webview，按需创建。

---

## 1. 需求与硬约束

| 原始需求 | 真正的约束 | 设计含义 |
| --- | --- | --- |
| 平时以悬浮窗显示 | 圆球常驻、置顶、**不抢焦点**、可拖动、贴边吸附 | 原生分层窗口 + `WS_EX_NOACTIVATE`；不能用 webview 常驻 |
| 悬浮/点击展开工具项 | 面板要能搜索、键盘操作、秒开 | 面板 = 独立 webview 窗口，按需创建 + 短时保活 |
| 可打开主界面 / 工具界面 | 多窗口生命周期 | 窗口按类型注册、按需创建、关闭即销毁 |
| 平时低资源占用 | 空闲不能有常驻 renderer | 空闲 = 1 个 Rust 进程 + 托盘 + 原生圆球 |
| 可给工具功能设快捷键 | 功能项级注册 | Action Registry（静态声明 + 运行期动态）+ 全局热键服务 + 改键 UI |
| 即插即用，数据同目录 | 不写 AppData/注册表 | `AppPaths` + WebView2 user data 重定向 |
| 要能装别人的工具 | **不可信代码** | 独立 webview、零 Tauri 权限、Bridge 能力校验、安装时授权、导航/网络封锁 |

---

## 2. 技术栈

### 2.1 选型结论

**Tauri 2（Rust 宿主）+ React 19 + TypeScript + Vite + Tailwind v4 + zustand + Radix**

Windows-only 之后，Tauri 的两个主要顾虑（Linux WebKitGTK 差异、跨平台抽象成本）都消失了，而它的优势被放大：

1. **空闲成本**：没有 Node 主进程；圆球用原生窗口；没有窗口时零 webview。
2. **便携可控**：路径全自己管，不强制 `app_data_dir`。
3. **热键/托盘/单实例/自启动/窗口状态**都是官方插件，Windows 上都是原生实现。
4. **体积**：便携包 ~12MB，对比 Electron ~100MB。

工具本体：**不限定技术栈**，契约只要求"产出静态站点（index.html + 资源）"。

### 2.2 依赖清单

宿主（Rust）：

```toml
tauri = { version = "2", features = ["tray-icon", "image-png"] }
tauri-plugin-global-shortcut = "2"
tauri-plugin-single-instance   = "2"
tauri-plugin-autostart         = "2"
tauri-plugin-dialog            = "2"
tauri-plugin-opener            = "2"
tauri-plugin-clipboard-manager = "2"
tauri-plugin-log               = "2"
serde / serde_json / zip / resvg / image
windows = "0.58"        # WS_EX_NOACTIVATE / DWM / TrackPopupMenu / DragAcceptFiles
notify                   # 监听 tools/ 目录变化（dev 模式）
```

前端（宿主 UI）：React 19 + TS + Vite + Tailwind v4 + zustand + Radix + lucide-react。

### 2.3 目录结构（Tauri 侧）

```
src-tauri/src/
├── main.rs
├── app.rs                # Builder 组装、启动顺序
├── paths.rs              # AppPaths：便携检测、目录创建、只读兜底
├── config/               # config.json / hotkeys.json / tools.json（原子写 + 迁移）
├── trust/                # 信任级别、安装校验、hash 记录
├── registry/
│   ├── tools.rs          # 扫描、manifest 校验、启停
│   └── actions.rs        # 功能项注册表（静态 + 动态 + 延迟派发）
├── hotkeys.rs            # 全局热键 ↔ Action，冲突检测、按窗口生命周期注册/注销
├── windows/
│   ├── ball.rs           # ★ 原生圆球（分层窗口、拖动、吸附、DPI、拖放接收）
│   ├── panel.rs          # 全局面板（webview，按需 + 保活 5 分钟）
│   ├── tool_window.rs    # 每工具一个窗口/webview
│   └── main_window.rs
├── bridge/               # 自定义协议 + Bridge 命令 + 能力校验
└── toolserver.rs         # toolbox:// 协议：按工具 root 提供静态文件（防穿越）
```

### 2.4 前端入口

```
apps/shell/
├── index.html        # 面板（纯 HTML/CSS/JS，几百行，不用 React——启动快）
├── main.html         # 主界面（React：工具管理 / 快捷键 / 设置 / 日志）
└── src/              # 主界面 React 源码 + 共享 UI
```

面板刻意不用 React：它要 100ms 内出现，手写 DOM + 一个 `innerHTML` 渲染列表足够，也省掉一个 bundle 的解析时间。

### 2.5 WebView2 依赖的实际影响（回答你的问题）

**是什么**：WebView2 Runtime 是微软的 Chromium 运行时，Edge 和所有 WebView2 应用共用它。你的应用不是自带浏览器内核，而是调用系统里这个。

**要不要装**：

- **Windows 11：预装，什么都不用装。** 你这台机器我已经查过了：`C:\Program Files (x86)\Microsoft\EdgeWebView\Application\155.0.4283.62`，已注册，直接可用。
- **Windows 10：绝大多数机器已有**（2021 年后随 Edge 一起更新）。没有的话装一次 Evergreen Bootstrapper（~2MB 在线，安装后约 400–900MB 磁盘）。
- 安装器里配 `webview2InstallMode = "downloadBootstrapper"`，缺了就自动拉；便携包启动时检测，缺失给提示 + 一个下载按钮，不会静默失败。

**对你项目的三个真实影响**：

| 影响 | 说明 | 对策 |
| --- | --- | --- |
| 运行时本体占 C 盘 | 约 400–900MB，在 `Program Files (x86)`，是**运行时不是你的数据** | 接受。真要求零 C 盘写入，用 WebView2 **Fixed Version** 模式：把运行时放进应用目录（+~180MB，且要自己跟版本），个人用没必要 |
| 随 Edge 自动更新 | 安全补丁自动来，但偶尔版本回归 | 接受；Fixed Version 是解药但代价大 |
| 换机器/新系统可能要装一次 | 只有 Win10 老机器会遇到 | 启动检测 + 引导；或做"带运行时的完整包" |

**和 Electron 比**：Electron 把 Chromium 打进包里（+80–120MB 安装体积），换来"零系统依赖"；代价是空闲内存 3–4 倍。你选了"低资源"+"Windows 单平台"，WebView2 是更优解。真正需要在意的只有一条：**WebView2 的 user data 目录必须重定向**，否则缓存/localStorage 会写 `%LOCALAPPDATA%`（M0 验证项）。

---

## 3. 架构

### 3.1 进程与窗口

```
┌──────────────────────── Toolbox.exe (Rust 宿主，无 webview 时 ~20MB) ────────────────────────┐
│                                                                                            │
│  AppPaths      ToolRegistry    ActionRegistry   HotkeyService   WindowService   BridgeHost   │
│                                                                                            │
│  ┌──────────────┐   ┌──────────────┐   ┌──────────────┐   ┌──────────────────────────────┐ │
│  │ 圆球（原生）   │   │ 面板(webview) │   │ 主界面(webview)│   │ 工具窗口 ×N（各自 webview）    │ │
│  │ 常驻 ~5MB     │   │ 按需+保活     │   │ 按需          │   │ 独立 renderer，沙箱           │ │
│  └──────────────┘   └──────────────┘   └──────────────┘   └──────────────────────────────┘ │
│         │                   │                 │                        │                    │
│         └───────────────────┴─────────────────┴────────────────────────┘                    │
│                                    事件总线（Rust 内）                                       │
└────────────────────────────────────────────────────────────────────────────────────────────┘
                                      ▲ 只走 bridge（能力校验）
                        ┌─────────────┴─────────────┐
                        │ toolbox://<toolId>/index  │  ← 每工具独立 origin
                        └───────────────────────────┘
```

要点：

- **没有窗口 = 没有 webview**。Tauri 支持无窗口启动（托盘存活），这点决定了空闲成本。
- **工具 = 独立窗口 + 独立 origin + 独立 renderer**，不再用 iframe。
- **特权只在 Rust**：工具窗口的能力由 Rust 在命令入口按窗口 label 反查 manifest 决定。

### 3.2 信任模型

| 级别 | 来源 | 授权 | 默认权限 |
| --- | --- | --- | --- |
| `builtin` | `<exe>/tools`（随包） | 无需确认 | 按 manifest 声明全给 |
| `installed` | `data/tools`（用户安装） | **安装时逐项授权 + 显示作者/文件数/大小** | 仅用户勾选项；`network` 默认全禁 |
| `dev` | 任意本地路径（开发模式） | 每次启动确认 | 同 installed，窗口带"开发中"标识 |

三条不可协商的规则：

1. **工具窗口零 Tauri 权限**：capability 只给 `bridge:call` 一项，`withGlobalTauri: false`。工具拿不到任何核心命令（fs/shell/http 全无）。
2. **toolId 只从窗口 label 来**，绝不信任 JS 传上来的 id（防工具 A 冒充 B 读 B 的数据）。
3. **文件必须经用户选择**：`open`/`save` 一律走系统对话框；工具永远拿不到一个"用户没点过"的路径。工具私有目录固定 `data/tools/<id>/`，路径规范化后前缀校验。

### 3.3 一次"按热键执行工具动作"的时序（对应决策 #4）

```
Ctrl+Alt+Z（绑定到 tool.sprite-align.realign）
  └─ global-shortcut → HotkeyService → ActionDispatcher.dispatch("tool.sprite-align.realign")
        ├─ 工具窗口未开？
        │    ├─ ToolRegistry.open("sprite-align")  → 建窗口、加载 toolbox://sprite-align/
        │    └─ 动作进 pending 队列，等 tool:ready 后派发（带 3s 超时，超时丢弃并提示）
        ├─ 工具已开但窗口在后台？ → 聚焦窗口
        └─ 窗口就绪 → emit 事件 "action" { id: "realign" } → 工具执行
```

工具没实现该动作、或动作已注销 → 托盘气泡提示"该功能当前不可用"，不静默失败。

---

## 4. 圆球 + 全局面板（决策 #2）

### 4.1 圆球（原生窗口）

| 项 | 规格 |
| --- | --- |
| 尺寸 | 56×56 DIP（含 8px 阴影区），内容圆 44px |
| 样式 | 分层窗口，圆角/圆形，半透明 0.92，自绘抗锯齿 |
| 窗口样式 | `WS_POPUP | WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST` |
| 行为 | 单击 → 面板；右键 → 菜单（主界面/设置/最近工具/退出）；拖动 → 移动，松手吸附到屏幕边（距边 <24px 吸附）；双击 → 最近使用的工具 |
| 状态 | 空闲 3s 后透明度降到 0.35；hover 恢复 1.0（CSS 无关，纯窗口 alpha，一次 `SetLayeredWindowAttributes`） |
| 徽标 | 有工具窗口打开时右下角小点；有未读提示时数字 |
| 多显示器 | 按显示器分别记住位置（`data/config.json` 里存 `{monitorId, x, y, edge}`），DPI 变化重算 |
| 拖放 | `DragAcceptFiles`：文件拖到球上 → 面板打开并过滤"接受文件的工具" |
| 空闲开销 | 无定时器；透明度切换用一个 `SetTimer` 一次性回调，无轮询 |

为什么不用 webview 做球：一个常驻 webview 就是 ~50MB + 一个 renderer 进程 + 一个 Chromium 事件循环，和"低资源"直接冲突。原生球是 ~5MB 且完全可控。代价是 ~300–400 行 Rust（分层窗口 + DPI + 拖动 + 菜单），一次写完不再动。

> 图标渲染：manifest 的 `icon.svg` 用 `resvg` 光栅化到当前 DPI 的位图，缓存到内存；没有 svg 就退回内置默认图标。

### 4.2 面板

| 项 | 规格 |
| --- | --- |
| 尺寸 | 460×560，圆角 14，毛玻璃（Win32 Acrylic + 等半径窗口区域，见下） |
| 打开方式 | 点圆球 → 球旁边弹出（贴边时向内侧展开）；按热键 → **鼠标位置**弹出（自动避让屏幕边缘） |
| 内容 | 顶部搜索框（聚焦）；"最近"（4 个）；"全部工具"网格；底部条：主界面 / 设置 / 退出 |
| 搜索范围 | **工具名 + 工具动作名**（输入"导出"能直接命中 `sprite-align` 的导出动作，Enter 即执行） |
| 键盘 | ↑↓ 选择、Enter 执行、Esc 关闭、`Ctrl+1..9` 直启第 N 项 |
| 关闭 | Esc / 失焦 / 执行后 |
| 生命周期 | 首次打开创建；关闭后**保活 5 分钟**（`tokio::sleep` 一次性任务），之后销毁。重复使用秒开，长期不用不占内存 |
| 焦点 | 面板**要**抢焦点（搜索框要输入），只有圆球不抢 |

面板与主界面共用一套设计 token（`ui/shared/theme.css`），两边都是零框架手写页面。

> **毛玻璃（0.2.0 实装）**：CSS `backdrop-filter` 在透明窗口里没有可模糊的背景（窗口本身
> 透明，页面背后的 backdrop 是空的），所以模糊只能由窗口合成器提供。
> 实现 = `SetWindowCompositionAttribute(ACCENT_ENABLE_ACRYLICBLURBEHIND)` + 与 CSS 圆角
> 同半径的 `SetWindowRgn` 裁剪（不加区域的话亚克力会把四角补成直角），见
> `src-tauri/src/windows/glass.rs`。应用成功后宿主在 `get_state` 的 `env.glass` 里报 `true`，
> 面板据此切到半透明表面（`[data-glass="on"]` 覆写 `--panel-bg` / `--panel-foot`）；
> 失败就保持不透明的 0.1.0 外观——半透明而没有模糊会把桌面糊在面板上，不可读。
> 主题切换要重设模糊的 tint（`panel::refresh_glass`），它烘焙在窗口属性里而不是 CSS 里。
> 排障开关：`TOOLBOX_PANEL_GLASS=off|accent|full`。

### 4.3 交互边界（避免和 PixPin 抢键）

- 圆球不响应鼠标滚轮，不拦截任何输入。
- 面板打开时**不**全局钩键盘（用窗口焦点 + 页面 `keydown` 即可），避免装 `WH_KEYBOARD_LL` 钩子（性能 + 杀软关注）。
- 圆球与面板都不在任务栏出现。

---

## 5. 工具插件契约与沙箱（决策 #5，v1.1 重写）

### 5.1 工具包结构

```
sprite-align/
├── tool.json          # 清单（必需）
├── icon.svg           # 图标（必需）
├── index.html         # 入口
└── ...                # 工具自己的资源
```

原则：**工具是一个能独立运行的静态 web 应用**。删掉 `tool.json` 仍可双击 `index.html` 用；有 `tool.json` 就被宿主接管（热键、Bridge、窗口、授权）。

### 5.2 `tool.json` v1

> 0.2.0 起 `bridgeVersion` 接受 **1 和 2**（v1 工具用 v2 shim 照常加载，>2 明确拒绝），并新增
> 可选的内联多语言字段 `locales`；`manifestVersion` 仍为 1。实现见
> `src-tauri/src/registry.rs`（`Manifest::name_for` / `search_text`）与 §5.6。

```json
{
  "manifestVersion": 1,
  "bridgeVersion": 2,
  "id": "sprite-align",
  "name": "序列帧对齐",
  "description": "切分 sprite sheet、识别帧内容、锚点对齐、导出 ZIP",
  "locales": {
    "en-US": {
      "name": "Sprite Align",
      "description": "Slice sprite sheets, align anchors, export PNG / ZIP",
      "actions": { "open": "Open Sprite Align", "export-zip": "Export as ZIP" }
    }
  },
  "version": "1.2.0",
  "author": "yhmr",
  "homepage": "https://github.com/...",
  "entry": "sprite-align.html",
  "icon": "icon.svg",
  "accent": "#4ea1ff",
  "window": { "width": 1280, "height": 820, "minWidth": 900, "minHeight": 600 },
  "permissions": {
    "files.open": true,
    "files.save": true,
    "clipboard.write": true,
    "clipboard.read": false,
    "data": true,
    "external.open": true,
    "network": []
  },
  "actions": [
    { "id": "open",       "title": "打开序列帧对齐", "defaultHotkey": "Ctrl+Alt+S" },
    { "id": "realign",    "title": "重新对齐",       "defaultHotkey": "Ctrl+Alt+Z", "requiresOpen": true },
    { "id": "export-zip", "title": "导出为 ZIP",     "requiresOpen": true }
  ],
  "accepts": ["image/png", "image/webp"]
}
```

- `permissions` 是**能力白名单**，缺省全禁；安装时逐项展示给用户。
- `actions` 是静态功能项；`requiresOpen: true` 表示"必须先打开工具"（宿主自动开窗 + 延迟派发）。
- `accepts` 让圆球拖放/命令行能筛出"能处理这个文件的工具"。
- 校验：前端 `packages/manifest`（zod）出 JSON Schema，Rust 侧 `serde` 结构体 + 手写校验，两边共用 schema 文档 + 一份 fixtures 做交叉测试。

### 5.3 隔离手段（逐条对应攻击面）

| 攻击面 | 手段 |
| --- | --- |
| 调宿主特权命令 | 工具窗口 capability 只有 `bridge:call`；`withGlobalTauri:false`；核心插件（fs/shell/http/process）**不给**该窗口任何权限 |
| 冒充别的工具读数据 | Rust 从 `window.label` 解析 toolId，JS 传的 id 一律忽略；label 与 toolId 绑定在建窗时 |
| 读任意文件 | 只暴露"对话框选中的文件"；私有目录固定 `data/tools/<id>/`，`canonicalize` 后前缀校验，拒绝 `..`/符号链接逃逸 |
| 自定义协议穿越 | `toolbox://<id>/../../etc` → 拒绝；每个工具 root 独立，只服务自己目录 |
| 跳转到外网/钓鱼页 | `on_navigation` 拦截：非 `toolbox://<self>/` 一律 `false`；`window.open` 拦截 → 走系统浏览器且需确认 |
| 偷偷联网外传 | WebView2 `WebResourceRequested` 过滤（`with_webview` + COM）：默认只允许 `toolbox://` 与 `data:`；`network` 白名单命中才放行（M0 验证可行性） |
| 注入脚本/XSS | 协议响应头下发 CSP：`default-src 'self' toolbox:; script-src 'self' 'unsafe-inline'`（工具自带内联脚本，故放开 inline；禁止 eval 之外的远程加载） |
| 无限占资源 | 关闭即销毁；单工具单实例；manifest 限制包大小（50MB）/ 文件数（5000）；崩溃只影响自己窗口 |
| 静默持久化 | `data` 权限必须显式声明；卸载时问是否删数据 |

> 威胁模型说明：WebView2 的 renderer 本身有 Chromium 沙箱兜底，我们防的是"工具通过 Bridge 越权"，不是"工具攻破 Chromium"。这套边界足以支撑"装别人的工具"，但不是浏览器级沙箱；UI 里如实写清楚。

### 5.4 安装流程

```
用户拖入 .zip / 点"安装工具"
  → 解压到 data/tmp/install-<uuid>/
  → 校验：manifest schema、id 合法性、id 是否已存在（同 id = 升级）、entry 存在、体积/文件数
  → 授权页：名称/作者/版本/描述 + 权限逐项勾选 + 文件列表（可展开）+ "我只安装信任来源的工具"
  → 确认 → 移到 data/tools/<id>/（升级先备份旧版到 data/backups/tools/）
  → 写 tools.json：{ id, version, source:"installed", installedAt, sha256, permissions }
  → 出现在面板/主界面
```

卸载：删目录 + 询问是否连带 `data/tools/<id>/` 数据。
更新：同 id + 更高 semver；不做自动更新（第三方工具自动更新 = 自动执行未审代码）。

### 5.5 开发模式

- `toolbox.exe --dev-tool <path>` 或主界面"加载开发中工具"：注册为 `dev` 信任级，文件监听 → 改动后自动 reload 窗口。
- `pnpm create toolbox-tool` 脚手架 + `@toolbox/tool-sdk`（TS 类型 + shim 类型 + 调试工具）。
- 模板内置 `bridge` 的类型声明，所以工具侧写 `window.__toolbox.files.save(...)` 有完整补全。

### 5.6 Bridge API v2

> 0.2.0 起 shim 声明 `version: 2`，并增加了环境字段与两个事件。**v1 工具无需改动**：
> v2 是 v1 的超集，shim 从不移除 v1 成员；`tool.json` 的 `bridgeVersion` 1 与 2 都接受。
> 工具页面不要依赖 `navigator.language` 或 `<html lang>`——运行期切换语言时它们不变，
> 以 `__toolbox.locale` 与 `locale` 事件为准。shim 也不注入 `data-theme`、不改 `lang`。

注入的 shim（`initialization_script`，工具源码零改动）：

```ts
interface ToolboxBridge {
  readonly version: 2;                      // v1 工具照常用，只是看到 2
  readonly toolId: string;
  readonly mode: "hosted" | "standalone";   // standalone 时全部空实现，工具照常工作
  readonly theme: "dark" | "light";         // 生效主题（宿主可能显式锁定）
  readonly locale: "zh-CN" | "en-US";       // 生效语言（权威值）
  readonly availableLocales: string[];      // ["zh-CN", "en-US"]

  files: {
    open(opts?: { accept?: string[]; multiple?: boolean }): Promise<OpenedFile[]>;
    save(opts: { suggestedName: string; data: ArrayBuffer | string; mime?: string }): Promise<boolean>;
  };
  clipboard: { writeText(t: string): Promise<void>; readText(): Promise<string>; writeImage(b: Blob): Promise<void>; readImage(): Promise<Blob | null> };
  data: { get<T>(key: string): Promise<T | null>; set(key: string, v: unknown): Promise<void>; remove(key: string): Promise<void> };
  actions: { register(a: { id: string; title: string; defaultHotkey?: string; handler: () => void }): () => void };
  hotkeys: { claimLocal(combo: string, label: string): () => void };  // 仅登记用于展示，实际按键由工具自己处理
  window: { setTitle(t: string): void; close(): void; setAlwaysOnTop(v: boolean): void };
  events: { on(e: "action" | "theme" | "locale" | "files-dropped", cb: (v: unknown) => void): () => void };
  log: { info(msg: string, data?: unknown): void; error(msg: string, data?: unknown): void };
}
```

- 事件 payload：`theme` 是 `"dark" | "light"`，`locale` 是 BCP-47 串（`"zh-CN" | "en-US"`）。
- 每次调用都带 `{method, args}` → Rust 侧统一 `bridge::call` 命令 → 校验（label→toolId、权限、参数 schema）→ 执行。
- `bridgeVersion` 高于 2 → 拒绝加载并给明确原因（"需要 Bridge v3，本工具箱仅支持到 v2"）；
  1 与 2 都按 v2 shim 加载。
- 宿主切换语言时：已开工具窗口的标题由 Rust 重新设置（`tools::refresh_titles`），
  `locale` 事件送达页面；工具自行决定是否重绘。

---

## 6. 快捷键（决策 #4：功能项级）

### 6.1 数据结构

```ts
type Action = {
  id: string;                 // "tool.sprite-align.export-zip" | "app.open-panel"
  scope: "app" | "tool";
  toolId?: string;
  title: string;
  defaultHotkey?: string;
  requiresOpen: boolean;      // 需要工具窗口先存在
  accepts?: string[];         // 可接收拖放文件
};
```

### 6.2 注册与派发

1. **启动**：收集宿主内置 Action（唤出面板 / 显示圆球 / 打开主界面 / 退出）+ 所有已启用工具的静态 `actions`。
2. **合并改键**：`hotkeys.json` 用户值 > 工具 `defaultHotkey` > 宿主默认。key 用 `action.id`，工具升级后旧改键继续有效。
3. **注册**：逐个 `RegisterHotKey`；失败（被占用/非法）不崩，进"未生效"列表并给原因。
4. **动态动作**：工具运行期 `actions.register()` → 立即进注册表；`requiresOpen` 的动作**只在窗口打开时注册热键**，窗口关闭即注销（避免抢着全局键却用不了）。
5. **派发**：见 §3.3 时序；`requiresOpen` 未开窗时自动开窗 + pending 队列（3s 超时）。

### 6.3 改键 UI（主界面"快捷键"页）

表格：功能项（按工具分组）/ 当前键 / 状态（生效 / 被 X 占用 / 非法 / 未设置）/ 改键 / 恢复默认。

- 改键控件：按下即录，Esc 取消；录入时先 `unregister` 试注册，成功后提交。
- 冲突时并列显示冲突方（哪个工具的哪个动作），一键"把对方改成空"。
- 单独一节"工具内快捷键"（`claimLocal` 登记的），只展示不干预，避免用户以为全局可用。

### 6.4 已知限制（写进 UI，不当 bug 修）

- 前台是**提权窗口**（UAC、管理员权限的程序）时 `RegisterHotKey` 收不到键。
- 全屏独占游戏里通常无效。
- v1 只支持 `Ctrl/Alt/Shift/Win + 单键`；不做 chord（`Ctrl+K Ctrl+S`）和双击修饰键——那需要低级键盘钩子，代价与风险不成比例。

---

## 7. 数据与便携

### 7.1 目录布局

```
Toolbox/                      ← 整个目录可拷走
├── Toolbox.exe
├── tools/                    ← 随包工具（升级宿主不动这里）
│   └── sprite-align/
├── data/                     ← 运行时生成，全部用户数据
│   ├── config.json           # 外观、圆球位置、语言、启动行为
│   ├── hotkeys.json          # 快捷键覆盖
│   ├── tools.json            # 安装记录、启用状态、授权、hash
│   ├── tools/<id>/           # 每工具私有数据（state.json 等）
│   ├── backups/              # 配置/工具升级前快照（各留 5 份）
│   ├── logs/                 # 按天滚动，留 7 天
│   └── webview2/             # ★ WebView2 user data（缓存/localStorage/GPU）
└── data/README.txt           # "删掉本目录即完全重置"
```

### 7.2 路径解析（`AppPaths`）

```
1. --data-dir <path>
2. 环境变量 TOOLBOX_HOME
3. <exe 目录>/data                 ← 默认（便携）；写探测 .write-probe 成功才用
4. 兜底 %LOCALAPPDATA%\Toolbox\data （exe 在 Program Files / 只读盘 / 同步盘锁定时）
   → 主界面顶部黄条"当前为非便携模式"，显示实际路径 + 一键"复制到 exe 目录"
```

代码里禁止出现 `app_data_dir()` / `app_config_dir()` / `dirs::config_dir()`，加一条 CI grep 守卫。

### 7.3 必须显式改掉的系统默认

| 位置 | 默认 | 处理 |
| --- | --- | --- |
| WebView2 user data | `%LOCALAPPDATA%\<exe>.WebView2` | 建 webview 时指定 `data/webview2`（M0 验证） |
| WebView2 运行时本体 | `Program Files (x86)\Microsoft\EdgeWebView` | **不可避免**（系统运行时，见 §2.5） |
| 日志 | 库默认可能写 AppData | 统一 `data/logs` |
| 自启动 | `HKCU\...\Run` | 可接受；关闭开关时主动删值 |
| 崩溃转储 | 系统 WER | v1 不启用 |

### 7.4 配置

- JSON + `CONFIG_VERSION`（0.2.0 起为 **2**），加载时迁移；v1 的 `theme`/`locale` 只是从未暴露的
  占位值，迁移一律重置为 `"system"`（实测见 `src-tauri/src/state.rs` 的 `migrate` 测试）。
- 归一化：非法 `theme`/`locale` 值退回 `"system"`；允许手改（容忍 UTF-8 BOM）。
- 原子写：`*.tmp` → `fsync` → `rename`；debounce 500ms。
- 写失败 → 只读模式 + 顶部提示，不反复弹错。
- 不加密不混淆（个人工具，可读性优先）。

---

## 8. 资源预算

| 状态 | 目标 | 组成 |
| --- | --- | --- |
| 空闲（圆球常驻，无窗口） | **≤ 30 MB，CPU ≈ 0%** | Rust 进程 ~15MB + 原生圆球 ~5MB + 托盘 |
| 面板打开 | +50–70 MB | 1 个 webview（保活 5 分钟后释放） |
| 主界面打开 | +70–100 MB | React 页面 |
| 单工具打开 | +60–120 MB | 独立 renderer（canvas 类工具偏高） |
| 启动到圆球可见 | < 500 ms | 先建球，再懒加载 registry |

手段：

1. 无窗口 = 无 webview（Tauri 无窗口启动 + 托盘存活）。
2. 圆球原生，不常驻 webview（这是相对 v1.0 最大的改动）。
3. 面板/主界面/工具窗口关闭即 `destroy()`，不做 hide 保活。
4. 面板保活用一次性定时器，不用轮询。
5. WebView2 闲置可调 `MemoryUsageTargetLevel::Low`（面板保活期用得上）。
6. 工具 renderer 进程上限（`additional_browser_args` 里 `--renderer-process-limit`）。
7. 发版前跑 `scripts/measure-idle.ps1`（WorkingSet + CPU 采样），记录到 `benchmarks/idle.json` 做回归。

---

## 9. sprite-align 接入（已确认现状）

现状：单文件 221KB、零外部依赖；导出用 `URL.createObjectURL` + `<a download>`；无 localStorage / File System Access / 剪贴板 API；`selftest.mjs`（24 项）+ `e2e.mjs`（15 项）可继续跑。

接入（尽量零改动）：

1. 加 `tool.json`（`entry: "sprite-align.html"`，权限只开 `files.save`、`data`）+ `icon.svg`，放进 `<exe>/tools/sprite-align/`，信任级 `builtin`。
2. **下载接管**：工具窗口设 `on_download` 回调 → 弹原生保存框 → `save_to(path)`。`downloadBlob()` 一行不改，PNG/ZIP 直接落盘；默认目录记忆上次选择（存 `config.json`）。
3. **拖拽**：工具窗口 `dragDropEnabled: false`（否则 Tauri 吞掉 HTML5 drop）；`<input type=file>` 在 WebView2 原生可用。
4. **命令行/拖到球**：`Toolbox.exe --tool sprite-align --open "a.png"` → 单实例转发 → 工具窗口 `files-dropped` 事件。工具侧可选 3 行接 `bridge.files`。
5. **可选增强**（各 5–10 行）：`bridge.data` 记住 alpha 阈值/落点模式；导出后 `bridge.log.info` 进"最近导出"。
6. **保持可独立运行**：`mode === "standalone"` 时 shim 全 no-op。

测试：`selftest.mjs` / `e2e.mjs` 继续作为工具层回归（与宿主无关，CI 可跑）；宿主侧加冒烟（启动 → 热键开工具 → 导出落盘）。

---

## 10. 仓库结构（新仓库）

```
toolbox/
├── apps/shell/
│   ├── index.html            # 面板（零框架）
│   ├── main.html             # 主界面
│   ├── src/                  # 主界面 React
│   └── src-tauri/            # 见 §2.3
├── packages/
│   ├── bridge/               # __toolbox shim（TS → iife，注入用）+ 类型
│   ├── manifest/             # zod schema + JSON Schema + 校验 CLI + fixtures
│   └── ui/                   # 设计 token + 基础组件
├── tools/sprite-align/       # 从旧仓库平移
├── scripts/                  # 便携包、内存基线、图标
├── docs/
└── pnpm-workspace.yaml
```

---

## 11. 构建与分发

- 开发：`pnpm tauri dev`。
- 构建：`pnpm tauri build` + `scripts/make-portable.ps1`：
  ```
  Toolbox-portable-v1.0.0.zip
  ├── Toolbox.exe
  ├── tools/
  └── data/README.txt
  ```
- WebView2：安装器 `downloadBootstrapper`；便携包启动检测 + 引导下载。
- 签名：个人自用可跳过；要免 SmartScreen 再上证书。
- 更新：便携模式"替换 exe"最简；v1 不做自动更新，主界面放"检查更新"链接。`tools/` 与 `data/` 永不进更新范围。

---

## 12. 测试

| 层 | 工具 | 内容 |
| --- | --- | --- |
| Rust 单测 | `cargo test` | 路径解析（便携/兜底/只读）、manifest 校验、**协议路径穿越**、权限矩阵、热键字符串解析、配置迁移 |
| 安全用例 | `cargo test` + fixtures | 恶意工具样例：`../` 逃逸、伪造 toolId、未授权 network、超大包、循环符号链接 |
| 宿主前端 | vitest + TL | 工具列表、快捷键页、授权页、bridge 路由（mock） |
| 工具层 | 沿用 `selftest.mjs` / `e2e.mjs` | sprite-align 全流程 |
| 端到端 | `tauri-driver` + WebdriverIO | 启动 → 圆球出现 → 热键开工具 → 动作派发 → 导出落盘 |
| 资源 | `measure-idle.ps1` | idle 内存/CPU/启动时间，发版对比 |
| 便携 | 手动 checklist | U 盘 / 只读盘 / 另一台 Win10 |

---

## 13. 里程碑

| 阶段 | 内容 | 验收 |
| --- | --- | --- |
| **M0 骨架 + 4 个 spike**（2 天） | 仓库、Tauri 无窗口启动、`AppPaths`、托盘、单实例；spike：① WebView2 user data 重定向 ② 原生圆球（分层/无焦点/拖动/DPI）③ 下载接管 ④ 网络封锁 | 双击 exe 只见圆球；`data/` 在 exe 旁且 `%LOCALAPPDATA%` 无本应用目录；点圆球不抢记事本焦点 |
| **M1 圆球 + 面板**（3–4 天） | 吸附/多显示器记忆/右键菜单/拖放；面板搜索 + 键盘 + 保活；`config.json` | 热键 100ms 内出面板；搜索能命中动作名；闲置内存 ≤30MB |
| **M2 工具运行 + 沙箱**（5–7 天） | `toolbox://` 协议、ToolRegistry、manifest 校验、工具窗口、Bridge v1、权限校验、安装/卸载/授权 UI | 装一个测试工具，越权调用全部被拒并给出原因 |
| **M3 动作 + 热键**（3 天） | ActionRegistry、动态注册、延迟派发、改键 UI、冲突处理 | "导出 ZIP" 可绑键；工具没开时自动开窗并执行 |
| **M4 sprite-align 接入**（2–3 天） | tool.json、下载接管、拖放、命令行 | 拖图 → 对齐 → 导出 ZIP；工具单独双击仍可用 |
| **M5 加固**（2–3 天） | 日志、只读兜底、崩溃重载、便携包、资源基线 | 发版 checklist 全绿 |

M2 之后就该拉一个"别人的工具"来做真实验证（哪怕是你自己写的第二个工具，也按 `installed` 路径安装），否则沙箱是纸上的。

---

## 14. 风险

| # | 风险 | 影响 | 对策 |
| --- | --- | --- | --- |
| 1 | WebView2 缺失（仅 Win10 老机） | 起不来 | 启动检测 + 引导下载；安装器 bootstrapper |
| 2 | 原生圆球工作量超预期（DPI/多屏/焦点） | 拖进度 | M0 先做 spike；兜底退回 webview 球（+50MB） |
| 3 | WebView2 网络封锁无干净 API | 第三方工具可外联 | `WebResourceRequested` COM 拦截；不成就退化为 CSP + 审计提示，并在授权页写明 |
| 4 | 全局热键被占 / 提权窗口失效 | 功能不可用 | 冲突检测 + 改键 + 说明；圆球/托盘兜底 |
| 5 | 便携目录只读 | 数据写不进 | 写探测 + 兜底 + 黄条提示 |
| 6 | 沙箱边界被第三方工具绕过 | 安全 | 零核心权限 + label 定身份 + 路径前缀校验 + 恶意 fixtures 回归；如实声明非浏览器级沙箱 |
| 7 | Tauri 细节与文档不一致（下载事件/user data/COM） | 返工 | 全部塞 M0，半天内验证完再往下写 |
| 8 | 工具生态分裂（单文件 vs 需要构建） | 规范混乱 | 契约只要求"产出静态站点"；单文件是 sprite-align 的选择 |

---

## 15. 剩余待定

1. **圆球默认位置**：右下角（PixPin 习惯）还是上次退出位置？建议"首次右下角，之后记忆"。
2. **面板是否常驻热键**：唤出面板的热键用 `Ctrl+Alt+Space` 还是 `Alt+``？后者更容易被别的软件占用。
3. **是否需要"工具市场"概念**：v1 建议只做"本地 zip 安装"，市场留到有 5+ 工具之后。
4. **主界面要不要内嵌工具**（同窗口 tab）：v1 建议不做，工具一律独立窗口。
