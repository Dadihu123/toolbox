//! Tool icon rasterisation.
//!
//! Manifest icons are `icon.svg` by contract (docs/design.md §4.1). Two native
//! consumers need them as pixels:
//!
//!   * the taskbar / titlebar icon of a tool window ([`crate::windows::tools`])
//!   * the hover quick menu ([`crate::windows::quickmenu`])
//!
//! Both must work without a WebView, so the SVG is rasterised in-process with
//! `resvg` and cached per `(tool, size)`. A tool whose icon cannot be rendered
//! falls back to an accent-coloured glyph built from primitives, so a broken
//! icon never turns into a blank window icon.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use crate::registry;

/// A rasterised icon: tightly packed RGBA with **straight** (non-premultiplied)
/// alpha, which is what both `UpdateLayeredWindow` sources and Tauri's
/// `Image::new_owned` expect after conversion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaIcon {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl RgbaIcon {
    /// Hand the bitmap to Tauri (window icon / tray icon).
    pub fn to_tauri_image(&self) -> tauri::image::Image<'static> {
        tauri::image::Image::new_owned(self.pixels.clone(), self.width, self.height)
    }

    /// Colour of one pixel, straight alpha. Used by the software renderers.
    pub fn pixel(&self, x: u32, y: u32) -> [f32; 4] {
        if x >= self.width || y >= self.height {
            return [0.0, 0.0, 0.0, 0.0];
        }
        let i = ((y * self.width + x) * 4) as usize;
        [
            self.pixels[i] as f32 / 255.0,
            self.pixels[i + 1] as f32 / 255.0,
            self.pixels[i + 2] as f32 / 255.0,
            self.pixels[i + 3] as f32 / 255.0,
        ]
    }
}

// ------------------------------------------------------------------- cache

type CacheMap = HashMap<(String, u32), Arc<RgbaIcon>>;

fn cache() -> &'static Mutex<CacheMap> {
    static CACHE: OnceLock<Mutex<CacheMap>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Rasterised icon of a tool, cached. `None` when the manifest has no usable
/// icon (missing file / unparsable SVG / unsupported format).
pub fn tool_icon(tool_id: &str, size: u32) -> Option<Arc<RgbaIcon>> {
    let size = size.clamp(8, 512);
    if let Some(hit) = cache()
        .lock()
        .ok()
        .and_then(|c| c.get(&(tool_id.to_string(), size)).cloned())
    {
        return Some(hit);
    }
    let manifest = registry::find(tool_id)?;
    let dir = std::path::PathBuf::from(&manifest.dir);
    let icon = icon_from_dir(&dir, &manifest.icon, size)?;
    let icon = Arc::new(icon);
    if let Ok(mut c) = cache().lock() {
        c.insert((tool_id.to_string(), size), icon.clone());
    }
    Some(icon)
}

/// Like [`tool_icon`], but never fails: the fallback glyph uses the manifest
/// accent colour (or Toolbox blue when the tool is unknown).
pub fn tool_icon_or_fallback(tool_id: &str, size: u32) -> Arc<RgbaIcon> {
    if let Some(icon) = tool_icon(tool_id, size) {
        return icon;
    }
    let accent = registry::find(tool_id)
        .map(|m| parse_hex(&m.accent))
        .unwrap_or([78, 161, 255]);
    Arc::new(fallback(size, accent))
}

/// Drop a cached tool icon (after an upgrade / reinstall).
pub fn invalidate(tool_id: &str) {
    if let Ok(mut c) = cache().lock() {
        c.retain(|(id, _), _| id != tool_id);
    }
}

// --------------------------------------------------------------- rendering

/// Render an icon file (`icon.svg` / `.png`) inside a tool directory.
fn icon_from_dir(dir: &Path, rel: &str, size: u32) -> Option<RgbaIcon> {
    let rel = rel.trim();
    if rel.is_empty() {
        return None;
    }
    let joined = dir.join(rel.replace('\\', "/"));
    let path = crate::paths::ensure_inside(dir, &joined)?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "svg" => {
            let text = std::fs::read_to_string(&path).ok()?;
            from_svg(&text, size)
        }
        "png" | "jpg" | "jpeg" | "webp" => {
            let bytes = std::fs::read(&path).ok()?;
            from_encoded(&bytes, size)
        }
        _ => None,
    }
}

