# i18n glossary

One term per concept, in the order they appear in the UI. Use these strings verbatim in
both word lists (`ui/locales/*` for the webviews, `src-tauri/locales/*` for native menus
and Rust-produced messages) and in the documentation.

Column notes: **zh-CN** is the source language (write it first), **en-US** is the
translation. "Do not use" lists the tempting synonyms that would make the two languages
drift apart.

| Concept | zh-CN | en-US | Do not use (en) | Notes |
| --- | --- | --- | --- | --- |
| The product | 工具箱 | Toolbox | — | Never "the toolkit", never translated |
| Floating ball | 悬浮球 | floating ball | orb, bubble, ball widget | Menu: "ball". Code: `windows::ball` |
| Search panel | 面板 / 工具面板 | panel | popup, launcher, palette | First mention "search panel", then "panel" |
| Main window | 主界面 | main window | main screen, dashboard | |
| Hover quick icons | 悬停快捷图标 | quick menu | hover icons, fan menu, radial menu | The native fan around the ball |
| Quick-menu slot | 槽位 | slot | cell | Settings → 悬停快捷图标 |
| Tool | 工具 | tool | plugin, extension, app | A tool is a static site + `tool.json` |
| Tool market | 工具市场 | tool market | store, gallery | Official tools ship with the app |
| Tool manifest | 清单 / `tool.json` | manifest | config, descriptor | |
| Tool window | 工具窗口 | tool window | tool view | One webview per tool |
| Host (window) | 宿主 | host | shell, core | The Rust process; host windows are panel/main |
| Bridge | 桥接 | bridge | API layer, IPC | `window.__toolbox` |
| Bridge version | Bridge v2 | Bridge v2 | API version | `bridgeVersion` in `tool.json` |
| Action | 功能 / 动作 | action | command, task | "功能" in the panel, "动作" in code |
| Hotkey | 快捷键 | shortcut | hot key, keybinding | "hotkey" only for the global ones |
| Shortcut conflict | 冲突 | conflicts with … | clash | Show the other action's title |
| Install review | 安装审查 | install review | permission dialog | Per-permission checkboxes |
| Permission | 权限 | permission | capability (in UI) | `capability` is the Rust/Tauri term |
| Third-party tool | 第三方工具 | third-party tool | external tool | `source: "installed"` |
| Portable mode | 便携模式 | portable | standalone | Data directory next to the exe |
| Theme | 主题 | theme | colour scheme | Follows Windows unless pinned |
| Follow system | 跟随系统 | follow system | automatic, auto | Shows the resolved value next to it |
| System theme | 系统主题 | system theme | OS theme | HKCU `AppsUseLightTheme` |
| Light / dark | 浅色 / 深色 | light / dark | bright, night | |
| Language | 语言 | language | locale (in UI) | `locale` is the code-level term |
| Resolved theme/locale | 当前生效 | resolved | effective | Not user-facing text |
| WebView2 profile | WebView2 配置（profile） | WebView2 profile | user data dir | Shared by every window |

## Conventions

- Sentence case for English UI text ("Open panel", not "Open Panel"); Chinese needs no case.
- Ellipsis: use the single character `…` in both languages when a control opens a dialog.
- No trailing punctuation on buttons, labels and menu items; full stops on sentences.
- Counts: Chinese `搜索结果 · 12`, English `Results · 12` — the separator is part of the
  word list, not the code.
- Action titles in the hotkey table are `<tool name><separator><action title>`; the
  separator is a translated string (`native.action.join`), not a hard-coded `：`.
- Never translate: tool ids, file paths, `toolbox://` URLs, permission keys
  (`files.open`, `network`, …), shortcut strings (`Ctrl+Shift+T`).
