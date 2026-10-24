//! The `toolbox://` protocol and the tool bridge.
//!
//! Security model (see docs/design §5.3):
//!   * every tool gets its own origin: `http://toolbox.localhost/<toolId>/...`
//!   * the protocol handler resolves the tool from the request path and refuses
//!     any path that escapes the tool directory
//!   * the tool's identity comes from the *webview label* on every bridge call,
//!     so a tool can never claim to be another tool
//!   * every capability is checked against the manifest `permissions`

use std::borrow::Cow;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::http::{Request, Response, StatusCode};
use tauri::{AppHandle, Manager, UriSchemeContext};
use tauri_plugin_clipboard_manager::ClipboardExt;

use crate::paths;
use crate::registry;

// ------------------------------------------------------------- protocol

pub fn handle_protocol<R: tauri::Runtime>(
    _ctx: UriSchemeContext<'_, R>,
    request: Request<Vec<u8>>,
) -> Response<Cow<'static, [u8]>> {
    let path = request.uri().path().trim_start_matches('/');
    let (tool_id, rel) = match path.split_once('/') {
        Some((id, rest)) => (id.to_string(), rest.to_string()),
        None => (path.to_string(), String::new()),
    };

    let Some(manifest) = registry::find(&tool_id) else {
        return text_response(StatusCode::NOT_FOUND, "tool not found");
    };
    let root = PathBuf::from(&manifest.dir);

    let rel = if rel.is_empty() {
        manifest.entry.clone()
    } else {
        rel
    };
    let Some(target) = resolve_inside(&root, &rel) else {
        return text_response(StatusCode::FORBIDDEN, "path escapes tool root");
    };

    let bytes = match std::fs::read(&target) {
        Ok(b) => b,
        Err(_) => return text_response(StatusCode::NOT_FOUND, "not found"),
    };

    let mime = mime_for(&target);
    let csp = "default-src 'self' http://toolbox.localhost; \
               script-src 'self' 'unsafe-inline' 'unsafe-eval' http://toolbox.localhost; \
               style-src 'self' 'unsafe-inline'; \
               img-src 'self' data: blob: http://toolbox.localhost; \
               font-src 'self' data:; \
               connect-src 'self' http://toolbox.localhost; \
               worker-src 'self' blob:"
        .to_string();

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", mime)
        .header("Cache-Control", "no-cache")
        .header("Content-Security-Policy", csp)
        .header("X-Content-Type-Options", "nosniff")
        .body(Cow::Owned(bytes))
        .unwrap_or_else(|_| text_response(StatusCode::INTERNAL_SERVER_ERROR, "build failed"))
}

fn text_response(status: StatusCode, body: &str) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .status(status)
        .header("Content-Type", "text/plain; charset=utf-8")
        .body(Cow::Owned(body.as_bytes().to_vec()))
        .unwrap()
}

/// Lexical containment check, then canonical check when the file exists.
fn resolve_inside(root: &Path, rel: &str) -> Option<PathBuf> {
    let rel = rel.replace('\\', "/");
    let candidate = Path::new(&rel);
    if candidate.is_absolute() {
        return None;
    }
    for c in candidate.components() {
        match c {
            Component::Normal(_) | Component::CurDir => {}
            _ => return None,
        }
    }
    let joined = root.join(candidate);
    if joined.exists() {
        paths::ensure_inside(root, &joined)
    } else {
        None
    }
}

fn mime_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "wasm" => "application/wasm",
        "txt" | "md" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

// ----------------------------------------------------------------- shim

