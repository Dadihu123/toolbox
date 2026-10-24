//! Environment service: the effective theme and display language.
//!
//! Two layers, deliberately kept apart:
//!
//! * **Resolution** — `Config.theme` / `Config.locale` hold `"system"` or an
//!   explicit choice. `system` re-reads the OS every call, so the resolution is
//!   re-entrant: the same code path serves startup, the settings page and a
//!   `WM_SETTINGCHANGE` arriving at any later moment.
//! * **Application** — every webview is driven from here:
//!   - the native title bar through `Window::set_theme` (DWM immersive dark mode),
//!   - `prefers-color-scheme` through `ICoreWebView2Profile::SetPreferredColorScheme`.
//!
//! The WebView2 side is **profile scoped**: all webviews of this app share one
//! profile directory, so one call re-tints every window at once. Measured in the
//! S0 spike: an already-loaded page recomputes its media queries and repaints
//! without a reload (~90 ms), and creating a window with an explicit theme
//! re-tints the ones that are already open. Consequences encoded below:
//!
//! * every window creation passes `Some(resolved)`, never `None` (tao would stamp
//!   the *OS* theme onto the shared profile and undo an explicit light choice);
//! * switching back to `system` sets both halves explicitly (`None` + `AUTO`).

use serde::Serialize;
use tauri::{AppHandle, Manager, WebviewWindow};

use crate::state;

/// Value of `Config.theme` / `Config.locale` that means "follow Windows".
pub const SYSTEM: &str = "system";

/// Locales the host ships translations for.
pub const LOCALES: [&str; 2] = ["zh-CN", "en-US"];

/// Theme choices offered by the settings page.
pub const THEME_CHOICES: [&str; 3] = [SYSTEM, "light", "dark"];

/// Locale choices offered by the settings page (`system` first).
pub const LOCALE_CHOICES: [&str; 3] = [SYSTEM, "zh-CN", "en-US"];

// --------------------------------------------------------------- resolution

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    Dark,
    Light,
}

impl Theme {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    pub fn tauri_theme(self) -> tauri::Theme {
        match self {
            Self::Dark => tauri::Theme::Dark,
            Self::Light => tauri::Theme::Light,
        }
    }

    /// Background painted before the page's first frame.
    ///
    /// Matches the shell token `--bg` for the same theme, so the WebView2
    /// background flash (Microsoft does not guarantee it disappears) is the
    /// window colour instead of white.
    pub fn background(self) -> tauri::window::Color {
        match self {
            // --bg / --bg2 from ui/ (dark: #14161a, light: #f4f5f7)
            Self::Dark => tauri::window::Color(0x14, 0x16, 0x1a, 0xff),
            Self::Light => tauri::window::Color(0xf4, 0xf5, 0xf7, 0xff),
        }
    }
}

/// Theme that actually applies right now (config choice resolved against the OS).
pub fn resolved_theme() -> Theme {
    match state::load_config().theme.as_str() {
        "dark" => Theme::Dark,
        "light" => Theme::Light,
        _ => system_theme(),
    }
}

/// What Windows currently asks for, ignoring the user's choice.
pub fn system_theme() -> Theme {
    if system_uses_dark() {
        Theme::Dark
    } else {
        Theme::Light
    }
}

/// `AppsUseLightTheme == 0` **and** not high contrast — the same rule tao uses
/// for `should_use_dark_mode`, so host windows and WebView2 agree.
pub fn system_uses_dark() -> bool {
    apps_use_light_theme() == Some(false) && !high_contrast()
}

/// Locale that actually applies right now.
pub fn resolved_locale() -> &'static str {
    match state::load_config().locale.as_str() {
        "zh-CN" => "zh-CN",
        "en-US" => "en-US",
        _ => system_locale(),
    }
}

/// Display language Windows is set to (first `MUI_LANGUAGE_NAME` entry).
pub fn system_locale() -> &'static str {
    map_language(preferred_ui_language().as_deref().unwrap_or(""))
}

