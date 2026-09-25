//! A host for one client's engine: the worker and its lanes for one wallet, the unlock, and the
//! engine's own auto-lock. The daemon runs one per connected client ([`crate::server`]); a
//! standalone TUI runs one in its own process. Either way the client says the same things
//! ([`crate::protocol::ClientMsg`]) and hears the same events.
//!
//! Keys live where the host runs, in the host's custody scope: a client's sessions share them
//! among themselves and with nobody else ([`wallet_core::custody::for_wallet_in`]). They reach
//! the daemon's own watcher only when the user shares unlocks with it (`daemon_share_unlock`),
//! and then as the same keys, not a copy.

use crate::protocol::ClientMsg;
use crate::worker::{Cmd, Ev, Worker};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wallet_core::config::AppConfig;
use wallet_core::registry::{Registry, WalletMeta};
use zeroize::Zeroizing;

/// How long past the configured auto-lock the engine waits before locking on its own. The
/// client locks first and shows it; the engine's lock is for a client that stopped (hung, or
/// its terminal was lost) with the wallet unlocked. Activity arrives at most every few seconds.
pub const AUTO_LOCK_GRACE: Duration = Duration::from_secs(30);

/// How often the engine checks for that.
const WATCH_EVERY: Duration = Duration::from_secs(5);

/// One client's engine.
pub struct Host {
    worker: Worker,
    /// The unscoped registry: vaults are opened through it, and its custody scope is the host
    /// process's own.
    registry: Registry,
    scope: String,
    /// Give unlocked keys to the host process's own scope too (the daemon's watcher), when the
    /// user shares unlocks with it.
    share: bool,
    state: Arc<Shared>,
}

/// What the auto-lock watch shares with the host.
struct Shared {
    wallet: Mutex<String>,
    last_activity: Mutex<Instant>,
    /// Wallets whose keys this host gave the process's own scope: locking here locks them there.
    shared: Mutex<Vec<String>>,
    stop: AtomicBool,
}

impl Host {
    /// Start an engine for `meta` on `network`, its keys held in `scope` (empty: the process's
    /// own). `share` gives the host process's own scope the same keys at each unlock.
    pub fn attach(
        registry: Registry,
        config: AppConfig,
        scope: &str,
        share: bool,
        meta: WalletMeta,
        network: String,
        wake: impl Fn() + Send + 'static,
    ) -> std::io::Result<Host> {
        let wallet = meta.id.clone();
        let worker = Worker::spawn(registry.scoped(scope), config, meta, network, wake)?;
        let host = Host::over(worker, registry, scope, share);
        *host.state.wallet.lock().unwrap_or_else(|e| e.into_inner()) = wallet;
        host.watch();
        Ok(host)
    }

    /// A host over a worker already running (tests hand it a capturing one).
    pub fn over(worker: Worker, registry: Registry, scope: &str, share: bool) -> Host {
        let state = Arc::new(Shared {
            wallet: Mutex::new(String::new()),
            last_activity: Mutex::new(Instant::now()),
            shared: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
        });
        Host { worker, registry: registry.scoped(""), scope: scope.to_string(), share, state }
    }

    /// The worker's events, for a host that forwards them from another thread.
    pub fn take_events(&mut self) -> std::sync::mpsc::Receiver<Ev> {
        self.worker.take_events()
    }

    /// The next event, if one is waiting.
    pub fn try_recv(&self) -> Option<Ev> {
        self.worker.rx.try_recv().ok()
    }

    /// Whatever a client said, after the handshake and the attach.
    pub fn handle(&self, msg: ClientMsg) {
        match msg {
            ClientMsg::Cmd(cmd) => self.send(cmd),
            ClientMsg::Unlock { wallet, password } => self.unlock(wallet, password),
            ClientMsg::Activity => self.touch(),
            // Only the server reads these: the handshake before it builds a host, and data
            // requests, which go to the client's data worker.
            ClientMsg::Hello { .. } | ClientMsg::Attach { .. } | ClientMsg::Data(_) => {}
        }
    }

    /// A command for the engine.
    pub fn send(&self, cmd: Cmd) {
        match &cmd {
            // Host-only commands never come from a client: the protocol cannot carry them, and a
            // client in this process has no business sending them either.
            Cmd::UseKeys { .. } | Cmd::QiSynced(_) | Cmd::Committed | Cmd::Journal => return,
            Cmd::SwitchWallet(wallet) => {
                *self.state.wallet.lock().unwrap_or_else(|e| e.into_inner()) = wallet.clone();
            }
            Cmd::Lock => self.lock_shared(),
            _ => {}
        }
        self.worker.send(cmd);
    }

    /// Someone is at the keyboard.
    pub fn touch(&self) {
        *self.state.last_activity.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
    }

