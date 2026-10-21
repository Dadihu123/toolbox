//! Tool windows: one WebView per tool, isolated origin via `toolbox://`.

use std::collections::BTreeSet;
use std::sync::Mutex;

use serde_json::json;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::paths;
use crate::registry;

pub const PREFIX: &str = "tool-";

/// Pending actions waiting for a tool window to report ready.
static PENDING: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

/// Pending file handoffs waiting for a tool window to report ready.
static PENDING_FILES: Mutex<Vec<(String, Vec<String>)>> = Mutex::new(Vec::new());

/// Labels of tool windows whose page called `bridge.ready()`. Events pushed before
/// that moment would be dropped by the page (no listeners yet), so they are queued.
static READY: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

fn is_ready(label: &str) -> bool {
    READY.lock().map(|s| s.contains(label)).unwrap_or(false)
}

fn mark_ready(label: &str) {
    if let Ok(mut s) = READY.lock() {
        s.insert(label.to_string());
    }
}

fn mark_not_ready(label: &str) {
    if let Ok(mut s) = READY.lock() {
        s.remove(label);
    }
}

pub fn label_for(tool_id: &str) -> String {
    format!("{PREFIX}{tool_id}")
}

pub fn tool_id_from_label(label: &str) -> Option<String> {
    label.strip_prefix(PREFIX).map(|s| s.to_string())
}

pub fn open_tool_ids(app: &AppHandle) -> Vec<String> {
    app.webview_windows()
        .keys()
        .filter_map(|l| tool_id_from_label(l))
        .collect()
}

/// Open (or focus) a tool window. Safe to call from any thread and from inside
/// event-loop callbacks: creation is hopped onto the main thread when needed.
pub fn open(app: &AppHandle, tool_id: &str) -> Result<(), String> {
    if app.get_webview_window(&label_for(tool_id)).is_some() {
        focus(app, tool_id);
        return Ok(());
    }
    let Some(manifest) = registry::find(tool_id) else {
        return Err(format!("未找到工具「{tool_id}」"));
    };
    if !manifest.error.is_empty() {
        return Err(format!("工具「{tool_id}」无法加载：{}", manifest.error));
    }
    if !registry::is_enabled(&crate::state::load_tools(), tool_id) {
        return Err(format!("工具「{tool_id}」已在设置中停用"));
    }

    let handle = app.clone();
    let id = tool_id.to_string();
    crate::windows::panel::defer_main(app, move |app2| {
        if let Err(e) = build(&app2, &id) {
            crate::logging::write("error", "tools", &format!("open {id} failed: {e}"));
        }
    });
    let _ = handle;
    Ok(())
}

