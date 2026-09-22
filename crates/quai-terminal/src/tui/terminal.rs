//! Terminal capabilities, herdr-style kitty graphics, and diagnostics.

use crate::commands::Ctx;
use base64::Engine;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use std::collections::VecDeque;
use std::io::Write;
use std::time::{Duration, Instant};
use wallet_core::Result;
use wallet_core::config::GraphicsMode;

/// Graphics tier used for QR codes and images.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// Kitty graphics protocol bitmaps.
    Pixels,
    /// Unicode half blocks.
    Cells,
    /// Text only.
    Text,
}

/// Probed capabilities.
#[derive(Clone, Debug)]
pub struct Caps {
    pub tier: Tier,
    pub truecolor: bool,
    pub light_background: Option<bool>,
    pub terminal: String,
    pub tmux: bool,
    pub ssh: bool,
    pub kitty_keyboard: bool,
    pub cell_px: (u16, u16),
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

/// Detect capabilities without touching the terminal (safe before raw mode).
pub fn detect(mode: GraphicsMode) -> Caps {
    let term = env("TERM");
    let program = env("TERM_PROGRAM");
    let tmux = std::env::var_os("TMUX").is_some() || term.starts_with("tmux") || term.starts_with("screen");
    let ssh = std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some();
    let kitty_like = std::env::var_os("KITTY_WINDOW_ID").is_some()
        || term == "xterm-kitty"
        || term == "xterm-ghostty"
        || program.eq_ignore_ascii_case("ghostty")
        || program.eq_ignore_ascii_case("WezTerm");
    let truecolor = matches!(env("COLORTERM").as_str(), "truecolor" | "24bit") || kitty_like;
    let auto_tier = if term == "dumb" {
        Tier::Text
    } else if kitty_like && !tmux && !ssh {
        Tier::Pixels
    } else {
        Tier::Cells
    };
    let tier = match mode {
        GraphicsMode::Auto => auto_tier,
        GraphicsMode::Pixels => Tier::Pixels,
        GraphicsMode::Cells => Tier::Cells,
        GraphicsMode::Text => Tier::Text,
    };
    let cell_px = crossterm::terminal::window_size()
        .ok()
        .filter(|s| s.columns > 0 && s.rows > 0 && s.width > 0 && s.height > 0)
        .map(|s| ((s.width / s.columns).max(1), (s.height / s.rows).max(1)))
        .unwrap_or((8, 16));
    Caps {
        tier,
        truecolor,
        light_background: None,
        terminal: if program.is_empty() { term } else { program },
        tmux,
        ssh,
        kitty_keyboard: kitty_like,
        cell_px,
    }
}

/// The sole terminal input owner, including capability negotiation. Crossterm consumes DA1
/// internally; OSC 11 is filtered from its decoded event stream. No raw-stdin reader can survive
/// a timeout or steal later keys. Unrelated events retain their exact codes, modifiers and order.
#[derive(Default)]
pub struct Input {
    ready: VecDeque<Event>,
    reply_events: Vec<Event>,
    reply: Vec<u8>,
    /// When the current partial reply started, to tell a terminal's answer from a person's Esc.
    reply_at: Option<Instant>,
    light: Option<bool>,
}

/// How long a buffer that could still become an OSC 11 or DA1 reply is held before it is treated
/// as ordinary keys. A terminal emits the rest of its answer in microseconds; a person reaching
/// for the next key takes far longer, so this separates them without swallowing either.
const AMBIGUOUS_REPLY_GRACE: Duration = Duration::from_millis(50);

impl Input {
    /// Run after raw mode starts. The same owner must service the subsequent UI event loop, since
    /// a terminal may finish a fragmented OSC response after this bounded startup window.
    pub fn probe_background(timeout: Duration) -> Self {
        let mut input = Self::default();
        // Both ends, not just stdin: the query goes out on stdout, so a redirected stdout would
        // write escape bytes into whatever is collecting it — a file, a pipe, the JSON a caller is
        // about to parse — and still never reach a terminal that could answer. The TUI has both on
        // the tty and is unaffected; a command whose output is piped is not.
        if !std::io::IsTerminal::is_terminal(&std::io::stdin()) || !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
            return input;
        }
        let mut out = std::io::stdout();
        if out.write_all(b"\x1b]11;?\x1b\\\x1b[c").and_then(|_| out.flush()).is_err() {
            return input;
        }
        let deadline = Instant::now() + timeout;
        // A noisy input source cannot allocate unbounded memory during negotiation.
        while input.ready.len() < 256 {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match event::poll(remaining) {
                Ok(true) => match event::read() {
                    Ok(event) => input.feed(event),
                    Err(_) => break,
                },
                _ => break,
            }
        }
        input.flush_idle_prefix();
        input
    }

