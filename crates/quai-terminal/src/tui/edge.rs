//! Filament: light lives on lines.
//!
//! A pass over the finished frame that recolors only box-drawing border glyphs, never titles or
//! content, so amounts and addresses are untouched. Modeled on Omarchy's Hyprland windows:
//!
//! - The focused panel's border is a two-stop 45° gradient (theme accent → a companion color).
//! - Vivid: the gradient turns slowly (one turn per 12 s) and a short glint laps the border every
//!   11 s, skipped while you are typing. Nothing moves while a modal is open.
//! - On a screen change every border draws itself in from its top-left corner (replaces the
//!   random cell scramble, which briefly animated amounts).
//! - The header carries a gradient hairline (colored underline) that a block heartbeat runs along.
//!
//! Truecolor only; other terminals keep the plain bold focus border.

use super::app::{App, Modal};
use super::theme::Theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use wallet_core::config::Motion;

type Rgb = (u8, u8, u8);

const TURN_MS: u64 = 12_000;
const TURN_STEP_MS: u64 = 80;
const GLINT_EVERY_MS: u64 = 11_000;
const GLINT_LAP_MS: u64 = 1_400;
const GLINT_STEP_MS: u64 = 33;
const GLINT_TAIL: f32 = 9.0;
/// Border draw-in on screen change.
pub const INTRO_MS: u128 = 220;
const INTRO_STAGGER_MS: u128 = 25;
/// How long a draw-in (with stagger) can take.
pub const INTRO_TOTAL_MS: u128 = INTRO_MS + INTRO_STAGGER_MS * 10;
/// One-shot light on a row or slot (confirmation reached, coin landed).
pub const FLASH_MS: u128 = 900;

/// Block heartbeat by block order: (duration ms, tail cells, strength). Zone blocks arrive every
/// few seconds, so they whisper (Vivid only); region blocks run the full rule; prime blocks also
/// send a light around every panel.
fn heartbeat(order: u8) -> (f32, f32, f32) {
    match order {
        0 => (950.0, 16.0, 1.0),
        1 => (700.0, 10.0, 0.9),
        _ => (450.0, 4.0, 0.55),
    }
}

/// Heartbeat progress (0..1) when the current block light should play at this motion level.
fn beat_progress(app: &App) -> Option<(f32, u8)> {
    let order = app.beat_order;
    let allowed = match order {
        0 | 1 => app.motion().effects(),
        _ => app.motion() == Motion::Vivid,
    };
    let (ms, ..) = heartbeat(order);
    let elapsed = app.beat?.elapsed().as_millis() as f32;
    (allowed && elapsed < ms).then(|| (elapsed / ms, order))
}

/// A color mixed into the surface (`k` of `color`), for tinted chips; truecolor themes only.
pub fn tint(t: &Theme, color: Color, k: f32) -> Option<Color> {
    let (c, s) = (rgb(color)?, rgb(t.surface)?);
    let m = lerp(s, c, k);
    Some(Color::Rgb(m.0, m.1, m.2))
}

/// Background for a one-shot flash `ms` into its life: `color` fading to a faint tint.
pub fn flash_bg(t: &Theme, color: Color, ms: u128) -> Option<Color> {
    if ms >= FLASH_MS {
        return None;
    }
    let k = ease_out(ms as f32 / FLASH_MS as f32);
    tint(t, color, 0.55 - 0.40 * k)
}

