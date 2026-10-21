//! Host commands exposed to the toolbox's own windows (panel / main).
//!
//! Every command here is refused when the caller is a tool window. Tool
//! windows may only use `bridge_call`, which is capability checked.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Manager, WebviewWindow};

use crate::actions;
use crate::install;
use crate::paths;
use crate::registry;
use crate::state::{self, Config};
use crate::windows;

fn require_host(window: &WebviewWindow) -> Result<(), String> {
    if windows::tools::tool_id_from_label(window.label()).is_some() {
        return Err("工具窗口无权调用宿主接口".into());
    }
    Ok(())
}

// ------------------------------------------------------------------ state

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDto {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub author: String,
    pub source: String,
    pub enabled: bool,
    pub accent: String,
    pub icon_data_url: Option<String>,
    pub dir: String,
    pub size_bytes: u64,
    pub error: String,
    pub actions: Vec<ActionDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionDto {
    pub id: String,
    pub title: String,
    pub combo: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppStateDto {
    pub version: String,
    pub portable: bool,
    pub data_dir: String,
    pub official_tools_dir: String,
    pub installed_tools_dir: String,
    pub config: Config,
    pub tools: Vec<ToolDto>,
    pub actions: Vec<actions::HotkeyRow>,
}

fn build_state() -> AppStateDto {
    let p = paths::get();
    let cfg = state::load_config();
    let overrides = state::load_hotkeys();
    let states = state::load_tools();
    let manifests = registry::scan();
    let rows = actions::hotkey_table(&crate::app_handle());

    let tools = manifests
        .into_iter()
        .map(|m| {
            let dir = std::path::PathBuf::from(&m.dir);
            let tool_actions = m
                .actions
                .iter()
                .map(|a| ActionDto {
                    id: a.id.clone(),
                    title: a.title.clone(),
                    combo: overrides
                        .get(&format!("tool.{}.{}", m.id, a.id))
                        .cloned()
                        .unwrap_or_else(|| a.default_hotkey.clone()),
                })
                .collect();
            ToolDto {
                enabled: registry::is_enabled(&states, &m.id),
                size_bytes: registry::dir_size(&dir),
                icon_data_url: registry::icon_data_url(&dir, &m.icon),
                actions: tool_actions,
                error: m.error.clone(),
                id: m.id,
                name: m.name,
                description: m.description,
                version: m.version,
                author: m.author,
                source: m.source,
                accent: m.accent,
                dir: m.dir,
            }
        })
        .collect();

    AppStateDto {
        version: env!("CARGO_PKG_VERSION").to_string(),
        portable: p.portable,
        data_dir: p.data.to_string_lossy().to_string(),
        official_tools_dir: p.official_tools.to_string_lossy().to_string(),
        installed_tools_dir: p.installed_tools.to_string_lossy().to_string(),
        config: cfg,
        tools,
        actions: rows,
    }
}

#[tauri::command]
pub fn get_state(window: WebviewWindow) -> Result<AppStateDto, String> {
    require_host(&window)?;
    Ok(build_state())
}

// ------------------------------------------------------------------ tools

#[tauri::command]
pub fn open_tool(window: WebviewWindow, id: String) -> Result<(), String> {
    require_host(&window)?;
    let app = window.app_handle().clone();
    windows::tools::open(&app, &id)
}

#[tauri::command]
pub fn close_tool(window: WebviewWindow, id: String) -> Result<(), String> {
    require_host(&window)?;
    let app = window.app_handle().clone();
    windows::tools::close(&app, &id);
    Ok(())
}

#[tauri::command]
pub fn set_tool_enabled(window: WebviewWindow, id: String, enabled: bool) -> Result<(), String> {
    require_host(&window)?;
    let mut states = state::load_tools();
    let entry = states.entry(id.clone()).or_default();
    entry.enabled = enabled;
    if entry.source.is_empty() {
        entry.source = registry::find(&id).map(|m| m.source).unwrap_or_default();
    }
    state::save_tools(&states)?;
    let app = window.app_handle().clone();
    actions::apply_hotkeys(&app);
    let _ = app.emit_all_hosts("tools-changed", json!({ "id": id }));
    Ok(())
}

#[tauri::command]
pub fn uninstall_tool(window: WebviewWindow, id: String) -> Result<(), String> {
    require_host(&window)?;
    let app = window.app_handle().clone();
    windows::tools::close(&app, &id);
    install::uninstall(&id, false)?;
    let _ = app.emit_all_hosts("tools-changed", json!({ "id": id }));
    Ok(())
}

#[tauri::command]
pub fn install_tool(window: WebviewWindow) -> Result<Value, String> {
    require_host(&window)?;
    use tauri_plugin_dialog::DialogExt;
    let app = window.app_handle().clone();
    let picked = app
        .dialog()
        .file()
        .add_filter("Toolbox 工具包", &["zip"])
        .blocking_pick_file()
        .and_then(|f| f.into_path().ok());
    let Some(zip) = picked else {
        return Ok(json!({ "ok": false, "error": "cancelled" }));
    };
    match install::install_from_zip(&zip) {
        Ok(r) => {
            let _ = app.emit_all_hosts("tools-changed", json!({ "id": r.id }));
            Ok(serde_json::to_value(r).unwrap_or(json!({ "ok": true })))
        }
        Err(e) => Ok(json!({ "ok": false, "error": e })),
    }
}

// ---------------------------------------------------------------- hotkeys

#[tauri::command]
pub fn set_hotkey(window: WebviewWindow, action_id: String, combo: String) -> Result<Value, String> {
    require_host(&window)?;
    let app = window.app_handle().clone();
    let combo = actions::normalize_combo(&combo);
    if !combo.is_empty() && actions::parse_combo(&combo).is_err() {
        return Ok(json!({ "ok": false, "reason": "快捷键格式无法识别" }));
    }
    // conflict against other actions
    let rows = actions::hotkey_table(&app);
    if let Some(other) = rows
        .iter()
        .find(|r| r.id != action_id && !combo.is_empty() && r.combo.eq_ignore_ascii_case(&combo))
    {
        return Ok(json!({ "ok": false, "reason": format!("与「{}」冲突", other.title) }));
    }

    let mut overrides = state::load_hotkeys();
    overrides.insert(action_id.clone(), combo.clone());
    state::save_hotkeys(&overrides)?;

    // panel hotkey also lives in config.json so the config page shows it
    if action_id == "app.open-panel" {
        let mut cfg = state::load_config();
        cfg.panel_hotkey = combo.clone();
        let _ = state::save_config(&cfg);
    }

    actions::apply_hotkeys(&app);
    let _ = app.emit_all_hosts("config-changed", json!({}));
    Ok(json!({ "ok": true }))
}

#[tauri::command]
pub fn reset_hotkey(window: WebviewWindow, action_id: String) -> Result<Value, String> {
    require_host(&window)?;
    let app = window.app_handle().clone();
    let mut overrides = state::load_hotkeys();
    overrides.remove(&action_id);
    state::save_hotkeys(&overrides)?;
    if action_id == "app.open-panel" {
        let mut cfg = state::load_config();
        cfg.panel_hotkey = "Ctrl+Shift+T".into();
        let _ = state::save_config(&cfg);
    }
    actions::apply_hotkeys(&app);
    let _ = app.emit_all_hosts("config-changed", json!({}));
    Ok(json!({ "ok": true }))
}

// ----------------------------------------------------------------- config

#[tauri::command]
pub fn save_config(window: WebviewWindow, patch: Value) -> Result<Value, String> {
    require_host(&window)?;
    let app = window.app_handle().clone();
    let mut cfg = state::load_config();
    if let Some(obj) = patch.as_object() {
        if let Some(v) = obj.get("locale").and_then(|v| v.as_str()) {
            cfg.locale = v.to_string();
        }
        if let Some(v) = obj.get("theme").and_then(|v| v.as_str()) {
            cfg.theme = v.to_string();
        }
        if let Some(v) = obj.get("ballSide").and_then(|v| v.as_str()) {
            cfg.ball_side = v.to_string();
        }
        if let Some(v) = obj.get("ballY").and_then(|v| v.as_f64()) {
            cfg.ball_y = v.clamp(0.0, 1.0);
        }
        if let Some(v) = obj.get("ballMode").and_then(|v| v.as_str()) {
            cfg.ball_mode = v.to_string();
        }
        if let Some(v) = obj.get("downloadMode").and_then(|v| v.as_str()) {
            cfg.download_mode = v.to_string();
        }
        if let Some(v) = obj.get("autostart").and_then(|v| v.as_bool()) {
            cfg.autostart = v;
        }
        if let Some(v) = obj.get("closeToTray").and_then(|v| v.as_bool()) {
            cfg.close_to_tray = v;
        }
    }
    state::save_config(&cfg)?;
    windows::ball::reposition(&app);
    let _ = app.emit_all_hosts("config-changed", json!({}));
    Ok(json!({ "ok": true }))
}

// ------------------------------------------------------------------ misc

#[tauri::command]
pub fn open_panel(window: WebviewWindow) -> Result<(), String> {
    require_host(&window)?;
    windows::panel::toggle(&window.app_handle().clone());
    Ok(())
}

#[tauri::command]
pub fn open_main(window: WebviewWindow) -> Result<(), String> {
    require_host(&window)?;
    windows::main::open(&window.app_handle().clone());
    Ok(())
}

#[tauri::command]
pub fn close_window(window: WebviewWindow, label: String) -> Result<(), String> {
    require_host(&window)?;
    let app = window.app_handle().clone();
    if let Some(w) = app.get_webview_window(&label) {
        // The panel is cheap to keep warm; the main window follows the
        // "close to tray" preference; anything else is destroyed outright.
        if label == windows::panel::LABEL {
            let _ = w.hide();
        } else if label == windows::main::LABEL && state::load_config().close_to_tray {
            let _ = w.hide();
        } else {
            let _ = w.destroy();
        }
    }
    Ok(())
}

#[tauri::command]
pub fn quit_app(window: WebviewWindow) -> Result<(), String> {
    require_host(&window)?;
    window.app_handle().exit(0);
    Ok(())
}

#[tauri::command]
pub fn restart_app(window: WebviewWindow) -> Result<(), String> {
    require_host(&window)?;
    let app = window.app_handle().clone();
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    std::process::Command::new(exe)
        .spawn()
        .map_err(|e| e.to_string())?;
    app.exit(0);
    Ok(())
}

#[tauri::command]
pub fn reveal_path(window: WebviewWindow, kind: String) -> Result<Value, String> {
    require_host(&window)?;
    let p = paths::get();
    let target = match kind.as_str() {
        "data" => p.data.clone(),
        "officialTools" => p.official_tools.clone(),
        "installedTools" => p.installed_tools.clone(),
        "logs" => p.logs.clone(),
        _ => p.data.clone(),
    };
    let _ = std::fs::create_dir_all(&target);
    let shown = target.to_string_lossy().to_string();
    tauri_plugin_opener::open_path(shown.clone(), None::<&str>).map_err(|e| e.to_string())?;
    Ok(json!({ "ok": true, "path": shown }))
}

#[tauri::command]
pub fn minimize_window(window: WebviewWindow, label: Option<String>) -> Result<Value, String> {
    require_host(&window)?;
    let app = window.app_handle().clone();
    let target = label.unwrap_or_else(|| window.label().to_string());
    if let Some(w) = app.get_webview_window(&target) {
        w.minimize().map_err(|e| e.to_string())?;
    }
    Ok(json!({ "ok": true }))
}

#[tauri::command]
pub fn get_log_tail(window: WebviewWindow, lines: usize) -> Result<Value, String> {
    require_host(&window)?;
    Ok(json!({ "lines": crate::logging::tail(lines.clamp(1, 500)) }))
}

// ------------------------------------------------------------- bridge call

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeArgs {
    pub tool_id: String,
    pub method: String,
    #[serde(default)]
    pub args: Option<Value>,
}

#[tauri::command]
pub async fn bridge_call(window: WebviewWindow, tool_id: String, method: String, args: Option<Value>) -> Result<Value, String> {
    let call = crate::bridge::BridgeCall { tool_id, method, args };
    crate::bridge::dispatch_bridge(&window, call)
}

// -------------------------------------------------------------- app helper

pub trait EmitHosts {
    fn emit_all_hosts(&self, event: &str, payload: Value) -> Result<(), String>;
}

impl EmitHosts for AppHandle {
    fn emit_all_hosts(&self, event: &str, payload: Value) -> Result<(), String> {
        let payload = payload.to_string();
        for (label, win) in self.webview_windows() {
            if windows::tools::tool_id_from_label(&label).is_some() {
                continue;
            }
            let js = format!(
                "window.__hostEmit && window.__hostEmit({}, {})",
                serde_json::to_string(event).unwrap_or_else(|_| "\"\"".into()),
                payload
            );
            let _ = win.eval(&js);
        }
        Ok(())
    }
}

/// Exposed for the settings page.
pub fn config_snapshot() -> BTreeMap<String, Value> {
    BTreeMap::new()
}
