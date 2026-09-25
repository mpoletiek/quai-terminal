//! One set of keys per wallet in a process (docs/ARCHITECTURE_REVIEW_2026-09-24.md, phase 4).
//!
//! A test binary of its own, with one test, so nothing else in the process unlocks a wallet
//! while the live-key count is being read.

use wallet_core::identity::live_unlocked;
use wallet_core::session::Session;

const PHRASE: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

#[test]
fn every_session_of_a_wallet_shares_one_set_of_keys_and_one_lock() {
    let dir = tempfile::tempdir().unwrap();
    let paths = wallet_core::paths::Paths::resolve(Some(dir.path().to_path_buf())).unwrap();
    let registry = wallet_core::registry::Registry::new(paths);
    let meta = registry.create_hd("main", PHRASE, "english", "", "password123", true).unwrap();
    let other = registry
        .create_hd(
            "other",
            "legal winner thank year wave sausage worth useful legal winner thank yellow",
            "english",
            "",
            "password123",
            true,
        )
        .unwrap();
    let networks = wallet_core::network::NetworkProfile::builtins();
    let config = wallet_core::config::AppConfig::default();
    let open = |meta: &wallet_core::registry::WalletMeta, n: usize| {
        Session::open(registry.clone(), config.clone(), meta.clone(), networks[n % networks.len()].clone()).unwrap()
    };
    // The worker, the signing lane, the Qi lane and the tracker — and one on another network,
    // as after a network switch.
    let mut worker = open(&meta, 0);
    let lanes = [open(&meta, 0), open(&meta, 0), open(&meta, 0), open(&meta, 1)];
    let stranger = open(&other, 0);
    assert_eq!(live_unlocked(), 0);
    worker.unlock("password123").unwrap();
    assert_eq!(live_unlocked(), 1, "one unlock, one set of keys");
    for lane in &lanes {
        assert!(lane.is_unlocked(), "every session of the wallet signs with the same keys");
    }
    assert!(!stranger.is_unlocked(), "another wallet's session never sees them");
    // A network switch needs no password: the keys belong to the wallet.
    assert!(lanes[3].unlocked_keys().is_some());
    assert_eq!(live_unlocked(), 1, "and handing them out does not copy them");
    // A new account is a public change: the keys stay, still one set.
    worker.add_account(Some("second")).unwrap();
    assert_eq!(live_unlocked(), 1);
    // Locking any one session locks them all, and the keys are gone from memory.
    let mut tracker = lanes.into_iter().nth(3).unwrap();
    tracker.lock();
    assert!(!worker.is_unlocked());
    assert_eq!(live_unlocked(), 0, "nothing left to chase through the lanes");
}

/// A wallet's sessions open together (the worker and its lanes, the daemon's watcher), and the
/// first open of a new wallet registers its addresses: whoever loses that race re-reads, never
/// fails. It used to fail about half the time as "stale or mismatched wallet snapshot".
#[test]
fn many_sessions_open_a_fresh_wallet_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let paths = wallet_core::paths::Paths::resolve(Some(dir.path().to_path_buf())).unwrap();
    let registry = wallet_core::registry::Registry::new(paths);
    let meta = registry.create_hd("fresh", PHRASE, "english", "", "password123", true).unwrap();
    let network = wallet_core::network::NetworkProfile::builtins().remove(0);
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let (registry, meta, network) = (registry.clone(), meta.clone(), network.clone());
            std::thread::spawn(move || {
                Session::open(registry, wallet_core::config::AppConfig::default(), meta, network).map(|_| ()).map_err(|e| e.to_string())
            })
        })
        .collect();
    let errors: Vec<String> = handles.into_iter().filter_map(|h| h.join().unwrap().err()).collect();
    assert!(errors.is_empty(), "{errors:?}");
}