/// Sparkline text colored by bar height: low bars sink toward the surface, the newest bar is lit.
pub fn spark_spans(app: &App, t: &Theme, text: &str, base: Color) -> Vec<ratatui::text::Span<'static>> {
    use ratatui::style::Style;
    use ratatui::text::Span;
    let bars = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
    let (Some(b), Some(s), Some(strong)) = (rgb(base), rgb(t.surface), rgb(t.strong)) else {
        return vec![Span::styled(text.to_string(), Style::default().fg(base))];
    };
    if !app.caps.truecolor || app.plain || app.no_color {
        return vec![Span::styled(text.to_string(), Style::default().fg(base))];
    }
    let count = text.chars().count();
    text.chars()
        .enumerate()
        .map(|(i, ch)| {
            let level = bars.iter().position(|bar| bar.starts_with(ch)).unwrap_or(3) as f32 / 7.0;
            let mut c = lerp(s, b, 0.45 + 0.55 * level);
            if i + 1 == count {
                c = lerp(b, strong, 0.4);
            }
            Span::styled(ch.to_string(), Style::default().fg(Color::Rgb(c.0, c.1, c.2)))
        })
        .collect()
}

/// Recolor a rendered multi-row sparkline by height: the higher a bar cell, the brighter.
pub fn ramp_bars(app: &App, buf: &mut Buffer, area: Rect, base: Color, t: &Theme) {
    let (Some(b), Some(s)) = (rgb(base), rgb(t.surface)) else { return };
    if !app.caps.truecolor || app.plain || app.no_color || area.height == 0 {
        return;
    }
    for y in area.top()..area.bottom() {
        let height = f32::from(area.bottom() - y) / f32::from(area.height);
        for x in area.left()..area.right() {
            if let Some(cell) = buf.cell_mut((x, y))
                && matches!(cell.symbol(), "▁" | "▂" | "▃" | "▄" | "▅" | "▆" | "▇" | "█")
            {
                let c = lerp(s, b, 0.4 + 0.6 * height);
                cell.set_fg(Color::Rgb(c.0, c.1, c.2));
            }
        }
    }
}

/// Edge-lit selection: the first cells of a selected row carry a hot edge in the background
/// that fades into the selection color. Text is untouched.
fn selection_edge(buf: &mut Buffer, t: &Theme) {
    let (Some(sel), Some(focus)) = (rgb(t.selection), t.accent_rgb.or_else(|| rgb(t.focus))) else { return };
    let area = buf.area;
    for y in area.top()..area.bottom() {
        let mut x = area.left();
        while x < area.right() {
            if buf.cell((x, y)).is_some_and(|c| c.bg == t.selection) {
                let start = x;
                while x < area.right() && buf.cell((x, y)).is_some_and(|c| c.bg == t.selection) {
                    x += 1;
                }
                if x - start >= 8 {
                    for (i, k) in [0.34f32, 0.20, 0.09].into_iter().enumerate() {
                        if let Some(cell) = buf.cell_mut((start + i as u16, y)) {
                            let c = lerp(sel, focus, k);
                            cell.set_bg(Color::Rgb(c.0, c.1, c.2));
                        }
                    }
                }
            } else {
                x += 1;
            }
        }
    }
}

fn rgb(c: Color) -> Option<Rgb> {
    match c {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        _ => None,
    }
}

