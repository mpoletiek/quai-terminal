//! The clock.

/// Current unix time in seconds.
pub fn now() -> u64 {
    if let Some(at) = FROZEN.with(std::cell::Cell::get) {
        return at;
    }
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

thread_local! {
    static FROZEN: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Hold [`now`] at `at` on this thread only (`None` lets it run again). For snapshot tests,
/// whose screens are dated from now: other threads, and so other tests, keep the real clock.
#[doc(hidden)]
pub fn freeze_clock(at: Option<u64>) {
    FROZEN.with(|f| f.set(at));
}
