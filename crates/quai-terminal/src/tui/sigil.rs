//! Address sigils: a small picture drawn from an address, so a changed address is noticed before
//! its hex is read. Beside the address, never instead of it.
//!
//! A sigil is SHA-256 of `quai-terminal/sigil/v1:` and the address in lowercase. The first bits
//! choose a hue; the next fifteen fill the left three columns of a 5×5 grid, mirrored into the
//! right two. In cells it is two cells of quadrant blocks, the second the mirror of the first, on
//! a tint of its hue; in kitty the 5×5 picture is placed over those two cells, the way token icons
//! are. Two addresses can share a sigil (it is a few dozen bits); it only says "this looks like
//! the one you know", never that it is.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use sha2::{Digest, Sha256};
use wallet_core::media::Rendition;

use super::app::App;
use super::theme::Theme;

/// Quadrant blocks by bits: top-left 1, top-right 2, bottom-left 4, bottom-right 8.
const QUADRANTS: [&str; 16] = [" ", "▘", "▝", "▀", "▖", "▌", "▞", "▛", "▗", "▚", "▐", "▜", "▄", "▙", "▟", "█"];

/// What a sigil is made of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Sigil {
    /// 0..360.
    pub hue: u16,
    /// The 5×5 grid, row by row: bit `r*5 + c`.
    pub grid: u32,
    /// The left cell's quadrants (never empty).
    pub quadrants: u8,
}

pub fn of(address: &str) -> Sigil {
    let mut h = Sha256::new();
    h.update(b"quai-terminal/sigil/v1:");
    h.update(address.trim().to_lowercase().as_bytes());
    let d = h.finalize();
    let hue = u16::from_be_bytes([d[0], d[1]]) % 360;
    let bits = u32::from_be_bytes([d[2], d[3], d[4], d[5]]);
    let mut grid = 0u32;
    for r in 0..5 {
        for c in 0..3 {
            if bits >> (r * 3 + c) & 1 == 1 {
                grid |= 1 << (r * 5 + c);
                grid |= 1 << (r * 5 + (4 - c));
            }
        }
    }
    // Never blank: a sigil with nothing in it reads as no sigil at all.
    let quadrants = match d[6] & 0x0f {
        0 => 0x0f,
        q => q,
    };
    Sigil { hue, grid: if grid == 0 { 0b00100_01110_11111_01110_00100 } else { grid }, quadrants }
}

/// HSL to RGB.
fn hsl(h: f64, s: f64, l: f64) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let to = |v: f64| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    (to(r), to(g), to(b))
}

impl Sigil {
    /// The ink and the tile under it, for a dark or a light page.
    pub fn colours(&self, light: bool) -> ((u8, u8, u8), (u8, u8, u8)) {
        let h = f64::from(self.hue);
        if light { (hsl(h, 0.65, 0.38), hsl(h, 0.45, 0.88)) } else { (hsl(h, 0.70, 0.66), hsl(h, 0.40, 0.18)) }
    }

    /// The two cells: the quadrants and their mirror (left and right swapped).
    pub fn cells(&self) -> String {
        let q = self.quadrants;
        let mirror = ((q & 1) << 1) | ((q & 2) >> 1) | ((q & 4) << 1) | ((q & 8) >> 1);
        format!("{}{}", QUADRANTS[q as usize], QUADRANTS[mirror as usize])
    }

    /// The 5×5 picture on its tile, `edge` pixels square.
    fn rendition(&self, light: bool, edge: u32) -> Rendition {
        let ((ir, ig, ib), (tr, tg, tb)) = self.colours(light);
        let mut canvas = super::raster::Canvas::new(edge as usize, edge as usize, [tr, tg, tb], 255);
        let pad = edge as f64 * 0.12;
        let cell = (edge as f64 - 2.0 * pad) / 5.0;
        for r in 0..5 {
            for c in 0..5 {
                if self.grid >> (r * 5 + c) & 1 == 1 {
                    canvas.fill(pad + c as f64 * cell + 0.5, pad + r as f64 * cell + 0.5, cell - 1.0, cell - 1.0, [ir, ig, ib], 1.0);
                }
            }
        }
        let key = (u64::from(self.grid) << 16) | u64::from(self.hue) | (u64::from(light) << 60);
        canvas.rendition(key)
    }
}