fn focus(app: &AppHandle, tool_id: &str) {
    if let Some(win) = app.get_webview_window(&label_for(tool_id)) {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

fn build(app: &AppHandle, tool_id: &str) -> Result<(), String> {
    if app.get_webview_window(&label_for(tool_id)).is_some() {
        focus(app, tool_id);
        return Ok(());
    }
    let Some(manifest) = registry::find(tool_id) else {
        return Err(format!("未找到工具「{tool_id}」"));
    };
    if !manifest.error.is_empty() {
        return Err(format!("工具「{tool_id}」无法加载：{}", manifest.error));
    }
    if !registry::is_enabled(&crate::state::load_tools(), tool_id) {
        return Err(format!("工具「{tool_id}」已在设置中停用"));
    }

    let label = label_for(tool_id);
    // A freshly (re)created window is not ready until its shim calls bridge.ready().
    mark_not_ready(&label);
    let webview_dir = crate::paths::get().webview_data.clone();
    let _ = std::fs::create_dir_all(&webview_dir);
    let url = WebviewUrl::CustomProtocol(
        format!("toolbox://localhost/{}/{}", manifest.id, manifest.entry)
            .parse()
            .map_err(|e| format!("{e}"))?,
    );

    let win = WebviewWindowBuilder::new(app, &label, url)
        .title(&manifest.name)
        .inner_size(manifest.window.width, manifest.window.height)
        .min_inner_size(manifest.window.min_width, manifest.window.min_height)
        .decorations(false)
        .data_directory(webview_dir)
        .disable_drag_drop_handler() // HTML5 drop must reach the page
        .initialization_script(&crate::bridge::shim_script(
            &manifest,
            &registry::effective_permissions(&manifest),
        ))
        .on_download(|_webview, event| {
            handle_download(event);
            true
        })
        .build()
        .map_err(|e| e.to_string())?;

    let _ = win.set_focus();

    // Runtime actions (`toolbox.actions.register`) only live while this window does.
    let tool_id_ev = tool_id.to_string();
    let label_ev = label.clone();
    win.on_window_event(move |event| {
        if matches!(event, tauri::WindowEvent::Destroyed) {
            crate::actions::clear_runtime(&tool_id_ev);
            mark_not_ready(&label_ev);
            crate::actions::apply_hotkeys(&crate::app_handle());
        }
    });

    let mut cfg = crate::state::load_config();
    cfg.last_tool = tool_id.to_string();
    let _ = crate::state::save_config(&cfg);

    crate::actions::apply_hotkeys(app);
    let _ = paths::get();
    Ok(())
}

fn handle_download(event: tauri::webview::DownloadEvent<'_>) {
    use tauri::webview::DownloadEvent;
    match event {
        DownloadEvent::Requested { url, destination } => {
            let name = url
                .path_segments()
                .and_then(|mut s| s.next_back().map(|x| x.to_string()))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "download.bin".into());
            let dir = paths::get().downloads.clone();
            let _ = std::fs::create_dir_all(&dir);
            *destination = dir.join(sanitize(&name));
        }
        DownloadEvent::Finished { path, success, .. } => {
            if let Some(p) = path {
                crate::logging::write(
                    "info",
                    "download",
                    &format!("{} -> {}", if success { "ok" } else { "failed" }, p.display()),
                );
            }
        }
        _ => {}
    }
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if "\\/:*?\"<>|".contains(c) { '_' } else { c })
        .collect()
}

pub fn close(app: &AppHandle, tool_id: &str) {
    let label = label_for(tool_id);
    mark_not_ready(&label);
    crate::actions::clear_runtime(tool_id);
    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.destroy();
    }
    crate::actions::apply_hotkeys(app);
}

/// Deliver a host -> tool event by driving the injected shim.
///
/// Tool windows have zero Tauri permissions, so `webview.emit` never reaches them
/// (the event plugin needs a capability those windows do not have). The injected
/// shim installs `window.__toolboxEmit`, and we call it with `eval` — the same
/// approach the host windows use for `__hostEmit` (see panel::HOST_BOOTSTRAP).
fn deliver(win: &tauri::WebviewWindow, event: &str, payload: &serde_json::Value) {
    let js = format!(
        "window.__toolboxEmit && window.__toolboxEmit({}, {})",
        serde_json::to_string(event).unwrap_or_else(|_| "\"\"".into()),
        serde_json::to_string(payload).unwrap_or_else(|_| "null".into())
    );
    let _ = win.eval(&js);
}

fn file_payload(files: &[String]) -> serde_json::Value {
    let items: Vec<serde_json::Value> = files
        .iter()
        .map(|p| {
            let path = std::path::Path::new(p);
            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            json!({ "name": name, "path": p, "size": size, "exists": path.is_file() })
        })
        .collect();
    json!({ "files": items })
}

/// Build the `action` event payload for a dispatched action. Handles both manifest
/// actions and runtime actions (`runtime.<localId>`) contributed by the tool itself.
fn action_payload(tool_id: &str, action_id: &str) -> Option<serde_json::Value> {
    let manifest = registry::find(tool_id)?;
    if manifest.actions.iter().any(|a| a.id == action_id) {
        return Some(json!({
            "id": action_id,
            "action": format!("tool.{tool_id}.{action_id}"),
            "toolId": tool_id,
        }));
    }
    let local = action_id.strip_prefix("runtime.")?;
    if !crate::actions::runtime_has(tool_id, local) {
        return None;
    }
    Some(json!({
        "id": local,
        "action": format!("tool.{tool_id}.runtime.{local}"),
        "toolId": tool_id,
    }))
}

