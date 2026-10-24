# Toolbox

Portable personal toolset host for Windows. A floating ball sits at the screen edge; a
hotkey opens a search panel; each tool runs in its own sandboxed webview.

> Status: 0.2.0 milestone. Ball, panel, main window, hotkey registry, tool sandbox/bridge,
> reviewed installs, runtime actions and packaging are implemented and verified end to end.
> 0.2.0 adds the environment layer: light/dark themes that follow Windows (or are pinned),
> zh-CN/en-US for both the host UI and tool metadata, a Vite build for the host UI, and
> Bridge v2. `scripts/make-portable.ps1` produces the portable folder + zip; `tauri build`
> produces an NSIS installer.

## What works today

- **Floating ball** — native Win32 layered window (not a webview), docked to the right
  edge, collapsed to a 14 px sliver, slides out on hover, right-click for a menu. Drag it
  anywhere: it follows the pointer 1:1 and only docks when released within 24 px of a side
  edge, otherwise it stays exactly where you dropped it (position and dock state persist).
- **Quick icons** — rest the pointer on the ball and the tools you picked fan out around
  it: a half fan opening away from the screen edge while docked, a full circle while
  floating, one or two rings depending on the count. The fan is native (no webview), stays
  open while you move onto an icon, hides while the ball is being dragged, and a click
  opens that tool. Slots per ball position, icon count and rings are configured in
  Settings → 悬停快捷图标, with a live hover preview.
- **Panel** — `Ctrl+Shift+T`, search across tool names *and* tool actions, ↑↓/Enter/Esc,
  opens next to the ball. Drag it by the search bar; it closes when focus really leaves
  (clicks inside it do not count). Hiding it keeps the webview warm for 5 minutes (instant
  reopen), then releases it so the idle footprint comes back.
- **Main window** — `Ctrl+Shift+M`, tools / hotkeys / settings / about + official tool
  market.
- **Tool sandbox** — every tool gets its own origin (`toolbox://localhost/<id>/`) and its
  own webview. Zero Tauri permissions; all privileged work goes through a capability
  checked bridge. Tool windows are normal app windows: native title bar with
  minimise/maximise/close, resize borders, drag by the caption, and the tool's own icon in
  the title bar and taskbar.
- **Reviewed installs** — a zip is unpacked and validated first, then shown in a permission
  review dialog (author/version/size, one checkbox per permission, upgrade notice).
  Only the ticked permissions are granted and persisted; the bridge enforces the grant at
  runtime, so an unchecked capability fails with an explicit error.
- **Hotkeys** — per-action, user rebindable, conflict detection, `requiresOpen` actions
  only register while their tool window exists. Tools can additionally register actions at
  runtime with `toolbox.actions.register`; those only live while the window is open.
- **File handoff** — `--open`, a shell file association, or dragging a file onto the exe
  routes it to the tool that declares it accepts that file type (falling back to the last
  used tool), delivered as a `files-dropped` event once the page reports ready.
- **Portable data** — everything under `<exe>/data`, including the WebView2 profile.
- **Theme** — light and dark token sets for the panel, the main window and every tool
  window's native title bar. Defaults to following Windows, and keeps following it live
  (`WM_SETTINGCHANGE` → the ball's window procedure re-reads the registry and re-tints
  every open webview). Settings → 外观 can pin 浅色 or 深色; the caption bar, the page
  tokens and `prefers-color-scheme` all switch in one step, without a reload. The search
  panel sits on a real acrylic backdrop (window-level blur; it falls back to the opaque
  panel when the compositor refuses). The floating ball and the hover fan are unchanged by
  design.
- **Language** — zh-CN (source) and en-US, following the Windows display language by
  default and pinnable in Settings. The host UI, the tray/ball menus, tool window titles,
  the install review dialog and the permission labels all follow it; running tools see
  `__toolbox.locale` (Bridge v2) instead of `navigator.language`.

