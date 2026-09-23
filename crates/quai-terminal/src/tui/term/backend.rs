//! The ratatui backend: cells to escape sequences, one frame at a time.
//!
//! A frame is built in memory and leaves in one write, wrapped in synchronized output (DEC 2026)
//! where the terminal has it, so the terminal shows it whole: no tearing mid-frame, no border
//! half-recolored. Out-of-band writes — kitty images, the clipboard, the bell, the title — are
//! queued into the same frame, inside the same synchronized block, instead of racing the cells on
//! a second handle. Colors are brought down to the terminal's depth here, once, for every
//! widget: a theme is truecolor, and a 256-color terminal gets the nearest palette entries rather
//! than whatever it makes of a sequence it doesn't speak.

use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use ratatui::style::{Color, Modifier};
use std::fmt::Write as _;
use std::io::{self, Write};

/// How many colors the terminal can show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Depth {
    TrueColor,
    Ansi256,
    Ansi16,
}

/// Where the frame goes: the terminal, or (in tests) memory.
pub trait Sink: Write {
    fn size(&self) -> io::Result<Size>;
    fn window_size(&self) -> io::Result<WindowSize>;
}

/// Text drawn larger than a cell with kitty's text sizing protocol (OSC 66), over cells the
/// frame filled with [`BIG_TEXT_CELL`]. The placeholder is what makes it go away cleanly: when
/// the text is no longer wanted, whatever replaces it differs from the placeholder, so ratatui
/// rewrites those cells, and writing any cell of a sized character erases all of it.
#[derive(Clone, Debug, PartialEq)]
pub struct BigText {
    pub x: u16,
    pub y: u16,
    /// Each character is `scale` cells wide and `scale` rows tall.
    pub scale: u8,
    pub text: String,
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
}

/// Underline styles. Ratatui has one underline; these ride on the two blink modifiers, which
/// the wallet never blinks with, and the backend turns them into curly (`4:3`) and double
/// (`4:2`) underlines where the terminal draws them, or a plain one where it doesn't.
pub mod underline {
    use ratatui::style::Modifier;
    /// Something to look at twice: a fee above the policy.
    pub const CURLY: Modifier = Modifier::UNDERLINED.union(Modifier::SLOW_BLINK);
    /// The figure that settles it: a review's total.
    pub const DOUBLE: Modifier = Modifier::UNDERLINED.union(Modifier::RAPID_BLINK);
}

/// A cell's underline as the terminal should draw it: `4`, `4:3` or `4:2`, or none.
fn underline_code(m: Modifier, styled: bool) -> Option<&'static str> {
    if !m.contains(Modifier::UNDERLINED) {
        return None;
    }
    Some(match (styled, m.contains(Modifier::SLOW_BLINK), m.contains(Modifier::RAPID_BLINK)) {
        (true, true, _) => "4:3",
        (true, _, true) => "4:2",
        _ => "4",
    })
}

