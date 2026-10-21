# Changelog

## 0.1.0 — unreleased

First working build of the host. Not yet packaged.

### Host

- Native Win32 floating ball: layered popup window, right-edge dock, 14 px peek,
  hover slide-out, drag + edge snapping, right-click menu, per-monitor DPI.
  Software-rasterised (4×4 supersampling) so it does not depend on GDI+/Direct2D.
- Global hotkeys registered per action, with conflict detection and rebinding.
  Actions marked `requiresOpen` only bind while their tool window is open.
- Search panel (`Ctrl+Shift+T`): searches tool names and tool actions, keyboard driven,
  anchored next to the ball.
- Main window (`Ctrl+Shift+M`): tools, hotkeys, settings, about, official tool market.
- Tray icon with panel / main / quit.
- Single instance: a second launch forwards its CLI arguments into the running app.
- CLI: `--tool <id>`, `--open <file>`, `--panel`, `--main`, `--quiet`, `--data-dir <path>`.

### Tool sandbox

- Each tool runs in its own webview at `toolbox://localhost/<toolId>/`, isolated per tool.
- Zero Tauri permissions for tool windows; all privileged work goes through
  `bridge_call`, which resolves the calling tool from the window label (never from the
  payload) and checks the manifest permission matrix.
- Path traversal is rejected both lexically and after canonicalisation; the protocol
  handler only serves files inside the tool's own directory.
- CSP and `X-Content-Type-Options: nosniff` on every protocol response.
- Bridge v1: `files.open`, `files.save`, `data.*`, `clipboard.*`, `window.*`, `events.*`,
  `log.*`, `ready()`.
- Zip install / uninstall with manifest validation, id rules, entry check, size limits,
  and refusal to overwrite bundled tools.

### Portability

- All state lives under `<exe>/data`, with a write probe and an `%LOCALAPPDATA%` fallback
  for read-only locations.
- WebView2 profile redirected to `data/webview2`, so Chromium caches never touch AppData.
- Atomic JSON writes; daily logs with 7-day retention.

### Tools

- `sprite-align` 1.0.0 as the first official tool, running unmodified under the protocol.
  Download interception routes its `<a download>` exports into `data/downloads`.

### Known limitations

- Packaging (NSIS / portable zip) not written yet.
- Tool install has no permission review screen; permissions are shown after install.
- Panel is destroyed on close rather than kept warm.
- No runtime action registration from open tools yet.
