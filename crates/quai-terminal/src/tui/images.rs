//! Images in the terminal: kitty bitmaps (pixels tier), half-block pictures (cells tier) and
//! monogram badges (text tier, missing or failed images). Amounts and names are always text;
//! images never carry meaning on their own.
//!
//! Inline icons in text rows are drawn as monogram badges; in the pixels tier a pass over the
//! finished frame finds those badges and places the icon bitmap over them.

use super::app::App;
use super::eco::KittyPng;
use super::terminal::Tier;
use super::theme::Theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use std::sync::Arc;
use wallet_core::media::{ICON, ICON_LARGE, Rendition, THUMB, is_native_icon, monogram, native_icon};

/// Most bitmaps placed in one frame.
const MAX_PLACEMENTS: usize = 128;

fn rgb_of(c: Color, fallback: (u8, u8, u8)) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => fallback,
    }
}

/// Nearest xterm-256 color for terminals without truecolor.
pub fn ansi256(r: u8, g: u8, b: u8) -> u8 {
    let level = |v: u8| {
        if v < 48 {
            0
        } else if v < 115 {
            1
        } else {
            (v - 35) / 40
        }
    };
    let (cr, cg, cb) = (level(r), level(g), level(b));
    let cube = 16 + 36 * cr + 6 * cg + cb;
    let gray = (u16::from(r) + u16::from(g) + u16::from(b)) / 3;
    if r.abs_diff(g) < 12 && g.abs_diff(b) < 12 && gray > 8 && gray < 238 { 232 + ((gray - 8) / 10).min(23) as u8 } else { cube }
}

fn color(app: &App, (r, g, b): (u8, u8, u8)) -> Color {
    if app.caps.truecolor { Color::Rgb(r, g, b) } else { Color::Indexed(ansi256(r, g, b)) }
}

fn blend(fg: [u8; 4], bg: (u8, u8, u8), fade: f32) -> (u8, u8, u8) {
    let a = f32::from(fg[3]) / 255.0 * fade.clamp(0.0, 1.0);
    let mix = |f: u8, b: u8| (f32::from(f) * a + f32::from(b) * (1.0 - a)).round() as u8;
    (mix(fg[0], bg.0), mix(fg[1], bg.1), mix(fg[2], bg.2))
}

/// Average RGBA of an image region (box filter), for downscaling into cells.
fn sample(r: &Rendition, x0: f32, y0: f32, x1: f32, y1: f32) -> [u8; 4] {
    let (xa, ya) = (x0.floor().max(0.0) as u32, y0.floor().max(0.0) as u32);
    let (xb, yb) = ((x1.ceil() as u32).clamp(xa + 1, r.width), (y1.ceil() as u32).clamp(ya + 1, r.height));
    let (mut sr, mut sg, mut sb, mut sa, mut n) = (0u32, 0u32, 0u32, 0u32, 0u32);
    for y in ya..yb {
        for x in xa..xb {
            let p = r.pixel(x, y);
            let a = u32::from(p[3]);
            sr += u32::from(p[0]) * a;
            sg += u32::from(p[1]) * a;
            sb += u32::from(p[2]) * a;
            sa += a;
            n += 1;
        }
    }
    if sa == 0 || n == 0 {
        return [0, 0, 0, 0];
    }
    [(sr / sa) as u8, (sg / sa) as u8, (sb / sa) as u8, (sa / n) as u8]
}

/// A light card behind transparent art drawn with dark strokes on a dark theme (OpenMoji-style
/// line art would otherwise lose its outlines). None when the art reads on the surface as is.
pub fn matte(r: &Rendition, t: &Theme) -> Option<(u8, u8, u8)> {
    if t.light || r.rgba.is_empty() {
        return None;
    }
    let (mut transparent, mut opaque, mut dark) = (0u32, 0u32, 0u32);
    for p in r.rgba.chunks_exact(4).step_by(3) {
        if p[3] < 128 {
            transparent += 1;
        } else {
            opaque += 1;
            if u32::from(p[0]) * 299 + u32::from(p[1]) * 587 + u32::from(p[2]) * 114 < 60_000 {
                dark += 1;
            }
        }
    }
    let total = transparent + opaque;
    (total > 0 && transparent * 10 > total && opaque > 0 && dark * 4 > opaque).then(|| {
        let surface = rgb_of(t.surface, (20, 20, 24));
        let strong = rgb_of(t.strong, (240, 240, 240));
        let m = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * 0.88).round() as u8;
        (m(surface.0, strong.0), m(surface.1, strong.1), m(surface.2, strong.2))
    })
}

