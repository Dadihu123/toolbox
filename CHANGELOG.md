# Changelog

## 0.2.0 — unreleased

Environment and language: light/dark themes and zh-CN/en-US across the host UI, the native
windows and tool metadata.

### Host (Rust)

- **Environment service** (`env.rs`): resolves the effective theme from `Config.theme`
  (`system | light | dark`) and the display language from `Config.locale`
  (`system | zh-CN | en-US`). `system` re-reads Windows every time (`AppsUseLightTheme` +
  high contrast, exactly like tao's `should_use_dark_mode`; `GetUserPreferredUILanguages`
  with `MUI_LANGUAGE_NAME` for the UI language).
- **Live theme switching**: one service drives every webview — the native caption via
  `Window::set_theme`, `prefers-color-scheme` via
  `ICoreWebView2_13::Profile::SetPreferredColorScheme`. The ball's window procedure handles
  `WM_SETTINGCHANGE` (not filtered on `lParam`), re-resolves and pushes the result to the
  panel, the main window and every open tool window; measured ~90 ms, no reload. Switching
  back to `system` sets both halves explicitly (`None` + `AUTO`).
- **Shared WebView2 profile discipline**: every window is created with
  `.theme(Some(resolved))`. Without that, creating a window would stamp the *OS* theme onto
  the shared profile and undo an explicit light/dark choice (measured, not assumed). Tool
  windows and the main window also get a matching `background_color`, removing the white
  first-frame flash.
- **Config v2** (`state.rs`): `CONFIG_VERSION` 1 → 2. A v1 file resets `theme`/`locale` to
  `"system"` (the old values were placeholders that were never shown in the UI), unusable
  values are normalised, and a UTF-8 BOM in a hand-edited file no longer breaks loading.
- **Native translations** (`rust-i18n`, `src-tauri/locales/*.json`): tray menu, ball
  right-click menu, built-in action titles, hotkey-table reasons, the install review
  permission list and host-side error strings. The word list is selected before the tray is
  built; a language switch re-selects it and rebuilds the tray menu. Tool window titles
  follow the language too.
- Word-list rules are enforced by `cargo test` (same keys in both locales, no empty values,
  no CJK left in en-US).

### Host UI

- **Vite multi-page build**: `ui/` stays the source root and is built into `ui/dist`
  (`frontendDist: "../ui/dist"`), with `pnpm dev` for the browser preview (mock host
  bridge) and `pnpm build` required before `cargo build`. No React/TS, no framework added;
  the panel is still hand-written DOM.
- **Shared design tokens** (`ui/shared/theme.css`): one dark set (the 0.1.0 look, unchanged)
  and one light set. `data-theme` is applied before the first paint; a runtime switch
  re-points that attribute. `prefers-color-scheme` is only a preview fallback, never the
  driver of the host tokens.
- **Light theme** for the panel, the main window and the tool captions, with contrast fixes:
  `--accent-text` (`#1f6fd0`) for accent-coloured text on light surfaces, `--sel-title`
  instead of hard-coded `#fff`, and a darker light `--fg-faint` (`#5f6877`) that clears
  4.5:1 on every surface it is used on.
- **Frosted search panel**: the panel now has a real acrylic backdrop. CSS
  `backdrop-filter` cannot blur the desktop behind a transparent window, so the blur is
  applied by the compositor (`SetWindowCompositionAttribute` +
  `ACCENT_ENABLE_ACRYLICBLURBEHIND`) together with a rounded `SetWindowRgn` at the same
  radius as the CSS, otherwise the acrylic squares off the corners. The host reports
  success as `env.glass`; only then does the panel switch to its translucent surface —
  a see-through panel without blur would be unreadable. The acrylic tint follows theme
  switches, and `TOOLBOX_PANEL_GLASS=off|accent|full` exists for troubleshooting.
- Saturated border tints (`--accent-line`, `--ok-line`, `--warn-line`, `--danger-line`
  with `-strong`/`-soft` variants), the dimming layers (`--scrim`, `--scrim-soft`) and
  `--shadow-toast` are shared tokens now instead of page-local literals; the alpha
  differences of .02–.04 between call sites were merged, so the dark look is unchanged.
- **i18next** for the host UI: namespaces `common` / `panel` / `main`; the panel loads only
  the two it needs, with resources bundled (no extra round trip on the hotkey path). The
  settings page gains 主题 (跟随系统/浅色/深色) and 语言 (跟随系统/简体中文/English)
  controls that apply immediately. `pnpm dev?lng=en-XA` wraps every string in `⟦…⟧` to catch
  hard-coded copy.
- Panel search matches tool metadata in **every** shipped language (`searchText`), so "对齐"
  and "align" find the same tool.
- Fixed the hotkey capsule (`.kbd-btn`) never drawing its border: the global
  `button { border: 0 }` reset zeroed the width it only set a colour for.

### Tool contract (Bridge v2)

