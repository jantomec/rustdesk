//! Arrow cursor for niri virtual-display sessions.
//!
//! While a session takeover is engaged the physical outputs are off, so the DRM
//! hardware-cursor stream has nothing to serve, and the XFixes fallback reads
//! Xwayland's pointer - whatever theme and size Xwayland happened to inherit at
//! spawn (observed: 24 px "default" while niri draws a 64 px cursor). Serve the
//! arrow from the Xcursor theme niri itself is configured with instead.
//!
//! The image is served at the theme's nominal (logical) size: the client
//! registers cursor pixels as logical points, which renders at the same
//! on-screen size the compositor would draw on the scale-2 virtual output.
//! Cursor shape is not tracked - the pointer is always the arrow - because niri
//! exposes no cursor-shape events to clients.

use hbb_common::{log, message_proto::CursorData};
use std::path::PathBuf;
use std::sync::OnceLock;

/// Stable id, distinct from XFixes serials and the hash-derived DRM ids.
const ID: u64 = 0x6e69_7269_6375_7273; // "niricurs"

pub fn cursor_id() -> Option<u64> {
    if !super::niri_virtual_display::takeover_active() {
        return None;
    }
    cursor().as_ref().map(|_| ID)
}

pub fn cursor_data(hcursor: u64) -> Option<CursorData> {
    if hcursor != ID {
        return None;
    }
    cursor().clone()
}

fn cursor() -> &'static Option<CursorData> {
    static CACHE: OnceLock<Option<CursorData>> = OnceLock::new();
    CACHE.get_or_init(|| match load() {
        Some(cd) => {
            log::info!(
                "niri cursor: serving theme arrow {}x{} (hot {},{})",
                cd.width,
                cd.height,
                cd.hotx,
                cd.hoty
            );
            Some(cd)
        }
        None => {
            log::warn!("niri cursor: no Xcursor arrow found; falling back to XFixes");
            None
        }
    })
}

/// The `cursor { xcursor-theme; xcursor-size }` block of niri's config. niri
/// applies these itself but only exports them to processes it spawns later, so
/// the config file is the source of truth; env vars and niri's documented
/// defaults are the fallbacks.
fn niri_cursor_config() -> (String, u32) {
    let mut theme = std::env::var("XCURSOR_THEME").unwrap_or_default();
    let mut size: u32 = std::env::var("XCURSOR_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let config = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .ok()
        .or_else(|| std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".config")))
        .map(|d| d.join("niri/config.kdl"));
    if let Some(Ok(text)) = config.map(std::fs::read_to_string) {
        for line in text.lines() {
            let line = line.split("//").next().unwrap_or("").trim();
            if let Some(rest) = line.strip_prefix("xcursor-theme") {
                let v = rest.trim().trim_matches('"');
                if !v.is_empty() {
                    theme = v.to_owned();
                }
            } else if let Some(rest) = line.strip_prefix("xcursor-size") {
                if let Ok(v) = rest.trim().parse() {
                    size = v;
                }
            }
        }
    }
    if theme.is_empty() {
        theme = "default".to_owned();
    }
    if size == 0 {
        size = 24;
    }
    (theme, size)
}

fn load() -> Option<CursorData> {
    let (theme, size) = niri_cursor_config();
    let icon_path = ["default", "left_ptr"]
        .iter()
        .find_map(|name| xcursor::CursorTheme::load(&theme).load_icon(name))?;
    let data = std::fs::read(&icon_path).ok()?;
    let images = xcursor::parser::parse_xcursor(&data)?;
    let best = images
        .iter()
        .min_by_key(|i| (i.size as i64 - size as i64).abs())?;
    if best.size != size {
        log::warn!(
            "niri cursor: theme {theme} has no size-{size} arrow, using {}",
            best.size
        );
    }
    // Xcursor pixels are premultiplied; the XFixes and DRM paths pass
    // premultiplied RGBA through as-is, so do the same.
    let mut cd = CursorData::default();
    cd.id = ID;
    cd.hotx = best.xhot as _;
    cd.hoty = best.yhot as _;
    cd.width = best.width as _;
    cd.height = best.height as _;
    cd.colors = best.pixels_rgba.clone().into();
    Some(cd)
}
