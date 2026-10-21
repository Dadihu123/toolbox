//! Tool discovery: scans the official tools directory (shipped with the app)
//! and the installed tools directory (user installed, higher priority).

use base64::Engine;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::paths;
use crate::state::{self, ToolState};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct WindowSpec {
    pub width: f64,
    pub height: f64,
    pub min_width: f64,
    pub min_height: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Permissions {
    pub files_open: bool,
    pub files_save: bool,
    pub clipboard_read: bool,
    pub clipboard_write: bool,
    pub data: bool,
    pub external_open: bool,
    pub network: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ManifestAction {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub default_hotkey: String,
    #[serde(default)]
    pub requires_open: bool,
    #[serde(default)]
    pub accepts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Manifest {
    pub manifest_version: u32,
    pub bridge_version: u32,
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub author: String,
    pub entry: String,
    pub icon: String,
    pub accent: String,
    pub window: WindowSpec,
    pub permissions: Permissions,
    pub actions: Vec<ManifestAction>,
    pub accepts: Vec<String>,
    /// "official" | "installed" | "dev" - filled in by the scanner.
    pub source: String,
    /// Absolute directory of the tool.
    pub dir: String,
    /// Manifest validation error, if any.
    pub error: String,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            manifest_version: 1,
            bridge_version: 1,
            id: String::new(),
            name: String::new(),
            description: String::new(),
            version: "0.0.0".into(),
            author: String::new(),
            entry: "index.html".into(),
            icon: "icon.svg".into(),
            accent: "#4ea1ff".into(),
            window: WindowSpec {
                width: 1100.0,
                height: 760.0,
                min_width: 720.0,
                min_height: 480.0,
            },
            permissions: Permissions::default(),
            actions: Vec::new(),
            accepts: Vec::new(),
            source: String::new(),
            dir: String::new(),
            error: String::new(),
        }
    }
}

fn read_manifest(dir: &Path, source: &str) -> Option<Manifest> {
    let file = dir.join("tool.json");
    let text = std::fs::read_to_string(&file).ok()?;
    let mut m: Manifest = serde_json::from_str(&text).ok()?;

    let dir_name = dir.file_name()?.to_string_lossy().to_string();
    m.source = source.to_string();
    m.dir = dir.to_string_lossy().to_string();

    // --- validation -------------------------------------------------------
    let mut problems: Vec<String> = Vec::new();
    if m.id.is_empty() {
        m.id = dir_name.clone();
    }
    if m.id != dir_name {
        problems.push(format!("清单 id「{}」与目录名「{}」不一致", m.id, dir_name));
    }
    if !m
        .id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        problems.push("id 只能包含小写字母、数字、- 和 _".into());
    }
    if m.name.trim().is_empty() {
        problems.push("缺少 name".into());
    }
    if m.manifest_version > 1 {
        problems.push(format!(
            "清单版本 {} 高于本工具箱支持的 1",
            m.manifest_version
        ));
    }
    if m.bridge_version > 1 {
        problems.push(format!(
            "需要 Bridge v{}，本工具箱仅支持 v1",
            m.bridge_version
        ));
    }
    let entry_path = dir.join(&m.entry);
    if !entry_path.is_file() {
        problems.push(format!("入口文件不存在：{}", m.entry));
    }
    // entry must stay inside the tool directory
    if let Some(parent) = entry_path.parent() {
        if paths::ensure_inside(dir, parent).is_none() && entry_path.exists() {
            problems.push("入口路径越界".into());
        }
    }
    if problems.is_empty() {
        m.error.clear();
    } else {
        m.error = problems.join("；");
    }
    Some(m)
}

fn scan_dir(base: &Path, source: &str, out: &mut Vec<Manifest>) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if name.starts_with('.') || name.starts_with('_') {
            continue;
        }
        if let Some(m) = read_manifest(&path, source) {
            out.push(m);
        }
    }
}

/// Scan both roots. Installed tools win on id collision.
pub fn scan() -> Vec<Manifest> {
    let p = paths::get();
    let mut official = Vec::new();
    scan_dir(&p.official_tools, "official", &mut official);
    let mut installed = Vec::new();
    scan_dir(&p.installed_tools, "installed", &mut installed);

    let mut by_id: BTreeMap<String, Manifest> = BTreeMap::new();
    for m in official {
        by_id.insert(m.id.clone(), m);
    }
    for m in installed {
        by_id.insert(m.id.clone(), m);
    }
    by_id.into_values().collect()
}

pub fn find(id: &str) -> Option<Manifest> {
    scan().into_iter().find(|m| m.id == id)
}

pub fn tool_dir(id: &str) -> Option<PathBuf> {
    find(id).map(|m| PathBuf::from(m.dir))
}

/// Enabled flag from tools.json, defaulting to enabled.
pub fn is_enabled(states: &BTreeMap<String, ToolState>, id: &str) -> bool {
    states.get(id).map(|s| s.enabled).unwrap_or(true)
}

pub fn icon_data_url(dir: &Path, icon: &str) -> Option<String> {
    if icon.trim().is_empty() {
        return None;
    }
    let path = dir.join(icon);
    let safe = paths::ensure_inside(dir, &path).or_else(|| {
        // canonicalize fails for non-existent files; fall back to lexical check
        if path.starts_with(dir) {
            Some(path.clone())
        } else {
            None
        }
    })?;
    let bytes = std::fs::read(&safe).ok()?;
    if bytes.len() > 512 * 1024 {
        return None;
    }
    let mime = match safe
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    };
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    Some(format!("data:{mime};base64,{b64}"))
}

pub fn dir_size(dir: &Path) -> u64 {
    fn walk(p: &Path, acc: &mut u64, depth: u32) {
        if depth > 8 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(p) else {
            return;
        };
        for e in entries.flatten() {
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() {
                walk(&e.path(), acc, depth + 1);
            } else {
                *acc += md.len();
            }
        }
    }
    let mut total = 0;
    walk(dir, &mut total, 0);
    total
}

/// Refresh the source/installed-at bookkeeping after a scan.
pub fn sync_states(states: &mut BTreeMap<String, ToolState>, tools: &[Manifest]) {
    for t in tools {
        let entry = states.entry(t.id.clone()).or_insert_with(|| ToolState {
            enabled: true,
            installed_at: now_iso(),
            source: t.source.clone(),
            pinned: false,
        });
        entry.source = t.source.clone();
    }
    if let Err(e) = state::save_tools(states) {
        eprintln!("[toolbox] save tools.json failed: {e}");
    }
}

pub fn now_iso() -> String {
    // Small dependency-free timestamp: seconds since epoch rendered as ISO-8601 UTC.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_iso(secs)
}

fn format_iso(secs: u64) -> String {
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // civil-from-days (Howard Hinnant)
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}
