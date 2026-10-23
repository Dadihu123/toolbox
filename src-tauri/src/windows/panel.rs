//! The search panel: the main entry point, summoned with a hotkey.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use tauri::{AppHandle, LogicalPosition, Manager, WebviewUrl, WebviewWindowBuilder};

pub const LABEL: &str = "panel";

/// How long a hidden panel stays warm before its webview is destroyed.
///
/// Chromium costs hundreds of MB once it exists; keeping the panel alive for a few
/// minutes makes reopening instant, while the timeout gives the idle footprint
/// back. `TOOLBOX_PANEL_WARM_SECS` overrides it for dev/e2e runs.
fn warm_ttl() -> std::time::Duration {
    let secs = std::env::var("TOOLBOX_PANEL_WARM_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(300);
    std::time::Duration::from_secs(secs)
}

/// Bumped on every show/hide. A scheduled teardown only fires while its token is
/// still current — that is how reopening the panel cancels the destroy task.
static WARM_TOKEN: AtomicU64 = AtomicU64::new(0);

/// Wall-clock millisecond timestamp of the last time the panel was shown.
///
/// A blur event that arrives immediately after a show is not the user clicking
/// somewhere else: the webview can lose/regain focus while it initialises, and
/// the shell sometimes hands focus back to the window that was active before.
/// Closing on those events would make the panel flash open and disappear.
static LAST_SHOW_MS: AtomicU64 = AtomicU64::new(0);

/// Bumped whenever the panel window regains focus.
///
/// A pending blur-close re-reads this token: if it changed, the window got focus
/// back before the grace period expired and the blur was just WebView2 bouncing
/// focus between its document and the window, not the user leaving.
static FOCUS_TOKEN: AtomicU64 = AtomicU64::new(0);

/// True while a panel webview is being created. Two opens that race before the
/// first window exists must not both call the builder.
static CREATING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// RAII guard for [`CREATING`].
struct CreatingGuard;

impl CreatingGuard {
    fn acquire() -> Option<Self> {
        if CREATING.swap(true, Ordering::SeqCst) {
            None
        } else {
            Some(Self)
        }
    }
}

impl Drop for CreatingGuard {
    fn drop(&mut self) {
        CREATING.store(false, Ordering::SeqCst);
    }
}

/// Blur events this soon after a show are ignored (see [`LAST_SHOW_MS`]).
const BLUR_GRACE_MS: u64 = 250;

/// How long a blur has to last before it counts as "the user clicked outside".
///
/// Clicking anywhere *inside* the panel makes WebView2 blur and immediately
/// refocus its document (measured: 4–10 ms apart), and the same bounce happens
/// when the search input takes focus. Acting on the blur directly closed the
/// panel on every inside click — so "click outside closes it" waits this long
/// and gives up if the window got focus back in the meantime.
const BLUR_SETTLE_MS: u64 = 180;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn mark_shown() {
    LAST_SHOW_MS.store(now_ms(), Ordering::SeqCst);
}

fn mark_focused() {
    FOCUS_TOKEN.fetch_add(1, Ordering::SeqCst);
}

fn focus_token() -> u64 {
    FOCUS_TOKEN.load(Ordering::SeqCst)
}

fn shown_recently() -> bool {
    let last = LAST_SHOW_MS.load(Ordering::SeqCst);
    now_ms().saturating_sub(last) < BLUR_GRACE_MS
}

fn cancel_warm_destroy() {
    WARM_TOKEN.fetch_add(1, Ordering::SeqCst);
}

/// Hide the panel (keep it warm) and schedule its teardown after `warm_ttl()`.
pub fn hide(app: &AppHandle) {
    if let Some(win) = app.get_webview_window(LABEL) {
        let _ = win.hide();
    }
    crate::windows::ball::collapse();
    schedule_destroy(app);
}

fn schedule_destroy(app: &AppHandle) {
    let token = WARM_TOKEN.fetch_add(1, Ordering::SeqCst) + 1;
    let ttl = warm_ttl();
    let handle = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(ttl);
        if WARM_TOKEN.load(Ordering::SeqCst) != token {
            return;
        }
        defer_main(&handle, move |h| {
            if WARM_TOKEN.load(Ordering::SeqCst) != token {
                return;
            }
            if let Some(win) = h.get_webview_window(LABEL) {
                if win.is_visible().unwrap_or(false) {
                    return;
                }
                let _ = win.destroy();
                crate::logging::write("info", "panel", "warm window released after idle timeout");
            }
        });
    });
}

