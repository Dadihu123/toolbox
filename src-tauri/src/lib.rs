//! Toolbox host: app bootstrap, tray, single instance, plugin wiring.

pub mod actions;
pub mod bridge;
pub mod install;
pub mod ipc;
pub mod logging;
pub mod paths;
pub mod registry;
pub mod state;
pub mod windows;

use std::sync::OnceLock;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::AppHandle;

static APP: OnceLock<AppHandle> = OnceLock::new();

pub fn app_handle() -> AppHandle {
    APP.get().cloned().expect("app handle not initialised")
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            handle_second_instance(app, argv);
        }))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .register_uri_scheme_protocol("toolbox", |ctx, request| {
            bridge::handle_protocol(ctx, request)
        })
        .invoke_handler(tauri::generate_handler![
            ipc::get_state,
            ipc::open_tool,
            ipc::close_tool,
            ipc::set_tool_enabled,
            ipc::uninstall_tool,
            ipc::install_tool,
            ipc::inspect_tool_zip,
            ipc::set_hotkey,
            ipc::reset_hotkey,
            ipc::save_config,
            ipc::open_panel,
            ipc::open_main,
            ipc::close_window,
            ipc::quit_app,
            ipc::restart_app,
            ipc::reveal_path,
            ipc::minimize_window,
            ipc::get_log_tail,
            ipc::bridge_call,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            let _ = APP.set(handle.clone());

            let p = paths::init();
            logging::write(
                "info",
                "app",
                &format!(
                    "startup portable={} data={} official_tools={}",
                    p.portable,
                    p.data.display(),
                    p.official_tools.display()
                ),
            );

            build_tray(&handle)?;

            if let Err(e) = windows::ball::init(&handle) {
                logging::write("error", "ball", &format!("init failed: {e}"));
            }

            actions::apply_hotkeys(&handle);

            let args: Vec<String> = std::env::args().collect();
            apply_cli(&handle, &args, true);

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let label = window.label().to_string();
                if label == windows::panel::LABEL {
                    api.prevent_close();
                    windows::panel::hide(&app_handle());
                } else if label == windows::main::LABEL {
                    let cfg = state::load_config();
                    if cfg.close_to_tray {
                        api.prevent_close();
                        let _ = window.hide();
                    }
                }
            }
            if let tauri::WindowEvent::Destroyed = event {
                let label = window.label().to_string();
                if windows::tools::tool_id_from_label(&label).is_some() {
                    actions::apply_hotkeys(&app_handle());
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building toolbox")
        .run(|_app, event| {
            // Toolbox is a tray + floating-ball app: it must survive with zero
            // Tauri windows open. Without this, Tauri exits as soon as the last
            // webview closes (and immediately, since it starts with none).
            if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
                if code.is_none() {
                    api.prevent_exit();
                }
            }
        });
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let panel = MenuItem::with_id(app, "panel", "打开工具面板", true, None::<&str>)?;
    let main = MenuItem::with_id(app, "main", "主界面…", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&panel, &main, &sep, &quit])?;

    let mut builder = TrayIconBuilder::with_id("toolbox-tray")
        .tooltip("Toolbox")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "panel" => windows::panel::toggle(app),
            "main" => windows::main::open(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                windows::panel::toggle(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }
    builder.build(app)?;
    Ok(())
}

fn handle_second_instance(app: &AppHandle, argv: Vec<String>) {
    logging::write("info", "app", &format!("second instance: {argv:?}"));
    apply_cli(app, &argv, false);
}

fn apply_cli(app: &AppHandle, args: &[String], first_launch: bool) {
    let mut tool: Option<String> = None;
    let mut files: Vec<String> = Vec::new();
    // On the very first start we only bring up the ball: no webview exists until
    // the user actually asks for something. Re-launching the exe (which lands in
    // the single-instance handler) opens the panel instead.
    let mut want_panel = !first_launch;
    let mut want_main = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--tool" if i + 1 < args.len() => {
                tool = Some(args[i + 1].clone());
                i += 2;
            }
            "--open" if i + 1 < args.len() => {
                files.push(args[i + 1].clone());
                i += 2;
            }
            "--panel" => {
                want_panel = true;
                i += 1;
            }
            "--main" => {
                want_main = true;
                i += 1;
            }
            "--quiet" => {
                want_panel = false;
                i += 1;
            }
            other => {
                // Shell file association / drag onto the exe: bare file path.
                if !other.starts_with('-') && std::path::Path::new(other).is_file() {
                    files.push(other.to_string());
                }
                i += 1;
            }
        }
    }
    if want_main {
        windows::main::open(app);
        return;
    }
    // Without an explicit --tool, send the files to the first enabled tool that
    // declares it accepts this type (falling back to the last used tool).
    let mut target = tool;
    if target.is_none() && !files.is_empty() {
        target = windows::tools::tool_for_files(&files).or_else(|| {
            let cfg = state::load_config();
            if cfg.last_tool.is_empty() {
                None
            } else {
                Some(cfg.last_tool)
            }
        });
        if target.is_none() {
            logging::write(
                "warn",
                "cli",
                &format!("no tool accepts the passed file(s): {files:?}"),
            );
        }
    }
    if let Some(id) = target {
        if files.is_empty() {
            if let Err(e) = windows::tools::open(app, &id) {
                logging::write("error", "cli", &format!("open tool {id} failed: {e}"));
            }
        } else {
            windows::tools::open_files(app, &id, files);
        }
    } else if want_panel || !files.is_empty() {
        // Defer by one tick: creating a webview window while the event loop is
        // still starting up on Windows is unreliable.
        let handle = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(120));
            if let Err(e) = windows::panel::open(&handle) {
                logging::write("error", "cli", &format!("open panel failed: {e}"));
            }
        });
    }
}
