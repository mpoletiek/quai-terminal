//! Pictures drawn by code: an RGBA canvas with anti-aliased dots, glows, lines and curves (after
//! quai-node-dashboard's `raster.rs`), for charts and diagrams that cells draw coarsely.
//!
//! [`scene`] puts one on screen. The drawing runs off the UI thread, cached by a key that names
//! everything the drawing reads (data, size, colours); the kitty tier places it as a bitmap, the
//! cells tier draws it in half blocks, and the text tier gets `false` back and draws its own cell
//! version. Until a drawing is ready, the last one for the same `slot` and size stands in for it,
//! so new data doesn't blink the picture out.

// The Chain screen and the charts are the users (docs/VISUAL_PLAN_2026-10-08.md S1, S2).
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use wallet_core::media::Rendition;

use super::app::App;
use super::images::{RESERVED, bitmaps, encode_rgba, half_block, png_key};
use super::terminal::Tier;
use super::theme::Theme;

/// An RGB colour.
pub type Rgb = [u8; 3];

/// A theme colour as RGB, for drawing; None for terminal-palette colours (a picture needs exact
/// ones, so the cells draw instead).
pub fn rgb(c: ratatui::style::Color) -> Option<Rgb> {
    match c {
        ratatui::style::Color::Rgb(r, g, b) => Some([r, g, b]),
        _ => None,
    }
}

/// An RGBA image.
pub struct Canvas {
    pub w: usize,
    pub h: usize,
    /// Row-major RGBA, straight alpha.
    pub px: Vec<u8>,
}

impl Canvas {
    /// A canvas filled with `bg` at opacity `alpha` (0 is see-through: the page shows behind it).
    pub fn new(w: usize, h: usize, bg: Rgb, alpha: u8) -> Canvas {
        let px = std::iter::repeat_n([bg[0], bg[1], bg[2], alpha], w * h).flatten().collect();
        Canvas { w, h, px }
    }

    /// Blend `c` over (`x`, `y`) at opacity `a` (0..1). Coverage also raises the pixel's alpha,
    /// so marks on a see-through canvas stay visible.
    pub fn blend(&mut self, x: i64, y: i64, c: Rgb, a: f64) {
        if x < 0 || y < 0 || x >= self.w as i64 || y >= self.h as i64 || a <= 0.0 {
            return;
        }
        let i = (y as usize * self.w + x as usize) * 4;
        let a = a.min(1.0);
        let below = f64::from(self.px[i + 3]) / 255.0;
        let out = a + below * (1.0 - a);
        for (dst, src) in self.px[i..i + 3].iter_mut().zip(c) {
            let v = (f64::from(src) * a + f64::from(*dst) * below * (1.0 - a)) / out.max(f64::EPSILON);
            *dst = v.round().clamp(0.0, 255.0) as u8;
        }
        self.px[i + 3] = (out * 255.0).round().clamp(0.0, 255.0) as u8;
    }

    /// A soft-edged disc of radius `r`.
    pub fn dot(&mut self, x: f64, y: f64, r: f64, c: Rgb, a: f64) {
        let (x0, x1) = ((x - r - 1.0).floor() as i64, (x + r + 1.0).ceil() as i64);
        let (y0, y1) = ((y - r - 1.0).floor() as i64, (y + r + 1.0).ceil() as i64);
        for py in y0..=y1 {
            for px in x0..=x1 {
                let d = ((px as f64 + 0.5 - x).powi(2) + (py as f64 + 0.5 - y).powi(2)).sqrt();
                self.blend(px, py, c, a * (r + 0.5 - d).clamp(0.0, 1.0));
            }
        }
    }

    /// A radial falloff of radius `r`, strongest at the centre.
    pub fn glow(&mut self, x: f64, y: f64, r: f64, c: Rgb, a: f64) {
        let (x0, x1) = ((x - r).floor() as i64, (x + r).ceil() as i64);
        let (y0, y1) = ((y - r).floor() as i64, (y + r).ceil() as i64);
        for py in y0..=y1 {
            for px in x0..=x1 {
                let d = ((px as f64 + 0.5 - x).powi(2) + (py as f64 + 0.5 - y).powi(2)).sqrt() / r;
                if d < 1.0 {
                    self.blend(px, py, c, a * (1.0 - d).powi(2));
                }
            }
        }
    }