/// Run a tool action (manifest-declared or runtime-registered), opening the
/// window first if needed.
pub fn run_tool_action(app: &AppHandle, tool_id: &str, action_id: &str) {
    let Some(payload) = action_payload(tool_id, action_id) else {
        return;
    };
    let label = label_for(tool_id);

    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
        if is_ready(&label) {
            deliver(&win, "action", &payload);
            crate::logging::write(
                "info",
                "actions",
                &format!("dispatched {action_id} -> {tool_id}"),
            );
        } else if let Ok(mut q) = PENDING.lock() {
            // Window exists but its page is still booting; deliver on `ready`.
            q.push((label.clone(), action_id.to_string()));
        }
        return;
    }

    if let Ok(mut q) = PENDING.lock() {
        q.push((label.clone(), action_id.to_string()));
    }
    let _ = open(app, tool_id);

    // drop the pending entry if the tool never reports ready
    let label2 = label;
    let action2 = action_id.to_string();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(3));
        if let Ok(mut q) = PENDING.lock() {
            q.retain(|(l, a)| !(l == &label2 && a == &action2));
        }
    });
}

/// Called from the bridge when a tool page finished booting.
pub fn on_ready(app: &AppHandle, tool_id: &str) {
    let label = label_for(tool_id);
    mark_ready(&label);
    let (pending, pending_files) = {
        let mut actions: Vec<String> = Vec::new();
        if let Ok(mut q) = PENDING.lock() {
            actions = q
                .iter()
                .filter(|(l, _)| l == &label)
                .map(|(_, a)| a.clone())
                .collect();
            q.retain(|(l, _)| l != &label);
        }
        let mut files: Vec<String> = Vec::new();
        if let Ok(mut q) = PENDING_FILES.lock() {
            for (l, list) in q.iter() {
                if l == &label {
                    files.extend(list.iter().cloned());
                }
            }
            q.retain(|(l, _)| l != &label);
        }
        (actions, files)
    };
    if pending.is_empty() && pending_files.is_empty() {
        return;
    }
    if let Some(win) = app.get_webview_window(&label) {
        for action in pending {
            if let Some(payload) = action_payload(tool_id, &action) {
                deliver(&win, "action", &payload);
                crate::logging::write(
                    "info",
                    "actions",
                    &format!("queued {action} -> {tool_id} delivered on ready"),
                );
            }
        }
        if !pending_files.is_empty() {
            deliver(&win, "files-dropped", &file_payload(&pending_files));
            crate::logging::write(
                "info",
                "files",
                &format!(
                    "{} file(s) delivered -> {tool_id} on ready",
                    pending_files.len()
                ),
            );
        }
    }
}

/// Pick an enabled tool that declares it accepts the given file type. Used by the
/// CLI / shell file association, where no `--tool` was given.
pub fn tool_for_files(files: &[String]) -> Option<String> {
    let first = files.first()?;
    let ext = std::path::Path::new(first)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "webp" => "image/webp",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        _ => return None,
    };
    let states = crate::state::load_tools();
    registry::scan()
        .into_iter()
        .filter(|m| m.error.is_empty() && registry::is_enabled(&states, &m.id))
        .find(|m| m.accepts.iter().any(|a| a.eq_ignore_ascii_case(mime)))
        .map(|m| m.id)
}

/// Hand files to a tool window (CLI `--open` / drag & drop), opening it first.
pub fn open_files(app: &AppHandle, tool_id: &str, files: Vec<String>) {    let label = label_for(tool_id);
    let existing = app.get_webview_window(&label);
    let _ = open(app, tool_id);

    if let Some(win) = existing {
        let _ = win.show();
        let _ = win.set_focus();
        if is_ready(&label) {
            deliver(&win, "files-dropped", &file_payload(&files));
            crate::logging::write(
                "info",
                "files",
                &format!("{} file(s) delivered -> {tool_id}", files.len()),
            );
            return;
        }
    }

    // Window missing or still booting: hold the files until the page reports ready.
    if let Ok(mut q) = PENDING_FILES.lock() {
        q.push((label.clone(), files));
    }
    // drop the pending entry if the tool never reports ready
    let label2 = label;
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(5));
        if let Ok(mut q) = PENDING_FILES.lock() {
            q.retain(|(l, _)| l != &label2);
        }
    });
}
