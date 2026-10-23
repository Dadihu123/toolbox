//! Hover quick menu: the fan of tool icons that opens while the pointer rests
//! on the floating ball (#8 native half, docs/design.md §1 / §4.1).
//!
//! Why native: this window lives next to the always-resident ball. A WebView for
//! a menu that is shown for a second would cost a renderer process and ~50 MB;
//! like [`super::ball`] this is a layered popup painted by a tiny software
//! rasteriser (4x4 supersampling, straight-alpha canvas, 32bpp DIB pushed with
//! `UpdateLayeredWindow`). No UI toolkit, no font rendering, every pixel known.
//!
//! Behaviour:
//!   * A watcher thread polls the ball (`~120 ms`) and the pointer. When the
//!     pointer dwells on the ball for [`DWELL`] and the ball is not moving, not
//!     being dragged and the panel is closed, the fan opens around the ball.
//!   * Geometry follows the ball's anchor: docked right -> half fan opening
//!     left, docked left -> half fan opening right, floating -> full circle.
//!     One ring while the count fits the ring capacity, otherwise two rings
//!     (inner ring filled first).
//!   * The fan stays open while the pointer is over the fan sector or the ball;
//!     it collapses after [`HIDE_DELAY`] once the pointer left both.
//!   * Clicking a chip opens the tool with [`super::tools::open`] and collapses
//!     the fan. The window is `WS_EX_NOACTIVATE`, so it never takes focus, and
//!     its per-pixel alpha makes the transparent padding click-through.
//!
//! Threading: the window is created and pumped on its own thread (never on the
//! Tauri event loop); all the window state (window handles, DIBs) is thread
//! local there. Everything other threads touch is an atomic or a short-lived
//! mutex (see [`show`]/[`hide`]/[`contains_point`]).

#![cfg(windows)]

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, EndPaint, GetDC,
    GetMonitorInfoW, MonitorFromPoint, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER,
    BLENDFUNCTION, BI_RGB, DIB_RGB_COLORS, HGDIOBJ, HMONITOR, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::*;

use super::ball;
use crate::icons::{self, RgbaIcon};
use crate::state::{self, QuickMenuConfig};

// --------------------------------------------------------------- public types

/// Where the ball currently is, which decides the fan geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    /// Docked to the left screen edge: fan opens to the right.
    LeftEdge,
    /// Docked to the right screen edge: fan opens to the left.
    RightEdge,
    /// Floating free: fan opens as a full circle.
    Floating,
}

/// Toolbar of icons for a position, already resolved against the registry.
#[derive(Debug, Clone)]
pub struct Item {
    pub tool_id: String,
    pub name: String,
    pub accent: String,
}

/// Resolve the configured tool ids for `anchor`, skipping unknown / disabled
/// tools, capped at `config.count`.
pub fn resolve_items(config: &QuickMenuConfig, anchor: Anchor) -> Vec<Item> {
    let side = match anchor {
        Anchor::LeftEdge => Some(false),
        Anchor::RightEdge => Some(true),
        Anchor::Floating => None,
    };
    let states = crate::state::load_tools();
    let mut out: Vec<Item> = Vec::new();
    let configured = config.slots(side);
    let manifests = crate::registry::scan();
    let push = |id: &str, out: &mut Vec<Item>| {
        if out.len() >= config.count {
            return;
        }
        if out.iter().any(|i| i.tool_id == id) {
            return;
        }
        let Some(manifest) = manifests.iter().find(|m| m.id == id) else {
            return;
        };
        if !manifest.error.is_empty() || !crate::registry::is_enabled(&states, id) {
            return;
        }
        out.push(Item {
            tool_id: manifest.id.clone(),
            name: manifest.name.clone(),
            accent: manifest.accent.clone(),
        });
    };
    for id in configured {
        push(id, &mut out);
    }
    // Fill any remaining fan slots with the first enabled tools so the feature
    // works out of the box, before the user configures anything.
    if out.len() < config.count {
        for manifest in manifests.iter() {
            if out.len() >= config.count {
                break;
            }
            push(&manifest.id, &mut out);
        }
    }
    out
}

// ------------------------------------------------------------------ constants

// Geometry is authored in "logical" px at 96 dpi and multiplied by the monitor
// scale on the ball's monitor. Working in physical px is what keeps the chips
// and the rasterised icons crisp at 150 % / 200 %.
/// Chip ("icon button") side, logical px.
const CHIP_SIZE: f32 = 44.0;
/// Corner radius of a chip, logical px.
const CHIP_RADIUS: f32 = 13.0;
/// Size the tool icon is rasterised to and drawn at, logical px.
const CHIP_ICON: f32 = 28.0;
/// Transparent margin around a chip: holds its drop shadow / accent glow and is
/// also the minimum margin the fan window keeps around the outermost chip.
const CHIP_GLOW: f32 = 10.0;
/// Minimum edge-to-edge gap between two neighbouring chips on the same ring.
const CHIP_MIN_GAP: f32 = 12.0;
/// Radial gap between a chip of the inner ring and a chip of the outer ring.
const CHIP_RING_GAP: f32 = 14.0;
/// `ball.rs` window size / transparent padding: the visible ball is a
/// `BALL_WINDOW - 2 * BALL_PADDING` px disc centred in its 64x64 window.
const BALL_WINDOW: f32 = 64.0;
const BALL_PADDING: f32 = 9.0;
const BALL_CIRCLE_RADIUS: f32 = BALL_WINDOW / 2.0 - BALL_PADDING; // 23 px

/// Pointer must rest on the ball this long before the fan opens.
const DWELL: Duration = Duration::from_millis(200);
/// The ball (and therefore the fan centre) must have been still this long. The
/// docked ball slides out of its peek sliver on hover; waiting for the slide to
/// settle is what keeps a half-finished rect from becoming the fan centre.
const SETTLE: Duration = Duration::from_millis(80);
/// Collapse delay after the pointer left both the fan and the ball. The pointer
/// needs a moment to cross the transparent gap between the ball and a chip, so
/// collapsing immediately would make the fan unusable.
const HIDE_DELAY: Duration = Duration::from_millis(250);
/// Idle poll of the watcher thread while the fan is closed.
const POLL_MS: u32 = 120;
/// Message wait while the fan is open: mouse messages wake the loop earlier,
/// this only bounds how long a pure cursor move is seen.
const OPEN_WAIT_MS: u32 = 16;
/// Re-read config.json this often (the settings page writes it from another
/// thread; the file is tiny, so a poll is simpler and safer than a callback).
const CONFIG_REFRESH: Duration = Duration::from_millis(800);
/// How often the panel visibility is probed while the fan is open.
const PANEL_CHECK: Duration = Duration::from_millis(250);
/// A click is a press + release on the same chip within this long.
const CLICK_TIMEOUT: Duration = Duration::from_millis(800);
/// A press that is still held after this long is stale (the release went
/// somewhere else); never turn it into a click.
const PRESS_STALE: Duration = Duration::from_millis(1500);

/// `WM_MOUSELEAVE` is not re-exported by the windows crate (same as ball.rs).
const MSG_MOUSELEAVE: u32 = 0x02A3;
/// Wakes the watcher thread out of `MsgWaitForMultipleObjectsEx`: used by
/// [`hide`] / [`maybe_show`], which may run on any thread.
const WM_QUICKMENU_WAKE: u32 = WM_APP + 0x51;
/// Window class of the fan. Kept unique per process; the ball uses its own.
const CLASS_NAME: windows::core::PCWSTR = w!("ToolboxQuickMenu");
/// Class the ball window registers (see ball.rs). Used from the watcher thread
/// to check `IsWindowVisible` on a handle `ball_rect()` does not expose.
const BALL_CLASS: windows::core::PCWSTR = w!("ToolboxFloatingBall");

