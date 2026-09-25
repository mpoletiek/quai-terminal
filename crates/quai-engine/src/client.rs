//! What a terminal holds of its engine: one in the daemon, reached over the engine socket, or
//! one in this process (`--standalone`). The terminal says the same things to either and hears
//! the same events; only a standalone terminal ever holds keys.
//!
//! A remote engine that goes away (the daemon was stopped, killed, or replaced by a newer
//! build) takes this client's keys with it, so the terminal hears [`Ev::Locked`] at once and
//! shows the lock screen ([`Ev::EngineLost`]): nothing on screen may claim a wallet that can sign when nothing can.
//! The client then reconnects on its own, starting the daemon again if it has to, and attaches
//! to the wallet and network on screen — locked, until the password is given again.

use crate::data::{DataCmd, DataEv, DataSender, DataWorker};
use crate::host::Host;
use crate::protocol::{self, ClientMsg, FrameError, HOST_FRAME_LIMIT, HostMsg, PROTOCOL};
use crate::worker::{Cmd, Ev};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

/// How often activity is passed on to a remote engine's auto-lock, at most.
const ACTIVITY_EVERY: Duration = Duration::from_secs(5);

/// A terminal's engine.
pub enum Engine {
    /// In this process: the keys are here.
    Local(Box<Host>),
    /// In the daemon.
    Remote(Remote),
}

impl Engine {
    /// A command for the engine.
    pub fn send(&self, cmd: Cmd) {
        match self {
            Engine::Local(host) => host.send(cmd),
            Engine::Remote(remote) => remote.send(ClientMsg::Cmd(cmd)),
        }
    }

    /// Check this password where the keys live, and hold them there. The answer is an event:
    /// [`Ev::Unlocked`] or [`Ev::UnlockFailed`].
    pub fn unlock(&self, wallet: String, password: Zeroizing<String>) {
        match self {
            Engine::Local(host) => host.unlock(wallet, password),
            Engine::Remote(remote) => remote.send(ClientMsg::Unlock { wallet, password }),
        }
    }

    /// Someone is at the keyboard: the engine's auto-lock starts over.
    pub fn activity(&self) {
        match self {
            Engine::Local(host) => host.touch(),
            Engine::Remote(remote) => {
                let now = Instant::now();
                if remote.activity_sent.get().is_none_or(|at| now.duration_since(at) >= ACTIVITY_EVERY) {
                    remote.activity_sent.set(Some(now));
                    remote.send(ClientMsg::Activity);
                }
            }
        }
    }

    /// The next event, if one is waiting.
    pub fn try_recv(&self) -> Option<Ev> {
        match self {
            Engine::Local(host) => host.try_recv(),
            Engine::Remote(remote) => remote.events.try_recv().ok(),
        }
    }

    /// Whether the engine (and the keys) run in another process.
    pub fn is_remote(&self) -> bool {
        matches!(self, Engine::Remote(_))
    }
}

/// How a client finds the daemon: provided by the program, which knows its runtime files.
pub struct Dialer {
    /// The engine socket.
    pub socket: std::path::PathBuf,
    /// The pid of the process holding the daemon's lock, if one does: the only process a
    /// client talks to (a stale socket someone else bound is never it).
    pub daemon_pid: Box<dyn Fn() -> Option<u32> + Send>,
    /// Start a daemon running this program, or replace one running another build. Blocking.
    pub ensure_daemon: Box<dyn Fn() -> Result<(), String> + Send>,
}

/// An engine in the daemon.
pub struct Remote {
    outbox: tokio::sync::mpsc::UnboundedSender<ClientMsg>,
    events: Receiver<Ev>,
    /// The data service's results, until [`Remote::data_worker`] takes them.
    data: std::cell::RefCell<Option<Receiver<DataEv>>>,
    activity_sent: std::cell::Cell<Option<Instant>>,
}

