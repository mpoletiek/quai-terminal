//! The daemon: every wallet on this computer watched in one process — monitoring, notifications,
//! alerts, chats, the status file, and private payments and sealed chats for the wallets it holds
//! unlocked.
//! `daemon start` runs it in the background and `daemon stop` ends it; `daemon run` is the same
//! loop in the foreground (systemd, or watching its log). A control socket in the user's private
//! runtime directory takes `unlock`, `lock`, `status` and `stop`.

use crate::commands::Ctx;
use crate::notify;
use crate::prompt;
use wallet_core::config::{AppConfig, Features};
use wallet_core::extras;
use wallet_core::{CoreError, Result};

/// Never chmod through a pre-existing runtime symlink or adopt another user's directory.
fn ensure_runtime_dir(path: &std::path::Path) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt};
        for ancestor in path.ancestors() {
            if std::fs::symlink_metadata(ancestor).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(CoreError::Storage("daemon runtime path must not contain symlinks".into()));
            }
        }
        match std::fs::symlink_metadata(path) {
            Ok(metadata) => {
                if !metadata.is_dir() || metadata.uid() != own_uid() || metadata.mode() & 0o077 != 0 {
                    return Err(CoreError::Storage("daemon runtime directory must already be private and owned by you".into()));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if let Some(parent) = path.parent()
                    && !parent.exists()
                {
                    ensure_runtime_dir(parent)?;
                }
                match std::fs::DirBuilder::new().mode(0o700).create(path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.into()),
                }
                return ensure_runtime_dir(path);
            }
            Err(error) => return Err(error.into()),
        }
    }
    #[cfg(not(target_os = "linux"))]
    wallet_core::paths::ensure_private_dir(path)?;
    Ok(())
}

const CONTROL_REQUEST_LIMIT: usize = 64 * 1024;
const CONTROL_REPLY_LIMIT: usize = 1024 * 1024;

async fn read_control_line(read: impl tokio::io::AsyncRead + Unpin, limit: usize) -> Result<zeroize::Zeroizing<Vec<u8>>> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};
    let mut bytes = zeroize::Zeroizing::new(Vec::with_capacity(limit + 1));
    let mut reader = tokio::io::BufReader::new(read.take((limit + 1) as u64));
    reader.read_until(b'\n', &mut bytes).await?;
    if bytes.len() > limit || bytes.last() != Some(&b'\n') {
        return Err(CoreError::Invalid("daemon control frame is too large or incomplete".into()));
    }
    Ok(bytes)
}

/// An in-flight read may be cancelled so control commands never wait behind a node timeout.
async fn work_or_control<T>(
    work: impl std::future::Future<Output = T>,
    requests: &mut tokio::sync::mpsc::Receiver<Request>,
) -> std::result::Result<T, Request> {
    tokio::select! {
        result = work => Ok(result),
        Some(request) = requests.recv() => Err(request),
    }
}

/// Public-data monitoring has its own runtime and database connections. No Session, vault or
/// signing key crosses into this thread, and a stalled wallet cannot suspend the control loop.
struct AlertWorker {
    stop: tokio::sync::watch::Sender<bool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl AlertWorker {
    fn start(paths: wallet_core::paths::Paths, network: wallet_core::network::NetworkProfile, interval: u64) -> Result<Self> {
        let (stop, mut stopped) = tokio::sync::watch::channel(false);
        let thread = std::thread::Builder::new()
            .name("wallet-alerts".into())
            .spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
                runtime.block_on(async move {
                    use futures::StreamExt;
                    let registry = wallet_core::registry::Registry::new(paths.clone());
                    while let Ok(config) = AppConfig::load(&paths) {
                        let wallets = registry.list().unwrap_or_default();
                        let round = futures::stream::iter(wallets.into_iter().map(|wallet| {
                            let (paths, network, config) = (&paths, &network, &config);
                            async move {
                                let Ok(mut data) = wallet_core::data::DataCtx::open(paths, &wallet.id, network.clone(), config) else {
                                    return;
                                };
                                let key = format!("alerts_heartbeat:{}", network.id);
                                let started = wallet_core::registry::now();
                                let prior = data
                                    .app
                                    .kv(&key)
                                    .ok()
                                    .flatten()
                                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                                    .unwrap_or_default();
                                let _ = data.app.set_kv(
                                    &key,
                                    &serde_json::json!({"attempt_at": started, "completed_at": prior["completed_at"]}).to_string(),
                                );
                                let check = async {
                                    let _ = data.use_monitor().await;
                                    wallet_core::alerts::run(&data, config.features.trading).await
                                };
                                let result = tokio::time::timeout(std::time::Duration::from_secs(10), check).await;
                                let error = match result {
                                    Ok(Ok(_)) => None,
                                    Ok(Err(e)) => Some(e.to_string()),
                                    Err(_) => Some("alert check timed out".into()),
                                };
                                // Completion is distinct from source freshness: the market observation
                                // envelope controls whether a price alert is eligible to fire.
                                let _ = data.app.set_kv(
                                    &key,
                                    &serde_json::json!({"attempt_at": started,
                                "completed_at": wallet_core::registry::now(), "error": error})
                                    .to_string(),
                                );
                            }
                        }))
                        .buffer_unordered(4)
                        .collect::<Vec<_>>();
                        tokio::select! { _ = round => {}, _ = stopped.changed() => break }
                        tokio::select! {
                            _ = tokio::time::sleep(std::time::Duration::from_secs(interval.max(5))) => {},
                            _ = stopped.changed() => break,
                        }
                    }
                });
            })
            .map_err(|e| CoreError::Storage(format!("alert worker: {e}")))?;
        Ok(Self { stop, thread: Some(thread) })
    }
}

impl Drop for AlertWorker {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Exclusive daemon lock, held by the kernel for the process lifetime.
///
/// This used to be a pid file: read the pid, look for `/proc/<pid>`, write ours. That races two
/// daemons starting together, survives a SIGKILL as a lock nobody holds, and trusts that a pid has
/// not been reused. An advisory lock on an open file has none of those problems — the kernel
/// releases it when the process ends, however it ends.
struct LockFile {
    /// Held open for as long as the daemon runs: closing it is what releases the lock.
    file: std::fs::File,
}

impl LockFile {
    fn acquire(path: std::path::PathBuf) -> Result<Self> {
        if let Some(dir) = path.parent() {
            ensure_runtime_dir(dir)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).read(true).write(true).truncate(false);
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
        }
        let file = options.open(&path)?;
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = file.metadata()?;
            if !metadata.is_file() || metadata.uid() != own_uid() || metadata.nlink() != 1 {
                return Err(CoreError::Storage("daemon lock must be an owned regular file with one link".into()));
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
        }
        match file.try_lock() {
            Ok(()) => {}
            Err(_) => return Err(CoreError::Storage("a daemon is already running for this data directory".into())),
        }
        // The pid is written for a human reading the file; the lock is what enforces exclusivity.
        use std::io::Write;
        let mut file = file;
        let _ = file.set_len(0);
        let _ = write!(file, "{}", std::process::id());
        let _ = file.flush();
        Ok(Self { file })
    }
}

