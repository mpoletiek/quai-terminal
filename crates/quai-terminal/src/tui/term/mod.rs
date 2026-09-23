//! The terminal, owned in one place.
//!
//! [`Term`] opens the terminal through termina, asks what it can do ([`probe`]), switches modes
//! on through the [`modes`] registry (so every exit path can switch them off), draws frames
//! through [`backend::FrameBackend`] (one synchronized write each), and reads input as the app's
//! own event types ([`input`]). Background threads call [`wake`] after sending the UI anything,
//! which interrupts the wait for input so their result is on screen in the same moment, not at
//! the next timeout.

pub mod backend;
pub mod input;
pub mod modes;
pub mod probe;
pub mod signals;

use backend::{Depth, FrameBackend, Sink};
use modes::Mode;
use ratatui::backend::WindowSize;
use ratatui::layout::Size;
use std::io::{self, Write};
use std::sync::OnceLock;
use std::time::Duration;
use termina::{PlatformTerminal, Terminal as _};

/// Interrupts the loop's wait for input (set once the terminal is open).
static WAKE: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

/// Wake the UI loop: something was sent to it. Harmless before the TUI starts and after it ends.
pub fn wake() {
    if let Some(w) = WAKE.get() {
        w();
    }
}

/// Where the terminal's input comes from: stdin, or the controlling tty when stdin was
/// redirected (termina makes the same choice).
#[cfg(unix)]
fn input_handle() -> io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::AsFd;
    let stdin = std::io::stdin();
    if std::io::IsTerminal::is_terminal(&stdin) {
        return stdin.as_fd().try_clone_to_owned();
    }
    Ok(std::fs::File::open("/dev/tty")?.into())
}

/// The terminal as a place to write frames.
pub struct Tty(PlatformTerminal);

impl Write for Tty {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.write(b)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

/// The size to assume when the terminal can't say (a serial line, a PTY nobody sized).
const FALLBACK_SIZE: Size = Size { width: 80, height: 24 };

impl Sink for Tty {
    fn size(&self) -> io::Result<Size> {
        Ok(self.0.get_dimensions().map(|s| Size::new(s.cols, s.rows)).unwrap_or(FALLBACK_SIZE))
    }
    fn window_size(&self) -> io::Result<WindowSize> {
        Ok(match self.0.get_dimensions() {
            Ok(s) => WindowSize {
                columns_rows: Size::new(s.cols, s.rows),
                pixels: Size::new(s.pixel_width.unwrap_or_default(), s.pixel_height.unwrap_or_default()),
            },
            Err(_) => WindowSize { columns_rows: FALLBACK_SIZE, pixels: Size::new(0, 0) },
        })
    }
}

pub type Ui = ratatui::Terminal<FrameBackend<Tty>>;

/// How the pointer is reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pointer {
    /// No mouse reporting: the terminal's own selection works as usual.
    Off,
    /// Presses, releases, drags and the wheel.
    Clicks,
    /// Clicks, and every motion (hover).
    Hover,
}

pub struct Term {
    pub ui: Ui,
    reader: termina::EventReader,
    pub answers: probe::Answers,
    pointer: Pointer,
    /// The pointer shape last asked for (OSC 22), empty for the terminal's own.
    shape: &'static str,
    /// Pointer shapes are sent (not under a multiplexer, which does not pass them on).
    pub shapes: bool,
    /// Input read by the probe after its last answer, delivered before anything else.
    early: Vec<crossterm::event::Event>,
}

/// Whether this terminal is known to understand synchronized output even without answering the
/// mode query (older kitty and WezTerm builds answer late or not at all).
fn known_sync() -> bool {
    std::env::var_os("KITTY_WINDOW_ID").is_some()
        || std::env::var("TERM").is_ok_and(|t| t == "xterm-kitty" || t == "xterm-ghostty" || t.contains("wezterm"))
        || std::env::var("TERM_PROGRAM").is_ok_and(|p| matches!(p.as_str(), "ghostty" | "WezTerm" | "iTerm.app"))
}

impl Term {
    /// Open the terminal: raw mode, the capability probe, and the modes the UI runs in.
    /// `truecolor_hint` is what the environment says (`COLORTERM`); the probe can only add to it.
    pub fn start(truecolor_hint: bool, mono: bool) -> io::Result<Term> {
        // Whatever fails part-way, the terminal goes back as it was: modes switched on before the
        // failure are in the registry, and raw mode is in the saved line settings.
        Self::open(truecolor_hint, mono).inspect_err(|_| modes::restore())
    }

