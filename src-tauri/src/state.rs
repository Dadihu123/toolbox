//! Persisted state: config.json / hotkeys.json / tools.json.
//!
//! Writes are atomic (tmp + rename) so a crash never leaves a half file.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::paths;
use crate::registry::Permissions;

pub const CONFIG_VERSION: u32 = 1;

/// Hover quick-menu ("快捷图标") configuration.
///
/// The ball fans tool icons out around itself while the pointer rests on it.
/// Which icons appear depends on where the ball is: docked to the left edge it
/// fans to the right, docked right it fans left, and while floating free it
/// fans out in a full circle.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct QuickMenuConfig {
    /// Master switch for the hover fan.
    pub enabled: bool,
    /// Icons shown per fan (clamped to `1..=MAX_ICONS`).
    pub count: usize,
    /// Concentric rings to use, 1 or 2.
    pub rings: usize,
    /// Ordered tool ids for a ball docked to the **left** edge.
    pub left: Vec<String>,
    /// Ordered tool ids for a ball docked to the **right** edge.
    pub right: Vec<String>,
    /// Ordered tool ids for a **floating** ball (full circle).
    pub center: Vec<String>,
}

/// Upper bound for [`QuickMenuConfig::count`]; two rings of eight.
pub const QUICK_MENU_MAX_ICONS: usize = 12;

impl Default for QuickMenuConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            count: 5,
            rings: 2,
            left: Vec::new(),
            right: Vec::new(),
            center: Vec::new(),
        }
    }
}

impl QuickMenuConfig {
    /// Clamp the numeric fields and clean the tool-id lists.
    pub fn normalize(&mut self) {
        self.count = self.count.clamp(1, QUICK_MENU_MAX_ICONS);
        self.rings = self.rings.clamp(1, 2);
        for list in [&mut self.left, &mut self.right, &mut self.center] {
            let mut seen = std::collections::BTreeSet::new();
            list.retain(|id| {
                let id = id.trim();
                !id.is_empty() && seen.insert(id.to_string())
            });
            for id in list.iter_mut() {
                *id = id.trim().to_string();
            }
            list.truncate(QUICK_MENU_MAX_ICONS);
        }
    }

    /// The configured tool ids for a ball position.
    ///
    /// `side` is `Some(true)` for a right-edge dock, `Some(false)` for a
    /// left-edge dock and `None` while the ball floats.
    pub fn slots(&self, side: Option<bool>) -> &[String] {
        match side {
            Some(true) => &self.right,
            Some(false) => &self.left,
            None => &self.center,
        }
    }
}


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
    /// 0.0..1.0 fraction of the work area width; used while `ball_docked` is false.
    pub ball_x: f64,
    /// `true`: pinned to `ball_side` (peek mode). `false`: floats where the user
    /// dropped it, i.e. it only docks when dragged within the snap distance.
    pub ball_docked: bool,
    /// "peek" (only a sliver visible) | "always"
    pub ball_mode: String,
    /// Hover quick menu (fan of tool icons around the ball).
    pub quick_menu: QuickMenuConfig,
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
            ball_x: 0.5,
            ball_docked: true,
            ball_mode: "peek".into(),
            quick_menu: QuickMenuConfig::default(),
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
    /// Permissions the user explicitly granted in the install review dialog.
    ///
    /// `None` means "never reviewed" (official / dev tools, or tools installed
    /// by an older Toolbox build); those fall back to the manifest permissions.
    /// Omitted from JSON when unset so existing tools.json files keep their
    /// shape and older builds can still read the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<Permissions>,
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
    cfg.ball_x = cfg.ball_x.clamp(0.0, 1.0);
    cfg.ball_y = cfg.ball_y.clamp(0.0, 1.0);
    if cfg.ball_side != "left" {
        cfg.ball_side = "right".into();
    }
    cfg.quick_menu.normalize();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_menu_normalize_clamps_and_dedupes() {
        let mut qm = QuickMenuConfig {
            enabled: true,
            count: 99,
            rings: 9,
            left: vec![" a ".into(), "a".into(), "".into(), "b".into()],
            right: Vec::new(),
            center: Vec::new(),
        };
        qm.normalize();
        assert_eq!(qm.count, QUICK_MENU_MAX_ICONS);
        assert_eq!(qm.rings, 2);
        assert_eq!(qm.left, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn quick_menu_slots_follow_ball_position() {
        let qm = QuickMenuConfig {
            left: vec!["l".into()],
            right: vec!["r".into()],
            center: vec!["c".into()],
            ..Default::default()
        };
        assert_eq!(qm.slots(Some(true)), ["r"]);
        assert_eq!(qm.slots(Some(false)), ["l"]);
        assert_eq!(qm.slots(None), ["c"]);
    }
}
