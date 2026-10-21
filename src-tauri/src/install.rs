//! Installing and uninstalling third party tools (zip based).
//!
//! Installing is a two-phase process:
//!
//! 1. [`inspect_zip`] unpacks a zip into `<data>/tmp/install-<uuid>/`, validates
//!    it and returns a structured [`Inspection`] (metadata, size, permission
//!    list). Nothing is installed yet; the unpacked tree is kept in a Rust-side
//!    static so JS can never point the installer at an arbitrary path.
//! 2. [`install_pending`] consumes that pending inspection plus the permission
//!    keys the user ticked in the review dialog, then copies the staged tree
//!    into `data/tools/<id>/` and records the granted permissions in tools.json.
//!
//! Cancelling (or picking another zip) discards the staging directory, so a
//! cancelled review leaves nothing installed and no state behind.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use serde_json::Value;

use crate::paths;
use crate::registry::{self, Manifest, Permissions};
use crate::state::{self, ToolState};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallResult {
    pub ok: bool,
    pub id: String,
    pub name: String,
    pub error: String,
}

/// One permission row of the install review dialog.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionItem {
    /// Stable id used when the UI reports back the granted subset.
    pub key: String,
    pub label: String,
    pub detail: String,
    /// Whether the tool's manifest actually declares this permission.
    pub bit: bool,
}

/// What the review dialog shows and what `install_tool` later installs.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inspection {
    pub ok: bool,
    pub error: String,
    /// Opaque token; must be sent back to `install_tool`.
    pub inspection_id: String,
    pub zip_path: String,
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    pub entry: String,
    pub file_count: u64,
    pub total_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_data_url: Option<String>,
    pub is_upgrade: bool,
    /// "installed" when the id is already installed, empty otherwise.
    pub existing_source: String,
    /// Permissions as declared by the manifest (ceiling for the grant).
    pub permissions: Permissions,
    pub permission_items: Vec<PermissionItem>,
}

const MAX_ENTRIES: usize = 5_000;
const MAX_BYTES: u64 = 200 * 1024 * 1024;
const SESSION_EXPIRED: &str = "安装会话已失效，请重新选择工具包";

/// A zip that has been unpacked and reviewed, waiting for the user's decision.
struct PendingInstall {
    inspection_id: String,
    zip_path: PathBuf,
    /// `<data>/tmp/install-<uuid>` (always removed after install/cancel).
    staging: PathBuf,
    /// Tool root inside the staging dir (zip may have a single wrapper dir).
    root: PathBuf,
    inspection: Inspection,
}

static PENDING: Mutex<Option<PendingInstall>> = Mutex::new(None);

fn pending_lock() -> std::sync::MutexGuard<'static, Option<PendingInstall>> {
    PENDING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Directories used by the installer. Injectable so unit tests never touch the
/// real portable data directory.
#[derive(Debug, Clone)]
struct InstallLayout {
    staging_base: PathBuf,
    installed_tools: PathBuf,
    official_tools: PathBuf,
    backups: PathBuf,
    tools_json: PathBuf,
}

impl InstallLayout {
    fn from_paths() -> Self {
        let p = paths::get();
        Self {
            staging_base: p.data.join("tmp"),
            installed_tools: p.installed_tools.clone(),
            official_tools: p.official_tools.clone(),
            backups: p.backups.join("tools"),
            tools_json: state::tools_path(),
        }
    }
}

// ------------------------------------------------------------------- inspect