impl Drop for LockFile {
    fn drop(&mut self) {
        // The kernel would do this on exit anyway; doing it here covers a daemon that stops while
        // the process carries on, and leaves the file empty rather than naming a pid that is gone.
        let _ = self.file.set_len(0);
        let _ = self.file.unlock();
    }
}

/// One wallet the daemon watches: its own session, locked unless someone handed it a password.
struct Watched {
    session: wallet_core::session::Session,
    /// The newest notification already sent to the desktop.
    last_notice: i64,
    last_payment_sync: Option<std::time::Instant>,
    /// Channel notifications already handled by [`ChannelNotices`], so they are not sent again
    /// with the rest.
    quiet: std::collections::HashSet<i64>,
    /// When the monitoring node was last tried, while it is not in use.
    monitor_checked: Option<std::time::Instant>,
    /// When this wallet's limit orders were last re-checked.
    orders_checked: Option<std::time::Instant>,
}

/// How often the daemon re-checks a wallet's limit orders: the open terminal's pace.
const ORDER_CHECK: std::time::Duration = std::time::Duration::from_secs(30);

/// Re-check this wallet's active limit orders against fresh quotes. Reading only: the daemon
/// never signs an order, locked or not. One found reachable for the first time writes a
/// notification (`orders::observe`), which `notify_desktop` then puts on the desktop; the open
/// terminal, if it saw it first, has already said so and this stays quiet.
async fn check_orders(session: &mut wallet_core::session::Session) -> Vec<String> {
    let name = session.meta.name.clone();
    let plans = match wallet_core::orders::list(session) {
        Ok(plans) => plans,
        Err(e) => {
            eprintln!("[{name}] orders: {e}");
            return Vec::new();
        }
    };
    let mut announced = Vec::new();
    for plan in plans {
        if !wallet_core::orders::details(&plan).is_ok_and(|v| v.state.active()) {
            continue;
        }
        match wallet_core::orders::observe(session, &plan.id).await {
            Ok(seen) if seen.announced => announced.push(plan.id),
            Ok(_) => {}
            // Another process holding the order's lease, a quote that failed: next poll.
            Err(e) => eprintln!("[{name}] order {}: {e}", &plan.id[..8.min(plan.id.len())]),
        }
    }
    announced
}

/// Channel posts already put on the desktop. A public channel reads the same from every wallet,
/// so several wallets subscribed to it would otherwise each announce the same post; this lets
/// one of them do it. Posts are known by id, not time, so a wallet that looks a poll later
/// than another still finds them announced.
#[derive(Default)]
struct ChannelNotices {
    /// Per channel, the ids of the posts announced, oldest first.
    announced: std::collections::HashMap<String, std::collections::VecDeque<String>>,
}

impl ChannelNotices {
    /// Posts remembered per channel: far more than a board window holds between two polls.
    const KEEP: usize = 512;

    /// The desktop notification for a channel's news: the posts nobody has announced yet and
    /// no wallet on this computer wrote. None when nothing is left to say.
    fn take(&mut self, news: &wallet_core::chat::ChatNews, own: &[String]) -> Option<String> {
        let announced = self.announced.entry(news.target.clone()).or_default();
        let fresh: Vec<wallet_core::chat::ChatLine> =
            news.fresh.iter().filter(|l| !own.contains(&l.from) && !announced.contains(&l.id)).cloned().collect();
        for l in &fresh {
            announced.push_back(l.id.clone());
        }
        while announced.len() > Self::KEEP {
            announced.pop_front();
        }
        wallet_core::chat::summarize(&fresh)
    }
}

impl Watched {
    fn open(ctx: &Ctx, meta: wallet_core::registry::WalletMeta, network: &wallet_core::network::NetworkProfile) -> Result<Self> {
        let session = wallet_core::session::Session::open(ctx.registry.clone(), ctx.config.clone(), meta, network.clone())?;
        let last_notice = session.app.notifications(1)?.first().map_or(0, |n| n.id);
        Ok(Self { session, last_notice, last_payment_sync: None, quiet: Default::default(), monitor_checked: None, orders_checked: None })
    }

    /// Every read goes to the monitoring node when one is configured and checks out; it is tried
    /// again every few minutes while it does not. Nothing the daemon does broadcasts.
    async fn check_monitor(&mut self) {
        let session = &mut self.session;
        if session.network.monitor.is_some() && !session.monitoring() && self.monitor_checked.is_none_or(|t| t.elapsed().as_secs() >= 180) {
            self.monitor_checked = Some(std::time::Instant::now());
            if let Some(why) = session.use_monitor().await {
                eprintln!("[{}] {why}; reading from the RPC endpoint", session.meta.name);
            }
        }
    }

    /// One poll for this wallet: Qi, private payments (unlocked), chats, tracking, the dashboard
    /// cache and alerts. Returns the news of its subscribed channels,
    /// which the daemon announces once for every wallet. Chats are read only with messaging on,
    /// and pair alerts only with trading on.
    async fn tick(&mut self, ctx: &Ctx, features: Features) -> Vec<wallet_core::chat::ChatNews> {
        self.check_monitor().await;
        let session = &mut self.session;
        let name = session.meta.name.clone();
        if (session.meta.qi_xpub.is_some() || !session.meta.qi_imported.is_empty())
            && let Err(e) = session.refresh_qi().await
        {
            eprintln!("[{name}] qi refresh: {e}");
        }
        // Discover private payments from new senders (mailbox) and rescan known channels.
        if session.is_unlocked() && self.last_payment_sync.is_none_or(|t| t.elapsed().as_secs() >= 90) {
            self.last_payment_sync = Some(std::time::Instant::now());
            match session.sync_payment_channels().await {
                Ok(sync) => {
                    // Offers only: nothing an announcement says is registered without the user.
                    for offer in &sync.new_offers {
                        let (title, body) = offer.notice();
                        let _ = session.app.notify("info", &title, &body);
                    }
                }
                Err(e) => eprintln!("[{name}] payment sync: {e}"),
            }
        }
        // Subscribed chats: channels always (public reads), private conversations only while
        // unlocked. Each chat that has news becomes one notification saying who said what.
        let mut channels = Vec::new();
        if features.messaging {
            match session.chat_news().await {
                Ok(news) => channels.extend(news.into_iter().filter(|n| n.is_channel())),
                Err(e) => eprintln!("[{name}] chats: {e}"),
            }
        }
        match session.track().await {
            Ok(report) => {
                for e in report.errors {
                    eprintln!("[{name}] track: {e}");
                }
            }
            Err(e) => eprintln!("[{name}] track: {e}"),
        }
        // Balances one poll old for whenever this wallet is opened. Public reads, no keys.
        if let Err(e) = session.warm_dashboard_cache().await {
            eprintln!("[{name}] dashboard cache: {e}");
        }
        if features.trading && self.orders_checked.is_none_or(|t| t.elapsed() >= ORDER_CHECK) {
            self.orders_checked = Some(std::time::Instant::now());
            check_orders(&mut self.session).await;
        }
        let _ = ctx;
        channels
    }

