//! Signals that end or pause the wallet from outside.
//!
//! SIGTERM, SIGHUP (the window closed) and SIGINT ask the loop to stop, and it leaves the way a
//! quit does: keys dropped, worker shut down, terminal restored. The loop is woken so it acts at
//! once instead of at its next timeout. SIGCONT after an outside stop (a `kill -STOP`, a shell
//! job control) means the screen may be anything now, so the loop redraws it whole.

use std::sync::atomic::{AtomicBool, Ordering};

static STOP: AtomicBool = AtomicBool::new(false);
static CONTINUED: AtomicBool = AtomicBool::new(false);

/// Start listening, on a thread of its own. `wake` interrupts the loop's wait for input.
pub fn listen(wake: impl Fn() + Send + 'static) {
    use signal_hook::consts::{SIGCONT, SIGHUP, SIGINT, SIGTERM};
    let Ok(mut signals) = signal_hook::iterator::Signals::new([SIGTERM, SIGHUP, SIGINT, SIGCONT]) else { return };
    let _ = std::thread::Builder::new().name("signals".into()).spawn(move || {
        for signal in signals.forever() {
            match signal {
                SIGCONT => CONTINUED.store(true, Ordering::SeqCst),
                _ => STOP.store(true, Ordering::SeqCst),
            }
            wake();
        }
    });
}

/// Whether a signal asked the wallet to stop.
pub fn stop_requested() -> bool {
    STOP.load(Ordering::SeqCst)
}

/// Whether the process was continued since the last call (and the screen needs a full redraw).
pub fn take_continued() -> bool {
    CONTINUED.swap(false, Ordering::SeqCst)
}

/// Stop the process here, as Ctrl-Z would in a shell: the caller has already given the terminal
/// back, and takes it again when this returns (the shell's `fg`).
pub fn suspend_self() {
    #[cfg(unix)]
    {
        let _ = signal_hook::low_level::raise(signal_hook::consts::SIGTSTP);
    }
    CONTINUED.store(false, Ordering::SeqCst);
}