/// Reduce a BCP-47 tag to one of the two shipped locales.
///
/// `zh*` (zh-CN, zh-Hans-CN, zh-TW, …) all fall back to Simplified Chinese, which
/// is the only Chinese translation that ships; everything else gets `en-US`,
/// the language used for every non-Chinese Windows install.
pub fn map_language(tag: &str) -> &'static str {
    if tag.to_ascii_lowercase().starts_with("zh") {
        "zh-CN"
    } else {
        "en-US"
    }
}

// ------------------------------------------------------------------- probing

/// `Some(true)` = light, `Some(false)` = dark, `None` = value missing/unreadable.
fn apps_use_light_theme() -> Option<bool> {
    use windows::core::w;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

    let mut data: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut _ as *mut _),
            Some(&mut size),
        )
    };
    if status == ERROR_SUCCESS {
        Some(data != 0)
    } else {
        None
    }
}

fn high_contrast() -> bool {
    use windows::Win32::UI::Accessibility::{HIGHCONTRASTW, HIGHCONTRASTW_FLAGS};
    use windows::Win32::UI::WindowsAndMessaging::{
        SystemParametersInfoW, SPI_GETHIGHCONTRAST, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    };
    const HCF_HIGHCONTRASTON: u32 = 1;

    let mut hc = HIGHCONTRASTW {
        cbSize: std::mem::size_of::<HIGHCONTRASTW>() as u32,
        dwFlags: HIGHCONTRASTW_FLAGS(0),
        lpszDefaultScheme: windows::core::PWSTR::null(),
    };
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            std::mem::size_of_val(&hc) as u32,
            Some(&mut hc as *mut _ as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    ok.is_ok() && (hc.dwFlags.0 & HCF_HIGHCONTRASTON) != 0
}

/// First entry of `GetUserPreferredUILanguages(MUI_LANGUAGE_NAME)`, e.g. `zh-CN`.
pub fn preferred_ui_language() -> Option<String> {
    use windows::core::PWSTR;
    use windows::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};

    let mut count: u32 = 0;
    let mut chars: u32 = 0;
    // First call sizes the buffer; the list is NUL-separated (double-NUL at the end).
    unsafe {
        let _ = GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, None, &mut chars);
    }
    if chars == 0 {
        return None;
    }
    let mut buf = vec![0u16; chars as usize + 1];
    let mut chars2 = buf.len() as u32;
    unsafe {
        let _ = GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut count,
            Some(PWSTR(buf.as_mut_ptr())),
            &mut chars2,
        );
    }
    let first: Vec<u16> = buf.iter().copied().take_while(|c| *c != 0).collect();
    if first.is_empty() {
        return None;
    }
    Some(String::from_utf16_lossy(&first))
}

// ---------------------------------------------------------------- applying

/// Push an explicit theme to every webview (title bar + media query) and announce it.
///
/// Safe to call from any thread: `set_theme`/`with_webview` run inline on the main
/// thread and are posted from anywhere else (tauri-runtime-wry `send_user_message`).
pub fn apply_theme(app: &AppHandle, theme: Theme) -> usize {
    let windows: Vec<(String, WebviewWindow)> = app.webview_windows().into_iter().collect();
    for (label, win) in &windows {
        if let Err(e) = win.set_theme(Some(theme.tauri_theme())) {
            crate::logging::write("warn", "env", &format!("set_theme on {label} failed: {e}"));
        }
        set_webview_scheme(win, Some(theme));
    }
    crate::logging::write(
        "info",
        "env",
        &format!(
            "theme -> {} ({} window(s): {:?})",
            theme.as_str(),
            windows.len(),
            windows.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>()
        ),
    );
    broadcast(app, "theme", serde_json::json!(theme.as_str()));
    windows.len()
}

