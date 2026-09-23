//! What the terminal says it can do, asked once at startup.
//!
//! Every question goes out in one write with device attributes (DA1) last; terminals answer in
//! order, so DA1 arriving means every answer is in, and the wait ends there — a few milliseconds
//! on a local terminal, where the old probe always waited out its full window. A terminal that
//! doesn't know a question stays silent on it; one that answers nothing is given up on after
//! [`TIMEOUT`]. Keys typed in those milliseconds are dropped: a reply split across reads can
//! arrive looking like keystrokes, and nobody has a screen to type at yet.

use std::io::Write;
use std::time::{Duration, Instant};
use termina::Event;
use termina::escape::csi::{Csi, Cursor, DecModeSetting, DecPrivateMode, DecPrivateModeCode, Device, Keyboard, Mode};
use termina::escape::dcs::{Dcs, DcsResponse};
use termina::escape::osc::{ColorOrQuery, DynamicColorNumber, Osc};

/// Longest wait for a terminal that answers nothing at all (the Linux console, some serial
/// lines). Every terminal worth drawing on answers DA1 in well under this.
pub const TIMEOUT: Duration = Duration::from_millis(300);

/// A test color for the truecolor check: set as a background, then asked for back.
const PROBE_RGB: (u8, u8, u8) = (150, 151, 152);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Answers {
    /// The terminal's default background and foreground (OSC 11 / OSC 10).
    pub background: Option<(u8, u8, u8)>,
    pub foreground: Option<(u8, u8, u8)>,
    /// The kitty keyboard protocol is understood (it answered `CSI ? u`).
    pub kitty_keyboard: bool,
    /// Synchronized output (DEC 2026) is known to the terminal.
    pub sync_output: bool,
    /// The terminal reported back a 24-bit background it was given (DECRQSS), so truecolor
    /// works even where `COLORTERM` wasn't passed along (SSH).
    pub truecolor: bool,
    /// Colored underlines (SGR 58) came back the same way.
    pub underline_color: bool,
    /// The palette's color 8 ("bright black"), which the terminal theme draws lines and faint
    /// text in: on some palettes it is the background itself (OSC 4).
    pub ansi8: Option<(u8, u8, u8)>,
    /// Text drawn larger than a cell (kitty's text sizing protocol, OSC 66): a two-cell-wide
    /// space moved the cursor two columns.
    pub text_sizing: bool,
    /// DA1 arrived: the terminal answers queries at all.
    pub answered: bool,
    /// How long the whole exchange took.
    pub took: Duration,
    /// Input that arrived after the last answer, in the same read: keys pressed just after
    /// startup, which belong to the UI.
    pub after: Vec<Event>,
}

impl Answers {
    /// Whether the background is light (perceived luminance over half).
    pub fn light(&self) -> Option<bool> {
        self.background.map(|(r, g, b)| u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114 >= 128_000)
    }
}

/// The questions, in the order the answers are expected.
pub fn queries() -> String {
    let (r, g, b) = PROBE_RGB;
    [
        "\x1b]11;?\x1b\\",
        "\x1b]10;?\x1b\\",
        "\x1b[?u",
        "\x1b[?2026$p",
        // Truecolor and colored underlines: set both, ask what is set, put everything back.
        &format!("\x1b[48;2;{r};{g};{b}m\x1b[58;2;{r};{g};{b}m\x1bP$qm\x1b\\\x1b[0m"),
        // Palette color 8 (termina does not parse OSC 4; `palette_reply` reads it from the bytes).
        "\x1b]4;8;?\x1b\\",
        // Text sizing: a space two cells wide, then where the cursor ended up; the line is
        // cleared after. (Width, not scale: a scaled space on the last row would scroll.)
        "\r\x1b]66;w=2; \x07\x1b[6n\r\x1b[K",
        "\x1b[c",
    ]
    .concat()
}