    /// Put this wallet's channel news on the desktop, minus what another wallet already did.
    /// Every wallet keeps its own copy in its notification list.
    fn announce_channels(&mut self, ctx: &Ctx, news: Vec<wallet_core::chat::ChatNews>, notices: &mut ChannelNotices, own: &[String]) {
        for n in news {
            if let Some(id) = n.notice {
                self.quiet.insert(id);
            }
            if let Some(body) = notices.take(&n, own)
                && ctx.config.notifications
            {
                // A channel is the same whichever wallet follows it, so it is not named by wallet.
                notify::desktop(&n.title, &body);
            }
        }
    }

    /// Send this wallet's new notifications to the desktop, named by wallet when there are
    /// several.
    fn notify_desktop(&mut self, ctx: &Ctx, several: bool) {
        if !ctx.config.notifications {
            return;
        }
        let Ok(list) = self.session.app.notifications(20) else { return };
        let threshold = self.last_notice;
        for n in list.iter().rev().filter(|n| n.id > threshold) {
            self.last_notice = self.last_notice.max(n.id);
            if self.quiet.remove(&n.id) {
                continue;
            }
            let title = if several { format!("{} · {}", self.session.meta.name, n.title) } else { n.title.clone() };
            notify::desktop(&title, &n.body);
        }
    }
}

/// What the running daemon says about itself, in the runtime directory. No amounts, no keys.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default)]
pub struct DaemonState {
    pub pid: u32,
    pub since: u64,
    pub interval: u64,
    pub network: String,
    /// (wallet id, name, unlocked)
    pub wallets: Vec<(String, String, bool)>,
    /// Which program it is running ([`build_id`]), so a newer one can take over.
    #[serde(default)]
    pub build: String,
}

/// This program, as a file: version, path, size and modification time. A rebuild or an upgrade
/// changes it, which is how an open terminal knows the daemon runs older code.
pub fn build_id() -> String {
    quai_engine::protocol::build()
}

/// A daemon is being started or replaced on a background thread (`ensure_current_soon`).
pub static STARTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `ensure_current` off the caller's thread. Replacing an older daemon waits for it to stop
/// (up to 10 s), which must not hold the first frame back; a wallet unlocked meanwhile waits for
/// `STARTING` to clear before it is handed over (`App::hand_to_daemon`).
pub fn ensure_current_soon(paths: wallet_core::paths::Paths, interval: u64) {
    use std::sync::atomic::Ordering;
    STARTING.store(true, Ordering::SeqCst);
    let started = std::thread::Builder::new().name("daemon-start".into()).spawn(move || {
        match ensure_current(&paths, interval) {
            Ok(what) => wallet_core::diag::mark(&format!("daemon.autostart {what}")),
            Err(e) => wallet_core::diag::mark(&format!("daemon.autostart_failed {e}")),
        }
        STARTING.store(false, Ordering::SeqCst);
    });
    if started.is_err() {
        STARTING.store(false, Ordering::SeqCst);
    }
}

/// Make sure a daemon is running this program: start one if none is, and replace one running
/// an older build (after an upgrade or rebuild). Wallets it held unlocked are locked by the
/// restart; the terminal hands them over again as they are unlocked. Returns what happened.
pub fn ensure_current(paths: &wallet_core::paths::Paths, interval: u64) -> Result<&'static str> {
    let build = build_id();
    match state(paths) {
        Some(s) if s.build == build => Ok("running"),
        Some(_) => {
            let _ = request(paths, &serde_json::json!({"cmd": "stop"}));
            for _ in 0..200 {
                if !daemon_running(paths) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            if daemon_running(paths) {
                return Ok("busy");
            }
            spawn_background(paths, interval)?;
            Ok("replaced")
        }
        None => {
            spawn_background(paths, interval)?;
            Ok("started")
        }
    }
}

impl DaemonState {
    /// Whether the daemon holds this wallet unlocked (it then reads its sealed chats itself).
    pub fn unlocked(&self, wallet: &str) -> bool {
        self.wallets.iter().any(|(id, _, u)| id == wallet && *u)
    }
}

/// The running daemon's state, if one is running.
pub fn state(paths: &wallet_core::paths::Paths) -> Option<DaemonState> {
    if !daemon_running(paths) {
        return None;
    }
    serde_json::from_str(&std::fs::read_to_string(paths.daemon_state()).ok()?).ok()
}

/// A request on the control socket, and where its answer goes.
#[derive(Debug)]
struct Request {
    body: serde_json::Value,
    reply: Option<tokio::sync::oneshot::Sender<serde_json::Value>>,
}

impl Drop for Request {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        if let Some(serde_json::Value::String(password)) = self.body.get_mut("password") {
            password.zeroize();
        }
    }
}

/// Bind a socket only this user can reach: in the private runtime directory, mode 0600,
/// replacing a stale socket of ours but never anything else.
fn bind_private(path: &std::path::Path) -> Result<std::os::unix::net::UnixListener> {
    if let Some(dir) = path.parent() {
        ensure_runtime_dir(dir)?;
    }
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        if !metadata.file_type().is_socket() || metadata.uid() != own_uid() {
            return Err(CoreError::Storage("refusing to replace a non-socket daemon runtime path".into()));
        }
        std::fs::remove_file(path)?;
    }
    let listener = std::os::unix::net::UnixListener::bind(path).map_err(|e| CoreError::Storage(format!("socket: {e}")))?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(listener)
}

