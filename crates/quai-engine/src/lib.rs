//! Quai Terminal's engine: the wallet worker and its lanes (signing, tracking, Qi, messaging),
//! the data worker, and limit orders. It owns the wallet's sessions; a host — the TUI today, the
//! daemon next — sends it commands and draws what it says.
//!
//! See docs/ARCHITECTURE_REVIEW_2026-09-24.md §5.

pub mod client;
pub mod data;
pub mod host;
pub mod messaging;
pub mod orders;
pub mod plans;
pub mod protocol;
pub mod resource;
pub mod server;
pub mod worker;

use std::sync::OnceLock;

static WAKER: OnceLock<fn()> = OnceLock::new();

/// How the engine tells its host that events are waiting (the TUI's loop sleeps between frames).
/// Set once, before the engine starts; without one, events wait to be polled.
pub fn set_waker(wake: fn()) {
    let _ = WAKER.set(wake);
}

/// Wake the host.
pub(crate) fn wake() {
    if let Some(wake) = WAKER.get() {
        wake();
    }
}
