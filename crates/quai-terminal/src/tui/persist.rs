//! The persistence lane: preferences are written off the UI thread.
//!
//! Saving `config.toml` is an atomic write with two fsyncs, 15 ms on this machine, and it happened
//! on keypresses (`$`, a settings toggle). Now the UI thread only serializes (microseconds) and
//! hands the text to this lane, which writes the newest version it has and drops any older one
//! still waiting. Errors come back to be shown; quitting waits for the lane, so nothing is lost.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

enum Job {
    Write(PathBuf, String),
    Flush(mpsc::Sender<()>),
}

pub struct Lane {
    tx: mpsc::Sender<Job>,
    errors: mpsc::Receiver<String>,
}

impl Lane {
    pub fn start() -> Self {
        let (tx, rx) = mpsc::channel::<Job>();
        let (err_tx, errors) = mpsc::channel();
        std::thread::Builder::new()
            .name("persist".into())
            .spawn(move || {
                while let Ok(first) = rx.recv() {
                    // Everything already waiting: only the newest write of each file matters.
                    let mut jobs = vec![first];
                    jobs.extend(rx.try_iter());
                    let mut latest: Vec<(PathBuf, String)> = Vec::new();
                    let mut flushes = Vec::new();
                    for job in jobs {
                        match job {
                            Job::Write(path, text) => {
                                latest.retain(|(p, _)| *p != path);
                                latest.push((path, text));
                            }
                            Job::Flush(ack) => flushes.push(ack),
                        }
                    }
                    for (path, text) in latest {
                        if let Err(e) = wallet_vault::write_private_atomic(&path, text.as_bytes()) {
                            let _ = err_tx.send(format!("could not save preferences: {e}"));
                            super::term::wake();
                        }
                    }
                    for ack in flushes {
                        let _ = ack.send(());
                    }
                }
            })
            .expect("persistence thread");
        Lane { tx, errors }
    }

    /// Queue a write of `text` to `path`.
    pub fn write(&self, path: PathBuf, text: String) {
        let _ = self.tx.send(Job::Write(path, text));
    }

    /// Wait (up to two seconds) until everything queued so far is on disk.
    pub fn flush(&self) {
        let (ack, done) = mpsc::channel();
        if self.tx.send(Job::Flush(ack)).is_ok() {
            let _ = done.recv_timeout(Duration::from_secs(2));
        }
    }

    /// A write that failed since the last look.
    pub fn error(&self) -> Option<String> {
        self.errors.try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes land, the newest wins, and flush waits for them.
    #[test]
    fn the_newest_write_lands_and_flush_waits_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let lane = Lane::start();
        for i in 0..50 {
            lane.write(path.clone(), format!("n = {i}\n"));
        }
        lane.flush();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "n = 49\n");
        assert!(lane.error().is_none());
    }
}
