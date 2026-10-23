//! Native Win32 floating ball.
//!
//! Why native instead of a WebView: the ball is always resident. A resident
//! WebView would cost ~50 MB plus a renderer process; a layered popup costs a
//! few MB and, when idle, literally zero CPU (no timers, no animation loop).
//!
//! Rendering is a small software rasterizer into a 32bpp DIB (4x4 supersampling)
//! pushed with `UpdateLayeredWindow`, so the visual does not depend on GDI+
//! availability and every pixel is predictable.
//!
//! Behaviour:
//!   * docked to the right (or left) edge, vertical position from config
//!   * "peek" mode: only a sliver visible; hover slides it out
//!   * left click -> toggle the panel; drag -> move, and *only* a release within
//!     `SNAP_DISTANCE` px of a side edge docks (peek); otherwise the ball keeps
//!     exactly the position it was dropped at, fully inside the work area
//!   * right click -> native context menu

#![cfg(windows)]

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::OnceLock;

use tauri::AppHandle;
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, EndPaint, GetDC,
    GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, ReleaseDC, SelectObject, BITMAPINFO,
    BITMAPINFOHEADER, BLENDFUNCTION, BI_RGB, DIB_RGB_COLORS, HGDIOBJ, HMONITOR, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::state;

const BALL_SIZE: i32 = 64;
const PEEK_VISIBLE: i32 = 14;
const PADDING: f32 = 9.0;
/// A drag that ends within this many physical pixels of the left/right work-area
/// edge docks the ball there (docs/design.md 4.1: 距边 <24px 吸附). Anything
/// farther away stays free-floating at the drop position.
const SNAP_DISTANCE: i32 = 24;
const TIMER_SLIDE: usize = 1;
/// Delayed collapse: the pointer may briefly leave the window while it is
/// sliding, or while crossing the transparent margin next to the screen edge.
/// Collapsing only after the pointer stayed away for a moment is what stops the
/// old expand/collapse feedback loop.
const TIMER_COLLAPSE: usize = 2;
const COLLAPSE_DELAY_MS: u32 = 220;
const DRAG_THRESHOLD: i32 = 4;
/// While the hover slide animates the window, Windows synthesises mouse clicks
/// on the no-activate layered window as it passes under the stationary pointer.
/// Those must not open the panel; this is how long they are ignored after a
/// slide starts (the animation itself is ~10 timer ticks at 12 ms).
const SLIDE_IGNORE_CLICKS_MS: u64 = 320;

/// Clicks are ignored until this wall-clock millisecond (see [`SLIDE_IGNORE_CLICKS_MS`]).
static IGNORE_CLICKS_UNTIL: AtomicU64 = AtomicU64::new(0);

/// Private message used to drive the window from other threads (the tray, the
/// panel or an IPC handler) without touching the window's thread-local state.
const WM_BALL_CMD: u32 = 0x8000 + 1;
const CMD_EXPAND: usize = 1;
const CMD_COLLAPSE: usize = 2;
const CMD_REPOSITION: usize = 3;
const CMD_TOGGLE_VISIBLE: usize = 4;

/// The ball window handle, for code running off the window thread.
static BALL_HWND: AtomicIsize = AtomicIsize::new(0);

/// Wall-clock milliseconds; the click-ignore window needs a shared clock.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Are clicks currently suppressed because the ball is sliding?
fn clicks_ignored() -> bool {
    now_ms() < IGNORE_CLICKS_UNTIL.load(Ordering::SeqCst)
}

fn ball_hwnd() -> Option<HWND> {
    let raw = BALL_HWND.load(Ordering::SeqCst);
    if raw == 0 {
        None
    } else {
        Some(HWND(raw as *mut _))
    }
}

fn run_ball_cmd(cmd: usize) {
    if let Some(hwnd) = ball_hwnd() {
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_BALL_CMD, WPARAM(cmd), LPARAM(0));
        }
    }
}

/// `WM_MOUSELEAVE` is not re-exported by the windows crate, so define it here.
/// (Using an unknown identifier directly in a `match` arm would silently become
/// a catch-all binding and swallow every message below it.)
const MSG_MOUSELEAVE: u32 = 0x02A3;

thread_local! {
    static BALL: RefCell<Option<Ball>> = const { RefCell::new(None) };
}

/// Cross-thread snapshot of the ball state.
///
/// [`ball_rect`], [`dragging`] and [`anchor_kind`] are also called from threads
/// other than the ball's window thread (panel anchoring runs on the main thread,
/// the native quick menu on its own), where the thread-local `BALL` is invisible.
/// The window thread therefore mirrors the three fields those callers need into
/// atomics after every message it handles.
static SHARED_DOCKED: AtomicBool = AtomicBool::new(true);
static SHARED_SIDE_RIGHT: AtomicBool = AtomicBool::new(true);
static SHARED_DRAGGING: AtomicBool = AtomicBool::new(false);

fn publish_shared(ball: &Ball) {
    SHARED_DOCKED.store(ball.docked, Ordering::SeqCst);
    SHARED_SIDE_RIGHT.store(ball.side_right, Ordering::SeqCst);
    SHARED_DRAGGING.store(ball.dragging, Ordering::SeqCst);
}

/// Mirror the current thread-local ball into the cross-thread atomics.
/// No-op on threads that do not own the ball window (where `BALL` is `None`).
fn publish_current() {
    BALL.with(|b| {
        // `try_borrow`, not `borrow`: Win32 calls such as `SetWindowPos` may
        // re-enter this window procedure synchronously, and a nested message
        // handled while a handler still holds the mutable borrow must never
        // panic here — a panic inside a wndproc aborts the whole process.
        // The outer handler publishes the final state when it returns anyway.
        if let Ok(guard) = b.try_borrow() {
            if let Some(ball) = guard.as_ref() {
                publish_shared(ball);
            }
        }
    });
}

static APP: OnceLock<AppHandle> = OnceLock::new();

fn app() -> AppHandle {
    APP.get().cloned().expect("ball app handle")
}