/// Hand every window back to the OS (`None` in tao terms, `AUTO` in WebView2).
pub fn apply_system_theme(app: &AppHandle) -> usize {
    let windows: Vec<(String, WebviewWindow)> = app.webview_windows().into_iter().collect();
    for (label, win) in &windows {
        if let Err(e) = win.set_theme(None) {
            crate::logging::write("warn", "env", &format!("set_theme(None) on {label} failed: {e}"));
        }
        set_webview_scheme(win, None);
    }
    let resolved = system_theme();
    crate::logging::write(
        "info",
        "env",
        &format!(
            "theme -> system ({} window(s), OS says {})",
            windows.len(),
            resolved.as_str()
        ),
    );
    broadcast(app, "theme", serde_json::json!(resolved.as_str()));
    windows.len()
}

/// Apply whatever `Config.theme` says, then announce the resolved locale too.
///
/// Locale resolution is cheap and re-entrant; the frontend re-reads it from the
/// `locale` event (`__toolbox.locale` for tools), while the native word lists are
/// switched by the caller that owns them (see `i18n` in lib.rs).
pub fn apply_all(app: &AppHandle) -> usize {
    let touched = apply_from_config(app);
    // Native strings that live outside the webviews: tool window titles.
    crate::windows::tools::refresh_titles(app);
    // The panel's acrylic tint is a window-composition attribute, not CSS.
    crate::windows::panel::refresh_glass(app);
    broadcast(app, "locale", serde_json::json!(resolved_locale()));
    touched
}

fn apply_from_config(app: &AppHandle) -> usize {
    match state::load_config().theme.as_str() {
        "dark" => apply_theme(app, Theme::Dark),
        "light" => apply_theme(app, Theme::Light),
        _ => apply_system_theme(app),
    }
}

fn set_webview_scheme(win: &WebviewWindow, theme: Option<Theme>) {
    let label = win.label().to_string();
    let _ = win.with_webview(move |webview| unsafe {
        use webview2_com::Microsoft::Web::WebView2::Win32::{
            ICoreWebView2_13, COREWEBVIEW2_PREFERRED_COLOR_SCHEME_AUTO,
            COREWEBVIEW2_PREFERRED_COLOR_SCHEME_DARK, COREWEBVIEW2_PREFERRED_COLOR_SCHEME_LIGHT,
        };
        use windows::core::Interface;

        let Ok(core) = webview.controller().CoreWebView2() else {
            crate::logging::write("warn", "env", &format!("{label}: no CoreWebView2"));
            return;
        };
        let c13 = match core.cast::<ICoreWebView2_13>() {
            Ok(v) => v,
            Err(e) => {
                // Very old WebView2 runtimes: the title bar still switches and the
                // `theme` event is authoritative for the page, so this is a warning.
                crate::logging::write(
                    "warn",
                    "env",
                    &format!("{label}: ICoreWebView2_13 unavailable ({e}), media query not forced"),
                );
                return;
            }
        };
        let Ok(profile) = c13.Profile() else {
            crate::logging::write("warn", "env", &format!("{label}: WebView2 profile unavailable"));
            return;
        };
        let scheme = match theme {
            Some(Theme::Dark) => COREWEBVIEW2_PREFERRED_COLOR_SCHEME_DARK,
            Some(Theme::Light) => COREWEBVIEW2_PREFERRED_COLOR_SCHEME_LIGHT,
            None => COREWEBVIEW2_PREFERRED_COLOR_SCHEME_AUTO,
        };
        if let Err(e) = profile.SetPreferredColorScheme(scheme) {
            crate::logging::write(
                "warn",
                "env",
                &format!("{label}: SetPreferredColorScheme failed: {e}"),
            );
        }
    });
}

