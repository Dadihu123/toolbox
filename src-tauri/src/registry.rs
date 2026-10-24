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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
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

impl Permissions {
    /// The manifest is always the ceiling: a granted set can never hand a tool
    /// a capability its own `tool.json` does not declare (tools.json can be
    /// edited by hand, and an upgrade may shrink the manifest).
    pub fn intersect(&self, declared: &Permissions) -> Permissions {
        Permissions {
            files_open: self.files_open && declared.files_open,
            files_save: self.files_save && declared.files_save,
            clipboard_read: self.clipboard_read && declared.clipboard_read,
            clipboard_write: self.clipboard_write && declared.clipboard_write,
            data: self.data && declared.data,
            external_open: self.external_open && declared.external_open,
            network: self
                .network
                .iter()
                .filter(|domain| declared.network.contains(domain))
                .cloned()
                .collect(),
        }
    }

    /// True when the set contains at least one capability.
    pub fn any(&self) -> bool {
        self.files_open
            || self.files_save
            || self.clipboard_read
            || self.clipboard_write
            || self.data
            || self.external_open
            || !self.network.is_empty()
    }
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

/// Per-language metadata a tool can ship inside `tool.json`.
///
/// The base `name` / `description` / `actions[].title` stay the fallback: a
/// language may translate any subset, and everything missing falls through to
/// the base string (and from there to the tool id).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct LocalizedManifest {
    pub name: String,
    pub description: String,
    /// action id -> translated title
    pub actions: BTreeMap<String, String>,
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
    /// BCP-47 tag -> translated metadata (`tool.json` `locales` object).
    pub locales: BTreeMap<String, LocalizedManifest>,
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
            locales: BTreeMap::new(),
            source: String::new(),
            dir: String::new(),
            error: String::new(),
        }
    }
}

