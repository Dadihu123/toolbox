//! Main window: tool management, hotkeys, settings, official tool market.

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

pub const LABEL: &str = "main";

pub fn open(app: &AppHandle) {
    if let Some(win) = app.get_webview_window(LABEL) {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
        return;
    }
    crate::windows::panel::defer_main(app, |handle| {
        if let Err(e) = build(&handle) {
            crate::logging::write("error", "main", &format!("open failed: {e}"));
        }
    });
}

fn build(app: &AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window(LABEL) {
        let _ = win.show();
        let _ = win.set_focus();
        return Ok(());
    }
    let dir = crate::paths::get().webview_data.clone();
    let _ = std::fs::create_dir_all(&dir);
    let win = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("main.html".into()))
        .title("Toolbox")
        .inner_size(1000.0, 700.0)
        .min_inner_size(820.0, 560.0)
        .decorations(false)
        .center()
        .data_directory(dir)
        .initialization_script(crate::windows::panel::HOST_BOOTSTRAP)
        .build()
        .map_err(|e| e.to_string())?;
    let _ = win.set_focus();
    Ok(())
}