impl Remote {
    /// Attach to an engine for `wallet` on `network` in the daemon, starting the daemon if it
    /// is not running. Returns at once: connecting happens on a thread of its own, and
    /// commands sent meanwhile wait for it.
    pub fn connect(dialer: Dialer, wallet: String, network: String, wake: fn()) -> std::io::Result<Remote> {
        let (outbox, commands) = tokio::sync::mpsc::unbounded_channel::<ClientMsg>();
        let (events_tx, events) = std::sync::mpsc::channel::<Ev>();
        let (data_tx, data) = std::sync::mpsc::channel::<DataEv>();
        std::thread::Builder::new().name("engine-client".into()).spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
                let _ = events_tx.send(Ev::Error("engine client: no runtime".into()));
                wake();
                return;
            };
            let say = move |ev: Ev| {
                let _ = events_tx.send(ev);
                wake();
            };
            let answer = move |ev: DataEv| {
                let _ = data_tx.send(ev);
                wake();
            };
            runtime.block_on(run(dialer, wallet, network, commands, say, answer));
        })?;
        Ok(Remote { outbox, events, data: std::cell::RefCell::new(Some(data)), activity_sent: std::cell::Cell::new(None) })
    }

    /// The daemon's data service for this terminal, as a data worker: requests travel on this
    /// connection, in order with the engine's commands, and are answered by a worker of this
    /// terminal's own in the daemon. Requests made while the engine is away are dropped (a
    /// screen asks again when its read goes stale). Once per connection: `None` after the first.
    pub fn data_worker(&self) -> Option<DataWorker> {
        let rx = self.data.borrow_mut().take()?;
        Some(DataWorker { tx: DataSender::Remote(self.outbox.clone()), rx })
    }

    fn send(&self, msg: ClientMsg) {
        let _ = self.outbox.send(msg);
    }
}

/// Where the client stands, for reattaching after the engine went away.
struct Attached {
    wallet: String,
    network: String,
}

/// Data requests made while no engine was connected, and how this terminal last configured its
/// data service. Unlike engine commands they are only reads, so they are kept and sent when the
/// engine is back instead of being refused: a terminal asks for its first screen's data before a
/// daemon it had to start is listening. A daemon's new data worker hears the configuration again
/// first, since it knows nothing of this terminal's.
#[derive(Default)]
struct DataReplay {
    configured: Option<(wallet_core::network::NetworkProfile, wallet_core::config::DataPolicy, Option<std::path::PathBuf>)>,
    held: std::collections::VecDeque<DataCmd>,
}

/// Requests held while disconnected, at most: the newest are kept.
const DATA_HELD: usize = 512;

impl DataReplay {
    /// Note a configuration on its way out.
    fn saw(&mut self, cmd: &DataCmd) {
        if let DataCmd::Configure { network, policy, app_db } = cmd {
            // A wallet database once named stays named: a rebind to the same network keeps it.
            let app_db = app_db.clone().or_else(|| self.configured.as_ref().and_then(|c| c.2.clone()));
            self.configured = Some((network.clone(), *policy, app_db));
        }
    }

    /// Keep a request until the engine is back.
    fn hold(&mut self, cmd: DataCmd) {
        self.saw(&cmd);
        if matches!(cmd, DataCmd::Configure { .. } | DataCmd::Shutdown) {
            return;
        }
        if self.held.len() == DATA_HELD {
            self.held.pop_front();
        }
        self.held.push_back(cmd);
    }

    /// What a newly attached engine hears first: the configuration, then what was held.
    fn replay(&mut self) -> Vec<ClientMsg> {
        let configure =
            self.configured.clone().map(|(network, policy, app_db)| ClientMsg::Data(DataCmd::Configure { network, policy, app_db }));
        configure.into_iter().chain(self.held.drain(..).map(ClientMsg::Data)).collect()
    }
}

