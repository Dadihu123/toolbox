//! Minimal file logging. One file per day under `<data>/logs`, 7 day retention.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use crate::paths;

fn log_dir() -> PathBuf {
    let dir = paths::get().logs.clone();
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn today_file() -> PathBuf {
    let iso = crate::registry::now_iso();
    let day = &iso[..10];
    log_dir().join(format!("toolbox-{day}.log"))
}

pub fn write(level: &str, scope: &str, message: &str) {
    let stamp = crate::registry::now_iso();
    let line = format!("{stamp} [{level:<5}] [{scope}] {message}\n");
    if level == "error" {
        eprintln!("{line}");
    }
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(today_file())
    {
        let _ = f.write_all(line.as_bytes());
    }
    prune();
}

pub fn tool_log(tool_id: &str, level: &str, message: &str) {
    write(level, &format!("tool:{tool_id}"), message);
}

pub fn tail(lines: usize) -> Vec<String> {
    let path = today_file();
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(lines);
    all[start..].iter().map(|s| s.to_string()).collect()
}

fn prune() {
    let Ok(entries) = std::fs::read_dir(log_dir()) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("log") {
                return None;
            }
            let t = e.metadata().ok()?.modified().ok()?;
            Some((t, p))
        })
        .collect();
    if files.len() <= 7 {
        return;
    }
    files.sort_by_key(|(t, _)| *t);
    for (_, p) in files.iter().take(files.len() - 7) {
        let _ = std::fs::remove_file(p);
    }
}