fn lerp(a: Rgb, b: Rgb, k: f32) -> Rgb {
    let m = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * k.clamp(0.0, 1.0)).round() as u8;
    (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

/// Hue in degrees and saturation (HSV-style).
fn hue_sat((r, g, b): Rgb) -> (f32, f32) {
    let (r, g, b) = (f32::from(r) / 255.0, f32::from(g) / 255.0, f32::from(b) / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let d = max - min;
    if d < 1e-6 {
        return (0.0, 0.0);
    }
    let h = if max == r {
        60.0 * (((g - b) / d).rem_euclid(6.0))
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h, d / max)
}

fn ease_out(x: f32) -> f32 {
    1.0 - (1.0 - x.clamp(0.0, 1.0)).powi(3)
}

fn ease_in_out(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if x < 0.5 { 4.0 * x * x * x } else { 1.0 - (-2.0 * x + 2.0).powi(3) / 2.0 }
}

/// Edge colors for a theme: gradient start (accent), gradient end (a companion hue 40–150° away
/// from the accent, else a lighter accent) and the glint. Every color reads at ≥ 3:1 on the surface.
pub fn ramp(t: &Theme) -> Option<(Rgb, Rgb, Rgb)> {
    let accent = t.accent_rgb.or_else(|| rgb(t.focus))?;
    let strong = rgb(t.strong)?;
    rgb(t.surface)?;
    let (accent_hue, _) = hue_sat(accent);
    let companion = [t.link, t.qi, t.quai]
        .into_iter()
        .filter_map(rgb)
        .find(|c| {
            let (h, s) = hue_sat(*c);
            let d = (h - accent_hue).abs();
            s > 0.2 && (40.0..=150.0).contains(&d.min(360.0 - d))
        })
        .map(|c| lerp(c, accent, 0.25))
        .unwrap_or_else(|| lerp(accent, strong, 0.45));
    let start = super::images::readable(accent, t);
    let end = super::images::readable(companion, t);
    let glint = if t.light { lerp(start, (0, 0, 0), 0.25) } else { lerp(start, strong, 0.6) };
    Some((start, end, glint))
}

fn is_edge(symbol: &str) -> bool {
    matches!(symbol, "─" | "│" | "┌" | "┐" | "└" | "┘" | "┬" | "┴" | "├" | "┤" | "┼")
}

/// A bordered rectangle found in the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Panel {
    pub rect: Rect,
    pub focused: bool,
}

/// Every complete `┌┐└┘` rectangle in the frame, in reading order. A panel is focused when its
/// top-left corner is drawn in the focus color, bold (see `ui::panel`).
pub fn panels(buf: &Buffer, t: &Theme) -> Vec<Panel> {
    let area = buf.area;
    let symbol = |x: u16, y: u16| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or("");
    let mut out = Vec::new();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if symbol(x, y) != "┌" {
                continue;
            }
            let Some(x1) = (x + 1..area.right()).find(|cx| symbol(*cx, y) == "┐") else { continue };
            let Some(y1) = (y + 1..area.bottom()).find(|cy| !matches!(symbol(x, *cy), "│" | "├")) else { continue };
            if symbol(x, y1) != "└" || symbol(x1, y1) != "┘" || x1 < x + 2 || y1 < y + 2 {
                continue;
            }
            let focused = buf.cell((x, y)).is_some_and(|c| c.fg == t.focus && c.modifier.contains(Modifier::BOLD));
            out.push(Panel { rect: Rect::new(x, y, x1 - x + 1, y1 - y + 1), focused });
        }
    }
    out
}

/// Border cells clockwise from the top-left corner.
pub fn perimeter(r: Rect) -> Vec<(u16, u16)> {
    let (x0, y0, x1, y1) = (r.x, r.y, r.right() - 1, r.bottom() - 1);
    let mut cells = Vec::with_capacity(2 * (r.width as usize + r.height as usize));
    cells.extend((x0..=x1).map(|x| (x, y0)));
    cells.extend((y0 + 1..=y1).map(|y| (x1, y)));
    cells.extend((x0..x1).rev().map(|x| (x, y1)));
    cells.extend((y0 + 1..y1).rev().map(|y| (x0, y)));
    cells
}

