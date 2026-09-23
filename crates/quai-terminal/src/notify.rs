//! Notifications: in-terminal OSC 9/99 (herdr's approach) and freedesktop `notify-send`.

use std::io::Write;

/// Terminal notification backend detected from the environment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TermBackend {
    Osc9,
    Osc99,
    /// foot and others: `OSC 777 ; notify ; title ; body`.
    Osc777,
}

pub fn detect_terminal() -> Option<TermBackend> {
    let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    let term = std::env::var("TERM").unwrap_or_default();
    if matches!(term_program.as_str(), "ghostty" | "iTerm.app" | "WezTerm") || term == "xterm-ghostty" || term.contains("wezterm") {
        return Some(TermBackend::Osc9);
    }
    if std::env::var_os("KITTY_WINDOW_ID").is_some() || term == "xterm-kitty" {
        return Some(TermBackend::Osc99);
    }
    if term.starts_with("foot") {
        return Some(TermBackend::Osc777);
    }
    None
}

fn clean(text: &str) -> String {
    wallet_core::explorer::clean(text, 200)
}

/// Build the escape sequence for a terminal notification.
///
/// kitty's notifications each get an id of their own: sharing one made every notice replace the
/// last, so two arriving together showed as one. `o=unfocused` has kitty show it only while its
/// window is in the background, where a notice is news rather than an echo of the screen.
pub fn sequence(backend: TermBackend, title: &str, body: &str) -> String {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    let title = clean(title);
    let body = clean(body);
    let seq = match backend {
        TermBackend::Osc9 => format!("\x1b]9;{title}: {body}\x1b\\"),
        TermBackend::Osc99 => {
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            format!("\x1b]99;i=qt{id}:d=0:o=unfocused;{title}\x1b\\\x1b]99;i=qt{id}:d=1:p=body;{body}\x1b\\")
        }
        TermBackend::Osc777 => format!("\x1b]777;notify;{};{}\x1b\\", title.replace(';', ","), body.replace(';', ",")),
    };
    if std::env::var_os("TMUX").is_some() { format!("\x1bPtmux;{}\x1b\\", seq.replace('\x1b', "\x1b\x1b")) } else { seq }
}

/// Emit a terminal notification to stdout when attached to a supporting terminal.
pub fn terminal(title: &str, body: &str) -> bool {
    match detect_terminal() {
        Some(b) if std::io::IsTerminal::is_terminal(&std::io::stdout()) => {
            let mut out = std::io::stdout();
            let _ = out.write_all(sequence(b, title, body).as_bytes());
            let _ = out.flush();
            true
        }
        _ => false,
    }
}

/// Escape the markup some notification servers render in a body (mako, dunst and GNOME all do):
/// a post saying `<b>security update</b>` or carrying a link must arrive as the text it is.
fn plain(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Desktop notification through `notify-send` when available.
pub fn desktop(title: &str, body: &str) {
    let title = clean(title);
    let body = clean(body);
    let spawned = std::process::Command::new("notify-send")
        // `--` ends the options: nothing after it is read as one, whatever it starts with.
        .args(["--app-name=Quai Terminal", "--", &title, &plain(&body)])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if spawned.is_err() {
        terminal(&title, &body);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_markup_is_shown_as_text() {
        assert_eq!(plain("<b>update</b> & <a href=x>here</a>"), "&lt;b&gt;update&lt;/b&gt; &amp; &lt;a href=x&gt;here&lt;/a&gt;");
    }

    #[test]
    fn sequences_strip_controls() {
        let s = sequence(TermBackend::Osc9, "Hi\x1b]evil", "body\x07");
        assert!(s.starts_with("\x1b]9;Hi]evil: body"));
        let k = sequence(TermBackend::Osc99, "T", "B");
        assert!(k.contains("p=body;B"));
    }
}
