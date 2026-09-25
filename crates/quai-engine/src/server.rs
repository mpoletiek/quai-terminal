//! The engine socket, served by the daemon: each terminal that connects gets an engine of its
//! own ([`crate::host::Host`]), its keys held for it alone.
//!
//! Who may connect: only this user. The socket sits in the user's private runtime directory
//! (0700) with mode 0600, and each connection's peer is asked of the kernel (`SO_PEERCRED` on
//! Linux, `getpeereid` on macOS) — another user's process is dropped before a byte is read.
//! A connection must say [`ClientMsg::Hello`] with this [`PROTOCOL`] within a few seconds, then
//! attach to a wallet; anything malformed, oversized or out of order closes it. When the client
//! goes, its keys go with it.
//!
//! Each connection also gets the daemon's data service: a data worker of its own, started on its
//! first request, in this one process. Every terminal's reads then share one HTTP client, one
//! per-host pace (the explorer allows so many requests a minute per address, not per terminal),
//! in-flight reads and caches, while each worker keeps its own queue and answers only its client.
//! That worker parses third-party data (explorer, indexers, metadata) in the process that holds
//! keys; pictures are still decoded in a sandboxed helper process of their own.

use crate::data::{DataCmd, DataEv, DataSender, DataWorker};
use crate::host::Host;
use crate::protocol::{self, CLIENT_FRAME_LIMIT, ClientMsg, FrameError, HostMsg, PROTOCOL};
use crate::worker::Cmd;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use wallet_core::config::AppConfig;
use wallet_core::registry::Registry;

/// Terminals served at once.
const CONNECTIONS: usize = 8;

/// How long a new connection has to say hello and attach.
const HANDSHAKE: Duration = Duration::from_secs(10);

/// This process's real user id.
pub fn own_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// Serve engines on `listener` until the returned handle is dropped. `build` is this program
/// ([`protocol::build`]), told to each client so it can tell when the daemon runs other code.
pub fn serve(listener: std::os::unix::net::UnixListener, registry: Registry, build: String) -> std::io::Result<Server> {
    listener.set_nonblocking(true)?;
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let thread = std::thread::Builder::new().name("engine-server".into()).spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() else { return };
        runtime.block_on(async move {
            let Ok(listener) = tokio::net::UnixListener::from_std(listener) else { return };
            let slots = Arc::new(tokio::sync::Semaphore::new(CONNECTIONS));
            let next = Arc::new(AtomicU64::new(1));
            loop {
                let accepted = tokio::select! {
                    accepted = listener.accept() => accepted,
                    _ = stopped.changed() => break,
                };
                let Ok((stream, _)) = accepted else { continue };
                // Only this user, on the kernel's word.
                if !stream.peer_cred().is_ok_and(|c| c.uid() == own_uid()) {
                    wallet_core::diag::mark("engine.refused_peer");
                    continue;
                }
                let Ok(permit) = slots.clone().try_acquire_owned() else {
                    let mut stream = stream;
                    let _ = protocol::write_msg(&mut stream, &HostMsg::Refused("the daemon serves 8 terminals at once".into())).await;
                    continue;
                };
                let (registry, build) = (registry.clone(), build.clone());
                let id = next.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let _permit = permit;
                    let why = connection(stream, registry, build, id).await;
                    wallet_core::diag::mark(&format!("engine.client_{id}_closed {why}"));
                });
            }
        });
    })?;
    Ok(Server { stop, thread: Some(thread) })
}