struct Ball {
    expanded: bool,
    hovering: bool,
    pressed: bool,
    dragging: bool,
    drag_origin: POINT,
    win_origin: POINT,
    /// `true`: pinned to a screen edge (peek mode). `false`: free floating at
    /// the position the user dropped it at.
    docked: bool,
    side_right: bool,
    x_frac: f64,
    y_frac: f64,
    slide_to: i32,
    slide_step: i32,
}

impl Ball {
    fn geometry(&self) -> Geometry {
        Geometry {
            docked: self.docked,
            side_right: self.side_right,
            x_frac: self.x_frac,
            y_frac: self.y_frac,
        }
    }

    fn apply_geometry(&mut self, geometry: Geometry) {
        self.docked = geometry.docked;
        self.side_right = geometry.side_right;
        self.x_frac = geometry.x_frac.clamp(0.0, 1.0);
        self.y_frac = geometry.y_frac.clamp(0.0, 1.0);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Face {
    Idle,
    Hover,
}

pub fn init(handle: &AppHandle) -> Result<(), String> {
    let _ = APP.set(handle.clone());
    let cfg = state::load_config();
    // Materialise config.json on first start. Everything (ball position, quick
    // menu slots, hotkeys) reads defaults from code anyway, but a file on disk
    // makes the settings page editable straight away and documents the schema
    // for the user. Best effort: a read-only data dir must not stop the app.
    if !state::config_path().exists() {
        if let Err(e) = state::save_config(&cfg) {
            crate::logging::write("warn", "ball", &format!("could not write default config: {e}"));
        }
    }

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let hinstance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let class_name = w!("ToolboxFloatingBall");

        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            lpszClassName: class_name,
            hCursor: LoadCursorW(None, IDC_HAND).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);

        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            class_name,
            w!("Toolbox"),
            WS_POPUP,
            0,
            0,
            BALL_SIZE,
            BALL_SIZE,
            None,
            None,
            Some(hinstance.into()),
            None,
        )
        .map_err(|e| e.to_string())?;

        BALL_HWND.store(hwnd.0 as isize, Ordering::SeqCst);

        // Restart with whatever the user last did: a docked peek ball on
        // `ball_side`, or a free ball at `(ball_x, ball_y)`.
        let geometry = Geometry::from_config(&cfg);
        crate::logging::write(
            "info",
            "ball",
            &format!(
                "init position: docked={} side={} x={:.3} y={:.3}",
                geometry.docked,
                if geometry.side_right { "right" } else { "left" },
                geometry.x_frac,
                geometry.y_frac
            ),
        );

        BALL.with(|b| {
            *b.borrow_mut() = Some(Ball {
                expanded: false,
                hovering: false,
                pressed: false,
                dragging: false,
                drag_origin: POINT::default(),
                win_origin: POINT::default(),
                docked: geometry.docked,
                side_right: geometry.side_right,
                x_frac: geometry.x_frac,
                y_frac: geometry.y_frac,
                slide_to: 0,
                slide_step: 0,
            });
        });

        publish_current();
        place(hwnd, geometry, false);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        render(hwnd, Face::Idle);
    }
    Ok(())
}

// ---------------------------------------------------------------- geometry

/// Work area of one monitor, in physical pixels.
fn monitor_work_area(monitor: HMONITOR) -> RECT {
    unsafe {
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            info.rcWork
        } else {
            RECT {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            }
        }
    }
}

/// Work area of the monitor the ball window sits on (the nearest one when it
/// straddles two), in physical pixels. This is the authority for both the dock
/// position and the "fully visible" bounds of a floating ball.
fn work_area(hwnd: HWND) -> RECT {
    unsafe { monitor_work_area(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST)) }
}

/// Work area of the monitor under `pt` (the nearest one if the point is outside
/// every monitor).
///
/// While dragging the ball is bounded by the monitor under the *cursor*, not the
/// one under the window: that is what lets the user drag across monitors (the
/// bounds switch as the cursor crosses) while the ball still never leaves the
/// work area of the screen it is on.
fn work_area_at(pt: POINT) -> RECT {
    unsafe { monitor_work_area(MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST)) }
}

fn target_x(rect: &RECT, side_right: bool, expanded: bool) -> i32 {
    // While collapsed we want PEEK_VISIBLE pixels of the *circle* on screen, not
    // of the window: the window has a transparent margin of PADDING on each side,
    // so sliding it out by PADDING more is what makes the sliver actually visible.
    let pad = PADDING as i32;
    if side_right {
        if expanded {
            rect.right - BALL_SIZE
        } else {
            rect.right - pad - PEEK_VISIBLE
        }
    } else if expanded {
        rect.left
    } else {
        rect.left + pad + PEEK_VISIBLE - BALL_SIZE
    }
}

/// Pixels a window can travel along one axis while staying fully inside the
/// work area.
fn travel(span: i32) -> i32 {
    (span - BALL_SIZE).max(1)
}

/// Map a `0.0..1.0` fraction onto an axis: `0.0` puts the window against the
/// near edge, `1.0` against the far edge (both fully inside the work area).
fn frac_to_px(origin: i32, travel: i32, frac: f64) -> i32 {
    origin + (travel as f64 * frac.clamp(0.0, 1.0)).round() as i32
}

/// Inverse of [`frac_to_px`]; exact for integer pixel positions.
fn px_to_frac(origin: i32, travel: i32, px: i32) -> f64 {
    ((px - origin) as f64 / travel as f64).clamp(0.0, 1.0)
}

/// Clamp the window's top-left so the whole 64x64 window fits inside `area`.
fn clamp_into(area: &RECT, x: i32, y: i32) -> (i32, i32) {
    let max_x = (area.right - BALL_SIZE).max(area.left);
    let max_y = (area.bottom - BALL_SIZE).max(area.top);
    (x.clamp(area.left, max_x), y.clamp(area.top, max_y))
}

/// The ball position that survives a restart.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Geometry {
    /// `true`: pinned to the edge named by `side_right` (peek mode).
    /// `false`: free floating at `(x_frac, y_frac)`.
    docked: bool,
    /// The docked edge; while floating, the side the panel opens towards.
    side_right: bool,
    /// Floating x: `0.0` = left edge of the work area, `1.0` = right edge.
    x_frac: f64,
    /// Vertical position: `0.0` = top, `1.0` = bottom. Used in both modes.
    y_frac: f64,
}