// -------------------------------------------------------------- shared state

static APP: OnceLock<AppHandle> = OnceLock::new();
/// The fan window handle, for [`hide`] / [`maybe_show`] running off-thread.
static FAN_HWND: AtomicIsize = AtomicIsize::new(0);
/// The fan window is visible.
static OPEN: AtomicBool = AtomicBool::new(false);
/// `hide()` asked for a collapse; the watcher thread consumes it.
static FORCE_HIDE: AtomicBool = AtomicBool::new(false);

/// A show request posted by another thread. The ball may call [`maybe_show`]
/// from its own hover code (cheaper and snappier than a second dwell timer).
static REQUEST: Mutex<Option<(QuickMenuConfig, Anchor)>> = Mutex::new(None);

/// Screen-space hit test geometry, published while the fan is open so
/// [`contains_point`] works from any thread.
static HIT: Mutex<Option<HitArea>> = Mutex::new(None);

thread_local! {
    /// The fan while its window thread is running.
    static MENU: RefCell<Option<Menu>> = const { RefCell::new(None) };
}

fn wake() {
    let raw = FAN_HWND.load(Ordering::SeqCst);
    if raw != 0 {
        // PostMessage is the only cross-thread safe way to nudge the window's
        // own thread; touching the thread-local `MENU` from here would race.
        unsafe {
            let _ = PostMessageW(Some(HWND(raw as *mut _)), WM_QUICKMENU_WAKE, WPARAM(0), LPARAM(0));
        }
    }
}

// ---------------------------------------------------------------- public api

/// Create the fan window and start its watcher thread. Called once from
/// `lib.rs` after the ball is up; the watcher owns the window from then on.
pub fn init(app: &AppHandle) -> Result<(), String> {
    let _ = APP.set(app.clone());
    let handle = app.clone();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
    std::thread::Builder::new()
        .name("toolbox-quickmenu".into())
        .spawn(move || run(handle, ready_tx))
        .map_err(|e| e.to_string())?;
    // Surface window-creation failures to the caller (and the log) instead of
    // failing silently on a background thread.
    match ready_rx.recv_timeout(Duration::from_secs(3)) {
        Ok(result) => result,
        Err(_) => Err("quick menu window did not initialise in time".into()),
    }
}

/// Open the fan for `anchor`. Has the same effect as the watcher's own dwell
/// detection; called from any thread (the ball's hover handler may use it).
/// Ignored while disabled / dragging / while the panel is open.
pub fn maybe_show(config: &QuickMenuConfig, anchor: Anchor) {
    if !config.enabled {
        return;
    }
    if let Ok(mut slot) = REQUEST.lock() {
        *slot = Some((config.clone(), anchor));
    }
    wake();
}

/// Collapse the fan now. Safe from any thread: the actual `ShowWindow` runs on
/// the fan's own thread.
pub fn hide() {
    FORCE_HIDE.store(true, Ordering::SeqCst);
    wake();
}

/// The fan window is on screen.
pub fn is_open() -> bool {
    OPEN.load(Ordering::SeqCst)
}

/// `(x, y)` (physical screen px) is inside the fan's interactive region: over a
/// chip, the ball-to-chip gap or the ball itself. Callers use this to keep the
/// fan (and the expanded ball) alive while the pointer travels to an icon.
///
/// Transparent pixels of the layered window are click-through, so this is a
/// geometric test against the published layout rather than a window query.
pub fn contains_point(x: i32, y: i32) -> bool {
    HIT.lock()
        .ok()
        .and_then(|guard| guard.as_ref().map(|hit| hit.contains(x, y)))
        .unwrap_or(false)
}

/// Hide the fan together with the ball (ball hidden from the tray).
pub fn on_ball_hidden() {
    hide();
}

// --------------------------------------------------------------- watcher loop

