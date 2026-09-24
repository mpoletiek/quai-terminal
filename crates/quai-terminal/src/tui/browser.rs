//! Opening an explorer page in the desktop's browser.
//!
//! The TUI takes the mouse, so a terminal never sees a click on an OSC 8 link it drew: the click
//! comes to the wallet. The wallet opens the page itself, with the desktop's own opener, and only
//! for a link it built ([`super::links`]): an http(s) URL of printable ASCII, passed as one
//! argument and never through a shell. Over SSH the desktop is on another computer, so it refuses
//! and the caller copies the link instead.

use std::process::{Command, Stdio};

/// The opener this platform has, if it is installed.
fn opener() -> Option<&'static str> {
    let bin = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(bin).is_file())).then_some(bin)
}

/// Hand `url` to the desktop's browser. Returns the opener's name, or why it could not.
pub fn open(url: &str) -> Result<&'static str, String> {
    if !super::links::safe(url) {
        return Err("not a link the wallet built".into());
    }
    if super::clipboard::remote() {
        return Err("the browser is on the other end of this SSH session".into());
    }
    let bin =
        opener().ok_or_else(|| if cfg!(target_os = "macos") { "no `open`".to_string() } else { "no `xdg-open` installed".to_string() })?;
    let mut child =
        Command::new(bin).arg(url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map_err(|e| e.to_string())?;
    // Reaped off the frame loop: an opener that waits on the browser must not hold a frame.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(bin)
}