/// Script injected before the tool's own scripts run.
///
/// Bridge v2 surface: the same calls as v1 plus the resolved environment
/// (`theme`, `locale`, `availableLocales`) and the `theme` / `locale` events the
/// host dispatches through `window.__toolboxEmit`. Tools that only know v1 keep
/// working unchanged — the shim never removes a v1 member.
pub fn shim_script(
    manifest: &registry::Manifest,
    permissions: &registry::Permissions,
    theme: crate::env::Theme,
    locale: &str,
) -> String {
    let tool_id = json!(manifest.id);
    let perms = serde_json::to_string(permissions).unwrap_or_else(|_| "{}".into());
    let theme = json!(theme.as_str());
    let locale = json!(locale);
    let available_locales = json!(crate::env::LOCALES);
    format!(
        r#"(function () {{
  if (window.__toolbox) return;
  var TOOL_ID = {tool_id};
  var PERMS = {perms};
  var THEME = {theme};
  var LOCALE = {locale};
  var AVAILABLE_LOCALES = {available_locales};
  var listeners = Object.create(null);

  function call(method, args) {{
    return window.__TAURI_INTERNALS__.invoke('bridge_call', {{
      toolId: TOOL_ID, method: method, args: args || null
    }});
  }}

  window.__toolbox = {{
    // 2: adds theme / locale / availableLocales and the theme + locale events.
    version: 2,
    toolId: TOOL_ID,
    mode: 'hosted',
    permissions: PERMS,
    // Authoritative for the current language: `navigator.language` does not
    // follow a runtime language switch. Re-read on the `locale` event.
    locale: LOCALE,
    // Best-effort copy of the resolved theme; `prefers-color-scheme` is the
    // primary signal, and this one also tracks an explicit setting.
    theme: THEME,
    availableLocales: AVAILABLE_LOCALES,
    ready: function () {{ return call('__ready', null); }},
    files: {{
      open: function (opts) {{ return call('files.open', opts); }},
      save: function (opts) {{ return call('files.save', opts); }}
    }},
    clipboard: {{
      readText: function () {{ return call('clipboard.readText', null); }},
      writeText: function (t) {{ return call('clipboard.writeText', {{ text: t }}); }}
    }},
    data: {{
      get: function (key) {{ return call('data.get', {{ key: key }}); }},
      set: function (key, value) {{ return call('data.set', {{ key: key, value: value }}); }},
      remove: function (key) {{ return call('data.remove', {{ key: key }}); }}
    }},
    window: {{
      setTitle: function (t) {{ return call('window.setTitle', {{ title: t }}); }},
      close: function () {{ return call('window.close', null); }}
    }},
    events: {{
      on: function (type, cb) {{
        (listeners[type] || (listeners[type] = [])).push(cb);
        return function () {{
          var arr = listeners[type] || [];
          var i = arr.indexOf(cb);
          if (i >= 0) arr.splice(i, 1);
        }};
      }}
    }},
    actions: {{
      // Register an action contributed by the running tool. Resolves to an
      // unregister function. The hotkey only exists while this window is open.
      register: function (opts) {{
        opts = opts || {{}};
        var localId = opts.id;
        var handler = opts.handler;
        var unlisten = function () {{}};
        if (typeof handler === 'function') {{
          unlisten = window.__toolbox.events.on('action', function (detail) {{
            if (detail && detail.id === localId) {{
              try {{ handler(detail); }} catch (e) {{ console.error(e); }}
            }}
          }});
        }}
        return call('actions.register', {{
          id: localId,
          title: opts.title || localId,
          defaultHotkey: opts.defaultHotkey || ''
        }}).then(function () {{
          return function () {{
            unlisten();
            return call('actions.unregister', {{ id: localId }});
          }};
        }});
      }},
      unregister: function (id) {{ return call('actions.unregister', {{ id: id }}); }}
    }},
    log: {{
      info: function (m, d) {{ return call('log', {{ level: 'info', message: m, data: d }}); }},
      error: function (m, d) {{ return call('log', {{ level: 'error', message: m, data: d }}); }}
    }},
    _dispatch: function (type, payload) {{
      var arr = listeners[type] || [];
      for (var i = 0; i < arr.length; i++) {{
        try {{ arr[i](payload); }} catch (e) {{ console.error(e); }}
      }}
    }}
  }};

  // Host -> tool events (action dispatch, dropped files). Tool windows carry no
  // Tauri event permissions, so `webview.emit` never reaches them; the host eval's
  // `window.__toolboxEmit(type, payload)` instead (see windows/tools.rs::deliver).
  window.__toolboxEmit = function (type, payload) {{
    window.__toolbox._dispatch(type, payload);
  }};

  // tell the host we are alive so queued actions can be delivered
  document.addEventListener('DOMContentLoaded', function () {{
    window.__toolbox.ready().catch(function () {{}});
  }});
}})();"#
    )
}