/// Accept control connections: one JSON line in, one JSON line out. The socket is the user's own
/// (0600, in their private runtime directory), which is what lets a password travel over it.
fn serve_control(path: std::path::PathBuf, tx: tokio::sync::mpsc::Sender<Request>) -> Result<()> {
    let listener = bind_private(&path)?;
    listener.set_nonblocking(true)?;
    let listener = tokio::net::UnixListener::from_std(listener).map_err(|e| CoreError::Storage(format!("control socket: {e}")))?;
    tokio::spawn(async move {
        let connections = std::sync::Arc::new(tokio::sync::Semaphore::new(16));
        while let Ok((stream, _)) = listener.accept().await {
            let Ok(permit) = connections.clone().try_acquire_owned() else { continue };
            // Only this user: the socket's mode says so already, and the kernel's word on who
            // connected makes sure of it.
            if !stream.peer_cred().is_ok_and(|c| c.uid() == own_uid()) {
                continue;
            }
            let tx = tx.clone();
            tokio::spawn(async move {
                let _permit = permit;
                use tokio::io::AsyncWriteExt;
                let (read, mut write) = stream.into_split();
                let Ok(Ok(line)) =
                    tokio::time::timeout(std::time::Duration::from_secs(5), read_control_line(read, CONTROL_REQUEST_LIMIT)).await
                else {
                    return;
                };
                let Ok(body) = serde_json::from_slice::<serde_json::Value>(&line) else { return };
                drop(line);
                let (reply, answer) = tokio::sync::oneshot::channel();
                let request = Request { body, reply: Some(reply) };
                let exchange = async {
                    if tx.send(request).await.is_err() {
                        return;
                    }
                    if let Ok(out) = answer.await {
                        let _ = write.write_all(format!("{out}\n").as_bytes()).await;
                    }
                };
                let _ = tokio::time::timeout(std::time::Duration::from_secs(60), exchange).await;
            });
        }
    });
    Ok(())
}

/// Poll every wallet on this computer: monitoring, notifications, the status file, alerts and
/// chats for all of them, and private payments and sealed chats for those holding keys. `locked` never asks for a
/// password; otherwise each wallet that can sign is offered a prompt (empty skips it). Keys can
/// also arrive later over the control socket (`daemon unlock`).
pub async fn run(ctx: &Ctx, interval: u64, locked: bool, detached: bool) -> Result<()> {
    // Polling is background work: it keeps headroom in shared API limits for interactive use.
    wallet_core::http::set_background_process(true);
    let _lock = LockFile::acquire(ctx.paths.daemon_lock())?;
    // A configuration read, nothing on the network: a daemon that cannot run is refused before it
    // binds a socket terminals would find and get nothing from.
    let network = ctx.network()?;
    let build = build_id();
    let since = wallet_core::registry::now();
    // Terminals attach before anything slow happens here (a node check can take seconds): the
    // engine they get does not wait for this loop, and neither does the first frame they draw.
    let early = DaemonState { pid: std::process::id(), since, interval, build: build.clone(), ..DaemonState::default() };
    let _ = wallet_vault::write_private_atomic(&ctx.paths.daemon_state(), serde_json::to_string(&early).unwrap_or_default().as_bytes());
    let _engines = quai_engine::server::serve(bind_private(&ctx.paths.engine_socket())?, ctx.registry.clone(), build.clone())
        .map_err(|e| CoreError::Storage(format!("engine server: {e}")))?;
    let mut watched: Vec<Watched> = Vec::new();
    for meta in ctx.registry.list()? {
        match Watched::open(ctx, meta, &network) {
            Ok(w) => watched.push(w),
            Err(e) => eprintln!("open wallet: {e}"),
        }
    }
    // Onto the monitoring node before the first read, so start-up reads use it too.
    for w in &mut watched {
        w.check_monitor().await;
    }
    // The node is checked before anything is read from it, but a node that is down (or a
    // network that is off) must not take the daemon down: terminals' engines run here. Checked
    // again at each poll until it answers.
    let mut verified = false;
    if !locked && !detached {
        for w in watched.iter_mut().filter(|w| w.session.meta.can_sign()) {
            let prompt_text = format!("Password for `{}` (enter to leave it locked)", w.session.meta.name);
            match prompt::password(None, &prompt_text) {
                Ok(p) if !p.is_empty() => {
                    if let Err(e) = w.session.unlock(&p) {
                        eprintln!("{}: {e}; it stays locked", w.session.meta.name);
                    }
                }
                _ => {}
            }
        }
    }
    let (tx, mut requests) = tokio::sync::mpsc::channel::<Request>(8);
    serve_control(ctx.paths.daemon_socket(), tx)?;
    let _alerts = AlertWorker::start(ctx.paths.clone(), network.clone(), interval)?;
    let write_state = |watched: &[Watched]| {
        let state = DaemonState {
            pid: std::process::id(),
            since,
            interval,
            network: network.id.clone(),
            wallets: watched.iter().map(|w| (w.session.meta.id.clone(), w.session.meta.name.clone(), w.session.is_unlocked())).collect(),
            build: build.clone(),
        };
        let _ = wallet_vault::write_private_atomic(&ctx.paths.daemon_state(), serde_json::to_string(&state).unwrap_or_default().as_bytes());
    };
    write_state(&watched);
    eprintln!(
        "quai-terminal daemon: {} on {} · {} unlocked · polling every {interval}s{}",
        wallet_core::amount::count(watched.len(), "wallet"),
        network.id,
        watched.iter().filter(|w| w.session.is_unlocked()).count(),
        if detached { "" } else { " (Ctrl-C to stop)" }
    );
    let interval = std::time::Duration::from_secs(interval.max(5));
    let stop = shutdown_signal(detached);
    tokio::pin!(stop);
    let mut stopping = false;
    let mut channel_notices = ChannelNotices::default();
    'poll: loop {
        // New wallets join without a restart; removed ones drop out.
        if let Ok(list) = ctx.registry.list() {
            watched.retain(|w| list.iter().any(|m| m.id == w.session.meta.id));
            for meta in list {
                if !watched.iter().any(|w| w.session.meta.id == meta.id)
                    && let Ok(w) = Watched::open(ctx, meta, &network)
                {
                    watched.push(w);
                }
            }
        }
        if !verified {
            let check = match watched.first() {
                Some(first) => match tokio::time::timeout(std::time::Duration::from_secs(15), first.session.verify_node()).await {
                    Ok(result) => result.map_err(|e| e.to_string()),
                    Err(_) => Err("the node did not answer".into()),
                },
                None => Ok(()),
            };
            match check {
                Ok(()) => verified = true,
                Err(e) => eprintln!("node check: {e}; nothing is read from it until it passes"),
            }
        }
        if verified {
            let several = watched.len() > 1;
            // Keys held longer than the configured lifetime are dropped: the daemon runs unattended,
            // and a recovery phrase should not sit in it indefinitely.
            let lifetime = ctx.config.daemon_unlock_hours.max(1) * 3600;
            for w in watched.iter_mut().filter(|w| w.session.is_unlocked()) {
                if wallet_core::registry::now().saturating_sub(w.session.unlocked_at()) >= lifetime {
                    w.session.lock();
                    eprintln!("[{}] locked after {} h unlocked", w.session.meta.name, ctx.config.daemon_unlock_hours);
                }
            }
            // Posts from any wallet here are the user's own, whichever wallet reads them.
            let own: Vec<String> = watched.iter().flat_map(|w| w.session.meta.quai_owner_addresses()).map(|a| a.to_lowercase()).collect();
            // Switched in Settings while this runs: read every poll, so turning a feature off stops
            // its monitoring without a restart.
            let features = AppConfig::load(&ctx.paths).map_or(ctx.config.features, |c| c.features);
            if let Some(first) = watched.first() {
                let result = tokio::select! {
                    result = work_or_control(warm_shared_feeds(&first.session, features), &mut requests) => result,
                    _ = &mut stop => break 'poll,
                };
                if let Err(request) = result {
                    if handle(ctx, &mut watched, request) {
                        break 'poll;
                    }
                    write_state(&watched);
                    continue 'poll;
                }
            }
            for i in 0..watched.len() {
                let result = tokio::select! {
                    result = work_or_control(watched[i].tick(ctx, features), &mut requests) => result,
                    _ = &mut stop => { stopping = true; break 'poll; }
                };
                let channels = match result {
                    Ok(channels) => channels,
                    Err(request) => {
                        if handle(ctx, &mut watched, request) {
                            break 'poll;
                        }
                        write_state(&watched);
                        continue 'poll;
                    }
                };
                watched[i].announce_channels(ctx, channels, &mut channel_notices, &own);
                watched[i].notify_desktop(ctx, several);
                // Answer anyone waiting between wallets, so an unlock does not wait out a whole poll.
                let mut answered = false;
                while let Ok(req) = requests.try_recv() {
                    stopping |= handle(ctx, &mut watched, req);
                    answered = true;
                }
                if answered {
                    write_state(&watched);
                }
                if stopping {
                    break 'poll;
                }
            }
            // The status bar shows the default wallet (or the first).
            let shown = watched
                .iter()
                .position(|w| ctx.config.default_wallet.as_deref().is_some_and(|d| d == w.session.meta.id || d == w.session.meta.name))
                .or((!watched.is_empty()).then_some(0));
            if let Some(w) = shown.map(|i| &mut watched[i]) {
                let result = tokio::select! {
                    result = work_or_control(w.session.public_status(ctx.config.show_amounts_in_notifications), &mut requests) => result,
                    _ = &mut stop => break 'poll,
                };
                let status = match result {
                    Ok(status) => status,
                    Err(request) => {
                        if handle(ctx, &mut watched, request) {
                            break 'poll;
                        }
                        write_state(&watched);
                        continue 'poll;
                    }
                };
                if let Err(e) = extras::write_status(&ctx.paths, &status) {
                    eprintln!("status: {e}");
                }
            }
        }
        write_state(&watched);
        let sleep = tokio::time::sleep(interval);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                _ = &mut sleep => break,
                _ = &mut stop => break 'poll,
                Some(req) = requests.recv() => {
                    if handle(ctx, &mut watched, req) {
                        break 'poll;
                    }
                    write_state(&watched);
                }
            }
        }
    }
    let _ = stopping;
    for w in &mut watched {
        w.session.lock();
    }
    if let Some(w) = watched.first_mut() {
        let mut status = w.session.public_status(false).await;
        status.unlocked = false;
        let _ = extras::write_status(&ctx.paths, &status);
    }
    let _ = std::fs::remove_file(ctx.paths.daemon_socket());
    let _ = std::fs::remove_file(ctx.paths.engine_socket());
    let _ = std::fs::remove_file(ctx.paths.daemon_state());
    eprintln!("daemon stopped; wallets locked");
    Ok(())
}

