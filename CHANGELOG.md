# Changelog

## 0.1.0 — unreleased

First packaged build of the host: portable folder/zip plus an NSIS installer.

### Host

- Native Win32 floating ball: layered popup window, right-edge dock, 14 px peek,
  hover slide-out, drag + edge snapping, right-click menu, per-monitor DPI.
  Software-rasterised (4×4 supersampling) so it does not depend on GDI+/Direct2D.
- Global hotkeys registered per action, with conflict detection and rebinding.
  Actions marked `requiresOpen` only bind while their tool window is open. Tools can add
  actions at runtime through the bridge; those are registered while the window is open and
  dropped when it closes.
- Search panel (`Ctrl+Shift+T`): searches tool names and tool actions, keyboard driven,
  anchored next to the ball. Hiding it keeps the webview warm for 5 minutes
  (`TOOLBOX_PANEL_WARM_SECS` overrides that for development) so reopening is instant, then
  destroys it to give the WebView2 footprint back. Reopening inside the window cancels the
  scheduled teardown.
- Main window (`Ctrl+Shift+M`): tools, hotkeys, settings, about, official tool market.
- Tray icon with panel / main / quit.
- Single instance: a second launch forwards its CLI arguments into the running app.
- CLI: `--tool <id>`, `--open <file>`, `--panel`, `--main`, `--quiet`, `--data-dir <path>`.
  A bare file path (shell file association / drag onto the exe) and `--open` without
  `--tool` resolve the tool from its `accepts` list, falling back to the last used tool.
  Files are queued until the tool page reports ready, then delivered as `files-dropped`.

### Tool sandbox

- Each tool runs in its own webview at `toolbox://localhost/<toolId>/`, isolated per tool.
- Zero Tauri permissions for tool windows; all privileged work goes through
  `bridge_call`, which resolves the calling tool from the window label (never from the
  payload) and checks the manifest permission matrix.
- Installed tools only get the permissions the user ticked in the review dialog
  (`registry::effective_permissions` clamps the grant to the manifest), enforced by the
  bridge at call time and exposed to the tool via `window.__toolbox.permissions`.
- Path traversal is rejected both lexically and after canonicalisation; the protocol
  handler only serves files inside the tool's own directory.
- CSP and `X-Content-Type-Options: nosniff` on every protocol response.
- Bridge v1: `files.open`, `files.save`, `data.*`, `clipboard.*`, `window.*`, `events.*`,
  `log.*`, `ready()`, and runtime actions (`actions.register` / `actions.unregister`).
- Host→tool events are delivered by evaluating the injected shim (`window.__toolboxEmit`);
  `webview.emit` never reaches a zero-permission tool window. `action` payloads carry
  `{ id, action, toolId }`; `files-dropped` carries `{ files: [{ name, path, size, exists }] }`.
- Zip install is two-phase: `inspect_tool_zip` unpacks and validates into `data/tmp` and
  reports metadata, size, upgrade status and one row per declared permission; the review
  dialog then lets the user tick the permissions to grant. Only the ticked subset is
  persisted in `tools.json`. Upgrades back the old version up to `data/backups/tools/`,
  built-in tools cannot be overwritten, and cancelling leaves nothing installed.

### Portability

- All state lives under `<exe>/data`, with a write probe and an `%LOCALAPPDATA%` fallback
  for read-only locations.
- WebView2 profile redirected to `data/webview2`, so Chromium caches never touch AppData.
- Atomic JSON writes; daily logs with 7-day retention.

### Packaging

- Portable: `scripts/make-portable.ps1` produces `dist/Toolbox-portable-<version>/` and a
  zip containing the exe, `tools/` and an empty `data/`.
- NSIS: `npx --yes @tauri-apps/cli@2 build` produces
  `src-tauri/target/release/bundle/nsis/Toolbox_0.1.0_x64-setup.exe`. Per-user install
  (`currentUser`), SimpChinese + English, custom installer icon, Start Menu shortcut and an
  optional desktop shortcut, and `tools/` bundled next to the exe via `bundle.resources`.
  `bundle.windows.webviewInstallMode = downloadBootstrapper` only downloads WebView2 when
  the runtime is missing. Uninstalling keeps the user's `data/` directory.
- Sizes: release exe 4.11 MB, portable zip 1.83 MB, NSIS installer 1.53 MiB.

### Tools

- `sprite-align` 1.0.0 as the first official tool, running unmodified under the protocol.
  Download interception routes its `<a download>` exports into `data/downloads`.

### Known limitations

- The NSIS installer is unsigned, so SmartScreen may warn; code signing is a roadmap item.
- The native zip picker cannot be scripted: install verification above the picker stays
  manual, while extraction/validation/commit are covered by Rust unit tests.
- Runtime actions only exist while their tool window is open (by design).
- No dev-mode tool loading with reload on change yet.