impl Geometry {
    fn from_config(cfg: &state::Config) -> Self {
        Self {
            docked: cfg.ball_docked,
            side_right: cfg.ball_side != "left",
            x_frac: cfg.ball_x.clamp(0.0, 1.0),
            y_frac: cfg.ball_y.clamp(0.0, 1.0),
        }
    }

    /// Top-left of the ball window for this position inside `area`.
    ///
    /// `expanded` only matters while docked: it is the difference between the
    /// peek sliver and the fully slid-out ball.
    fn position(&self, area: &RECT, expanded: bool) -> (i32, i32) {
        let y = frac_to_px(area.top, travel(area.bottom - area.top), self.y_frac);
        if self.docked {
            (target_x(area, self.side_right, expanded), y)
        } else {
            (
                frac_to_px(area.left, travel(area.right - area.left), self.x_frac),
                y,
            )
        }
    }
}

/// Which side edge (if any) a window released at `x` should dock to.
///
/// Only the left/right edges dock — the design has no top/bottom docking — so a
/// ball dropped near the top or bottom of the screen simply floats there.
/// `None` means "not near an edge": keep floating exactly at the drop position.
fn snap_side(area: &RECT, x: i32) -> Option<bool> {
    let left_gap = x - area.left;
    let right_gap = area.right - (x + BALL_SIZE);
    let near_left = left_gap <= SNAP_DISTANCE;
    let near_right = right_gap <= SNAP_DISTANCE;
    match (near_left, near_right) {
        (true, false) => Some(false),
        (false, true) => Some(true),
        // Degenerate work area narrower than 2 * SNAP_DISTANCE + BALL_SIZE:
        // both edges are in range, so the nearer one wins.
        (true, true) => Some(left_gap > right_gap),
        (false, false) => None,
    }
}

/// Position to persist after a drag release. Snaps to the side edge when the
/// window ended within [`SNAP_DISTANCE`] of it; otherwise keeps the drop
/// position exactly, clamped so the whole ball stays inside the work area.
fn geometry_after_release(area: &RECT, released: &RECT) -> Geometry {
    let (x, y) = clamp_into(area, released.left, released.top);
    let x_frac = px_to_frac(area.left, travel(area.right - area.left), x);
    let y_frac = px_to_frac(area.top, travel(area.bottom - area.top), y);
    match snap_side(area, x) {
        Some(side_right) => Geometry {
            docked: true,
            side_right,
            x_frac,
            y_frac,
        },
        None => Geometry {
            docked: false,
            // Which half of the monitor the ball rests on: panel.rs opens the
            // panel towards the roomier side, the quick menu picks its fan side.
            side_right: (x + BALL_SIZE / 2) > (area.left + area.right) / 2,
            x_frac,
            y_frac,
        },
    }
}

/// Move the ball window to `geometry` (physical pixels).
fn place(hwnd: HWND, geometry: Geometry, expanded: bool) {
    let area = work_area(hwnd);
    let (x, y) = geometry.position(&area, expanded);
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            x,
            y,
            BALL_SIZE,
            BALL_SIZE,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
}

/// Slide the ball out (hover / panel open) or back to its docked sliver.
///
/// Only a *docked* ball has an edge to slide to. While the ball floats free this
/// is deliberately a no-op position-wise: `expand()` / `collapse()` come from
/// panel.rs and the tray and must never yank a free ball back to an edge.
fn slide(hwnd: HWND, expand: bool) {
    let Some(geometry) = BALL.with(|b| {
        let mut guard = b.borrow_mut();
        let ball = guard.as_mut()?;
        ball.expanded = expand;
        Some(ball.geometry())
    }) else {
        return;
    };

    if !geometry.docked {
        // A floating ball is fully visible wherever it was dropped; there is
        // nothing to slide. Stop any dock animation that may still be running.
        unsafe {
            let _ = KillTimer(Some(hwnd), TIMER_SLIDE);
        }
        return;
    }

    let area = work_area(hwnd);
    let mut wr = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut wr);
    }
    let to = target_x(&area, geometry.side_right, expand);
    let dist = (to - wr.left).abs();
    BALL.with(|b| {
        if let Some(ball) = b.borrow_mut().as_mut() {
            ball.slide_to = to;
            ball.slide_step = ((dist / 4).max(6)) * if to > wr.left { 1 } else { -1 };
        }
    });
    if dist < 6 {
        // Even a tiny reposition moves the window under the pointer, which is
        // enough for Windows to synthesise a click; suppress those too.
        IGNORE_CLICKS_UNTIL.store(now_ms() + SLIDE_IGNORE_CLICKS_MS, Ordering::SeqCst);
        place(hwnd, geometry, expand);
        return;
    }
    // Moving a no-activate layered window under a stationary pointer makes
    // Windows synthesise a click on it. The slide is a pure hover animation, so
    // clicks that land while it runs are ignored (see IGNORE_CLICKS_UNTIL).
    IGNORE_CLICKS_UNTIL.store(
        now_ms() + SLIDE_IGNORE_CLICKS_MS,
        Ordering::SeqCst,
    );
    unsafe {
        SetTimer(Some(hwnd), TIMER_SLIDE, 12, None);
    }
}

fn tick_slide(hwnd: HWND) {
    let mut stop = false;
    let mut restart: Option<(Geometry, bool)> = None;
    BALL.with(|b| {
        let guard = b.borrow();
        let Some(ball) = guard.as_ref() else { return };
        if !ball.docked {
            // The ball was un-docked / picked up mid-animation: never let a
            // leftover timer drag a floating ball back to an edge.
            stop = true;
            restart = Some((ball.geometry(), ball.expanded));
            return;
        }
        let mut wr = RECT::default();
        unsafe {
            let _ = GetWindowRect(hwnd, &mut wr);
        }
        let next = wr.left + ball.slide_step;
        stop = if ball.slide_step > 0 {
            next >= ball.slide_to
        } else {
            next <= ball.slide_to
        };
        if !stop {
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    next,
                    wr.top,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
        } else {
            restart = Some((ball.geometry(), ball.expanded));
        }
    });
    if stop {
        unsafe {
            let _ = KillTimer(Some(hwnd), TIMER_SLIDE);
        }
        if let Some((geometry, expanded)) = restart {
            // place() is skipped for the floating case: the ball is already
            // exactly where it belongs, and a redock would be wrong.
            if geometry.docked {
                place(hwnd, geometry, expanded);
            }
        }
    }
}

