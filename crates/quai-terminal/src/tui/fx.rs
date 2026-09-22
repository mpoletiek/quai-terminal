//! Ceremony effects via Omarchy's ttfx, embedded without its run loop, stdout writers
//! or process signal handlers. Frames are parsed from SGR text into ratatui cells.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ttfx::engine::ctx::{Clock, EngineCtx};
use ttfx::engine::effect::Effect;
use ttfx::engine::terminal::TerminalConfig;

/// Wordmark rendered by ceremonies: filled block glyphs, the way Omarchy's logo animates.
///
/// Line art (`/ _ \ | |`) gives an effect almost nothing to paint — a gradient across two or three
/// lit cells per letter reads as scattered sparks. Solid strokes give every letter a body, so a
/// sweep, a burn or a final gradient fills the word itself. One codepoint per cell, as ttfx wants.
pub const WORDMARK: &str = r" ███  ██ ██  ███  ████   ██████ █████ ████  ██   ██ ████ ██   ██  ███  ██   
██ ██ ██ ██ ██ ██  ██      ██   ██    ██ ██ ███ ███  ██  ███  ██ ██ ██ ██   
██ ██ ██ ██ █████  ██      ██   ████  ████  ██ █ ██  ██  ██ █ ██ █████ ██   
██ ██ ██ ██ ██ ██  ██      ██   ██    ██ ██ ██   ██  ██  ██  ███ ██ ██ ██   
 ██▄█  ███  ██ ██ ████     ██   █████ ██ ██ ██   ██ ████ ██   ██ ██ ██ █████";

/// The wordmark as a rectangle: every line padded to the widest.
///
/// Each line is centred on its own, so ragged line lengths shift rows against each other — every
/// row but the last ends inside the `L`, three columns short, and would sit right of the rest.
/// Padding here rather than in the literal keeps the art correct even if the trailing spaces are
/// ever trimmed from the source.
pub fn wordmark_block() -> String {
    let width = WORDMARK.lines().map(|l| l.chars().count()).max().unwrap_or(0);
    WORDMARK.lines().map(|l| format!("{l:<width$}")).collect::<Vec<_>>().join("\n")
}

/// A running effect.
pub struct Ceremony {
    width: u16,
    height: u16,
    effect: Box<dyn Effect>,
    ctx: EngineCtx,
    frame: Option<String>,
    done: bool,
    frames: u32,
    max_frames: u32,
    last_step: Option<std::time::Instant>,
    speed: u32,
}

/// Wall-clock interval between ttfx frames (the engine's virtual clock runs at 30 fps).
const FRAME_INTERVAL: std::time::Duration = std::time::Duration::from_millis(33);

/// Effect frames advanced per redraw on the lock screen.
///
/// ttfx effects are authored for 30 fps, which reads as a slow dissolve on a screen you are waiting
/// on. Advancing several frames per redraw plays them faster *without* redrawing faster, so the
/// terminal does the same work and only the animation speeds up.
pub const LOCK_SPEED: u32 = 3;

impl Ceremony {
    /// Build a named effect over `text` for a canvas of `width`×`height`.
    #[cfg(test)]
    pub fn new(name: &str, text: &str, width: u16, height: u16, max_frames: u32) -> Option<Self> {
        Self::with_args(name, &[], text, width, height, max_frames)
    }

    /// Build a named effect with extra ttfx effect arguments (e.g. theme gradient stops).
    /// Falls back to the effect's defaults when the arguments are rejected.
    pub fn with_args(name: &str, args: &[String], text: &str, width: u16, height: u16, max_frames: u32) -> Option<Self> {
        let mut argv = vec!["ttfx".to_string(), ttfx_name(name).to_string()];
        argv.extend(args.iter().cloned());
        let parsed: ttfx::cli::Cli =
            clap::Parser::try_parse_from(&argv).ok().or_else(|| clap::Parser::try_parse_from(["ttfx", ttfx_name(name)]).ok())?;
        let command = parsed.effect?;
        let config = TerminalConfig {
            canvas_width: i64::from(width.max(1)),
            canvas_height: i64::from(height.max(1)),
            ignore_terminal_dimensions: true,
            anchor_canvas: ttfx::engine::canvas::Anchor::parse("c")?,
            anchor_text: ttfx::engine::canvas::Anchor::parse("c")?,
            frame_rate: 30,
            ..TerminalConfig::default()
        };
        let ctx = EngineCtx::new(text, config, ttfx::utils::rng::Rng::from_entropy(), Clock::virtual_with_frame_rate(30)).ok()?;
        let mut effect = command.build_effect();
        let mut ctx = ctx;
        effect.build(&mut ctx).ok()?;
        Some(Self { width, height, effect, ctx, frame: None, done: false, frames: 0, max_frames, last_step: None, speed: 1 })
    }

