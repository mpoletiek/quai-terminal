//! Cell effects (tachyonfx) on the drawn frame: what plays, where, and what it may never touch.
//!
//! A [`Shot`] is one running effect. `ui::draw_frame` steps every shot after the screen is drawn
//! and before the footer, modals and toasts, so nothing plays over them; the shots are dropped,
//! not paused, when a modal opens, the window loses focus, the wallet locks or motion is below
//! Full. Each frame is a pure function of `(content, seed, elapsed)`: shots carry a fixed seed
//! and step by the clock. Everything an effect writes goes through [`apply`], which keeps the
//! cells it is told to keep (amounts, addresses, fees) and never lets a control character out.
//! docs/VISUAL_PLAN_2026-10-08.md §5 has what the trial of tachyonfx found.

// The glitch and the filter are for the Chain screen (docs/VISUAL_PLAN_2026-10-08.md S1).
#![cfg_attr(not(test), allow(dead_code))]

use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use tachyonfx::{CellFilter, Duration, Effect, Interpolation, IntoEffect, SimpleRng, fx};
use unicode_width::UnicodeWidthStr;

use super::kana::DECODE as KANA;

const HEX: &[u8; 16] = b"0123456789abcdef";

/// The header height's decode when a block lands: long enough to read as a decode, short enough
/// to be over before the eye goes back to work.
pub const HEIGHT_DECODE_MS: u32 = 320;

/// Where a shot plays: a fixed area, or wherever `text` is drawn on `row` this frame. A header
/// segment moves when others drop, and the shot ends when its text is gone.
pub enum Spot {
    Area(Rect),
    Text {
        row: u16,
        text: String,
    },
    /// The first place `text` is drawn, top to bottom (a panel whose row the layout decides).
    Anywhere(String),
}

/// Where `text` starts on `row`, if it is drawn there.
fn on_row(buf: &Buffer, row: u16, text: &str) -> Option<Rect> {
    let w = text.chars().count() as u16;
    if w == 0 || row >= buf.area.bottom() || w > buf.area.width {
        return None;
    }
    (buf.area.left()..=buf.area.right() - w)
        .find(|&x| text.chars().enumerate().all(|(i, c)| buf[(x + i as u16, row)].symbol().chars().eq(std::iter::once(c))))
        .map(|x| Rect::new(x, row, w, 1))
}

impl Spot {
    fn find(&self, buf: &Buffer) -> Option<Rect> {
        match self {
            Spot::Area(r) => Some(r.intersection(buf.area)).filter(|r| !r.is_empty()),
            Spot::Text { row, text } => on_row(buf, *row, text),
            Spot::Anywhere(text) => (buf.area.top()..buf.area.bottom()).find_map(|row| on_row(buf, row, text)),
        }
    }
}

/// One running effect: what it is (`tag`, so a newer one can replace it), where it plays and
/// what it keeps.
pub struct Shot {
    pub tag: &'static str,
    effect: Effect,
    spot: Spot,
    keep: Vec<Rect>,
    last: Option<Instant>,
}

impl Shot {
    pub fn new(tag: &'static str, effect: Effect, spot: Spot) -> Self {
        Shot { tag, effect, spot, keep: Vec::new(), last: None }
    }

    /// Cells the effect must leave exactly as drawn.
    pub fn keeping(mut self, keep: Vec<Rect>) -> Self {
        self.keep = keep;
        self
    }
}

/// Step every shot by the clock, not per frame (keys arriving mid-effect don't speed it up; a
/// stall is capped at 250 ms), and drop the ones that finished or lost their spot.
pub fn play(shots: &mut Vec<Shot>, buf: &mut Buffer, now: Instant) {
    shots.retain_mut(|s| {
        let Some(area) = s.spot.find(buf) else { return false };
        let dt = s.last.map_or(0, |at| now.saturating_duration_since(at).as_millis().min(250) as u32);
        s.last = Some(now);
        apply(&mut s.effect, dt, buf, area, &s.keep);
        !s.effect.done()
    });
}

/// The same rects as a tachyonfx filter, for effects that honour one. Not a guarantee:
/// `Effect::with_filter` is ignored by an effect that already has a filter, and `Glitch` always
/// has one. `apply` is what keeps money still.
pub fn sparing(keep: Vec<Rect>) -> CellFilter {
    CellFilter::PositionFn(tachyonfx::ref_count(move |p: Position| !keep.iter().any(|r| r.contains(p))))
}