/// Rasterise SVG source into a `size`×`size` square, preserving the aspect
/// ratio and centring the artwork.
pub fn from_svg(svg: &str, size: u32) -> Option<RgbaIcon> {
    let size = size.clamp(8, 512);
    let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default()).ok()?;
    let src = tree.size();
    if src.width() <= 0.0 || src.height() <= 0.0 {
        return None;
    }
    let scale = (size as f32 / src.width()).min(size as f32 / src.height());
    let tx = (size as f32 - src.width() * scale) / 2.0;
    let ty = (size as f32 - src.height() * scale) / 2.0;
    let transform = resvg::tiny_skia::Transform::from_translate(tx, ty).pre_scale(scale, scale);
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size)?;
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Some(unpremultiply(&pixmap, size, size))
}

/// Decode PNG/JPEG and scale it into a `size`×`size` square.
pub fn from_encoded(bytes: &[u8], size: u32) -> Option<RgbaIcon> {
    let size = size.clamp(8, 512);
    let image = tauri::image::Image::from_bytes(bytes).ok()?;
    let (w, h) = (image.width(), image.height());
    if w == 0 || h == 0 {
        return None;
    }
    let src = image.rgba();
    let scale = (size as f32 / w as f32).min(size as f32 / h as f32);
    let draw_w = (w as f32 * scale).round().max(1.0) as u32;
    let draw_h = (h as f32 * scale).round().max(1.0) as u32;
    let ox = (size.saturating_sub(draw_w)) / 2;
    let oy = (size.saturating_sub(draw_h)) / 2;
    let mut out = vec![0u8; (size * size * 4) as usize];
    for y in 0..draw_h {
        let sy = (y as f32 / scale) as u32;
        for x in 0..draw_w {
            let sx = (x as f32 / scale) as u32;
            if sx >= w || sy >= h {
                continue;
            }
            let si = ((sy * w + sx) * 4) as usize;
            let di = (((oy + y) * size) + (ox + x)) as usize * 4;
            out[di..di + 4].copy_from_slice(&src[si..si + 4]);
        }
    }
    Some(RgbaIcon {
        width: size,
        height: size,
        pixels: out,
    })
}