impl Manifest {
    /// Pick the entry for `locale` from a per-language map.
    ///
    /// Matching chain: exact tag (`en-US`) → language prefix (`en`, also matches
    /// `en-GB` when only `en-US` is shipped) → nothing (caller falls back to the
    /// base string).
    fn localized<'a>(&'a self, locale: &str) -> Option<&'a LocalizedManifest> {
        if let Some(exact) = self.locales.get(locale) {
            return Some(exact);
        }
        let wanted = locale.to_ascii_lowercase();
        if let Some((_, hit)) = self
            .locales
            .iter()
            .find(|(tag, _)| tag.to_ascii_lowercase() == wanted)
        {
            return Some(hit);
        }
        let prefix = wanted.split('-').next().unwrap_or(&wanted).to_string();
        self.locales
            .iter()
            .find(|(tag, _)| {
                let tag = tag.to_ascii_lowercase();
                tag == prefix || tag.starts_with(&format!("{prefix}-"))
            })
            .map(|(_, hit)| hit)
    }

    /// Display name in `locale`, falling back to the base name.
    pub fn name_for(&self, locale: &str) -> String {
        self.localized(locale)
            .map(|l| l.name.trim())
            .filter(|n| !n.is_empty())
            .map(|n| n.to_string())
            .unwrap_or_else(|| self.name.clone())
    }

    /// Description in `locale`, falling back to the base description.
    pub fn description_for(&self, locale: &str) -> String {
        self.localized(locale)
            .map(|l| l.description.trim())
            .filter(|d| !d.is_empty())
            .map(|d| d.to_string())
            .unwrap_or_else(|| self.description.clone())
    }

    /// Action title in `locale`, falling back to the base title.
    pub fn action_title_for(&self, action: &ManifestAction, locale: &str) -> String {
        self.localized(locale)
            .and_then(|l| l.actions.get(&action.id))
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
            .map(|t| t.to_string())
            .unwrap_or_else(|| action.title.clone())
    }

    /// Everything searchable about the tool, in **every** shipped language.
    ///
    /// The panel matches against this so typing `对齐` or `align` both find
    /// sprite-align, without the panel having to know about manifests.
    pub fn search_text(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        parts.push(self.name.clone());
        parts.push(self.id.clone());
        parts.push(self.description.clone());
        for action in &self.actions {
            parts.push(action.title.clone());
            parts.push(action.id.clone());
        }
        for localized in self.locales.values() {
            parts.push(localized.name.clone());
            parts.push(localized.description.clone());
            for title in localized.actions.values() {
                parts.push(title.clone());
            }
        }
        parts
            .into_iter()
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn read_manifest(dir: &Path, source: &str) -> Option<Manifest> {
    let file = dir.join("tool.json");
    let text = std::fs::read_to_string(&file).ok()?;
    let mut m: Manifest = serde_json::from_str(&text).ok()?;

    let dir_name = dir.file_name()?.to_string_lossy().to_string();
    m.source = source.to_string();
    m.dir = dir.to_string_lossy().to_string();

    let problems = validate_fields(&mut m, dir, &dir_name);
    if problems.is_empty() {
        m.error.clear();
    } else {
        m.error = problems.join(&rust_i18n::t!("native.registry.join"));
    }
    Some(m)
}

/// Field-level validation, split from the file IO so it can be unit tested.
fn validate_fields(m: &mut Manifest, dir: &Path, dir_name: &str) -> Vec<String> {
    let mut problems: Vec<String> = Vec::new();
    if m.id.is_empty() {
        m.id = dir_name.to_string();
    }
    if m.id != dir_name {
        problems.push(
            rust_i18n::t!("native.registry.id_mismatch", id = m.id.as_str(), dir = dir_name)
                .to_string(),
        );
    }
    if !m
        .id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        problems.push(rust_i18n::t!("native.registry.bad_id").to_string());
    }
    if m.name.trim().is_empty() {
        problems.push(rust_i18n::t!("native.registry.missing_name").to_string());
    }
    if m.manifest_version > 1 {
        problems.push(
            rust_i18n::t!(
                "native.registry.manifest_version",
                version = m.manifest_version
            )
            .to_string(),
        );
    }
    // Bridge v1 and v2 are both loadable; the v2 shim stays backwards compatible
    // with v1 tools, so anything newer is the only hard rejection.
    if m.bridge_version > 2 {
        problems.push(
            rust_i18n::t!("native.registry.bridge_version", version = m.bridge_version).to_string(),
        );
    }
    let entry_path = dir.join(&m.entry);
    if !entry_path.is_file() {
        problems.push(
            rust_i18n::t!("native.registry.entry_missing", entry = m.entry.as_str()).to_string(),
        );
    }
    // entry must stay inside the tool directory
    if let Some(parent) = entry_path.parent() {
        if paths::ensure_inside(dir, parent).is_none() && entry_path.exists() {
            problems.push(rust_i18n::t!("native.registry.entry_escape").to_string());
        }
    }
    problems
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

/// Permissions that actually apply to a tool.
///
/// Installed tools that went through the review dialog use the subset the user
/// granted (clamped to the manifest). Official / dev tools and installs made by
/// older builds have no stored grant and keep their manifest permissions.
pub fn effective_permissions(m: &Manifest) -> Permissions {
    effective_permissions_with(&state::load_tools(), m)
}

/// Same as [`effective_permissions`] but reuses an already loaded tools.json.
pub fn effective_permissions_with(
    states: &BTreeMap<String, ToolState>,
    m: &Manifest,
) -> Permissions {
    if m.source == "installed" {
        if let Some(granted) = states.get(&m.id).and_then(|s| s.permissions.as_ref()) {
            return granted.intersect(&m.permissions);
        }
    }
    m.permissions.clone()
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
            permissions: None,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn installed_manifest(id: &str, permissions: Permissions) -> Manifest {
        Manifest {
            id: id.into(),
            name: "测试工具".into(),
            source: "installed".into(),
            permissions,
            ..Default::default()
        }
    }

    fn states_with(id: &str, granted: Option<Permissions>) -> BTreeMap<String, ToolState> {
        let mut states = BTreeMap::new();
        states.insert(
            id.to_string(),
            ToolState {
                enabled: true,
                source: "installed".into(),
                permissions: granted,
                ..Default::default()
            },
        );
        states
    }

    #[test]
    fn effective_permissions_use_the_granted_subset_for_installed_tools() {
        let manifest = installed_manifest(
            "demo",
            Permissions {
                files_open: true,
                files_save: true,
                clipboard_write: true,
                network: vec!["api.example.com".into()],
                ..Default::default()
            },
        );
        let granted = Permissions {
            files_open: true,
            files_save: false,
            // Not declared by the manifest: must stay off.
            data: true,
            network: vec!["api.example.com".into(), "evil.example.com".into()],
            ..Default::default()
        };
        let states = states_with("demo", Some(granted));

        let effective = effective_permissions_with(&states, &manifest);
        assert!(effective.files_open, "checked permission is granted");
        assert!(!effective.files_save, "unchecked permission stays off");
        assert!(!effective.data, "manifest stays the ceiling");
        assert_eq!(effective.network, ["api.example.com"]);
    }

    #[test]
    fn effective_permissions_fall_back_to_the_manifest_without_a_grant() {
        let manifest = installed_manifest(
            "demo",
            Permissions {
                files_open: true,
                data: true,
                ..Default::default()
            },
        );

        // Old install (no stored grant) keeps the declared permissions.
        let states = states_with("demo", None);
        assert_eq!(
            effective_permissions_with(&states, &manifest),
            manifest.permissions
        );

        // Official tools ignore any (hand edited) grant in tools.json.
        let mut official = manifest.clone();
        official.source = "official".into();
        let states = states_with("demo", Some(Permissions::default()));
        assert_eq!(
            effective_permissions_with(&states, &official),
            manifest.permissions
        );
    }

    #[test]
    fn permissions_intersect_keeps_only_declared_capabilities() {
        let declared = Permissions {
            files_open: true,
            clipboard_read: true,
            network: vec!["a.example.com".into()],
            ..Default::default()
        };
        let granted = Permissions {
            files_open: true,
            files_save: true,
            clipboard_read: true,
            network: vec!["a.example.com".into(), "b.example.com".into()],
            ..Default::default()
        };
        let effective = granted.intersect(&declared);
        assert!(effective.files_open);
        assert!(effective.clipboard_read);
        assert!(!effective.files_save);
        assert_eq!(effective.network, ["a.example.com"]);
        assert!(granted.any());
        assert!(!Permissions::default().any());
    }

    fn localized_manifest() -> Manifest {
        let mut m = Manifest {
            id: "sprite-align".into(),
            name: "序列帧对齐".into(),
            description: "切分 sprite sheet".into(),
            actions: vec![
                ManifestAction {
                    id: "open".into(),
                    title: "打开序列帧对齐".into(),
                    ..Default::default()
                },
                ManifestAction {
                    id: "export-zip".into(),
                    title: "导出为 ZIP".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let mut en = LocalizedManifest {
            name: "Sprite Align".into(),
            description: "Slice sprite sheets".into(),
            ..Default::default()
        };
        en.actions.insert("open".into(), "Open Sprite Align".into());
        m.locales.insert("en-US".into(), en);

        let mut zh_tw = LocalizedManifest {
            name: "序列影格對齊".into(),
            ..Default::default()
        };
        zh_tw.actions.insert("export-zip".into(), "匯出為 ZIP".into());
        m.locales.insert("zh-TW".into(), zh_tw);
        m
    }

    #[test]
    fn localized_metadata_prefers_exact_then_prefix_then_base() {
        let m = localized_manifest();
        // exact tag
        assert_eq!(m.name_for("en-US"), "Sprite Align");
        assert_eq!(m.description_for("en-US"), "Slice sprite sheets");
        assert_eq!(m.action_title_for(&m.actions[0], "en-US"), "Open Sprite Align");
        // language prefix: en-GB has no entry, en-US matches on `en`
        assert_eq!(m.name_for("en-GB"), "Sprite Align");
        // zh-TW exists, zh-CN does not -> prefix `zh` picks zh-TW
        assert_eq!(m.name_for("zh-TW"), "序列影格對齊");
        assert_eq!(m.name_for("zh-CN"), "序列影格對齊");
        // action absent from the localised block falls back to the base title
        assert_eq!(
            m.action_title_for(&m.actions[1], "en-US"),
            "导出为 ZIP",
            "a partially translated tool keeps the base action title"
        );
        // completely unknown language -> base strings
        assert_eq!(m.name_for("de-DE"), "序列帧对齐");
        assert_eq!(m.description_for("de-DE"), "切分 sprite sheet");
    }

    #[test]
    fn search_text_covers_every_shipped_language() {
        let m = localized_manifest();
        let text = m.search_text();
        for needle in [
            "序列帧对齐",
            "Sprite Align",
            "序列影格對齊",
            "Slice sprite sheets",
            "Open Sprite Align",
            "导出为 ZIP",
            "匯出為 ZIP",
            "open",
            "export-zip",
        ] {
            assert!(text.contains(needle), "search text is missing {needle:?}: {text}");
        }
    }

    #[test]
    fn bridge_versions_one_and_two_load_and_later_ones_are_rejected() {
        let dir = std::path::Path::new(".");
        for version in [1u32, 2] {
            let mut m = Manifest {
                id: "sprite-align".into(),
                name: "x".into(),
                bridge_version: version,
                entry: "Cargo.toml".into(), // exists in the crate root, keeps the entry check quiet
                ..Default::default()
            };
            let problems = validate_fields(&mut m, dir, "sprite-align");
            assert!(
                !problems.iter().any(|p| p.contains("Bridge")),
                "bridge v{version} must load: {problems:?}"
            );
        }

        let mut m = Manifest {
            id: "sprite-align".into(),
            name: "x".into(),
            bridge_version: 3,
            entry: "Cargo.toml".into(),
            ..Default::default()
        };
        let problems = validate_fields(&mut m, dir, "sprite-align");
        assert!(
            problems.iter().any(|p| p.contains("Bridge")),
            "bridge v3 must be rejected: {problems:?}"
        );
    }
}