// ------------------------------------------------------------ bridge call

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeCall {
    pub tool_id: String,
    pub method: String,
    #[serde(default)]
    pub args: Option<Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenedFile {
    pub name: String,
    pub path: String,
    pub size: u64,
    /// base64 without the data: prefix, so the page can build a Blob itself
    pub data: String,
    pub mime: String,
}

/// Resolve the calling tool from the window label, never from the payload.
pub fn caller_tool(window: &tauri::WebviewWindow) -> Result<registry::Manifest, String> {
    let label = window.label();
    let tool_id = crate::windows::tools::tool_id_from_label(label)
        .ok_or_else(|| rust_i18n::t!("native.bridge.host_only_window").to_string())?;
    registry::find(&tool_id)
        .ok_or_else(|| rust_i18n::t!("native.tool.not_found", id = tool_id.as_str()).to_string())
}

pub fn dispatch_bridge(window: &tauri::WebviewWindow, call: BridgeCall) -> Result<Value, String> {
    let manifest = caller_tool(window)?;
    if call.tool_id != manifest.id {
        return Err(rust_i18n::t!("native.bridge.identity").into());
    }
    // Installed tools only get the permissions the user ticked in the review
    // dialog (clamped to what tool.json declares); everything else uses the
    // manifest as-is. See registry::effective_permissions.
    let perms = crate::registry::effective_permissions(&manifest);
    let app: AppHandle = window.app_handle().clone();

    match call.method.as_str() {
        "__ready" => {
            crate::windows::tools::on_ready(&app, &manifest.id);
            Ok(Value::Bool(true))
        }
        "files.open" => {
            require(&perms, &manifest.id, |p| p.files_open, "files.open")?;
            let opts = call.args.unwrap_or(Value::Null);
            let multiple = opts
                .get("multiple")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let accept = opts
                .get("accept")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str())
                        .map(|s| s.trim_start_matches('.').to_string())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let picked = pick_files(&app, multiple, accept)?;
            let mut out = Vec::new();
            for path in picked {
                out.push(read_opened_file(&path)?);
            }
            Ok(serde_json::to_value(out).unwrap_or(Value::Array(vec![])))
        }
        "files.save" => {
            require(&perms, &manifest.id, |p| p.files_save, "files.save")?;
            let args = call.args.unwrap_or(Value::Null);
            let name = args
                .get("suggestedName")
                .and_then(|v| v.as_str())
                .unwrap_or("output.bin")
                .to_string();
            let data = args.get("data").and_then(|v| v.as_str()).unwrap_or("");
            let is_b64 = args
                .get("base64")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let bytes = if is_b64 {
                use base64::Engine;
                base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .map_err(|e| {
                        rust_i18n::t!("native.bridge.decode_failed", error = e.to_string())
                            .to_string()
                    })?
            } else {
                data.as_bytes().to_vec()
            };
            let path = save_file(&app, &name, &bytes)?;
            Ok(json!({ "ok": true, "path": path }))
        }
        "clipboard.writeText" => {
            require(&perms, &manifest.id, |p| p.clipboard_write, "clipboard.write")?;
            let text = call
                .args
                .as_ref()
                .and_then(|v| v.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            app.clipboard()
                .write_text(text.to_string())
                .map_err(|e| e.to_string())?;
            Ok(Value::Bool(true))
        }
        "clipboard.readText" => {
            require(&perms, &manifest.id, |p| p.clipboard_read, "clipboard.read")?;
            let t = app.clipboard().read_text().map_err(|e| e.to_string())?;
            Ok(json!(t))
        }
        "data.get" => {
            require(&perms, &manifest.id, |p| p.data, "data")?;
            let key = call
                .args
                .as_ref()
                .and_then(|v| v.get("key"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let store = load_tool_store(&manifest.id);
            Ok(store.get(key).cloned().unwrap_or(Value::Null))
        }
        "data.set" => {
            require(&perms, &manifest.id, |p| p.data, "data")?;
            let args = call.args.unwrap_or(Value::Null);
            let key = args
                .get("key")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let value = args.get("value").cloned().unwrap_or(Value::Null);
            let mut store = load_tool_store(&manifest.id);
            store.insert(key, value);
            save_tool_store(&manifest.id, &store)?;
            Ok(Value::Bool(true))
        }
        "data.remove" => {
            require(&perms, &manifest.id, |p| p.data, "data")?;
            let key = call
                .args
                .as_ref()
                .and_then(|v| v.get("key"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let mut store = load_tool_store(&manifest.id);
            store.remove(&key);
            save_tool_store(&manifest.id, &store)?;
            Ok(Value::Bool(true))
        }
        "window.setTitle" => {
            let title = call
                .args
                .as_ref()
                .and_then(|v| v.get("title"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let _ = window.set_title(title);
            Ok(Value::Bool(true))
        }
        "window.close" => {
            let _ = window.close();
            Ok(Value::Bool(true))
        }
        "log" => {
            let args = call.args.unwrap_or(Value::Null);
            let level = args
                .get("level")
                .and_then(|v| v.as_str())
                .unwrap_or("info");
            let msg = args
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            crate::logging::tool_log(&manifest.id, level, msg);
            Ok(Value::Bool(true))
        }
        "actions.register" => {
            let args = call.args.unwrap_or(Value::Null);
            let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let title = args.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let combo = args
                .get("defaultHotkey")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let full = crate::actions::register_runtime(&manifest.id, id, title, combo)?;
            crate::logging::write(
                "info",
                "actions",
                &format!("runtime action registered: {full}"),
            );
            // The bridge runs on an async worker; rebuild hotkeys on the main thread.
            crate::windows::panel::defer_main(&app, |h| crate::actions::apply_hotkeys(&h));
            Ok(json!({ "id": id, "action": full }))
        }
        "actions.unregister" => {
            let id = call
                .args
                .as_ref()
                .and_then(|v| v.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let removed = crate::actions::unregister_runtime(&manifest.id, &id);
            crate::windows::panel::defer_main(&app, |h| crate::actions::apply_hotkeys(&h));
            Ok(json!({ "ok": removed }))
        }
        other => Err(rust_i18n::t!("native.bridge.unknown_method", method = other).to_string()),
    }
}

fn require(
    perms: &registry::Permissions,
    tool_id: &str,
    check: impl Fn(&registry::Permissions) -> bool,
    name: &str,
) -> Result<(), String> {
    if check(perms) {
        Ok(())
    } else {
        Err(rust_i18n::t!(
            "native.bridge.missing_permission",
            id = tool_id,
            name = name
        )
        .to_string())
    }
}

fn tool_store_path(tool_id: &str) -> PathBuf {
    paths::get().tool_store.join(tool_id).join("store.json")
}

fn load_tool_store(tool_id: &str) -> std::collections::BTreeMap<String, Value> {
    let p = tool_store_path(tool_id);
    std::fs::read_to_string(p)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_tool_store(
    tool_id: &str,
    store: &std::collections::BTreeMap<String, Value>,
) -> Result<(), String> {
    let p = tool_store_path(tool_id);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    crate::state::write_json(&p, store)
}

fn pick_files(app: &AppHandle, multiple: bool, accept: Vec<String>) -> Result<Vec<PathBuf>, String> {
    use tauri_plugin_dialog::DialogExt;
    let mut builder = app.dialog().file();
    for ext in &accept {
        if !ext.is_empty() {
            builder = builder.add_filter(ext.to_uppercase(), &[ext.as_str()]);
        }
    }
    if multiple {
        let picked = builder.blocking_pick_files().unwrap_or_default();
        Ok(picked
            .into_iter()
            .filter_map(|f| f.into_path().ok())
            .collect())
    } else {
        Ok(builder
            .blocking_pick_file()
            .and_then(|f| f.into_path().ok())
            .into_iter()
            .collect())
    }
}

fn save_file(app: &AppHandle, name: &str, bytes: &[u8]) -> Result<String, String> {
    use tauri_plugin_dialog::DialogExt;
    let path = app
        .dialog()
        .file()
        .set_file_name(name)
        .blocking_save_file()
        .and_then(|f| f.into_path().ok())
        .ok_or_else(|| rust_i18n::t!("native.bridge.save_cancelled").to_string())?;
    std::fs::write(&path, bytes).map_err(|e| {
        rust_i18n::t!("native.bridge.write_failed", error = e.to_string()).to_string()
    })?;
    Ok(path.to_string_lossy().to_string())
}

fn read_opened_file(path: &Path) -> Result<OpenedFile, String> {
    use base64::Engine;
    let bytes = std::fs::read(path).map_err(|e| {
        rust_i18n::t!("native.bridge.read_failed", error = e.to_string()).to_string()
    })?;
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    Ok(OpenedFile {
        name,
        path: path.to_string_lossy().to_string(),
        size: bytes.len() as u64,
        data: base64::engine::general_purpose::STANDARD.encode(&bytes),
        mime: mime_for(path).to_string(),
    })
}