thread_local! {
    /// This frame's hyperlinks, set when the frame is composed and read as its cells are written
    /// (the two happen one after the other, on the UI thread).
    static LINKS: std::cell::RefCell<Vec<super::super::links::Link>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The hyperlinks for the frame about to be written (see `links`).
pub fn set_links(links: Vec<super::super::links::Link>) {
    LINKS.with(|l| *l.borrow_mut() = links);
}

/// What a frame puts in the cells a [`BigText`] covers (a blank braille cell: drawn as nothing,
/// and never what another widget draws).
pub const BIG_TEXT_CELL: &str = "\u{2800}";

impl BigText {
    pub fn width(&self) -> u16 {
        self.text.chars().count() as u16 * u16::from(self.scale)
    }
}

pub struct FrameBackend<S: Sink> {
    sink: S,
    /// The frame being built.
    buf: Vec<u8>,
    /// Out-of-band bytes for the end of this frame (images, OSC, bell).
    queued: Vec<u8>,
    /// Inside `begin`/`end`: ratatui's own flushes wait for `end`.
    in_frame: bool,
    pub sync: bool,
    pub depth: Depth,
    /// Bytes written by the last frame, for the timing trace.
    pub last_frame_bytes: usize,
    /// A background drawn as the terminal's own (`49`) instead: the theme's page color when it
    /// should let the terminal's background and opacity show through.
    pub see_through: Option<Color>,
    /// Nearest palette entries already worked out (a theme uses a few dozen colors, each looked
    /// up per styled cell per frame on a 256-color terminal).
    nearest: std::collections::HashMap<(u8, u8, u8), u8>,
    /// Columns written per row since the sized text was last placed (first, last), and whether
    /// the screen was cleared: either can erase sized text, which is then sent again.
    touched: Vec<Option<(u16, u16)>>,
    cleared: bool,
    /// Sized text as last sent.
    big_sent: Vec<BigText>,
    /// Curly and double underlines are drawn (the terminal answered the underline-color
    /// probe, which the same terminals implement alongside them).
    pub styled_underline: bool,
}

impl<S: Sink> FrameBackend<S> {
    pub fn new(sink: S, depth: Depth, sync: bool) -> Self {
        FrameBackend {
            sink,
            buf: Vec::with_capacity(256 * 1024),
            queued: Vec::new(),
            in_frame: false,
            sync,
            depth,
            last_frame_bytes: 0,
            see_through: None,
            nearest: std::collections::HashMap::new(),
            touched: Vec::new(),
            cleared: false,
            big_sent: Vec::new(),
            styled_underline: false,
        }
    }

    pub fn sink_mut(&mut self) -> &mut S {
        &mut self.sink
    }

    /// Start a frame: from here to `end` everything the terminal receives is shown at once.
    pub fn begin(&mut self) {
        self.in_frame = true;
        if self.sync {
            self.buf.extend_from_slice(b"\x1b[?2026h");
        }
    }

    /// Add bytes to go out with this frame, after its cells (or now, outside a frame).
    pub fn queue(&mut self, bytes: &[u8]) {
        self.queued.extend_from_slice(bytes);
        if !self.in_frame {
            let _ = self.flush_now();
        }
    }

    /// Place this frame's sized text after its cells: whatever is new, and whatever the cells
    /// just written (or a clear) may have erased. Text that stays untouched is not sent again.
    pub fn big_text(&mut self, wanted: &[BigText]) {
        let mut out = String::new();
        for b in wanted {
            let hit = (b.y..b.y + u16::from(b.scale)).any(|y| {
                self.touched.get(y as usize).copied().flatten().is_some_and(|(first, last)| first < b.x + b.width() && last >= b.x)
            });
            if !(self.cleared || hit || !self.big_sent.contains(b)) {
                continue;
            }
            let mut sgr = String::from("0");
            if b.bold {
                sgr.push_str(";1");
            }
            self.color(b.fg, Layer::Fg, &mut sgr);
            let bg = if Some(b.bg) == self.see_through { Color::Reset } else { b.bg };
            self.color(bg, Layer::Bg, &mut sgr);
            let _ = write!(out, "\x1b[{};{}H\x1b[{sgr}m\x1b]66;s={};{}\x07\x1b[0m", b.y + 1, b.x + 1, b.scale, b.text);
        }
        self.buf.extend_from_slice(out.as_bytes());
        self.big_sent = wanted.to_vec();
        self.touched.clear();
        self.cleared = false;
    }

    /// Finish the frame and send it, queued bytes included, in one write.
    pub fn end(&mut self) -> io::Result<()> {
        self.in_frame = false;
        self.flush_now()
    }

    fn flush_now(&mut self) -> io::Result<()> {
        if !self.queued.is_empty() {
            let queued = std::mem::take(&mut self.queued);
            self.buf.extend_from_slice(&queued);
            self.queued = queued;
            self.queued.clear();
        }
        if self.sync && self.buf.starts_with(b"\x1b[?2026h") {
            self.buf.extend_from_slice(b"\x1b[?2026l");
        }
        if self.buf.is_empty() {
            return Ok(());
        }
        self.last_frame_bytes = self.buf.len();
        let result = self.sink.write_all(&self.buf).and_then(|_| self.sink.flush());
        self.buf.clear();
        result
    }

    fn color(&mut self, c: Color, layer: Layer, out: &mut String) {
        let (rgb_prefix, idx_prefix, reset) = match layer {
            Layer::Fg => ("38;2", "38;5", "39"),
            Layer::Bg => ("48;2", "48;5", "49"),
            Layer::Underline => ("58;2", "58;5", "59"),
        };
        let base16 = |n: u8| -> String {
            // 0–7 and 8–15 have their own codes for fg and bg; underline has only the indexed form.
            match layer {
                Layer::Fg => (if n < 8 { 30 + n as u16 } else { 90 + (n - 8) as u16 }).to_string(),
                Layer::Bg => (if n < 8 { 40 + n as u16 } else { 100 + (n - 8) as u16 }).to_string(),
                Layer::Underline => format!("58;5;{n}"),
            }
        };
        let code = match c {
            Color::Reset => reset.to_string(),
            Color::Black => base16(0),
            Color::Red => base16(1),
            Color::Green => base16(2),
            Color::Yellow => base16(3),
            Color::Blue => base16(4),
            Color::Magenta => base16(5),
            Color::Cyan => base16(6),
            Color::Gray => base16(7),
            Color::DarkGray => base16(8),
            Color::LightRed => base16(9),
            Color::LightGreen => base16(10),
            Color::LightYellow => base16(11),
            Color::LightBlue => base16(12),
            Color::LightMagenta => base16(13),
            Color::LightCyan => base16(14),
            Color::White => base16(15),
            Color::Indexed(i) => match self.depth {
                Depth::Ansi16 if i >= 16 => base16(nearest16(palette256(i))),
                _ => format!("{idx_prefix};{i}"),
            },
            Color::Rgb(r, g, b) => match self.depth {
                Depth::TrueColor => format!("{rgb_prefix};{r};{g};{b}"),
                Depth::Ansi256 => format!("{idx_prefix};{}", *self.nearest.entry((r, g, b)).or_insert_with(|| nearest256((r, g, b)))),
                Depth::Ansi16 => base16(nearest16((r, g, b))),
            },
        };
        if !out.is_empty() {
            out.push(';');
        }
        out.push_str(&code);
    }
}

#[derive(Clone, Copy)]
enum Layer {
    Fg,
    Bg,
    Underline,
}

/// The xterm 256-color palette entry `i` as RGB (16–255; 0–15 are the terminal's own).
fn palette256(i: u8) -> (u8, u8, u8) {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    match i {
        16..=231 => {
            let n = i - 16;
            (LEVELS[(n / 36) as usize], LEVELS[(n / 6 % 6) as usize], LEVELS[(n % 6) as usize])
        }
        232..=255 => {
            let v = 8 + (i - 232) * 10;
            (v, v, v)
        }
        _ => ANSI16[i as usize],
    }
}

/// Typical values for the 16 ANSI colors, for matching only (the terminal draws its own).
const ANSI16: [(u8, u8, u8); 16] = [
    (0, 0, 0),
    (205, 49, 49),
    (13, 188, 121),
    (229, 229, 16),
    (36, 114, 200),
    (188, 63, 188),
    (17, 168, 205),
    (229, 229, 229),
    (102, 102, 102),
    (241, 76, 76),
    (35, 209, 139),
    (245, 245, 67),
    (59, 142, 234),
    (214, 112, 214),
    (41, 184, 219),
    (255, 255, 255),
];

fn distance(a: (u8, u8, u8), b: (u8, u8, u8)) -> u32 {
    // Weighted for the eye's sensitivity, which matters most between neighbouring greys.
    let d = |x: u8, y: u8| (i32::from(x) - i32::from(y)).pow(2) as u32;
    2 * d(a.0, b.0) + 4 * d(a.1, b.1) + 3 * d(a.2, b.2)
}

/// The nearest xterm palette entry (16–255) to an RGB color.
pub fn nearest256(c: (u8, u8, u8)) -> u8 {
    (16..=255u8).min_by_key(|&i| distance(c, palette256(i))).unwrap_or(16)
}

/// The nearest of the 16 ANSI colors.
pub fn nearest16(c: (u8, u8, u8)) -> u8 {
    (0..16u8).min_by_key(|&i| distance(c, ANSI16[i as usize])).unwrap_or(7)
}

fn modifier_codes(from: Modifier, to: Modifier, out: &mut String) {
    let removed = from - to;
    let added = to - from;
    let mut push = |s: &str| {
        if !out.is_empty() {
            out.push(';');
        }
        out.push_str(s);
    };
    // 22 clears both bold and dim, so whichever of the two stays is set again after it.
    if removed.intersects(Modifier::BOLD | Modifier::DIM) {
        push("22");
        if to.contains(Modifier::BOLD) {
            push("1");
        }
        if to.contains(Modifier::DIM) {
            push("2");
        }
    }
    if removed.contains(Modifier::ITALIC) {
        push("23");
    }
    if removed.contains(Modifier::UNDERLINED) {
        push("24");
    }
    if removed.intersects(Modifier::SLOW_BLINK | Modifier::RAPID_BLINK) {
        push("25");
    }
    if removed.contains(Modifier::REVERSED) {
        push("27");
    }
    if removed.contains(Modifier::HIDDEN) {
        push("28");
    }
    if removed.contains(Modifier::CROSSED_OUT) {
        push("29");
    }
    if added.contains(Modifier::BOLD) {
        push("1");
    }
    if added.contains(Modifier::DIM) {
        push("2");
    }
    if added.contains(Modifier::ITALIC) {
        push("3");
    }
    if added.contains(Modifier::UNDERLINED) {
        push("4");
    }
    if added.contains(Modifier::SLOW_BLINK) {
        push("5");
    }
    if added.contains(Modifier::RAPID_BLINK) {
        push("6");
    }
    if added.contains(Modifier::REVERSED) {
        push("7");
    }
    if added.contains(Modifier::HIDDEN) {
        push("8");
    }
    if added.contains(Modifier::CROSSED_OUT) {
        push("9");
    }
}

impl<S: Sink> Write for FrameBackend<S> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.in_frame { Ok(()) } else { self.flush_now() }
    }
}