/// Light the frame's edges. Sets `anim_step` when something moves on its own.
pub fn paint(app: &App, buf: &mut Buffer, t: &Theme) {
    app.eco.anim_step.set(None);
    if !app.caps.truecolor || app.plain || app.no_color || t.monochrome {
        return;
    }
    let Some((start, end, glint)) = ramp(t) else { return };
    let Some(surface) = rgb(t.surface) else { return };
    let quiet = matches!(app.modal, Modal::None);
    let vivid = app.motion() == Motion::Vivid && quiet && app.focused;
    let clock = app.eco.anim_ms;
    let intro = app.edge_intro.map(|s| s.elapsed().as_millis()).filter(|ms| *ms < INTRO_TOTAL_MS && app.motion().effects());
    let found = panels(buf, t);
    let angle = if vivid { 45.0 + 360.0 * (clock % TURN_MS) as f32 / TURN_MS as f32 } else { 45.0 };
    let (cos, sin) = (angle.to_radians().cos(), angle.to_radians().sin());
    let glinting = vivid && app.last_input.elapsed().as_secs() >= 2 && clock % GLINT_EVERY_MS < GLINT_LAP_MS;
    // A prime block sends a light around every panel.
    let prime_lap = beat_progress(app).filter(|(_, order)| *order == 0).map(|(p, _)| ease_in_out(p));
    let mut moving = false;
    selection_edge(buf, t);
    for (i, panel) in found.iter().enumerate() {
        let cells = perimeter(panel.rect);
        let n = cells.len() as f32;
        let reveal = intro.map(|ms| ease_out((ms as f32 - (i as u128 * INTRO_STAGGER_MS) as f32) / INTRO_MS as f32));
        let (cx, cy) =
            (f32::from(panel.rect.x) + f32::from(panel.rect.width) / 2.0, f32::from(panel.rect.y) + f32::from(panel.rect.height) / 2.0);
        let extent = (f32::from(panel.rect.width) / 2.0 * cos.abs() + f32::from(panel.rect.height) * sin.abs()).max(1.0);
        let head = glinting.then(|| ease_in_out((clock % GLINT_EVERY_MS) as f32 / GLINT_LAP_MS as f32) * (n + GLINT_TAIL));
        for (k, (x, y)) in cells.into_iter().enumerate() {
            let Some(cell) = buf.cell_mut((x, y)) else { continue };
            if !is_edge(cell.symbol()) {
                continue;
            }
            if let Some(r) = reveal {
                // Both directions from the top-left corner, meeting at the bottom-right.
                let along = (k as f32).min(n - k as f32) / (n / 2.0);
                if along > r {
                    cell.set_fg(Color::Rgb(surface.0, surface.1, surface.2));
                    continue;
                }
            }
            if let Some(p) = prime_lap {
                let behind = p * (n + 16.0) - k as f32;
                if (0.0..=16.0).contains(&behind) {
                    let base = rgb(cell.fg).unwrap_or(start);
                    let c = lerp(base, glint, (1.0 - behind / 16.0) * 0.9);
                    cell.set_fg(Color::Rgb(c.0, c.1, c.2));
                    continue;
                }
            }
            if !panel.focused {
                continue;
            }
            let (px, py) = (f32::from(x) + 0.5 - cx, (f32::from(y) + 0.5 - cy) * 2.0);
            let along = ((px * cos + py * sin) / extent * 0.5 + 0.5).clamp(0.0, 1.0);
            // Quantized so a slow turn only repaints cells whose color actually changes.
            let mut color = lerp(start, end, (along * 32.0).round() / 32.0);
            if let Some(head) = head {
                let behind = head - k as f32;
                if (0.0..=GLINT_TAIL).contains(&behind) {
                    color = lerp(color, glint, 1.0 - behind / GLINT_TAIL);
                }
            }
            cell.set_fg(Color::Rgb(color.0, color.1, color.2));
            moving |= vivid;
        }
    }
    header_rule(app, buf, t, start, end);
    if moving {
        let step = if glinting {
            GLINT_STEP_MS
        } else {
            // Wake for the next glint on time.
            let to_glint = GLINT_EVERY_MS - clock % GLINT_EVERY_MS;
            TURN_STEP_MS.min(to_glint.max(1))
        };
        app.eco.anim_step.set(Some(step));
        app.eco.anim_drawn.set(clock / step);
    }
}

