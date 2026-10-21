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
//!   * left click -> toggle the panel; drag -> move + snap to nearest edge
//!   * right click -> native context menu

#![cfg(windows)]

use std::cell::RefCell;
use std::sync::OnceLock;

use tauri::AppHandle;
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, EndPaint, GetDC,
    GetMonitorInfoW, MonitorFromWindow, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER,
    BLENDFUNCTION, BI_RGB, DIB_RGB_COLORS, HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::state;

const BALL_SIZE: i32 = 64;
const PEEK_VISIBLE: i32 = 14;
const PADDING: f32 = 9.0;
const TIMER_SLIDE: usize = 1;

/// `WM_MOUSELEAVE` is not re-exported by the windows crate, so define it here.
/// (Using an unknown identifier directly in a `match` arm would silently become
/// a catch-all binding and swallow every message below it.)
const MSG_MOUSELEAVE: u32 = 0x02A3;

thread_local! {
    static BALL: RefCell<Option<Ball>> = const { RefCell::new(None) };
}

static APP: OnceLock<AppHandle> = OnceLock::new();

fn app() -> AppHandle {
    APP.get().cloned().expect("ball app handle")
}

struct Ball {
    hwnd: HWND,
    expanded: bool,
    hovering: bool,
    pressed: bool,
    dragging: bool,
    drag_origin: POINT,
    win_origin: POINT,
    side_right: bool,
    y_frac: f64,
    slide_to: i32,
    slide_step: i32,
}

#[derive(Clone, Copy, PartialEq)]
enum Face {
    Idle,
    Hover,
}

pub fn init(handle: &AppHandle) -> Result<(), String> {
    let _ = APP.set(handle.clone());
    let cfg = state::load_config();

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

        let side_right = cfg.ball_side != "left";
        let y_frac = cfg.ball_y.clamp(0.0, 1.0);

        BALL.with(|b| {
            *b.borrow_mut() = Some(Ball {
                hwnd,
                expanded: false,
                hovering: false,
                pressed: false,
                dragging: false,
                drag_origin: POINT::default(),
                win_origin: POINT::default(),
                side_right,
                y_frac,
                slide_to: 0,
                slide_step: 0,
            });
        });

        place(hwnd, side_right, y_frac, false);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        render(hwnd, Face::Idle);
    }
    Ok(())
}

// ---------------------------------------------------------------- geometry

