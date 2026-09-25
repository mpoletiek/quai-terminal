//! The daemon hosts the engine (docs/ARCHITECTURE_REVIEW_2026-09-24.md, phase 5), tested against
//! a real daemon process: this test process is the terminal.
//!
//! - an unlock checks the password in the daemon, and the terminal's process never holds keys;
//! - a second terminal attached to the same wallet is not unlocked by the first one's password;
//! - killing the daemon mid-session locks the terminal at once, and says why.
//!
//! The daemon runs on a scratch home and a scratch runtime directory, against a network that
//! does not answer: nothing here touches the user's own daemon or any node.

use quai_engine::client::{Dialer, Engine, Remote};
use quai_engine::worker::{Cmd, Ev};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

const PHRASE: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_quai-terminal")
}

struct Daemon {
    child: std::process::Child,
    runtime: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn cli(home: &Path, runtime: &Path, args: &[&str]) {
    let out = std::process::Command::new(bin())
        .args(args)
        .env("QUAI_TERMINAL_HOME", home)
        .env("XDG_RUNTIME_DIR", runtime)
        .env("QUAI_TERMINAL_NO_DAEMON", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// A file in the daemon's runtime directory, by prefix and suffix (its name carries a hash of the home).
fn runtime_file(runtime: &Path, prefix: &str, suffix: &str) -> Option<PathBuf> {
    std::fs::read_dir(runtime.join("quai-terminal"))
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(prefix) && n.ends_with(suffix)))
}

fn daemon_pid(runtime: &Path) -> Option<u32> {
    let state = std::fs::read_to_string(runtime_file(runtime, "daemon-", ".json")?).ok()?;
    serde_json::from_str::<serde_json::Value>(&state).ok()?["pid"].as_u64().map(|p| p as u32)
}

fn start_daemon(home: &Path, runtime: &Path) -> Daemon {
    start_daemon_with(home, runtime, &[])
}

fn start_daemon_with(home: &Path, runtime: &Path, env: &[(&str, &str)]) -> Daemon {
    start_daemon_on(home, runtime, "offline", env)
}

fn start_daemon_on(home: &Path, runtime: &Path, network: &str, env: &[(&str, &str)]) -> Daemon {
    let log = std::fs::File::create(home.join("daemon.test.log")).unwrap();
    let child = std::process::Command::new(bin())
        .args(["--network", network, "daemon", "run", "--locked", "--detached", "--interval", "5"])
        .env("QUAI_TERMINAL_HOME", home)
        .env("XDG_RUNTIME_DIR", runtime)
        .envs(env.iter().copied())
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .spawn()
        .unwrap();
    let daemon = Daemon { child, runtime: runtime.to_path_buf() };
    let started = Instant::now();
    while runtime_file(runtime, "engine-", ".sock").is_none() || daemon_pid(runtime).is_none() {
        assert!(started.elapsed() < Duration::from_secs(20), "the daemon never served its engine socket");
        std::thread::sleep(Duration::from_millis(50));
    }
    daemon
}

fn connect(daemon: &Daemon, wallet: &str) -> Engine {
    let runtime = daemon.runtime.clone();
    let socket = runtime_file(&runtime, "engine-", ".sock").unwrap();
    let dialer = Dialer {
        socket,
        daemon_pid: Box::new(move || daemon_pid(&runtime)),
        // Never start another: this test watches what happens without one.
        ensure_daemon: Box::new(|| Err("not in this test".into())),
    };
    Engine::Remote(Remote::connect(dialer, wallet.to_string(), "offline".into(), || {}).unwrap())
}

/// Events until one matches, or the time is up.
fn wait_for(engine: &Engine, what: &str, secs: u64, mut matches: impl FnMut(&Ev) -> bool) -> Vec<String> {
    let started = Instant::now();
    let mut seen = Vec::new();
    loop {
        while let Some(ev) = engine.try_recv() {
            let hit = matches(&ev);
            seen.push(describe(&ev));
            if hit {
                return seen;
            }
        }
        assert!(started.elapsed() < Duration::from_secs(secs), "no {what} within {secs}s; saw {seen:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn describe(ev: &Ev) -> String {
    match ev {
        Ev::Dashboard(d) => format!("Dashboard(unlocked={})", d.unlocked),
        Ev::Unlocked => "Unlocked".into(),
        Ev::Locked => "Locked".into(),
        Ev::EngineLost(why) => format!("EngineLost({why})"),
        Ev::UnlockFailed(e) => format!("UnlockFailed({e})"),
        Ev::Error(e) => format!("Error({e})"),
        Ev::Info(e) => format!("Info({e})"),
        Ev::Busy(b) => format!("Busy({b:?})"),
        _ => "other".into(),
    }
}

#[test]
fn the_daemon_holds_the_keys_and_a_killed_daemon_leaves_the_terminal_locked() {
    let home = tempfile::tempdir().unwrap();
    let runtime = tempfile::tempdir().unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(runtime.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let paths = wallet_core::paths::Paths::resolve(Some(home.path().to_path_buf())).unwrap();
    let registry = wallet_core::registry::Registry::new(paths);
    let meta = registry.create_hd("main", PHRASE, "english", "", "password123", true).unwrap();
    // A network nobody answers on: the engine starts, reads local state, and never reaches a node.
    cli(
        home.path(),
        runtime.path(),
        &["network", "add", "offline", "--rpc", "http://127.0.0.1:9", "--chain-id", "1337", "--genesis", &format!("0x{}", "11".repeat(32))],
    );
    let mut daemon = start_daemon(home.path(), runtime.path());

    let first = connect(&daemon, &meta.id);
    wait_for(&first, "first dashboard", 20, |ev| matches!(ev, Ev::Dashboard(_)));
    // A wrong password is refused where the vault is.
    first.unlock(meta.id.clone(), Zeroizing::new("wrong-password".into()));
    wait_for(&first, "refusal", 30, |ev| matches!(ev, Ev::UnlockFailed(_)));
    // The right one unlocks — in the daemon.
    first.unlock(meta.id.clone(), Zeroizing::new("password123".into()));
    wait_for(&first, "unlock", 30, |ev| matches!(ev, Ev::Unlocked));
    assert_eq!(wallet_core::identity::live_unlocked(), 0, "the terminal's process never holds keys");
    first.send(Cmd::Refresh { full: false });
    wait_for(&first, "an unlocked dashboard", 30, |ev| matches!(ev, Ev::Dashboard(d) if d.unlocked));

    // Another terminal on the same wallet starts locked: the first one's password is its own.
    let second = connect(&daemon, &meta.id);
    wait_for(&second, "second dashboard", 20, |ev| matches!(ev, Ev::Dashboard(_)));
    second.send(Cmd::Refresh { full: false });
    wait_for(&second, "the second terminal's dashboard", 30, |ev| matches!(ev, Ev::Dashboard(d) if !d.unlocked));
    // Nor can it export the phrase without the password.
    second.send(Cmd::ExportPhrase(Zeroizing::new("wrong-password".into())));
    let seen = wait_for(&second, "an export refusal", 30, |ev| matches!(ev, Ev::Error(_) | Ev::Secret(_)));
    assert!(seen.last().is_some_and(|s| s.starts_with("Error")), "a wrong password exported nothing: {seen:?}");

    // The daemon dies mid-session: the terminal locks at once, and says why.
    daemon.child.kill().unwrap();
    let _ = daemon.child.wait();
    wait_for(&first, "a lock that says why", 10, |ev| matches!(ev, Ev::EngineLost(_)));
    // Asked to sign meanwhile, it says nothing was sent, and never that something might be.
    let mut answered = None;
    first.send(Cmd::CommitConfirmed { op_id: "op".into(), words: "pay 5b32".into() });
    wait_for(&first, "an answer to the commit", 10, |ev| {
        if let Ev::CommitError { ambiguous, .. } = ev {
            answered = Some(*ambiguous);
            true
        } else {
            false
        }
    });
    assert_eq!(answered, Some(false), "nothing left this terminal, and it says so");
    assert_eq!(wallet_core::identity::live_unlocked(), 0);
}

/// A scratch home with one wallet and the silent network, and a private runtime directory.
fn scratch() -> (tempfile::TempDir, tempfile::TempDir, wallet_core::registry::WalletMeta) {
    let home = tempfile::tempdir().unwrap();
    let runtime = tempfile::tempdir().unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(runtime.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let paths = wallet_core::paths::Paths::resolve(Some(home.path().to_path_buf())).unwrap();
    let meta = wallet_core::registry::Registry::new(paths).create_hd("main", PHRASE, "english", "", "password123", true).unwrap();
    cli(
        home.path(),
        runtime.path(),
        &["network", "add", "offline", "--rpc", "http://127.0.0.1:9", "--chain-id", "1337", "--genesis", &format!("0x{}", "11".repeat(32))],
    );
    (home, runtime, meta)
}

/// The engine locks on its own when the terminal stops showing signs of life, whatever the
/// terminal does (it may be hung, or its window lost with the wallet open).
#[test]
fn the_engine_locks_an_idle_terminal_by_itself() {
    let (home, runtime, meta) = scratch();
    let daemon = start_daemon_with(home.path(), runtime.path(), &[("QUAI_TERMINAL_TEST_AUTOLOCK_SECS", "2")]);
    let engine = connect(&daemon, &meta.id);
    wait_for(&engine, "first dashboard", 20, |ev| matches!(ev, Ev::Dashboard(_)));
    engine.unlock(meta.id.clone(), Zeroizing::new("password123".into()));
    wait_for(&engine, "unlock", 30, |ev| matches!(ev, Ev::Unlocked));
    let unlocked = Instant::now();
    wait_for(&engine, "the engine's own lock", 15, |ev| matches!(ev, Ev::Locked));
    assert!(unlocked.elapsed() >= Duration::from_secs(1), "not before the idle time is up");
}

/// A terminal attached to the daemon gets its data from the daemon's data service, answered on
/// the same connection. The daemon decides which files a data worker opens: a wallet database
/// that is not a registered wallet's own is never opened.
#[test]
fn a_terminal_s_data_comes_from_the_daemon_and_only_from_its_own_files() {
    use quai_engine::data::{AlertOp, DataCmd, DataEv};
    let (home, runtime, meta) = scratch();
    let daemon = start_daemon(home.path(), runtime.path());
    let engine = connect(&daemon, &meta.id);
    let Engine::Remote(remote) = &engine else { unreachable!() };
    let data = remote.data_worker().expect("the data service, once");
    assert!(remote.data_worker().is_none(), "and only once");
    let config =
        wallet_core::config::AppConfig::load(&wallet_core::paths::Paths::resolve(Some(home.path().to_path_buf())).unwrap()).unwrap();
    let network = config.network("offline").unwrap();
    let elsewhere = home.path().join("elsewhere");
    for forged in [elsewhere.join("app.sqlite"), home.path().join("wallets").join("not-a-wallet").join("app.sqlite")] {
        std::fs::create_dir_all(forged.parent().unwrap()).unwrap();
        data.tx.send(DataCmd::Configure { network: network.clone(), policy: config.data_policy(), app_db: Some(forged) });
    }
    data.tx.send(DataCmd::Alerts(AlertOp::Load));
    let started = Instant::now();
    loop {
        match data.rx.recv_timeout(Duration::from_millis(100)) {
            Ok(DataEv::Alerts { alerts, .. }) => {
                assert!(alerts.is_empty(), "a new wallet has none");
                break;
            }
            Ok(_) => {}
            Err(_) => assert!(started.elapsed() < Duration::from_secs(20), "no answer from the daemon's data service"),
        }
    }
    assert!(!elsewhere.join("app.sqlite").exists(), "a path the client named is not opened");
    assert!(!home.path().join("wallets/not-a-wallet/app.sqlite").exists(), "nor a wallet that is not registered");
    // The engine on the same connection still answers.
    wait_for(&engine, "a dashboard", 20, |ev| matches!(ev, Ev::Dashboard(_)));
}

/// Whoever connects and says nonsense is dropped; the daemon keeps serving everyone else.
#[test]
fn the_engine_socket_survives_hostile_clients() {
    use std::io::{Read, Write};
    let (home, runtime, meta) = scratch();
    let daemon = start_daemon(home.path(), runtime.path());
    let socket = runtime_file(runtime.path(), "engine-", ".sock").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&socket).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "the socket is the user's alone");
    }
    // Another protocol is refused, and told why.
    let mut stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let hello = quai_engine::protocol::encode(&quai_engine::protocol::ClientMsg::Hello { protocol: 999, build: "x".into() }).unwrap();
    stream.write_all(&hello).unwrap();
    let mut answer = Vec::new();
    let _ = stream.read_to_end(&mut answer);
    let refused: quai_engine::protocol::HostMsg = quai_engine::protocol::decode(&answer[4..]).unwrap();
    assert!(matches!(refused, quai_engine::protocol::HostMsg::Refused(why) if why.contains("protocol")));
    // Random bytes, a giant announced frame, a frame that stops halfway, a Cmd before any hello.
    let mut rng = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let mut garbage: Vec<Vec<u8>> = vec![
        u32::MAX.to_be_bytes().to_vec(),
        [&100u32.to_be_bytes()[..], &[1, 2, 3]].concat(),
        quai_engine::protocol::encode(&quai_engine::protocol::ClientMsg::Cmd(Cmd::Lock)).unwrap().to_vec(),
    ];
    for _ in 0..40 {
        let len = (next() % 200) as usize;
        garbage.push((0..len).map(|_| next() as u8).collect());
    }
    for bytes in garbage {
        let mut stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
        let _ = stream.write_all(&bytes);
        let _ = stream.shutdown(std::net::Shutdown::Write);
        let mut sink = Vec::new();
        let _ = stream.read_to_end(&mut sink);
    }
    // Still serving.
    let engine = connect(&daemon, &meta.id);
    wait_for(&engine, "a dashboard after all that", 20, |ev| matches!(ev, Ev::Dashboard(_)));
}

/// A trade plan walked by the daemon's engine on the local dev chain, as a terminal would walk it:
/// every review answered, each next one asked for when the engine says the last step is in.
///
/// Run by hand against a trading fixture (`scripts/devnet-trading-e2e.sh` leaves one):
///   QW_PLAN_HOME=/tmp/quai-trading-e2e.X/wallet QW_PLAN_PASSWORD=/tmp/quai-trading-e2e.X/password \
///   QW_PLAN_FROM=<token> QW_PLAN_AMOUNT=1 cargo test --test engine_daemon -- --ignored plan
#[test]
#[ignore = "needs the local dev chain and a trading fixture"]
fn a_plan_runs_through_the_daemon_on_the_dev_chain() {
    use quai_engine::plans::{Phase, PlanCmd};
    let env = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("{k}"));
    let home = tempfile::tempdir().unwrap();
    let copied = std::process::Command::new("cp").args(["-a", &format!("{}/.", env("QW_PLAN_HOME"))]).arg(home.path()).status().unwrap();
    assert!(copied.success());
    let runtime = tempfile::tempdir().unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(runtime.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let password = std::fs::read_to_string(env("QW_PLAN_PASSWORD")).unwrap().trim().to_string();
    let daemon = start_daemon_on(home.path(), runtime.path(), "trading-fixture", &[("QUAI_WALLET_INSECURE_FAST_KDF", "1")]);
    let paths = wallet_core::paths::Paths::resolve(Some(home.path().to_path_buf())).unwrap();
    let registry = wallet_core::registry::Registry::new(paths);
    let meta = registry.list().unwrap().into_iter().find(|m| m.name == "trader").expect("the trader wallet");
    let engine = {
        let runtime = daemon.runtime.clone();
        let dialer = Dialer {
            socket: runtime_file(&runtime, "engine-", ".sock").unwrap(),
            daemon_pid: Box::new(move || daemon_pid(&runtime)),
            ensure_daemon: Box::new(|| Err("not in this test".into())),
        };
        Engine::Remote(Remote::connect(dialer, meta.id.clone(), "trading-fixture".into(), || {}).unwrap())
    };
    wait_for(&engine, "first dashboard", 30, |ev| matches!(ev, Ev::Dashboard(_)));
    engine.unlock(meta.id.clone(), Zeroizing::new(password));
    wait_for(&engine, "unlock", 60, |ev| matches!(ev, Ev::Unlocked));
    let account = meta.default_quai_account().unwrap().address.clone();
    let intent = wallet_core::execution::TradingIntent {
        account,
        max_fee: None,
        action: wallet_core::execution::TradingAction::Swap {
            from: env("QW_PLAN_FROM"),
            to: "quai".into(),
            amount: env("QW_PLAN_AMOUNT"),
            slippage: 300,
            deadline: 20,
        },
    };
    engine.send(Cmd::Plan(PlanCmd::Start { label: "plan e2e".into(), intent }));
    let started = Instant::now();
    let mut kinds = Vec::new();
    let mut id = String::new();
    let mut finished = false;
    while !finished {
        assert!(started.elapsed() < Duration::from_secs(600), "the plan did not finish: {kinds:?}");
        let Some(ev) = engine.try_recv() else {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        match ev {
            Ev::Plan(view) => {
                id = view.id.clone();
                eprintln!("plan: {:?} done={:?} last_step={}", view.phase, view.done, view.last_step);
                match &view.phase {
                    Phase::Ready => engine.send(Cmd::Plan(PlanCmd::Next { id: id.clone() })),
                    Phase::Stopped(said) => panic!("stopped: {said}"),
                    Phase::Done(_) => finished = true,
                    _ => {}
                }
            }
            Ev::Review(review) => {
                eprintln!("review: {} {:?} risks={:?}", review.op_id, review.kind, review.risks);
                kinds.push(review.kind.clone());
                match review.confirm.clone() {
                    Some(words) => engine.send(Cmd::CommitConfirmed { op_id: review.op_id.clone(), words }),
                    None => engine.send(Cmd::Commit(review.op_id.clone())),
                }
            }
            Ev::Submitted(sub) => eprintln!("submitted {} {}", sub.op_id, sub.tx_hash),
            Ev::PrepareError(e) | Ev::Error(e) => eprintln!("error: {e}"),
            Ev::CommitError { message, .. } => panic!("commit failed: {message}"),
            _ => {}
        }
    }
    assert!(!id.is_empty());
    assert_eq!(kinds.last(), Some(&wallet_core::journal::OpKind::Swap), "the swap was the last step: {kinds:?}");
    let app = wallet_core::appdb::AppDb::open(&registry.paths().wallet_dir(&meta.id).join("app.sqlite")).unwrap();
    let saved = app.trade_plan(&id).unwrap().unwrap();
    assert_eq!(saved.state, wallet_core::plans::PlanState::Complete, "the engine recorded it complete: {}", saved.reason);
}