fn run(app: AppHandle, ready: mpsc::Sender<Result<(), String>>) {
    let hwnd = match create_window() {
        Ok(hwnd) => hwnd,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    FAN_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
    let menu = Menu::new(hwnd, app);
    MENU.with(|slot| *slot.borrow_mut() = Some(menu));
    let _ = ready.send(Ok(()));
    pump(hwnd);
    // Only reached when the thread receives WM_QUIT (process shutdown).
    MENU.with(|slot| {
        if let Some(menu) = slot.borrow_mut().take() {
            unsafe {
                let _ = ShowWindow(menu.hwnd, SW_HIDE);
                let _ = DestroyWindow(menu.hwnd);
            }
        }
    });
    FAN_HWND.store(0, Ordering::SeqCst);
    OPEN.store(false, Ordering::SeqCst);
}

fn create_window() -> Result<HWND, String> {
    unsafe {
        let hinstance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            lpszClassName: CLASS_NAME,
            hCursor: LoadCursorW(None, IDC_HAND).unwrap_or_default(),
            ..Default::default()
        };
        // RegisterClassW fails harmlessly when the class already exists (init
        // called twice); CreateWindowExW below then reuses it.
        RegisterClassW(&wc);

        let hwnd = CreateWindowExW(
            // WS_EX_LAYERED: per-pixel alpha via UpdateLayeredWindow.
            // WS_EX_TOOLWINDOW: never in the taskbar / Alt+Tab.
            // WS_EX_TOPMOST: above normal windows, same band as the ball.
            // WS_EX_NOACTIVATE: clicking a chip must not steal focus from the
            //   panel / a tool window (the tool window itself takes focus).
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            CLASS_NAME,
            w!("ToolboxQuickMenu"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(hinstance.into()),
            None,
        )
        .map_err(|e| e.to_string())?;
        Ok(hwnd)
    }
}

fn pump(hwnd: HWND) {
    loop {
        // 1. Drain the message queue first: clicks and hover moves are then
        //    handled before the tick that reacts to them.
        let mut msg = MSG::default();
        loop {
            let got = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() };
            if !got {
                break;
            }
            if msg.message == WM_QUIT {
                return;
            }
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        // 2. Poll/drive the menu. The thread sleeps inside
        //    MsgWaitForMultipleObjectsEx below, so this is the only wake-up.
        MENU.with(|slot| {
            if let Some(menu) = slot.borrow_mut().as_mut() {
                menu.tick();
            }
        });
        // 3. Wait for input (fan open) or the next poll (fan closed).
        let mut open = OPEN.load(Ordering::SeqCst);
        // `open` may have changed during the tick; the atomic is authoritative.
        if !open {
            open = MENU.with(|slot| slot.borrow().as_ref().map(|m| m.open).unwrap_or(false));
        }
        let timeout = if open { OPEN_WAIT_MS } else { POLL_MS };
        unsafe {
            let _ = MsgWaitForMultipleObjectsEx(None, timeout, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
        }
        let _ = hwnd;
    }
}

// ---------------------------------------------------------------- menu state

/// One icon chip: a self-contained sprite per visual state plus its position in
/// the fan window's client area. Sprites are rasterised once (idle at show
/// time, hover lazily on the first hover) so moving the pointer between chips
/// is just a re-composite of a handful of small bitmaps.
struct Chip {
    tool_id: String,
    name: String,
    accent: [u8; 3],
    icon: Arc<RgbaIcon>,
    scale: f32,
    chip_px: i32,
    glow_px: i32,
    chip_radius: f32,
    /// Chip centre, client coordinates of the fan window.
    cx: f32,
    cy: f32,
    idle: Canvas,
    hover: Option<Canvas>,
}

impl Chip {
    fn ensure_hover(&mut self) {
        if self.hover.is_none() {
            let sprite = render_chip(self, true);
            self.hover = Some(sprite);
        }
    }
}

/// Published screen-space geometry for [`contains_point`]; cloned into the
/// `HIT` mutex on show and cleared on hide.
#[derive(Clone, Copy, Debug)]
struct HitArea {
    /// Ball circle centre (screen px).
    cx: i32,
    cy: i32,
    /// Distance from the centre covering the outermost chip + its glow.
    reach: f32,
    /// On the opening side the fan sector is a half disc; this is how far it
    /// may reach past the ball centre on the *other* side (chips at +-90 deg sit
    /// right below/above the ball, they need a little slack).
    margin: f32,
    anchor: Anchor,
}

impl HitArea {
    fn contains(&self, x: i32, y: i32) -> bool {
        let dx = x as f32 - self.cx as f32;
        let dy = y as f32 - self.cy as f32;
        if dx * dx + dy * dy > self.reach * self.reach {
            return false;
        }
        match self.anchor {
            Anchor::RightEdge => dx <= self.margin,
            Anchor::LeftEdge => dx >= -self.margin,
            Anchor::Floating => true,
        }
    }
}

struct Menu {
    hwnd: HWND,
    app: AppHandle,
    /// Last config snapshot (polled, see [`CONFIG_REFRESH`]).
    cfg: state::Config,
    cfg_at: Instant,
    /// Fan window visible.
    open: bool,
    /// Geometry of the current fan; also what [`hide`] uses to release a docked
    /// ball back to its peek sliver.
    anchor: Anchor,
    win_x: i32,
    win_y: i32,
    win_w: i32,
    win_h: i32,
    /// Ball window rect at show time. Any later change means the ball moved
    /// (drag / reposition / DPI change), which collapses the fan.
    ball_show: (i32, i32, i32, i32),
    chips: Vec<Chip>,
    hover: Option<usize>,
    /// Pointer left the fan+ball at this instant; collapse after [`HIDE_DELAY`].
    leave_at: Option<Instant>,
    /// Pressed chip (click candidate).
    pressed: Option<usize>,
    pressed_at: Instant,
    /// A chip was released -> open this tool on the next tick (outside the
    /// window procedure, so the tool window dispatch never runs inside it).
    pending_open: Option<(String, String)>,
    /// Watcher-side dwell state: pointer entered the ball at this instant.
    dwell_enter: Option<Instant>,
    last_ball: Option<(i32, i32, i32, i32)>,
    last_ball_change: Instant,
    /// `WM_MOUSELEAVE` tracking flag.
    tracking: bool,
    /// Explicit show request from `maybe_show` (kept until it can be honoured
    /// or becomes stale).
    requested: Option<(QuickMenuConfig, Anchor)>,
    /// Panel visibility, periodically refreshed (the panel must never share the
    /// screen with the fan).
    panel_visible: bool,
    panel_checked: Instant,
    /// Published copy of the keep-alive geometry.
    hit: Option<HitArea>,
}

impl Menu {
    fn new(hwnd: HWND, app: AppHandle) -> Self {
        Self {
            hwnd,
            app,
            cfg: state::load_config(),
            cfg_at: Instant::now(),
            open: false,
            anchor: Anchor::Floating,
            win_x: 0,
            win_y: 0,
            win_w: 1,
            win_h: 1,
            ball_show: (0, 0, 0, 0),
            chips: Vec::new(),
            hover: None,
            leave_at: None,
            pressed: None,
            pressed_at: Instant::now(),
            pending_open: None,
            dwell_enter: None,
            last_ball: None,
            last_ball_change: Instant::now(),
            tracking: false,
            requested: None,
            panel_visible: false,
            panel_checked: Instant::now() - Duration::from_secs(60),
            hit: None,
        }
    }

    // ------------------------------------------------------------- main tick

    fn tick(&mut self) {
        self.refresh_config();

        // A forced collapse may come from the tray/panel/ball on another thread.
        if FORCE_HIDE.swap(false, Ordering::SeqCst) && self.open {
            self.collapse("hide requested");
        }

        // A chip click: close the fan first, then open the tool from here (not
        // from the window procedure, which must stay fast and re-entrancy free).
        if let Some((tool_id, name)) = self.pending_open.take() {
            self.collapse("icon clicked");
            open_tool(&self.app, &tool_id, &name);
        }

        if self.open {
            self.check_panel(true);
            self.tick_open();
        } else {
            self.tick_closed();
        }
    }

    fn refresh_config(&mut self) {
        if self.cfg_at.elapsed() < CONFIG_REFRESH {
            return;
        }
        self.cfg = state::load_config();
        self.cfg_at = Instant::now();
    }

    fn check_panel(&mut self, force: bool) {
        if !force && self.panel_checked.elapsed() < PANEL_CHECK {
            return;
        }
        self.panel_checked = Instant::now();
        self.panel_visible = self
            .app
            .get_webview_window(super::panel::LABEL)
            .map(|win| win.is_visible().unwrap_or(false))
            .unwrap_or(false);
    }

    // ------------------------------------------------------- fan is visible

    fn tick_open(&mut self) {
        let now = Instant::now();

        // The fan and the panel never share the screen: the panel is a
        // focus-taking webview, the fan is a no-activate popup.
        if !self.cfg.quick_menu.enabled {
            self.collapse("quick menu disabled");
            return;
        }
        if self.panel_visible {
            self.collapse("panel opened");
            return;
        }
        if ball::dragging() {
            self.collapse("ball dragging");
            return;
        }
        let Some((bl, bt, br, bb, _)) = ball::ball_rect() else {
            self.collapse("ball gone");
            return;
        };
        if !ball_window_visible() {
            self.collapse("ball hidden");
            return;
        }
        if rect_moved(self.ball_show, (bl, bt, br, bb)) > 2 {
            // The ball slid back to its peek sliver (or was repositioned):
            // the fan would be anchored to a position the ball no longer has.
            self.collapse("ball moved");
            return;
        }

        // The pointer leaves the ball window as soon as it travels to a chip,
        // and the docked ball would then tuck itself back into the peek sliver
        // ~220 ms later, underneath the fan. Keep it slid out while the fan is
        // up (no-op while floating; the ball's own hover logic stays in charge
        // of the visual state).
        ball::expand();

        let Some(cursor) = cursor_pos() else {
            return;
        };
        let over_fan = self.hit.map(|hit| hit.contains(cursor.x, cursor.y)).unwrap_or(false);
        if over_fan {
            self.leave_at = None;
            let hover = if self.win_w > 0 && self.win_h > 0 {
                self.chip_at(cursor.x - self.win_x, cursor.y - self.win_y)
            } else {
                None
            };
            if hover != self.hover {
                self.set_hover(hover);
            }
        } else {
            // Delayed collapse: crossing the transparent gap between the ball
            // and the chips is not "leaving the fan".
            match self.leave_at {
                None => self.leave_at = Some(now),
                Some(since) if now.duration_since(since) >= HIDE_DELAY => {
                    self.collapse("pointer left");
                    return;
                }
                Some(_) => {}
            }
        }

        // A press whose release never arrived (the pointer was released over
        // another window) must not stay a click candidate forever.
        if self.pressed.is_some() && now.duration_since(self.pressed_at) > PRESS_STALE {
            self.pressed = None;
        }
    }

    // ------------------------------------------------------ fan is not shown

    fn tick_closed(&mut self) {
        let now = Instant::now();

        // Pull in an explicit request from the ball's hover handler, if any.
        if let Ok(mut slot) = REQUEST.lock() {
            if let Some(req) = slot.take() {
                self.requested = Some(req);
            }
        }

        if !self.cfg.quick_menu.enabled || self.panel_visible || ball::dragging() {
            self.reset_dwell();
            self.requested = None;
            self.last_ball = ball::ball_rect().map(|(l, t, r, b, _)| (l, t, r, b));
            return;
        }
        let Some((bl, bt, br, bb, _)) = ball::ball_rect() else {
            self.reset_dwell();
            self.requested = None;
            return;
        };
        if !ball_window_visible() {
            self.reset_dwell();
            self.requested = None;
            return;
        }
        let rect = (bl, bt, br, bb);

        // Fast movement of the ball rect == dragging / sliding / repositioning.
        // During a drag the fan must not appear at all; during the hover slide
        // the dwell restarts, and the fan only opens once the rect has settled.
        let moved = self
            .last_ball
            .map(|prev| rect_moved(prev, rect) > 1)
            .unwrap_or(true);
        if moved {
            self.last_ball_change = now;
        }
        self.last_ball = Some(rect);

        let Some(cursor) = cursor_pos() else {
            self.reset_dwell();
            return;
        };
        let (cx, cy) = rect_center(rect);
        let over_ball = point_near(cursor, cx, cy, BALL_CIRCLE_RADIUS + 4.0);
        if !over_ball {
            self.reset_dwell();
            self.requested = None;
            return;
        }
        if self.dwell_enter.is_none() {
            self.dwell_enter = Some(now);
        }

        // A request from the ball already decided "pointer rested on the ball";
        // the watcher's own trigger is the dwell timer.
        let requested = self.requested.is_some();
        let dwell_done = self
            .dwell_enter
            .map(|since| now.duration_since(since) >= DWELL)
            .unwrap_or(false);
        if !requested && !dwell_done {
            return;
        }
        if now.duration_since(self.last_ball_change) < SETTLE {
            return;
        }

        let anchor = ball::anchor_kind().unwrap_or(Anchor::Floating);
        // A docked ball that is still tucked into its peek sliver would give
        // the fan an off-screen centre: ask the ball to slide out and try again
        // on a later tick (the dwell state is kept, so no extra 200 ms).
        if !ball_slid_out(anchor, rect) {
            // Sliding the ball out moves its window rect, so `moved` is true on
            // the next tick and the SETTLE wait restarts. Keep the dwell anchor
            // so the user does not have to wait another full DWELL after the
            // slide; only the settle time is added.
            ball::expand();
            return;
        }

        let cfg = match self.requested.take() {
            Some((cfg, _anchor)) => cfg,
            None => self.cfg.quick_menu.clone(),
        };
        self.dwell_enter = None;
        self.show(&cfg, anchor, rect);
    }

    fn reset_dwell(&mut self) {
        self.dwell_enter = None;
    }

    // -------------------------------------------------------------- showing

    fn show(&mut self, config: &QuickMenuConfig, anchor: Anchor, ball_rect: (i32, i32, i32, i32)) {
        self.check_panel(true);
        if !config.enabled || self.panel_visible || ball::dragging() {
            return;
        }
        let items = resolve_items(config, anchor);
        if items.is_empty() {
            crate::logging::write("info", "quickmenu", "no enabled tools to show");
            return;
        }

        let scale = dpi_scale();
        let m = Metrics::new(scale);
        let layout = fan_layout(anchor, items.len(), config.rings, &m);
        let count = layout.chips.len();
        if count == 0 {
            return;
        }

        // The ball's window centre is its circle centre. For a docked ball this
        // is the fully slid-out rect (tick_closed waits for it), so the fan is
        // always laid out around the visible ball, never around the peek sliver.
        let (bcx, bcy) = rect_center(ball_rect);
        let (wx, wy, ww, wh) = window_for_layout(&layout, (bcx as f32, bcy as f32), &m);

        let chip_px = m.chip().round() as i32;
        let glow_px = m.glow().round() as i32;
        let icon_px = m.icon().round().clamp(8.0, 512.0) as u32;
        let mut chips = Vec::with_capacity(count);
        for (index, item) in items.iter().take(count).enumerate() {
            let (dx, dy) = layout.chips[index];
            let icon = icons::tool_icon_or_fallback(&item.tool_id, icon_px);
            let mut chip = Chip {
                tool_id: item.tool_id.clone(),
                name: item.name.clone(),
                accent: icons::parse_hex(&item.accent),
                icon,
                scale,
                chip_px,
                glow_px,
                chip_radius: (CHIP_RADIUS * scale).min(chip_px as f32 / 2.0 - 1.0),
                cx: bcx as f32 + dx - wx as f32,
                cy: bcy as f32 + dy - wy as f32,
                idle: Canvas::new(1, 1),
                hover: None,
            };
            chip.idle = render_chip(&chip, false);
            chips.push(chip);
        }

        self.chips = chips;
        self.hover = None;
        self.leave_at = None;
        self.pressed = None;
        self.tracking = false;
        self.anchor = anchor;
        self.ball_show = ball_rect;
        self.win_x = wx;
        self.win_y = wy;
        self.win_w = ww;
        self.win_h = wh;

        let hit = HitArea {
            cx: bcx,
            cy: bcy,
            reach: layout.reach,
            margin: m.margin() + 6.0 * scale,
            anchor,
        };
        self.hit = Some(hit);

        // Position first, then push the pixels, then show without activating:
        // UpdateLayeredWindow on a hidden window is fine and avoids a frame of
        // garbage appearing on screen.
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                wx,
                wy,
                ww.max(1),
                wh.max(1),
                SWP_NOACTIVATE,
            );
        }
        self.push_frame();
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }

        self.open = true;
        OPEN.store(true, Ordering::SeqCst);
        if let Ok(mut slot) = HIT.lock() {
            *slot = Some(hit);
        }
        crate::logging::write(
            "info",
            "quickmenu",
            &format!(
                "show anchor={anchor:?} chips={count} scale={scale:.2} ball_rect={ball_rect:?} \
                 window=({wx},{wy} {ww}x{wh})"
            ),
        );
    }

    fn collapse(&mut self, reason: &str) {
        if !self.open {
            return;
        }
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        self.open = false;
        OPEN.store(false, Ordering::SeqCst);
        if let Ok(mut slot) = HIT.lock() {
            *slot = None;
        }
        self.chips.clear();
        self.hover = None;
        self.leave_at = None;
        self.pressed = None;
        self.hit = None;
        self.tracking = false;

        // Release a docked ball back to its peek sliver, unless something else
        // keeps it out: an open panel, an ongoing drag, or the pointer itself
        // sitting on the ball.
        if self.anchor != Anchor::Floating && !self.panel_visible && !ball::dragging() {
            if !cursor_on_ball() {
                ball::collapse();
            }
        }
        crate::logging::write("info", "quickmenu", &format!("hide: {reason}"));
    }

    // ------------------------------------------------------- hover / clicks

    fn set_hover(&mut self, hover: Option<usize>) {
        if self.hover == hover {
            return;
        }
        self.hover = hover;
        if let Some(index) = hover {
            if let Some(chip) = self.chips.get_mut(index) {
                chip.ensure_hover();
            }
        }
        self.push_frame();
    }

    fn chip_at(&self, x: i32, y: i32) -> Option<usize> {
        let half = self
            .chips
            .first()
            .map(|chip| chip.chip_px as f32 / 2.0 + 2.0)
            .unwrap_or(CHIP_SIZE / 2.0);
        self.chips
            .iter()
            .position(|chip| (x as f32 - chip.cx).abs() <= half && (y as f32 - chip.cy).abs() <= half)
    }

    /// Re-composite all chip sprites into a frame and push it into the layered
    /// window. Cost is bounded: one small blit per chip plus the DIB copy.
    fn push_frame(&self) {
        if self.win_w <= 0 || self.win_h <= 0 {
            return;
        }
        let mut frame = Canvas::new(self.win_w, self.win_h);
        for (index, chip) in self.chips.iter().enumerate() {
            let sprite = if self.hover == Some(index) {
                chip.hover.as_ref().unwrap_or(&chip.idle)
            } else {
                &chip.idle
            };
            let x = (chip.cx - sprite.w as f32 / 2.0).round() as i32;
            let y = (chip.cy - sprite.h as f32 / 2.0).round() as i32;
            frame.draw_canvas(sprite, x, y);
        }
        let pixels = frame.to_bgra_premul();
        push_layered(
            self.hwnd,
            self.win_x,
            self.win_y,
            self.win_w,
            self.win_h,
            &pixels,
        );
    }
}