/// Fold one reply into the answers. Returns true on DA1, the last answer.
pub fn absorb(answers: &mut Answers, event: &Event) -> bool {
    match event {
        Event::Osc(Osc::ChangeDynamicColors(which, values)) => {
            if let Some(ColorOrQuery::Color(c)) = values.first() {
                let rgb = Some((c.red, c.green, c.blue));
                match which {
                    DynamicColorNumber::TextBackgroundColor => answers.background = rgb,
                    DynamicColorNumber::TextForegroundColor => answers.foreground = rgb,
                    _ => {}
                }
            }
        }
        Event::Csi(Csi::Keyboard(Keyboard::ReportFlags(_))) => answers.kitty_keyboard = true,
        Event::Csi(Csi::Mode(Mode::ReportDecPrivateMode {
            mode: DecPrivateMode::Code(DecPrivateModeCode::SynchronizedOutput),
            setting,
        })) => {
            answers.sync_output = matches!(setting, DecModeSetting::Set | DecModeSetting::Reset | DecModeSetting::PermanentlySet);
        }
        Event::Dcs(Dcs::Response { value: DcsResponse::GraphicRendition(sgrs), .. }) => {
            let text = format!("{sgrs:?}");
            let (r, g, b) = PROBE_RGB;
            let has = |needle: &str| text.contains(needle);
            // The debug form names the channels; both layers carry the same color.
            let rgb = format!("red: {r}, green: {g}, blue: {b}");
            answers.truecolor = has("Background") && has(&rgb);
            answers.underline_color = has("UnderlineColor") && has(&rgb);
        }
        // From column 1, a terminal that sized the space is at column 3; one that ignored it is
        // still at 1.
        Event::Csi(Csi::Cursor(Cursor::ActivePositionReport { col, .. })) => answers.text_sizing = col.get() == 3,
        Event::Csi(Csi::Device(Device::DeviceAttributes(_))) => {
            answers.answered = true;
            return true;
        }
        _ => {}
    }
    false
}

