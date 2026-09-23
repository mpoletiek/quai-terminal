//! Copying, honestly.
//!
//! A copied address is about to be pasted somewhere money goes. OSC 52 alone can't promise that:
//! Terminal.app ignores it, iTerm2 and tmux drop it unless configured, and the clipboard then
//! still holds whatever was there before, which could be another address. So on this machine the
//! desktop's own tool is used first (`wl-copy`, `xclip`, `xsel`, `pbcopy`) and read back; OSC 52 is
//! for SSH, where the terminal is on another computer, and the toast says it can't be checked.
//!
//! Only [`PublicText`] can be copied, and it has no way in from a secret: a recovery phrase or a
//! key can't reach the clipboard by any path this module offers.

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::mpsc;

/// Text that is safe to put on the clipboard: an address, a hash, a link, a shell command, or a
/// value already shown on screen. Constructed only from those; never from a `Zeroizing` secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicText(String);

impl PublicText {
    /// An address or payment code.
    pub fn address(s: impl Into<String>) -> Self {
        PublicText(s.into())
    }
    /// A link the wallet built (explorer, venue).
    pub fn link(s: impl Into<String>) -> Self {
        PublicText(s.into())
    }
    /// A `quai-terminal …` command line.
    pub fn command(s: impl Into<String>) -> Self {
        PublicText(s.into())
    }
    /// A value that is already on screen (a hash, an id, an amount).
    pub fn shown(s: impl Into<String>) -> Self {
        PublicText(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A copy the frame loop should carry out, and how to describe it.
#[derive(Clone, Debug)]
pub struct CopyRequest {
    pub text: PublicText,
    /// What it is, for the toast ("address", "command", "link").
    pub what: &'static str,
}

/// How a copy went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// On the clipboard, and read back to match.
    Verified,
    /// Handed to a tool that took it without complaint, with nothing to read it back with.
    Handed(&'static str),
    /// Sent to the terminal (OSC 52); whether it landed can't be known from here.
    Terminal,
    Failed(String),
}

/// The toast for an outcome: what happened, and the copied value, so it can be checked by eye.
pub fn describe(req: &CopyRequest, outcome: &Outcome) -> (String, bool) {
    let shown = shorten(req.text.as_str());
    match outcome {
        Outcome::Verified => (format!("copied {} · {shown}", req.what), false),
        Outcome::Handed(tool) => (format!("copied {} with {tool} · {shown}", req.what), false),
        Outcome::Terminal => (format!("sent {} to the terminal's clipboard · paste to check: {shown}", req.what), false),
        Outcome::Failed(why) => (format!("couldn't copy the {}: {why}", req.what), true),
    }
}

fn shorten(s: &str) -> String {
    if s.starts_with("0x") && s.len() > 20 && !s.contains(' ') {
        wallet_core::session::short_address(s)
    } else if s.chars().count() > 56 {
        format!("{}…", s.chars().take(55).collect::<String>())
    } else {
        s.to_string()
    }
}

/// Whether the terminal is on another computer, so a local tool would copy to the wrong desk.
pub fn remote() -> bool {
    ["SSH_CONNECTION", "SSH_TTY", "SSH_CLIENT"].iter().any(|v| std::env::var_os(v).is_some())
}

/// A desktop clipboard tool: the command that copies, and the one that reads back.
struct Tool {
    name: &'static str,
    copy: &'static [&'static str],
    paste: Option<&'static [&'static str]>,
}

/// Whether `bin` is on the PATH (checked before choosing a route, so a machine with no tool falls
/// back to the terminal instead of reporting a failure).
fn installed(bin: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(bin).is_file()))
}

fn tools() -> Vec<Tool> {
    let mut out = Vec::new();
    if cfg!(target_os = "macos") {
        out.push(Tool { name: "pbcopy", copy: &["pbcopy"], paste: Some(&["pbpaste"]) });
    }
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        out.push(Tool { name: "wl-copy", copy: &["wl-copy"], paste: Some(&["wl-paste", "--no-newline"]) });
    }
    if std::env::var_os("DISPLAY").is_some() {
        out.push(Tool {
            name: "xclip",
            copy: &["xclip", "-selection", "clipboard"],
            paste: Some(&["xclip", "-o", "-selection", "clipboard"]),
        });
        out.push(Tool { name: "xsel", copy: &["xsel", "--clipboard", "--input"], paste: Some(&["xsel", "--clipboard", "--output"]) });
    }
    out.retain(|t| installed(t.copy[0]));
    out
}