    pub fn light_background(&self) -> Option<bool> {
        self.light
    }

    pub fn poll(&mut self, timeout: Duration) -> std::io::Result<bool> {
        let deadline = Instant::now() + timeout;
        loop {
            if !self.ready.is_empty() {
                return Ok(true);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !event::poll(remaining)? {
                self.flush_idle_prefix();
                return Ok(!self.ready.is_empty());
            }
            self.feed(event::read()?);
            if Instant::now() >= deadline {
                self.flush_idle_prefix();
                return Ok(!self.ready.is_empty());
            }
        }
    }

    pub fn read(&mut self) -> std::io::Result<Event> {
        loop {
            if let Some(event) = self.ready.pop_front() {
                return Ok(event);
            }
            self.poll(Duration::from_secs(60))?;
        }
    }

    /// Release a buffer that is not a recognized reply. Called when something has already proved
    /// the buffered bytes were ordinary keys — an event that cannot continue either reply — so the
    /// keys are replayed at once, in order, with no delay.
    fn flush_ambiguous_prefix(&mut self) {
        if self.reply.starts_with(b"\x1b]11;") || self.reply.starts_with(b"\x1b[?") {
            return;
        }
        self.ready.extend(self.reply_events.drain(..));
        self.reply.clear();
        self.reply_at = None;
    }

    /// The same release, but on an idle timeout, where nothing has proved anything yet.
    ///
    /// A reply can be split across poll windows, leaving only part of its introducer buffered.
    /// Releasing that at the first timeout replayed the fragment as keys — `Alt+]`, `1`, `1`, `;`
    /// — and left the report that followed to arrive as text, so a terminal answering slowly typed
    /// its own answer into the UI. A buffer that could still become a reply is therefore held for
    /// [`AMBIGUOUS_REPLY_GRACE`] first: the rest of a terminal's answer beats that comfortably,
    /// and a person reaching for the next key does not.
    fn flush_idle_prefix(&mut self) {
        if (b"\x1b]11;".starts_with(&self.reply) || b"\x1b[?".starts_with(&self.reply))
            && self.reply_at.is_some_and(|at| at.elapsed() < AMBIGUOUS_REPLY_GRACE)
        {
            return;
        }
        self.flush_ambiguous_prefix();
    }

    fn feed(&mut self, event: Event) {
        // Non-text keys, paste, mouse, focus and resize are never part of an OSC reply.
        let bytes = match &event {
            Event::Key(key) if key.kind == KeyEventKind::Press => match (key.code, key.modifiers) {
                (KeyCode::Esc, KeyModifiers::NONE) => vec![0x1b],
                (KeyCode::Char('g'), KeyModifiers::CONTROL) => vec![0x07],
                (KeyCode::Char(c), KeyModifiers::ALT) if c.is_ascii() => vec![0x1b, c as u8],
                (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) if c.is_ascii() => vec![c as u8],
                _ => {
                    self.flush_ambiguous_prefix();
                    self.ready.push_back(event);
                    return;
                }
            },
            _ => {
                self.flush_ambiguous_prefix();
                self.ready.push_back(event);
                return;
            }
        };
        if self.reply.is_empty() && bytes[0] != 0x1b {
            self.ready.push_back(event);
            return;
        }
        if self.reply.is_empty() {
            self.reply_at = Some(Instant::now());
        }
        self.reply.extend(bytes);
        self.reply_events.push(event);
        let prefix = b"\x1b]11;";
        if prefix.starts_with(&self.reply) || b"\x1b[?".starts_with(&self.reply) {
            return;
        }
        if self.reply.starts_with(b"\x1b[?") {
            let body = &self.reply[3..];
            if body.ends_with(b"c") && body[..body.len() - 1].iter().all(|b| b.is_ascii_digit() || *b == b';') {
                self.reply.clear();
                self.reply_at = None;
                self.reply_events.clear();
                return;
            }
            if body.len() < 128 && body.iter().all(|b| b.is_ascii_digit() || *b == b';') {
                return;
            }
        }
        if !self.reply.starts_with(prefix) {
            // A genuine escape/Alt sequence merely shared a prefix with OSC. Replay exactly.
            self.ready.extend(self.reply_events.drain(..));
            self.reply.clear();
            self.reply_at = None;
            return;
        }
        if self.reply.ends_with(b"\x07") || self.reply.ends_with(b"\x1b\\") {
            if let Some(light) = parse_osc11(&self.reply) {
                self.light = Some(light);
            }
            self.reply.clear();
            self.reply_at = None;
            self.reply_events.clear();
        } else if self.reply.len() > 256 {
            // Recognized but malformed report: bounded storage, with no report text as commands.
            self.reply.truncate(prefix.len());
            self.reply_events.clear();
        }
    }
}

/// Parse a complete, framed OSC 11 RGB report (BEL or ST terminated). Components are one to
/// four hexadecimal digits, as specified by XParseColor; malformed values never shift/overflow.
pub fn parse_osc11(reply: &[u8]) -> Option<bool> {
    let start = reply.windows(9).position(|w| w == b"\x1b]11;rgb:")? + 9;
    let rest = &reply[start..];
    let end = rest.iter().position(|b| *b == 0x07 || *b == 0x1b)?;
    if rest[end] == 0x1b && rest.get(end + 1) != Some(&b'\\') {
        return None;
    }
    let color = std::str::from_utf8(&rest[..end]).ok()?;
    let values: Option<Vec<u32>> = color
        .split('/')
        .map(|part| {
            if part.is_empty() || part.len() > 4 || !part.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            let value = u32::from_str_radix(part, 16).ok()?;
            Some(value * 255 / ((1u32 << (4 * part.len())) - 1))
        })
        .collect();
    let values = values?;
    if values.len() != 3 {
        return None;
    }
    Some(values[0] * 299 + values[1] * 587 + values[2] * 114 >= 128_000)
}

/// One bitmap on screen for a frame: PNG, cell area and stacking order. `z < 0` draws under
/// the text but over cell backgrounds (backlight); `z = 0` covers the cells (icons, pictures).
#[derive(Clone, Debug)]
pub struct Placement {
    pub png: std::sync::Arc<Vec<u8>>,
    /// Content key of `png` (see `images::png_key`).
    pub key: u64,
    pub x: u16,
    pub y: u16,
    pub cols: u16,
    pub rows: u16,
    pub z: i32,
}

/// Placement identity on screen: image id, position, size and z.
type PlacementKey = (u32, u16, u16, u16, u16, i32);

/// Kitty graphics. Each image is transmitted once (by content hash) and kept in the terminal;
/// each frame's placements are diffed against the previous frame, so steady placements send
/// nothing and a changing light only swaps its own placement. New placements are written before
/// old ones are deleted, in one write, so nothing blinks. Images not on screen are evicted
/// oldest-first beyond a budget. `QW_KITTY_LOG=<file>` appends a trace of what is sent.
#[derive(Default)]
pub struct KittyGraphics {
    placed: std::collections::HashMap<PlacementKey, u32>,
    /// Content hash → (image id, frame last used, when it was sent).
    transmitted: std::collections::HashMap<u64, (u32, u64, std::time::Instant)>,
    next_image: u32,
    next_placement: u32,
    frame: u64,
    log: Option<std::fs::File>,
    log_checked: bool,
}

const IMAGE_ID: u32 = 0x5157; // private id base for this app
/// A picture returning to the screen after this long is transmitted again, so a terminal that
/// dropped it (a reload, a re-theme) shows it instead of an empty space. Pictures that stay on
/// screen are never re-sent, and a theme switch forgets them all at once.
const RESEND_AFTER: Duration = Duration::from_secs(600);
/// Transmitted images kept in the terminal (icons, thumbnails, light frames).
const MAX_TRANSMITTED: usize = 384;

impl KittyGraphics {
    fn trace(&mut self, line: impl FnOnce() -> String) {
        if !self.log_checked {
            self.log_checked = true;
            self.log = std::env::var_os("QW_KITTY_LOG").and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok());
        }
        if let Some(f) = self.log.as_mut() {
            let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
            let _ = writeln!(f, "{ms} {}", line());
        }
    }

