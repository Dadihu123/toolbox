# Toolbox

Portable personal toolset host for Windows. A floating ball sits at the screen edge; a
hotkey opens a search panel; each tool runs in its own sandboxed webview.

> Status: M1/M2 milestone. Ball, panel, main window, hotkey registry, tool sandbox and
> bridge are implemented and verified end to end. A portable zip is produced by
> `scripts/make-portable.ps1`; an NSIS installer is not written yet.

## What works today

- **Floating ball** — native Win32 layered window (not a webview), docked to the right
  edge, collapsed to a 14 px sliver, slides out on hover, drag to move, snaps back to
  the nearest edge, right-click for a menu.
- **Panel** — `Ctrl+Shift+T`, search across tool names *and* tool actions, ↑↓/Enter/Esc,
  opens next to the ball.
- **Main window** — `Ctrl+Shift+M`, tools / hotkeys / settings / about + official tool
  market.
- **Tool sandbox** — every tool gets its own origin (`toolbox://localhost/<id>/`) and its
  own webview. Zero Tauri permissions; all privileged work goes through a capability
  checked bridge.
- **Hotkeys** — per-action, user rebindable, conflict detection, `requiresOpen` actions
  only register while their tool window exists.
- **Portable data** — everything under `<exe>/data`, including the WebView2 profile.

## Measured footprint

Release build, Windows 11, no window open:

| State | Host RSS | WebView2 processes | Total | CPU |
| --- | --- | --- | --- | --- |
| Idle (ball only) | **11.8 MB** | 0 | **11.8 MB** | 0% |
| Panel open | 26.5 MB | 6 / 338.6 MB | 365.1 MB | 0% |

The idle number is the one that matters: the ball is a native layered window, so an
idle Toolbox is a single ~12 MB process with no Chromium at all. The first window pays
the fixed WebView2 cost; closing it releases those processes again. Re-measure with
`scripts/measure-idle.ps1`; history lives in `benchmarks/idle.json`.

Release binary: **4.05 MB**, portable zip: **1.8 MB**.

## Layout

```
Toolbox/
├── src-tauri/          Rust host
│   ├── src/
│   │   ├── lib.rs        bootstrap, tray, single instance, CLI
│   │   ├── paths.rs      portable path resolution
│   │   ├── state.rs      config.json / hotkeys.json / tools.json
│   │   ├── registry.rs   tool discovery + manifest validation
│   │   ├── actions.rs    action registry + global hotkeys
│   │   ├── bridge.rs     toolbox:// protocol + bridge + permission checks
│   │   ├── install.rs    zip install / uninstall
│   │   ├── ipc.rs        host-only commands
│   │   └── windows/      ball (native), panel, main, tool windows
│   └── capabilities/
├── ui/
│   ├── panel.html      search panel (no framework, ~13 KB)
│   ├── main.html       main window
│   └── shared/host.js  IPC bridge with a browser-preview mock
├── tools/              official tools, shipped as-is
│   └── sprite-align/
└── docs/
```

At runtime the executable expects `tools/` and `data/` next to itself.

## Develop

```powershell
cd src-tauri
cargo build            # debug build
.\target\debug\toolbox.exe --main      # open the main window directly
.\target\debug\toolbox.exe --tool sprite-align
.\target\debug\toolbox.exe --panel     # open the search panel
.\target\debug\toolbox.exe --quiet     # ball only, no panel
```

`tools/` must be copied next to the binary for tool discovery:
`Copy-Item ..\tools .\target\debug\tools -Recurse`.

The UI files are plain HTML and can be opened directly in a browser — `ui/main.html`
and `ui/panel.html` fall back to mock data when `window.__TAURI_INTERNALS__` is absent,
so layout work needs no Rust rebuild.

## Build a portable release

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\make-portable.ps1
```

Produces `dist/Toolbox-portable-<version>/` and a matching `.zip` containing
`toolbox.exe`, `tools/` and an empty `data/`. Copy the folder anywhere and run the exe;
upgrading means replacing `toolbox.exe` while `tools/` and `data/` stay untouched.

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
├── tools.json         install records, enabled flags
├── tool-data/<id>/    per-tool key/value store (bridge `data.*`)
├── logs/              daily log, 7 day retention
├── downloads/         tool download sink
└── webview2/          WebView2 profile (keeps Chromium caches out of AppData)
```

Deleting `data/` fully resets the app.

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
  ]
}
```

- `id` must match the directory name and use only lowercase letters, digits, `-`, `_`.
- The tool keeps working when opened directly in a browser; the bridge is a no-op then.
- Declare only the permissions you need — undeclared capabilities are rejected by the
  host with an explicit error.

Permissions: `filesOpen`, `filesSave`, `clipboardRead`, `clipboardWrite`, `data`,
`externalOpen`, `network` (array of allowed hosts; empty = offline).

Bridge surface (`window.__toolbox`):

```js
await toolbox.files.save({ suggestedName: 'out.zip', data: base64 });
await toolbox.files.open({ accept: ['png'], multiple: true });
await toolbox.data.set('key', value);
toolbox.events.on('action', (actionId) => { /* hotkey fired */ });
await toolbox.ready();
```

## Architecture notes

See `docs/design.md` for the full design (in the parent repository) and the decision log.

Key constraints discovered while building, worth knowing before changing window code:

- **A webview window must be created on the event-loop thread.** Creating one from a
  global-shortcut callback or from `setup` fails with `os error 5`. All window creation
  goes through `windows::panel::defer_main`.
- **The app must outlive its windows.** Toolbox starts with zero Tauri windows, so
  `RunEvent::ExitRequested` is intercepted to `prevent_exit()` — otherwise the process
  exits immediately on launch.
- **A layered window's DIB must stay selected** into the memory DC while calling
  `UpdateLayeredWindow`, or nothing is painted.
- **Win32 rects are physical pixels, Tauri positions are logical.** The panel anchor
  converts explicitly; mixing them silently pushes windows off screen at 150% scaling.
- **`WM_MOUSELEAVE` is not exported by the `windows` crate.** Referencing it directly in
  a `match` arm silently becomes a catch-all binding — it is declared as `MSG_MOUSELEAVE`.

## Roadmap

- NSIS installer + release signing
- Tool install UI: permission review before install
- Runtime action registration from open tools
- Panel warm-keep instead of destroy-on-hide
- `--open` file association wiring for sprite-align