/// Pictures made, by sigil and page shade: a 32-pixel PNG each, cheap, and kept.
type Pictures = HashMap<(Sigil, bool), Arc<Rendition>>;

static PICTURES: Mutex<Option<Pictures>> = Mutex::new(None);

fn picture(s: Sigil, light: bool) -> Arc<Rendition> {
    let mut guard = PICTURES.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    if map.len() > 512 {
        map.clear();
    }
    map.entry((s, light)).or_insert_with(|| Arc::new(s.rendition(light, 32))).clone()
}

/// What drawing a sigil needs from the app, taken apart so it can be drawn while the app is
/// borrowed elsewhere (a modal being drawn holds it).
pub struct Ctx<'a> {
    pub show: bool,
    pub bitmaps: bool,
    pub icons: &'a std::cell::RefCell<Vec<(String, Color, Arc<Rendition>)>>,
}

impl<'a> Ctx<'a> {
    pub fn of(app: &'a App) -> Ctx<'a> {
        Ctx { show: !app.term.plain && !app.term.no_color, bitmaps: super::images::bitmaps(app), icons: &app.eco.media.inline_icons }
    }

    /// An address's sigil as two cells, replaced by its picture in kitty. Nothing in plain or
    /// no-colour modes, where a block pattern without its colour says little.
    pub fn span(&self, t: &Theme, address: &str) -> Span<'static> {
        if !self.show || address.trim().is_empty() {
            return Span::raw("");
        }
        let s = of(address);
        let ((ir, ig, ib), (tr, tg, tb)) = s.colours(t.light);
        let (ink, tile) = (Color::Rgb(ir, ig, ib), Color::Rgb(tr, tg, tb));
        let cells = s.cells();
        if self.bitmaps {
            self.icons.borrow_mut().push((cells.clone(), tile, picture(s, t.light)));
        }
        Span::styled(cells, Style::default().fg(ink).bg(tile).add_modifier(Modifier::BOLD))
    }
}

/// An address's sigil as two cells (see [`Ctx::span`]).
pub fn span(app: &App, t: &Theme, address: &str) -> Span<'static> {
    Ctx::of(app).span(t, address)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sigil_is_the_addresss_own_and_case_does_not_matter() {
        let a = "0x004dd9afaa2768642b5cde15c24f37bf19d842e4";
        assert_eq!(of(a), of(&a.to_uppercase().replace("0X", "0x")));
        assert_ne!(of(a), of("0x004dd9afaa2768642b5cde15c24f37bf19d842e5"), "one hex digit off");
        assert_eq!(of(a).cells().chars().count(), 2);
    }

    #[test]
    fn grids_are_mirrored_and_never_blank() {
        for i in 0..200u32 {
            let s = of(&format!("0x{i:040x}"));
            assert_ne!(s.grid, 0);
            for r in 0..5 {
                for c in 0..2 {
                    assert_eq!(s.grid >> (r * 5 + c) & 1, s.grid >> (r * 5 + 4 - c) & 1, "row {r} is symmetric");
                }
            }
            assert_ne!(s.quadrants & 0x0f, 0);
        }
    }

    #[test]
    fn the_cells_mirror_each_other() {
        let s = Sigil { hue: 0, grid: 1, quadrants: 0b0101 };
        assert_eq!(s.cells(), "▌▐");
        let s = Sigil { hue: 0, grid: 1, quadrants: 0b0001 };
        assert_eq!(s.cells(), "▘▝");
    }

    #[test]
    fn a_hundred_addresses_rarely_share_a_look() {
        let mut seen = std::collections::HashSet::new();
        for i in 0..100u32 {
            let s = of(&format!("0x{i:040x}"));
            seen.insert((s.hue / 30, s.grid, s.quadrants));
        }
        assert!(seen.len() >= 98, "{} of 100 distinct", seen.len());
    }
}