/// Unpack + validate a zip the user picked, and remember it as the pending
/// install. Installs nothing: `data/tools/` and tools.json stay untouched.
pub fn inspect_zip(zip_path: &Path) -> Result<Inspection, String> {
    let layout = InstallLayout::from_paths();
    let mut pending = pending_lock();
    // A new pick always replaces the previous review (cancelled or not).
    discard_pending(&mut pending);
    let _ = std::fs::create_dir_all(&layout.staging_base);
    sweep_stale_staging(&layout.staging_base);

    let staging = layout
        .staging_base
        .join(format!("install-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;

    match inspect_at(zip_path, &staging, &layout) {
        Ok((inspection, root)) => {
            *pending = Some(PendingInstall {
                inspection_id: inspection.inspection_id.clone(),
                zip_path: zip_path.to_path_buf(),
                staging,
                root,
                inspection: inspection.clone(),
            });
            Ok(inspection)
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            Err(e)
        }
    }
}

/// Drop the pending review and its staging directory. Used when the user
/// cancels and when a new zip replaces an unreviewed one.
pub fn cancel_pending() {
    let mut pending = pending_lock();
    discard_pending(&mut pending);
}

fn discard_pending(pending: &mut Option<PendingInstall>) {
    if let Some(old) = pending.take() {
        let _ = std::fs::remove_dir_all(&old.staging);
    }
}

/// Leftover staging dirs from a crash / force quit. Safe: a single instance
/// means no other process is mid-install.
fn sweep_stale_staging(base: &Path) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with("install-") {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Extract, validate and describe a zip into an existing `staging` dir.
/// Returns the inspection and the tool root inside the staging dir.
fn inspect_at(
    zip_path: &Path,
    staging: &Path,
    layout: &InstallLayout,
) -> Result<(Inspection, PathBuf), String> {
    extract_zip(zip_path, staging)?;
    // A zip may either contain tool.json at the root or a single wrapper dir.
    let root = find_manifest_root(staging).ok_or_else(|| "压缩包内未找到 tool.json".to_string())?;
    let text = std::fs::read_to_string(root.join("tool.json")).map_err(|e| e.to_string())?;
    let manifest: Manifest =
        serde_json::from_str(&text).map_err(|e| format!("tool.json 解析失败：{e}"))?;

    let fallback_id = root
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let (id, entry) = validate_manifest(&root, &manifest, &fallback_id)?;

    // Never overwrite a bundled tool: official ones are read-only.
    if layout.official_tools.join(&id).exists() {
        return Err(format!("「{id}」是内置工具，不能覆盖"));
    }

    let (file_count, total_bytes) = dir_stats(&root);
    if file_count > MAX_ENTRIES as u64 || total_bytes > MAX_BYTES {
        return Err("压缩包解压后体积过大".into());
    }

    let dest = layout.installed_tools.join(&id);
    let is_upgrade = dest.exists();
    let existing_source = if is_upgrade {
        "installed".to_string()
    } else {
        String::new()
    };

    let permissions = manifest.permissions.clone();
    let permission_items = permission_items(&id, &permissions);
    let inspection = Inspection {
        ok: true,
        error: String::new(),
        inspection_id: uuid::Uuid::new_v4().to_string(),
        zip_path: zip_path.to_string_lossy().to_string(),
        id,
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        author: manifest.author.clone(),
        description: manifest.description.clone(),
        entry,
        file_count,
        total_bytes,
        icon_data_url: registry::icon_data_url(&root, &manifest.icon),
        is_upgrade,
        existing_source,
        permission_items,
        permissions,
    };
    Ok((inspection, root))
}

/// Validate the manifest of an unpacked tool. Returns `(id, entry)`.
fn validate_manifest(root: &Path, manifest: &Manifest, fallback_id: &str) -> Result<(String, String), String> {
    let id = if manifest.id.trim().is_empty() {
        fallback_id.to_string()
    } else {
        manifest.id.trim().to_string()
    };
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err("工具 id 非法（只允许小写字母、数字、- 和 _）".into());
    }
    if manifest.name.trim().is_empty() {
        return Err("tool.json 缺少工具名称（name）".into());
    }
    if manifest.manifest_version > 1 {
        return Err(format!(
            "清单版本 {} 高于本工具箱支持的最高版本 1",
            manifest.manifest_version
        ));
    }
    if manifest.bridge_version > 1 {
        return Err(format!(
            "该工具需要 Bridge v{}，本工具箱仅支持 v1",
            manifest.bridge_version
        ));
    }
    let entry = if manifest.entry.trim().is_empty() {
        "index.html".to_string()
    } else {
        manifest.entry.trim().to_string()
    };
    let entry_path = root.join(&entry);
    if !entry_path.is_file() {
        return Err(format!("入口文件不存在：{entry}"));
    }
    if paths::ensure_inside(root, &entry_path).is_none() {
        return Err("入口路径越界".into());
    }
    Ok((id, entry))
}

/// Chinese labels/explanations shown next to the checkboxes.
fn permission_items(id: &str, permissions: &Permissions) -> Vec<PermissionItem> {
    let network_detail = if permissions.network.is_empty() {
        "访问指定网络域名".to_string()
    } else {
        format!("访问指定网络域名：{}", permissions.network.join("、"))
    };
    let data_detail = format!("在 data/tool-data/{id} 下保存私有数据");
    let rows: [(&str, &str, String, bool); 7] = [
        ("files.open", "读取文件", "读取你选择的文件".into(), permissions.files_open),
        ("files.save", "保存文件", "弹出保存对话框并写入你选择的文件".into(), permissions.files_save),
        ("clipboard.read", "读取剪贴板", "读取系统剪贴板内容".into(), permissions.clipboard_read),
        ("clipboard.write", "写入剪贴板", "写入系统剪贴板".into(), permissions.clipboard_write),
        ("data", "本地数据", data_detail, permissions.data),
        ("external.open", "外部链接", "用系统浏览器打开链接".into(), permissions.external_open),
        ("network", "网络访问", network_detail, !permissions.network.is_empty()),
    ];
    rows.into_iter()
        .map(|(key, label, detail, bit)| PermissionItem {
            key: key.to_string(),
            label: label.to_string(),
            detail,
            bit,
        })
        .collect()
}

// ------------------------------------------------------------------- install

/// Install the pending inspection. `granted` is the permission subset the user
/// ticked in the review dialog (`["files.open", ...]` or `{"files.open": true}`).
pub fn install_pending(inspection_id: &str, granted: &Value) -> Result<InstallResult, String> {
    let layout = InstallLayout::from_paths();
    let mut pending = pending_lock();
    let Some(item) = pending.take() else {
        return Err(SESSION_EXPIRED.into());
    };
    if item.inspection_id != inspection_id {
        let _ = std::fs::remove_dir_all(&item.staging);
        return Err(SESSION_EXPIRED.into());
    }
    crate::logging::write(
        "info",
        "install",
        &format!(
            "installing {} from {}",
            item.inspection.id,
            item.zip_path.display()
        ),
    );
    let result = commit_install(&layout, &item, granted);
    let _ = std::fs::remove_dir_all(&item.staging);
    match &result {
        Ok(r) => logging_install(&r.id),
        Err(_) => {}
    }
    result
}

/// Copy the staged tree into `data/tools/<id>/`, then record the grant.
fn commit_install(
    layout: &InstallLayout,
    item: &PendingInstall,
    granted: &Value,
) -> Result<InstallResult, String> {
    let inspection = &item.inspection;
    let id = inspection.id.clone();
    if layout.official_tools.join(&id).exists() {
        return Err(format!("「{id}」是内置工具，不能覆盖"));
    }
    let permissions = granted_permissions(&inspection.permissions, granted)?;

    let dest = layout.installed_tools.join(&id);
    std::fs::create_dir_all(&layout.installed_tools).map_err(|e| e.to_string())?;

    // Upgrade: keep a backup of the current version like the design doc says.
    let mut backup: Option<PathBuf> = None;
    if dest.exists() {
        std::fs::create_dir_all(&layout.backups).map_err(|e| e.to_string())?;
        let backup_dir = layout.backups.join(format!(
            "{id}-{}",
            registry::now_iso().replace(':', "-")
        ));
        std::fs::rename(&dest, &backup_dir).map_err(|e| e.to_string())?;
        backup = Some(backup_dir);
    }

    if let Err(e) = copy_dir(&item.root, &dest) {
        // Roll back to the previous version instead of leaving a broken tool.
        let _ = std::fs::remove_dir_all(&dest);
        if let Some(backup) = &backup {
            let _ = std::fs::rename(backup, &dest);
        }
        return Err(e);
    }

    let mut states = load_tools_at(&layout.tools_json);
    let (enabled, pinned, installed_at) = match states.get(&id) {
        Some(previous) => (previous.enabled, previous.pinned, registry::now_iso()),
        None => (true, false, registry::now_iso()),
    };
    states.insert(
        id.clone(),
        ToolState {
            enabled,
            installed_at,
            source: "installed".into(),
            pinned,
            permissions: Some(permissions),
        },
    );
    if let Err(e) = state::write_json(&layout.tools_json, &states) {
        let _ = std::fs::remove_dir_all(&dest);
        if let Some(backup) = &backup {
            let _ = std::fs::rename(backup, &dest);
        }
        return Err(format!("写入 tools.json 失败：{e}"));
    }

    Ok(InstallResult {
        ok: true,
        id,
        name: inspection.name.clone(),
        error: String::new(),
    })
}

/// Parse the granted keys the review dialog sent back and clamp them to the
/// manifest. Unknown keys are rejected so a UI/backend mismatch is visible
/// instead of silently granting nothing.
pub fn granted_permissions(declared: &Permissions, granted: &Value) -> Result<Permissions, String> {
    let mut picked: BTreeSet<&'static str> = BTreeSet::new();
    let mut accept = |raw: &str| -> Result<(), String> {
        let key = normalize_permission_key(raw)
            .ok_or_else(|| format!("未知的权限项：{raw}"))?;
        picked.insert(key);
        Ok(())
    };
    match granted {
        Value::Null => {}
        Value::Array(items) => {
            for item in items {
                let raw = item
                    .as_str()
                    .ok_or_else(|| "granted 参数格式不正确".to_string())?;
                accept(raw)?;
            }
        }
        Value::Object(map) => {
            for (raw, enabled) in map {
                if enabled.as_bool().unwrap_or(false) {
                    accept(raw)?;
                }
            }
        }
        _ => return Err("granted 参数格式不正确".into()),
    }

    let has = |key: &str| picked.contains(key);
    Ok(Permissions {
        files_open: has("files.open") && declared.files_open,
        files_save: has("files.save") && declared.files_save,
        clipboard_read: has("clipboard.read") && declared.clipboard_read,
        clipboard_write: has("clipboard.write") && declared.clipboard_write,
        data: has("data") && declared.data,
        external_open: has("external.open") && declared.external_open,
        network: if has("network") {
            declared.network.clone()
        } else {
            Vec::new()
        },
    })
}

fn normalize_permission_key(raw: &str) -> Option<&'static str> {
    match raw.trim() {
        "files.open" | "filesOpen" => Some("files.open"),
        "files.save" | "filesSave" => Some("files.save"),
        "clipboard.read" | "clipboardRead" => Some("clipboard.read"),
        "clipboard.write" | "clipboardWrite" => Some("clipboard.write"),
        "data" => Some("data"),
        "external.open" | "externalOpen" => Some("external.open"),
        "network" => Some("network"),
        _ => None,
    }
}

fn logging_install(id: &str) {
    crate::logging::write("info", "install", &format!("installed tool {id}"));
}

pub fn uninstall(tool_id: &str, purge_data: bool) -> Result<(), String> {
    let p = paths::get();
    let dir = p.installed_tools.join(tool_id);
    if !dir.exists() {
        return Err("只能卸载第三方工具".into());
    }
    std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut states = state::load_tools();
    states.remove(tool_id);
    let _ = state::save_tools(&states);
    if purge_data {
        let _ = std::fs::remove_dir_all(p.tool_store.join(tool_id));
    }
    crate::logging::write("info", "install", &format!("uninstalled tool {tool_id}"));
    Ok(())
}

// ------------------------------------------------------------------- helpers

fn load_tools_at(path: &Path) -> BTreeMap<String, ToolState> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn find_manifest_root(dir: &Path) -> Option<PathBuf> {
    if dir.join("tool.json").is_file() {
        return Some(dir.to_path_buf());
    }
    let entries = std::fs::read_dir(dir).ok()?;
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    if dirs.len() == 1 {
        let d = dirs.remove(0);
        if d.join("tool.json").is_file() {
            return Some(d);
        }
    }
    None
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(from).map_err(|e| e.to_string())?.flatten() {
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            copy_dir(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// `(file_count, total_bytes)` of a directory tree.
fn dir_stats(dir: &Path) -> (u64, u64) {
    fn walk(path: &Path, depth: u32, count: &mut u64, bytes: &mut u64) {
        if depth > 16 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                walk(&entry.path(), depth + 1, count, bytes);
            } else {
                *count += 1;
                *bytes += metadata.len();
            }
        }
    }
    let (mut count, mut bytes) = (0u64, 0u64);
    walk(dir, 0, &mut count, &mut bytes);
    (count, bytes)
}

fn extract_zip(zip_path: &Path, dest: &Path) -> Result<(), String> {
    let file = std::fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("不是有效的 zip：{e}"))?;
    if archive.len() > MAX_ENTRIES {
        return Err("压缩包内文件过多".into());
    }
    let mut total: u64 = 0;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let Some(name) = entry.enclosed_name() else {
            return Err("压缩包内包含非法路径".into());
        };
        if name.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err("压缩包内包含非法路径".into());
        }
        total += entry.size();
        if total > MAX_BYTES {
            return Err("压缩包解压后体积过大".into());
        }
        let out = dest.join(name);
        if entry.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
        } else {
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).map_err(|e| e.to_string())?;
            if buf.len() as u64 > MAX_BYTES {
                return Err("压缩包解压后体积过大".into());
            }
            std::fs::write(&out, buf).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