/// Host → page event, for host windows and tool windows alike.
///
/// Tool windows carry no Tauri event permissions, so the injected shim installs
/// `__toolboxEmit`; host windows use `__hostEmit` (see `ipc::EmitHosts`).
pub fn broadcast(app: &AppHandle, event: &str, payload: serde_json::Value) {
    let event_json = serde_json::to_string(event).unwrap_or_else(|_| "\"\"".into());
    let payload_text = payload.to_string();
    for (label, win) in app.webview_windows() {
        let sink = if crate::windows::tools::tool_id_from_label(&label).is_some() {
            "window.__toolboxEmit"
        } else {
            "window.__hostEmit"
        };
        let _ = win.eval(&format!("{sink} && {sink}({event_json}, {payload_text})"));
    }
}

/// `WM_SETTINGCHANGE` reached the ball window: re-resolve and push everywhere.
///
/// Deliberately not filtered on `lParam`: Windows uses different payloads for a
/// theme switch, a high-contrast switch and a display-language change, and all of
/// them matter here. Re-resolution is cheap (one small JSON read + two Win32
/// queries) and the app sees a handful of these per session.
pub fn on_setting_change(app: &AppHandle) {
    let theme = resolved_theme();
    let locale = resolved_locale();
    crate::logging::write(
        "info",
        "env",
        &format!(
            "WM_SETTINGCHANGE -> resolved theme={} locale={}",
            theme.as_str(),
            locale
        ),
    );
    apply_all(app);
}

// ------------------------------------------------------------------- dto

/// What the shell UI needs to render the three-state controls.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvDto {
    /// Effective values.
    pub theme: String,
    pub locale: String,
    /// What Windows reports right now (shown next to "follow system").
    pub system_theme: String,
    pub system_locale: String,
    /// Whether the OS session is in high contrast (dark is suppressed then).
    pub high_contrast: bool,
    /// The raw first entry of `GetUserPreferredUILanguages`, for diagnostics.
    pub system_language_tag: String,
    pub theme_choices: [&'static str; 3],
    pub locale_choices: [&'static str; 3],
    pub available_locales: [&'static str; 2],
    /// True when the search panel really has a frosted (acrylic) backdrop. The
    /// panel only turns its translucent surface on in that case.
    pub glass: bool,
}

pub fn snapshot() -> EnvDto {
    EnvDto {
        theme: resolved_theme().as_str().to_string(),
        locale: resolved_locale().to_string(),
        system_theme: system_theme().as_str().to_string(),
        system_locale: system_locale().to_string(),
        high_contrast: high_contrast(),
        system_language_tag: preferred_ui_language().unwrap_or_default(),
        theme_choices: THEME_CHOICES,
        locale_choices: LOCALE_CHOICES,
        available_locales: LOCALES,
        glass: crate::windows::glass::panel_active(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_tags_reduce_to_a_shipped_locale() {
        assert_eq!(map_language("zh-CN"), "zh-CN");
        assert_eq!(map_language("zh-Hans-CN"), "zh-CN");
        assert_eq!(map_language("zh-TW"), "zh-CN");
        assert_eq!(map_language("ZH-cn"), "zh-CN");
        assert_eq!(map_language("en-US"), "en-US");
        assert_eq!(map_language("de-DE"), "en-US");
        assert_eq!(map_language(""), "en-US", "unreadable tag falls back to English");
    }

    #[test]
    fn theme_names_and_backgrounds_are_stable() {
        assert_eq!(Theme::Dark.as_str(), "dark");
        assert_eq!(Theme::Light.as_str(), "light");
        assert_eq!(Theme::Dark.background(), tauri::window::Color(0x14, 0x16, 0x1a, 0xff));
        assert_eq!(Theme::Light.background(), tauri::window::Color(0xf4, 0xf5, 0xf7, 0xff));
        assert!(matches!(Theme::Dark.tauri_theme(), tauri::Theme::Dark));
    }

    #[test]
    fn choices_start_with_system() {
        assert_eq!(THEME_CHOICES[0], SYSTEM);
        assert_eq!(LOCALE_CHOICES[0], SYSTEM);
        assert_eq!(LOCALES, ["zh-CN", "en-US"]);
    }
}