/// Answer one control request. Returns true when it asked the daemon to stop.
fn handle(ctx: &Ctx, watched: &mut [Watched], mut req: Request) -> bool {
    let body = std::mem::take(&mut req.body);
    let cmd = body["cmd"].as_str().unwrap_or_default();
    let find = |watched: &mut [Watched], wallet: &str| -> Option<usize> {
        watched.iter().position(|w| w.session.meta.id == wallet || w.session.meta.name.eq_ignore_ascii_case(wallet))
    };
    let answer = match cmd {
        "stop" => {
            if let Some(reply) = req.reply.take() {
                let _ = reply.send(serde_json::json!({"ok": true}));
            }
            return true;
        }
        "status" => serde_json::json!({
            "ok": true,
            "wallets": watched.iter().map(|w| serde_json::json!({"id": w.session.meta.id, "name": w.session.meta.name, "unlocked": w.session.is_unlocked()})).collect::<Vec<_>>(),
        }),
        "unlock" => {
            let mut body = body;
            let wallet = body["wallet"].as_str().unwrap_or_default().to_string();
            let wallet = wallet.as_str();
            // Taken out of the request, so the only copy is one that is wiped when it goes.
            let password = zeroize::Zeroizing::new(match body["password"].take() {
                serde_json::Value::String(p) => p,
                _ => String::new(),
            });
            match find(watched, wallet) {
                None => serde_json::json!({"ok": false, "error": format!("the daemon is not watching `{wallet}`")}),
                Some(i) => match watched[i].session.unlock(&password) {
                    Ok(()) => serde_json::json!({"ok": true, "name": watched[i].session.meta.name}),
                    Err(e) => serde_json::json!({"ok": false, "error": e.to_string()}),
                },
            }
        }
        "lock" => {
            let wallet = body["wallet"].as_str();
            for w in
                watched.iter_mut().filter(|w| wallet.is_none_or(|x| w.session.meta.id == x || w.session.meta.name.eq_ignore_ascii_case(x)))
            {
                w.session.lock();
            }
            serde_json::json!({"ok": true})
        }
        other => serde_json::json!({"ok": false, "error": format!("unknown request `{other}`")}),
    };
    let _ = ctx;
    if let Some(reply) = req.reply.take() {
        let _ = reply.send(answer);
    }
    false
}