    /// Advance at most one frame per `FRAME_INTERVAL` of wall time, so playback speed does not
    /// depend on how often the screen redraws (keystrokes, worker events). Returns false when finished.
    pub fn advance(&mut self) -> bool {
        if self.done {
            return false;
        }
        let now = std::time::Instant::now();
        // A few ms of slack so a loop waking just before the deadline doesn't drop to half speed.
        let slack = std::time::Duration::from_millis(4);
        if self.last_step.is_some_and(|t| now + slack < t + FRAME_INTERVAL) {
            return true;
        }
        // Keep a fixed cadence, but never try to catch up after a stall.
        self.last_step = Some(match self.last_step {
            Some(t) if now.saturating_duration_since(t) < FRAME_INTERVAL * 2 => t + FRAME_INTERVAL,
            _ => now,
        });
        self.step()
    }

    /// Play this effect `n` frames per step. Only the last frame of each step is painted, so the
    /// cost is one render either way.
    pub fn at_speed(mut self, n: u32) -> Self {
        self.speed = n.max(1);
        self
    }

    /// Advance one step — `speed` effect frames. Returns false when finished.
    pub fn step(&mut self) -> bool {
        let mut alive = self.one_frame();
        for _ in 1..self.speed {
            if !alive {
                break;
            }
            alive = self.one_frame();
        }
        alive
    }

    fn one_frame(&mut self) -> bool {
        if self.done {
            return false;
        }
        match self.effect.next_frame(&mut self.ctx) {
            Some(frame) => {
                self.frame = Some(frame);
                self.frames += 1;
                if self.max_frames > 0 && self.frames >= self.max_frames {
                    self.done = true;
                }
                true
            }
            None => {
                self.done = true;
                false
            }
        }
    }

    /// Paint the current frame centered in `area`.
    pub fn render(&self, area: Rect, buf: &mut Buffer, base: Style) {
        if let Some(frame) = &self.frame {
            paint_ansi(frame, area, buf, base);
            legible(area, buf, base);
        }
    }

    /// Canvas size the effect was built for.
    pub fn size(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    /// The current frame, as SGR text — what a hand-over carries over into the next effect.
    pub fn frame(&self) -> Option<&str> {
        self.frame.as_deref()
    }

    /// Whether a frame has been produced yet. A ceremony carries none until it is stepped, and
    /// painting one before that leaves the canvas blank — which is what a hand-over must avoid.
    #[cfg(test)]
    pub fn has_frame(&self) -> bool {
        self.frame.is_some()
    }

    /// Whether the effect has run out (frame cap, or the effect itself ended).
    #[cfg(test)]
    pub fn finished(&self) -> bool {
        self.done
    }
}

/// Paint SGR-colored text into a buffer region, clipping to the area.
pub fn paint_ansi(text: &str, area: Rect, buf: &mut Buffer, base: Style) {
    paint_dissolve(text, area, buf, base, 1.0);
}

/// Paint SGR-colored text, keeping only `keep` of its cells — the rest are left untouched.
///
/// This is how one lock-screen effect hands over to the next: the finished wordmark is painted
/// under the incoming effect and eroded cell by cell as `keep` falls, so the screen dissolves from
/// one animation into the other instead of cutting to an empty canvas. It erodes rather than dims
/// because `legible` would pull dimmed cells straight back up to a readable contrast.
///
/// Which cells go is a hash of the coordinate, not a random draw: a cell that has gone stays gone
/// as `keep` falls, so the wordmark thins out steadily rather than flickering.
pub fn paint_dissolve(text: &str, area: Rect, buf: &mut Buffer, base: Style, keep: f32) {
    let survives = |x: u16, y: u16| {
        if keep >= 1.0 {
            return true;
        }
        let n = u32::from(x).wrapping_mul(2_654_435_761) ^ u32::from(y).wrapping_mul(40_503).wrapping_mul(2_246_822_519);
        (f32::from(((n >> 11) & 0xff) as u8) / 255.0) < keep
    };
    let mut style = base;
    let mut x = area.x;
    let mut y = area.y;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                let mut params = String::new();
                let mut terminator = ' ';
                for p in chars.by_ref() {
                    if p.is_ascii_alphabetic() {
                        terminator = p;
                        break;
                    }
                    params.push(p);
                }
                if terminator == 'm' {
                    style = apply_sgr(style, base, &params);
                }
            }
            continue;
        }
        if c == '\n' {
            y += 1;
            x = area.x;
            style = base;
            if y >= area.y + area.height {
                break;
            }
            continue;
        }
        if c.is_control() {
            continue;
        }
        if x < area.x + area.width
            && y < area.y + area.height
            && (c != ' ' || style.bg.is_some_and(|b| b != base.bg.unwrap_or(Color::Reset)))
            && survives(x, y)
            && let Some(cell) = buf.cell_mut((x, y))
        {
            cell.set_char(c).set_style(style);
        }
        x += 1;
    }
}