/// Paint a rendition into `area` with half blocks, preserving aspect ratio (2 pixels per cell row).
pub fn half_block(app: &App, buf: &mut Buffer, area: Rect, r: &Rendition, t: &Theme, fade: f32, art: bool) {
    if area.width == 0 || area.height == 0 || r.width == 0 || r.height == 0 {
        return;
    }
    let surface = rgb_of(t.surface, if t.light { (250, 250, 250) } else { (20, 20, 24) });
    let card = if art { matte(r, t) } else { None };
    let bg = surface;
    let (cw, ch) = (f32::from(area.width), f32::from(area.height) * 2.0);
    let scale = (cw / r.width as f32).min(ch / r.height as f32);
    let (dw, dh) = (r.width as f32 * scale, r.height as f32 * scale);
    let (ox, oy) = ((cw - dw) / 2.0, (ch - dh) / 2.0);
    let px = |x: f32, y: f32| -> (u8, u8, u8) {
        if x < ox || y < oy || x >= ox + dw || y >= oy + dh {
            return bg;
        }
        let (ix0, iy0) = ((x - ox) / scale, (y - oy) / scale);
        let (ix1, iy1) = ((x + 1.0 - ox) / scale, (y + 1.0 - oy) / scale);
        blend(sample(r, ix0, iy0, ix1, iy1), card.unwrap_or(bg), fade)
    };
    for cy in 0..area.height {
        for cx in 0..area.width {
            let (x, y) = (f32::from(cx), f32::from(cy) * 2.0);
            let top = px(x, y);
            let bottom = px(x, y + 1.0);
            if let Some(cell) = buf.cell_mut((area.x + cx, area.y + cy)) {
                cell.set_char('▀').set_fg(color(app, top)).set_bg(color(app, bottom));
            }
        }
    }
}

/// Readable tint for a token: its icon's dominant color once the icon is loaded, else its
/// monogram color, lightened or darkened until it reaches 3:1 contrast against the surface.
/// QUAI and Qi keep their theme colors. Plain and no-color modes use the text color.
pub fn token_tint(app: &App, t: &Theme, icon_url: Option<&str>, symbol: &str, contract: &str) -> Color {
    if app.plain || app.no_color {
        return t.text;
    }
    match contract {
        "quai" => return t.quai,
        "qi" => return t.qi,
        _ => {}
    }
    let loaded = icon_url.filter(|u| icons_allowed(app, u)).and_then(|u| app.eco.cached_image(u, ICON)).map(|r| r.dominant);
    let base = loaded.unwrap_or_else(|| monogram(symbol, contract).1);
    color(app, readable(base, t))
}

/// Token icons may be shown: bundled logos always, fetched icons when the setting allows.
fn icons_allowed(app: &App, url: &str) -> bool {
    is_native_icon(url) || app.config.token_icons
}

/// Mix a color toward the text end until it reads against the theme surface (≥ 3:1).
pub fn readable(base: (u8, u8, u8), t: &Theme) -> (u8, u8, u8) {
    let surface = rgb_of(t.surface, if t.light { (250, 250, 250) } else { (20, 20, 24) });
    let target = if t.light { (0, 0, 0) } else { (255, 255, 255) };
    let mut c = base;
    for step in 1..=10 {
        if super::theme::contrast(c, surface) >= 3.0 {
            break;
        }
        let k = f64::from(step) / 10.0;
        let m = |x: u8, y: u8| (f64::from(x) + (f64::from(y) - f64::from(x)) * k).round() as u8;
        c = (m(base.0, target.0), m(base.1, target.1), m(base.2, target.2));
    }
    c
}