/// Connect, attach, relay; and again when the engine goes away, until the terminal closes.
async fn run(
    dialer: Dialer,
    wallet: String,
    network: String,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<ClientMsg>,
    say: impl Fn(Ev),
    answer: impl Fn(DataEv),
) {
    let mut at = Attached { wallet, network };
    let mut data = DataReplay::default();
    let build = protocol::build();
    let mut attempt = 0u32;
    let mut replaced = false;
    let mut connected_before = false;
    loop {
        match dial(&dialer, &build, &at).await {
            Ok((stream, their_build)) => {
                // A daemon running other code is replaced once; if it is still other code after
                // that (two builds taking turns), this one is served anyway: the protocol matches.
                if their_build != build && !replaced {
                    replaced = true;
                    drop(stream);
                    let ensure = &dialer.ensure_daemon;
                    let _ = ensure();
                    continue;
                }
                attempt = 0;
                if connected_before {
                    say(Ev::EngineBack);
                }
                connected_before = true;
                wallet_core::diag::mark("engine.attached");
                let why = relay(stream, &mut at, &mut data, &mut commands, &say, &answer).await;
                match why {
                    Gone::Closed => return,
                    Gone::Engine(why) => {
                        wallet_core::diag::mark(&format!("engine.lost {why}"));
                        // The keys went with the engine: the screen locks now, and says why.
                        say(Ev::EngineLost(why));
                    }
                }
            }
            Err(Dial::Refused(why)) => {
                say(Ev::Error(format!("the daemon would not serve this terminal: {why}")));
                if !replaced {
                    replaced = true;
                    let ensure = &dialer.ensure_daemon;
                    let _ = ensure();
                    continue;
                }
                attempt += 1;
            }
            Err(Dial::Unavailable(why)) => {
                attempt += 1;
                if attempt == 1 || attempt.is_multiple_of(10) {
                    wallet_core::diag::mark(&format!("engine.dial_failed {why}"));
                }
                // Not running (or not answering): start it, off this loop's back.
                let ensure = &dialer.ensure_daemon;
                if let Err(e) = ensure()
                    && attempt >= 3
                {
                    say(Ev::Error(format!("the daemon did not start ({e}); run with --standalone to keep the engine in this terminal")));
                }
            }
        }
        // Whatever was asked while there was no engine is answered, not kept for the next one:
        // a review belongs to the engine that built it, and a password waits for nobody.
        while let Ok(msg) = commands.try_recv() {
            match msg {
                ClientMsg::Cmd(Cmd::Shutdown) => return,
                ClientMsg::Data(cmd) => data.hold(cmd),
                ClientMsg::Cmd(cmd) => {
                    track(&mut at, &cmd);
                    refuse(cmd, &say);
                }
                ClientMsg::Unlock { .. } => say(Ev::UnlockFailed("the engine is not connected yet — try again in a moment".into())),
                _ => {}
            }
        }
        if commands.is_closed() {
            return;
        }
        let pause = Duration::from_millis(200 * u64::from(attempt.clamp(1, 10)));
        // Wait, but notice a terminal that closed meanwhile.
        tokio::select! {
            _ = tokio::time::sleep(pause) => {}
            msg = commands.recv() => match msg {
                None | Some(ClientMsg::Cmd(Cmd::Shutdown)) => return,
                Some(ClientMsg::Cmd(cmd)) => { track(&mut at, &cmd); refuse(cmd, &say); }
                Some(ClientMsg::Unlock { .. }) => say(Ev::UnlockFailed("the engine is not connected yet — try again in a moment".into())),
                Some(ClientMsg::Data(cmd)) => data.hold(cmd),
                Some(_) => {}
            },
        }
    }
}

/// Keep up with the wallet and network on screen.
fn track(at: &mut Attached, cmd: &Cmd) {
    match cmd {
        Cmd::SwitchWallet(w) => at.wallet = w.clone(),
        Cmd::SwitchNetwork(n) => at.network = n.clone(),
        _ => {}
    }
}

