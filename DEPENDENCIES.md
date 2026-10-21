# Dependencies

## Runtime

| Component | Why | Where it comes from |
| --- | --- | --- |
| **WebView2 Runtime** | Chromium engine used to render the panel, main window and every tool | Preinstalled on Windows 11. On Windows 10 it ships with Edge; the installer can fetch it with `webview2InstallMode = "downloadBootstrapper"` |

Nothing else is required. The Rust host links the MSVC runtime statically by default
(`-C target-feature=+crt-static` is not set, so `vcruntime140.dll` is expected — present
on any machine with the VC++ redistributable, which is standard on Windows 10/11).

## Build

| Tool | Version used |
| --- | --- |
| Rust | 1.96 (edition 2021, `rust-version = 1.80`) |
| MSVC build tools | Visual Studio 2022 (x64) |
| Windows SDK | 10.0.26100 |

## Crates

The full graph is pinned in `src-tauri/Cargo.lock`. Direct dependencies:

| Crate | Purpose |
| --- | --- |
| `tauri` 2 | window / webview / tray / IPC runtime |
| `tauri-plugin-single-instance` | forward a second launch into the running instance |
| `tauri-plugin-global-shortcut` | `RegisterHotKey`-based global hotkeys |
| `tauri-plugin-dialog` | native open/save dialogs for the bridge |
| `tauri-plugin-opener` | reveal a folder in Explorer |
| `tauri-plugin-clipboard-manager` | clipboard access for tools that declare it |
| `serde` / `serde_json` | config and manifest (de)serialisation |
| `base64` | file payloads across the bridge |
| `uuid` | staging directory names during tool install |
| `zip` | third-party tool installation |
| `windows` | Win32 calls for the native floating ball (layered window, DPI, menus) |

## Licences

Run `cargo license` (or `cargo deny`) before publishing binaries. Tauri and its official
plugins are MIT/Apache-2.0; `windows` is MIT; `zip` is MIT.