    fn open(truecolor_hint: bool, mono: bool) -> io::Result<Term> {
        modes::save_line_settings();
        let mut tty = PlatformTerminal::new()?;
        tty.enter_raw_mode()?;
        let reader = tty.event_reader();
        let waker = reader.waker();
        let _ = WAKE.set(Box::new(move || {
            let _ = waker.wake();
        }));
        let mut answers = probe::run(&mut tty, input_handle()?);
        let early = std::mem::take(&mut answers.after).into_iter().filter_map(input::convert).collect();
        let sync = answers.sync_output || known_sync();
        let depth = if mono {
            Depth::Ansi16
        } else if truecolor_hint || answers.truecolor {
            Depth::TrueColor
        } else if std::env::var("TERM").is_ok_and(|t| t.contains("256")) {
            Depth::Ansi256
        } else if std::env::var("TERM").is_ok_and(|t| t == "linux") {
            Depth::Ansi16
        } else {
            Depth::Ansi256
        };
        modes::enable(&mut tty, Mode::AltScreen)?;
        modes::enable(&mut tty, Mode::BracketedPaste)?;
        modes::enable(&mut tty, Mode::Focus)?;
        modes::enable(&mut tty, Mode::HiddenCursor)?;
        modes::enable(&mut tty, Mode::TitleStack)?;
        // Disambiguated keys: Esc arrives as its own sequence, so it is never mistaken for the
        // start of a reply, and never waits to find out.
        if answers.kitty_keyboard {
            modes::enable(&mut tty, Mode::KittyKeyboard(1))?;
        }
        tty.flush()?;
        let mut backend = FrameBackend::new(Tty(tty), depth, sync);
        backend.styled_underline = answers.underline_color;
        let mut ui = ratatui::Terminal::new(backend)?;
        ui.clear()?;
        Ok(Term { ui, reader, answers, pointer: Pointer::Off, shape: "", shapes: false, early })
    }

    /// Wait up to `timeout` for input (or a [`wake`]), then take everything already waiting, so
    /// one frame answers a whole burst. Pointer motion is coalesced.
    pub fn events(&mut self, timeout: Duration) -> io::Result<Vec<crossterm::event::Event>> {
        let mut out = std::mem::take(&mut self.early);
        let timeout = if out.is_empty() { timeout } else { Duration::ZERO };
        if !self.reader.poll(Some(timeout), |_| true)? {
            return Ok(out);
        }
        // Bounded, so a flood of motion can't hold the loop away from drawing.
        for _ in 0..512 {
            let event = self.reader.read(|_| true)?;
            // A reply that came after the probe gave up (a slow terminal) still counts.
            if event.is_escape() {
                probe::absorb(&mut self.answers, &event);
            }
            if let Some(e) = input::convert(event) {
                out.push(e);
            }
            if !self.reader.poll(Some(Duration::ZERO), |_| true)? {
                break;
            }
        }
        Ok(input::coalesce(out))
    }

    /// Queue bytes to go out with the next frame (images, clipboard, bell, notifications, title).
    pub fn queue(&mut self, bytes: &[u8]) {
        self.ui.backend_mut().queue(bytes);
    }

    /// Set how the pointer is reported.
    pub fn set_pointer(&mut self, pointer: Pointer) -> io::Result<()> {
        if pointer == self.pointer {
            return Ok(());
        }
        let mut seq: Vec<u8> = Vec::new();
        for m in [Mode::MouseMotion, Mode::MouseDrag, Mode::MouseClicks, Mode::MouseSgr] {
            modes::disable(&mut seq, m)?;
        }
        if pointer != Pointer::Off {
            modes::enable(&mut seq, Mode::MouseClicks)?;
            modes::enable(&mut seq, Mode::MouseDrag)?;
            modes::enable(&mut seq, Mode::MouseSgr)?;
            if pointer == Pointer::Hover {
                modes::enable(&mut seq, Mode::MouseMotion)?;
            }
        }
        self.pointer = pointer;
        self.queue(&seq);
        Ok(())
    }

    /// Set the pointer's shape (OSC 22) when it changes. "default" hands it back to the terminal.
    pub fn set_shape(&mut self, shape: &'static str) {
        let shape = if self.shapes && self.pointer == Pointer::Hover { shape } else { "default" };
        let shape = if shape == "default" { "" } else { shape };
        if shape == self.shape {
            return;
        }
        self.shape = shape;
        self.queue(format!("\x1b]22;{shape}\x1b\\").as_bytes());
    }

    /// Give the terminal back and stop the process, as Ctrl-Z does in a shell; take it again
    /// when the shell continues it, and redraw everything.
    pub fn suspend(&mut self) -> io::Result<()> {
        self.set_shape("default");
        let active = modes::active();
        modes::restore();
        signals::suspend_self();
        // Continued.
        modes::save_line_settings();
        self.ui.backend_mut().sink_mut().0.enter_raw_mode()?;
        let mut seq = Vec::new();
        for m in active {
            modes::enable(&mut seq, m)?;
        }
        self.queue(&seq);
        self.ui.clear()?;
        Ok(())
    }

    /// Put the screen back together after an outside stop (`kill -STOP`, then `CONT`): the
    /// terminal may have been reset under us, so raw mode and every recorded mode are sent again.
    pub fn repaint(&mut self) -> io::Result<()> {
        self.ui.backend_mut().sink_mut().0.enter_raw_mode()?;
        let active = modes::active();
        for m in &active {
            modes::disable(&mut io::sink(), *m)?;
        }
        let mut seq = Vec::new();
        for m in active {
            modes::enable(&mut seq, m)?;
        }
        self.queue(&seq);
        self.ui.clear()
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        // The terminal's own pointer back before anything else.
        self.set_shape("default");
        let _ = self.ui.backend_mut().end();
        modes::restore();
    }
}