/// Say that a command reached no engine, where the screen waits for an answer.
fn refuse(cmd: Cmd, say: &impl Fn(Ev)) {
    const WHY: &str = "not sent: the engine is reconnecting";
    match cmd {
        Cmd::Prepare(_) => say(Ev::PrepareError(WHY.into())),
        Cmd::Commit(op_id) | Cmd::CommitConfirmed { op_id, .. } => say(Ev::CommitError { op_id, message: WHY.into(), ambiguous: false }),
        Cmd::Refresh { .. } | Cmd::MarkRead | Cmd::ChatNews { .. } => {}
        _ => say(Ev::Error(WHY.into())),
    }
}

enum Dial {
    /// Nothing to talk to (no daemon, a stale socket, one that is not ours).
    Unavailable(String),
    /// The daemon answered and said no.
    Refused(String),
}

/// Connect, check who is at the other end, and shake hands. Returns the stream and the
/// daemon's build.
async fn dial(dialer: &Dialer, build: &str, at: &Attached) -> Result<(tokio::net::UnixStream, String), Dial> {
    let socket = &dialer.socket;
    check_private(socket).map_err(Dial::Unavailable)?;
    let pid = (dialer.daemon_pid)().ok_or_else(|| Dial::Unavailable("no daemon is running".into()))?;
    let mut stream = tokio::net::UnixStream::connect(socket).await.map_err(|e| Dial::Unavailable(format!("connect: {e}")))?;
    let cred = stream.peer_cred().map_err(|_| Dial::Unavailable("the kernel would not say who is listening".into()))?;
    if cred.uid() != crate::server::own_uid() {
        return Err(Dial::Unavailable("the other end is not you".into()));
    }
    // Where the kernel says which process it is, it must be the one holding the daemon's lock.
    if let Some(peer) = cred.pid()
        && peer as u32 != pid
    {
        return Err(Dial::Unavailable("the other end is not the daemon holding the lock".into()));
    }
    let handshake = async {
        protocol::write_msg(&mut stream, &ClientMsg::Hello { protocol: PROTOCOL, build: build.to_string() }).await?;
        let welcome = protocol::read_msg::<HostMsg>(&mut stream, HOST_FRAME_LIMIT).await?;
        let their = match welcome {
            HostMsg::Welcome { protocol, build } if protocol == PROTOCOL => build,
            HostMsg::Welcome { protocol, .. } => return Ok(Err(format!("the daemon speaks protocol {protocol}"))),
            HostMsg::Refused(why) => return Ok(Err(why)),
            HostMsg::Ev(_) | HostMsg::Data(_) => return Ok(Err("the daemon spoke out of turn".into())),
        };
        protocol::write_msg(&mut stream, &ClientMsg::Attach { wallet: at.wallet.clone(), network: at.network.clone() }).await?;
        Ok::<_, FrameError>(Ok(their))
    };
    match tokio::time::timeout(Duration::from_secs(10), handshake).await {
        Ok(Ok(Ok(their))) => Ok((stream, their)),
        Ok(Ok(Err(why))) => Err(Dial::Refused(why)),
        Ok(Err(e)) => Err(Dial::Unavailable(e.to_string())),
        Err(_) => Err(Dial::Unavailable("the daemon did not answer the handshake".into())),
    }
}

/// The socket and its directory belong to this user and nobody else can reach them.
fn check_private(socket: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let me = crate::server::own_uid();
    let dir = socket.parent().ok_or("no runtime directory")?;
    let dir_meta = std::fs::symlink_metadata(dir).map_err(|_| "no runtime directory".to_string())?;
    if !dir_meta.is_dir() || dir_meta.uid() != me || dir_meta.mode() & 0o077 != 0 {
        return Err("the runtime directory is not private to you".into());
    }
    let meta = std::fs::symlink_metadata(socket).map_err(|_| "no engine socket".to_string())?;
    if !meta.file_type().is_socket() || meta.uid() != me || meta.mode() & 0o077 != 0 {
        return Err("the engine socket is not private to you".into());
    }
    Ok(())
}

/// Why a connection ended.
enum Gone {
    /// The terminal closed.
    Closed,
    /// The engine went away.
    Engine(String),
}