    /// A line `width` pixels thick with soft edges: each pixel's coverage is its distance from the
    /// segment, so a line at any angle is equally smooth.
    pub fn line(&mut self, (x0, y0): (f64, f64), (x1, y1): (f64, f64), width: f64, c: Rgb, a: f64) {
        let r = width / 2.0;
        let (dx, dy) = (x1 - x0, y1 - y0);
        let len2 = (dx * dx + dy * dy).max(f64::EPSILON);
        let (bx0, bx1) = ((x0.min(x1) - r - 1.0).floor() as i64, (x0.max(x1) + r + 1.0).ceil() as i64);
        let (by0, by1) = ((y0.min(y1) - r - 1.0).floor() as i64, (y0.max(y1) + r + 1.0).ceil() as i64);
        for py in by0..=by1 {
            for px in bx0..=bx1 {
                let (cx, cy) = (px as f64 + 0.5, py as f64 + 0.5);
                let t = (((cx - x0) * dx + (cy - y0) * dy) / len2).clamp(0.0, 1.0);
                let d = ((cx - x0 - t * dx).powi(2) + (cy - y0 - t * dy).powi(2)).sqrt();
                self.blend(px, py, c, a * (r + 0.5 - d).clamp(0.0, 1.0));
            }
        }
    }

    /// A polyline through `points`; joints are drawn once, not twice over.
    pub fn path(&mut self, points: &[(f64, f64)], width: f64, c: Rgb, a: f64) {
        if points.len() == 1 {
            self.dot(points[0].0, points[0].1, width / 2.0, c, a);
        }
        // Drawn on a scratch layer at full strength and blended once, so overlapping segment
        // ends don't double up into beads at every joint.
        let mut layer = Canvas::new(self.w, self.h, c, 0);
        for pair in points.windows(2) {
            layer.line(pair[0], pair[1], width, c, 1.0);
        }
        for (i, p) in layer.px.chunks_exact(4).enumerate() {
            if p[3] > 0 {
                self.blend((i % self.w) as i64, (i / self.w) as i64, c, a * f64::from(p[3]) / 255.0);
            }
        }
    }

    /// A quadratic Bézier from `p0` to `p2`, pulled toward `p1`, as a path of `steps` segments.
    pub fn curve(&mut self, p0: (f64, f64), p1: (f64, f64), p2: (f64, f64), width: f64, c: Rgb, a: f64) {
        let steps = (((p0.0 - p2.0).abs() + (p0.1 - p2.1).abs()) / 3.0).clamp(8.0, 96.0) as usize;
        self.path(&(0..=steps).map(|i| bezier(p0, p1, p2, i as f64 / steps as f64)).collect::<Vec<_>>(), width, c, a);
    }

    /// A filled rectangle (pixel-aligned).
    pub fn fill(&mut self, x: f64, y: f64, w: f64, h: f64, c: Rgb, a: f64) {
        for py in y.floor() as i64..(y + h).ceil() as i64 {
            for px in x.floor() as i64..(x + w).ceil() as i64 {
                self.blend(px, py, c, a);
            }
        }
    }

    /// The canvas as a rendition (PNG and pixels), which the picture paths take.
    pub fn rendition(self, key: u64) -> Rendition {
        let (w, h) = (self.w as u32, self.h as u32);
        let png = encode_rgba(w, h, &self.px).unwrap_or_default();
        Rendition { hash: format!("{key:016x}"), width: w, height: h, png, rgba: self.px, dominant: (0, 0, 0) }
    }
}

/// The point `t` (0..1) along a quadratic Bézier.
pub fn bezier(p0: (f64, f64), p1: (f64, f64), p2: (f64, f64), t: f64) -> (f64, f64) {
    let u = 1.0 - t;
    (u * u * p0.0 + 2.0 * u * t * p1.0 + t * t * p2.0, u * u * p0.1 + 2.0 * u * t * p1.1 + t * t * p2.1)
}

/// A key for a drawing from everything it reads: hash the data, the colours, anything.
pub fn key_of(parts: &impl Hash) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    parts.hash(&mut h);
    h.finish()
}

/// A scene's cache key: what it draws, at what pixel size.
type SceneKey = (u64, u32, u32);