## Measured footprint

Release build, Windows 11, 2560×1600 @150%:

| State | Host RSS | WebView2 processes | Total | CPU |
| --- | --- | --- | --- | --- |
| Idle (ball only) | **11.7 MB** | 0 | **11.7 MB** | 0% |
| Panel open | 26.7 MB | 6 / 316.7 MB | 343.4 MB | 0% |
| Panel hidden, still warm | 26.7 MB | 6 / 323.5 MB | 350.2 MB | 0% |

The idle number is the one that matters: the ball is a native layered window, so an idle
Toolbox is a single ~12 MB process with no Chromium at all. The first window pays the fixed
WebView2 cost; closing a tool window releases those processes again. The panel is the one
deliberate exception — it stays warm for 5 minutes after being hidden
(`TOOLBOX_PANEL_WARM_SECS` overrides that in development), then its webview is destroyed and
the footprint drops back to idle. Re-measure with `scripts/measure-idle.ps1`; history lives
in `benchmarks/idle.json`.

Release binary: **4.11 MB**, portable zip: **1.83 MB**, NSIS installer: **1.53 MiB**
(0.1.0 numbers — re-run `scripts/make-portable.ps1` and `measure-idle.ps1` before shipping
0.2.0: the built host UI and the two word-list sets are embedded in the exe now).

## Layout

```
Toolbox/
├── src-tauri/          Rust host
│   ├── src/
│   │   ├── lib.rs        bootstrap, tray, single instance, CLI
│   │   ├── paths.rs      portable path resolution
│   │   ├── state.rs      config.json / hotkeys.json / tools.json
│   │   ├── registry.rs   tool discovery + manifest validation + effective permissions
│   │   ├── actions.rs    action registry + global hotkeys + runtime actions
│   │   ├── bridge.rs     toolbox:// protocol + shim + bridge + permission checks
│   │   ├── install.rs    two-phase zip install (inspect → review → commit) / uninstall
│   │   ├── ipc.rs        host-only commands
│   │   └── windows/      ball (native), panel, main, tool windows
│   └── capabilities/
├── ui/                 host UI source (Vite root, built into ui/dist)
│   ├── panel.html/.js  search panel (no framework, zero-dependency DOM)
│   ├── main.html       main window (plain HTML + one inline module script)
│   ├── main-i18n.js, panel-i18n.js   per-page i18next bundles
│   ├── shared/         theme.css (design tokens), host.js (IPC + browser mock),
│   │                   i18n-core.js (i18next bootstrap + data-i18n)
│   └── locales/<locale>/{common,panel,main}.json   webview word lists
├── tools/              official tools, shipped as-is
│   └── sprite-align/
├── scripts/            portable build, idle baseline, i18n:check
├── benchmarks/         measured resource baselines
└── docs/               design.md (design + decision log), i18n-glossary.md
```

At runtime the executable expects `tools/` and `data/` next to itself.

## Develop

The host UI is a Vite multi-page build (`ui/` → `ui/dist`); the Rust side embeds the
built output at compile time, so build the UI **before** cargo.

```powershell
pnpm install           # once
pnpm dev               # UI preview at http://127.0.0.1:5173 (mock host bridge)
pnpm build             # ui/dist — required by cargo build below
pnpm i18n:check        # word-list checks (key parity, no empty values, no CJK in en-US)

cd src-tauri
cargo build            # debug build (embeds the current ui/dist)
cargo test             # unit tests: manifest/bridge versions, permissions, i18n word lists,
                       # config migration, install inspection/commit
.\target\debug\toolbox.exe --main      # open the main window directly
.\target\debug\toolbox.exe --panel     # open the search panel
.\target\debug\toolbox.exe --quiet     # ball only, no panel
```

`tools/` must be copied next to the binary for tool discovery:

```powershell
Copy-Item ..\tools .\target\debug\ -Recurse -Force
```

