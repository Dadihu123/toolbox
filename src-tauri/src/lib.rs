//! Toolbox host: app bootstrap, tray, single instance, plugin wiring.

pub mod actions;
pub mod bridge;
pub mod env;
pub mod icons;
pub mod install;
pub mod ipc;
pub mod l10n;
pub mod logging;
pub mod paths;
pub mod registry;
pub mod state;
pub mod windows;

// Native word lists: embedded at compile time from src-tauri/locales/*.json.
// zh-CN is the source language and the fallback (see l10n.rs).
rust_i18n::i18n!("locales", fallback = "zh-CN");

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Wry};

/// Tray icon id; also how [`apply_locale`] finds the icon to swap its menu.
const TRAY_ID: &str = "toolbox";

static APP: OnceLock<AppHandle> = OnceLock::new();

/// The live tray icon. Tauri keeps it registered on its own, but holding the
/// handle here documents the lifetime and leaves room for icon refreshes.
static TRAY: OnceLock<Mutex<Option<TrayIcon<Wry>>>> = OnceLock::new();

/// Whether the shell actually put the icon into the notification area.
static TRAY_VISIBLE: AtomicBool = AtomicBool::new(false);

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

            // Resolve theme/locale once at startup so the log records what every
            // window will be created with. Windows are created lazily and each one
            // asks the environment service again, which keeps them consistent even
            // if the user switches the theme while they are open.
            let env_snapshot = env::snapshot();
            logging::write(
                "info",
                "env",
                &format!(
                    "resolved theme={} locale={} | system theme={} locale={} tag={:?} high_contrast={}",
                    env_snapshot.theme,
                    env_snapshot.locale,
                    env_snapshot.system_theme,
                    env_snapshot.system_locale,
                    env_snapshot.system_language_tag,
                    env_snapshot.high_contrast
                ),
            );

            // The native word list must be selected before anything is built from
            // it — the tray menu below is the first consumer.
            l10n::set_locale(env::resolved_locale());

            // The tray is a convenience, never a hard requirement: failing to add
            // the icon must not stop the app from starting (the floating ball and
            // the global hotkeys keep every feature reachable).
            if let Err(e) = build_tray(&handle) {
                logging::write("warn", "tray", &format!("tray unavailable: {e}"));
            }

            if let Err(e) = windows::ball::init(&handle) {
                logging::write("error", "ball", &format!("init failed: {e}"));
            }

            // The hover quick-launch fan (native layered window, own thread).
            // Optional: a failure must not keep the app from starting.
            if let Err(e) = windows::quickmenu::init(&handle) {
                logging::write("warn", "quickmenu", &format!("init failed: {e}"));
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
    let menu = tray_menu(app)?;
    build_tray_icon(app, menu)
}

fn tray_menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    // Menu labels come from the native word list (`rust_i18n::t!`), so this must
    // run after `l10n::set_locale` — see `apply_locale`.
    let panel = MenuItem::with_id(
        app,
        "panel",
        rust_i18n::t!("native.menu.panel").to_string(),
        true,
        None::<&str>,
    )?;
    let main = MenuItem::with_id(
        app,
        "main",
        rust_i18n::t!("native.menu.main").to_string(),
        true,
        None::<&str>,
    )?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(
        app,
        "quit",
        rust_i18n::t!("native.menu.quit").to_string(),
        true,
        None::<&str>,
    )?;
    Menu::with_items(app, &[&panel, &main, &sep, &quit])
}

/// Select the native word list and refresh the strings that were already built
/// from it (today: the tray menu).
///
/// Called once at startup — before the tray exists, so the first menu is already
/// translated — and again whenever the language setting changes.
pub fn apply_locale(app: &AppHandle) {
    let locale = env::resolved_locale();
    l10n::set_locale(locale);
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        match tray_menu(app) {
            Ok(menu) => {
                if let Err(e) = tray.set_menu(Some(menu)) {
                    logging::write("warn", "i18n", &format!("tray menu update failed: {e}"));
                }
            }
            Err(e) => logging::write("warn", "i18n", &format!("tray menu rebuild failed: {e}")),
        }
    }
    logging::write("info", "i18n", &format!("native locale -> {locale}"));
}

/// Builds the tray icon exactly once.
///
/// Historical note: this used to retry while `TrayIcon::rect()` reported no rect.
/// That probe is not a registration test — a locked-down / virtualised session
/// refuses `Shell_NotifyIcon(NIM_ADD)` with `ERROR_ACCESS_DENIED` while the
/// `tray-icon` crate still reports the builder as successful, so each retry just
/// built another icon (up to six duplicates whenever the shell *did* accept them).
/// The log line below carries the diagnosis; the app never waits on the tray.
fn build_tray_icon(app: &AppHandle, menu: Menu<Wry>) -> tauri::Result<()> {
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
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

    match tray_icon_image() {
        Some(icon) => builder = builder.icon(icon),
        None => logging::write("warn", "tray", "no icon available for the tray"),
    }

    let tray = builder.build(app)?;

    // `TrayIcon::rect()` is only a hint: the shell reports no rect for an icon
    // that sits in the overflow area and reports none at all when it refuses the
    // registration. Log it, never treat it as the success criterion.
    #[cfg(target_os = "windows")]
    let location = match tray.rect() {
        Ok(Some(rect)) => format!("at {:?}", rect.position),
        Ok(None) => "in the notification area (overflow)".to_string(),
        Err(_) => "in the notification area".to_string(),
    };
    #[cfg(not(target_os = "windows"))]
    let location = "in the tray".to_string();
    TRAY_VISIBLE.store(true, Ordering::SeqCst);
    logging::write("info", "tray", &format!("tray icon created {location}"));

    let slot = TRAY.get_or_init(|| Mutex::new(None));
    if let Ok(mut guard) = slot.lock() {
        *guard = Some(tray);
    }
    Ok(())
}

/// True once the shell accepted a tray icon.
pub fn tray_available() -> bool {
    TRAY_VISIBLE.load(Ordering::SeqCst)
}

/// Icon handed to the shell: the 32px asset keeps the small tray slot sharp;
/// the decoded app icon is only a fallback.
fn tray_icon_image() -> Option<tauri::image::Image<'static>> {
    tauri::image::Image::from_bytes(include_bytes!("../icons/32x32.png"))
        .map_err(|e| logging::write("warn", "tray", &format!("tray png decode failed: {e}")))
        .ok()
        .or_else(|| tauri::image::Image::from_bytes(include_bytes!("../icons/icon.png")).ok())
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
