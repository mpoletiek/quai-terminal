//! Timing trace for slow-path diagnosis. `QW_TIMING_LOG=<file>` appends one line per measured
//! step: unix ms at the end, thread name, label, milliseconds taken.
//!
//! Two kinds of line share that shape. [`timing`] measures one step and is written every time the
//! step runs. [`mark`] records a milestone against the moment the process started and is written
//! **once per label**, which is what makes `startup.*` a launch measurement rather than a running
//! average: the second refresh of a session must not overwrite what the first one cost.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

fn log() -> Option<&'static Mutex<std::fs::File>> {
    static LOG: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();
    LOG.get_or_init(|| {
        std::env::var_os("QW_TIMING_LOG").and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok()).map(Mutex::new)
    })
    .as_ref()
}

fn write(label: &str, millis: u128) {
    let Some(log) = log() else { return };
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let thread = std::thread::current().name().unwrap_or("?").to_string();
    if let Ok(mut f) = log.lock() {
        use std::io::Write;
        let _ = writeln!(f, "{ms} {thread} {label} {millis}");
    }
}

/// Record that `label` took since `started`. Does nothing unless `QW_TIMING_LOG` is set.
pub fn timing(label: &str, started: Instant) {
    if log().is_none() {
        return;
    }
    write(label, started.elapsed().as_millis());
}

/// Avoid computing optional diagnostic payloads during ordinary rendering.
pub fn enabled() -> bool {
    log().is_some()
}

/// Record a plain number against a label, in the same shape as a timing line.
///
/// For the things worth tracing that are not durations — how old a cached value was, how many of
/// something there were. Written every time, like [`timing`] and unlike [`mark`].
pub fn count(label: &str, value: u64) {
    if log().is_none() {
        return;
    }
    write(label, u128::from(value));
}

/// When this process started, as the `startup.*` marks count it.
///
/// Called first from `main` before any work, so every later caller gets that same instant. A
/// caller that beats `main` to it would only shift the origin later, which flatters the numbers,
/// so the call in `main` is the one that matters.
pub fn started() -> Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

/// Record a launch milestone: milliseconds from [`started`] to now, written the first time this
/// label is marked and ignored afterwards.
pub fn mark(label: &str) {
    if log().is_none() {
        return;
    }
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    let first = seen.lock().map(|mut s| s.insert(label.to_string())).unwrap_or(false);
    if first {
        write(label, started().elapsed().as_millis());
    }
}
