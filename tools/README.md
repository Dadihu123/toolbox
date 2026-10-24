# 官方工具目录

这个目录**就是**工具箱的官方工具列表。

- 每个子目录是一个工具，必须包含 `tool.json` 和 `icon.svg`（或 png），入口文件由
  `tool.json` 的 `entry` 指定。
- 工具箱启动时扫描本目录，把每个合法工具标为 `official`，在主界面的「工具市场」
  里作为官方工具展示，并默认启用。
- 打包脚本（`scripts/make-portable.ps1`）会把整个 `tools/` 原样复制到 exe 旁边，
  所以这里新增一个目录 = 发行版多一个官方工具，不需要改任何代码。

## 加一个官方工具

```
tools/
└── my-tool/
    ├── tool.json
    ├── icon.svg
    └── index.html
```

`tool.json` 最小示例：

```json
{
  "manifestVersion": 1,
  "bridgeVersion": 2,
  "id": "my-tool",
  "name": "我的工具",
  "description": "一句话说明",
  "locales": {
    "en-US": {
      "name": "My tool",
      "description": "One line about it",
      "actions": { "open": "Open my tool" }
    }
  },
  "version": "0.1.0",
  "author": "yhmr",
  "entry": "index.html",
  "icon": "icon.svg",
  "accent": "#4ea1ff",
  "window": { "width": 1100, "height": 760, "minWidth": 720, "minHeight": 480 },
  "permissions": { "data": true },
  "actions": [
    { "id": "open", "title": "打开我的工具", "defaultHotkey": "Ctrl+Alt+1" }
  ]
}
```

约束：

- `id` 必须与目录名一致，只允许小写字母、数字、`-`、`_`。
- `entry` 必须存在且不能指向目录外。
- 工具应当能独立运行（浏览器直接打开入口文件即可用），这样便于开发和调试。
- 官方工具随包分发，不可在界面里卸载，只能停用。

## 多语言（0.2.0）

- `manifestVersion` 仍是 1；`bridgeVersion` 现在接受 **1 和 2**（写成 2 表示你用了下面的
  新字段；写 1 也能加载，走同一个 shim）。
- `locales` 是**可选**的内联对象，键是 BCP-47 语言标签。每个语言可以只翻译一部分：
  `name` / `description` / `actions`（action id → 标题）。缺的部分回退到顶层的基础文案，
  所以顶层的中文文案永远要写。
- 匹配顺序：精确标签（`en-US`）→ 语言前缀（`en`，因此 `en-GB` 也能拿到 `en-US` 的文案）
  → 基础文案。
- 切换语言时，工具窗口标题、快捷键页的动作名、安装审阅页都会用新语言重新渲染；
  工具箱还会把 `theme` / `locale` / `availableLocales` 塞进 `window.__toolbox`，并在变化时
  派发 `theme`、`locale` 事件。**页面里判断语言请用 `__toolbox.locale`，不要用
  `navigator.language`**——运行期切换语言时后者不会变。
- 工具本体要不要跟着翻译由你自己决定；`sprite-align` 目前只翻译了清单元数据。

## 第三方工具

用户从 zip 安装的工具放在 `<data>/tools/`，标为 `installed`，优先级高于官方目录
（同 id 会覆盖官方工具——但安装流程会拒绝覆盖内置工具，避免被替换）。