/// Effects bring their own palettes (near-white beams, neon rain). Pull any painted color that
/// would be hard to read on its background toward the theme's text color, so every effect is
/// legible on light and dark themes alike.
fn legible(area: Rect, buf: &mut Buffer, base: Style) {
    use super::theme::contrast;
    let rgb = |c: Option<Color>| match c {
        Some(Color::Rgb(r, g, b)) => Some((r, g, b)),
        _ => None,
    };
    let (Some(surface), Some(text)) = (rgb(base.bg), rgb(base.fg)) else { return };
    let bottom = area.bottom().min(buf.area.bottom());
    let right = area.right().min(buf.area.right());
    for y in area.y..bottom {
        for x in area.x..right {
            let Some(cell) = buf.cell_mut((x, y)) else { continue };
            let bg = match cell.bg {
                Color::Rgb(r, g, b) => (r, g, b),
                _ => surface,
            };
            let Color::Rgb(r, g, b) = cell.fg else { continue };
            let mut fg = (r, g, b);
            let mut steps = 0;
            while contrast(fg, bg) < 3.0 && steps < 8 {
                let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * 0.3) as u8;
                fg = (mix(fg.0, text.0), mix(fg.1, text.1), mix(fg.2, text.2));
                steps += 1;
            }
            cell.fg = Color::Rgb(fg.0, fg.1, fg.2);
        }
    }
}

fn apply_sgr(mut style: Style, base: Style, params: &str) -> Style {
    let codes: Vec<u16> = params.split(';').filter_map(|p| p.parse().ok()).collect();
    if codes.is_empty() {
        return base;
    }
    let mut i = 0;
    while i < codes.len() {
        match codes[i] {
            0 => style = base,
            1 => style = style.add_modifier(Modifier::BOLD),
            2 => style = style.add_modifier(Modifier::DIM),
            3 => style = style.add_modifier(Modifier::ITALIC),
            4 => style = style.add_modifier(Modifier::UNDERLINED),
            7 => style = style.add_modifier(Modifier::REVERSED),
            39 => style.fg = base.fg,
            49 => style.bg = base.bg,
            38 | 48 => {
                let fg = codes[i] == 38;
                let color = match codes.get(i + 1) {
                    Some(2) if i + 4 < codes.len() + 1 && codes.len() > i + 4 => {
                        let c = Color::Rgb(codes[i + 2] as u8, codes[i + 3] as u8, codes[i + 4] as u8);
                        i += 4;
                        Some(c)
                    }
                    Some(5) if codes.len() > i + 2 => {
                        let c = Color::Indexed(codes[i + 2] as u8);
                        i += 2;
                        Some(c)
                    }
                    _ => None,
                };
                if let Some(c) = color {
                    if fg {
                        style.fg = Some(c);
                    } else {
                        style.bg = Some(c);
                    }
                }
            }
            c @ 30..=37 => style.fg = Some(Color::Indexed((c - 30) as u8)),
            c @ 90..=97 => style.fg = Some(Color::Indexed((c - 90 + 8) as u8)),
            c @ 40..=47 => style.bg = Some(Color::Indexed((c - 40) as u8)),
            _ => {}
        }
        i += 1;
    }
    style
}