/// Inline icon for a token or NFT collection in text (activity rows, tables, chips): a
/// two-letter badge tinted like the token, replaced by the icon bitmap in the pixels tier (see
/// [`place_inline_icons`]). Plain / no-color modes show `[AB]`.
pub fn badge_span(app: &App, t: &Theme, icon_url: Option<&str>, symbol: &str, contract: &str) -> ratatui::text::Span<'static> {
    let (letters, _) = monogram(symbol, contract);
    let letters: String = format!("{letters:<2}").chars().take(2).collect();
    if app.plain || app.no_color {
        return ratatui::text::Span::styled(format!("[{letters}]"), t.dim_style());
    }
    let bg = token_tint(app, t, icon_url, symbol, contract);
    if let Some(url) = icon_url.filter(|u| app.caps.tier != Tier::Text && icons_allowed(app, u))
        && let Some((r, _)) = app.eco.image(url, ICON)
        && bitmaps(app)
    {
        app.eco.inline_icons.borrow_mut().push((letters.clone(), bg, r));
    }
    let lum = match bg {
        Color::Rgb(r, g, b) => u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114,
        _ => 0,
    };
    let fg = if lum > 150_000 { Color::Black } else { Color::White };
    ratatui::text::Span::styled(letters, Style::default().bg(bg).fg(fg).add_modifier(Modifier::BOLD))
}

/// Inline icon for a token contract, or `quai` / `qi` for the native coins.
pub fn asset_span(app: &App, t: &Theme, contract: &str, symbol: &str) -> ratatui::text::Span<'static> {
    if matches!(contract, "quai" | "qi") {
        return native_span(app, t, contract);
    }
    badge_span(app, t, app.asset_icon_url(contract).as_deref(), symbol, contract)
}

/// Inline icon for a native coin (`quai` or `qi`): the bundled logo where bitmaps can be placed,
/// otherwise the coin's glyph in its theme color (two cells either way).
pub fn native_span(app: &App, t: &Theme, asset: &str) -> ratatui::text::Span<'static> {
    let (symbol, contract, glyph, color) =
        if asset.eq_ignore_ascii_case("qi") { ("Qi", "qi", "◉ ", t.qi) } else { ("QUAI", "quai", "◆ ", t.quai) };
    let url = native_icon(contract);
    if app.plain || app.no_color || (bitmaps(app) && url.is_some_and(|u| app.eco.cached_image(u, ICON).is_some())) {
        return badge_span(app, t, url, symbol, contract);
    }
    if let Some(u) = url.filter(|_| app.caps.tier == Tier::Pixels) {
        // Load the logo so it can replace the glyph once bitmaps are allowed.
        let _ = app.eco.image(u, ICON);
    }
    ratatui::text::Span::styled(glyph, Style::default().fg(color).add_modifier(Modifier::BOLD))
}

/// Whether kitty bitmaps are placed for the current frame: the pixels tier, and no modal that
/// would sit under them (reviews, receive and the token picker place their own).
pub fn bitmaps(app: &App) -> bool {
    use super::app::Modal;
    app.caps.tier == Tier::Pixels
        && !app.plain
        && matches!(app.modal, Modal::None | Modal::Receive { .. } | Modal::Review(_) | Modal::TokenPicker { .. })
}

