//! Recording a session for a demo: `QW_RECORD=<file>` tees what the terminal draws into a JSON
//! file a web page replays cell by cell (`scripts/player/quai-terminal-player.html`), after
//! quai-node-dashboard's recorder.
//!
//! The file holds a style table and, per frame, its time and the runs of cells that changed since
//! the frame before (`[y, x, style, text]`; the first frame and any after a resize are whole),
//! and which keys were pressed when. At most ten frames a second are kept. Pictures drawn through
//! kitty are not in it; the cells under them are.
//!
//! It records watch-only wallets only. A wallet that can sign can show a recovery phrase or a
//! key, and a demo file is passed around; with keys, nothing is recorded and it says so.

use std::collections::HashMap;
use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

/// The shortest gap between kept frames.
const FRAME_MS: u128 = 100;

pub struct Recorder {
    path: std::path::PathBuf,
    started: Instant,
    last: Option<Instant>,
    /// When the file was last written: every few seconds, so a session that is killed (its
    /// window closed) still leaves its recording.
    written: Instant,
    styles: Vec<(String, String, u8)>,
    index: HashMap<(String, String, u8), usize>,
    prev: Option<Buffer>,
    frames: Vec<Value>,
    keys: Vec<Value>,
}

/// A colour as `#rrggbb`; the terminal's own colours as `default` (the player's page colours).
fn hex(c: Color, default: &str) -> String {
    const ANSI: [&str; 16] = [
        "#000000", "#cd3131", "#0dbc79", "#e5e510", "#2472c8", "#bc3fbc", "#11a8cd", "#e5e5e5", "#666666", "#f14c4c", "#23d18b", "#f5f543",
        "#3b8eea", "#d670d6", "#29b8db", "#ffffff",
    ];
    match c {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(i) if i < 16 => ANSI[i as usize].into(),
        Color::Black => ANSI[0].into(),
        Color::Red => ANSI[1].into(),
        Color::Green => ANSI[2].into(),
        Color::Yellow => ANSI[3].into(),
        Color::Blue => ANSI[4].into(),
        Color::Magenta => ANSI[5].into(),
        Color::Cyan => ANSI[6].into(),
        Color::Gray => ANSI[7].into(),
        Color::DarkGray => ANSI[8].into(),
        Color::LightRed => ANSI[9].into(),
        Color::LightGreen => ANSI[10].into(),
        Color::LightYellow => ANSI[11].into(),
        Color::LightBlue => ANSI[12].into(),
        Color::LightMagenta => ANSI[13].into(),
        Color::LightCyan => ANSI[14].into(),
        Color::White => ANSI[15].into(),
        _ => default.into(),
    }
}

impl Recorder {
    /// A recorder for `path`, if the wallet is watch-only (None otherwise, said on stderr).
    pub fn new(path: std::path::PathBuf, watch_only: bool) -> Option<Recorder> {
        if !watch_only {
            eprintln!("QW_RECORD: records watch-only wallets only (a wallet with keys can show secrets); nothing recorded");
            return None;
        }
        Some(Recorder {
            path,
            started: Instant::now(),
            last: None,
            written: Instant::now(),
            styles: Vec::new(),
            index: HashMap::new(),
            prev: None,
            frames: Vec::new(),
            keys: Vec::new(),
        })
    }

    fn style(&mut self, fg: String, bg: String, flags: u8) -> usize {
        let key = (fg, bg, flags);
        if let Some(i) = self.index.get(&key) {
            return *i;
        }
        self.styles.push(key.clone());
        self.index.insert(key, self.styles.len() - 1);
        self.styles.len() - 1
    }

    /// A key was pressed: its name, at the next frame kept.
    pub fn key(&mut self, name: &str) {
        self.keys.push(json!([self.frames.len(), name]));
    }