/// Every ttfx effect, with a one-line description for the lock screen gallery.
pub const EFFECTS: &[(&str, &str)] = &[
    ("beams", "beams sweep the canvas, lighting the text behind them"),
    ("binarypath", "binary trails travel home to each character"),
    ("blackhole", "a black hole swallows the text and bursts it back out"),
    ("bouncyballs", "characters drop in as bouncing balls"),
    ("bubbles", "bubbles float down and pop into letters"),
    ("burn", "the text burns in from the bottom"),
    ("colorshift", "a slowly shifting color gradient"),
    ("crumble", "letters crumble to dust and reform"),
    ("decrypt", "movie-style decryption"),
    ("errorcorrect", "misplaced characters snap into place"),
    ("expand", "the text grows from a single point"),
    ("fireworks", "characters launch, explode and fall into place"),
    ("highlight", "a specular highlight glides across"),
    ("laseretch", "a laser etches every character"),
    ("matrix", "digital rain resolves into the wordmark"),
    ("matrix-red", "Quai-red digital rain resolves into the wordmark"),
    ("middleout", "a line opens from the middle, then everything"),
    ("orbittingvolley", "orbiting launchers fire the text into place"),
    ("overflow", "scrolling noise settles into order"),
    ("pour", "characters pour into position"),
    ("print", "a print head types each line"),
    ("rain", "characters rain from the top"),
    ("randomsequence", "letters appear in random order"),
    ("rings", "characters spin in rings, then settle"),
    ("scattered", "scattered letters drift home"),
    ("slice", "two halves slide together"),
    ("slide", "characters slide in from the edges"),
    ("smoke", "smoke drifts across, coloring what it touches"),
    ("spotlights", "spotlights search, converge and expand"),
    ("spray", "characters spray from one point"),
    ("swarm", "swarms circle and land"),
    ("sweep", "a sweep reveals, then colors, the text"),
    ("synthgrid", "a synthwave grid dissolves into the text"),
    ("thunderstorm", "rain, lightning and thunder"),
    ("unstable", "jumbled letters explode and reassemble"),
    ("vhstape", "VHS tracking glitches"),
    ("waves", "waves roll across, leaving the text behind"),
    ("wipe", "a wipe reveals the text"),
];

/// The ttfx effect behind a gallery name (`matrix-red` is `matrix` with red rain).
pub fn ttfx_name(name: &str) -> &str {
    name.strip_suffix("-red").unwrap_or(name)
}

/// Pick a random lock-screen effect.
pub fn random_lock_effect() -> &'static str {
    let mut b = [0u8; 2];
    let _ = wallet_core::sdk::crypto::fill_random(&mut b);
    EFFECTS[u16::from_le_bytes(b) as usize % EFFECTS.len()].0
}

/// Rain for `matrix` where its katakana cannot be drawn: ttfx's own ASCII rain symbols, with the
/// rest of the digits. None starts with `-`, which the argument parser would take for a flag.
const ASCII_RAIN: &[&str] =
    &["0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "Z", "*", ")", "(", ":", ".", "\"", "=", "+", "|", "_", "<", ">"];

static KATAKANA: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Whether this terminal can draw half-width katakana (U+FF66–FF9D), most of `matrix`'s rain.
///
/// Never waits: until [`probe_fonts`] has answered, the rain is ASCII, which draws everywhere.
/// Missing glyphs are drawn blank or boxed, so a rain of them reads as a broken screen.
pub fn katakana_renders() -> bool {
    KATAKANA.get().copied().unwrap_or(false)
}

/// Ask once, off the render path, whether a font here covers half-width katakana (fontconfig
/// takes tens of milliseconds). macOS always ships one. Over SSH the font is on the other
/// machine and the Linux console has none, so both keep the ASCII rain, as does a system
/// without fontconfig.
pub fn probe_fonts() {
    std::thread::spawn(|| {
        let remote = std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some();
        let console = std::env::var("TERM").is_ok_and(|t| t == "linux");
        let covered = !remote
            && !console
            && (cfg!(target_os = "macos")
                || std::process::Command::new("fc-list")
                    .args([":charset=ff71", "family"])
                    .stderr(std::process::Stdio::null())
                    .output()
                    .is_ok_and(|out| out.status.success() && !out.stdout.trim_ascii().is_empty()));
        let _ = KATAKANA.set(covered);
    });
}

