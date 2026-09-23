//! Every mode the TUI turns on, in one list, so the terminal is given back exactly as it was
//! found — on a normal exit, on a panic, on SIGTERM or SIGHUP, and around a Ctrl-Z suspend.
//!
//! A mode is recorded *before* its sequence is written, and [`restore`] undoes the list newest
//! first and restores the termios the TUI started from. It is idempotent and needs nothing but
//! this module's statics, so the panic hook and the signal thread can call it with the UI in any
//! state. Without it a killed wallet leaves the shell in raw mode, printing mouse reports for
//! every twitch of the pointer.

use std::io::Write;
use std::sync::Mutex;

/// A terminal mode the TUI can switch on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The alternate screen (1049): the shell's scrollback is left untouched.
    AltScreen,
    /// Bracketed paste (2004).
    BracketedPaste,
    /// Focus in/out reports (1004).
    Focus,
    /// Button presses and releases (1000).
    MouseClicks,
    /// Motion while a button is held (1002).
    MouseDrag,
    /// Every motion (1003): hover.
    MouseMotion,
    /// SGR mouse encoding (1006): exact coordinates past column 223.
    MouseSgr,
    /// The kitty keyboard protocol, pushed with these flags.
    KittyKeyboard(u8),
    /// The cursor hidden (25 reset).
    HiddenCursor,
    /// The window title pushed on the terminal's title stack (restored on pop).
    TitleStack,
}

impl Mode {
    fn on(self) -> String {
        match self {
            Mode::AltScreen => "\x1b[?1049h".into(),
            Mode::BracketedPaste => "\x1b[?2004h".into(),
            Mode::Focus => "\x1b[?1004h".into(),
            Mode::MouseClicks => "\x1b[?1000h".into(),
            Mode::MouseDrag => "\x1b[?1002h".into(),
            Mode::MouseMotion => "\x1b[?1003h".into(),
            Mode::MouseSgr => "\x1b[?1006h".into(),
            Mode::KittyKeyboard(flags) => format!("\x1b[>{flags}u"),
            Mode::HiddenCursor => "\x1b[?25l".into(),
            Mode::TitleStack => "\x1b[22;0t".into(),
        }
    }

    fn off(self) -> &'static str {
        match self {
            Mode::AltScreen => "\x1b[?1049l",
            Mode::BracketedPaste => "\x1b[?2004l",
            Mode::Focus => "\x1b[?1004l",
            Mode::MouseClicks => "\x1b[?1000l",
            Mode::MouseDrag => "\x1b[?1002l",
            Mode::MouseMotion => "\x1b[?1003l",
            Mode::MouseSgr => "\x1b[?1006l",
            Mode::KittyKeyboard(_) => "\x1b[<u",
            Mode::HiddenCursor => "\x1b[?25h",
            Mode::TitleStack => "\x1b[23;0t",
        }
    }
}

struct State {
    /// Modes switched on, oldest first.
    on: Vec<Mode>,
    /// The line settings the TUI started from (cooked mode).
    #[cfg(unix)]
    termios: Option<rustix::termios::Termios>,
}

static STATE: Mutex<State> = Mutex::new(State {
    on: Vec::new(),
    #[cfg(unix)]
    termios: None,
});

fn state() -> std::sync::MutexGuard<'static, State> {
    // A panic while this was held must not stop the terminal from being restored.
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// The terminal to talk to when the UI's own handle may be gone (panic, signal): the
/// controlling tty when there is one, else stdout.
fn tty() -> Box<dyn Write> {
    #[cfg(unix)]
    if let Ok(f) = std::fs::OpenOptions::new().write(true).open("/dev/tty") {
        return Box::new(f);
    }
    Box::new(std::io::stdout())
}

/// Remember the line settings before raw mode, so [`restore`] can put them back.
pub fn save_line_settings() {
    #[cfg(unix)]
    {
        let stdin = std::io::stdin();
        let settings = rustix::termios::tcgetattr(&stdin).ok().or_else(|| {
            let tty = std::fs::File::open("/dev/tty").ok()?;
            rustix::termios::tcgetattr(&tty).ok()
        });
        let mut s = state();
        if s.termios.is_none() {
            s.termios = settings;
        }
    }
}