// --------------------------------------------------------------- rendering

struct Canvas {
    w: i32,
    h: i32,
    px: Vec<[f32; 4]>,
}

impl Canvas {
    fn new(w: i32, h: i32) -> Self {
        // A 1/255 alpha keeps every pixel hit-testable: a layered window with
        // zero-alpha pixels is click-through, and that is what used to make the
        // hover state flip while the ball sat at the screen edge.
        Self {
            w,
            h,
            px: vec![[0.0, 0.0, 0.0, 1.0 / 255.0]; (w * h) as usize],
        }
    }

    fn blend(&mut self, x: i32, y: i32, color: [f32; 4], coverage: f32) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return;
        }
        let a = color[3] * coverage;
        if a <= 0.0 {
            return;
        }
        let i = (y * self.w + x) as usize;
        let dst = &mut self.px[i];
        let out_a = a + dst[3] * (1.0 - a);
        if out_a <= 0.0 {
            return;
        }
        for c in 0..3 {
            dst[c] = (color[c] * a + dst[c] * dst[3] * (1.0 - a)) / out_a;
        }
        dst[3] = out_a;
    }

    fn fill_disc(&mut self, cx: f32, cy: f32, r: f32, color: [f32; 4]) {
        let x0 = (cx - r - 1.0).floor().max(0.0) as i32;
        let x1 = (cx + r + 1.0).ceil().min(self.w as f32) as i32;
        let y0 = (cy - r - 1.0).floor().max(0.0) as i32;
        let y1 = (cy + r + 1.0).ceil().min(self.h as f32) as i32;
        for y in y0..y1 {
            for x in x0..x1 {
                let mut hits = 0;
                for sy in 0..4 {
                    for sx in 0..4 {
                        let px = x as f32 + (sx as f32 + 0.5) / 4.0;
                        let py = y as f32 + (sy as f32 + 0.5) / 4.0;
                        let dx = px - cx;
                        let dy = py - cy;
                        if dx * dx + dy * dy <= r * r {
                            hits += 1;
                        }
                    }
                }
                let cov = hits as f32 / 16.0;
                if cov > 0.0 {
                    self.blend(x, y, color, cov);
                }
            }
        }
    }

    fn fill_ring(&mut self, cx: f32, cy: f32, r_out: f32, width: f32, color: [f32; 4]) {
        let r_in = r_out - width;
        let x0 = (cx - r_out - 1.0).floor().max(0.0) as i32;
        let x1 = (cx + r_out + 1.0).ceil().min(self.w as f32) as i32;
        let y0 = (cy - r_out - 1.0).floor().max(0.0) as i32;
        let y1 = (cy + r_out + 1.0).ceil().min(self.h as f32) as i32;
        for y in y0..y1 {
            for x in x0..x1 {
                let mut hits = 0;
                for sy in 0..4 {
                    for sx in 0..4 {
                        let px = x as f32 + (sx as f32 + 0.5) / 4.0;
                        let py = y as f32 + (sy as f32 + 0.5) / 4.0;
                        let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
                        if d <= r_out && d >= r_in {
                            hits += 1;
                        }
                    }
                }
                let cov = hits as f32 / 16.0;
                if cov > 0.0 {
                    self.blend(x, y, color, cov);
                }
            }
        }
    }

    fn fill_round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, color: [f32; 4]) {
        let x0 = x.floor().max(0.0) as i32;
        let x1 = (x + w).ceil().min(self.w as f32) as i32;
        let y0 = y.floor().max(0.0) as i32;
        let y1 = (y + h).ceil().min(self.h as f32) as i32;
        for py in y0..y1 {
            for px in x0..x1 {
                let mut hits = 0;
                for sy in 0..4 {
                    for sx in 0..4 {
                        let fx = px as f32 + (sx as f32 + 0.5) / 4.0;
                        let fy = py as f32 + (sy as f32 + 0.5) / 4.0;
                        if inside_round_rect(fx, fy, x, y, w, h, r) {
                            hits += 1;
                        }
                    }
                }
                let cov = hits as f32 / 16.0;
                if cov > 0.0 {
                    self.blend(px, py, color, cov);
                }
            }
        }
    }

    /// Premultiplied BGRA, which is what UpdateLayeredWindow expects.
    fn to_bgra_premul(&self) -> Vec<u8> {
        let mut out = vec![0u8; (self.w * self.h * 4) as usize];
        for (i, p) in self.px.iter().enumerate() {
            let a = p[3].clamp(0.0, 1.0);
            let o = i * 4;
            out[o] = (p[2].clamp(0.0, 1.0) * a * 255.0).round() as u8;
            out[o + 1] = (p[1].clamp(0.0, 1.0) * a * 255.0).round() as u8;
            out[o + 2] = (p[0].clamp(0.0, 1.0) * a * 255.0).round() as u8;
            out[o + 3] = (a * 255.0).round() as u8;
        }
        out
    }
}

fn inside_round_rect(px: f32, py: f32, x: f32, y: f32, w: f32, h: f32, r: f32) -> bool {
    let r = r.min(w / 2.0).min(h / 2.0);
    let cx = px.clamp(x + r, x + w - r);
    let cy = py.clamp(y + r, y + h - r);
    let dx = px - cx;
    let dy = py - cy;
    dx * dx + dy * dy <= r * r
}

fn rgba(r: u8, g: u8, b: u8, a: f32) -> [f32; 4] {
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a]
}