/// The `--rain-symbols` a matrix effect needs where its katakana would not draw.
fn rain_args(effect: &str, katakana: bool) -> Vec<String> {
    if ttfx_name(effect) != "matrix" || katakana {
        return Vec::new();
    }
    std::iter::once("--rain-symbols").chain(ASCII_RAIN.iter().copied()).map(str::to_string).collect()
}

/// ttfx arguments that tint an effect with the active theme (accent → QUAI → Qi gradient), and
/// keep its symbols to ones this terminal can draw.
pub fn theme_args(effect: &str, theme: &super::theme::Theme) -> Vec<String> {
    let mut args = theme_colors(effect, theme);
    args.extend(rain_args(effect, katakana_renders()));
    args
}

fn theme_colors(effect: &str, theme: &super::theme::Theme) -> Vec<String> {
    use super::theme::Theme;
    if effect == "matrix-red" {
        // Quai's brand red on black, whatever the active theme.
        return [
            "--rain-color-gradient",
            "ff3a14",
            "7a0c00",
            "--highlight-color",
            "ffd9cf",
            "--final-gradient-stops",
            "ff3a14",
            "e22901",
            "ffffff",
        ]
        .map(str::to_string)
        .to_vec();
    }
    let (Some(accent), Some(quai), Some(qi), Some(ok)) =
        (Theme::hex(theme.focus), Theme::hex(theme.quai), Theme::hex(theme.qi), Theme::hex(theme.ok))
    else {
        return Vec::new();
    };
    let mut args = Vec::new();
    match effect {
        "synthgrid" => {}
        _ => args.extend(["--final-gradient-stops".to_string(), accent.clone(), quai.clone(), qi.clone()]),
    }
    match effect {
        "fireworks" => args.extend(["--firework-colors".to_string(), ok, qi, quai, accent]),
        "rings" => args.extend(["--ring-colors".to_string(), accent, qi, ok]),
        _ => {}
    }
    args
}

/// Decrypted after the `:poem` hash rain (ASCII: ttfx treats one codepoint as one cell).
pub const POEM_HAIKU: &str = "hashes fall like rain\nthe least of them holds the chain\norder out of noise";

/// The `:poem` rain: recent block hashes, with the entropy minimum (the smallest hash, the most
/// leading zeros) marked. Public chain data only. None until a few blocks have been seen.
pub fn poem_rain<'a>(hashes: impl Iterator<Item = &'a str>) -> Option<String> {
    let hashes: Vec<String> = hashes
        .map(|h| h.trim_start_matches("0x").to_ascii_lowercase())
        .filter(|h| !h.is_empty() && h.chars().all(|c| c.is_ascii_hexdigit()))
        .collect();
    if hashes.len() < 3 {
        return None;
    }
    let winner = hashes.iter().min_by(|a, b| (a.len(), a.as_str()).cmp(&(b.len(), b.as_str())))?.clone();
    let mut lines: Vec<String> = hashes
        .iter()
        .rev()
        .take(12)
        .map(|h| {
            let short: String = h.chars().take(32).collect();
            if *h == winner { format!("> 0x{short} <  entropy minimum") } else { format!("  0x{short}") }
        })
        .collect();
    if !lines.iter().any(|l| l.starts_with('>')) {
        let short: String = winner.chars().take(32).collect();
        lines.push(format!("> 0x{short} <  entropy minimum"));
    }
    Some(lines.join("\n"))
}