    /// A frame was drawn: keep it, as what changed, unless the last was under 100 ms ago.
    pub fn frame(&mut self, buf: &Buffer, fg: Color, bg: Color) {
        let now = Instant::now();
        if self.last.is_some_and(|at| now.duration_since(at).as_millis() < FRAME_MS) {
            return;
        }
        self.last = Some(now);
        let (dfg, dbg) = (hex(fg, "#d0d0d0"), hex(bg, "#000000"));
        let area = buf.area;
        let prev = self.prev.take();
        let whole = prev.as_ref().is_none_or(|p| p.area != area);
        let mut runs = Vec::new();
        for y in area.top()..area.bottom() {
            let mut x = area.left();
            while x < area.right() {
                let changed = |xx: u16| whole || prev.as_ref().and_then(|p| p.cell((xx, y))) != buf.cell((xx, y));
                if !changed(x) {
                    x += 1;
                    continue;
                }
                let cell = &buf[(x, y)];
                let style = cell.style();
                let m = style.add_modifier;
                let flags = u8::from(m.contains(Modifier::BOLD))
                    | u8::from(m.contains(Modifier::DIM)) << 1
                    | u8::from(m.contains(Modifier::ITALIC)) << 2
                    | u8::from(m.contains(Modifier::REVERSED)) << 3
                    | u8::from(m.contains(Modifier::UNDERLINED)) << 4;
                let ix = self.style(hex(style.fg.unwrap_or(Color::Reset), &dfg), hex(style.bg.unwrap_or(Color::Reset), &dbg), flags);
                let x0 = x;
                let mut text = String::new();
                while x < area.right() {
                    let c = &buf[(x, y)];
                    if c.style() != style || !changed(x) {
                        break;
                    }
                    let sym = if c.symbol().is_empty() { " " } else { c.symbol() };
                    text.push_str(sym);
                    // A two-cell glyph's tail is not its own cell.
                    x += sym.width().max(1) as u16;
                }
                if x == x0 {
                    x += 1;
                    continue;
                }
                runs.push(json!([y - area.y, x0 - area.x, ix, text]));
            }
        }
        let t = now.duration_since(self.started).as_millis() as u64;
        self.frames.push(json!({ "t": t, "w": area.width, "h": area.height, "runs": runs }));
        self.prev = Some(buf.clone());
        if now.duration_since(self.written).as_secs() >= 5 {
            self.written = now;
            let _ = self.write();
        }
    }

    /// The recording as JSON.
    pub fn to_json(&self) -> Value {
        let (w, h) = self.prev.as_ref().map_or((0, 0), |b| (b.area.width, b.area.height));
        json!({
            "version": 1,
            "app": "quai-terminal",
            "w": w,
            "h": h,
            "styles": self.styles.iter().map(|(fg, bg, fl)| json!([fg, bg, fl])).collect::<Vec<_>>(),
            "frames": self.frames,
            "keys": self.keys,
        })
    }

    fn write(&self) -> std::io::Result<()> {
        // Whole or not at all: a reader never sees half a file.
        let partial = self.path.with_extension("json.part");
        std::fs::write(&partial, serde_json::to_vec(&self.to_json())?)?;
        std::fs::rename(&partial, &self.path)
    }

    /// Write the file (on leaving the TUI).
    pub fn finish(&self) -> std::io::Result<()> {
        self.write()?;
        eprintln!("QW_RECORD: {} frames written to {}", self.frames.len(), self.path.display());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    #[test]
    fn frames_keep_only_what_changed_and_wide_glyphs_whole() {
        let mut r = Recorder::new("unused".into(), true).unwrap();
        let mut a = Buffer::empty(Rect::new(0, 0, 8, 2));
        a.set_string(0, 0, "ab", Style::default().fg(Color::Rgb(1, 2, 3)));
        a.set_string(0, 1, "\u{6642}x", Style::default());
        r.frame(&a, Color::Rgb(200, 200, 200), Color::Rgb(0, 0, 0));
        let mut b = a.clone();
        b.set_string(1, 0, "Z", Style::default().fg(Color::Rgb(1, 2, 3)));
        r.last = None;
        r.frame(&b, Color::Rgb(200, 200, 200), Color::Rgb(0, 0, 0));
        let v = r.to_json();
        let first = v["frames"][0]["runs"].as_array().unwrap();
        assert!(first.iter().any(|run| run[3] == "ab"), "the first frame is whole: {first:?}");
        assert!(first.iter().any(|run| run[3].as_str().unwrap().starts_with("\u{6642}x")), "a wide glyph and the cell after it");
        let second = v["frames"][1]["runs"].as_array().unwrap();
        assert_eq!(second.len(), 1, "only the changed cell: {second:?}");
        assert_eq!(second[0][3], "Z");
        assert_eq!(v["styles"][second[0][2].as_u64().unwrap() as usize][0], "#010203");
    }

    #[test]
    fn frames_closer_than_a_tenth_of_a_second_are_skipped() {
        let mut r = Recorder::new("unused".into(), true).unwrap();
        let a = Buffer::empty(Rect::new(0, 0, 4, 1));
        r.frame(&a, Color::Reset, Color::Reset);
        r.frame(&a, Color::Reset, Color::Reset);
        assert_eq!(r.to_json()["frames"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_wallet_with_keys_is_never_recorded() {
        assert!(Recorder::new("unused".into(), false).is_none());
    }
}