/// Drawings done and being done, shared with the drawing threads. Small: a screen has a handful
/// of scenes, and the oldest go first.
struct Scenes {
    ready: HashMap<SceneKey, Arc<Rendition>>,
    order: VecDeque<SceneKey>,
    running: Vec<SceneKey>,
    /// The last ready drawing per slot and size, shown while a newer one renders.
    shown: HashMap<(&'static str, u32, u32), Arc<Rendition>>,
}

const KEEP: usize = 48;

static SCENES: std::sync::Mutex<Option<Scenes>> = std::sync::Mutex::new(None);
static FRESH: AtomicBool = AtomicBool::new(false);

/// True once since a drawing finished: a frame is due to show it.
pub fn poll() -> bool {
    FRESH.swap(false, Ordering::AcqRel)
}

/// The pixel size a scene is drawn at for `area`: the cells' pixels for bitmaps (capped, as a
/// screen-wide picture needs no more), and two pixels per cell height for half blocks.
fn pixel_size(app: &App, area: Rect) -> (u32, u32) {
    if bitmaps(app) {
        let (cw, ch) = (u32::from(app.term.caps.cell_px.0.max(1)), u32::from(app.term.caps.cell_px.1.max(1)));
        let (w, h) = (u32::from(area.width) * cw, u32::from(area.height) * ch);
        let shrink = (f64::from(w) / 1600.0).max(f64::from(h) / 1000.0).max(1.0);
        ((f64::from(w) / shrink) as u32, (f64::from(h) / shrink) as u32)
    } else {
        (u32::from(area.width), u32::from(area.height) * 2)
    }
}

/// Put a drawing on screen over `area`. `key` names everything `draw` reads; `slot` names the
/// picture (one per panel), so the last drawing stands in while a new one renders. Returns
/// false where no picture can be shown (text tier, plain mode, or nothing drawn yet): the caller
/// then draws its cell version.
pub fn scene(
    app: &App,
    buf: &mut Buffer,
    area: Rect,
    t: &Theme,
    slot: &'static str,
    key: u64,
    draw: impl FnOnce(&mut Canvas) + Send + 'static,
) -> bool {
    let cells = app.term.caps.tier == Tier::Cells && !app.term.plain;
    if area.width == 0 || area.height == 0 || !(bitmaps(app) || cells) {
        return false;
    }
    let (w, h) = pixel_size(app, area);
    let k = (key, w, h);
    let picture = {
        let Ok(mut guard) = SCENES.lock() else { return false };
        let s = guard.get_or_insert_with(|| Scenes {
            ready: HashMap::new(),
            order: VecDeque::new(),
            running: Vec::new(),
            shown: HashMap::new(),
        });
        match s.ready.get(&k).cloned() {
            Some(r) => {
                s.shown.insert((slot, w, h), r.clone());
                Some(r)
            }
            None => {
                if !s.running.contains(&k) {
                    s.running.push(k);
                    std::thread::spawn(move || {
                        let mut canvas = Canvas::new(w as usize, h as usize, [0, 0, 0], 0);
                        draw(&mut canvas);
                        let r = Arc::new(canvas.rendition(key));
                        if let Ok(mut guard) = SCENES.lock()
                            && let Some(s) = guard.as_mut()
                        {
                            s.running.retain(|x| *x != k);
                            s.ready.insert(k, r);
                            s.order.push_back(k);
                            while s.order.len() > KEEP {
                                if let Some(old) = s.order.pop_front() {
                                    s.ready.remove(&old);
                                }
                            }
                        }
                        FRESH.store(true, Ordering::Release);
                        super::term::wake();
                    });
                }
                s.shown.get(&(slot, w, h)).cloned()
            }
        }
    };
    let Some(r) = picture else { return false };
    if bitmaps(app) {
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_symbol(RESERVED).set_bg(t.surface);
                }
            }
        }
        let mut kitty = app.eco.media.kitty.borrow_mut();
        if kitty.len() < super::images::MAX_PLACEMENTS {
            kitty.push((area, (Arc::new(r.png.clone()), png_key(&r.png)), 0));
        }
    } else {
        half_block(app, buf, area, &r, t, 1.0, false);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha(c: &Canvas, x: usize, y: usize) -> u8 {
        c.px[(y * c.w + x) * 4 + 3]
    }

    #[test]
    fn a_dot_is_solid_inside_soft_at_its_edge_and_empty_outside() {
        let mut c = Canvas::new(20, 20, [0, 0, 0], 0);
        // Pixel 14's centre is 4.5 from the centre: 0.3 covered by a radius of 4.3.
        c.dot(10.0, 10.0, 4.3, [255, 0, 0], 1.0);
        assert_eq!(alpha(&c, 10, 10), 255);
        let edge = alpha(&c, 14, 10);
        assert!(edge > 0 && edge < 255, "edge {edge}");
        assert_eq!(alpha(&c, 18, 10), 0);
        assert_eq!(&c.px[(10 * 20 + 10) * 4..][..3], &[255, 0, 0]);
    }

    #[test]
    fn a_glow_fades_from_its_centre() {
        let mut c = Canvas::new(21, 1, [0, 0, 0], 0);
        c.glow(10.5, 0.5, 8.0, [0, 200, 255], 1.0);
        let a: Vec<u8> = (10..21).map(|x| alpha(&c, x, 0)).collect();
        assert!(a.windows(2).all(|w| w[0] >= w[1]), "{a:?}");
        assert!(a[0] > 200 && a[10] == 0);
    }

    #[test]
    fn a_line_covers_its_ends_and_nothing_far_from_it() {
        let mut c = Canvas::new(40, 40, [0, 0, 0], 255);
        c.line((5.0, 5.0), (35.0, 30.0), 2.0, [0, 255, 0], 1.0);
        let green = |x: usize, y: usize| c.px[(y * 40 + x) * 4 + 1];
        assert!(green(5, 5) > 128 && green(34, 29) > 128);
        assert_eq!(green(35, 5), 0);
    }

    #[test]
    fn a_path_does_not_bead_at_its_joints() {
        let mut c = Canvas::new(40, 10, [0, 0, 0], 0);
        c.path(&[(2.0, 5.0), (20.0, 5.0), (38.0, 5.0)], 2.0, [255, 255, 255], 0.5);
        // The joint is no stronger than the middle of a segment.
        assert_eq!(alpha(&c, 20, 5), alpha(&c, 11, 5));
    }

    #[test]
    fn a_curve_passes_through_its_ends() {
        assert_eq!(bezier((0.0, 0.0), (5.0, 9.0), (10.0, 0.0), 0.0), (0.0, 0.0));
        assert_eq!(bezier((0.0, 0.0), (5.0, 9.0), (10.0, 0.0), 1.0), (10.0, 0.0));
        let mut c = Canvas::new(30, 20, [0, 0, 0], 0);
        c.curve((2.0, 18.0), (15.0, -10.0), (28.0, 18.0), 2.0, [9, 9, 9], 1.0);
        assert!(alpha(&c, 15, 4) > 0, "the curve rises toward its control point");
    }

    #[test]
    fn the_rendition_is_a_png_of_the_canvas() {
        let mut c = Canvas::new(8, 4, [10, 20, 30], 255);
        c.fill(0.0, 0.0, 2.0, 2.0, [200, 0, 0], 1.0);
        let r = c.rendition(7);
        let decoder = png::Decoder::new(std::io::Cursor::new(r.png.clone()));
        let mut reader = decoder.read_info().unwrap();
        let mut out = vec![0; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut out).unwrap();
        assert_eq!((r.width, r.height), (8, 4));
        assert_eq!(&out[..4], &[200, 0, 0, 255]);
        assert_eq!(out, r.rgba);
    }

    /// Text tier: nothing, the caller draws cells. Cells tier: half blocks once drawn. Pixels
    /// tier: the cells are held for a bitmap placed over them. A new key keeps showing the old
    /// drawing for its slot until it is ready.
    #[test]
    fn a_scene_shows_on_every_tier_that_can_and_stands_in_while_redrawn() {
        let (_dir, mut app) = super::super::ui::tests::populated_app();
        let t = app.theme.clone();
        let area = Rect::new(2, 1, 20, 5);
        let draw = |c: &mut Canvas| c.fill(0.0, 0.0, c.w as f64, c.h as f64, [250, 60, 20], 1.0);
        let until = |app: &App, key: u64| {
            for _ in 0..200 {
                let mut buf = Buffer::empty(Rect::new(0, 0, 30, 8));
                if scene(app, &mut buf, area, &t, "test", key, draw) {
                    return buf;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("the scene never showed");
        };
        app.term.caps.tier = Tier::Text;
        assert!(!scene(&app, &mut Buffer::empty(Rect::new(0, 0, 30, 8)), area, &t, "test", 1, draw));
        app.term.caps.tier = Tier::Cells;
        let buf = until(&app, 0xC0FFEE);
        assert!(area.positions().all(|p| buf[p].symbol() == "▀" || buf[p].symbol() == "█"), "half blocks");
        // A new drawing for the same slot: the old one stands in at once.
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 8));
        assert!(scene(&app, &mut buf, area, &t, "test", 0xC0FFEF, |c: &mut Canvas| c.fill(0.0, 0.0, 1.0, 1.0, [0, 0, 0], 1.0)));
        app.term.caps.tier = Tier::Pixels;
        app.term.plain = false;
        let buf = until(&app, 0xBEEF);
        assert!(area.positions().all(|p| buf[p].symbol() == RESERVED));
        assert!(app.eco.media.kitty.borrow().iter().any(|(r, ..)| *r == area), "a bitmap placed over them");
    }

    #[test]
    fn key_of_changes_with_what_is_drawn() {
        assert_eq!(key_of(&(1, "a")), key_of(&(1, "a")));
        assert_ne!(key_of(&(1, "a")), key_of(&(2, "a")));
    }
}