/// Seal stamped by celebrations (ASCII only: ttfx treats one codepoint as one cell).
pub fn seal(word: &str) -> String {
    let inner = format!("  {word}  ");
    let rule = "-".repeat(inner.len());
    format!("+{rule}+\n|{inner}|\n+{rule}+")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every line of the wordmark is the same width. Each is centred on its own, so a short row
    /// slides sideways against the others — which is exactly what a ragged top row did.
    ///
    /// The strokes are solid blocks, not line art: that is what gives an effect's gradient
    /// something to fill.
    #[test]
    fn the_wordmark_is_a_rectangle() {
        let block = wordmark_block();
        let widths: Vec<usize> = block.lines().map(|l| l.chars().count()).collect();
        assert_eq!(widths.len(), 5);
        assert!(widths.iter().all(|w| *w == widths[0]), "ragged wordmark: {widths:?}");
        // And it is the art itself that got wider, not the padding: the widest line is unchanged.
        assert_eq!(widths[0], WORDMARK.lines().map(|l| l.chars().count()).max().unwrap());
        // Solid strokes, not line art: effects colour cells, so a letter needs a body to fill.
        let filled = block.chars().filter(|c| *c == '█').count();
        assert!(filled * 3 > block.chars().filter(|c| *c != '\n').count(), "only {filled} filled cells");
        assert!(!block.contains('_') && !block.contains('\\'), "line art left in the wordmark");
    }

    #[test]
    fn poem_marks_the_entropy_minimum() {
        assert!(poem_rain(["0xab", "0xcd"].into_iter()).is_none(), "needs a few blocks");
        let hashes = ["0x9f3c00aa", "0x0000abcd", "0x00ff1234", "0x77777777"];
        let text = poem_rain(hashes.into_iter()).unwrap();
        assert!(text.is_ascii());
        let marked: Vec<&str> = text.lines().filter(|l| l.starts_with('>')).collect();
        assert_eq!(marked, vec!["> 0x0000abcd <  entropy minimum"]);
        assert!(POEM_HAIKU.is_ascii() && POEM_HAIKU.lines().count() == 3);
        // The red matrix builds from the matrix effect with red rain.
        assert_eq!(ttfx_name("matrix-red"), "matrix");
        let t = super::super::theme::resolve(std::path::Path::new("/nonexistent"), "quai-dark", false, false).0;
        assert!(theme_args("matrix-red", &t).iter().any(|a| a == "ff3a14"));
    }

    #[test]
    fn speed_advances_several_frames_per_step() {
        // At 1× a step is one frame; at 3× the same number of steps consumes three times as many.
        let frames_after = |speed: u32, steps: usize| {
            let mut c = Ceremony::new("rings", "quai", 40, 10, 60).unwrap().at_speed(speed);
            for _ in 0..steps {
                c.step();
            }
            c.frames
        };
        assert_eq!(frames_after(1, 5), 5);
        assert_eq!(frames_after(3, 5), 15, "three frames per redraw, one render");
        assert_eq!(frames_after(0, 5), 5, "zero is clamped to one, never a stall");
        // The frame cap still ends it, and a finished effect stays finished.
        let mut c = Ceremony::new("rings", "quai", 40, 10, 4).unwrap().at_speed(3);
        assert!(c.step(), "three of four frames used");
        assert!(!c.step(), "the fourth frame hits the cap, and the step reports the end");
        assert!(c.done);
        assert!(!c.step(), "a finished effect stays finished");
        assert_eq!(c.frames, 4, "never past the cap");
    }

    /// Where no font covers katakana, both matrix rains fall in ASCII. ttfx quietly falls back to
    /// its katakana defaults when it rejects arguments, so this is checked in the frames, not
    /// the arguments.
    #[test]
    fn matrix_rain_falls_in_ascii_where_katakana_cannot_be_drawn() {
        let keys = super::super::theme::builtin("tokyo-night").unwrap();
        let theme = super::super::theme::Theme::from_palette("tokyo-night", "t", &keys).unwrap();
        let rain = |name: &str, katakana: bool| {
            let mut args = theme_colors(name, &theme);
            args.extend(rain_args(name, katakana));
            let mut c = Ceremony::with_args(name, &args, &wordmark_block(), 100, 30, 400).unwrap();
            let (mut kana, mut other) = (0, 0);
            for _ in 0..90 {
                c.step();
                for ch in c.frame().unwrap_or_default().split('\x1b').flat_map(|s| s.split_once('m').map(|(_, t)| t)).flat_map(str::chars) {
                    match ch {
                        '\u{FF66}'..='\u{FF9D}' => kana += 1,
                        ' ' | '\n' | '█' | '▄' => {}
                        _ => other += 1,
                    }
                }
            }
            (kana, other)
        };
        for name in ["matrix", "matrix-red"] {
            let (kana, other) = rain(name, false);
            assert_eq!(kana, 0, "{name}: katakana fell where no font draws it");
            assert!(other > 0, "{name}: and the rain still falls");
            assert!(rain(name, true).0 > 0, "{name}: where katakana draws, it is the rain");
        }
        assert!(rain_args("rain", false).is_empty(), "only the matrix rain is katakana");
    }

    #[test]
    fn effects_build_step_and_paint() {
        for name in EFFECTS.iter().map(|(n, _)| *n) {
            let mut c = Ceremony::new(name, WORDMARK, 80, 12, 120).unwrap_or_else(|| panic!("{name}"));
            let mut steps = 0;
            while c.step() && steps < 40 {
                steps += 1;
            }
            assert!(steps > 0, "{name}");
            let area = Rect::new(0, 0, 80, 12);
            let mut buf = Buffer::empty(area);
            c.render(area, &mut buf, Style::default());
        }
    }

    #[test]
    fn themed_celebrations_build() {
        let keys = super::super::theme::builtin("tokyo-night").unwrap();
        let theme = super::super::theme::Theme::from_palette("tokyo-night", "t", &keys).unwrap();
        for name in EFFECTS.iter().map(|(n, _)| *n) {
            let args = theme_args(name, &theme);
            let mut argv = vec!["ttfx".to_string(), ttfx_name(name).to_string()];
            argv.extend(args.iter().cloned());
            let parsed: Result<ttfx::cli::Cli, _> = clap::Parser::try_parse_from(&argv);
            assert!(parsed.is_ok(), "{name}: {:?}", parsed.err().map(|e| e.to_string()));
            let mut c = Ceremony::with_args(name, &args, &seal("RECEIVED"), 30, 5, 60).unwrap_or_else(|| panic!("{name}"));
            assert!(c.step(), "{name}");
        }
    }

    /// The hand-over dissolve thins a frame steadily: fewer cells survive as `keep` falls, a cell
    /// that has gone stays gone, and `keep` of 1 paints everything (which is plain `paint_ansi`).
    #[test]
    fn the_dissolve_erodes_a_frame_without_flicker() {
        let area = Rect::new(0, 0, 40, 6);
        let text = wordmark_block();
        let painted = |keep: f32| -> std::collections::HashSet<(u16, u16)> {
            let mut buf = Buffer::empty(area);
            paint_dissolve(&text, area, &mut buf, Style::default(), keep);
            let mut set = std::collections::HashSet::new();
            for y in 0..area.height {
                for x in 0..area.width {
                    if buf[(x, y)].symbol().trim() != "" {
                        set.insert((x, y));
                    }
                }
            }
            set
        };
        let (all, half, gone) = (painted(1.0), painted(0.5), painted(0.0));
        assert!(gone.is_empty(), "nothing survives keep = 0");
        assert!(!all.is_empty() && half.len() < all.len(), "{} of {} at half", half.len(), all.len());
        assert!(half.len() * 4 > all.len(), "half should thin it, not erase it: {}", half.len());
        // Monotone: every cell alive at a lower keep is still alive at a higher one, so the
        // wordmark only ever loses cells as the dissolve runs.
        for keep in [0.25_f32, 0.5, 0.75] {
            let fewer = painted(keep);
            assert!(fewer.is_subset(&all), "cells reappeared at keep = {keep}");
            assert!(painted(keep - 0.2).is_subset(&fewer), "a cell came back as keep fell past {keep}");
        }
    }

    #[test]
    fn sgr_parsing() {
        let area = Rect::new(0, 0, 10, 2);
        let mut buf = Buffer::empty(area);
        paint_ansi("\x1b[38;2;255;0;0mA\x1b[0mB\nC", area, &mut buf, Style::default());
        assert_eq!(buf[(0, 0)].symbol(), "A");
        assert_eq!(buf[(0, 0)].fg, Color::Rgb(255, 0, 0));
        assert_eq!(buf[(1, 0)].symbol(), "B");
        assert_eq!(buf[(0, 1)].symbol(), "C");
    }
}