/// Ask, and read the answers until DA1 or the timeout. Input that isn't an answer is dropped.
///
/// The answers are read here byte by byte through termina's parser with "more may follow" set,
/// rather than through the event reader: the reader decides a lone Esc at the end of a short
/// read *is* the Esc key, so a reply the terminal happened to split right after its Esc would
/// arrive as `Esc`, `]`, `1`, `1`, … — the terminal typing its own answer into the UI. That was
/// seen under load before this rewrite. Nothing else reads the terminal until this returns.
#[cfg(unix)]
pub fn run(out: &mut dyn Write, input: impl std::os::fd::AsFd) -> Answers {
    use rustix::event::{PollFd, PollFlags, Timespec};
    let started = Instant::now();
    let mut answers = Answers::default();
    if out.write_all(queries().as_bytes()).and_then(|_| out.flush()).is_err() {
        return answers;
    }
    let mut parser = termina::Parser::default();
    let deadline = started + TIMEOUT;
    let mut buf = [0u8; 1024];
    // Everything read, for the replies termina drops (palette colors).
    let mut raw = Vec::new();
    'read: loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let timeout = Timespec { tv_sec: left.as_secs() as _, tv_nsec: left.subsec_nanos() as _ };
        let mut fds = [PollFd::new(&input, PollFlags::IN)];
        match rustix::event::poll(&mut fds, Some(&timeout)) {
            Ok(0) => break,
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(_) => break,
        }
        let n = match rustix::io::read(&input, &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        raw.extend_from_slice(&buf[..n]);
        parser.parse(&buf[..n], true);
        while let Some(event) = parser.pop() {
            if absorb(&mut answers, &event) {
                // What follows the last answer is ordinary input: hand it on. An incomplete
                // tail is settled now (a lone Esc there is the Esc key).
                // Answers that came out of order after it are still answers.
                parser.parse(&[], false);
                while let Some(event) = parser.pop() {
                    if event.is_escape() {
                        absorb(&mut answers, &event);
                    } else {
                        answers.after.push(event);
                    }
                }
                break 'read;
            }
        }
    }
    answers.ansi8 = palette_reply(&raw, 8);
    answers.took = started.elapsed();
    answers
}
/// A palette color reply (`OSC 4 ; n ; rgb:RRRR/GGGG/BBBB` ended by ST or BEL) in raw bytes.
pub fn palette_reply(raw: &[u8], index: u8) -> Option<(u8, u8, u8)> {
    let text = String::from_utf8_lossy(raw);
    let start = text.find(&format!("\x1b]4;{index};rgb:"))? + format!("\x1b]4;{index};rgb:").len();
    let rest = &text[start..];
    let end = rest.find(['\x1b', '\x07'])?;
    let mut parts = rest[..end].split('/').map(|h| {
        // 1–4 hex digits a channel; the top eight bits are the color.
        let v = u32::from_str_radix(h, 16).ok()?;
        Some(match h.len() {
            1 => (v * 17) as u8,
            2 => v as u8,
            3 => (v >> 4) as u8,
            _ => (v >> 8) as u8,
        })
    });
    Some((parts.next()??, parts.next()??, parts.next()??))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_palette_reply_is_read_from_the_bytes() {
        assert_eq!(
            super::palette_reply(b"\x1b]11;rgb:0000/0000/0000\x1b\\\x1b]4;8;rgb:0000/2b2b/3636\x1b\\\x1b[?62c", 8),
            Some((0, 0x2b, 0x36))
        );
        assert_eq!(super::palette_reply(b"\x1b]4;8;rgb:58/6e/75\x07", 8), Some((0x58, 0x6e, 0x75)));
        assert_eq!(super::palette_reply(b"\x1b[?62c", 8), None);
    }

    use super::*;

    /// termina reads real reply bytes into the answers; a terminal that splits its replies is
    /// still read whole.
    #[test]
    fn replies_are_read_into_answers() {
        let replies: &[u8] = b"\x1b]11;rgb:1e1e/2020/2e2e\x1b\\\x1b]10;rgb:c0c0/caca/f5f5\x07\x1b[?1u\x1b[?2026;2$y\x1bP1$r0;48:2::150:151:152;58:2::150:151:152m\x1b\\\x1b[?62;22c";
        for split in [replies.len(), 7, 23, 50] {
            let mut parser = termina::Parser::default();
            let mut answers = Answers::default();
            let mut done = false;
            for chunk in replies.chunks(split) {
                parser.parse(chunk, true);
                while let Some(e) = parser.pop() {
                    done |= absorb(&mut answers, &e);
                }
            }
            assert!(done && answers.answered, "split {split}: DA1 ends the exchange");
            assert_eq!(answers.background, Some((0x1e, 0x20, 0x2e)), "split {split}");
            assert_eq!(answers.foreground, Some((0xc0, 0xca, 0xf5)), "split {split}");
            assert!(answers.kitty_keyboard && answers.sync_output, "split {split}: {answers:?}");
            assert_eq!(answers.light(), Some(false));
            assert!(!answers.text_sizing, "no position report, no text sizing");
        }
    }

    #[test]
    fn a_two_cell_space_means_text_sizing() {
        for (report, sized) in [(&b"\x1b[5;3R\x1b[?62c"[..], true), (&b"\x1b[5;1R\x1b[?62c"[..], false)] {
            let mut parser = termina::Parser::default();
            let mut answers = Answers::default();
            parser.parse(report, false);
            while let Some(e) = parser.pop() {
                absorb(&mut answers, &e);
            }
            assert_eq!(answers.text_sizing, sized, "{:?}", String::from_utf8_lossy(report));
        }
    }

    #[test]
    fn queries_end_with_device_attributes() {
        assert!(queries().ends_with("\x1b[c"), "DA1 last: its answer means the others are in");
    }
}
