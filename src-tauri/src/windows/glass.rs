//! Frosted-glass backdrop for the search panel.
//!
//! CSS `backdrop-filter` cannot blur the desktop: the panel window is transparent,
//! so the "backdrop" inside the webview is empty and there is nothing to blur. The
//! blur has to come from the window compositor instead.
//!
//! On Windows that means `SetWindowCompositionAttribute(WCA_ACCENT_POLICY,
//! ACCENT_ENABLE_ACRYLICBLURBEHIND)` — the undocumented API every Electron app
//! uses for acrylic. Two details matter for this window:
//!
//! * the panel is a **normal** DWM-composited window (tao makes it transparent by
//!   calling `DwmEnableBlurBehindWindow` with an empty region, it does not create a
//!   layered window), which is exactly the case acrylic supports; and
//! * the blur covers the whole window rectangle, including the transparent corners
//!   that the panel's CSS rounds off — so the window also gets a matching rounded
//!   region, otherwise the acrylic would square off the corners.
//!
//! Everything here is best-effort: on a system where the API or the composition
//! mode is unavailable the panel simply stays translucent, which is the 0.1.0 look.

#![cfg(windows)]

use std::sync::atomic::{AtomicBool, Ordering};
use tauri::WebviewWindow;
use windows::core::{s, w};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{CreateRoundRectRgn, DeleteObject, SetWindowRgn};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

/// Panel corner radius in CSS pixels (must match `#panel` in panel.html).
const PANEL_RADIUS_CSS: f64 = 14.0;

#[repr(C)]
struct AccentPolicy {
    state: u32,
    flags: u32,
    gradient_color: u32,
    animation_id: u32,
}

#[repr(C)]
struct WindowCompositionAttribData {
    attrib: u32,
    data: *mut core::ffi::c_void,
    size: usize,
}

const WCA_ACCENT_POLICY: u32 = 19;
const ACCENT_ENABLE_ACRYLICBLURBEHIND: u32 = 4;
const ACCENT_ENABLE_BLURBEHIND: u32 = 3;
const ACCENT_FLAG_NONE: u32 = 0;

/// True once the panel really has a frosted backdrop. The page turns its
/// translucent surface on only then, so a machine without acrylic keeps the
/// fully opaque panel instead of a see-through one with no blur.
static PANEL_GLASS: AtomicBool = AtomicBool::new(false);

/// Whether the frosted surface should be used by `panel.html`.
pub fn panel_active() -> bool {
    PANEL_GLASS.load(Ordering::Relaxed)
}

type SetWindowCompositionAttribute =
    unsafe extern "system" fn(HWND, *mut WindowCompositionAttribData) -> i32;

fn set_window_composition_attribute() -> Option<SetWindowCompositionAttribute> {
    unsafe {
        let module = GetModuleHandleW(w!("user32.dll")).ok()?;
        let proc = GetProcAddress(module, s!("SetWindowCompositionAttribute"))?;
        Some(std::mem::transmute::<
            unsafe extern "system" fn() -> isize,
            SetWindowCompositionAttribute,
        >(proc))
    }
}

/// `0xAABBGGRR` — acrylic wants the tint the compositor blends over the blurred
/// backdrop, in the theme's surface colour.
fn tint(dark: bool) -> u32 {
    if dark {
        // #14161a at ~62 % alpha
        0x9E_1A_16_14
    } else {
        // #f4f5f7 at ~55 % alpha
        0x8C_F7_F5_F4
    }
}

/// Enable the frosted backdrop. Returns a short status for the log.
///
/// `TOOLBOX_PANEL_GLASS` overrides the behaviour while tuning/verifying:
/// `off` (no change at all), `accent` (blur without the rounded region) or
/// `full` (default).
pub fn apply(win: &WebviewWindow, dark: bool) -> String {
    let mode = std::env::var("TOOLBOX_PANEL_GLASS").unwrap_or_else(|_| "full".into());
    if mode == "off" {
        return "disabled".into();
    }
    let Ok(hwnd) = win.hwnd() else {
        return "no hwnd".into();
    };

    // 1. Round the window itself so the acrylic follows the panel's CSS corners.
    let radius = panel_radius_px(hwnd);
    let round = if mode == "accent" {
        "region skipped".to_string()
    } else {
        unsafe {
            let mut rect = RECT::default();
            if GetClientRect(hwnd, &mut rect).is_err() {
                "no client rect".to_string()
            } else {
                let rgn = CreateRoundRectRgn(
                    0,
                    0,
                    rect.right + 1,
                    rect.bottom + 1,
                    radius * 2,
                    radius * 2,
                );
                if rgn.is_invalid() {
                    "no region".to_string()
                } else if SetWindowRgn(hwnd, Some(rgn), true) == 0 {
                    let _ = DeleteObject(rgn.into());
                    "SetWindowRgn failed".to_string()
                } else {
                    // The region is owned by the system now; no DeleteObject.
                    "round ok".to_string()
                }
            }
        }
    };

    // 2. Ask the compositor for the blur.
    let Some(set_attribute) = set_window_composition_attribute() else {
        return format!("{round}; SetWindowCompositionAttribute unavailable");
    };
    let mut policy = AccentPolicy {
        state: ACCENT_ENABLE_ACRYLICBLURBEHIND,
        flags: ACCENT_FLAG_NONE,
        gradient_color: tint(dark),
        animation_id: 0,
    };
    let mut data = WindowCompositionAttribData {
        attrib: WCA_ACCENT_POLICY,
        data: &mut policy as *mut _ as *mut core::ffi::c_void,
        size: std::mem::size_of::<AccentPolicy>(),
    };
    let rc = unsafe { set_attribute(hwnd, &mut data) };
    if rc == 0 {
        // Acrylic needs composition; blur-behind is the older fallback.
        policy.state = ACCENT_ENABLE_BLURBEHIND;
        let rc2 = unsafe { set_attribute(hwnd, &mut data) };
        PANEL_GLASS.store(rc2 != 0, Ordering::Relaxed);
        return format!("{round}; acrylic failed, blur-behind rc={rc2}");
    }
    PANEL_GLASS.store(true, Ordering::Relaxed);
    format!("{round}; acrylic ok")
}

/// CSS radius → physical pixels for the window's current DPI.
fn panel_radius_px(hwnd: HWND) -> i32 {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let scale = if dpi == 0 { 1.0 } else { dpi as f64 / 96.0 };
    (PANEL_RADIUS_CSS * scale).round() as i32
}