/// Place icon bitmaps over the inline badges that survived into the finished frame (pixels
/// tier). Each badge cell pair is blanked to the background beside it, so the icon sits on the
/// row's own color (selection, raised panel, surface).
pub fn place_inline_icons(app: &App, buf: &mut Buffer, t: &Theme) {
    let icons: Vec<(String, Color, Arc<Rendition>)> = app.eco.inline_icons.borrow_mut().drain(..).collect();
    if !bitmaps(app) {
        app.eco.kitty.borrow_mut().clear();
        return;
    }
    // Picture placements whose reserved cells were drawn over (a modal, a popup) are dropped.
    app.eco.kitty.borrow_mut().retain(|(rect, _, z)| {
        *z < 0
            || (rect.top()..rect.bottom())
                .all(|y| (rect.left()..rect.right()).all(|x| buf.cell((x, y)).is_some_and(|c| c.symbol() == " " && c.bg == t.surface)))
    });
    if icons.is_empty() {
        return;
    }
    let area = buf.area;
    // A badge is two bold letters on its own color, standing alone: bold words in the same color
    // (a selected tab reading "QUAI", a highlighted row) must not be taken for one. Each icon is
    // also placed at most as often as this frame drew it.
    let same = |a: &(String, Color, Arc<Rendition>), b: &(String, Color, Arc<Rendition>)| a.0 == b.0 && a.1 == b.1;
    let mut budget: Vec<usize> = icons
        .iter()
        .enumerate()
        .map(|(i, icon)| {
            // The first badge of its kind carries the count for all of them.
            match icons.iter().position(|other| same(other, icon)) == Some(i) {
                true => icons.iter().filter(|other| same(other, icon)).count(),
                false => 0,
            }
        })
        .collect();
    let letter_beside = |x: u16, y: u16, bg: Color| {
        buf.cell((x, y)).is_some_and(|c| c.bg == bg && c.symbol().chars().next().is_some_and(char::is_alphanumeric))
    };
    let mut found = Vec::new();
    for y in area.top()..area.bottom() {
        let mut x = area.left();
        while x + 1 < area.right() {
            let hit = match (buf.cell((x, y)), buf.cell((x + 1, y))) {
                (Some(a), Some(b)) if a.bg == b.bg && a.modifier.contains(Modifier::BOLD) => {
                    icons.iter().enumerate().find(|(i, (letters, bg, _))| {
                        let mut l = letters.chars();
                        budget[*i] > 0
                            && a.bg == *bg
                            && a.symbol().chars().eq(l.next())
                            && b.symbol().chars().eq(l.next())
                            && !(x > area.left() && letter_beside(x - 1, y, *bg))
                            && !letter_beside(x + 2, y, *bg)
                    })
                }
                _ => None,
            };
            match hit {
                Some((i, (_, _, r))) => {
                    budget[i] -= 1;
                    found.push((x, y, r.clone()));
                    x += 2;
                }
                None => x += 1,
            }
        }
    }
    let covered = |cx: u16, cy: u16| found.iter().any(|(fx, fy, _)| *fy == cy && (*fx == cx || *fx + 1 == cx));
    let fills: Vec<Color> = found
        .iter()
        .map(|(x, y, _)| {
            // The background beside the badge, skipping neighboring badges (icon pairs).
            let right = (x + 2..area.right()).find(|cx| !covered(*cx, *y));
            let left = (area.left()..*x).rev().find(|cx| !covered(*cx, *y));
            [right, left]
                .into_iter()
                .flatten()
                .filter_map(|cx| buf.cell((cx, *y)).map(|c| c.bg))
                .find(|c| *c != Color::Reset)
                .unwrap_or(t.surface)
        })
        .collect();
    for ((x, y, r), fill) in found.into_iter().zip(fills) {
        for dx in 0..2 {
            if let Some(cell) = buf.cell_mut((x + dx, y)) {
                cell.set_char(' ').set_bg(fill).set_style(Style::default().bg(fill).remove_modifier(Modifier::BOLD));
            }
        }
        push_kitty(app, Rect::new(x, y, 2, 1), &r, false);
    }
}

/// Queue a bitmap for `area`, padded to the area's pixel aspect so kitty does not stretch it.
fn push_kitty(app: &App, area: Rect, r: &Rendition, art: bool) {
    let png = fitted_png(app, r, area.width, area.height, art);
    let mut kitty = app.eco.kitty.borrow_mut();
    if kitty.len() < MAX_PLACEMENTS {
        kitty.push((area, png, 0));
    }
}

/// PNG-encode straight-alpha RGBA pixels.
pub fn encode_rgba(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header().and_then(|mut w| w.write_image_data(rgba)).ok()?;
    }
    Some(png)
}

/// Content key for a PNG (FNV-1a), computed once per encoded picture.
pub fn png_key(bytes: &[u8]) -> u64 {
    let mut h: u64 = 1469598103934665603;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(1099511628211);
    }
    h
}