/// Injected into every host window: a tiny event bus that the Rust side
/// dispatches into with `webview.eval`. Avoids depending on the event plugin
/// (tool windows deliberately have no plugin permissions at all).
pub const HOST_BOOTSTRAP: &str = r#"(function(){
  window.__hostBus = Object.create(null);
  window.__hostEmit = function (type, payload) {
    var cbs = window.__hostBus[type];
    if (!cbs) return;
    for (var i = 0; i < cbs.length; i++) { try { cbs[i](payload); } catch (e) { console.error(e); } }
  };
  window.__hostOn = function (type, cb) {
    (window.__hostBus[type] || (window.__hostBus[type] = [])).push(cb);
  };
})();"#;

/// Create the panel on the main thread, one event-loop tick later.
///
/// Two Windows constraints force this indirection:
///   * a WebView2 window must be created on the thread that owns the event loop
///     (creating it from a global-shortcut callback thread fails with os error 5)
///   * creating it *during* `setup` is equally unreliable, so we hop a tick.
pub fn defer<F: FnOnce() + Send + 'static>(f: F) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(60));
        f();
    });
}

pub fn defer_main<F: FnOnce(AppHandle) + Send + 'static>(app: &AppHandle, f: F) {
    let handle = app.clone();
    let handle2 = handle.clone();
    defer(move || {
        if let Err(e) = handle2.run_on_main_thread(move || f(handle)) {
            crate::logging::write("error", "panel", &format!("run_on_main_thread failed: {e}"));
        }
    });
}

pub fn toggle(app: &AppHandle) {
    crate::logging::write("info", "panel", "toggle requested");
    if let Some(win) = app.get_webview_window(LABEL) {
        if win.is_visible().unwrap_or(false) {
            hide(app);
            return;
        }
        cancel_warm_destroy();
        // A warm panel keeps the position it was created with. The ball may have
        // been dragged elsewhere since, so re-anchor before making it visible —
        // moving a hidden window avoids a visible jump at the old spot.
        let (x, y) = anchor(app);
        let _ = win.set_position(LogicalPosition::new(x, y));
        let _ = win.show();
        let _ = win.set_focus();
        mark_shown();
        crate::windows::ball::expand();
        return;
    }
    defer_main(app, |handle| {
        match open(&handle) {
            Ok(()) => crate::logging::write("info", "panel", "opened"),
            Err(e) => crate::logging::write("error", "panel", &format!("open failed: {e}")),
        }
    });
}