/// Decode on change: the characters scramble and settle left to right over `ms`. Scrambled pairs
/// of cells show one katakana when `kana` (a font covers it), otherwise single hex digits. A kana
/// takes two cells, so it is only written over two scrambling cells and the second becomes its
/// blank tail, the way ratatui writes a wide glyph; the row never grows.
pub fn decode(ms: u32, seed: u32, kana: bool) -> Effect {
    fx::effect_fn_buf(seed, (ms, Interpolation::Linear), move |seed, ctx, buf| {
        let area = ctx.area.intersection(buf.area);
        let alpha = ctx.alpha();
        let frame = ctx.timer.alpha().to_bits() ^ *seed;
        let valid = ctx.filter().map(|f| f.validator());
        let open = |b: &Buffer, x: u16, y: u16| valid.as_ref().is_none_or(|v| v.is_valid(Position::new(x, y), &b[(x, y)]));
        for y in area.top()..area.bottom() {
            let settle = area.left() + (alpha * f32::from(area.width)).round() as u16;
            let mut x = settle.max(area.left());
            while x < area.right() {
                let scrambling =
                    |b: &Buffer, x: u16| open(b, x, y) && b.cell((x, y)).is_some_and(|c| c.symbol().is_ascii() && c.symbol() != " ");
                if !scrambling(buf, x) {
                    x += 1;
                    continue;
                }
                let r = noise(frame, u32::from(x), u32::from(y));
                if kana && x + 1 < area.right() && scrambling(buf, x + 1) && r.is_multiple_of(3) {
                    buf[(x, y)].set_symbol(KANA[(r as usize / 3) % KANA.len()]);
                    buf[(x + 1, y)].set_symbol(" ");
                    x += 2;
                } else {
                    let i = (r as usize) % 16;
                    buf[(x, y)].set_symbol(std::str::from_utf8(&HEX[i..=i]).unwrap_or("0"));
                    x += 1;
                }
            }
        }
    })
}

/// A seeded glitch. tachyonfx's own can shift `~` by +9 into U+0080–U+0087 (C1 controls), so
/// every effect that rewrites symbols runs through `printable` afterwards.
pub fn glitch(seed: u32, ratio: f32) -> Effect {
    fx::Glitch::builder()
        .cell_glitch_ratio(ratio)
        .action_start_delay_ms(0..400)
        .action_ms(60..240)
        .rng(SimpleRng::new(seed))
        .build()
        .into_effect()
}

/// Run `effect` for `dt` over `area`. The cells in `keep` (amounts, addresses, fees) come out
/// exactly as they went in, whatever the effect did; a two-cell glyph the effect left just before
/// a kept rect would cover its first cell on the terminal, so it becomes a space. Nothing written
/// can reach the terminal as a control character.
pub fn apply(effect: &mut Effect, dt_ms: u32, buf: &mut Buffer, area: Rect, keep: &[Rect]) {
    let saved: Vec<(Position, ratatui::buffer::Cell)> =
        keep.iter().flat_map(|r| r.intersection(buf.area).positions()).map(|p| (p, buf[p].clone())).collect();
    effect.process(Duration::from_millis(dt_ms), buf, area);
    for (p, cell) in saved {
        buf[p] = cell;
    }
    let whole = buf.area;
    for r in keep.iter().map(|r| r.intersection(whole)).filter(|r| r.left() > whole.left()) {
        for y in r.top()..r.bottom() {
            if buf[(r.left() - 1, y)].symbol().width() > 1 {
                buf[(r.left() - 1, y)].set_symbol(" ");
            }
        }
    }
    printable(buf, area);
}

/// Any symbol holding a C0/C1 control or DEL becomes a space.
pub fn printable(buf: &mut Buffer, area: Rect) {
    let area = area.intersection(buf.area);
    for p in area.positions() {
        if buf[p].symbol().chars().any(|c| c.is_control()) {
            buf[p].set_symbol(" ");
        }
    }
}

/// A two-cell glyph whose tail falls off the area's right edge would wrap on a real terminal.
pub fn straddles(buf: &Buffer, area: Rect) -> bool {
    (area.top()..area.bottom()).any(|y| buf.cell((area.right() - 1, y)).is_some_and(|c| c.symbol().width() > 1))
}