    /// Show exactly `items` this frame. Unchanged placements send nothing.
    pub fn place_all(&mut self, items: &[Placement], tmux: bool) {
        self.frame += 1;
        let frame = self.frame;
        let mut out = Vec::new();
        let mut wanted: Vec<PlacementKey> = Vec::with_capacity(items.len());
        for p in items {
            let h = p.key;
            let key = (0, p.x, p.y, p.cols, p.rows, p.z);
            // A picture coming back on screen is sent again when the terminal has had long
            // enough to drop it (a config reload frees what it was keeping); one that stayed put
            // is not touched.
            let returning = !self.placed.keys().any(|k| (0, k.1, k.2, k.3, k.4, k.5) == key);
            let stale = self.transmitted.get(&h).is_some_and(|(_, _, sent)| returning && sent.elapsed() > RESEND_AFTER);
            if stale {
                self.transmitted.remove(&h);
            }
            let id = match self.transmitted.get_mut(&h) {
                Some(entry) => {
                    entry.1 = frame;
                    entry.0
                }
                None => {
                    let id = IMAGE_ID + self.next_image;
                    self.next_image = self.next_image.wrapping_add(1) % 1_000_000;
                    let encoded = base64::engine::general_purpose::STANDARD.encode(p.png.as_slice());
                    let chunks: Vec<&[u8]> = encoded.as_bytes().chunks(3072).collect();
                    for (j, chunk) in chunks.iter().enumerate() {
                        let more = u8::from(j + 1 < chunks.len());
                        let header = if j == 0 { format!("a=t,f=100,t=d,i={id},q=2,m={more}") } else { format!("m={more},q=2") };
                        out.extend(wrap(format!("\x1b_G{header};{}\x1b\\", String::from_utf8_lossy(chunk)), tmux).as_bytes());
                    }
                    self.transmitted.insert(h, (id, frame, std::time::Instant::now()));
                    let bytes = p.png.len();
                    self.trace(|| format!("{frame} transmit id={id} bytes={bytes}"));
                    id
                }
            };
            let key = (id, p.x, p.y, p.cols, p.rows, p.z);
            if !wanted.contains(&key) {
                wanted.push(key);
            }
        }
        let mut next = std::collections::HashMap::with_capacity(wanted.len());
        for key in wanted {
            let pid = match self.placed.remove(&key) {
                Some(pid) => pid,
                None => {
                    self.next_placement = self.next_placement % 4_000_000 + 1;
                    let pid = self.next_placement;
                    let (id, x, y, cols, rows, z) = key;
                    out.extend(format!("\x1b7\x1b[{};{}H", y + 1, x + 1).as_bytes());
                    out.extend(wrap(format!("\x1b_Ga=p,i={id},p={pid},c={cols},r={rows},z={z},C=1,q=2\x1b\\"), tmux).as_bytes());
                    out.extend(b"\x1b8");
                    self.trace(|| format!("{frame} place id={id} p={pid} at {x},{y} {cols}x{rows} z={z}"));
                    pid
                }
            };
            next.insert(key, pid);
        }
        for ((id, x, y, ..), pid) in std::mem::replace(&mut self.placed, next) {
            out.extend(wrap(format!("\x1b_Ga=d,d=i,i={id},p={pid},q=2\x1b\\"), tmux).as_bytes());
            self.trace(|| format!("{frame} remove id={id} p={pid} at {x},{y}"));
        }
        if self.transmitted.len() > MAX_TRANSMITTED {
            let on_screen: std::collections::HashSet<u32> = self.placed.keys().map(|k| k.0).collect();
            let mut idle: Vec<(u64, u32, u64)> = self
                .transmitted
                .iter()
                .filter(|(_, (id, ..))| !on_screen.contains(id))
                .map(|(h, (id, used, _))| (*used, *id, *h))
                .collect();
            idle.sort_unstable();
            let excess = self.transmitted.len() - MAX_TRANSMITTED * 3 / 4;
            for (_, id, h) in idle.into_iter().take(excess) {
                out.extend(wrap(format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\"), tmux).as_bytes());
                self.transmitted.remove(&h);
            }
            let left = self.transmitted.len();
            self.trace(|| format!("{frame} evicted down to {left}"));
        }
        if !out.is_empty() {
            let mut stdout = std::io::stdout();
            let _ = stdout.write_all(&out);
            let _ = stdout.flush();
        }
    }

    /// Remove every placement (image data stays cached in the terminal).
    pub fn clear(&mut self, tmux: bool) {
        if self.placed.is_empty() {
            return;
        }
        let mut out = Vec::new();
        for ((id, ..), pid) in self.placed.drain() {
            out.extend(wrap(format!("\x1b_Ga=d,d=i,i={id},p={pid},q=2\x1b\\"), tmux).as_bytes());
        }
        self.trace(|| "clear".into());
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(&out);
        let _ = stdout.flush();
    }

    /// Forget what the terminal holds, so the next frame sends the pictures again. A theme
    /// switch reloads the terminal too (Omarchy re-themes kitty), and a kitty that reloaded has
    /// dropped the images it was keeping: placing them again would show nothing.
    ///
    /// A resize is the same situation. Clearing the screen destroys placements, and a terminal
    /// is free to drop the image data behind them once nothing refers to it — which is what a
    /// layout reorganizing does to every picture it no longer has room for. Placements carry
    /// `q=2`, so a placement naming data the terminal no longer has fails silently and the
    /// pictures never come back. Re-sending a screenful costs about 33 KB, so the wallet pays
    /// that rather than guess what survived.
    pub fn forget_images(&mut self, tmux: bool) {
        self.clear(tmux);
        let transmitted = self.transmitted.len();
        self.transmitted.clear();
        self.trace(|| format!("forget {transmitted} image(s)"));
    }

    /// Free all image data on exit.
    pub fn free_all(&mut self, tmux: bool) {
        self.clear(tmux);
        let mut out = Vec::new();
        for (id, ..) in self.transmitted.values() {
            out.extend(wrap(format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\"), tmux).as_bytes());
        }
        self.transmitted.clear();
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(&out);
        let _ = stdout.flush();
    }
}

fn wrap(seq: String, tmux: bool) -> String {
    if tmux { format!("\x1bPtmux;{}\x1b\\", seq.replace('\x1b', "\x1b\x1b")) } else { seq }
}

/// Encode a QR code as a black-on-white PNG with a quiet zone.
pub fn qr_png(data: &str, scale: usize) -> Option<Vec<u8>> {
    use qrcode::{Color, EcLevel, QrCode};
    let code = QrCode::with_error_correction_level(data.as_bytes(), EcLevel::M).ok()?;
    let width = code.width();
    let colors = code.to_colors();
    let quiet = 4;
    let size = (width + quiet * 2) * scale;
    let mut pixels = vec![255u8; size * size];
    for y in 0..width {
        for x in 0..width {
            if colors[y * width + x] == Color::Dark {
                for dy in 0..scale {
                    for dx in 0..scale {
                        pixels[((y + quiet) * scale + dy) * size + (x + quiet) * scale + dx] = 0;
                    }
                }
            }
        }
    }
    let mut buf = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut buf, size as u32, size as u32);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().ok()?;
        w.write_image_data(&pixels).ok()?;
    }
    Some(buf)
}

/// QR module grid for cell rendering (true = dark), with a quiet zone of `quiet` modules (the standard asks for 4).
pub fn qr_modules(data: &str, quiet: usize) -> Option<(usize, Vec<bool>)> {
    use qrcode::{Color, EcLevel, QrCode};
    let code = QrCode::with_error_correction_level(data.as_bytes(), EcLevel::M).ok()?;
    let width = code.width();
    let colors = code.to_colors();
    let size = width + quiet * 2;
    let mut grid = vec![false; size * size];
    for y in 0..width {
        for x in 0..width {
            grid[(y + quiet) * size + x + quiet] = colors[y * width + x] == Color::Dark;
        }
    }
    Some((size, grid))
}

pub fn diagnostics_cmd(ctx: &Ctx) -> Result<()> {
    let caps = detect(ctx.config.graphics);
    // The background is the one capability `detect` cannot infer: it has to be asked for, and the
    // terminal answers as input. That negotiation is where a slow terminal once had its reply
    // decoded as keystrokes, so a command called "probe terminal capabilities" should report what
    // the probe actually got rather than leave this the only thing it does not cover. Raw mode is
    // required to read the reply and is restored immediately; a non-terminal stdin reports unknown.
    // `probe_background` refuses unless both ends are a terminal, so a piped run reports unknown
    // rather than writing its query into the caller's output. Raw mode is needed to read the
    // reply and is restored immediately.
    let raw = crossterm::terminal::enable_raw_mode().is_ok();
    let light_background = Input::probe_background(Duration::from_millis(150)).light_background();
    if raw {
        let _ = crossterm::terminal::disable_raw_mode();
    }
    let (theme, file) = super::theme::resolve(ctx.paths.root(), &ctx.config.theme, false, ctx.global.no_color);
    let info = serde_json::json!({
        "terminal": caps.terminal,
        "light_background": light_background,
        "graphics_tier": format!("{:?}", caps.tier),
        "truecolor": caps.truecolor,
        "tmux": caps.tmux,
        "ssh": caps.ssh,
        "kitty_keyboard_protocol": caps.kitty_keyboard,
        "cell_pixels": [caps.cell_px.0, caps.cell_px.1],
        "size": crossterm::terminal::size().ok(),
        "theme": theme.name,
        "theme_file": file,
        "omarchy_theme": super::theme::omarchy_colors(),
        "motion": format!("{:?}", ctx.config.motion),
        "notifications_backend": format!("{:?}", crate::notify::detect_terminal()),
    });
    if ctx.out.json() {
        ctx.out.emit("diagnostics terminal", &info);
    } else {
        println!("{}", serde_json::to_string_pretty(&info).unwrap_or_default());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc11_parsing() {
        assert_eq!(parse_osc11(b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\\x1b[?62;22c"), Some(true));
        assert_eq!(parse_osc11(b"\x1b]11;rgb:0000/0000/0000\x07"), Some(false));
        assert_eq!(parse_osc11(b"\x1b[?62c"), None);
        assert_eq!(parse_osc11(b"\x1b]11;rgb:1e/1e/2e\x1b\\"), Some(false));
    }

    fn key(c: char) -> Event {
        Event::Key(crossterm::event::KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
    }

    fn osc_events() -> Vec<Event> {
        let mut events = vec![Event::Key(crossterm::event::KeyEvent::new(KeyCode::Char(']'), KeyModifiers::ALT))];
        events.extend("11;rgb:ffff/ffff/ffff".chars().map(key));
        events.push(Event::Key(crossterm::event::KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::ALT)));
        events
    }

    #[test]
    fn negotiated_reply_keeps_real_events_and_late_fragments() {
        let mut input = Input::default();
        let before = Event::Paste("user pasted OSC-like rgb:1/2/3".into());
        let after = Event::Key(crossterm::event::KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL));
        input.feed(before.clone());
        let reply = osc_events();
        for event in &reply[..8] {
            input.feed(event.clone());
        }
        // A timeout hands the same parser to the main loop; partial report state survives.
        input.flush_idle_prefix();
        for event in &reply[8..] {
            input.feed(event.clone());
        }
        input.feed(after.clone());
        assert_eq!(input.light_background(), Some(true));
        assert_eq!(input.ready.into_iter().collect::<Vec<_>>(), vec![before, after]);
    }

    /// A reply split inside its own introducer — `\x1b]`, `\x1b]1`, `\x1b]11` — used to be
    /// replayed as keys at the first idle timeout, so the terminal's answer arrived as `Alt+]`,
    /// `1`, `1`, `;` and the background was never learned. Every cut point must survive instead.
    #[test]
    fn an_idle_timeout_inside_the_introducer_does_not_type_the_reply_as_keys() {
        let reply = osc_events();
        for cut in 1..=5 {
            let mut input = Input::default();
            for event in &reply[..cut] {
                input.feed(event.clone());
            }
            input.flush_idle_prefix();
            assert!(input.ready.is_empty(), "cut {cut} released a partial reply as keys: {:?}", input.ready);
            for event in &reply[cut..] {
                input.feed(event.clone());
            }
            assert_eq!(input.light_background(), Some(true), "cut {cut} lost the report");
            assert!(input.ready.is_empty(), "cut {cut} leaked reply text: {:?}", input.ready);
        }
    }

    /// The grace only covers an idle timeout. An event that cannot continue either reply proves
    /// the buffer was ordinary typing, so those keys are released at once and in order.
    #[test]
    fn a_key_that_cannot_continue_a_reply_releases_the_buffer_immediately() {
        let mut input = Input::default();
        let esc = Event::Key(crossterm::event::KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let lambda = key('\u{03bb}');
        input.feed(esc.clone());
        assert!(input.ready.is_empty(), "a lone Esc is still ambiguous");
        input.feed(lambda.clone());
        assert_eq!(input.ready.iter().cloned().collect::<Vec<_>>(), vec![esc, lambda], "no delay, no reordering");
    }

    /// The query goes out on stdout, so a redirected stdout must not be written to: the bytes would
    /// land in whatever is collecting it — `diagnostics terminal -o json` is piped routinely — and
    /// no terminal would answer anyway. Under `cargo test` stdout is not a tty, which is exactly
    /// the case being guarded.
    #[test]
    fn a_redirected_stdout_is_never_written_a_query_it_cannot_answer() {
        let input = Input::probe_background(Duration::from_millis(5));
        assert_eq!(input.light_background(), None, "nothing can be learned without a terminal on both ends");
        assert!(input.ready.is_empty(), "and nothing is invented to deliver");
    }

    #[test]
    fn genuine_escape_alt_and_unicode_keys_are_preserved() {
        let keys = vec![
            Event::Key(crossterm::event::KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            key('\u{03bb}'),
            key('x'),
            Event::Key(crossterm::event::KeyEvent::new(KeyCode::Char(']'), KeyModifiers::ALT)),
            key('z'),
        ];
        let mut input = Input::default();
        for event in &keys {
            input.feed(event.clone());
        }
        input.flush_ambiguous_prefix();
        assert_eq!(input.ready.into_iter().collect::<Vec<_>>(), keys);
    }

    #[test]
    fn osc11_requires_complete_bounded_components_and_frame() {
        for bytes in [
            b"rgb:ffff/ffff/ffff".as_slice(),
            b"\x1b]11;rgb:ffff/ffff/ffff",
            b"\x1b]11;rgb:100000000/0/0\x07",
            b"\x1b]11;rgb:/0/0\x07",
            b"\x1b]11;rgb:ffff/ffff/ffff/0\x07",
        ] {
            assert_eq!(parse_osc11(bytes), None);
        }
    }

    // The parent test below launches this exact worker inside a real PTY. No wallet is opened.
    #[test]
    fn input_pty_worker() {
        let Ok(output) = std::env::var("QUAI_INPUT_PTY_RESULT") else {
            return;
        };
        crossterm::terminal::enable_raw_mode().unwrap();
        // Wide enough that a loaded machine cannot starve the reply out of the window and make the
        // parent's `late` case arrive early: the parent waits four times this before its late OSC.
        let mut input = Input::probe_background(Duration::from_millis(200));
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut keys = Vec::new();
        while Instant::now() < deadline && keys.len() < 3 {
            if input.poll(Duration::from_millis(50)).unwrap() {
                keys.push(format!("{:?}", input.read().unwrap()));
            }
        }
        crossterm::terminal::disable_raw_mode().unwrap();
        std::fs::write(output, serde_json::to_vec(&serde_json::json!({"light": input.light_background(), "keys":keys})).unwrap()).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn input_pty_negotiation_preserves_keys_in_both_orders_and_after_timeout() {
        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/test-terminal-input.py");
        // The harness is a development script and is not shipped everywhere this crate is. Say so
        // rather than failing: a missing harness is not a negotiation bug, and pretending it is
        // would train whoever sees it to ignore this test.
        if !script.exists() {
            eprintln!("skipped: {} is not present, so the PTY negotiation is unverified here", script.display());
            return;
        }
        let result = std::process::Command::new("python3").arg(script).arg(std::env::current_exe().unwrap()).output().unwrap();
        assert!(result.status.success(), "{}\n{}", String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));
    }

    /// A theme switch re-themes the terminal, which drops the images it was keeping: the next
    /// frame must send them again, not just place them.
    #[test]
    fn forgetting_images_makes_the_next_frame_send_them_again() {
        let png = std::sync::Arc::new(wallet_core::media::fixture_png(4, 4, (10, 20, 30)));
        let item = Placement { png, key: 42, x: 0, y: 0, cols: 2, rows: 1, z: 0 };
        let mut kitty = KittyGraphics::default();
        kitty.place_all(std::slice::from_ref(&item), false);
        assert_eq!((kitty.transmitted.len(), kitty.placed.len()), (1, 1));
        let first = kitty.transmitted[&42].0;
        // Placing the same picture again sends nothing.
        kitty.place_all(std::slice::from_ref(&item), false);
        assert_eq!(kitty.transmitted[&42].0, first, "the terminal still holds it");
        kitty.forget_images(false);
        assert_eq!((kitty.transmitted.len(), kitty.placed.len()), (0, 0));
        kitty.place_all(std::slice::from_ref(&item), false);
        assert_eq!(kitty.transmitted.len(), 1);
        assert_ne!(kitty.transmitted[&42].0, first, "sent again under a new id");
    }

    #[test]
    fn qr_png_roundtrip_header() {
        let png = qr_png("PM8TJTest", 4).unwrap();
        assert_eq!(&png[1..4], b"PNG");
        let (size, grid) = qr_modules("hello", 4).unwrap();
        assert_eq!(grid.len(), size * size);
    }
}