/// Shared WebView2 profile directory. Keeping it under `data/` is what makes the
/// portable promise real: Chromium would otherwise write caches, localStorage and
/// GPU shader blobs to `%LOCALAPPDATA%\<exe>.WebView2`.
fn webview_data_dir() -> PathBuf {
    let dir = crate::paths::get().webview_data.clone();
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn open(app: &AppHandle) -> Result<(), String> {
    // A warm panel (hidden but alive) already owns the `panel` label. Reuse it:
    // rapid clicks on the ball used to queue several deferred opens, and every
    // one after the first failed with "a webview with label `panel` already
    // exists".
    if let Some(win) = app.get_webview_window(LABEL) {
        cancel_warm_destroy();
        let (x, y) = anchor(app);
        let _ = win.set_position(LogicalPosition::new(x, y));
        let _ = win.show();
        let _ = win.set_focus();
        mark_shown();
        crate::windows::ball::expand();
        return Ok(());
    }

    // Belt and braces for two opens racing before the first window exists.
    let Some(_creating) = CreatingGuard::acquire() else {
        return Ok(());
    };

    let (x, y) = anchor(app);
    let win = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("panel.html".into()))
        .title("Toolbox Panel")
        .inner_size(460.0, 560.0)
        .position(x, y)
        .decorations(false)
        .transparent(true)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .shadow(false)
        .visible(false)
        .data_directory(webview_data_dir())
        .initialization_script(HOST_BOOTSTRAP)
        .build()
        .map_err(|e| e.to_string())?;

    // Click-outside-to-close: the panel is a transient popup, so losing focus to
    // another window means the user moved on. Go through hide() so the ball
    // collapses and the warm-keep timer starts exactly like every other close
    // path. Three guards stop this from fighting the show sequence:
    //   * never act on a window that is already hidden — hiding the panel makes
    //     it lose focus too, which would otherwise re-enter hide();
    //   * ignore blur inside the grace window right after a show (see
    //     LAST_SHOW_MS);
    //   * wait BLUR_SETTLE_MS and re-check the focus token, because WebView2
    //     bounces focus on every click inside the panel (see BLUR_SETTLE_MS).
    let handle = app.clone();
    win.on_window_event(move |event| {
        match event {
            tauri::WindowEvent::Focused(true) => mark_focused(),
            tauri::WindowEvent::Focused(false) => {
                if let Some(w) = handle.get_webview_window(LABEL) {
                    if !w.is_visible().unwrap_or(false) || shown_recently() {
                        return;
                    }
                }
                let token = focus_token();
                let handle = handle.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(BLUR_SETTLE_MS));
                    // Focus came back: the blur was WebView2 handing focus
                    // between the window and its document, not the user leaving.
                    if focus_token() != token {
                        return;
                    }
                    let Some(w) = handle.get_webview_window(LABEL) else {
                        return;
                    };
                    if !w.is_visible().unwrap_or(false) {
                        return;
                    }
                    // A window that still owns the foreground after settling was
                    // never really left (e.g. a click on its own non-client area).
                    crate::logging::write("info", "panel", "hidden because the window lost focus");
                    hide(&handle);
                });
            }
            _ => {}
        }
    });

    let _ = win.show();
    let _ = win.set_focus();
    mark_shown();
    cancel_warm_destroy();
    Ok(())
}

/// Place the panel next to the ball, clamped to the work area.
///
/// `ball_rect()` comes from Win32 and is in **physical** pixels, while
/// `WebviewWindowBuilder::position()` takes **logical** ones. Mixing them up
/// silently pushes the window off screen on any scaled display, so the
/// conversion happens here and nowhere else.
fn anchor(app: &AppHandle) -> (f64, f64) {
    const PANEL_W: f64 = 460.0;
    const PANEL_H: f64 = 560.0;
    const GAP: f64 = 12.0;

    let scale = app
        .primary_monitor()
        .ok()
        .flatten()
        .map(|m| m.scale_factor())
        .unwrap_or(1.0);

    let mut x = 100.0;
    let mut y = 100.0;

    if let Some((bx, by, br, _bb, side_right)) = crate::windows::ball::ball_rect() {
        // physical -> logical
        let (bx, by, br) = (bx as f64 / scale, by as f64 / scale, br as f64 / scale);
        x = if side_right {
            bx - PANEL_W - GAP
        } else {
            br + GAP
        };
        // align the panel's top with the ball's top, nudged up so the search box
        // lands under the cursor
        y = by - 40.0;
    }

    if let Some(mon) = app.primary_monitor().ok().flatten() {
        let size = mon.size();
        let w = size.width as f64 / scale;
        let h = size.height as f64 / scale;
        x = x.clamp(8.0, (w - PANEL_W - 8.0).max(8.0));
        y = y.clamp(8.0, (h - PANEL_H - 8.0).max(8.0));
    }
    (x, y)
}
