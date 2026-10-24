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
    // Same rule as every other window: always pass the resolved theme explicitly
    // (it stamps the shared WebView2 profile) and give the webview the shell
    // background so the first frame does not flash white.
    let theme = crate::env::resolved_theme();
    let win = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("main.html".into()))
        .title("Toolbox")
        .inner_size(1000.0, 700.0)
        .min_inner_size(820.0, 560.0)
        .decorations(false)
        .center()
        .theme(Some(theme.tauri_theme()))
        .background_color(theme.background())
        .data_directory(dir)
        .initialization_script(&crate::windows::panel::host_bootstrap())
        .build()
        .map_err(|e| e.to_string())?;
    let _ = win.set_focus();
    Ok(())
}