/// Commands out, events in, until one side goes.
async fn relay(
    stream: tokio::net::UnixStream,
    at: &mut Attached,
    data: &mut DataReplay,
    commands: &mut tokio::sync::mpsc::UnboundedReceiver<ClientMsg>,
    say: &impl Fn(Ev),
    answer: &impl Fn(DataEv),
) -> Gone {
    let (mut read, mut write) = stream.into_split();
    for msg in data.replay() {
        if let Err(e) = protocol::write_msg(&mut write, &msg).await {
            return Gone::Engine(e.to_string());
        }
    }
    loop {
        tokio::select! {
            msg = commands.recv() => {
                let Some(msg) = msg else { return Gone::Closed };
                let closing = matches!(msg, ClientMsg::Cmd(Cmd::Shutdown));
                match &msg {
                    ClientMsg::Cmd(cmd) => track(at, cmd),
                    ClientMsg::Data(cmd) => data.saw(cmd),
                    _ => {}
                }
                if closing {
                    // The engine ends with the connection; nothing waits for it.
                    return Gone::Closed;
                }
                if let Err(e) = protocol::write_msg(&mut write, &msg).await {
                    match msg {
                        ClientMsg::Cmd(cmd) => refuse(cmd, say),
                        ClientMsg::Data(cmd) => data.hold(cmd),
                        _ => {}
                    }
                    return Gone::Engine(e.to_string());
                }
            }
            incoming = protocol::read_msg::<HostMsg>(&mut read, HOST_FRAME_LIMIT) => match incoming {
                Ok(HostMsg::Ev(ev)) => say(ev),
                Ok(HostMsg::Data(ev)) => answer(ev),
                Ok(HostMsg::Refused(why)) => return Gone::Engine(why),
                Ok(HostMsg::Welcome { .. }) => return Gone::Engine("the daemon repeated its welcome".into()),
                Err(e) => return Gone::Engine(e.to_string()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configure(app_db: Option<&str>) -> DataCmd {
        DataCmd::Configure {
            network: wallet_core::network::NetworkProfile::builtins().remove(0),
            policy: wallet_core::config::AppConfig::default().data_policy(),
            app_db: app_db.map(Into::into),
        }
    }

    fn names(msgs: &[ClientMsg]) -> Vec<String> {
        msgs.iter()
            .map(|m| match m {
                ClientMsg::Data(DataCmd::Configure { app_db, .. }) => {
                    format!("configure {}", app_db.as_ref().map_or("-".into(), |p| p.display().to_string()))
                }
                ClientMsg::Data(DataCmd::TokenInfo(a)) => format!("token {a}"),
                ClientMsg::Data(_) => "data".into(),
                _ => "other".into(),
            })
            .collect()
    }

    #[test]
    fn reads_asked_before_the_engine_is_back_are_sent_after_its_configuration() {
        let mut data = DataReplay::default();
        data.hold(DataCmd::TokenInfo("a".into()));
        data.hold(configure(Some("/w/app.sqlite")));
        data.hold(DataCmd::TokenInfo("b".into()));
        data.hold(DataCmd::Shutdown);
        assert_eq!(names(&data.replay()), ["configure /w/app.sqlite", "token a", "token b"]);
        // A later connection hears the configuration again, and nothing already sent.
        assert_eq!(names(&data.replay()), ["configure /w/app.sqlite"]);
        // A rebind that names no database (a network switch) keeps the one named before.
        data.saw(&configure(None));
        assert_eq!(names(&data.replay()), ["configure /w/app.sqlite"]);
    }

    #[test]
    fn a_long_outage_keeps_the_newest_reads() {
        let mut data = DataReplay::default();
        for i in 0..DATA_HELD + 10 {
            data.hold(DataCmd::TokenInfo(i.to_string()));
        }
        let sent = names(&data.replay());
        assert_eq!(sent.len(), DATA_HELD);
        assert_eq!(sent.first().map(String::as_str), Some("token 10"));
    }
}
