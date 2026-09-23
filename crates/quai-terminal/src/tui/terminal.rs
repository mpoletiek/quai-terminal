//! Terminal capabilities, herdr-style kitty graphics, and diagnostics.

use crate::commands::Ctx;
use base64::Engine;
use std::io::Write;
use std::time::Duration;
#[cfg(test)]
use std::time::Instant;
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
    /// The palette's color 8 as the terminal reported it (for the terminal theme's lines).
    pub ansi8: Option<(u8, u8, u8)>,
    /// Text can be drawn larger than a cell (OSC 66), for the headline number.
    pub text_sizing: bool,
    /// The terminal draws block and legacy-computing glyphs itself (kitty, Ghostty), so octants
    /// show whatever the font; elsewhere they depend on a font that has them.
    pub drawn_blocks: bool,
    /// Pictures go as kitty Unicode placeholders (inside tmux with passthrough allowed), so
    /// tmux moves them with the pane (`placeholders`).
    pub placeholders: bool,
    /// The taskbar shows progress sent as OSC 9;4. Only where it is known to: elsewhere OSC 9 is
    /// a desktop notification (iTerm2, and others), and a progress update would pop one up.
    pub taskbar_progress: bool,
    /// Hyperlinks (OSC 8) are safe to send: everywhere but the Linux console, which prints
    /// sequences it doesn't know.
    pub hyperlinks: bool,
    pub cell_px: (u16, u16),
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