`pnpm dev` is the UI iteration loop: both pages load `ui/shared/host.js`, which falls back to
mock data when `window.__TAURI_INTERNALS__` is absent. Opening the HTML files straight from
disk (`file://`) no longer works — they are ES modules with imports now. Language preview:
`?lng=en-US`, and `?lng=en-XA` (dev only) wraps every string in `⟦…⟧` so hard-coded copy
stands out. Theme preview: the page follows the OS unless `window.__TOOLBOX_ENV__` is
injected, so use the DevTools "emulate prefers-color-scheme" switch.

## Build a portable release

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\make-portable.ps1
```

Produces `dist/Toolbox-portable-<version>/` and a matching `.zip` containing
`toolbox.exe`, `tools/` and an empty `data/`. Copy the folder anywhere and run the exe;
upgrading means replacing `toolbox.exe` while `tools/` and `data/` stay untouched.

## Build an installer

```powershell
npx --yes @tauri-apps/cli@2 build
```

Produces `src-tauri/target/release/bundle/nsis/Toolbox_0.1.0_x64-setup.exe` (~1.5 MiB). The
installer is per-user (`currentUser`, no admin prompt), ships `tools/` next to the exe via
`bundle.resources`, and only downloads the WebView2 bootstrapper when the runtime is missing
(`bundle.windows.webviewInstallMode = downloadBootstrapper`). The first build downloads the
Tauri CLI and the NSIS toolchain, so it needs network. The installer is not signed yet, so
SmartScreen may warn. Uninstalling keeps `<install dir>\data` — delete it manually for a
clean removal.

## Data directory

Resolution order:

1. `--data-dir <path>`
2. `TOOLBOX_HOME` environment variable
3. `<exe>/data` — the portable default, used when writable
4. `%LOCALAPPDATA%\Toolbox\data` — fallback for read-only locations (Program Files,
   locked sync folders); the UI then shows a "non-portable" badge

```
data/
├── config.json        theme/locale choice, ball position, panel hotkey (v2)
├── hotkeys.json       per-action overrides
├── tools.json         install records, enabled flags, granted permissions
├── tools/             installed third-party tools
├── tool-data/<id>/    per-tool key/value store (bridge `data.*`)
├── tmp/               install staging (cleaned on the next install attempt)
├── backups/tools/     previous versions of upgraded tools
├── logs/              daily log, 7 day retention
├── downloads/         tool download sink
└── webview2/          WebView2 profile (keeps Chromium caches out of AppData)
```

Deleting `data/` fully resets the app. The NSIS installer is per-user: it installs under
`%LOCALAPPDATA%\Toolbox`, so `data/` is created there on first run and is writable.

`config.json` is versioned (`CONFIG_VERSION` 2). v1 files are migrated on load: `theme` and
`locale` used to be placeholders that were never shown in the UI, so a v1 file is reset to
`"system"` for both. Hand-edited values outside `system | light | dark` and
`system | zh-CN | en-US` are normalised to `"system"`, and a UTF-8 BOM is tolerated.

## Writing a tool

A tool is a static site plus a `tool.json`:

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
  "author": "you",
  "entry": "index.html",
  "icon": "icon.svg",
  "accent": "#4ea1ff",
  "window": { "width": 1100, "height": 760, "minWidth": 720, "minHeight": 480 },
  "permissions": { "filesSave": true, "data": true },
  "actions": [
    { "id": "open", "title": "打开我的工具", "defaultHotkey": "Ctrl+Alt+1" }
  ],
  "accepts": ["image/png"]
}
```

- `id` must match the directory name and use only lowercase letters, digits, `-`, `_`.
- `bridgeVersion` 1 and 2 are both accepted; 2 adds the environment fields below to
  `window.__toolbox`. A v1 tool loads unchanged through the same shim. A manifest asking for
  a newer bridge is refused with an explicit reason.