// ------------------------------------------------------------------------- the client side

/// This process's real user id.
pub fn own_uid() -> u32 {
    // From the kernel, not `/proc/self`: a process marked non-dumpable ([`protect_memory`]) has
    // its `/proc` entries owned by root, so reading the owner there would say uid 0.
    #[cfg(unix)]
    {
        rustix::process::getuid().as_raw()
    }
    #[cfg(not(unix))]
    {
        u32::MAX
    }
}

/// Hand the running daemon a wallet's password, so it holds that wallet unlocked too.
///
/// Nothing is sent unless every check passes: the socket and the directory it sits in belong to
/// this user and nobody else can reach them; the process at the other end runs as this user
/// (asked of the kernel, `SO_PEERCRED`) and is the one holding the daemon's lock. The request is
/// built in one buffer sized up front — so it never reallocates and leaves a copy behind — and
/// wiped once written; the daemon takes the password out of the request into a buffer it wipes.
/// Returns the wallet name the daemon unlocked.
pub async fn hand_unlock(paths: &wallet_core::paths::Paths, wallet: &str, password: &str) -> Result<String> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        use tokio::io::AsyncWriteExt;
        let refuse = |why: &str| CoreError::Invalid(format!("not handing the password to the daemon: {why}"));
        let state = state(paths).ok_or_else(|| refuse("no daemon is running"))?;
        let socket = paths.daemon_socket();
        let me = own_uid();
        let dir = socket.parent().ok_or_else(|| refuse("no runtime directory"))?;
        let dir_meta = std::fs::symlink_metadata(dir).map_err(|_| refuse("its directory is missing"))?;
        if !dir_meta.is_dir() || dir_meta.uid() != me || dir_meta.mode() & 0o077 != 0 {
            return Err(refuse("its directory is not private to you"));
        }
        let meta = std::fs::symlink_metadata(&socket).map_err(|_| refuse("its socket is missing"))?;
        if !meta.file_type().is_socket() || meta.uid() != me || meta.mode() & 0o077 != 0 {
            return Err(refuse("its socket is not private to you"));
        }
        let stream = tokio::net::UnixStream::connect(&socket).await.map_err(|_| refuse("it did not answer"))?;
        let cred = stream.peer_cred().map_err(|_| refuse("the kernel would not say who is listening"))?;
        if cred.uid() != me {
            return Err(refuse("the other end is not you"));
        }
        if cred.pid().map(|p| p as u32) != Some(state.pid) || !daemon_running(paths) {
            return Err(refuse("the other end is not the daemon holding the lock"));
        }
        let wallet_json = serde_json::to_string(wallet).unwrap_or_default();
        let password_json = zeroize::Zeroizing::new(serde_json::to_string(password).unwrap_or_default());
        let mut line = zeroize::Zeroizing::new(String::with_capacity(64 + wallet_json.len() + password_json.len()));
        line.push_str("{\"cmd\":\"unlock\",\"wallet\":");
        line.push_str(&wallet_json);
        line.push_str(",\"password\":");
        line.push_str(&password_json);
        line.push_str("}\n");
        drop(password_json);
        let (read, mut write) = stream.into_split();
        write.write_all(line.as_bytes()).await?;
        drop(line);
        let answer = tokio::time::timeout(std::time::Duration::from_secs(60), read_control_line(read, CONTROL_REPLY_LIMIT))
            .await
            .map_err(|_| CoreError::Network("the daemon did not answer in time; it may be mid-poll".into()))??;
        let answer: serde_json::Value =
            serde_json::from_slice(&answer).map_err(|_| CoreError::Network("the daemon did not answer; try again".into()))?;
        if answer["ok"].as_bool() != Some(true) {
            return Err(CoreError::Invalid(answer["error"].as_str().unwrap_or("the daemon refused").to_string()));
        }
        Ok(answer["name"].as_str().unwrap_or(wallet).to_string())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (paths, wallet, password);
        Err(CoreError::Invalid(
            "not handing the password to the daemon: it checks the peer with SO_PEERCRED, which this system lacks; use `daemon run` in a terminal"
                .into(),
        ))
    }
}

/// Ask the running daemon something over its socket.
/// Lock one wallet in the running daemon, if it holds it. The terminal calls this when the user
/// locks, so "locked" on screen means locked everywhere.
pub fn lock_wallet(paths: &wallet_core::paths::Paths, wallet: &str) -> Result<()> {
    request(paths, &serde_json::json!({"cmd": "lock", "wallet": wallet})).map(|_| ())
}

/// Keep other processes of this user from reading this one's memory (`/proc/PID/mem`, ptrace
/// attach) or getting a core dump of it. Both processes can hold a wallet's keys.
pub fn protect_memory() {
    #[cfg(target_os = "linux")]
    let _ = rustix::process::set_dumpable_behavior(rustix::process::DumpableBehavior::NotDumpable);
}

fn request(paths: &wallet_core::paths::Paths, body: &serde_json::Value) -> Result<serde_json::Value> {
    use std::io::{BufRead, Read, Write};
    let mut stream = std::os::unix::net::UnixStream::connect(paths.daemon_socket())
        .map_err(|_| CoreError::NotFound("the daemon is not running (quai-terminal daemon start)".into()))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(60)))?;
    let line = zeroize::Zeroizing::new(format!("{body}\n"));
    stream.write_all(line.as_bytes())?;
    let mut answer = String::new();
    std::io::BufReader::new(stream.take((CONTROL_REPLY_LIMIT + 1) as u64)).read_line(&mut answer)?;
    if answer.len() > CONTROL_REPLY_LIMIT || !answer.ends_with('\n') {
        return Err(CoreError::Network("daemon reply is too large or incomplete".into()));
    }
    let answer: serde_json::Value =
        serde_json::from_str(&answer).map_err(|_| CoreError::Network("the daemon did not answer; it may be mid-poll, try again".into()))?;
    if answer["ok"].as_bool() != Some(true) {
        return Err(CoreError::Invalid(answer["error"].as_str().unwrap_or("the daemon refused").to_string()));
    }
    Ok(answer)
}