fn render(hwnd: HWND, face: Face) {
    let size = BALL_SIZE;
    let mut c = Canvas::new(size, size);
    let cx = size as f32 / 2.0;
    let cy = size as f32 / 2.0;
    let radius = size as f32 / 2.0 - PADDING;

    // Soft halo so the orb separates from any desktop background.
    c.fill_disc(cx, cy, radius + 5.0, rgba(60, 140, 255, 0.10));
    c.fill_disc(cx, cy, radius + 3.0, rgba(60, 140, 255, 0.12));

    // Drop shadow: a dark disc nudged down, drawn before the body.
    c.fill_disc(cx, cy + 1.6, radius, rgba(0, 0, 0, 0.28));

    // Body: radial gradient outward from a bright core, so it reads on both
    // light and dark desktops (a near-black orb disappeared on dark themes).
    let steps = 22;
    for i in (0..steps).rev() {
        let t = i as f32 / steps as f32;
        let r = radius * (0.12 + 0.88 * t);
        let shade = 1.0 - 0.42 * t;
        let a = if face == Face::Hover { 1.0 } else { 0.97 };
        c.fill_disc(
            cx,
            cy - radius * 0.06,
            r,
            rgba(
                (60.0 * shade + 12.0) as u8,
                (140.0 * shade + 40.0) as u8,
                (255.0 * shade + 0.0) as u8,
                a,
            ),
        );
    }

    // Top sheen
    c.fill_disc(
        cx,
        cy - radius * 0.34,
        radius * 0.62,
        rgba(255, 255, 255, if face == Face::Hover { 0.16 } else { 0.10 }),
    );

    // Rim
    let ring = if face == Face::Hover {
        rgba(190, 220, 255, 0.85)
    } else {
        rgba(150, 195, 255, 0.55)
    };
    c.fill_ring(cx, cy, radius - 0.4, 1.4, ring);

    let cell = radius * 0.44;
    let gap = radius * 0.17;
    let left = cx - cell - gap / 2.0;
    let top = cy - cell - gap / 2.0;
    let glyph = rgba(255, 255, 255, if face == Face::Hover { 0.98 } else { 0.9 });
    for row in 0..2 {
        for col in 0..2 {
            let x = left + col as f32 * (cell + gap);
            let y = top + row as f32 * (cell + gap);
            c.fill_round_rect(x, y, cell, cell, cell * 0.26, glyph);
        }
    }

    let pixels = c.to_bgra_premul();

    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        if let Ok(dib) = CreateDIBSection(Some(mem), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            let old = SelectObject(mem, HGDIOBJ(dib.0));
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u8, pixels.len());

            let mut wr = RECT::default();
            let _ = GetWindowRect(hwnd, &mut wr);
            let dst = POINT {
                x: wr.left,
                y: wr.top,
            };
            let src = POINT { x: 0, y: 0 };
            let sz = SIZE {
                cx: size,
                cy: size,
            };
            let blend = BLENDFUNCTION {
                BlendOp: 0,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: 1,
            };
            // NOTE: the DIB must still be selected into `mem` here — deselecting
            // it first leaves the DC holding the default 1x1 monochrome bitmap
            // and UpdateLayeredWindow silently paints nothing.
            let result = UpdateLayeredWindow(
                hwnd,
                Some(screen),
                Some(&dst),
                Some(&sz),
                Some(mem),
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
            if result.is_err() {
                crate::logging::write(
                    "error",
                    "ball",
                    &format!("UpdateLayeredWindow failed: {:?}", result.err()),
                );
            }

            SelectObject(mem, old);
            let _ = DeleteObject(HGDIOBJ(dib.0));
        }
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
    }
}