// --------------------------------------------------------------- wndproc

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let result = match msg {
        WM_PAINT => {
            // Everything is painted through UpdateLayeredWindow; this only
            // satisfies the window manager's paint validation.
            let mut ps = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut ps);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            // The tick polls the cursor anyway; this just registers for the
            // leave notification once per hover session.
            MENU.with(|slot| {
                if let Some(menu) = slot.borrow_mut().as_mut() {
                    if !menu.tracking {
                        menu.tracking = true;
                        let mut tme = TRACKMOUSEEVENT {
                            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: hwnd,
                            dwHoverTime: 0,
                        };
                        let _ = TrackMouseEvent(&mut tme);
                    }
                }
            });
            LRESULT(0)
        }
        MSG_MOUSELEAVE => {
            MENU.with(|slot| {
                if let Some(menu) = slot.borrow_mut().as_mut() {
                    menu.tracking = false;
                    // Clear the highlight right away instead of waiting for the
                    // next tick; the collapse decision stays in the tick.
                    if menu.hover.is_some() {
                        menu.set_hover(None);
                    }
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            MENU.with(|slot| {
                if let Some(menu) = slot.borrow_mut().as_mut() {
                    menu.press();
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            MENU.with(|slot| {
                if let Some(menu) = slot.borrow_mut().as_mut() {
                    menu.release();
                }
            });
            LRESULT(0)
        }
        WM_RBUTTONDOWN | WM_RBUTTONUP => LRESULT(0),
        WM_DESTROY => {
            FAN_HWND.store(0, Ordering::SeqCst);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    };
    let _ = (wparam, lparam);
    result
}

impl Menu {
    /// Remember which chip is under the pointer on press (click candidate).
    fn press(&mut self) {
        let Some(cursor) = cursor_pos() else {
            self.pressed = None;
            return;
        };
        self.pressed = self.chip_at(cursor.x - self.win_x, cursor.y - self.win_y);
        self.pressed_at = Instant::now();
    }

    /// A release directly on the pressed chip is a click; anything else (drag,
    /// release over the desktop, a paused press) is discarded.
    fn release(&mut self) {
        let Some(index) = self.pressed.take() else {
            return;
        };
        if self.pressed_at.elapsed() > CLICK_TIMEOUT {
            return;
        }
        let Some(cursor) = cursor_pos() else {
            return;
        };
        if self.chip_at(cursor.x - self.win_x, cursor.y - self.win_y) != Some(index) {
            return;
        }
        let Some(chip) = self.chips.get(index) else {
            return;
        };
        self.pending_open = Some((chip.tool_id.clone(), chip.name.clone()));
        self.hover = None;
    }
}

fn open_tool(app: &AppHandle, tool_id: &str, name: &str) {
    match super::tools::open(app, tool_id) {
        Ok(()) => crate::logging::write(
            "info",
            "quickmenu",
            &format!("open tool {tool_id} ({name})"),
        ),
        Err(e) => crate::logging::write(
            "error",
            "quickmenu",
            &format!("open tool {tool_id} failed: {e}"),
        ),
    }
}

// -------------------------------------------------------------- platform help

fn cursor_pos() -> Option<POINT> {
    let mut point = POINT::default();
    unsafe {
        GetCursorPos(&mut point).ok()?;
    }
    Some(point)
}

fn rect_center(rect: (i32, i32, i32, i32)) -> (i32, i32) {
    (
        (rect.0 + rect.2) / 2,
        (rect.1 + rect.3) / 2,
    )
}

/// Largest per-side difference between two rects.
fn rect_moved(a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)) -> i32 {
    (a.0 - b.0)
        .abs()
        .max((a.1 - b.1).abs())
        .max((a.2 - b.2).abs())
        .max((a.3 - b.3).abs())
}

fn point_near(point: POINT, cx: i32, cy: i32, radius: f32) -> bool {
    let dx = (point.x - cx) as f32;
    let dy = (point.y - cy) as f32;
    dx * dx + dy * dy <= radius * radius
}

fn cursor_on_ball() -> bool {
    let Some((bl, bt, br, bb, _)) = ball::ball_rect() else {
        return false;
    };
    let Some(cursor) = cursor_pos() else {
        return false;
    };
    let (cx, cy) = rect_center((bl, bt, br, bb));
    point_near(cursor, cx, cy, BALL_CIRCLE_RADIUS + 4.0)
}

fn dpi_scale() -> f32 {
    // The ball lives on the monitor the fan opens on; its DPI is exactly the
    // scale the chips and their rasterised icons need to be crisp there.
    let dpi = ball::dpi().max(96);
    (dpi as f32 / 96.0).clamp(1.0, 2.5)
}

/// Work area of the monitor under `point`, physical pixels.
fn work_area_at(point: POINT) -> windows::Win32::Foundation::RECT {
    unsafe {
        let monitor: HMONITOR = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            info.rcWork
        } else {
            windows::Win32::Foundation::RECT {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            }
        }
    }
}

/// Is the ball fully slid out of its docked peek sliver?
///
/// A collapsed docked ball's 64x64 window deliberately extends past the work
/// area edge (ball.rs slides it out so `PEEK_VISIBLE` px of the circle show), so
/// the rect alone cannot be centred. Expanded, the window is flush with the edge
/// it is docked to. Floating balls are always fully visible.
fn ball_slid_out(anchor: Anchor, rect: (i32, i32, i32, i32)) -> bool {
    match anchor {
        Anchor::Floating => true,
        Anchor::RightEdge => {
            let area = work_area_at(POINT {
                x: rect_center(rect).0,
                y: rect_center(rect).1,
            });
            rect.2 >= area.right - 2
        }
        Anchor::LeftEdge => {
            let area = work_area_at(POINT {
                x: rect_center(rect).0,
                y: rect_center(rect).1,
            });
            rect.0 <= area.left + 2
        }
    }
}

/// The ball window's HWND. `ball_rect()` returns the rect but not the handle;
/// the class name is registered in ball.rs. Cached and revalidated.
fn ball_window_visible() -> bool {
    static CACHE: AtomicIsize = AtomicIsize::new(0);
    let raw = CACHE.load(Ordering::SeqCst);
    if raw != 0 {
        let hwnd = HWND(raw as *mut _);
        if unsafe { IsWindow(Some(hwnd)).as_bool() } {
            return unsafe { IsWindowVisible(hwnd).as_bool() };
        }
        CACHE.store(0, Ordering::SeqCst);
    }
    let found = unsafe { FindWindowW(BALL_CLASS, PCWSTR::null()) };
    match found {
        Ok(hwnd) => {
            CACHE.store(hwnd.0 as isize, Ordering::SeqCst);
            unsafe { IsWindowVisible(hwnd).as_bool() }
        }
        // No ball window: the feature is simply not available, not "hidden".
        Err(_) => false,
    }
}

// ------------------------------------------------------------------ layout
//
// Angles are measured in screen coordinates: 0 rad points right, +PI/2 points
// down (screen y grows downwards). That makes the fan side a simple range:
//   docked right (fan opens left)  -> PI/2 .. 3PI/2   (90 .. 270 deg)
//   docked left  (fan opens right) -> -PI/2 .. PI/2   (-90 .. 90 deg)
//   floating (full circle)         -> -PI/2 .. 3PI/2  (sweep 2PI)

/// Physical-pixel measurements for the current monitor scale.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Metrics {
    scale: f32,
}

impl Metrics {
    fn new(scale: f32) -> Self {
        Self { scale }
    }
    fn chip(&self) -> f32 {
        CHIP_SIZE * self.scale
    }
    fn icon(&self) -> f32 {
        CHIP_ICON * self.scale
    }
    fn glow(&self) -> f32 {
        CHIP_GLOW * self.scale
    }
    fn min_distance(&self) -> f32 {
        (CHIP_SIZE + CHIP_MIN_GAP) * self.scale
    }
    fn ring_gap(&self) -> f32 {
        CHIP_RING_GAP * self.scale
    }
    /// Distance from the ball centre to the nearest allowed chip centre: leaves
    /// the ball itself fully clear and gives the first ring a visible gap.
    fn min_radius(&self) -> f32 {
        BALL_CIRCLE_RADIUS + CHIP_RING_GAP * self.scale + self.chip() / 2.0
    }
    /// Half a chip plus the sprite glow: the window margin around the chips.
    fn margin(&self) -> f32 {
        self.chip() / 2.0 + self.glow()
    }
}

/// Chip centres relative to the ball centre (physical px) + the outer reach used
/// for keep-alive hit testing.
#[derive(Debug)]
struct FanLayout {
    chips: Vec<(f32, f32)>,
    reach: f32,
}

/// How many chips a single ring holds: a 180 deg fan fits five, a full circle
/// eight. Two rings are only used when the count exceeds this.
fn ring_capacity(anchor: Anchor) -> usize {
    match anchor {
        Anchor::Floating => 8,
        Anchor::LeftEdge | Anchor::RightEdge => 5,
    }
}

/// `(start, end, full_circle)` of the fan arc, radians.
///
/// Slot order is the same for every anchor: the first configured tool is the
/// chip nearest the top of the screen, the last one sits at the bottom (for a
/// floating ball the order runs clockwise from the top). A right-edge fan is
/// therefore the true mirror image of a left-edge fan, not a flipped one.
fn arc(anchor: Anchor) -> (f32, f32, bool) {
    use std::f32::consts::{FRAC_PI_2, PI};
    match anchor {
        Anchor::RightEdge => (PI * 1.5, FRAC_PI_2, false),
        Anchor::LeftEdge => (-FRAC_PI_2, FRAC_PI_2, false),
        Anchor::Floating => (-FRAC_PI_2, PI * 1.5, true),
    }
}

/// How many chips are drawn for `count` icons: a single ring is capped at its
/// capacity (extra icons simply do not fit; the settings page hints at it), two
/// rings are only used when the count overflows the first one.
fn effective_count(count: usize, rings: usize, anchor: Anchor) -> usize {
    let cap = ring_capacity(anchor);
    let count = count.min(state::QUICK_MENU_MAX_ICONS);
    if count <= cap || rings >= 2 {
        count
    } else {
        cap
    }
}

/// Radius needed so that `n` evenly spread chips of this ring keep at least
/// [`CHIP_MIN_GAP`] between them, but never closer than `min_radius` to the ball.
fn ring_radius(n: usize, anchor: Anchor, m: &Metrics, min_radius: f32) -> f32 {
    if n <= 1 {
        return min_radius;
    }
    let (start, end, full) = arc(anchor);
    let pitch = if full {
        std::f32::consts::TAU / n as f32
    } else {
        (end - start).abs() / (n as f32 - 1.0)
    };
    // chord between neighbours = 2R sin(pitch/2); solve for R.
    let required = m.min_distance() / (2.0 * (pitch / 2.0).sin());
    required.max(min_radius)
}

fn ring_positions(n: usize, anchor: Anchor, radius: f32, out: &mut Vec<(f32, f32)>) {
    let (start, end, full) = arc(anchor);
    match n {
        0 => {}
        1 => {
            // A lone chip sits in the middle of the opening, or on top for a
            // floating ball (symmetric and out of the way of the pointer).
            let angle = if full {
                -std::f32::consts::FRAC_PI_2
            } else {
                (start + end) / 2.0
            };
            out.push((radius * angle.cos(), radius * angle.sin()));
        }
        _ if full => {
            for index in 0..n {
                let angle = start + index as f32 * std::f32::consts::TAU / n as f32;
                out.push((radius * angle.cos(), radius * angle.sin()));
            }
        }
        _ => {
            for index in 0..n {
                let angle = start + (end - start) * index as f32 / (n as f32 - 1.0);
                out.push((radius * angle.cos(), radius * angle.sin()));
            }
        }
    }
}

/// Lay the fan out around a ball centre at `(0, 0)`.
fn fan_layout(anchor: Anchor, count: usize, rings: usize, m: &Metrics) -> FanLayout {
    let count = effective_count(count, rings, anchor);
    let cap = ring_capacity(anchor);
    let mut chips = Vec::with_capacity(count);
    let (reach_radius, _) = if count <= cap {
        let n = count.min(cap);
        let radius = ring_radius(n, anchor, m, m.min_radius());
        ring_positions(n, anchor, radius, &mut chips);
        (radius, ())
    } else {
        // Inner ring filled first, remainder on the second ring.
        let inner = cap;
        let outer = count - cap;
        let r1 = ring_radius(inner, anchor, m, m.min_radius());
        let min_outer = r1 + m.chip() + m.ring_gap();
        let r2 = ring_radius(outer, anchor, m, min_outer);
        ring_positions(inner, anchor, r1, &mut chips);
        ring_positions(outer, anchor, r2, &mut chips);
        (r2, ())
    };
    FanLayout {
        chips,
        reach: reach_radius + m.margin(),
    }
}

/// Window rect (physical px) that holds every chip sprite, centred on `center`.
fn window_for_layout(
    layout: &FanLayout,
    center: (f32, f32),
    m: &Metrics,
) -> (i32, i32, i32, i32) {
    let half = m.margin();
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    for (dx, dy) in &layout.chips {
        let x = center.0 + dx;
        let y = center.1 + dy;
        min_x = min_x.min(x - half);
        min_y = min_y.min(y - half);
        max_x = max_x.max(x + half);
        max_y = max_y.max(y + half);
    }
    if min_x > max_x {
        return (0, 0, 1, 1);
    }
    let x = min_x.floor() as i32;
    let y = min_y.floor() as i32;
    let w = (max_x.ceil() as i32 - x).max(1);
    let h = (max_y.ceil() as i32 - y).max(1);
    (x, y, w, h)
}

// ---------------------------------------------------------------- rendering
//
// The same software rasteriser as ball.rs: straight-alpha RGBA canvas, 4x4
// supersampling for shapes, then premultiplied BGRA for UpdateLayeredWindow.
// Unlike ball.rs the canvas starts fully transparent (alpha 0): a layered window
// is hit-tested by alpha, and the transparent padding must be click-through.

struct Canvas {
    w: i32,
    h: i32,
    px: Vec<[f32; 4]>,
}

impl Canvas {
    fn new(w: i32, h: i32) -> Self {
        let w = w.max(1);
        let h = h.max(1);
        Self {
            w,
            h,
            px: vec![[0.0, 0.0, 0.0, 0.0]; (w * h) as usize],
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
        let index = (y * self.w + x) as usize;
        let dst = &mut self.px[index];
        let out_a = a + dst[3] * (1.0 - a);
        if out_a <= 0.0 {
            return;
        }
        for channel in 0..3 {
            dst[channel] = (color[channel] * a + dst[channel] * dst[3] * (1.0 - a)) / out_a;
        }
        dst[3] = out_a;
    }

    fn fill_disc(&mut self, cx: f32, cy: f32, radius: f32, color: [f32; 4]) {
        let x0 = (cx - radius - 1.0).floor().max(0.0) as i32;
        let x1 = (cx + radius + 1.0).ceil().min(self.w as f32) as i32;
        let y0 = (cy - radius - 1.0).floor().max(0.0) as i32;
        let y1 = (cy + radius + 1.0).ceil().min(self.h as f32) as i32;
        for y in y0..y1 {
            for x in x0..x1 {
                let mut hits = 0;
                for sy in 0..4 {
                    for sx in 0..4 {
                        let px = x as f32 + (sx as f32 + 0.5) / 4.0;
                        let py = y as f32 + (sy as f32 + 0.5) / 4.0;
                        let dx = px - cx;
                        let dy = py - cy;
                        if dx * dx + dy * dy <= radius * radius {
                            hits += 1;
                        }
                    }
                }
                let coverage = hits as f32 / 16.0;
                if coverage > 0.0 {
                    self.blend(x, y, color, coverage);
                }
            }
        }
    }

    fn fill_ring(&mut self, cx: f32, cy: f32, r_out: f32, width: f32, color: [f32; 4]) {
        let r_in = (r_out - width).max(0.0);
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
                let coverage = hits as f32 / 16.0;
                if coverage > 0.0 {
                    self.blend(x, y, color, coverage);
                }
            }
        }
    }

    fn fill_round_rect(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        color: [f32; 4],
    ) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
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
                        if inside_round_rect(fx, fy, x, y, w, h, radius) {
                            hits += 1;
                        }
                    }
                }
                let coverage = hits as f32 / 16.0;
                if coverage > 0.0 {
                    self.blend(px, py, color, coverage);
                }
            }
        }
    }

    /// Alpha-composite a sprite onto this canvas at `(ox, oy)`.
    fn draw_canvas(&mut self, src: &Canvas, ox: i32, oy: i32) {
        for y in 0..src.h {
            for x in 0..src.w {
                let color = src.px[(y * src.w + x) as usize];
                if color[3] > 0.0 {
                    self.blend(ox + x, oy + y, color, 1.0);
                }
            }
        }
    }

    /// Premultiplied BGRA, which is what UpdateLayeredWindow expects.
    fn to_bgra_premul(&self) -> Vec<u8> {
        let mut out = vec![0u8; (self.w * self.h * 4) as usize];
        for (index, pixel) in self.px.iter().enumerate() {
            let alpha = pixel[3].clamp(0.0, 1.0);
            let offset = index * 4;
            out[offset] = (pixel[2].clamp(0.0, 1.0) * alpha * 255.0).round() as u8;
            out[offset + 1] = (pixel[1].clamp(0.0, 1.0) * alpha * 255.0).round() as u8;
            out[offset + 2] = (pixel[0].clamp(0.0, 1.0) * alpha * 255.0).round() as u8;
            out[offset + 3] = (alpha * 255.0).round() as u8;
        }
        out
    }
}