    /// Check a password off this thread and hold the keys: [`Ev::Unlocked`] as soon as they are
    /// in this client's custody, so an unlock never waits behind whatever the worker is doing.
    /// The worker hears too, in turn; if it has since opened another wallet (the switch that
    /// opened this one failed) it says [`Ev::KeysRefused`] and the screen locks again. A wrong
    /// password says [`Ev::UnlockFailed`].
    pub fn unlock(&self, wallet: String, password: Zeroizing<String>) {
        self.touch();
        let registry = self.registry.clone();
        let tx = self.worker.tx.clone();
        let events = self.worker.events();
        let share = self.share;
        let scope = self.scope.clone();
        let state = self.state.clone();
        let spawned = std::thread::Builder::new().name("wallet-unlock".into()).spawn(move || {
            let checked = registry.load(&wallet).and_then(|meta| registry.unlock(&meta, &password));
            drop(password);
            match checked {
                Ok(keys) => {
                    let keys = Arc::new(keys);
                    // As the user has it now: the setting can change while a terminal is open.
                    let share = share && AppConfig::load(registry.paths()).is_ok_and(|c| c.daemon_share_unlock);
                    if share && let Some(own) = wallet_core::custody::existing("", &wallet) {
                        own.install_shared(keys.clone());
                        let mut shared = state.shared.lock().unwrap_or_else(|e| e.into_inner());
                        if !shared.contains(&wallet) {
                            shared.push(wallet.clone());
                        }
                    }
                    // Into the custody the lanes already hold, so a review can sign at once; and to
                    // the worker, queued behind any switch, for the session it opens.
                    if let Some(custody) = wallet_core::custody::existing(&scope, &wallet) {
                        custody.install_shared(keys.clone());
                    }
                    let _ = tx.send(Cmd::UseKeys { wallet, keys });
                    // The idle time counts from here: checking the password can take a while.
                    *state.last_activity.lock().unwrap_or_else(|e| e.into_inner()) = std::time::Instant::now();
                    let _ = events.send(Ev::Unlocked);
                    crate::wake();
                }
                Err(e) => {
                    let _ = events.send(Ev::UnlockFailed(e.to_string()));
                    crate::wake();
                }
            }
        });
        if let Err(e) = spawned {
            self.worker.say(Ev::UnlockFailed(format!("could not start unlocking: {e}")));
        }
    }

    /// The keys this host gave the process's own scope go when the client locks.
    fn lock_shared(&self) {
        let shared = std::mem::take(&mut *self.state.shared.lock().unwrap_or_else(|e| e.into_inner()));
        for wallet in shared {
            if let Some(own) = wallet_core::custody::existing("", &wallet) {
                own.clear();
            }
        }
    }

    /// Lock this client's keys, for a client that went away. Keys shared with the daemon's own
    /// scope stay there, as the user asked, until the daemon's own lock.
    pub fn lock_now_keeping_shared(&self) {
        self.worker.send(Cmd::Lock);
    }

    /// Whether this client's current wallet is unlocked here.
    pub fn is_unlocked(&self) -> bool {
        let wallet = self.state.wallet.lock().unwrap_or_else(|e| e.into_inner()).clone();
        wallet_core::custody::existing(&self.scope, &wallet).is_some_and(|c| c.is_unlocked())
    }

    /// The engine's own auto-lock: past the configured minutes (and a grace) without activity,
    /// the keys go and the client hears [`Ev::Locked`], whatever the client is doing.
    fn watch(&self) {
        let state = self.state.clone();
        let scope = self.scope.clone();
        let paths = self.registry.paths().clone();
        let lock = self.worker.locker();
        let every = if test_seconds().is_some() { Duration::from_millis(200) } else { WATCH_EVERY };
        let _ = std::thread::Builder::new().name("engine-autolock".into()).spawn(move || {
            while !state.stop.load(Ordering::SeqCst) {
                std::thread::sleep(every);
                let minutes = AppConfig::load(&paths).map_or(0, |c| c.auto_lock_minutes);
                if minutes == 0 {
                    continue;
                }
                let idle = state.last_activity.lock().unwrap_or_else(|e| e.into_inner()).elapsed();
                if idle < auto_lock_after(minutes) {
                    continue;
                }
                let wallet = state.wallet.lock().unwrap_or_else(|e| e.into_inner()).clone();
                if !wallet_core::custody::existing(&scope, &wallet).is_some_and(|c| c.is_unlocked()) {
                    continue;
                }
                wallet_core::diag::mark("engine.autolock");
                for wallet in std::mem::take(&mut *state.shared.lock().unwrap_or_else(|e| e.into_inner())) {
                    if let Some(own) = wallet_core::custody::existing("", &wallet) {
                        own.clear();
                    }
                }
                if !lock() {
                    return;
                }
            }
        });
    }
}

/// Idle time after which the engine locks on its own.
fn auto_lock_after(minutes: u32) -> Duration {
    test_seconds().map_or(Duration::from_secs(u64::from(minutes) * 60) + AUTO_LOCK_GRACE, Duration::from_secs)
}

/// A debug build's tests may make the engine lock sooner (never later): seconds instead of
/// minutes, so the lock can be watched happen.
fn test_seconds() -> Option<u64> {
    if !cfg!(debug_assertions) {
        return None;
    }
    std::env::var("QUAI_TERMINAL_TEST_AUTOLOCK_SECS").ok()?.parse().ok()
}

impl Drop for Host {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::SeqCst);
        self.worker.send(Cmd::Shutdown);
    }
}