// --------------------------------------------------------------- windowproc

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let result = match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut ps);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let mut start_hover = false;
            let mut drag_pos: Option<(i32, i32)> = None;
            BALL.with(|b| {
                let mut guard = b.borrow_mut();
                let Some(ball) = guard.as_mut() else { return };
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                // A press only turns into a drag once the pointer travelled past
                // the threshold; below it the same press stays a click and must
                // toggle the panel instead of nudging the ball.
                if ball.pressed
                    && !ball.dragging
                    && ((pt.x - ball.drag_origin.x).abs() > DRAG_THRESHOLD
                        || (pt.y - ball.drag_origin.y).abs() > DRAG_THRESHOLD)
                {
                    ball.dragging = true;
                    // The hover slide may still be animating the window towards
                    // the pointer. Rebase the drag on the *current* window
                    // position so the first step does not jump, then stop the
                    // animation so it cannot fight the drag.
                    let mut wr = RECT::default();
                    let _ = GetWindowRect(hwnd, &mut wr);
                    ball.win_origin = POINT {
                        x: wr.left,
                        y: wr.top,
                    };
                    // Deliberately keep `drag_origin` at the *press* point: the
                    // pointer may already have travelled further than the
                    // threshold before this message arrived (a fast flick, or a
                    // coarse pointer report). Re-basing it to `pt` would swallow
                    // that distance and leave the ball permanently trailing the
                    // cursor — which is what made a drag towards the edge stop
                    // ~50 px short and never dock.
                    let _ = KillTimer(Some(hwnd), TIMER_SLIDE);
                    // Capture the mouse: the ball is only 64x64, so without it
                    // the very first pointer move outside the window would end
                    // the drag instead of moving the ball.
                    let _ = SetCapture(hwnd);
                }
                if ball.dragging {
                    // Follow the pointer 1:1, but bound by the work area of the
                    // monitor under the cursor. That is what lets the ball be
                    // dragged across monitors while never going off screen.
                    let area = work_area_at(pt);
                    drag_pos = Some(clamp_into(
                        &area,
                        ball.win_origin.x + (pt.x - ball.drag_origin.x),
                        ball.win_origin.y + (pt.y - ball.drag_origin.y),
                    ));
                    return;
                }
                if !ball.hovering {
                    ball.hovering = true;
                    start_hover = true;
                }
            });
            if start_hover {
                // The pointer is back: cancel any pending delayed collapse.
                let _ = KillTimer(Some(hwnd), TIMER_COLLAPSE);
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut tme);
                render(hwnd, Face::Hover);
                // No-op (position-wise) while floating; dodges the edge slide.
                slide(hwnd, true);
            }
            if let Some((x, y)) = drag_pos {
                unsafe {
                    let _ = SetWindowPos(
                        hwnd,
                        Some(HWND_TOPMOST),
                        x,
                        y,
                        0,
                        0,
                        SWP_NOSIZE | SWP_NOACTIVATE,
                    );
                }
            }
            LRESULT(0)
        }
        MSG_MOUSELEAVE => {
            let mut schedule_collapse = false;
            BALL.with(|b| {
                let mut g = b.borrow_mut();
                if let Some(ball) = g.as_mut() {
                    ball.hovering = false;
                    // Only a docked peek ball has a sliver to collapse back to;
                    // a floating ball stays fully visible where it was dropped.
                    schedule_collapse =
                        ball.docked && ball.expanded && !ball.dragging && !ball.pressed;
                }
            });
            render(hwnd, Face::Idle);
            if schedule_collapse {
                // Do not snap back immediately: while the ball slides, or while
                // the pointer crosses the transparent margin next to the screen
                // edge, it can re-enter at once. Waiting here is what breaks the
                // old expand/collapse feedback loop.
                let _ = SetTimer(Some(hwnd), TIMER_COLLAPSE, COLLAPSE_DELAY_MS, None);
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            // A press means the pointer is here: drop any pending delayed collapse.
            let _ = KillTimer(Some(hwnd), TIMER_COLLAPSE);
            // Freeze the hover slide too. The drag maths below assume the window
            // stays where it was when the button went down; letting a slide keep
            // running until the drag threshold is crossed would add that motion
            // to the first drag step.
            let _ = KillTimer(Some(hwnd), TIMER_SLIDE);
            BALL.with(|b| {
                let mut g = b.borrow_mut();
                if let Some(ball) = g.as_mut() {
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    ball.drag_origin = pt;
                    let mut wr = RECT::default();
                    let _ = GetWindowRect(hwnd, &mut wr);
                    ball.win_origin = POINT {
                        x: wr.left,
                        y: wr.top,
                    };
                    ball.pressed = true;
                    ball.dragging = false;
                }
            });
            // Capture from the very press: the pointer is allowed to leave the
            // 64x64 window while it is down, and both the drag and the release
            // have to keep arriving here. Capture is released on WM_LBUTTONUP.
            let _ = SetCapture(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let mut clicked = false;
            let mut hover_after = false;
            // `place()` sends window messages, which re-enter this procedure.
            // It must therefore run *after* the thread-local borrow is dropped;
            // a second `borrow_mut` while this one is alive used to panic.
            let mut dropped_at: Option<Geometry> = None;
            BALL.with(|b| {
                let mut guard = b.borrow_mut();
                let Some(ball) = guard.as_mut() else { return };
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                // A release without a matching press is not a click on the ball.
                // Windows synthesises one when a no-activate window is moved
                // under a stationary pointer (the hover slide does exactly that),
                // and treating it as a click opened the panel on its own.
                let was_pressed = ball.pressed;
                let moved = ball.dragging
                    || (pt.x - ball.drag_origin.x).abs() > DRAG_THRESHOLD
                    || (pt.y - ball.drag_origin.y).abs() > DRAG_THRESHOLD;
                ball.pressed = false;
                ball.dragging = false;
                if !moved {
                    if was_pressed && !clicks_ignored() {
                        clicked = true;
                    }
                    return;
                }

                // Work out the persisted position from the actual window rect:
                // dock only when the release landed within SNAP_DISTANCE of a
                // side edge, otherwise keep exactly the drop position (the drag
                // loop already clamped it inside the work area).
                let area = work_area(hwnd);
                let mut wr = RECT::default();
                let _ = GetWindowRect(hwnd, &mut wr);
                let geometry = geometry_after_release(&area, &wr);
                ball.apply_geometry(geometry);
                ball.expanded = false;

                let mut cfg = state::load_config();
                cfg.ball_docked = geometry.docked;
                cfg.ball_side = if geometry.side_right { "right" } else { "left" }.into();
                cfg.ball_x = geometry.x_frac;
                cfg.ball_y = geometry.y_frac;
                let _ = state::save_config(&cfg);
                crate::logging::write(
                    "info",
                    "ball",
                    &format!(
                        "drag released: docked={} side={} x={:.3} y={:.3}",
                        geometry.docked,
                        if geometry.side_right { "right" } else { "left" },
                        geometry.x_frac,
                        geometry.y_frac
                    ),
                );

                // The ball may have snapped away from the pointer, or the
                // pointer may still sit on the (expanded) ball. Restore the
                // hover state now instead of waiting for the next mouse move,
                // so a snap never leaves a stuck peek sliver under the cursor.
                // For a docked ball the reachable area is the expanded one.
                let (probe_x, probe_y) = geometry.position(&area, geometry.docked);
                hover_after = pt.x >= probe_x
                    && pt.x < probe_x + BALL_SIZE
                    && pt.y >= probe_y
                    && pt.y < probe_y + BALL_SIZE;
                ball.hovering = hover_after;
                dropped_at = Some(geometry);
            });
            // Always drop the capture, click or drag. `ReleaseCapture` is safe
            // even when this window does not hold it.
            let _ = ReleaseCapture();
            if clicked {
                crate::logging::write("info", "ball", "clicked -> toggle panel");
                crate::windows::panel::toggle(&app());
            } else {
                if let Some(geometry) = dropped_at {
                    place(hwnd, geometry, false);
                }
                if hover_after {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut tme);
                    render(hwnd, Face::Hover);
                    // Docked: slide back out under the pointer; floating: no-op.
                    slide(hwnd, true);
                } else {
                    render(hwnd, Face::Idle);
                }
            }
            LRESULT(0)
        }
        WM_TIMER => {
            match wparam.0 {
                TIMER_SLIDE => tick_slide(hwnd),
                TIMER_COLLAPSE => {
                    let _ = KillTimer(Some(hwnd), TIMER_COLLAPSE);
                    // The pointer may have come back while the timer was pending;
                    // only collapse if it is still away.
                    let collapse = BALL.with(|b| {
                        let g = b.borrow();
                        g.as_ref()
                            .map(|ball| {
                                ball.expanded && !ball.hovering && !ball.dragging && !ball.pressed
                            })
                            .unwrap_or(false)
                    });
                    if collapse {
                        slide(hwnd, false);
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_BALL_CMD => {
            // Commands posted from another thread (tray, panel, IPC). They must
            // never touch the thread-local `BALL` state directly, so everything
            // is re-dispatched here on the window's own thread.
            let _ = KillTimer(Some(hwnd), TIMER_COLLAPSE);
            match wparam.0 {
                CMD_EXPAND => slide(hwnd, true),
                CMD_COLLAPSE => slide(hwnd, false),
                CMD_REPOSITION => reposition_now(hwnd),
                CMD_TOGGLE_VISIBLE => toggle_visible_now(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            BALL_HWND.store(0, Ordering::SeqCst);
            BALL.with(|b| {
                *b.borrow_mut() = None;
            });
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            show_menu(hwnd);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    };
    // The handlers above mutate the thread-local ball; mirror the cross-thread
    // snapshot once per message so `ball_rect` / `dragging` / `anchor_kind` never
    // observe a stale value from another thread.
    publish_current();
    result
}

fn show_menu(hwnd: HWND) {
    const ID_PANEL: usize = 1;
    const ID_MAIN: usize = 2;
    const ID_QUIT: usize = 3;
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let _ = AppendMenuW(menu, MF_STRING, ID_PANEL, w!("打开工具面板"));
        let _ = AppendMenuW(menu, MF_STRING, ID_MAIN, w!("主界面…"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, ID_QUIT, w!("退出"));

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            Some(0),
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);
        match cmd.0 as usize {
            ID_PANEL => crate::windows::panel::toggle(&app()),
            ID_MAIN => crate::windows::main::open(&app()),
            ID_QUIT => app().exit(0),
            _ => {}
        }
    }
}

// ------------------------------------------------------------- public api

/// Toggle the ball window's visibility. Safe to call from any thread: the
/// command is posted to the ball window and applied on its own thread.
pub fn toggle_visible(_app: &AppHandle) {
    run_ball_cmd(CMD_TOGGLE_VISIBLE);
}

/// Re-read the persisted position and dock the ball again. Safe from any thread.
pub fn reposition(_app: &AppHandle) {
    run_ball_cmd(CMD_REPOSITION);
}

fn toggle_visible_now(hwnd: HWND) {
    // A hidden ball must not leave its hover fan floating on its own.
    crate::windows::quickmenu::on_ball_hidden();
    BALL.with(|b| {
        let g = b.borrow();
        if g.is_none() {
            return;
        }
        unsafe {
            if IsWindowVisible(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_HIDE);
            } else {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                render(hwnd, Face::Idle);
            }
        }
    });
}

fn reposition_now(hwnd: HWND) {
    // Re-place from config in both modes: ipc::save_config calls this after the
    // settings page changes ballSide / ballY / ballDocked, so it has to handle
    // "dock to an edge" and "float at (ballX, ballY)" alike.
    let geometry = Geometry::from_config(&state::load_config());
    unsafe {
        let _ = KillTimer(Some(hwnd), TIMER_SLIDE);
        let _ = KillTimer(Some(hwnd), TIMER_COLLAPSE);
    }
    let mut found = false;
    BALL.with(|b| {
        let mut guard = b.borrow_mut();
        let Some(ball) = guard.as_mut() else { return };
        found = true;
        ball.apply_geometry(geometry);
        ball.expanded = false;
        ball.pressed = false;
        ball.dragging = false;
        ball.hovering = false;
    });
    if found {
        place(hwnd, geometry, false);
        render(hwnd, Face::Idle);
    }
}

/// Screen rect of the ball: physical-pixel `(left, top, right, bottom, side_right)`.
///
/// Callable from **any thread**: the window handle comes from an atomic and
/// `GetWindowRect` marshals to the owning thread, which is what
/// `panel.rs::anchor()` relies on when the panel is created on the main thread.
///
/// * Docked + peek: the 64x64 window deliberately extends past the work-area
///   edge so `PEEK_VISIBLE` pixels of the circle still show (`right - left` and
///   `bottom - top` stay `BALL_SIZE`).
/// * Floating: the rect is fully inside the work area of the monitor nearest
///   the ball (clamped on drop).
/// * `side_right`: the docked edge while docked; while floating, which half of
///   that monitor the ball rests in — so the panel opens towards the roomier
///   side instead of a stale config value.
pub fn ball_rect() -> Option<(i32, i32, i32, i32, bool)> {
    let hwnd = ball_hwnd()?;
    let mut wr = RECT::default();
    unsafe {
        GetWindowRect(hwnd, &mut wr).ok()?;
    }
    let docked = SHARED_DOCKED.load(Ordering::SeqCst);
    let side_right = if docked {
        SHARED_SIDE_RIGHT.load(Ordering::SeqCst)
    } else {
        let area = work_area(hwnd);
        (wr.left + wr.right) / 2 > (area.left + area.right) / 2
    };
    Some((wr.left, wr.top, wr.right, wr.bottom, side_right))
}

/// `true` while the user is dragging the ball.
///
/// Safe from **any thread** (the native quick menu and the panel may poll it);
/// `false` before the ball window exists and after it is destroyed.
pub fn dragging() -> bool {
    ball_hwnd().is_some() && SHARED_DRAGGING.load(Ordering::SeqCst)
}

/// Where the ball currently sits: docked to an edge, or floating free.
///
/// The hover quick menu uses this to pick the fan geometry (left/right half fan
/// vs. full circle). `None` means the ball window is not initialised.
pub fn anchor_kind() -> Option<crate::windows::quickmenu::Anchor> {
    use crate::windows::quickmenu::Anchor;
    if ball_hwnd().is_none() {
        return None;
    }
    Some(if !SHARED_DOCKED.load(Ordering::SeqCst) {
        Anchor::Floating
    } else if SHARED_SIDE_RIGHT.load(Ordering::SeqCst) {
        Anchor::RightEdge
    } else {
        Anchor::LeftEdge
    })
}

/// Slide the ball out. Safe to call from any thread (posted to the ball window).
/// No-op position-wise while the ball floats free.
pub fn expand() {
    run_ball_cmd(CMD_EXPAND);
}

/// Slide the ball back to its docked sliver. Safe from any thread.
/// No-op position-wise while the ball floats free.
pub fn collapse() {
    run_ball_cmd(CMD_COLLAPSE);
}

/// DPI of the monitor the ball currently sits on (96 before init / after exit).
pub fn dpi() -> u32 {
    match ball_hwnd() {
        Some(hwnd) => unsafe { GetDpiForWindow(hwnd) },
        None => 96,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1920x1040 work area (1080p minus a 40px taskbar), physical pixels.
    fn area() -> RECT {
        RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        }
    }

    /// Ball window rect with its top-left at `area` origin + `(x, y)`.
    fn rect_at(area: &RECT, x: i32, y: i32) -> RECT {
        RECT {
            left: area.left + x,
            top: area.top + y,
            right: area.left + x + BALL_SIZE,
            bottom: area.top + y + BALL_SIZE,
        }
    }

    #[test]
    fn release_away_from_edges_keeps_the_drop_position() {
        let area = area();
        let g = geometry_after_release(&area, &rect_at(&area, 400, 300));
        assert!(!g.docked, "must float when dropped mid-screen");
        assert_eq!(g.position(&area, false), (400, 300));
        assert!(!g.side_right, "floating left of the monitor centre");
    }

    #[test]
    fn release_within_snap_distance_docks_to_the_near_edge() {
        let area = area();

        // Window edge 24px from the work-area edge: snaps left.
        let left = geometry_after_release(&area, &rect_at(&area, SNAP_DISTANCE, 300));
        assert!(left.docked);
        assert!(!left.side_right);
        assert_eq!(left.position(&area, true).0, area.left);

        // One pixel farther: stays exactly where it was dropped.
        let free = geometry_after_release(&area, &rect_at(&area, SNAP_DISTANCE + 1, 300));
        assert!(!free.docked);
        assert_eq!(free.position(&area, false), (SNAP_DISTANCE + 1, 300));

        // Same rule on the right edge.
        let right_x = area.right - BALL_SIZE - SNAP_DISTANCE;
        let right = geometry_after_release(&area, &rect_at(&area, right_x, 300));
        assert!(right.docked);
        assert!(right.side_right);
        assert_eq!(right.position(&area, true).0, area.right - BALL_SIZE);

        let right_free = geometry_after_release(&area, &rect_at(&area, right_x - 1, 300));
        assert!(!right_free.docked);
        assert_eq!(right_free.position(&area, false), (right_x - 1, 300));
    }

    #[test]
    fn drop_outside_the_work_area_is_clamped_fully_inside() {
        let area = area();

        // Past the top-left corner: clamped to it and docked left. A docked
        // peek ball extends past the edge on purpose, so check the expanded
        // (fully slid-out) position for the x, and the vertical clamp.
        let g = geometry_after_release(&area, &rect_at(&area, -200, -200));
        assert_eq!(g.position(&area, true), (area.left, area.top));
        assert_eq!(g.position(&area, false).1, area.top);
        assert!(g.docked && !g.side_right);

        // Past the bottom-right corner: clamped to it and docked right.
        let g = geometry_after_release(&area, &rect_at(&area, area.right + 100, area.bottom + 100));
        assert_eq!(
            g.position(&area, true),
            (area.right - BALL_SIZE, area.bottom - BALL_SIZE)
        );
        assert!(g.docked && g.side_right);
    }

    #[test]
    fn floating_position_round_trips_through_config_fractions() {
        let area = area();
        for x in [SNAP_DISTANCE + 1, 100, 640, 1500] {
            for y in [0, 33, 500, 1040 - BALL_SIZE] {
                let g = geometry_after_release(&area, &rect_at(&area, x, y));
                assert!(!g.docked, "({x},{y}) should float");
                assert_eq!(g.position(&area, false), (x, y));
                assert_eq!(
                    g.side_right,
                    x + BALL_SIZE / 2 > (area.left + area.right) / 2
                );

                // The same position must survive the config round-trip.
                let cfg = state::Config {
                    ball_docked: g.docked,
                    ball_side: if g.side_right { "right" } else { "left" }.into(),
                    ball_x: g.x_frac,
                    ball_y: g.y_frac,
                    ..Default::default()
                };
                let restored = Geometry::from_config(&cfg);
                assert_eq!(restored, g);
                assert_eq!(restored.position(&area, false), (x, y));
            }
        }
    }

    #[test]
    fn narrow_work_area_snaps_to_the_nearer_edge() {
        // Width 88 == BALL_SIZE + SNAP_DISTANCE: both gaps are in range from
        // either end, so the nearer edge has to win.
        let area = RECT {
            left: 0,
            top: 0,
            right: BALL_SIZE + SNAP_DISTANCE,
            bottom: 600,
        };
        let left = geometry_after_release(&area, &rect_at(&area, 0, 100));
        assert_eq!((left.docked, left.side_right), (true, false));
        let right = geometry_after_release(&area, &rect_at(&area, area.right - BALL_SIZE, 100));
        assert_eq!((right.docked, right.side_right), (true, true));
    }

    #[test]
    fn public_hooks_are_safe_off_the_ball_thread() {
        // The ball window is not created in tests: `BALL_HWND` is zero and the
        // thread-local `BALL` is empty. The helpers are called from the panel
        // thread and the native quick-menu window in the real app, so they must
        // answer from the atomics alone and never panic on a foreign thread.
        let answer = std::thread::spawn(|| (dragging(), anchor_kind(), ball_rect(), dpi()))
            .join()
            .expect("hooks must be callable from any thread");
        assert!(!answer.0);
        assert_eq!(answer.1, None);
        assert_eq!(answer.2, None);
        assert_eq!(answer.3, 96);
    }
}
