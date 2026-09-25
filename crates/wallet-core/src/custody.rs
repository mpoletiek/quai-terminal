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

fn registry() -> &'static Mutex<HashMap<String, Weak<Custody>>> {
    static REGISTRY: std::sync::OnceLock<Mutex<HashMap<String, Weak<Custody>>>> = std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The custody of `wallet` in this process: the one every session of it shares.
pub fn for_wallet(wallet: &str) -> Arc<Custody> {
    let mut map = registry().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = map.get(wallet).and_then(Weak::upgrade) {
        return existing;
    }
    map.retain(|_, weak| weak.strong_count() > 0);
    let custody =
        Arc::new(Custody { wallet: wallet.to_string(), slot: RwLock::new(None), unlocked_at: std::sync::atomic::AtomicU64::new(0) });
    map.insert(wallet.to_string(), Arc::downgrade(&custody));
    custody
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
}