/// The rendition's PNG centered on a transparent canvas with the aspect of `cols × rows` cells.
/// `art`: an NFT picture, which may get a light card (see [`matte`]); token icons never do.
/// Encoded once per rendition, canvas and theme; later frames reuse it.
fn fitted_png(app: &App, r: &Rendition, cols: u16, rows: u16, art: bool) -> KittyPng {
    let (cw, ch) = (f64::from(app.caps.cell_px.0.max(1)), f64::from(app.caps.cell_px.1.max(1)));
    let aspect = (f64::from(cols.max(1)) * cw) / (f64::from(rows.max(1)) * ch);
    let (w, h) = (r.width.max(1), r.height.max(1));
    let (canvas_w, canvas_h) = if f64::from(w) / f64::from(h) > aspect {
        (w, ((f64::from(w) / aspect).round() as u32).clamp(h, h * 8))
    } else {
        (((f64::from(h) * aspect).round() as u32).clamp(w, w * 8), h)
    };
    let t = &app.theme;
    let theme = if art { png_key(format!("{:?}{:?}{}", t.surface, t.strong, t.light).as_bytes()) } else { 0 };
    let content = r.hash.get(..16).and_then(|h| u64::from_str_radix(h, 16).ok()).unwrap_or_else(|| png_key(r.hash.as_bytes()));
    let key = (content, art, canvas_w, canvas_h, theme);
    if let Some(png) = app.eco.fitted.borrow().get(&key) {
        return png.clone();
    }
    let card = if art { matte(r, t) } else { None };
    let encoded = if r.rgba.len() != (w * h * 4) as usize || ((canvas_w, canvas_h) == (w, h) && card.is_none()) {
        None
    } else {
        let mut canvas = vec![0u8; (canvas_w * canvas_h * 4) as usize];
        let (ox, oy) = ((canvas_w - w) / 2, (canvas_h - h) / 2);
        for row in 0..h {
            let src = (row * w * 4) as usize;
            let dst = (((row + oy) * canvas_w + ox) * 4) as usize;
            canvas[dst..dst + (w * 4) as usize].copy_from_slice(&r.rgba[src..src + (w * 4) as usize]);
            if let Some((mr, mg, mb)) = card {
                for px in canvas[dst..dst + (w * 4) as usize].chunks_exact_mut(4) {
                    let a = f32::from(px[3]) / 255.0;
                    let mix = |c: u8, m: u8| (f32::from(c) * a + f32::from(m) * (1.0 - a)).round() as u8;
                    px.copy_from_slice(&[mix(px[0], mr), mix(px[1], mg), mix(px[2], mb), 255]);
                }
            }
        }
        encode_rgba(canvas_w, canvas_h, &canvas)
    };
    let png = encoded.unwrap_or_else(|| r.png.clone());
    let key_of = png_key(&png);
    let entry = (Arc::new(png), key_of);
    let mut cache = app.eco.fitted.borrow_mut();
    if cache.len() > 512 {
        cache.clear();
    }
    cache.insert(key, entry.clone());
    entry
}

/// Monogram badge filling `area` (letters centered on a stable color).
pub fn badge(app: &App, buf: &mut Buffer, area: Rect, symbol: &str, contract: &str, t: &Theme) {
    let (letters, rgb) = monogram(symbol, contract);
    let bg = if app.plain || app.no_color { None } else { Some(color(app, rgb)) };
    let lum = u32::from(rgb.0) * 299 + u32::from(rgb.1) * 587 + u32::from(rgb.2) * 114;
    let fg = if lum > 150_000 { Color::Black } else { Color::White };
    let style = match bg {
        Some(b) => Style::default().bg(b).fg(fg).add_modifier(Modifier::BOLD),
        None => t.strong_style(),
    };
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char(' ').set_style(style);
            }
        }
    }
    let text: String = if bg.is_none() { format!("[{letters}]") } else { letters };
    let w = text.chars().count() as u16;
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height / 2;
    for (i, ch) in text.chars().enumerate() {
        if x + (i as u16) < area.right()
            && let Some(cell) = buf.cell_mut((x + i as u16, y))
        {
            cell.set_char(ch).set_style(style);
        }
    }
}