// --------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    /// Temporary directory tree removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut candidates = Vec::new();
            candidates.push(std::env::temp_dir());
            if let Ok(dir) = std::env::current_dir() {
                candidates.push(dir.join("target").join("test-tmp"));
            }
            let mut last_error = String::new();
            for base in candidates {
                let path = base.join(format!("toolbox-test-{tag}-{}", uuid::Uuid::new_v4()));
                match std::fs::create_dir_all(&path) {
                    Ok(()) => return TempDir(path),
                    Err(e) => last_error = format!("{}: {e}", path.display()),
                }
            }
            panic!("cannot create temp test dir ({last_error})");
        }

        fn layout(&self) -> InstallLayout {
            let root = &self.0;
            InstallLayout {
                staging_base: root.join("tmp"),
                installed_tools: root.join("data").join("tools"),
                official_tools: root.join("tools"),
                backups: root.join("data").join("backups").join("tools"),
                tools_json: root.join("data").join("tools.json"),
            }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_zip(dir: &Path, name: &str, files: &[(&str, &[u8])]) -> PathBuf {
        let path = dir.join(name);
        let file = std::fs::File::create(&path).expect("create zip");
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (entry, content) in files {
            zip.start_file(*entry, options).expect("start zip entry");
            zip.write_all(content).expect("write zip entry");
        }
        zip.finish().expect("finish zip");
        path
    }

    fn manifest_json(id: &str) -> String {
        json!({
            "manifestVersion": 1,
            "bridgeVersion": 1,
            "id": id,
            "name": "演示工具",
            "description": "用于安装流程单测的示例工具。",
            "version": "1.2.3",
            "author": "测试作者",
            "entry": "index.html",
            "permissions": {
                "filesOpen": true,
                "filesSave": true,
                "data": true,
                "externalOpen": true,
                "network": ["api.example.com", "cdn.example.com"]
            }
        })
        .to_string()
    }

    fn sample_zip(dir: &Path) -> PathBuf {
        write_zip(
            dir,
            "demo.zip",
            &[
                ("tool.json", manifest_json("demo-tool").as_bytes()),
                ("index.html", b"<h1>demo</h1>"),
                ("app.js", b"console.log('demo');"),
                (
                    "icon.svg",
                    br#"<svg xmlns="http://www.w3.org/2000/svg"><rect width="8" height="8"/></svg>"#,
                ),
            ],
        )
    }

    fn inspect_sample(zip: &Path, layout: &InstallLayout) -> (Inspection, PathBuf) {
        let staging = layout.staging_base.join("install-test");
        std::fs::create_dir_all(&staging).expect("create staging");
        inspect_at(zip, &staging, layout).expect("inspect should succeed")
    }

    #[test]
    fn inspect_reports_metadata_size_and_permissions() {
        let temp = TempDir::new("inspect");
        let layout = temp.layout();
        std::fs::create_dir_all(&layout.installed_tools).unwrap();
        std::fs::create_dir_all(&layout.official_tools).unwrap();
        let zip = sample_zip(&temp.0);

        let staging = layout.staging_base.join("install-test");
        std::fs::create_dir_all(&staging).expect("create staging");
        let (inspection, root) = inspect_at(&zip, &staging, &layout).expect("inspect");

        assert!(inspection.ok);
        assert!(!inspection.inspection_id.is_empty());
        assert_eq!(inspection.id, "demo-tool");
        assert_eq!(inspection.name, "演示工具");
        assert_eq!(inspection.version, "1.2.3");
        assert_eq!(inspection.author, "测试作者");
        assert_eq!(inspection.description, "用于安装流程单测的示例工具。");
        assert_eq!(inspection.entry, "index.html");
        assert_eq!(inspection.file_count, 4);
        assert_eq!(inspection.total_bytes, dir_stats(&root).1);
        assert!(!inspection.is_upgrade);
        assert!(inspection.existing_source.is_empty());
        assert!(inspection.icon_data_url.as_deref().unwrap_or("").starts_with("data:image/svg+xml;base64,"));

        assert!(inspection.permissions.files_open);
        assert!(inspection.permissions.files_save);
        assert!(inspection.permissions.data);
        assert!(inspection.permissions.external_open);
        assert!(!inspection.permissions.clipboard_read);
        assert_eq!(inspection.permissions.network, ["api.example.com", "cdn.example.com"]);

        assert_eq!(inspection.permission_items.len(), 7);
        let item = |key: &str| {
            inspection
                .permission_items
                .iter()
                .find(|item| item.key == key)
                .unwrap_or_else(|| panic!("missing permission item {key}"))
        };
        assert!(item("files.open").bit);
        assert_eq!(item("files.open").detail, "读取你选择的文件");
        assert!(!item("clipboard.read").bit);
        assert_eq!(item("data").detail, "在 data/tool-data/demo-tool 下保存私有数据");
        assert!(item("network").detail.contains("api.example.com"));
        assert!(!item("network").detail.is_empty());
    }

    #[test]
    fn inspect_detects_upgrade_and_reports_existing_source() {
        let temp = TempDir::new("upgrade");
        let layout = temp.layout();
        std::fs::create_dir_all(&layout.installed_tools.join("demo-tool")).unwrap();
        std::fs::create_dir_all(&layout.official_tools).unwrap();
        let zip = sample_zip(&temp.0);

        let (inspection, _) = inspect_sample(&zip, &layout);
        assert!(inspection.is_upgrade);
        assert_eq!(inspection.existing_source, "installed");
    }

    #[test]
    fn inspect_rejects_missing_manifest() {
        let temp = TempDir::new("no-manifest");
        let layout = temp.layout();
        std::fs::create_dir_all(&layout.installed_tools).unwrap();
        std::fs::create_dir_all(&layout.official_tools).unwrap();
        let zip = write_zip(&temp.0, "bad.zip", &[("index.html", b"<h1>no manifest</h1>")]);

        let staging = layout.staging_base.join("install-bad");
        std::fs::create_dir_all(&staging).unwrap();
        let error = inspect_at(&zip, &staging, &layout).unwrap_err();
        assert!(error.contains("tool.json"), "unexpected error: {error}");
    }

    #[test]
    fn inspect_rejects_illegal_id() {
        let temp = TempDir::new("illegal-id");
        let layout = temp.layout();
        std::fs::create_dir_all(&layout.installed_tools).unwrap();
        std::fs::create_dir_all(&layout.official_tools).unwrap();
        let zip = write_zip(
            &temp.0,
            "bad-id.zip",
            &[
                ("tool.json", manifest_json("Bad Id!").as_bytes()),
                ("index.html", b"<h1>x</h1>"),
            ],
        );

        let staging = layout.staging_base.join("install-bad-id");
        std::fs::create_dir_all(&staging).unwrap();
        let error = inspect_at(&zip, &staging, &layout).unwrap_err();
        assert!(error.contains("id 非法"), "unexpected error: {error}");
    }

    #[test]
    fn inspect_rejects_missing_entry_file() {
        let temp = TempDir::new("no-entry");
        let layout = temp.layout();
        std::fs::create_dir_all(&layout.installed_tools).unwrap();
        std::fs::create_dir_all(&layout.official_tools).unwrap();
        let manifest = json!({
            "id": "demo-tool",
            "name": "演示工具",
            "entry": "missing.html"
        })
        .to_string();
        let zip = write_zip(
            &temp.0,
            "no-entry.zip",
            &[("tool.json", manifest.as_bytes()), ("index.html", b"x")],
        );

        let staging = layout.staging_base.join("install-no-entry");
        std::fs::create_dir_all(&staging).unwrap();
        let error = inspect_at(&zip, &staging, &layout).unwrap_err();
        assert!(error.contains("入口文件不存在"), "unexpected error: {error}");
    }

    #[test]
    fn inspect_rejects_path_traversal_entries() {
        let temp = TempDir::new("traversal");
        let layout = temp.layout();
        std::fs::create_dir_all(&layout.installed_tools).unwrap();
        std::fs::create_dir_all(&layout.official_tools).unwrap();
        let zip = write_zip(
            &temp.0,
            "evil.zip",
            &[
                ("tool.json", manifest_json("demo-tool").as_bytes()),
                ("../escape.txt", b"pwned"),
            ],
        );

        let staging = layout.staging_base.join("install-evil");
        std::fs::create_dir_all(&staging).unwrap();
        let error = extract_zip(&zip, &staging).unwrap_err();
        assert!(error.contains("非法路径"), "unexpected error: {error}");
        assert!(!temp.0.join("escape.txt").exists());
    }

    #[test]
    fn inspect_accepts_zip_with_single_wrapper_dir() {
        let temp = TempDir::new("wrapper");
        let layout = temp.layout();
        std::fs::create_dir_all(&layout.installed_tools).unwrap();
        std::fs::create_dir_all(&layout.official_tools).unwrap();
        let zip = write_zip(
            &temp.0,
            "wrapped.zip",
            &[
                ("demo-tool/tool.json", manifest_json("demo-tool").as_bytes()),
                ("demo-tool/index.html", b"<h1>wrapped</h1>"),
            ],
        );

        let staging = layout.staging_base.join("install-wrapped");
        std::fs::create_dir_all(&staging).unwrap();
        let (inspection, root) = inspect_at(&zip, &staging, &layout).unwrap();
        assert_eq!(inspection.id, "demo-tool");
        assert!(root.ends_with("demo-tool"));
        assert_eq!(inspection.file_count, 2);
    }

    #[test]
    fn granted_permissions_are_clamped_to_the_manifest() {
        let declared = Permissions {
            files_open: true,
            files_save: true,
            clipboard_write: true,
            data: true,
            network: vec!["api.example.com".into()],
            ..Default::default()
        };

        let granted = granted_permissions(
            &declared,
            &json!(["files.open", "clipboard.write", "network", "data"]),
        )
        .unwrap();
        assert!(granted.files_open);
        assert!(!granted.files_save, "unchecked permissions must stay off");
        assert!(granted.clipboard_write);
        assert!(granted.data);
        assert_eq!(granted.network, ["api.example.com"]);

        // Object form, with camelCase keys, and a permission the manifest does
        // not declare: the manifest stays the ceiling.
        let granted = granted_permissions(
            &declared,
            &json!({ "filesSave": true, "externalOpen": true, "clipboardRead": false }),
        )
        .unwrap();
        assert!(granted.files_save);
        assert!(!granted.files_open);
        assert!(!granted.external_open);

        // Empty selection installs the tool without any capability.
        let granted = granted_permissions(&declared, &json!([])).unwrap();
        assert!(!granted.any());
    }

    #[test]
    fn granted_permissions_reject_unknown_keys() {
        let declared = Permissions::default();
        let error = granted_permissions(&declared, &json!(["root.everything"])).unwrap_err();
        assert!(error.contains("未知的权限项"), "unexpected error: {error}");
    }

    #[test]
    fn commit_install_copies_files_and_persists_granted_permissions() {
        let temp = TempDir::new("commit");
        let layout = temp.layout();
        std::fs::create_dir_all(&layout.installed_tools).unwrap();
        std::fs::create_dir_all(&layout.official_tools).unwrap();
        let zip = sample_zip(&temp.0);
        let staging = layout.staging_base.join("install-commit");
        std::fs::create_dir_all(&staging).unwrap();
        let (inspection, root) = inspect_at(&zip, &staging, &layout).unwrap();

        let item = PendingInstall {
            inspection_id: inspection.inspection_id.clone(),
            zip_path: zip.clone(),
            staging: staging.clone(),
            root,
            inspection,
        };
        let result = commit_install(&layout, &item, &json!(["files.open", "data"])).unwrap();
        assert!(result.ok);
        assert_eq!(result.id, "demo-tool");
        assert_eq!(result.name, "演示工具");

        let installed = layout.installed_tools.join("demo-tool");
        assert!(installed.join("tool.json").is_file());
        assert!(installed.join("index.html").is_file());
        assert!(installed.join("app.js").is_file());

        let states = load_tools_at(&layout.tools_json);
        let state = states.get("demo-tool").expect("tool state written");
        assert!(state.enabled);
        assert_eq!(state.source, "installed");
        let permissions = state.permissions.as_ref().expect("granted permissions stored");
        assert!(permissions.files_open);
        assert!(permissions.data);
        assert!(!permissions.files_save);
        assert!(!permissions.external_open);
        assert!(permissions.network.is_empty());

        // tools.json keeps the documented shape and omits the field when unset.
        let raw = std::fs::read_to_string(&layout.tools_json).unwrap();
        assert!(raw.contains("\"permissions\""));
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            parsed["demo-tool"]["permissions"]["filesOpen"],
            serde_json::Value::Bool(true)
        );
        assert_eq!(
            parsed["demo-tool"]["permissions"]["network"],
            serde_json::Value::Array(vec![])
        );
    }

    #[test]
    fn commit_install_upgrade_keeps_a_backup_and_old_state() {
        let temp = TempDir::new("commit-upgrade");
        let layout = temp.layout();
        std::fs::create_dir_all(&layout.installed_tools).unwrap();
        std::fs::create_dir_all(&layout.official_tools).unwrap();

        // First install.
        let zip = sample_zip(&temp.0);
        let staging = layout.staging_base.join("install-v1");
        std::fs::create_dir_all(&staging).unwrap();
        let (inspection, root) = inspect_at(&zip, &staging, &layout).unwrap();
        let item = PendingInstall {
            inspection_id: inspection.inspection_id.clone(),
            zip_path: zip.clone(),
            staging,
            root,
            inspection,
        };
        commit_install(&layout, &item, &json!(["files.open"])).unwrap();

        // User disabled the tool, then upgrades it.
        let mut states = load_tools_at(&layout.tools_json);
        states.get_mut("demo-tool").unwrap().enabled = false;
        state::write_json(&layout.tools_json, &states).unwrap();

        let staging = layout.staging_base.join("install-v2");
        std::fs::create_dir_all(&staging).unwrap();
        let (inspection, root) = inspect_at(&zip, &staging, &layout).unwrap();
        assert!(inspection.is_upgrade);
        let item = PendingInstall {
            inspection_id: inspection.inspection_id.clone(),
            zip_path: zip.clone(),
            staging,
            root,
            inspection,
        };
        commit_install(&layout, &item, &json!(["data"])).unwrap();

        let backups = std::fs::read_dir(&layout.backups)
            .unwrap()
            .flatten()
            .count();
        assert_eq!(backups, 1, "the previous version must be backed up");
        let states = load_tools_at(&layout.tools_json);
        let state = states.get("demo-tool").unwrap();
        assert!(!state.enabled, "upgrade keeps the enabled flag");
        let permissions = state.permissions.as_ref().unwrap();
        assert!(permissions.data);
        assert!(!permissions.files_open, "new grant replaces the old one");
    }

    #[test]
    fn inspect_alone_leaves_nothing_installed() {
        let temp = TempDir::new("cancel");
        let layout = temp.layout();
        std::fs::create_dir_all(&layout.installed_tools).unwrap();
        std::fs::create_dir_all(&layout.official_tools).unwrap();
        let zip = sample_zip(&temp.0);

        let staging = layout.staging_base.join("install-cancel");
        std::fs::create_dir_all(&staging).unwrap();
        let (inspection, _) = inspect_at(&zip, &staging, &layout).unwrap();
        assert!(inspection.ok);

        // Cancelling means: never call install_pending; the tool dir and
        // tools.json must not exist.
        assert!(!layout.installed_tools.join("demo-tool").exists());
        assert!(!layout.tools_json.exists());
        let _ = std::fs::remove_dir_all(&staging);
    }

    #[test]
    fn tool_state_without_permissions_stays_backwards_compatible() {
        let old = json!({
            "enabled": true,
            "installedAt": "2024-01-01T00:00:00Z",
            "source": "installed",
            "pinned": false
        });
        let state: ToolState = serde_json::from_value(old).unwrap();
        assert!(state.permissions.is_none());
        let serialized = serde_json::to_value(&state).unwrap();
        assert!(
            serialized.get("permissions").is_none(),
            "unset permissions must be omitted: {serialized}"
        );

        let with_permissions = ToolState {
            permissions: Some(Permissions {
                files_open: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        let serialized = serde_json::to_value(&with_permissions).unwrap();
        assert_eq!(
            serialized["permissions"]["filesOpen"],
            serde_json::Value::Bool(true)
        );
    }
}