fn work_area(hwnd: HWND) -> RECT {
    unsafe {
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        if GetMonitorInfoW(mon, &mut info).as_bool() {
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

fn place(hwnd: HWND, side_right: bool, y_frac: f64, expanded: bool) {
    unsafe {
        let rect = work_area(hwnd);
        let usable = (rect.bottom - rect.top - BALL_SIZE).max(1) as f64;
        let y = rect.top + (usable * y_frac) as i32;
        let x = target_x(&rect, side_right, expanded);
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

fn slide(hwnd: HWND, expand: bool) {
    let (side, frac) = BALL.with(|b| {
        let mut g = b.borrow_mut();
        let Some(ball) = g.as_mut() else {
            return (true, 0.5)
        };
        if ball.expanded == expand {
            return (ball.side_right, ball.y_frac);
        }
        ball.expanded = expand;
        (ball.side_right, ball.y_frac)
    });
    unsafe {
        let rect = work_area(hwnd);
        let mut wr = RECT::default();
        let _ = GetWindowRect(hwnd, &mut wr);
        let to = target_x(&rect, side, expand);
        let dist = (to - wr.left).abs();
        BALL.with(|b| {
            if let Some(ball) = b.borrow_mut().as_mut() {
                ball.slide_to = to;
                ball.slide_step = ((dist / 4).max(6)) * if to > wr.left { 1 } else { -1 };
            }
        });
        if dist < 6 {
            place(hwnd, side, frac, expand);
            return;
        }
        SetTimer(Some(hwnd), TIMER_SLIDE, 12, None);
    }
}

fn tick_slide(hwnd: HWND) {
    let mut stop = false;
    BALL.with(|b| {
        let g = b.borrow();
        let Some(ball) = g.as_ref() else { return };
        unsafe {
            let mut wr = RECT::default();
            let _ = GetWindowRect(hwnd, &mut wr);
            let next = wr.left + ball.slide_step;
            stop = if ball.slide_step > 0 {
                next >= ball.slide_to
            } else {
                next <= ball.slide_to
            };
            if !stop {
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
        }
    });
    if stop {
        let (side, frac, expanded) = BALL.with(|b| {
            let g = b.borrow();
            let ball = g.as_ref();
            (
                ball.map(|x| x.side_right).unwrap_or(true),
                ball.map(|x| x.y_frac).unwrap_or(0.5),
                ball.map(|x| x.expanded).unwrap_or(false),
            )
        });
        unsafe {
            let _ = KillTimer(Some(hwnd), TIMER_SLIDE);
        }
        place(hwnd, side, frac, expanded);
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
        Self {
            w,
            h,
            px: vec![[0.0; 4]; (w * h) as usize],
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
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut ps);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let mut start_hover = false;
            let mut dragging = false;
            let (dx, dy) = BALL.with(|b| {
                let mut g = b.borrow_mut();
                let Some(ball) = g.as_mut() else { return (0, 0) };
                if ball.dragging {
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    return (pt.x - ball.drag_origin.x, pt.y - ball.drag_origin.y);
                }
                if !ball.hovering {
                    ball.hovering = true;
                    start_hover = true;
                }
                (0, 0)
            });
            if start_hover {
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut tme);
                render(hwnd, Face::Hover);
                slide(hwnd, true);
            }
            BALL.with(|b| {
                if let Some(ball) = b.borrow().as_ref() {
                    dragging = ball.dragging;
                }
            });
            if dragging {
                let (ox, oy) = BALL.with(|b| {
                    let g = b.borrow();
                    let ball = g.as_ref();
                    (
                        ball.map(|x| x.win_origin.x).unwrap_or(0),
                        ball.map(|x| x.win_origin.y).unwrap_or(0),
                    )
                });
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    ox + dx,
                    oy + dy,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
            LRESULT(0)
        }
        MSG_MOUSELEAVE => {
            let mut collapse = false;
            BALL.with(|b| {
                let mut g = b.borrow_mut();
                if let Some(ball) = g.as_mut() {
                    ball.hovering = false;
                    if !ball.dragging && !ball.pressed {
                        collapse = ball.expanded;
                    }
                }
            });
            render(hwnd, Face::Idle);
            if collapse {
                slide(hwnd, false);
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
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
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let mut clicked = false;
            BALL.with(|b| {
                let mut g = b.borrow_mut();
                if let Some(ball) = g.as_mut() {
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    let moved = (pt.x - ball.drag_origin.x).abs() > 4
                        || (pt.y - ball.drag_origin.y).abs() > 4;
                    ball.pressed = false;
                    ball.dragging = false;
                    if moved {
                        let rect = work_area(hwnd);
                        let mut wr = RECT::default();
                        let _ = GetWindowRect(hwnd, &mut wr);
                        let mid = (rect.left + rect.right) / 2;
                        let side = wr.left + BALL_SIZE / 2 > mid;
                        let usable = (rect.bottom - rect.top - BALL_SIZE).max(1) as f64;
                        let frac = ((wr.top - rect.top) as f64 / usable).clamp(0.0, 1.0);
                        ball.side_right = side;
                        ball.y_frac = frac;
                        ball.expanded = false;
                        let mut cfg = state::load_config();
                        cfg.ball_side = if side { "right" } else { "left" }.into();
                        cfg.ball_y = frac;
                        let _ = state::save_config(&cfg);
                        place(hwnd, side, frac, false);
                    } else {
                        clicked = true;
                    }
                }
            });
            if clicked {
                crate::logging::write("info", "ball", "clicked -> toggle panel");
                crate::windows::panel::toggle(&app());
            }
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == TIMER_SLIDE {
                tick_slide(hwnd);
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            show_menu(hwnd);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
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

pub fn toggle_visible(_app: &AppHandle) {
    BALL.with(|b| {
        let g = b.borrow();
        let Some(ball) = g.as_ref() else { return };
        unsafe {
            if IsWindowVisible(ball.hwnd).as_bool() {
                let _ = ShowWindow(ball.hwnd, SW_HIDE);
            } else {
                let _ = ShowWindow(ball.hwnd, SW_SHOWNOACTIVATE);
                render(ball.hwnd, Face::Idle);
            }
        }
    });
}

pub fn reposition(_app: &AppHandle) {
    let cfg = state::load_config();
    BALL.with(|b| {
        let mut g = b.borrow_mut();
        let Some(ball) = g.as_mut() else { return };
        ball.side_right = cfg.ball_side != "left";
        ball.y_frac = cfg.ball_y;
        ball.expanded = false;
        place(ball.hwnd, ball.side_right, ball.y_frac, false);
        render(ball.hwnd, Face::Idle);
    });
}

/// Screen rect of the ball: (left, top, right, bottom, docked_right).
pub fn ball_rect() -> Option<(i32, i32, i32, i32, bool)> {
    BALL.with(|b| {
        let g = b.borrow();
        let ball = g.as_ref()?;
        unsafe {
            let mut wr = RECT::default();
            GetWindowRect(ball.hwnd, &mut wr).ok()?;
            Some((wr.left, wr.top, wr.right, wr.bottom, ball.side_right))
        }
    })
}

pub fn expand() {
    let hwnd = BALL.with(|b| b.borrow().as_ref().map(|x| x.hwnd));
    if let Some(h) = hwnd {
        slide(h, true);
    }
}

pub fn collapse() {
    let hwnd = BALL.with(|b| b.borrow().as_ref().map(|x| x.hwnd));
    if let Some(h) = hwnd {
        slide(h, false);
    }
}

/// DPI of the monitor the ball currently sits on.
pub fn dpi() -> u32 {
    BALL.with(|b| {
        let g = b.borrow();
        let Some(ball) = g.as_ref() else { return 96 };
        unsafe { GetDpiForWindow(ball.hwnd) }
    })
}