/// A gradient hairline under the header (a colored underline, so no row is added), faded into
/// the header at both ends, with a light running along it when a block arrives.
fn header_rule(app: &App, buf: &mut Buffer, t: &Theme, start: Rgb, end: Rgb) {
    let area = buf.area;
    if app.locked || app.onboarding.is_some() || area.width < 60 || area.height < 18 {
        return;
    }
    let raised = rgb(t.raised).unwrap_or((0, 0, 0));
    let spark = rgb(t.ok).map(|ok| lerp(ok, rgb(t.strong).unwrap_or(ok), 0.4));
    let (_, tail, strength) = heartbeat(app.beat_order);
    let beat = beat_progress(app).map(|(p, _)| ease_out(p) * (f32::from(area.width) + tail));
    let w = f32::from(area.width);
    for x in area.left()..area.right() {
        let Some(cell) = buf.cell_mut((x, area.y)) else { continue };
        let fx = f32::from(x - area.left());
        let fade = (fx / 8.0).min((w - 1.0 - fx) / 8.0).clamp(0.0, 1.0);
        let mut color = lerp(raised, lerp(start, end, fx / w.max(1.0)), 0.25 + 0.75 * fade);
        if let (Some(head), Some(spark)) = (beat, spark) {
            let behind = head - fx;
            if (0.0..=tail).contains(&behind) {
                color = lerp(color, spark, (1.0 - behind / tail) * strength);
            }
        }
        cell.underline_color = Color::Rgb(color.0, color.1, color.2);
        cell.modifier.insert(Modifier::UNDERLINED);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::widgets::{Block, BorderType, Borders, Widget};

    #[test]
    fn perimeter_walks_clockwise_once() {
        let cells = perimeter(Rect::new(2, 1, 5, 4));
        assert_eq!(cells.len(), 2 * (5 + 4) - 4);
        assert_eq!(cells[0], (2, 1));
        assert_eq!(cells[4], (6, 1));
        assert_eq!(cells[7], (6, 4));
        assert_eq!(*cells.last().unwrap(), (2, 2));
        let unique: std::collections::HashSet<_> = cells.iter().collect();
        assert_eq!(unique.len(), cells.len());
    }

    #[test]
    fn panels_are_found_and_focus_detected() {
        let t = super::super::theme::resolve(std::path::Path::new("/nonexistent"), "quai-red", false, false).0;
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 12));
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Plain)
            .border_style(t.border(false))
            .title(" a ")
            .render(Rect::new(0, 0, 20, 6), &mut buf);
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Plain)
            .border_style(t.border(true))
            .title(" b ")
            .render(Rect::new(20, 0, 20, 12), &mut buf);
        let found = panels(&buf, &t);
        assert_eq!(
            found,
            vec![Panel { rect: Rect::new(0, 0, 20, 6), focused: false }, Panel { rect: Rect::new(20, 0, 20, 12), focused: true }]
        );
    }

    #[test]
    fn flashes_fade_and_selection_gets_a_hot_edge() {
        let t = super::super::theme::resolve(std::path::Path::new("/nonexistent"), "quai-red", false, false).0;
        let early = flash_bg(&t, t.ok, 0).unwrap();
        let late = flash_bg(&t, t.ok, FLASH_MS - 1).unwrap();
        assert_ne!(early, late);
        assert!(flash_bg(&t, t.ok, FLASH_MS).is_none());
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 2));
        for x in 2..14 {
            buf[(x, 1)].set_bg(t.selection);
        }
        selection_edge(&mut buf, &t);
        assert_ne!(buf[(2, 1)].bg, t.selection, "hot edge");
        assert_ne!(buf[(2, 1)].bg, buf[(4, 1)].bg, "fades");
        assert_eq!(buf[(5, 1)].bg, t.selection, "then the plain selection");
        assert_eq!(buf[(2, 0)].bg, Color::Reset, "other rows untouched");
    }

    #[test]
    fn every_builtin_theme_has_readable_edges() {
        for theme in super::super::themes::CATALOG {
            let t = super::super::theme::resolve(std::path::Path::new("/nonexistent"), theme.id, false, false).0;
            let (a, b, g) = ramp(&t).unwrap_or_else(|| panic!("{} has no edge ramp", theme.id));
            let surface = rgb(t.surface).unwrap();
            for c in [a, b] {
                assert!(super::super::theme::contrast(c, surface) >= 2.95, "{}: {c:?} on {surface:?}", theme.id);
            }
            assert_ne!(a, g, "{}: glint differs from the edge", theme.id);
        }
    }
}
