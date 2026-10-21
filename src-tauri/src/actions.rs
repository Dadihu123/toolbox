//! Action registry + global hotkey service.
//!
//! An *action* is a user visible function that can be bound to a hotkey.
//! Actions come from two places: the host itself, and each tool manifest
//! (`actions[]`). Tool actions marked `requiresOpen` only get their global
//! hotkey while that tool's window exists.

use serde::Serialize;
use std::collections::BTreeMap;
use tauri::AppHandle;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

use crate::registry;
use crate::state::{self, Config};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionDef {
    pub id: String,
    pub scope: String,
    pub tool_id: Option<String>,
    pub title: String,
    pub default_combo: String,
    pub requires_open: bool,
    pub accepts: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyRow {
    pub id: String,
    pub scope: String,
    pub tool_id: Option<String>,
    pub title: String,
    pub combo: String,
    pub default_combo: String,
    pub effective: bool,
    pub reason: Option<String>,
}

/// host built-in actions
pub fn builtin_actions(cfg: &Config) -> Vec<ActionDef> {
    vec![
        ActionDef {
            id: "app.open-panel".into(),
            scope: "app".into(),
            tool_id: None,
            title: "唤出工具面板".into(),
            default_combo: cfg.panel_hotkey.clone(),
            requires_open: false,
            accepts: vec![],
        },
        ActionDef {
            id: "app.open-main".into(),
            scope: "app".into(),
            tool_id: None,
            title: "打开主界面".into(),
            default_combo: "Ctrl+Shift+M".into(),
            requires_open: false,
            accepts: vec![],
        },
        ActionDef {
            id: "app.toggle-ball".into(),
            scope: "app".into(),
            tool_id: None,
            title: "显示 / 隐藏悬浮球".into(),
            default_combo: String::new(),
            requires_open: false,
            accepts: vec![],
        },
    ]
}

/// All actions declared by every discovered tool manifest.
pub fn tool_actions(tools: &[registry::Manifest]) -> Vec<ActionDef> {
    let mut out = Vec::new();
    for t in tools {
        if !t.error.is_empty() {
            continue;
        }
        for a in &t.actions {
            out.push(ActionDef {
                id: format!("tool.{}.{}", t.id, a.id),
                scope: "tool".into(),
                tool_id: Some(t.id.clone()),
                title: format!("{}：{}", t.name, a.title),
                default_combo: a.default_hotkey.clone(),
                requires_open: a.requires_open,
                accepts: a.accepts.clone(),
            });
        }
    }
    out
}

pub fn all_actions(cfg: &Config, tools: &[registry::Manifest]) -> Vec<ActionDef> {
    let mut v = builtin_actions(cfg);
    v.extend(tool_actions(tools));
    v
}

/// Effective combo: user override wins over the manifest default.
pub fn effective_combo(action: &ActionDef, overrides: &BTreeMap<String, String>) -> String {
    match overrides.get(&action.id) {
        Some(v) => v.clone(),
        None => action.default_combo.clone(),
    }
}

/// Parse "Ctrl+Shift+T" into a Tauri shortcut.
pub fn parse_combo(combo: &str) -> Result<Shortcut, String> {
    let trimmed = combo.trim();
    if trimmed.is_empty() {
        return Err("未设置".into());
    }
    trimmed
        .to_ascii_lowercase()
        .parse::<Shortcut>()
        .map_err(|e| format!("无法解析快捷键「{trimmed}」：{e}"))
}

/// Normalise to the canonical "Ctrl+Shift+T" form.
pub fn normalize_combo(combo: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut key = String::new();
    for raw in combo.split('+') {
        let p = raw.trim();
        if p.is_empty() {
            continue;
        }
        let lower = p.to_ascii_lowercase();
        let canon = match lower.as_str() {
            "ctrl" | "control" => "Ctrl",
            "shift" => "Shift",
            "alt" => "Alt",
            "win" | "super" | "meta" | "cmd" | "command" => "Win",
            _ => {
                key = canonical_key(p);
                continue;
            }
        };
        parts.push(canon.to_string());
    }
    if key.is_empty() {
        return parts.join("+");
    }
    parts.push(key);
    parts.join("+")
}

fn canonical_key(k: &str) -> String {
    let t = k.trim();
    if t.chars().count() == 1 {
        return t.to_ascii_uppercase();
    }
    match t.to_ascii_lowercase().as_str() {
        "space" => "Space".into(),
        "enter" | "return" => "Enter".into(),
        "esc" | "escape" => "Escape".into(),
        "tab" => "Tab".into(),
        "backspace" => "Backspace".into(),
        "delete" | "del" => "Delete".into(),
        "insert" | "ins" => "Insert".into(),
        "home" => "Home".into(),
        "end" => "End".into(),
        "pageup" => "PageUp".into(),
        "pagedown" => "PageDown".into(),
        "up" | "arrowup" => "Up".into(),
        "down" | "arrowdown" => "Down".into(),
        "left" | "arrowleft" => "Left".into(),
        "right" | "arrowright" => "Right".into(),
        other => {
            if let Some(rest) = other.strip_prefix('f') {
                if rest.parse::<u8>().is_ok() {
                    return format!("F{rest}");
                }
            }
            let mut c = other.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        }
    }
}

/// Register every action that currently has a combo.
pub fn apply_hotkeys(app: &AppHandle) {
    let cfg = state::load_config();
    let overrides = state::load_hotkeys();
    let tools = registry::scan();
    let actions = all_actions(&cfg, &tools);

    let gs = app.global_shortcut();
    let _ = gs.unregister_all();

    let open_tools = crate::windows::tools::open_tool_ids(app);
    let mut registered = 0usize;
    let mut failed: Vec<String> = Vec::new();

    for action in actions {
        let combo = effective_combo(&action, &overrides);
        if combo.trim().is_empty() {
            continue;
        }
        if action.requires_open {
            let Some(tid) = &action.tool_id else { continue };
            if !open_tools.contains(tid) {
                continue;
            }
        }
        let shortcut = match parse_combo(&combo) {
            Ok(s) => s,
            Err(e) => {
                failed.push(format!("{} ({e})", action.id));
                continue;
            }
        };
        let id = action.id.clone();
        match gs.on_shortcut(shortcut, move |app, _sc, event| {
            if event.state() == ShortcutState::Pressed {
                dispatch(app, &id);
            }
        }) {
            Ok(()) => registered += 1,
            Err(e) => failed.push(format!("{} [{combo}] {e}", action.id)),
        }
    }

    crate::logging::write(
        "info",
        "hotkeys",
        &format!(
            "registered {registered} shortcut(s){}",
            if failed.is_empty() {
                String::new()
            } else {
                format!("; failed: {}", failed.join(", "))
            }
        ),
    );
}

fn is_registered(app: &AppHandle, combo: &str) -> bool {
    match parse_combo(combo) {
        Ok(sc) => app.global_shortcut().is_registered(sc),
        Err(_) => false,
    }
}

/// Status table for the hotkey settings page.
pub fn hotkey_table(app: &AppHandle) -> Vec<HotkeyRow> {
    let cfg = state::load_config();
    let overrides = state::load_hotkeys();
    let tools = registry::scan();
    let actions = all_actions(&cfg, &tools);
    let open_tools = crate::windows::tools::open_tool_ids(app);

    let mut rows = Vec::new();
    let mut seen: BTreeMap<String, String> = BTreeMap::new();

    for a in &actions {
        let combo = effective_combo(a, &overrides);
        let mut effective = true;
        let mut reason: Option<String> = None;

        if combo.trim().is_empty() {
            effective = false;
            reason = Some("未设置快捷键".into());
        } else if let Some(owner) = seen.get(&combo.to_ascii_lowercase()) {
            effective = false;
            reason = Some(format!("与「{owner}」冲突"));
        } else if parse_combo(&combo).is_err() {
            effective = false;
            reason = Some("快捷键格式无法识别".into());
        } else if a.requires_open {
            if let Some(tid) = &a.tool_id {
                if !open_tools.contains(tid) {
                    effective = false;
                    reason = Some("工具未打开，打开后生效".into());
                }
            }
        } else if !is_registered(app, &combo) {
            effective = false;
            reason = Some("注册失败（可能被其他程序占用）".into());
        }

        if !combo.trim().is_empty() {
            seen
                .entry(combo.to_ascii_lowercase())
                .or_insert_with(|| a.title.clone());
        }

        rows.push(HotkeyRow {
            id: a.id.clone(),
            scope: a.scope.clone(),
            tool_id: a.tool_id.clone(),
            title: a.title.clone(),
            combo,
            default_combo: a.default_combo.clone(),
            effective,
            reason,
        });
    }
    rows
}

/// Dispatch an action id. Called by hotkeys, tray menu and the panel.
pub fn dispatch(app: &AppHandle, action_id: &str) {
    match action_id {
        "app.open-panel" => crate::windows::panel::toggle(app),
        "app.open-main" => crate::windows::main::open(app),
        "app.toggle-ball" => crate::windows::ball::toggle_visible(app),
        other => {
            if let Some(rest) = other.strip_prefix("tool.") {
                if let Some((tool_id, action)) = rest.split_once('.') {
                    crate::windows::tools::run_tool_action(app, tool_id, action);
                }
            }
        }
    }
}