/// Draw the best available picture for `url` into `area`: kitty in pixel terminals, half blocks
/// in cell terminals, otherwise (or while loading / on failure) a monogram badge.
pub fn picture(app: &App, buf: &mut Buffer, area: Rect, t: &Theme, url: Option<&str>, symbol: &str, contract: &str, nft: bool) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let edge = match (nft, app.caps.tier) {
        (true, _) => THUMB,
        // Bitmaps and multi-row half blocks look soft from the 32 px icon.
        (false, Tier::Pixels) if area.width > 2 => ICON_LARGE,
        (false, Tier::Cells) if area.height > 4 => ICON_LARGE,
        _ => ICON,
    };
    let allowed = if nft { app.config.images } else { url.is_some_and(|u| icons_allowed(app, u)) };
    let ready = match (url, allowed, app.caps.tier) {
        (Some(u), true, Tier::Pixels | Tier::Cells) if !app.plain => app.eco.image(u, edge),
        _ => None,
    };
    match ready {
        Some((r, _)) if app.caps.tier == Tier::Pixels => {
            // Reserve the cells; the bitmap is placed after the frame is flushed.
            for y in area.top()..area.bottom() {
                for x in area.left()..area.right() {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_char(' ').set_bg(t.surface);
                    }
                }
            }
            push_kitty(app, area, &r, nft);
        }
        Some((r, fade)) => half_block(app, buf, area, &r, t, fade, nft),
        None => badge(app, buf, area, symbol, contract, t),
    }
}

/// This frame's kitty placements.
pub fn kitty_items(app: &App) -> Vec<super::terminal::Placement> {
    app.eco
        .kitty
        .borrow_mut()
        .drain(..)
        .map(|(rect, (png, key), z)| super::terminal::Placement { png, key, x: rect.x, y: rect.y, cols: rect.width, rows: rect.height, z })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tints_are_readable() {
        let dark = super::super::theme::resolve(std::path::Path::new("/nonexistent"), "quai-dark", false, false).0;
        let surface = rgb_of(dark.surface, (20, 20, 24));
        // A near-black icon color is lifted until it reads on a dark surface.
        let c = readable((10, 10, 12), &dark);
        assert!(super::super::theme::contrast(c, surface) >= 3.0, "{c:?}");
        // A color that already reads is unchanged.
        assert_eq!(readable((250, 200, 40), &dark), (250, 200, 40));
    }

    #[test]
    fn line_art_gets_a_card_on_dark_themes() {
        let dark = super::super::theme::resolve(std::path::Path::new("/nonexistent"), "quai-red", false, false).0;
        let light = super::super::theme::resolve(std::path::Path::new("/nonexistent"), "quai-light", false, false).0;
        let make = |f: &dyn Fn(usize) -> [u8; 4]| Rendition {
            hash: "h".into(),
            width: 10,
            height: 10,
            png: Vec::new(),
            rgba: (0..100).flat_map(f).collect(),
            dominant: (0, 0, 0),
        };
        // Transparent background, black outlines, orange fill: the squid case.
        let squid = make(&|i| {
            if i % 2 == 0 {
                [0, 0, 0, 0]
            } else if i % 3 == 0 {
                [0, 0, 0, 255]
            } else {
                [240, 160, 40, 255]
            }
        });
        assert!(matte(&squid, &dark).is_some());
        assert!(matte(&squid, &light).is_none(), "light themes show dark strokes already");
        // Opaque art and light line art need no card.
        assert!(matte(&make(&|_| [30, 30, 30, 255]), &dark).is_none());
        assert!(matte(&make(&|i| if i % 2 == 0 { [0, 0, 0, 0] } else { [250, 250, 250, 255] }), &dark).is_none());
    }

    #[test]
    fn palette_mapping() {
        assert_eq!(ansi256(0, 0, 0), 16);
        assert_eq!(ansi256(255, 0, 0), 196);
        assert!((232..=255).contains(&ansi256(128, 128, 128)));
        let white = blend([255, 255, 255, 255], (0, 0, 0), 1.0);
        assert_eq!(white, (255, 255, 255));
        assert_eq!(blend([255, 255, 255, 255], (0, 0, 0), 0.0), (0, 0, 0));
        assert_eq!(blend([255, 0, 0, 0], (10, 20, 30), 1.0), (10, 20, 30));
    }
}
