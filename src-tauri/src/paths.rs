//! Portable path resolution.
//!
//! Everything the app writes goes through here. The rule is simple:
//! the data directory lives next to the executable unless the user says
//! otherwise, or unless that location is not writable.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Debug, Clone)]
pub struct AppPaths {
    /// Directory that contains the executable (or the dev scratch dir).
    pub root: PathBuf,
    /// `<root>/data` normally, `%LOCALAPPDATA%\Toolbox\data` as a fallback.
    pub data: PathBuf,
    /// Bundled / official tools: `<root>/tools`.
    pub official_tools: PathBuf,
    /// User installed tools: `<data>/tools`.
    pub installed_tools: PathBuf,
    /// Per-tool private storage: `<data>/tools/<id>/store`.
    pub tool_store: PathBuf,
    /// WebView2 user data folder (keeps Chromium caches out of AppData).
    pub webview_data: PathBuf,
    /// Download sink when the save dialog is disabled.
    pub downloads: PathBuf,
    pub logs: PathBuf,
    pub backups: PathBuf,
    /// False when we had to fall back to %LOCALAPPDATA%.
    pub portable: bool,
}

static PATHS: OnceLock<AppPaths> = OnceLock::new();

pub fn init() -> &'static AppPaths {
    PATHS.get_or_init(resolve)
}

pub fn get() -> &'static AppPaths {
    PATHS.get_or_init(resolve)
}

fn exe_root() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

fn is_writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".write-probe");
    let ok = std::fs::write(&probe, b"ok").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

fn resolve() -> AppPaths {
    let root = exe_root();

    // 1) explicit --data-dir
    // 2) TOOLBOX_HOME
    // 3) <exe>/data   (portable default)
    // 4) %LOCALAPPDATA%\Toolbox\data
    let mut portable = true;
    let data = if let Some(dir) = arg_value("--data-dir") {
        portable = false;
        PathBuf::from(dir)
    } else if let Ok(home) = std::env::var("TOOLBOX_HOME") {
        if !home.trim().is_empty() {
            portable = false;
            PathBuf::from(home)
        } else {
            portable_candidate(&root, &mut portable)
        }
    } else {
        portable_candidate(&root, &mut portable)
    };

    let paths = AppPaths {
        official_tools: root.join("tools"),
        installed_tools: data.join("tools"),
        tool_store: data.join("tool-data"),
        webview_data: data.join("webview2"),
        downloads: data.join("downloads"),
        logs: data.join("logs"),
        backups: data.join("backups"),
        root,
        data,
        portable,
    };

    for d in [
        &paths.data,
        &paths.installed_tools,
        &paths.tool_store,
        &paths.webview_data,
        &paths.downloads,
        &paths.logs,
        &paths.backups,
    ] {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::create_dir_all(&paths.official_tools);

    paths
}

fn portable_candidate(root: &Path, portable: &mut bool) -> PathBuf {
    let candidate = root.join("data");
    if is_writable(&candidate) {
        candidate
    } else {
        *portable = false;
        local_app_data().join("Toolbox").join("data")
    }
}

fn local_app_data() -> PathBuf {
    std::env::var("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir())
}

fn arg_value(flag: &str) -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == flag {
            return args.next();
        }
        if let Some(rest) = a.strip_prefix(&format!("{flag}=")) {
            return Some(rest.to_string());
        }
    }
    None
}

/// Ensure a path stays inside `base`. Returns the canonical target on success.
pub fn ensure_inside(base: &Path, candidate: &Path) -> Option<PathBuf> {
    let base = base.canonicalize().ok()?;
    let target = candidate.canonicalize().ok()?;
    if target.starts_with(&base) {
        Some(target)
    } else {
        None
    }
}
