# Toolbox

Portable personal toolset host for Windows. A floating ball sits at the screen edge; a
hotkey opens a search panel; each tool runs in its own sandboxed webview.

> Status: 0.1.0 milestone. Ball, panel, main window, hotkey registry, tool sandbox/bridge,
> reviewed installs, runtime actions and packaging are implemented and verified end to end.
> `scripts/make-portable.ps1` produces the portable folder + zip; `tauri build` produces an
> NSIS installer.

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

Release binary: **4.11 MB**, portable zip: **1.83 MB**, NSIS installer: **1.53 MiB**.

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
├── ui/
│   ├── panel.html      search panel (no framework, ~13 KB)
│   ├── main.html       main window
│   └── shared/host.js  IPC bridge with a browser-preview mock
├── tools/              official tools, shipped as-is
│   └── sprite-align/
├── scripts/            portable build + idle baseline
├── benchmarks/         measured resource baselines
└── docs/               design.md (design + decision log)
```

At runtime the executable expects `tools/` and `data/` next to itself.

## Develop

```powershell
cd src-tauri
cargo build            # debug build
cargo test             # unit tests: manifest, permissions, install inspection/commit
.\target\debug\toolbox.exe --main      # open the main window directly
.\target\debug\toolbox.exe --tool sprite-align
.\target\debug\toolbox.exe --panel     # open the search panel
.\target\debug\toolbox.exe --quiet     # ball only, no panel
```

`tools/` must be copied next to the binary for tool discovery:

```powershell
Copy-Item ..\tools .\target\debug\ -Recurse -Force
```

The UI files are plain HTML and can be opened directly in a browser — `ui/main.html` and
`ui/panel.html` fall back to mock data when `window.__TAURI_INTERNALS__` is absent, so layout
work needs no Rust rebuild.

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
├── config.json        appearance, ball position, panel hotkey
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

## Writing a tool

A tool is a static site plus a `tool.json`:

```json
{
  "manifestVersion": 1,
  "bridgeVersion": 1,
  "id": "my-tool",
  "name": "我的工具",
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

// Host -> tool events. Actions include manifest actions and runtime ones.
toolbox.events.on('action', ({ id, action, toolId }) => { /* hotkey fired */ });
toolbox.events.on('files-dropped', ({ files }) => {
  // files: [{ name, path, size, exists }]
});

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
