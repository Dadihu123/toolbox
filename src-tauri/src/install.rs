//! Installing and uninstalling third party tools (zip based).

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::paths;
use crate::registry;
use crate::state::{self, ToolState};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallResult {
    pub ok: bool,
    pub id: String,
    pub name: String,
    pub error: String,
}

const MAX_ENTRIES: usize = 5_000;
const MAX_BYTES: u64 = 200 * 1024 * 1024;

/// Install from a zip file the user picked. Returns the installed tool id.
pub fn install_from_zip(zip_path: &Path) -> Result<InstallResult, String> {
    let p = paths::get();
    let staging = p.data.join("tmp").join(format!("install-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;

    let result = (|| -> Result<String, String> {
        extract_zip(zip_path, &staging)?;
        // A zip may either contain tool.json at the root or a single wrapper dir.
        let root = find_manifest_root(&staging).ok_or_else(|| "压缩包内未找到 tool.json".to_string())?;
        let text = std::fs::read_to_string(root.join("tool.json")).map_err(|e| e.to_string())?;
        let manifest: registry::Manifest =
            serde_json::from_str(&text).map_err(|e| format!("tool.json 解析失败：{e}"))?;

        let id = if manifest.id.trim().is_empty() {
            root.file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default()
        } else {
            manifest.id.clone()
        };
        if id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        {
            return Err("工具 id 非法（只允许小写字母、数字、- 和 _）".into());
        }
        let entry = if manifest.entry.trim().is_empty() {
            "index.html".to_string()
        } else {
            manifest.entry.clone()
        };
        if !root.join(&entry).is_file() {
            return Err(format!("入口文件不存在：{entry}"));
        }

        // Never overwrite a bundled tool: official ones are read-only.
        if p.official_tools.join(&id).exists() {
            return Err(format!("「{id}」是内置工具，不能覆盖"));
        }

        let dest = p.installed_tools.join(&id);
        if dest.exists() {
            let backup = p
                .backups
                .join("tools")
                .join(format!("{id}-{}", crate::registry::now_iso().replace(':', "-")));
            let _ = std::fs::create_dir_all(backup.parent().unwrap_or(&backup));
            let _ = std::fs::rename(&dest, &backup);
        }
        std::fs::create_dir_all(&p.installed_tools).map_err(|e| e.to_string())?;
        copy_dir(&root, &dest)?;
        Ok(id)
    })();

    let _ = std::fs::remove_dir_all(&staging);

    match result {
        Ok(id) => {
            let mut states = state::load_tools();
            states.insert(
                id.clone(),
                ToolState {
                    enabled: true,
                    installed_at: registry::now_iso(),
                    source: "installed".into(),
                    pinned: false,
                },
            );
            let _ = state::save_tools(&states);
            let name = registry::find(&id).map(|m| m.name).unwrap_or_else(|| id.clone());
            logging_install(&id);
            Ok(InstallResult { ok: true, id, name, error: String::new() })
        }
        Err(e) => Err(e),
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
            std::fs::write(&out, buf).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