/// `tiny_skia` hands back premultiplied RGBA; undo that so callers can treat
/// the bitmap as plain straight-alpha RGBA.
fn unpremultiply(pixmap: &resvg::tiny_skia::Pixmap, width: u32, height: u32) -> RgbaIcon {
    let mut out = Vec::with_capacity((width * height * 4) as usize);
    for pixel in pixmap.pixels() {
        let c = pixel.demultiply();
        out.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    RgbaIcon {
        width,
        height,
        pixels: out,
    }
}

// ---------------------------------------------------------------- fallback

/// Accent-coloured placeholder used when a tool's icon cannot be rendered.
fn fallback(size: u32, accent: [u8; 3]) -> RgbaIcon {
    let s = size.max(8);
    let mut out = vec![0u8; (s * s * 4) as usize];
    let pad = (s as f32 * 0.06).round();
    let rect = (pad, pad, s as f32 - pad * 2.0, s as f32 - pad * 2.0);
    let radius = (s as f32 * 0.22).round();
    let light = [
        (accent[0] as f32 * 1.25).min(255.0),
        (accent[1] as f32 * 1.25).min(255.0),
        (accent[2] as f32 * 1.25).min(255.0),
    ];
    for y in 0..s {
        for x in 0..s {
            let cov = covered_round_rect(x as f32, y as f32, rect.0, rect.1, rect.2, rect.3, radius);
            if cov <= 0.0 {
                continue;
            }
            let t = y as f32 / s as f32;
            let i = ((y * s + x) * 4) as usize;
            out[i] = lerp(light[0], accent[0] as f32, t) as u8;
            out[i + 1] = lerp(light[1], accent[1] as f32, t) as u8;
            out[i + 2] = lerp(light[2], accent[2] as f32, t) as u8;
            out[i + 3] = (255.0 * cov) as u8;
        }
    }
    // 2x2 glyph, mirroring the floating ball mark.
    let cell = s as f32 * 0.2;
    let gap = s as f32 * 0.09;
    let left = (s as f32 - (cell * 2.0 + gap)) / 2.0;
    let top = left;
    for row in 0..2 {
        for col in 0..2 {
            let x0 = left + col as f32 * (cell + gap);
            let y0 = top + row as f32 * (cell + gap);
            let r = cell * 0.28;
            for y in y0.floor().max(0.0) as u32..(y0 + cell).ceil().min(s as f32) as u32 {
                for x in x0.floor().max(0.0) as u32..(x0 + cell).ceil().min(s as f32) as u32 {
                    let cov = covered_round_rect(x as f32, y as f32, x0, y0, cell, cell, r);
                    if cov <= 0.0 {
                        continue;
                    }
                    let i = ((y * s + x) * 4) as usize;
                    let a = cov * 0.94;
                    out[i] = 255;
                    out[i + 1] = 255;
                    out[i + 2] = 255;
                    out[i + 3] = (a * 255.0) as u8;
                }
            }
        }
    }
    RgbaIcon {
        width: s,
        height: s,
        pixels: out,
    }
}

fn covered_round_rect(px: f32, py: f32, x: f32, y: f32, w: f32, h: f32, r: f32) -> f32 {
    const SUB: u32 = 4;
    let mut hits = 0;
    for sy in 0..SUB {
        for sx in 0..SUB {
            let fx = px + (sx as f32 + 0.5) / SUB as f32;
            let fy = py + (sy as f32 + 0.5) / SUB as f32;
            if inside_round_rect(fx, fy, x, y, w, h, r) {
                hits += 1;
            }
        }
    }
    hits as f32 / (SUB * SUB) as f32
}

fn inside_round_rect(px: f32, py: f32, x: f32, y: f32, w: f32, h: f32, r: f32) -> bool {
    if px < x || py < y || px > x + w || py > y + h {
        return false;
    }
    let r = r.min(w / 2.0).min(h / 2.0);
    let cx = px.clamp(x + r, x + w - r);
    let cy = py.clamp(y + r, y + h - r);
    let dx = px - cx;
    let dy = py - cy;
    dx * dx + dy * dy <= r * r
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

/// `#rrggbb` / `#rgb` manifest accent to RGB.
pub fn parse_hex(hex: &str) -> [u8; 3] {
    let raw = hex.trim().trim_start_matches('#');
    let expand = |s: &str, i: usize| -> u8 {
        u8::from_str_radix(&s[i..i + 1].repeat(2), 16).unwrap_or(0)
    };
    match raw.len() {
        6 => [
            u8::from_str_radix(&raw[0..2], 16).unwrap_or(78),
            u8::from_str_radix(&raw[2..4], 16).unwrap_or(161),
            u8::from_str_radix(&raw[4..6], 16).unwrap_or(255),
        ],
        3 => [expand(raw, 0), expand(raw, 1), expand(raw, 2)],
        _ => [78, 161, 255],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CROSSHAIR: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64">
        <rect width="64" height="64" rx="14" fill="#1b1e24"/>
        <circle cx="32" cy="32" r="18" fill="#4ea1ff"/>
    </svg>"##;

    #[test]
    fn svg_rasterises_to_requested_size() {
        let icon = from_svg(CROSSHAIR, 32).expect("svg renders");
        assert_eq!(icon.width, 32);
        assert_eq!(icon.pixels.len(), 32 * 32 * 4);
        // centre is opaque and blue-ish
        let centre = icon.pixel(16, 16);
        assert!(centre[3] > 0.9);
        assert!(centre[2] > centre[0]);
        // corners stay transparent (the svg has a rounded rect, not a full square)
        assert!(icon.pixel(0, 0)[3] < 0.1);
    }

    #[test]
    fn fallback_is_opaque_in_the_middle() {
        let icon = fallback(24, [78, 161, 255]);
        assert_eq!(icon.pixels.len(), 24 * 24 * 4);
        assert!(icon.pixel(12, 12)[3] > 0.9);
    }

    #[test]
    fn hex_parsing_accepts_both_forms() {
        assert_eq!(parse_hex("#4ea1ff"), [0x4e, 0xa1, 0xff]);
        assert_eq!(parse_hex("abc"), [0xaa, 0xbb, 0xcc]);
        assert_eq!(parse_hex("nope"), [78, 161, 255]);
    }

    #[test]
    fn invalid_svg_falls_back_instead_of_panicking() {
        assert!(from_svg("<svg", 16).is_none());
    }
}