/// Whether tmux passes escape sequences through to the terminal (`allow-passthrough`).
fn tmux_passthrough() -> bool {
    std::process::Command::new("tmux")
        .args(["show-options", "-gqv", "allow-passthrough"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .is_some_and(|o| matches!(String::from_utf8_lossy(&o.stdout).trim(), "on" | "all"))
}

/// Detect capabilities without touching the terminal (safe before raw mode).
pub fn detect(mode: GraphicsMode) -> Caps {
    let term = env("TERM");
    let program = env("TERM_PROGRAM");
    let tmux = std::env::var_os("TMUX").is_some() || term.starts_with("tmux") || term.starts_with("screen");
    // A multiplexer that doesn't pass kitty graphics through: pictures sent into it land as
    // garbage in its own screen, so it gets cells.
    let other_mux = std::env::var_os("ZELLIJ").is_some() || std::env::var_os("STY").is_some();
    let ssh = std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some();
    let kitty_graphics = std::env::var_os("KITTY_WINDOW_ID").is_some()
        || term == "xterm-kitty"
        || term == "xterm-ghostty"
        || program.eq_ignore_ascii_case("ghostty");
    // WezTerm speaks the kitty protocol only in part (placements with z-index and unicode
    // placeholders among what it lacks), so it is a truecolor terminal drawing cells by default;
    // `graphics = "pixels"` still turns bitmaps on for it.
    let wezterm = program.eq_ignore_ascii_case("WezTerm") || term.contains("wezterm");
    let truecolor = matches!(env("COLORTERM").as_str(), "truecolor" | "24bit") || kitty_graphics || wezterm;
    // Inside tmux, kitty pictures work through placeholders when tmux lets them through.
    let passthrough = tmux && kitty_graphics && !other_mux && !ssh && truecolor && tmux_passthrough();
    let auto_tier = if term == "dumb" {
        Tier::Text
    } else if kitty_graphics && ((!tmux && !other_mux && !ssh) || passthrough) {
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
    let hyperlinks = term != "linux";
    let taskbar_progress = !tmux
        && (program.eq_ignore_ascii_case("ghostty") || std::env::var_os("WT_SESSION").is_some() || std::env::var_os("ConEmuPID").is_some());
    Caps {
        tier,
        truecolor,
        light_background: None,
        terminal: if program.is_empty() { term } else { program },
        tmux,
        ssh,
        // Known only once the terminal has answered (see `term::probe`).
        kitty_keyboard: false,
        text_sizing: false,
        ansi8: None,
        drawn_blocks: kitty_graphics,
        placeholders: tmux && tier == Tier::Pixels,
        taskbar_progress,
        hyperlinks,
        cell_px,
    }
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
    /// What to send: taken by the frame loop and written inside the frame's synchronized block,
    /// so pictures change in the same instant as the cells around them.
    pending: Vec<u8>,
    /// Where large pictures are handed over as files (the terminal is on this machine), rather
    /// than base64 through the pty: a big NFT is hundreds of kilobytes of escape codes otherwise.
    pub file_dir: Option<std::path::PathBuf>,
    /// Unicode placeholder mode (inside tmux): (image id, cols, rows) → its virtual placement.
    virtual_placed: std::collections::HashMap<(u32, u16, u16), u32>,
}

/// Pictures above this size go by file when they can; smaller ones are cheaper inline.
const FILE_OVER: usize = 16 * 1024;

/// The directory for handed-over pictures, when the terminal runs on this machine: tmpfs where
/// there is one. Kitty deletes each file after reading it (`t=t`), which it does only for files
/// in a temporary directory whose path says `tty-graphics-protocol`.
pub fn picture_dir(remote: bool) -> Option<std::path::PathBuf> {
    if remote {
        return None;
    }
    let shm = std::path::Path::new("/dev/shm");
    let base = if shm.is_dir() { shm.to_path_buf() } else { std::env::temp_dir() };
    Some(base)
}

/// Remove this process's handed-over pictures that the terminal did not take (it deletes the
/// ones it reads).
pub fn clean_picture_files(dir: &std::path::Path) {
    let prefix = format!("quai-terminal-tty-graphics-protocol-{}-", std::process::id());
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if e.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
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
            let id = self.transmit(&p.png, h, tmux, &mut out);
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
        self.pending.extend_from_slice(&out);
    }

    /// The image's id, sending it first if the terminal does not have it yet.
    fn transmit(&mut self, png: &[u8], h: u64, tmux: bool, out: &mut Vec<u8>) -> u32 {
        let frame = self.frame;
        if let Some(entry) = self.transmitted.get_mut(&h) {
            entry.1 = frame;
            return entry.0;
        }
        let id = IMAGE_ID + self.next_image;
        self.next_image = self.next_image.wrapping_add(1) % 1_000_000;
        // A large picture, with the terminal on this machine: a file it reads and deletes.
        // Otherwise, or if the file cannot be written, the bytes inline.
        let file = self.file_dir.as_ref().filter(|_| png.len() > FILE_OVER).and_then(|dir| {
            let path = dir.join(format!("quai-terminal-tty-graphics-protocol-{}-{id}.png", std::process::id()));
            std::fs::write(&path, png).ok().map(|_| path)
        });
        match file {
            Some(path) => {
                let name = base64::engine::general_purpose::STANDARD.encode(path.to_string_lossy().as_bytes());
                out.extend(wrap(format!("\x1b_Ga=t,f=100,t=t,i={id},q=2;{name}\x1b\\"), tmux).as_bytes());
            }
            None => {
                let encoded = base64::engine::general_purpose::STANDARD.encode(png);
                let chunks: Vec<&[u8]> = encoded.as_bytes().chunks(3072).collect();
                for (j, chunk) in chunks.iter().enumerate() {
                    let more = u8::from(j + 1 < chunks.len());
                    let header = if j == 0 { format!("a=t,f=100,t=d,i={id},q=2,m={more}") } else { format!("m={more},q=2") };
                    out.extend(wrap(format!("\x1b_G{header};{}\x1b\\", String::from_utf8_lossy(chunk)), tmux).as_bytes());
                }
            }
        }
        self.transmitted.insert(h, (id, frame, std::time::Instant::now()));
        let bytes = png.len();
        self.trace(|| format!("{frame} transmit id={id} bytes={bytes}"));
        id
    }

    /// Placeholder mode: a new frame of pictures begins (for eviction by last use).
    pub fn begin_frame(&mut self) {
        self.frame += 1;
    }

    /// Placeholder mode: the image's id and a virtual placement of it at `cols` × `rows`,
    /// sending either if the terminal does not have it yet. The frame's cells then name both.
    pub fn ensure_virtual(&mut self, png: &[u8], h: u64, cols: u16, rows: u16, tmux: bool) -> (u32, u32) {
        let mut out = Vec::new();
        let id = self.transmit(png, h, tmux, &mut out);
        let pid = match self.virtual_placed.get(&(id, cols, rows)) {
            Some(pid) => *pid,
            None => {
                self.next_placement = self.next_placement % 4_000_000 + 1;
                let pid = self.next_placement;
                out.extend(wrap(format!("\x1b_Ga=p,U=1,i={id},p={pid},c={cols},r={rows},q=2\x1b\\"), tmux).as_bytes());
                self.virtual_placed.insert((id, cols, rows), pid);
                let frame = self.frame;
                self.trace(|| format!("{frame} virtual id={id} p={pid} {cols}x{rows}"));
                pid
            }
        };
        self.pending.extend_from_slice(&out);
        (id, pid)
    }

    /// Placeholder mode: past the budget, delete the images unused longest (their virtual
    /// placements go with them).
    pub fn evict_idle(&mut self, tmux: bool) {
        if self.transmitted.len() <= MAX_TRANSMITTED {
            return;
        }
        let frame = self.frame;
        let mut idle: Vec<(u64, u32, u64)> =
            self.transmitted.iter().filter(|(_, (_, used, _))| *used < frame).map(|(h, (id, used, _))| (*used, *id, *h)).collect();
        idle.sort_unstable();
        let excess = self.transmitted.len() - MAX_TRANSMITTED * 3 / 4;
        let mut out = Vec::new();
        for (_, id, h) in idle.into_iter().take(excess) {
            out.extend(wrap(format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\"), tmux).as_bytes());
            self.transmitted.remove(&h);
            self.virtual_placed.retain(|k, _| k.0 != id);
        }
        self.pending.extend_from_slice(&out);
    }

    /// The bytes waiting to go to the terminal (see `pending`).
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
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
        self.pending.extend_from_slice(&out);
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
        self.virtual_placed.clear();
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
        self.virtual_placed.clear();
        self.pending.extend_from_slice(&out);
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
    // What the terminal answers to the same questions the TUI asks at startup, rather than what
    // the environment suggests: `kitty_keyboard_protocol` used to be a guess from TERM, and said
    // yes inside tmux. Raw mode is needed to read the answers and is left at once. A run whose
    // input or output isn't a terminal reports nothing asked — its query would land in whatever
    // is collecting the output, and no terminal would answer it.
    let answers = probe_now();
    let (theme, file) = super::theme::resolve(ctx.paths.root(), &ctx.config.theme, false, ctx.global.no_color);
    let hex = |c: Option<(u8, u8, u8)>| c.map(|(r, g, b)| format!("#{r:02x}{g:02x}{b:02x}"));
    let info = serde_json::json!({
        "terminal": caps.terminal,
        "answered": answers.as_ref().map(|a| a.answered),
        "probe_ms": answers.as_ref().map(|a| a.took.as_secs_f64() * 1000.0),
        "background": answers.as_ref().and_then(|a| hex(a.background)),
        "foreground": answers.as_ref().and_then(|a| hex(a.foreground)),
        "light_background": answers.as_ref().and_then(|a| a.light()),
        "graphics_tier": format!("{:?}", caps.tier),
        "truecolor": caps.truecolor || answers.as_ref().is_some_and(|a| a.truecolor),
        "underline_color": answers.as_ref().map(|a| a.underline_color),
        "synchronized_output": answers.as_ref().map(|a| a.sync_output),
        "kitty_keyboard_protocol": answers.as_ref().map(|a| a.kitty_keyboard),
        "tmux": caps.tmux,
        "ssh": caps.ssh,
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

/// Run the startup probe outside the TUI, when both ends are a terminal.
#[cfg(unix)]
fn probe_now() -> Option<super::term::probe::Answers> {
    use termina::Terminal as _;
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) || !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        return None;
    }
    let mut tty = termina::PlatformTerminal::new().ok()?;
    tty.enter_raw_mode().ok()?;
    let answers = super::term::probe::run(&mut tty, std::io::stdin());
    let _ = tty.enter_cooked_mode();
    Some(answers)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The parent test below launches this exact worker inside a real PTY. No wallet is opened.
    // It runs the startup probe and then reads keys the way the TUI does, and reports both.
    #[test]
    #[cfg(unix)]
    fn input_pty_worker() {
        use termina::Terminal as _;
        let Ok(output) = std::env::var("QUAI_INPUT_PTY_RESULT") else {
            return;
        };
        let mut tty = termina::PlatformTerminal::new().unwrap();
        tty.enter_raw_mode().unwrap();
        let reader = tty.event_reader();
        let mut answers = super::super::term::probe::run(&mut tty, std::io::stdin());
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut keys: Vec<String> = std::mem::take(&mut answers.after)
            .into_iter()
            .filter_map(super::super::term::input::convert)
            .filter(|e| matches!(e, crossterm::event::Event::Key(_)))
            .map(|e| format!("{e:?}"))
            .collect();
        while Instant::now() < deadline && keys.len() < 2 {
            if reader.poll(Some(Duration::from_millis(50)), |_| true).unwrap() {
                let event = reader.read(|_| true).unwrap();
                if event.is_escape() {
                    super::super::term::probe::absorb(&mut answers, &event);
                }
                if let Some(e @ crossterm::event::Event::Key(_)) = super::super::term::input::convert(event) {
                    keys.push(format!("{e:?}"));
                }
            }
        }
        tty.enter_cooked_mode().unwrap();
        std::fs::write(output, serde_json::to_vec(&serde_json::json!({"light": answers.light(), "keys": keys})).unwrap()).unwrap();
    }

    /// The startup probe against a real PTY: replies in either order, split into single bytes,
    /// late, or missing never reach the app as keys, and keys typed after it arrive intact.
    #[test]
    #[cfg(unix)]
    fn input_pty_negotiation_keeps_replies_out_of_the_keys() {
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