/// Start the daemon in the background (locked; `daemon unlock` hands it keys). Returns false
/// when one was already running.
pub fn spawn_background(paths: &wallet_core::paths::Paths, interval: u64) -> Result<bool> {
    if daemon_running(paths) {
        return Ok(false);
    }
    let exe = std::env::current_exe()?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(paths.daemon_log())?;
    let mut command = std::process::Command::new(exe);
    command
        .args(["daemon", "run", "--locked", "--detached", "--interval", &interval.to_string()])
        .env("QUAI_TERMINAL_HOME", paths.root())
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    // Its own process group: closing the terminal, or Ctrl-C in it, does not reach the daemon.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command.spawn()?;
    // Wait for it to take the lock, so what follows (a status, an unlock) finds it.
    for _ in 0..100 {
        if daemon_running(paths) && paths.engine_socket().exists() {
            return Ok(true);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Err(CoreError::Storage(format!("the daemon did not start; see {}", paths.daemon_log().display())))
}

pub fn start(ctx: &Ctx, interval: u64) -> Result<()> {
    let n = ctx.registry.list()?.len();
    match ensure_current(&ctx.paths, interval)? {
        "started" => {
            println!("{} daemon started in the background, watching {}", ctx.out.green("✓"), wallet_core::amount::count(n, "wallet"))
        }
        "replaced" => {
            println!("{} daemon restarted on this version, watching {}", ctx.out.green("✓"), wallet_core::amount::count(n, "wallet"))
        }
        "busy" => println!("the daemon is finishing a poll; run this again in a moment"),
        _ => println!("the daemon is already running"),
    }
    println!("  it unlocks each wallet you unlock in the terminal · log: {}", ctx.paths.daemon_log().display());
    println!("  stop: quai-terminal daemon stop");
    Ok(())
}

pub fn stop(ctx: &Ctx) -> Result<()> {
    if !daemon_running(&ctx.paths) {
        println!("the daemon is not running");
        return Ok(());
    }
    // Politely first; a daemon stuck mid-request gets SIGTERM from its pid.
    if request(&ctx.paths, &serde_json::json!({"cmd": "stop"})).is_err()
        && let Some(pid) = state(&ctx.paths).map(|s| s.pid)
    {
        let _ = std::process::Command::new("kill").arg(pid.to_string()).status();
    }
    for _ in 0..200 {
        if !daemon_running(&ctx.paths) {
            println!("{} daemon stopped; its wallets are locked", ctx.out.green("✓"));
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Err(CoreError::Storage("the daemon is still finishing a poll; try again in a moment".into()))
}

pub fn status(ctx: &Ctx) -> Result<()> {
    let Some(s) = state(&ctx.paths) else {
        if ctx.out.json() {
            ctx.out.emit("daemon status", &serde_json::json!({"running": false}));
        } else {
            println!("the daemon is not running · quai-terminal daemon start");
        }
        return Ok(());
    };
    if ctx.out.json() {
        ctx.out.emit("daemon status", &serde_json::json!({"running": true, "state": s}));
        return Ok(());
    }
    let up = wallet_core::registry::now().saturating_sub(s.since);
    println!("running · pid {} · {} · every {}s · up {}", s.pid, s.network, s.interval, wallet_core::track::human_duration(up.max(1)));
    for (_, name, unlocked) in &s.wallets {
        println!("  {} {name}", if *unlocked { ctx.out.green("● unlocked") } else { "○ locked  ".to_string() });
    }
    println!("log: {}", ctx.paths.daemon_log().display());
    Ok(())
}

/// Hand the daemon a wallet's password, so it reads that wallet's private payments and sealed
/// chats. Each wallet is asked for in turn (enter skips); `-w` names one.
pub async fn unlock(ctx: &Ctx, all: bool) -> Result<()> {
    if !daemon_running(&ctx.paths) {
        return Err(CoreError::NotFound("the daemon is not running (quai-terminal daemon start)".into()));
    }
    let targets: Vec<wallet_core::registry::WalletMeta> = match (&ctx.global.wallet, all) {
        (Some(_), false) => vec![ctx.meta()?],
        _ => ctx.registry.list()?.into_iter().filter(|m| m.can_sign()).collect(),
    };
    let held = state(&ctx.paths).unwrap_or_default();
    for meta in targets {
        if held.unlocked(&meta.id) {
            println!("{} is already unlocked in the daemon", meta.name);
            continue;
        }
        let password = prompt::password(ctx.global.password_fd, &format!("Password for `{}` (enter to skip)", meta.name))?;
        if password.is_empty() {
            continue;
        }
        match hand_unlock(&ctx.paths, &meta.id, &password).await {
            Ok(name) => println!("{} {name} unlocked in the daemon", ctx.out.green("✓")),
            Err(e) => eprintln!("{}: {e}", meta.name),
        }
    }
    Ok(())
}

pub fn lock(ctx: &Ctx) -> Result<()> {
    let wallet = ctx.global.wallet.clone();
    request(&ctx.paths, &serde_json::json!({"cmd": "lock", "wallet": wallet}))?;
    println!("{} {} locked in the daemon", ctx.out.green("✓"), wallet.as_deref().unwrap_or("every wallet"));
    Ok(())
}

/// Keep the shared display cache current, so a wallet opened while the daemon runs finds the
/// market data already there instead of fetching it itself.
///
/// These are the feeds that are the same for every wallet, and they live once per data directory
/// (`Paths::shared_cache`), so this is one process refreshing them for all of them. Each read goes
/// through its own freshness window, so most polls fetch nothing at all; the daemon marks its
/// requests background, so a busy budget parks this before it parks anything a user is waiting on.
/// Failures are silent: nothing here is needed for the daemon's own work. Markets are warmed only
/// with trading on, and listings only with NFTs on.
async fn warm_shared_feeds(session: &wallet_core::session::Session, features: Features) {
    let Ok(data) = session.data_ctx() else { return };
    if !data.policy.market {
        return;
    }
    let _ = data.cached("prices", 60, || data.explorer.prices()).await;
    if features.trading {
        let _ = data.cached("token_markets", 300, || data.explorer.token_markets()).await;
        let _ = wallet_core::markets::all_markets(&data).await;
    }
    // The listings grid looks up the explorer's metadata for the rows it shows, and that is the
    // single biggest thing a wallet fetches when it opens. Held for a day, and the same rows for
    // everybody, so the daemon is the right place to pay for it once.
    if features.nfts
        && let Ok(listings) = wallet_core::market::listings(&data, None).await
    {
        for l in listings.iter().take(20) {
            let (c, id) = (l.contract.clone(), l.token_id.clone());
            let _ = data.cached(&format!("listing_nft:{c}:{id}"), 86_400, || data.explorer.nft(&c, &id)).await;
        }
    }
}

/// Resolves on Ctrl-C, SIGTERM or SIGHUP.
async fn shutdown_signal(detached: bool) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).ok();
        // In the background a hang-up is the terminal that started it closing: not a reason to go.
        // It is still taken (and dropped), since an unhandled SIGHUP would end the process.
        let mut hup = signal(SignalKind::hangup()).ok();
        if detached && let Some(mut ignored) = hup.take() {
            tokio::spawn(async move { while ignored.recv().await.is_some() {} });
        }
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = async { match term.as_mut() { Some(s) => { s.recv().await; } None => std::future::pending::<()>().await } } => {}
            _ = async { match hup.as_mut() { Some(s) => { s.recv().await; } None => std::future::pending::<()>().await } } => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = detached;
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Whether a daemon process currently holds the lock.
///
/// Asked by trying to take the lock ourselves: if it can be taken, nobody holds it. That answers
/// the question the caller actually has — is one running now — rather than whether a file with a
/// plausible pid in it happens to exist.
pub fn daemon_running(paths: &wallet_core::paths::Paths) -> bool {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    let Ok(file) = options.open(paths.daemon_lock()) else {
        return false;
    };
    match file.try_lock() {
        Ok(()) => {
            let _ = file.unlock();
            false
        }
        Err(_) => true,
    }
}

pub fn unit(ctx: &Ctx) -> String {
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "quai-terminal".into());
    format!(
        "# ~/.config/systemd/user/quai-terminal.service\n# Monitoring only (sealed chats and private payments need an unlocked daemon).\n[Unit]\nDescription=Quai Terminal monitor\nAfter=network-online.target\n\n[Service]\nEnvironment=QUAI_TERMINAL_HOME={}\nExecStart={exe} daemon run --locked\nRestart=on-failure\n\n[Install]\nWantedBy=default.target\n",
        ctx.paths.root().display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn control_frames_are_bounded_and_require_a_terminator() {
        assert_eq!(&**read_control_line(&b"{}\n"[..], 3).await.unwrap(), b"{}\n");
        assert!(read_control_line(&b"{}\n"[..], 2).await.is_err());
        assert!(read_control_line(&b"{}"[..], 3).await.is_err());
        let huge = vec![b'x'; 1000];
        assert!(read_control_line(huge.as_slice(), 8).await.is_err());
    }

    #[tokio::test]
    async fn a_control_request_interrupts_a_stalled_read() {
        let (send, mut receive) = tokio::sync::mpsc::channel(1);
        let (reply, _) = tokio::sync::oneshot::channel();
        send.send(Request { body: serde_json::json!({"cmd": "lock"}), reply: Some(reply) }).await.unwrap();
        let result =
            tokio::time::timeout(std::time::Duration::from_millis(100), work_or_control(std::future::pending::<()>(), &mut receive))
                .await
                .expect("control stays responsive");
        assert!(matches!(result, Err(request) if request.body["cmd"] == "lock"));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn runtime_symlinks_and_nonprivate_directories_are_refused_without_chmod() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        let link = dir.path().join("runtime");
        symlink(&target, &link).unwrap();
        assert!(ensure_runtime_dir(&link).is_err());
        assert!(ensure_runtime_dir(&target).is_err());
        assert_eq!(std::fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o755);
        let fresh = dir.path().join("private");
        ensure_runtime_dir(&fresh).unwrap();
        assert_eq!(std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777, 0o700);
        let lock_target = fresh.join("target.lock");
        std::fs::write(&lock_target, "unchanged").unwrap();
        symlink(&lock_target, fresh.join("link.lock")).unwrap();
        assert!(LockFile::acquire(fresh.join("link.lock")).is_err());
        assert_eq!(std::fs::read_to_string(lock_target).unwrap(), "unchanged");
    }

    /// With no daemon holding the lock, a password goes nowhere, whatever sits at the socket path.
    #[tokio::test]
    async fn a_password_is_not_handed_to_a_daemon_that_is_not_running() {
        let dir = tempfile::tempdir().unwrap();
        let paths = wallet_core::paths::Paths::resolve(Some(dir.path().to_path_buf())).unwrap();
        let err = hand_unlock(&paths, "w", "secret").await.unwrap_err().to_string();
        assert!(err.contains("not handing the password"), "{err}");
    }

    #[test]
    fn this_process_knows_its_own_user() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let dir = tempfile::tempdir().unwrap();
            assert_eq!(own_uid(), std::fs::metadata(dir.path()).unwrap().uid());
        }
    }

    fn post(id: &str, from: &str, text: &str) -> wallet_core::chat::ChatLine {
        wallet_core::chat::ChatLine { at: 1, id: id.into(), from: from.into(), who: from.into(), text: text.into() }
    }

    fn news(target: &str, fresh: Vec<wallet_core::chat::ChatLine>) -> wallet_core::chat::ChatNews {
        wallet_core::chat::ChatNews { target: target.into(), title: target.into(), body: String::new(), fresh, notice: None }
    }

    #[test]
    fn a_channel_post_is_announced_once_however_many_wallets_follow_it() {
        let mut notices = ChannelNotices::default();
        let gm = post("0xa:0", "0xbob", "gm");
        assert_eq!(notices.take(&news("#general", vec![gm.clone()]), &[]).as_deref(), Some("0xbob: gm"), "the first wallet announces");
        assert_eq!(notices.take(&news("#general", vec![gm.clone()]), &[]), None, "a second wallet on the same channel stays quiet");
        // A wallet that looked a poll later sees an announced post beside a new one.
        let wen = post("0xb:3", "0xcat", "wen");
        assert_eq!(notices.take(&news("#general", vec![gm.clone(), wen]), &[]).as_deref(), Some("0xcat: wen"), "only the new post");
        // Another channel, followed only by some other wallet, still speaks.
        assert_eq!(notices.take(&news("#trading", vec![post("0xc:0", "0xbob", "long")]), &[]).as_deref(), Some("0xbob: long"));
    }

    #[test]
    fn a_post_from_any_wallet_here_is_not_news() {
        let mut notices = ChannelNotices::default();
        let own = vec!["0xme".to_string()];
        assert_eq!(notices.take(&news("#general", vec![post("0xa:0", "0xme", "hi")]), &own), None, "written by another wallet here");
        let both = vec![post("0xa:0", "0xme", "hi"), post("0xa:1", "0xbob", "hey")];
        assert_eq!(notices.take(&news("#general", both), &own).as_deref(), Some("0xbob: hey"));
    }
}