impl<S: Sink> Backend for FrameBackend<S> {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let mut out = String::with_capacity(64 * 1024);
        let (mut fg, mut bg, mut ul, mut modifier) = (Color::Reset, Color::Reset, Color::Reset, Modifier::empty());
        let mut underline: Option<&'static str> = None;
        // The link the cells being written belong to (an index into this frame's links).
        let links = LINKS.with(|l| l.borrow().clone());
        let mut open: Option<usize> = None;
        let mut last: Option<(u16, u16)> = None;
        // Every frame starts from known attributes: the cursor may sit anywhere after a queued
        // write, and whatever SGR the previous frame ended with is gone after its reset.
        out.push_str("\x1b[0m");
        for (x, y, cell) in content {
            let row = y as usize;
            if self.touched.len() <= row {
                self.touched.resize(row + 1, None);
            }
            self.touched[row] = Some(self.touched[row].map_or((x, x), |(first, last)| (first.min(x), last.max(x))));
            if last != Some((x.wrapping_sub(1), y)) {
                let _ = write!(out, "\x1b[{};{}H", y + 1, x + 1);
            }
            last = Some((x, y));
            let mut sgr = String::new();
            // The blink bits are underline styles here (`underline`), never blinking.
            let wanted_underline = underline_code(cell.modifier, self.styled_underline);
            let cell_modifier = cell.modifier - (Modifier::SLOW_BLINK | Modifier::RAPID_BLINK);
            if cell_modifier != modifier {
                modifier_codes(modifier, cell_modifier, &mut sgr);
                modifier = cell_modifier;
                if !modifier.contains(Modifier::UNDERLINED) {
                    underline = None;
                } else if sgr.split(';').any(|c| c == "4") {
                    underline = Some("4");
                }
            }
            if let Some(code) = wanted_underline.filter(|w| Some(*w) != underline) {
                // A styled underline replaces the plain `4` the modifier change just asked for.
                if code != "4" && sgr.split(';').any(|c| c == "4") {
                    sgr = sgr.split(';').filter(|c| *c != "4").collect::<Vec<_>>().join(";");
                }
                if !sgr.is_empty() {
                    sgr.push(';');
                }
                sgr.push_str(code);
                underline = Some(code);
            }
            if cell.fg != fg {
                self.color(cell.fg, Layer::Fg, &mut sgr);
                fg = cell.fg;
            }
            let cell_bg = if Some(cell.bg) == self.see_through { Color::Reset } else { cell.bg };
            if cell_bg != bg {
                self.color(cell_bg, Layer::Bg, &mut sgr);
                bg = cell_bg;
            }
            if cell.underline_color != ul {
                self.color(cell.underline_color, Layer::Underline, &mut sgr);
                ul = cell.underline_color;
            }
            if !sgr.is_empty() {
                let _ = write!(out, "\x1b[{sgr}m");
            }
            let link = links.iter().position(|l| l.y == y && (l.x..l.end).contains(&x));
            if link != open {
                match link {
                    Some(i) => {
                        let _ = write!(out, "\x1b]8;;{}\x1b\\", links[i].url);
                    }
                    None => out.push_str("\x1b]8;;\x1b\\"),
                }
                open = link;
            }
            out.push_str(cell.symbol());
        }
        if open.is_some() {
            out.push_str("\x1b]8;;\x1b\\");
        }
        out.push_str("\x1b[0m");
        self.buf.extend_from_slice(out.as_bytes());
        Ok(())
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.buf.extend_from_slice(b"\x1b[?25l");
        Write::flush(self)
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.buf.extend_from_slice(b"\x1b[?25h");
        Write::flush(self)
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        // A fullscreen viewport never asks, and a reply read here would race the input loop.
        Ok(Position::ORIGIN)
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        let p = position.into();
        let _ = write!(self, "\x1b[{};{}H", p.y + 1, p.x + 1);
        Write::flush(self)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.clear_region(ClearType::All)
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.cleared = true;
        let seq: &[u8] = match clear_type {
            ClearType::All => b"\x1b[2J",
            ClearType::AfterCursor => b"\x1b[J",
            ClearType::BeforeCursor => b"\x1b[1J",
            ClearType::CurrentLine => b"\x1b[2K",
            ClearType::UntilNewLine => b"\x1b[K",
        };
        self.buf.extend_from_slice(seq);
        Write::flush(self)
    }

    fn size(&self) -> io::Result<Size> {
        self.sink.size()
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.sink.window_size()
    }

    fn flush(&mut self) -> io::Result<()> {
        Write::flush(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Mem {
        written: Vec<Vec<u8>>,
        pending: Vec<u8>,
    }
    impl Write for Mem {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.pending.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            if !self.pending.is_empty() {
                self.written.push(std::mem::take(&mut self.pending));
            }
            Ok(())
        }
    }
    impl Sink for Mem {
        fn size(&self) -> io::Result<Size> {
            Ok(Size::new(20, 4))
        }
        fn window_size(&self) -> io::Result<WindowSize> {
            Ok(WindowSize { columns_rows: Size::new(20, 4), pixels: Size::new(0, 0) })
        }
    }

    /// A whole frame — cells and everything queued with it — leaves in one write, between the
    /// synchronized-output marks, however often ratatui flushes along the way.
    #[test]
    fn a_frame_is_one_synchronized_write() {
        let mut b = FrameBackend::new(Mem::default(), Depth::TrueColor, true);
        let mut term = ratatui::Terminal::new(b).unwrap();
        term.backend_mut().begin();
        term.draw(|f| f.render_widget(ratatui::widgets::Paragraph::new("hi"), f.area())).unwrap();
        term.backend_mut().queue(b"\x07");
        term.backend_mut().end().unwrap();
        b = std::mem::replace(term.backend_mut(), FrameBackend::new(Mem::default(), Depth::TrueColor, true));
        let frames: Vec<String> = b.sink.written.iter().map(|w| String::from_utf8_lossy(w).into_owned()).collect();
        let last = frames.last().expect("a frame");
        assert!(last.starts_with("\x1b[?2026h") && last.ends_with("\x07\x1b[?2026l"), "{last:?}");
        assert!(last.contains("hi"));
    }

    #[test]
    fn colors_come_down_to_the_terminals_depth() {
        let b = |d| FrameBackend::new(Mem::default(), d, false);
        let mut s = String::new();
        b(Depth::TrueColor).color(Color::Rgb(255, 0, 0), Layer::Fg, &mut s);
        assert_eq!(s, "38;2;255;0;0");
        s.clear();
        b(Depth::Ansi256).color(Color::Rgb(255, 0, 0), Layer::Fg, &mut s);
        assert_eq!(s, "38;5;196", "pure red is palette 196");
        s.clear();
        b(Depth::Ansi256).color(Color::Rgb(128, 128, 128), Layer::Bg, &mut s);
        assert_eq!(s, "48;5;244", "a mid grey lands on the grey ramp");
        s.clear();
        b(Depth::Ansi16).color(Color::Rgb(250, 250, 250), Layer::Fg, &mut s);
        assert_eq!(s, "97", "near white is bright white");
    }

    /// The page color goes out as the terminal's own background; every other color as itself.
    #[test]
    fn the_page_color_can_be_the_terminals_background() {
        let page = Color::Rgb(8, 6, 6);
        let mut b = FrameBackend::new(Mem::default(), Depth::TrueColor, false);
        b.see_through = Some(page);
        let mut term = ratatui::Terminal::new(b).unwrap();
        term.draw(|f| {
            let buf = f.buffer_mut();
            buf[(0, 0)].set_char('a').set_bg(page);
            buf[(1, 0)].set_char('b').set_bg(Color::Rgb(40, 20, 20));
        })
        .unwrap();
        let out: String = term.backend().sink.written.iter().map(|w| String::from_utf8_lossy(w).into_owned()).collect();
        assert!(!out.contains("48;2;8;6;6"), "the page color is never sent: {out:?}");
        assert!(out.contains("48;2;40;20;20"), "{out:?}");
    }

    /// Sized text goes out once, after the cells that hold its place; again only when a clear or
    /// a cell written under it may have erased it; and not at all once no frame asks for it.
    #[test]
    fn sized_text_is_sent_when_it_may_have_been_erased() {
        let big = BigText { x: 2, y: 1, scale: 2, text: "$12".into(), fg: Color::Rgb(250, 250, 250), bg: Color::Reset, bold: true };
        let mut term = ratatui::Terminal::new(FrameBackend::new(Mem::default(), Depth::TrueColor, false)).unwrap();
        let frame = |term: &mut ratatui::Terminal<FrameBackend<Mem>>, wanted: &[BigText], under: char| -> String {
            term.backend_mut().begin();
            term.draw(|f| {
                for x in 2..8 {
                    for y in 1..3 {
                        f.buffer_mut()[(x, y)].set_symbol(BIG_TEXT_CELL);
                    }
                }
                f.buffer_mut()[(0, 3)].set_char(under);
            })
            .unwrap();
            term.backend_mut().big_text(wanted);
            term.backend_mut().end().unwrap();
            let out = term.backend().sink.written.last().map(|w| String::from_utf8_lossy(w).into_owned()).unwrap_or_default();
            term.backend_mut().sink.written.clear();
            out
        };
        let first = frame(&mut term, std::slice::from_ref(&big), 'a');
        assert!(first.contains("\x1b[2;3H\x1b[0;1;38;2;250;250;250;49m\x1b]66;s=2;$12\x07"), "placed where its cells are: {first:?}");
        let again = frame(&mut term, std::slice::from_ref(&big), 'b');
        assert!(!again.contains("]66;"), "untouched, not sent again: {again:?}");
        term.clear().unwrap();
        let cleared = frame(&mut term, std::slice::from_ref(&big), 'b');
        assert!(cleared.contains("]66;"), "a clear erases it, so it goes again");
        // A cell written inside its rows and columns erases it too.
        term.backend_mut().begin();
        term.draw(|f| {
            for x in 2..8 {
                for y in 1..3 {
                    f.buffer_mut()[(x, y)].set_symbol(BIG_TEXT_CELL);
                }
            }
            f.buffer_mut()[(5, 2)].set_char('x');
        })
        .unwrap();
        term.backend_mut().big_text(std::slice::from_ref(&big));
        term.backend_mut().end().unwrap();
        let touched = term.backend().sink.written.last().map(|w| String::from_utf8_lossy(w).into_owned()).unwrap_or_default();
        assert!(touched.contains("]66;"), "a cell under it was written: {touched:?}");
        term.backend_mut().sink.written.clear();
        let gone = frame(&mut term, &[], 'c');
        assert!(!gone.contains("]66;"), "nothing asked for, nothing sent");
    }

    /// Curly and double underlines go out as `4:3` and `4:2` where the terminal draws them, as a
    /// plain `4` where it doesn't, and never as blinking.
    #[test]
    fn underline_styles_ride_on_the_blink_bits() {
        for (styled, curly, double) in [(true, "4:3", "4:2"), (false, "4", "4")] {
            let mut b = FrameBackend::new(Mem::default(), Depth::TrueColor, false);
            b.styled_underline = styled;
            let mut term = ratatui::Terminal::new(b).unwrap();
            term.draw(|f| {
                let buf = f.buffer_mut();
                buf[(0, 0)].set_char('a').modifier = underline::CURLY;
                buf[(1, 0)].set_char('b').modifier = underline::CURLY;
                buf[(2, 0)].set_char('c').modifier = underline::DOUBLE;
                buf[(3, 0)].set_char('d').modifier = Modifier::UNDERLINED;
                buf[(4, 0)].set_char('e');
            })
            .unwrap();
            let out: String = term.backend().sink.written.iter().map(|w| String::from_utf8_lossy(w).into_owned()).collect();
            assert!(out.contains(&format!("\x1b[{curly}ma")), "{styled}: curly first: {out:?}");
            assert!(out.contains("ab"), "{styled}: the same style is not sent twice: {out:?}");
            if styled {
                assert!(out.contains(&format!("\x1b[{double}mc")), "double next: {out:?}");
                assert!(out.contains("\x1b[4md"), "then plain: {out:?}");
            }
            assert!(!out.contains("[5") && !out.contains(";5m") && !out.contains(";6m") && !out.contains("[6m"), "never blinks: {out:?}");
            assert!(out.contains("\x1b[24me"), "and off: {out:?}");
        }
    }

    /// A link wraps exactly its cells, and is closed before the next cell and at the end.
    #[test]
    fn a_link_wraps_exactly_its_cells() {
        let mut term = ratatui::Terminal::new(FrameBackend::new(Mem::default(), Depth::TrueColor, false)).unwrap();
        set_links(vec![super::super::super::links::Link { y: 0, x: 2, end: 4, url: "https://x.test/tx/0x1".into() }]);
        term.draw(|f| {
            let buf = f.buffer_mut();
            for (i, c) in "abcdef".chars().enumerate() {
                buf[(i as u16, 0)].set_char(c);
            }
        })
        .unwrap();
        set_links(Vec::new());
        let out: String = term.backend().sink.written.iter().map(|w| String::from_utf8_lossy(w).into_owned()).collect();
        assert!(out.contains("ab\x1b]8;;https://x.test/tx/0x1\x1b\\cd\x1b]8;;\x1b\\ef"), "{out:?}");
    }

    #[test]
    fn bold_off_keeps_dim_and_vice_versa() {
        let mut s = String::new();
        modifier_codes(Modifier::BOLD | Modifier::DIM, Modifier::DIM, &mut s);
        assert_eq!(s, "22;2");
    }
}