fn run_copy(tool: &Tool, text: &str) -> Result<(), String> {
    let (bin, args) = tool.copy.split_first().expect("a command");
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    child.stdin.take().ok_or("no stdin")?.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    let status = child.wait().map_err(|e| e.to_string())?;
    if status.success() { Ok(()) } else { Err(format!("{} exited with {status}", tool.name)) }
}

fn read_back(cmd: &[&str]) -> Option<String> {
    let (bin, args) = cmd.split_first()?;
    let out = Command::new(bin).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Copy with the first local tool that works. `None` when none of them ran at all.
fn local(tools: Vec<Tool>, text: &str) -> Option<Outcome> {
    let mut last_error = None;
    for tool in tools {
        match run_copy(&tool, text) {
            Ok(()) => {
                return Some(match tool.paste.and_then(read_back) {
                    Some(back) if back.trim_end_matches('\n') == text => Outcome::Verified,
                    Some(_) => Outcome::Failed(format!("{} took it but the clipboard reads back different", tool.name)),
                    None => Outcome::Handed(tool.name),
                });
            }
            // Not installed: try the next one quietly. Installed but failed: remember why.
            Err(e) if e.contains("No such file") || e.contains("not found") => {}
            Err(e) => last_error = Some(e),
        }
    }
    last_error.map(Outcome::Failed)
}

/// The OSC 52 sequence for `text`, wrapped for tmux when inside it.
pub fn osc52(text: &str, tmux: bool) -> String {
    use base64::Engine;
    let seq = format!("\x1b]52;c;{}\x07", base64::engine::general_purpose::STANDARD.encode(text.as_bytes()));
    if tmux { format!("\x1bPtmux;{}\x1b\\", seq.replace('\x1b', "\x1b\x1b")) } else { seq }
}

/// Carry out a copy off the UI thread (a tool and its read-back take a few milliseconds each).
/// When the terminal is remote, or this machine has no clipboard tool (a Linux console), the
/// returned sequence must be written to the terminal by the caller, and the outcome is
/// [`Outcome::Terminal`].
pub fn start(req: CopyRequest, tmux: bool) -> (Option<String>, mpsc::Receiver<(CopyRequest, Outcome)>) {
    let (tx, rx) = mpsc::channel();
    let tools = tools();
    if remote() || tools.is_empty() {
        let seq = osc52(req.text.as_str(), tmux);
        let _ = tx.send((req, Outcome::Terminal));
        return (Some(seq), rx);
    }
    let text = req.text.as_str().to_string();
    let spawned = std::thread::Builder::new().name("clipboard".into()).spawn({
        let tx = tx.clone();
        let req = req.clone();
        move || {
            let outcome = local(tools, &text).unwrap_or(Outcome::Failed("the clipboard tool did not run".into()));
            let _ = tx.send((req, outcome));
        }
    });
    if spawned.is_err() {
        let _ = tx.send((req, Outcome::Failed("could not start the copy".into())));
    }
    (None, rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toasts_say_what_happened_and_show_the_value() {
        let req = CopyRequest { text: PublicText::address("0x004dd9afaa2768642b5cde15c24f37bf19d842e4"), what: "address" };
        let (ok, err) = describe(&req, &Outcome::Verified);
        assert!(!err && ok.starts_with("copied address") && ok.contains("0x004d"), "{ok}");
        let (term, _) = describe(&req, &Outcome::Terminal);
        assert!(term.contains("paste to check"), "{term}");
        let (fail, err) = describe(&req, &Outcome::Failed("nope".into()));
        assert!(err && fail.contains("couldn't copy"), "{fail}");
    }

    #[test]
    fn osc52_is_wrapped_for_tmux() {
        assert!(osc52("x", false).starts_with("\x1b]52;c;"));
        let wrapped = osc52("x", true);
        assert!(wrapped.starts_with("\x1bPtmux;") && wrapped.ends_with("\x1b\\"));
    }
}