fn noise(a: u32, b: u32, c: u32) -> u32 {
    let mut x = u64::from(a).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ u64::from(b).wrapping_mul(0xBF58_476D_1CE4_E5B9) ^ u64::from(c) << 17;
    x ^= x >> 31;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    (x ^ (x >> 29)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Style};
    use tachyonfx::fx::EvolveSymbolSet;

    /// What a terminal holds: one entry per column, `None` for the tail of a two-cell glyph.
    /// Writes follow kitty: a glyph written over either half of a two-cell glyph erases the
    /// other half; a two-cell glyph also takes the next column.
    struct Term {
        w: u16,
        rows: Vec<Vec<Option<String>>>,
    }

    impl Term {
        fn new(w: u16, h: u16) -> Self {
            Term { w, rows: vec![vec![Some(" ".into()); w as usize]; h as usize] }
        }

        fn erase_around(&mut self, x: usize, y: usize) {
            let row = &mut self.rows[y];
            if row[x].is_none() && x > 0 {
                row[x - 1] = Some(" ".into());
            }
            if row[x].as_deref().is_some_and(|s| s.width() > 1) && x + 1 < row.len() {
                row[x + 1] = Some(" ".into());
            }
        }

        /// Apply a ratatui diff, as `Terminal::draw` hands it to the backend.
        fn apply(&mut self, updates: Vec<(u16, u16, &ratatui::buffer::Cell)>) -> Result<(), String> {
            for (x, y, cell) in updates {
                let (xu, yu) = (x as usize, y as usize);
                let s = cell.symbol();
                if s.chars().any(|c| c.is_control()) {
                    return Err(format!("control character {s:?} sent at {x},{y}"));
                }
                self.erase_around(xu, yu);
                if s.width() > 1 {
                    if x + 1 >= self.w {
                        return Err(format!("two-cell {s:?} at the last column {x},{y} wraps"));
                    }
                    self.erase_around(xu + 1, yu);
                    self.rows[yu][xu + 1] = None;
                }
                self.rows[yu][xu] = Some(s.to_string());
            }
            Ok(())
        }

        fn row(&self, y: usize) -> String {
            self.rows[y].iter().flatten().map(String::as_str).collect()
        }
    }

    /// What the buffer means a terminal to show: each symbol, its tail cells skipped.
    fn intended(buf: &Buffer, y: u16) -> String {
        let mut out = String::new();
        let mut x = buf.area.left();
        while x < buf.area.right() {
            let s = buf[(x, y)].symbol();
            out.push_str(s);
            x += s.width().max(1) as u16;
        }
        out
    }

    const W: u16 = 48;
    const H: u16 = 6;

    /// Mixed content: kana subtitles beside ASCII, the characters glitch can push past `~`, and a
    /// money row the filter must keep still.
    fn base() -> Buffer {
        let mut b = Buffer::empty(Rect::new(0, 0, W, H));
        let s = Style::default().fg(Color::Rgb(255, 58, 20));
        b.set_string(0, 0, "\u{25C8} PRIME CONVERGENCE \u{4E3B}\u{9396}\u{53CE}\u{675F} #10,390,405", s);
        b.set_string(0, 1, "\u{30B7}\u{30F3}\u{30AF}\u{30ED}\u{7387} SYNC 100.0% 64/64 vwxyz{|}~", s);
        b.set_string(0, 2, "hash 0x00a3f9c2e1d4b7 \u{627F}\u{8A8D} APPROVED", s);
        b.set_string(0, 3, "zone 0x6c1f...9a2e \u{5426}\u{6C7A} ~~~~ zzzz }}}}", s);
        b.set_string(0, 4, "balance:1,234.5678 QUAI fee=0.0021", s);
        b.set_string(0, 5, KANA.concat(), s);
        b
    }

    /// The amount and the fee on row 4, with decodable text on both sides of each.
    fn money() -> [Rect; 2] {
        [Rect::new(8, 4, 10, 1), Rect::new(28, 4, 6, 1)]
    }

    /// Every frame: the app redraws the content, the effect runs over it, ratatui diffs it against
    /// the last frame, and the terminal applies the diff. What the terminal shows must be what the
    /// buffer says, the money row must never change, and the last frame after the effect must be
    /// the content itself.
    fn run(name: &str, mut effect: Effect, ms: u32) -> Vec<String> {
        let area = Rect::new(0, 0, W, H);
        let content = base();
        let mut term = Term::new(W, H);
        let mut last = Buffer::empty(area);
        let mut faults = Vec::new();
        let mut t = 0;
        let mut frames = 0;
        let mut kana_frames = 0;
        while t <= ms {
            let mut buf = content.clone();
            if !effect.done() {
                apply(&mut effect, if t == 0 { 0 } else { 33 }, &mut buf, area, &money());
            }
            if straddles(&buf, area) {
                faults.push(format!("{name} t={t}: a two-cell glyph straddles the right edge"));
            }
            if let Err(e) = term.apply(last.diff(&buf)) {
                faults.push(format!("{name} t={t}: {e}"));
                break;
            }
            for y in 0..H {
                let (shown, meant) = (term.row(y as usize), intended(&buf, y));
                if shown != meant {
                    faults.push(format!("{name} t={t} row {y}: terminal shows {shown:?}, buffer means {meant:?}"));
                }
                if shown.width() != W as usize {
                    faults.push(format!("{name} t={t} row {y}: {} columns wide", shown.width()));
                }
            }
            kana_frames += usize::from((0..5).any(|y| (0..W).any(|x| buf[(x, y)].symbol().width() > 1 && content[(x, y)] != buf[(x, y)])));
            for p in money().iter().flat_map(|r| r.positions()) {
                if term.rows[p.y as usize][p.x as usize].as_deref() != Some(content[p].symbol()) {
                    faults.push(format!("{name} t={t}: the terminal hides money cell {},{}", p.x, p.y));
                    break;
                }
                if buf[p] != content[p] {
                    faults.push(format!("{name} t={t}: money cell {},{} changed to {:?}", p.x, p.y, buf[p].symbol()));
                    break;
                }
            }
            last = buf;
            t += 33;
            frames += 1;
        }
        // One clean frame after the effect: the screen is the content again.
        let _ = term.apply(last.diff(&content));
        for y in 0..H {
            if term.row(y as usize) != intended(&content, y) {
                faults.push(format!("{name}: row {y} not restored: {:?}", term.row(y as usize)));
            }
        }
        assert!(frames > 5, "{name} ran");
        if name.starts_with("decode_kana") {
            assert!(kana_frames > 3, "{name} wrote kana in only {kana_frames} frames");
        }
        faults
    }

    /// No filters: `apply` alone must keep the money row still.
    fn effects() -> Vec<(&'static str, Effect, u32)> {
        vec![
            ("glitch", glitch(7, 0.08), 1500),
            ("evolve_into", fx::evolve_into(EvolveSymbolSet::Shaded, 600), 700),
            ("evolve_from", fx::evolve_from(EvolveSymbolSet::Quadrants, 600), 700),
            ("dissolve", fx::dissolve(500).with_rng(SimpleRng::new(3)), 600),
            ("coalesce", fx::coalesce(500).with_rng(SimpleRng::new(3)), 600),
            ("sweep_in", fx::sweep_in(tachyonfx::Motion::LeftToRight, 10, 0, Color::Black, 500), 600),
            ("hsl_shift", fx::hsl_shift(Some([120.0, 0.0, 0.0]), None, 500), 600),
            ("decode_kana", decode(600, 11, true), 700),
            ("decode_hex", decode(600, 11, false), 700),
            // A kana written just left of a kept rect would cover its first cell.
            ("decode_kana_row4", decode(600, 5, true).with_area(Rect::new(0, 4, W, 1)), 700),
        ]
    }

    /// The trial's first question: two-cell glyphs come through every effect whole, on a terminal
    /// that follows kitty's rules, with money untouched.
    #[test]
    fn effects_keep_two_cell_glyphs_whole_and_money_still() {
        let faults: Vec<String> = effects().into_iter().flat_map(|(n, e, ms)| run(n, e, ms)).collect();
        assert!(faults.is_empty(), "{} faults:\n{}", faults.len(), faults.join("\n"));
    }

    /// A kana an effect writes over the cell before a kept rect, its tail on the rect's first
    /// cell: the cell comes back and the kana becomes a space, so the terminal shows the amount.
    #[test]
    fn a_kana_never_covers_a_kept_cell() {
        let area = Rect::new(0, 0, W, H);
        let mut e = fx::effect_fn_buf((), 300, |_, _, buf| {
            buf[(7, 4)].set_symbol(KANA[0]);
            buf[(8, 4)].set_symbol(" ");
        });
        let mut buf = base();
        apply(&mut e, 33, &mut buf, area, &money());
        assert_eq!(buf[(7, 4)].symbol(), " ");
        assert_eq!(buf[(8, 4)].symbol(), "1");
    }

    /// Why `apply` guards money itself: `with_filter` does nothing on a glitch.
    #[test]
    fn with_filter_is_ignored_by_glitch() {
        let area = Rect::new(0, 0, W, H);
        let mut e = glitch(7, 0.5).with_filter(sparing(money().to_vec()));
        let mut touched = false;
        for _ in 0..30 {
            let mut buf = base();
            e.process(Duration::from_millis(33), &mut buf, area);
            touched |= money().iter().flat_map(|r| r.positions()).any(|p| buf[p] != base()[p]);
        }
        assert!(touched, "glitch now honours with_filter; sparing() could become the guarantee");
        // The filter does reach a shader_fn effect.
        let mut d = decode(600, 11, true).with_filter(sparing(money().to_vec()));
        for _ in 0..10 {
            let mut buf = base();
            d.process(Duration::from_millis(33), &mut buf, area);
            assert!(money().iter().flat_map(|r| r.positions()).all(|p| buf[p] == base()[p]));
        }
    }

    /// Without `printable`, tachyonfx's glitch sends C1 control characters to the terminal.
    #[test]
    fn raw_glitch_can_emit_control_characters() {
        let area = Rect::new(0, 0, W, H);
        let mut e = glitch(7, 0.5);
        let mut hit = false;
        for _ in 0..60 {
            let mut buf = base();
            e.process(Duration::from_millis(33), &mut buf, area);
            hit |= buf.content.iter().any(|c| c.symbol().chars().any(char::is_control));
        }
        assert!(hit, "the raw glitch reached a control character; if not, the guard may be unneeded");
    }

    /// How many cells each effect makes the terminal repaint per frame on a full 160×48 screen of
    /// text (ratatui's diff), for the bytes-per-frame cost. Report only.
    #[test]
    #[ignore]
    fn cells_repainted_per_frame() {
        let area = Rect::new(0, 0, 160, 48);
        let mut content = Buffer::empty(area);
        for y in 0..48 {
            content.set_string(0, y, "0x00a3f9c2e1d4b7 balance:1,234.5678 QUAI ".repeat(4), Style::default().fg(Color::Rgb(215, 221, 229)));
        }
        let mut lines = Vec::new();
        for (name, mut e, ms) in effects() {
            let (mut last, mut total, mut frames, mut peak) = (content.clone(), 0usize, 0usize, 0usize);
            let mut t = 0;
            while t <= ms && !e.done() {
                let mut buf = content.clone();
                apply(&mut e, 33, &mut buf, area, &[]);
                let n = last.diff(&buf).len();
                (total, frames, peak) = (total + n, frames + 1, peak.max(n));
                last = buf;
                t += 33;
            }
            lines.push(format!("{name}: mean {} peak {peak} cells of 7680", total / frames.max(1)));
        }
        eprintln!("{}", lines.join("\n"));
    }

    /// A text spot follows its text and ends with it; the shot ends when its effect does.
    #[test]
    fn a_shot_plays_where_its_text_is_and_ends() {
        let mut shots = vec![Shot::new("height", decode(300, 1, true), Spot::Text { row: 2, text: "APPROVED".into() })];
        let mut buf = base();
        let start = Instant::now();
        play(&mut shots, &mut buf, start);
        assert_eq!(shots.len(), 1);
        let row = |b: &Buffer| intended(b, 2);
        let mut changed = false;
        for ms in [100, 200] {
            let mut b = base();
            play(&mut shots, &mut b, start + std::time::Duration::from_millis(ms));
            changed |= row(&b) != row(&base());
            assert_eq!(row(&b)[..20], row(&base())[..20], "left of the text is untouched");
        }
        assert!(changed, "the decode showed");
        let mut b = base();
        play(&mut shots, &mut b, start + std::time::Duration::from_millis(400));
        assert!(shots.is_empty(), "done after its 300 ms");
        assert_eq!(row(&b), row(&base()), "and the text is back");
        // An area spot keeps what it is told to keep.
        let mut area = vec![Shot::new("a", decode(300, 1, true), Spot::Area(Rect::new(0, 4, W, 1))).keeping(money().to_vec())];
        for ms in [0, 100, 200] {
            let mut b = base();
            play(&mut area, &mut b, start + std::time::Duration::from_millis(ms));
            assert!(money().iter().flat_map(|r| r.positions()).all(|p| b[p] == base()[p]));
        }
        // A text anywhere is found on whichever row has it.
        let mut anywhere = vec![Shot::new("y", decode(300, 1, true), Spot::Anywhere("APPROVED".into()))];
        let mut b = base();
        play(&mut anywhere, &mut b, start);
        assert_eq!(anywhere.len(), 1, "found on row 2");
        let mut gone = vec![Shot::new("x", decode(300, 1, true), Spot::Text { row: 2, text: "NOT THERE".into() })];
        play(&mut gone, &mut base(), start);
        assert!(gone.is_empty());
    }

    /// Same seed, same frames.
    #[test]
    fn effects_are_deterministic() {
        let area = Rect::new(0, 0, W, H);
        let frames = || {
            effects()
                .into_iter()
                .map(|(_, mut e, _)| {
                    let mut out = Vec::new();
                    for _ in 0..10 {
                        let mut buf = base();
                        apply(&mut e, 33, &mut buf, area, &money());
                        out.push(buf);
                    }
                    out
                })
                .collect::<Vec<_>>()
        };
        assert!(frames() == frames());
    }
}