fn inside_round_rect(px: f32, py: f32, x: f32, y: f32, w: f32, h: f32, radius: f32) -> bool {
    let radius = radius.min(w / 2.0).min(h / 2.0).max(0.0);
    let cx = px.clamp(x + radius, x + w - radius);
    let cy = py.clamp(y + radius, y + h - radius);
    let dx = px - cx;
    let dy = py - cy;
    dx * dx + dy * dy <= radius * radius
}

fn rgba(r: u8, g: u8, b: u8, a: f32) -> [f32; 4] {
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a]
}

/// Rasterise one chip. `hover` brightens the accent ring, adds the accent glow
/// and tints the plate, which is the whole hover affordance (no font, no image
/// assets: the icon comes from the manifest's SVG, the plate from this painter).
fn render_chip(chip: &Chip, hover: bool) -> Canvas {
    let scale = chip.scale;
    let chip_px = chip.chip_px as f32;
    let glow_px = chip.glow_px as f32;
    let size = chip.chip_px + chip.glow_px * 2;
    let mut canvas = Canvas::new(size, size);
    let cx = size as f32 / 2.0;
    let cy = cx;
    let half = chip_px / 2.0;
    let radius = chip.chip_radius;
    let accent = [
        chip.accent[0] as f32 / 255.0,
        chip.accent[1] as f32 / 255.0,
        chip.accent[2] as f32 / 255.0,
    ];

    // Accent glow that separates the chip from any desktop background.
    canvas.fill_disc(
        cx,
        cy,
        half + 5.0 * scale,
        [accent[0], accent[1], accent[2], if hover { 0.26 } else { 0.10 }],
    );
    if hover {
        canvas.fill_ring(cx, cy, half + 4.0 * scale, 2.0 * scale, [accent[0], accent[1], accent[2], 0.55]);
    }

    // Drop shadow: a dark rounded square nudged down, drawn before the plate.
    canvas.fill_round_rect(
        cx - half,
        cy - half + 1.8 * scale,
        chip_px,
        chip_px,
        radius,
        rgba(0, 0, 0, if hover { 0.40 } else { 0.32 }),
    );

    // Accent border: draw the full chip in the accent colour, then inset the
    // dark plate on top of it. Cheaper and sharper than stroking.
    let border = (if hover { 2.2 } else { 1.5 }) * scale;
    canvas.fill_round_rect(
        cx - half,
        cy - half,
        chip_px,
        chip_px,
        radius,
        [accent[0], accent[1], accent[2], if hover { 1.0 } else { 0.62 }],
    );
    let inner = chip_px - border * 2.0;
    let plate_radius = (radius - border).max(1.0);
    let plate = rgba(18, 21, 28, 0.96);
    canvas.fill_round_rect(
        cx - half + border,
        cy - half + border,
        inner,
        inner,
        plate_radius,
        plate,
    );
    if hover {
        canvas.fill_round_rect(
            cx - half + border,
            cy - half + border,
            inner,
            inner,
            plate_radius,
            [accent[0], accent[1], accent[2], 0.16],
        );
    }

    // The tool icon, straight-alpha, centred. It was rasterised at exactly the
    // physical pixel size, so no scaling filter is involved here.
    let icon_size = chip.icon.width as f32;
    let ox = ((size as f32 - icon_size) / 2.0).round() as i32;
    let oy = ((size as f32 - chip.icon.height as f32) / 2.0).round() as i32;
    for y in 0..chip.icon.height {
        for x in 0..chip.icon.width {
            let color = chip.icon.pixel(x, y);
            if color[3] > 0.0 {
                canvas.blend(ox + x as i32, oy + y as i32, color, 1.0);
            }
        }
    }
    let _ = glow_px;
    canvas
}

