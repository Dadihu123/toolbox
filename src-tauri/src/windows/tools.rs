//! Tool windows: one WebView per tool, isolated origin via `toolbox://`.

use std::collections::BTreeMap;
use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::paths;
use crate::registry;

pub const PREFIX: &str = "tool-";

/// Pending actions waiting for a tool window to report ready.
static PENDING: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

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

    let label = label_for(tool_id);
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
        .initialization_script(&crate::bridge::shim_script(&manifest))
        .on_download(|_webview, event| {
            handle_download(event);
            true
        })
        .build()
        .map_err(|e| e.to_string())?;

    let _ = win.set_focus();

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
    if let Some(win) = app.get_webview_window(&label_for(tool_id)) {
        let _ = win.destroy();
    }
    crate::actions::apply_hotkeys(app);
}

/// Run a manifest action on a tool, opening the window first if needed.
pub fn run_tool_action(app: &AppHandle, tool_id: &str, action_id: &str) {
    let Some(manifest) = registry::find(tool_id) else {
        return;
    };
    if !manifest.actions.iter().any(|a| a.id == action_id) {
        return;
    }
    let label = label_for(tool_id);

    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
        let _ = win.emit("toolbox:action", action_id.to_string());
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
    let pending: Vec<String> = {
        let mut q = match PENDING.lock() {
            Ok(q) => q,
            Err(_) => return,
        };
        let taken: Vec<String> = q
            .iter()
            .filter(|(l, _)| l == &label)
            .map(|(_, a)| a.clone())
            .collect();
        q.retain(|(l, _)| l != &label);
        taken
    };
    if pending.is_empty() {
        return;
    }
    if let Some(win) = app.get_webview_window(&label) {
        for action in pending {
            let _ = win.emit("toolbox:action", action);
        }
    }
}

pub fn open_files(app: &AppHandle, tool_id: &str, files: Vec<String>) {
    let _ = open(app, tool_id);
    let label = label_for(tool_id);
    let payload: BTreeMap<String, Vec<String>> = BTreeMap::from([("files".into(), files)]);
    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.emit("toolbox:files", payload);
    }
}
