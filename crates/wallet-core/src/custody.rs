//! Custody: the one place an unlocked wallet's keys live in this process.
//!
//! Every [`crate::session::Session`] of a wallet in this process — the worker, the signing lane,
//! the Qi lane, the tracker, the messaging lane — holds the same [`Custody`], so the keys exist
//! once. An operation that needs them takes a counted reference ([`Custody::get`]) for as long as
//! it signs, and lets it go. Locking empties the slot for every session at once: nothing is left
//! to chase through the lanes, and no copy outlives the lock except inside an operation that
//! was already signing when it came.
//!
//! The vault password is never kept here. A network switch keeps the keys (they belong to the
//! wallet, not the network), so nothing needs to ask for it again.

use crate::identity::Unlocked;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock, Weak};

/// One wallet's keys, shared by every session of that wallet in this process.
#[derive(Debug)]
pub struct Custody {
    wallet: String,
    slot: RwLock<Option<Arc<Unlocked>>>,
    /// Unix time of the last unlock.
    unlocked_at: std::sync::atomic::AtomicU64,
}

type Scoped = HashMap<(String, String), Weak<Custody>>;

fn registry() -> &'static Mutex<Scoped> {
    static REGISTRY: std::sync::OnceLock<Mutex<Scoped>> = std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The custody of `wallet` in this process: the one every session of it shares.
pub fn for_wallet(wallet: &str) -> Arc<Custody> {
    for_wallet_in("", wallet)
}

/// The custody of `wallet` in one scope. The daemon hosts an engine for each client that
/// attaches; each client's sessions share keys among themselves and with nobody else, so a
/// process that connects without the password can never sign with keys another one unlocked.
/// The empty scope is this process's own (the CLI, the daemon's watcher, a standalone TUI).
pub fn for_wallet_in(scope: &str, wallet: &str) -> Arc<Custody> {
    let mut map = registry().lock().unwrap_or_else(|e| e.into_inner());
    let key = (scope.to_string(), wallet.to_string());
    if let Some(existing) = map.get(&key).and_then(Weak::upgrade) {
        return existing;
    }
    map.retain(|_, weak| weak.strong_count() > 0);
    let custody =
        Arc::new(Custody { wallet: wallet.to_string(), slot: RwLock::new(None), unlocked_at: std::sync::atomic::AtomicU64::new(0) });
    map.insert(key, Arc::downgrade(&custody));
    custody
}

/// The custody of `wallet` in `scope`, only if some session holds it open.
pub fn existing(scope: &str, wallet: &str) -> Option<Arc<Custody>> {
    let map = registry().lock().unwrap_or_else(|e| e.into_inner());
    map.get(&(scope.to_string(), wallet.to_string())).and_then(Weak::upgrade)
}

impl Custody {
    /// The wallet these keys belong to.
    pub fn wallet(&self) -> &str {
        &self.wallet
    }

    /// Hold `unlocked` as this wallet's keys, replacing any before.
    pub fn install(&self, unlocked: Unlocked) {
        let mut slot = self.slot.write().unwrap_or_else(|e| e.into_inner());
        *slot = Some(Arc::new(unlocked));
        self.unlocked_at.store(crate::registry::now(), std::sync::atomic::Ordering::Relaxed);
    }

    /// Hold keys another scope holds too: the same keys, not a copy. Each scope's lock lets go of
    /// its reference; the keys leave memory once no scope holds them.
    pub fn install_shared(&self, unlocked: Arc<Unlocked>) {
        let mut slot = self.slot.write().unwrap_or_else(|e| e.into_inner());
        *slot = Some(unlocked);
        self.unlocked_at.store(crate::registry::now(), std::sync::atomic::Ordering::Relaxed);
    }

    /// Drop the keys, for every session of this wallet.
    pub fn clear(&self) {
        let mut slot = self.slot.write().unwrap_or_else(|e| e.into_inner());
        *slot = None;
    }

    /// The keys, counted, for as long as an operation needs them.
    pub fn get(&self) -> Option<Arc<Unlocked>> {
        self.slot.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Whether keys are held.
    pub fn is_unlocked(&self) -> bool {
        self.slot.read().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    /// Unix time of the last unlock (0: never).
    pub fn unlocked_at(&self) -> u64 {
        self.unlocked_at.load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_wallet_one_custody_and_unknown_wallets_are_apart() {
        let a = for_wallet("custody-test-a");
        let again = for_wallet("custody-test-a");
        let b = for_wallet("custody-test-b");
        assert!(Arc::ptr_eq(&a, &again));
        assert!(!Arc::ptr_eq(&a, &b));
        assert_eq!(a.wallet(), "custody-test-a");
        // Once every holder is gone, a later session starts a fresh (locked) custody.
        drop((a, again));
        assert!(!for_wallet("custody-test-a").is_unlocked());
    }

    #[test]
    fn scopes_are_apart_and_shared_keys_live_until_the_last_scope_lets_go() {
        let own = for_wallet("custody-test-c");
        let client = for_wallet_in("engine:1", "custody-test-c");
        let other = for_wallet_in("engine:2", "custody-test-c");
        assert!(!Arc::ptr_eq(&own, &client));
        assert!(Arc::ptr_eq(&client, &existing("engine:1", "custody-test-c").unwrap()));
        assert!(existing("engine:9", "custody-test-c").is_none());
        let keys = Arc::new(Unlocked::new(wallet_vault::Secrets { mnemonic: None, imported: Vec::new() }).unwrap());
        client.install_shared(keys.clone());
        own.install_shared(keys.clone());
        assert!(client.is_unlocked() && own.is_unlocked());
        assert!(!other.is_unlocked(), "another client never sees them");
        client.clear();
        assert!(own.is_unlocked(), "a client's lock leaves the daemon's reference");
        own.clear();
        assert_eq!(Arc::strong_count(&keys), 1, "no scope holds them any more");
    }
}
