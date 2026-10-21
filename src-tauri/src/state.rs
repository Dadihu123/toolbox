//! Persisted state: config.json / hotkeys.json / tools.json.
//!
//! Writes are atomic (tmp + rename) so a crash never leaves a half file.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::paths;

pub const CONFIG_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub version: u32,
    pub locale: String,
    pub theme: String,
    /// "right" | "left"
    pub ball_side: String,
    /// 0.0..1.0 fraction of the work area height.
    pub ball_y: f64,
    /// "peek" (only a sliver visible) | "always"
    pub ball_mode: String,
    /// "dialog" | "folder"
    pub download_mode: String,
    pub autostart: bool,
    pub close_to_tray: bool,
    pub panel_hotkey: String,
    pub tool_hotkeys_enabled: bool,
    pub last_tool: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            locale: "zh-CN".into(),
            theme: "dark".into(),
            ball_side: "right".into(),
            ball_y: 0.62,
            ball_mode: "peek".into(),
            download_mode: "dialog".into(),
            autostart: false,
            close_to_tray: true,
            panel_hotkey: "Ctrl+Shift+T".into(),
            tool_hotkeys_enabled: true,
            last_tool: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ToolState {
    pub enabled: bool,
    #[serde(default)]
    pub installed_at: String,
    /// "official" | "installed" | "dev"
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub pinned: bool,
}

fn read_json<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> T {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => T::default(),
    }
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let parent = path.parent().ok_or("no parent")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    let tmp: PathBuf = parent.join(format!(
        ".{}.tmp",
        path.file_name().and_then(|s| s.to_str()).unwrap_or("state")
    ));
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn config_path() -> PathBuf {
    paths::get().data.join("config.json")
}
pub fn hotkeys_path() -> PathBuf {
    paths::get().data.join("hotkeys.json")
}
pub fn tools_path() -> PathBuf {
    paths::get().data.join("tools.json")
}

pub fn load_config() -> Config {
    let mut cfg: Config = read_json(&config_path());
    if cfg.version < CONFIG_VERSION {
        cfg.version = CONFIG_VERSION;
    }
    if cfg.panel_hotkey.trim().is_empty() {
        cfg.panel_hotkey = "Ctrl+Shift+T".into();
    }
    cfg
}

pub fn save_config(cfg: &Config) -> Result<(), String> {
    write_json(&config_path(), cfg)
}

pub fn load_hotkeys() -> BTreeMap<String, String> {
    read_json(&hotkeys_path())
}

pub fn save_hotkeys(map: &BTreeMap<String, String>) -> Result<(), String> {
    write_json(&hotkeys_path(), map)
}

pub fn load_tools() -> BTreeMap<String, ToolState> {
    read_json(&tools_path())
}

pub fn save_tools(map: &BTreeMap<String, ToolState>) -> Result<(), String> {
    write_json(&tools_path(), map)
}