/// The engine server; dropping it stops accepting (engines already attached end as their
/// clients go, or with the process).
pub struct Server {
    stop: tokio::sync::watch::Sender<bool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// One terminal, from hello to goodbye. Returns why it ended.
async fn connection(stream: tokio::net::UnixStream, registry: Registry, build: String, id: u64) -> String {
    let (mut read, mut write) = stream.into_split();
    // The handshake.
    let hello = tokio::time::timeout(HANDSHAKE, protocol::read_msg::<ClientMsg>(&mut read, CLIENT_FRAME_LIMIT)).await;
    match hello {
        Ok(Ok(ClientMsg::Hello { protocol, .. })) if protocol == PROTOCOL => {}
        Ok(Ok(ClientMsg::Hello { protocol, .. })) => {
            let why = format!("this daemon speaks engine protocol {PROTOCOL}, not {protocol}: restart it with this program");
            let _ = protocol::write_msg(&mut write, &HostMsg::Refused(why)).await;
            return "protocol mismatch".into();
        }
        Ok(Ok(_)) => return "no hello".into(),
        Ok(Err(e)) => return e.to_string(),
        Err(_) => return "hello timed out".into(),
    }
    if protocol::write_msg(&mut write, &HostMsg::Welcome { protocol: PROTOCOL, build }).await.is_err() {
        return "closed at welcome".into();
    }
    let attach = tokio::time::timeout(HANDSHAKE, protocol::read_msg::<ClientMsg>(&mut read, CLIENT_FRAME_LIMIT)).await;
    let (wallet, network) = match attach {
        Ok(Ok(ClientMsg::Attach { wallet, network })) => (wallet, network),
        Ok(Ok(_)) => return "no attach".into(),
        Ok(Err(e)) => return e.to_string(),
        Err(_) => return "attach timed out".into(),
    };
    // The engine, for this client alone.
    let started = registry.load(&wallet).and_then(|meta| AppConfig::load(registry.paths()).map(|config| (meta, config))).and_then(
        |(meta, config)| {
            config.network(&network)?;
            // Whether keys go to the daemon's own watcher too is read at each unlock.
            Host::attach(registry.clone(), config, &format!("engine:{id}"), true, meta, network.clone(), || {})
                .map_err(|e| wallet_core::CoreError::Storage(format!("engine: {e}")))
        },
    );
    let mut host = match started {
        Ok(host) => host,
        Err(e) => {
            let _ = protocol::write_msg(&mut write, &HostMsg::Refused(e.to_string())).await;
            return format!("attach failed: {e}");
        }
    };
    // Events reach the socket through a thread of their own: the worker's channel blocks.
    let events = host.take_events();
    let (out, mut outbox) = tokio::sync::mpsc::unbounded_channel::<crate::worker::Ev>();
    let _ = std::thread::Builder::new().name(format!("engine-events-{id}")).spawn(move || {
        for ev in events {
            if out.send(ev).is_err() {
                return;
            }
        }
    });
    let mut at = Attached { wallet, network };
    let mut data: Option<DataSender> = None;
    let (data_out, mut data_inbox) = tokio::sync::mpsc::unbounded_channel::<DataEv>();
    // From here the client's frames are read by a task of their own: a read waiting in the
    // `select!` below would be dropped half-way whenever an event went out first.
    let (mut frames, _reader) = protocol::reader::<ClientMsg, _>(read, CLIENT_FRAME_LIMIT);
    let why = loop {
        tokio::select! {
            incoming = frames.recv() => match incoming.unwrap_or(Err(FrameError::Closed)) {
                Ok(ClientMsg::Hello { .. } | ClientMsg::Attach { .. }) => break "handshake repeated".to_string(),
                Ok(ClientMsg::Data(cmd)) => {
                    let Some(cmd) = host_side(cmd, &registry) else { continue };
                    if data.is_none() {
                        data = start_data(&registry, &at, data_out.clone());
                    }
                    // A worker that has stopped (the client asked it to) starts again on the next request.
                    if data.as_ref().is_some_and(|tx| !tx.send(cmd)) {
                        data = None;
                    }
                }
                Ok(msg) => {
                    if let ClientMsg::Cmd(cmd) = &msg {
                        at.track(cmd);
                    }
                    host.handle(msg)
                }
                Err(FrameError::Closed) => break "client left".to_string(),
                Err(e) => break e.to_string(),
            },
            ev = outbox.recv() => match ev {
                Some(ev) => {
                    if let Err(e) = protocol::write_msg(&mut write, &HostMsg::Ev(ev)).await {
                        break e.to_string();
                    }
                }
                None => break "engine stopped".to_string(),
            },
            Some(ev) = data_inbox.recv() => {
                if let Err(e) = protocol::write_msg(&mut write, &HostMsg::Data(ev)).await {
                    break e.to_string();
                }
            }
        }
    };
    if let Some(tx) = data {
        tx.send(DataCmd::Shutdown);
    }
    // The client is gone: so are its keys (the daemon's own, when shared, stay until its lock).
    if host.is_unlocked() {
        host.lock_now_keeping_shared();
    }
    drop(host);
    why
}

/// The wallet and network a client's engine is on, followed through its switches.
struct Attached {
    wallet: String,
    network: String,
}

impl Attached {
    fn track(&mut self, cmd: &Cmd) {
        match cmd {
            Cmd::SwitchWallet(w) => self.wallet = w.clone(),
            Cmd::SwitchNetwork(n) => self.network = n.clone(),
            _ => {}
        }
    }
}

/// A client's data worker, bound to the wallet and network its engine is on, with the data
/// policy in the configuration (the client sends its own with its first `Configure`).
fn start_data(registry: &Registry, at: &Attached, out: tokio::sync::mpsc::UnboundedSender<DataEv>) -> Option<DataSender> {
    let config = AppConfig::load(registry.paths()).ok()?;
    let network = config.network(&at.network).ok()?;
    registry.load(&at.wallet).ok()?;
    let app_db = registry.paths().wallet_dir(&at.wallet).join("app.sqlite");
    let worker = DataWorker::spawn(app_db, registry.paths().shared_cache(), network, config.data_policy()).ok()?;
    let events = worker.rx;
    // The worker's channel blocks; its results reach the socket through a thread of their own.
    std::thread::Builder::new()
        .name("engine-data-events".into())
        .spawn(move || {
            for ev in events {
                if out.send(ev).is_err() {
                    return;
                }
            }
        })
        .ok()?;
    Some(worker.tx)
}

/// A client's data request as the host serves it. A `Configure` names a network and a wallet
/// database: the network's profile comes from this host's configuration, by id, and the database
/// must be a registered wallet's own. Anything else is dropped.
fn host_side(cmd: DataCmd, registry: &Registry) -> Option<DataCmd> {
    match cmd {
        DataCmd::Configure { network, policy, app_db } => {
            let config = AppConfig::load(registry.paths()).ok()?;
            let network = config.network(&network.id).ok()?;
            let app_db = match app_db {
                None => None,
                Some(path) => Some(wallet_db(registry, &path)?),
            };
            Some(DataCmd::Configure { network, policy, app_db })
        }
        cmd => Some(cmd),
    }
}

/// `path`, when it is exactly a registered wallet's cache database.
fn wallet_db(registry: &Registry, path: &std::path::Path) -> Option<std::path::PathBuf> {
    let id = path.parent()?.file_name()?.to_str()?;
    registry.load(id).ok()?;
    let own = registry.paths().wallet_dir(id).join("app.sqlite");
    (own == path).then_some(own)
}