- `locales` is optional and inline (manifestVersion stays 1). Keys are BCP-47 tags; a
  language may translate any subset, and anything missing falls back to the base
  `name` / `description` / `actions[].title`. Matching is exact tag → language prefix →
  base string, so `en-GB` also gets `en-US` text.
- The tool keeps working when opened directly in a browser; the bridge is a no-op then.
- Declare only the permissions you need — undeclared capabilities are rejected by the
  host with an explicit error. Third-party installs additionally get only the permissions
  the user ticked in the review dialog.
- `accepts` lists MIME types so `--open` / file associations can route files to the tool.

Permissions: `filesOpen`, `filesSave`, `clipboardRead`, `clipboardWrite`, `data`,
`externalOpen`, `network` (array of allowed hosts; empty = offline).

Bridge surface (`window.__toolbox`):

```js
await toolbox.files.save({ suggestedName: 'out.zip', data: base64 });
await toolbox.files.open({ accept: ['png'], multiple: true });
await toolbox.data.set('key', value);

// Environment (Bridge v2). `locale` is authoritative: navigator.language does not
// follow a runtime language switch, and the host may pin a theme that differs
// from the OS.
toolbox.version;              // 2
toolbox.theme;                // 'dark' | 'light'  (initial value)
toolbox.locale;               // 'zh-CN' | 'en-US' (initial value; update on the event)
toolbox.availableLocales;     // ['zh-CN', 'en-US']

// Host -> tool events. Actions include manifest actions and runtime ones.
toolbox.events.on('action', ({ id, action, toolId }) => { /* hotkey fired */ });
toolbox.events.on('files-dropped', ({ files }) => {
  // files: [{ name, path, size, exists }]
});
toolbox.events.on('theme', (theme) => { /* 'dark' | 'light' */ });
toolbox.events.on('locale', (locale) => { /* 'zh-CN' | 'en-US' */ });

// Register a runtime action; the hotkey exists only while this window is open.
const unregister = await toolbox.actions.register({
  id: 'flip',
  title: '水平翻转',
  defaultHotkey: 'Ctrl+Alt+F',
  handler: ({ id }) => { /* hotkey fired */ },
});

await toolbox.ready();
```

## Architecture notes

See `docs/design.md` for the full design and the decision log.

Key constraints discovered while building, worth knowing before changing window code:

- **A webview window must be created on the event-loop thread.** Creating one from a
  global-shortcut callback or from `setup` fails with `os error 5`. All window creation
  goes through `windows::panel::defer_main`.
- **The app must outlive its windows.** Toolbox starts with zero Tauri windows, so
  `RunEvent::ExitRequested` is intercepted to `prevent_exit()` — otherwise the process
  exits immediately on launch.
- **Tool windows cannot receive Tauri events.** They have zero Tauri permissions, so
  `webview.emit` never arrives. Host→tool events are delivered by evaluating the injected
  shim (`window.__toolboxEmit`) with `win.eval` — the same approach the host windows use for
  `__hostEmit`.
- **Hiding the panel is not closing it.** `panel::hide` schedules a teardown thread; a
  reopen bumps a token that cancels it, so the 5-minute keep-warm never races a new open.
- **A layered window's DIB must stay selected** into the memory DC while calling
  `UpdateLayeredWindow`, or nothing is painted.
- **Win32 rects are physical pixels, Tauri positions are logical.** The panel anchor
  converts explicitly; mixing them silently pushes windows off screen at 150% scaling.
- **`WM_MOUSELEAVE` is not exported by the `windows` crate.** Referencing it directly in
  a `match` arm silently becomes a catch-all binding — it is declared as `MSG_MOUSELEAVE`.

## Roadmap

- Release signing (the NSIS installer is unsigned → SmartScreen warning)
- Dev-mode tools (`--dev-tool <path>`) with reload on change
- Multi-monitor ball position memory
- Auto-update for the portable build (today: replace the exe, keep `tools/` and `data/`)