- `bridgeVersion` 1 and 2 are accepted; anything newer is refused with an explicit reason.
  A v1 tool loads through the v2 shim unchanged.
- `window.__toolbox` gains `theme`, `locale` and `availableLocales`, plus `theme` and
  `locale` events. `navigator.language` still does not follow a runtime language switch —
  `__toolbox.locale` is authoritative (README + design.md say so).
- `tool.json` gains an optional inline `locales` object (`name`, `description`,
  `actions[].title`); matching is exact tag → language prefix → base string, and the base
  strings stay the fallback. `sprite-align` ships English metadata and asks for
  `bridgeVersion: 2` (its page body is still Chinese by design).

### Tooling and docs

- `pnpm i18n:check` (`scripts/i18n-check.mjs`): namespace key parity, no empty values, no
  CJK in en-US, and every `t('ns:key')` / `data-i18n` reference must resolve.
- `scripts/make-portable.ps1` builds the UI first (`pnpm install --frozen-lockfile`,
  `pnpm build`) and fails clearly when pnpm is missing or `ui/dist` is absent.
- New `DEPENDENCIES.md` and `docs/i18n-glossary.md` (terminology lock); README updates
  (build steps, Bridge v2, `tool.json` example, config v2); design.md decision-log entries
  #6–#8.
- `ui/theme-preview.html` — a standalone page (no build, no network) that puts both token
  sets, the panel and the main window side by side and computes every WCAG contrast live.
  It was the review artefact for the light palette and stays as living documentation of it.

## 0.1.0 — unreleased

First packaged build of the host: portable folder/zip plus an NSIS installer.

### Host

- Native Win32 floating ball: layered popup window, right-edge dock, 14 px peek,
  hover slide-out, right-click menu, per-monitor DPI. Dragging follows the pointer 1:1
  (mouse capture, no drift), the ball only docks when released within 24 px of a side
  edge, and otherwise stays exactly where it was dropped; position and dock state are
  persisted in `config.json`. Software-rasterised (4×4 supersampling) so it does not
  depend on GDI+/Direct2D.
- Hover quick icons: resting the pointer on the ball fans the configured tools out
  around it — a half fan opening away from the screen edge while docked, a full circle
  while floating, one or two rings depending on the icon count. The fan is a native
  layered window (no webview), stays open while the pointer crosses from the ball onto
  the icons, never appears while the ball is being dragged, and clicking a chip opens
  that tool. Per-zone icon slots, count, rings and a live demo box live in
  Settings → 悬停快捷图标. The demo lays its chips out with the same chord-based
  radius as the native fan, keeps them inside the preview box and constrains each
  tool icon to its chip, so nothing overlaps or spills over the frame.
- Ball clicks are only accepted for a fresh press released on the visible disc, and
  the quick-menu poll no longer re-places the window under a resting pointer: clicking
  the ball opens the panel on the first click even while the fan is open.
- Global hotkeys registered per action, with conflict detection and rebinding.
  Actions marked `requiresOpen` only bind while their tool window is open. Tools can add
  actions at runtime through the bridge; those are registered while the window is open and
  dropped when it closes.
- Search panel (`Ctrl+Shift+T`): searches tool names and tool actions, keyboard driven,
  anchored next to the ball. Hiding it keeps the webview warm for 5 minutes
  (`TOOLBOX_PANEL_WARM_SECS` overrides that for development) so reopening is instant, then
  destroys it to give the WebView2 footprint back. Reopening inside the window cancels the
  scheduled teardown. The frameless panel can be dragged by its search bar and closes when
  focus really leaves it; clicks inside the panel no longer count as "outside" (WebView2
  bounces focus on every click, so the close is deferred and cancelled if focus returns).
  Its rounded corners are genuinely transparent — no clipped drop shadow behind them.
  Rapid clicks reuse the warm panel instead of racing to create a second webview with the
  same label (which used to log `open failed: a webview with label 'panel' already exists`).
- Main window (`Ctrl+Shift+M`): tools, hotkeys, settings, about, official tool market.
- Tray icon with panel / main / quit. Building it never blocks startup and never retries
  into duplicate icons: a session that refuses `Shell_NotifyIcon` only produces one warning
  (the ball's right-click menu offers the same commands).
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
- Tool windows are ordinary app windows: native caption with minimise / maximise / close,
  resize borders, drag by the title bar, dark caption, and the tool's own icon in both the
  title bar and the taskbar (`icon.svg` is rasterised with `resvg`; a generic glyph is used
  when a tool has no icon). `tool.json` sizes are sanitised, and a minimum size never
  exceeds the initial size.

### Known limitations

- The NSIS installer is unsigned, so SmartScreen may warn; code signing is a roadmap item.
- The native zip picker cannot be scripted: install verification above the picker stays
  manual, while extraction/validation/commit are covered by Rust unit tests.
- Runtime actions only exist while their tool window is open (by design).
- No dev-mode tool loading with reload on change yet.