/// Paint `pixels` (premultiplied BGRA, `w*h*4` bytes) into the layered window at
/// `(x, y)` with the size `(w, h)`.
fn push_layered(hwnd: HWND, x: i32, y: i32, w: i32, h: i32, pixels: &[u8]) {
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bitmap_info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                // Negative height = top-down DIB, so row 0 is the top row.
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        if let Ok(dib) = CreateDIBSection(Some(mem), &bitmap_info, DIB_RGB_COLORS, &mut bits, None, 0)
        {
            let old = SelectObject(mem, HGDIOBJ(dib.0));
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u8, pixels.len());

            let destination = POINT { x, y };
            let source = POINT { x: 0, y: 0 };
            let size = SIZE { cx: w, cy: h };
            let blend = BLENDFUNCTION {
                BlendOp: 0,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: 1, // AC_SRC_ALPHA: per-pixel alpha
            };
            // NOTE (same pitfall as ball.rs): the DIB must still be selected into
            // `mem` here. Deselecting it first leaves the DC holding the default
            // 1x1 monochrome bitmap and UpdateLayeredWindow silently paints
            // nothing.
            let result = UpdateLayeredWindow(
                hwnd,
                Some(screen),
                Some(&destination),
                Some(&size),
                Some(mem),
                Some(&source),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
            if result.is_err() {
                crate::logging::write(
                    "error",
                    "quickmenu",
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

// -------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> Metrics {
        Metrics::new(1.0)
    }

    fn radius_of(chip: &(f32, f32)) -> f32 {
        (chip.0 * chip.0 + chip.1 * chip.1).sqrt()
    }

    #[test]
    fn semicircle_opens_away_from_the_docked_edge() {
        let layout = fan_layout(Anchor::RightEdge, 5, 1, &metrics());
        assert_eq!(layout.chips.len(), 5);
        // Dock right -> every chip sits left of the ball centre.
        assert!(layout.chips.iter().all(|chip| chip.0 <= 0.1));
        // Symmetric around the horizontal axis; slot 1 sits at the top and the
        // last slot at the bottom (same reading order as the left-edge fan).
        let y: Vec<f32> = layout.chips.iter().map(|chip| chip.1).collect();
        assert!((y[0] + y[4]).abs() < 0.01, "not symmetric: {y:?}");
        assert!((y[1] + y[3]).abs() < 0.01, "not symmetric: {y:?}");
        assert!(y[0] < 0.0 && y[4] > 0.0, "slot order not top-down: {y:?}");
        // Single ring: equal radius, pushed out by the chip gap requirement.
        let expected = (CHIP_SIZE + CHIP_MIN_GAP) / (2.0 * (std::f32::consts::PI / 8.0).sin());
        for chip in &layout.chips {
            assert!((radius_of(chip) - expected).abs() < 0.5, "{chip:?} vs {expected}");
        }
    }

    #[test]
    fn left_and_right_fans_are_mirror_images() {
        let left = fan_layout(Anchor::LeftEdge, 4, 1, &metrics());
        let right = fan_layout(Anchor::RightEdge, 4, 1, &metrics());
        assert_eq!(left.chips.len(), right.chips.len());
        for (l, r) in left.chips.iter().zip(right.chips.iter()) {
            assert!((l.0 + r.0).abs() < 0.01, "{l:?} vs {r:?}");
            assert!((l.1 - r.1).abs() < 0.01, "{l:?} vs {r:?}");
        }
        assert!(left.chips.iter().all(|chip| chip.0 >= -0.1));
    }

    #[test]
    fn floating_uses_a_full_circle() {
        let layout = fan_layout(Anchor::Floating, 8, 1, &metrics());
        assert_eq!(layout.chips.len(), 8);
        for chip in &layout.chips {
            let expected = (CHIP_SIZE + CHIP_MIN_GAP) / (2.0 * (std::f32::consts::PI / 8.0).sin());
            assert!((radius_of(chip) - expected).abs() < 0.5);
        }
        // Both halves of the circle are populated.
        assert!(layout.chips.iter().any(|chip| chip.0 > 0.0));
        assert!(layout.chips.iter().any(|chip| chip.0 < 0.0));
    }

    #[test]
    fn overflow_uses_two_rings_inner_first() {
        let layout = fan_layout(Anchor::Floating, 12, 2, &metrics());
        assert_eq!(layout.chips.len(), 12);
        let inner = ring_capacity(Anchor::Floating);
        let r_inner = radius_of(&layout.chips[0]);
        let r_outer = radius_of(&layout.chips[inner]);
        assert!(r_outer > r_inner + CHIP_SIZE, "{r_inner} -> {r_outer}");
        for chip in layout.chips.iter().take(inner) {
            assert!((radius_of(chip) - r_inner).abs() < 0.5);
        }
        for chip in layout.chips.iter().skip(inner) {
            assert!((radius_of(chip) - r_outer).abs() < 0.5);
        }
    }

    #[test]
    fn a_single_ring_caps_the_count_when_rings_are_disabled() {
        assert_eq!(effective_count(12, 1, Anchor::RightEdge), 5);
        assert_eq!(effective_count(12, 1, Anchor::Floating), 8);
        assert_eq!(effective_count(12, 2, Anchor::RightEdge), 12);
        assert_eq!(effective_count(3, 1, Anchor::Floating), 3);
    }

    #[test]
    fn small_counts_pull_the_ring_in_towards_the_ball() {
        let layout = fan_layout(Anchor::RightEdge, 1, 1, &metrics());
        assert_eq!(layout.chips.len(), 1);
        let min_radius = BALL_CIRCLE_RADIUS + CHIP_RING_GAP + CHIP_SIZE / 2.0;
        assert!((radius_of(&layout.chips[0]) - min_radius).abs() < 0.01);
        // A lone chip in a side fan sits in the middle of the opening (left).
        assert!(layout.chips[0].0 < -min_radius + 0.01);
    }

    #[test]
    fn the_window_covers_every_chip_sprite() {
        for (anchor, count, rings) in [
            (Anchor::RightEdge, 5, 1),
            (Anchor::RightEdge, 12, 2),
            (Anchor::LeftEdge, 12, 2),
            (Anchor::Floating, 12, 2),
            (Anchor::Floating, 1, 1),
        ] {
            let m = metrics();
            let layout = fan_layout(anchor, count, rings, &m);
            let center = (500.0, 400.0);
            let (wx, wy, ww, wh) = window_for_layout(&layout, center, &m);
            let half = m.margin();
            for (dx, dy) in &layout.chips {
                let x = center.0 + dx;
                let y = center.1 + dy;
                assert!(x - half >= wx as f32 - 1.0, "{anchor:?} left edge");
                assert!(y - half >= wy as f32 - 1.0, "{anchor:?} top edge");
                assert!(x + half <= (wx + ww) as f32 + 1.0, "{anchor:?} right edge");
                assert!(y + half <= (wy + wh) as f32 + 1.0, "{anchor:?} bottom edge");
            }
            assert!(ww > 0 && wh > 0);
        }
    }

    #[test]
    fn hit_area_covers_the_fan_side_only() {
        let m = metrics();
        let layout = fan_layout(Anchor::RightEdge, 5, 2, &m);
        let hit = HitArea {
            cx: 0,
            cy: 0,
            reach: layout.reach,
            margin: m.margin() + 6.0,
            anchor: Anchor::RightEdge,
        };
        // A chip and the gap in front of the ball are inside.
        let chip = layout.chips[0];
        assert!(hit.contains(chip.0 as i32, chip.1 as i32));
        assert!(hit.contains(-40, 0));
        assert!(hit.contains(0, 0));
        // The closed side (towards the docked edge) and the far desktop are not.
        assert!(!hit.contains(200, 0));
        assert!(!hit.contains(0, 400));
        // Floating keeps the whole disc.
        let floating = fan_layout(Anchor::Floating, 8, 1, &m);
        let hit = HitArea {
            cx: 0,
            cy: 0,
            reach: floating.reach,
            margin: m.margin() + 6.0,
            anchor: Anchor::Floating,
        };
        assert!(hit.contains(0, 60));
        assert!(hit.contains(60, 0));
        assert!(!hit.contains(0, 400));
    }

    #[test]
    fn dpi_scales_the_chips() {
        let m = Metrics::new(1.5);
        assert!((m.chip() - 66.0).abs() < 0.01);
        assert!((m.icon() - 42.0).abs() < 0.01);
        assert!((m.min_radius() - (BALL_CIRCLE_RADIUS + CHIP_RING_GAP * 1.5 + 33.0)).abs() < 0.01);
    }
}