/// Switch a mode on, recording it first: a signal between the two still finds it listed.
pub fn enable(out: &mut dyn Write, mode: Mode) -> std::io::Result<()> {
    {
        let mut s = state();
        if s.on.contains(&mode) {
            return Ok(());
        }
        s.on.push(mode);
    }
    out.write_all(mode.on().as_bytes())
}

/// Switch a mode off, if it is on.
pub fn disable(out: &mut dyn Write, mode: Mode) -> std::io::Result<()> {
    let was_on = {
        let mut s = state();
        let at = s.on.iter().position(|m| *m == mode);
        at.map(|i| s.on.remove(i)).is_some()
    };
    if was_on { out.write_all(mode.off().as_bytes()) } else { Ok(()) }
}

/// Whether a mode is on.
#[cfg(test)]
pub fn is_on(mode: Mode) -> bool {
    state().on.contains(&mode)
}

/// The modes that are on now, oldest first (to switch them back on after a suspend).
pub fn active() -> Vec<Mode> {
    state().on.clone()
}

/// Everything off, newest first, and the line settings back. Safe to call from anywhere, any
/// number of times; the second call does nothing.
pub fn restore() {
    let (modes, _termios) = {
        let mut s = state();
        #[cfg(unix)]
        let termios = s.termios.clone();
        #[cfg(not(unix))]
        let termios: Option<()> = None;
        (std::mem::take(&mut s.on), termios)
    };
    if !modes.is_empty() {
        let mut out = tty();
        let mut seq = String::new();
        for m in modes.iter().rev() {
            seq.push_str(m.off());
        }
        // Whatever was left half-drawn: attributes off, and a fresh line for the shell.
        seq.push_str("\x1b[0m");
        let _ = out.write_all(seq.as_bytes());
        let _ = out.flush();
    }
    #[cfg(unix)]
    if let Some(settings) = _termios {
        let stdin = std::io::stdin();
        if rustix::termios::tcsetattr(&stdin, rustix::termios::OptionalActions::Now, &settings).is_err()
            && let Ok(tty) = std::fs::File::open("/dev/tty")
        {
            let _ = rustix::termios::tcsetattr(&tty, rustix::termios::OptionalActions::Now, &settings);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tests share this module's global list, so they run as one.
    #[test]
    fn modes_are_recorded_undone_newest_first_and_only_once() {
        let mut out = Vec::new();
        enable(&mut out, Mode::AltScreen).unwrap();
        enable(&mut out, Mode::MouseClicks).unwrap();
        enable(&mut out, Mode::MouseSgr).unwrap();
        enable(&mut out, Mode::MouseSgr).unwrap();
        assert_eq!(String::from_utf8_lossy(&out), "\x1b[?1049h\x1b[?1000h\x1b[?1006h", "a mode already on is not sent twice");
        assert!(is_on(Mode::MouseClicks));
        out.clear();
        disable(&mut out, Mode::MouseClicks).unwrap();
        disable(&mut out, Mode::MouseClicks).unwrap();
        assert_eq!(String::from_utf8_lossy(&out), "\x1b[?1000l", "off once");
        assert_eq!(active(), vec![Mode::AltScreen, Mode::MouseSgr]);
        // Every mode has an off switch that differs from its on switch.
        for m in [
            Mode::AltScreen,
            Mode::BracketedPaste,
            Mode::Focus,
            Mode::MouseClicks,
            Mode::MouseDrag,
            Mode::MouseMotion,
            Mode::MouseSgr,
            Mode::KittyKeyboard(1),
            Mode::HiddenCursor,
            Mode::TitleStack,
        ] {
            assert_ne!(m.on(), m.off(), "{m:?}");
        }
        // Not a terminal here: restore empties the list (writing to wherever stdout goes).
        state().on.clear();
        assert!(active().is_empty());
    }
}
