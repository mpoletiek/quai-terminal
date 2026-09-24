//! Background worker: owns the wallet session on its own thread and runtime so RPC and
//! key-derivation work never blocks input or rendering.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;
use wallet_core::appdb::{Activity, Contact, Notice, Operation};
use wallet_core::config::AppConfig;
use wallet_core::extras::{self, Price};
use wallet_core::network::{self, NodeHealth};
use wallet_core::ops::{ConversionQuote, PeerView, TokenBalance, WrapStatus};
use wallet_core::registry::{Registry, WalletMeta};
use wallet_core::sdk::U256;
use wallet_core::session::{AccountBalance, QiSummary, Session};
use wallet_core::track::LockItem;
use wallet_core::tx::{Review, Submitted};
use zeroize::Zeroizing;

/// Everything the UI displays, refreshed in the background.
#[derive(Clone, Debug, Default)]
pub struct Dashboard {
    pub meta: Option<WalletMeta>,
    pub network_id: String,
    pub network_name: String,
    pub explorer: Option<String>,
    pub health: Option<NodeHealth>,
    pub node_error: Option<String>,
    pub accounts: Vec<AccountBalance>,
    pub qi: Option<QiSummary>,
    pub tokens: Vec<TokenBalance>,
    pub ops: Vec<Operation>,
    /// When `ops` was read ([`ops_stamp`]): the pending lane updates them between refreshes, and
    /// the newer read wins.
    pub ops_at: u64,
    pub activity: Vec<Activity>,
    pub locks: Vec<LockItem>,
    pub contacts: Vec<Contact>,
    /// Every address recorded for a contact, lowercase, as (address, contact name). A person is
    /// their payment code, so one contact answers to as many accounts as they write from.
    pub contact_addresses: Vec<(String, String)>,
    pub peers: Vec<PeerView>,
    /// Announced senders with Qi waiting, not registered: the user accepts or declines each.
    pub offers: Vec<wallet_core::ops::ChannelOffer>,
    pub notifications: Vec<Notice>,
    pub qi_addresses: Vec<(u32, String, Option<String>)>,
    pub wrap: Option<WrapStatus>,
    /// Why wrap status is unavailable (not configured, no account, node error).
    pub wrap_error: Option<String>,
    pub price: Option<Price>,
    pub unlocked: bool,
    pub refreshed_at: u64,
    pub latency_history: Vec<u64>,
    pub height_history: Vec<u64>,
    pub networks: Vec<(String, String)>,
}

/// What a form asks the worker to prepare.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Prepare {
    OrderRun {
        id: String,
    },
    InPlan {
        id: String,
        request: Box<Prepare>,
    },
    Trading {
        intent: wallet_core::execution::TradingIntent,
    },
    SendQuai {
        from: Option<String>,
        to: String,
        amount: String,
        max_fee: Option<String>,
    },
    SendQi {
        to: String,
        amount: String,
        max_fee: Option<String>,
    },
    SendToken {
        from: Option<String>,
        token: String,
        to: String,
        amount: String,
    },
    /// Call a function on a contract the wallet was never taught about.
    ContractCall {
        account: Option<String>,
        address: String,
        signature: String,
        args: Vec<String>,
        value: Option<String>,
    },
    Approve {
        token: String,
        spender: String,
        amount: Option<String>,
    },
    ConvertQuaiToQi {
        from: Option<String>,
        amount: String,
        slippage: Option<u16>,
    },
    ConvertQiToQuai {
        to: Option<String>,
        amount: String,
        slippage: Option<u16>,
    },
    WrapQi {
        account: Option<String>,
        amount: String,
    },
    ClaimWqi {
        account: Option<String>,
    },
    /// Approve LP, then stake it — the gauge's two-step walk.
    StakeNext {
        account: Option<String>,
        pair: String,
        #[serde(default)]
        gauge: Option<String>,
        amount: String,
    },
    Unstake {
        account: Option<String>,
        pair: String,
        #[serde(default)]
        gauge: Option<String>,
        amount: String,
    },
    Harvest {
        account: Option<String>,
        pair: String,
        #[serde(default)]
        gauge: Option<String>,
        /// Unstake everything alongside the claim.
        exit: bool,
    },
    /// Buy a token on its bonding curve with QUAI.
    CurveBuy {
        account: Option<String>,
        token: String,
        symbol: String,
        curve: String,
        amount: String,
        slippage: u16,

        #[serde(default)]
        deadline: Option<u32>,
    },
    /// Sell to a bonding curve: the exact approval, then the sale.
    CurveSellNext {
        account: Option<String>,
        token: String,
        symbol: String,
        curve: String,
        amount: String,
        slippage: u16,

        #[serde(default)]
        deadline: Option<u32>,
    },
    /// Collect the QUAI a curve credited (sales, overshoot past the target).
    CurveClaim {
        account: Option<String>,
        token: String,
        symbol: String,
        curve: String,
    },
    /// The next review of funding a pool's rewards: the exact approval, then the funding.
    IncentivizeNext {
        account: Option<String>,
        pair: String,
        token: String,
        amount: String,
        days: u32,
    },
    /// The next review of a deposit: each exact approval while one is needed, then the add.
    AddLiquidityNext {
        account: Option<String>,
        pair: String,
        amount: String,
        /// Which token `amount` is in; the pool derives the other side.
        token: Option<String>,
        slippage: u16,
        deadline: u32,
    },
    /// The next review of a withdrawal: the LP approval while one is needed, then the remove.
    RemoveLiquidityNext {
        account: Option<String>,
        pair: String,
        percent: u8,
        slippage: u16,
        deadline: u32,
    },
    UnwrapWqi {
        account: Option<String>,
        amount: String,
    },
    WrapQuai {
        account: Option<String>,
        amount: String,
    },
    UnwrapQuai {
        account: Option<String>,
        amount: String,
    },
    Notify {
        from: Option<String>,
        peer: String,
    },
    BoardPost {
        from: Option<String>,
        channel: String,
        text: String,
    },
    BoardDm {
        from: Option<String>,
        peer: String,
        text: String,
    },
    Consolidate {
        aggregate: bool,
    },
    SpeedUp {
        op: String,
    },
    FillGap {
        from: Option<String>,
    },
    /// The next review of a swap sequence: its exact approval while one is needed, then the swap.
    SwapNext {
        account: Option<String>,
        from: String,
        to: String,
        amount: String,
        slippage: u16,
        deadline: u32,
    },
    /// The next review of an NFT purchase: module approval, token approval, then the buy.
    NftBuyNext {
        account: Option<String>,
        contract: String,
        token_id: String,
        price: Option<String>,
    },
    /// The next review of listing an item: module approval, collection approval, then the ask
    /// (new listing or new price). `price: None` cancels the listing.
    NftListNext {
        account: Option<String>,
        contract: String,
        token_id: String,
        price: Option<String>,
        currency: String,
    },
    NftTransfer {
        account: Option<String>,
        contract: String,
        token_id: String,
        to: String,
        quantity: Option<String>,
    },
}

/// A change to the chat subscriptions or the pin. `label` is how the chat reads in the toast.
#[derive(Clone, Debug)]
pub enum ChatOp {
    Load,
    Toggle { target: String, label: String },
    Pin { target: Option<String>, label: String },
}

/// Commands from the UI.
pub enum Cmd {
    Order(super::order_ui::Request),
    SplitQuote {
        key: u64,
        account: Option<String>,
        from: String,
        to: String,
        amount: String,
        slippage: u16,
    },
    QiMax {
        key: u64,
        wrapping: bool,
        account: Option<String>,
        slippage: u16,
    },
    /// This wallet's trading performance.
    Pnl,
    Refresh {
        full: bool,
    },
    /// Keys the UI unlocked itself, off this thread, for the wallet with this id. The password
    /// was checked there, so an unlock never waits behind a sync; it rides along only so a
    /// network switch can re-open the vault.
    UseKeys {
        wallet: String,
        keys: Box<wallet_core::identity::Unlocked>,
        password: Zeroizing<String>,
    },
    Lock,
    /// What is at a send destination: a plain account, or a contract and the ABI it publishes.
    /// Answered with [`Ev::Contract`]; the send form asks while someone is still typing.
    InspectContract {
        address: String,
    },
    Prepare(Prepare),
    Commit(String),
    Discard(String),
    Quote {
        direction: String,
        amount: String,
    },
    AddAccount(Option<String>),
    /// Import a private key into this wallet. The password re-seals the vault.
    ImportKey {
        label: Option<String>,
        key: Zeroizing<String>,
        password: Zeroizing<String>,
    },
    /// Watch another address (watch-only wallets).
    WatchAddress {
        address: String,
        label: Option<String>,
    },
    RenameAccount {
        account: String,
        label: String,
    },
    NewQiAddress(Option<String>),
    ScanQi {
        deep: Option<u32>,
    },
    DiscoverMailbox,
    /// Add (original = None) or edit a contact.
    SaveContact {
        original: Option<String>,
        name: String,
        address: Option<String>,
        code: Option<String>,
        note: String,
    },
    /// Rescan one payment channel now.
    ScanPeer(String),
    /// Accept an announced sender's channel offer: register it and scan it.
    AcceptOffer(String),
    /// Decline an announced sender's channel offer.
    DeclineOffer(String),
    RemoveContact(String),
    ImportToken(String),
    /// Import tokens the explorer finds for this wallet's accounts.
    DiscoverTokens,
    SwitchNetwork(String),
    /// Open a different wallet. Its keys are its own, so the session starts locked.
    SwitchWallet(String),
    /// Read the sealed conversation with a peer (needs this wallet's payment key).
    ReadConversation {
        peer: String,
        blocks: u64,
    },
    MarkRead,
    /// The signing lane broadcast a transaction: refresh now so it shows.
    Committed,
    /// The signing lane changed the journal (a review discarded): re-read it.
    Journal,
    /// Chat subscriptions and the pin: read them, toggle a subscription, or set the pin.
    Chat(ChatOp),
    /// Check subscribed chats for news (sealed ones need the key, so this is the wallet worker);
    /// only the sealed ones when a daemon reads the channels.
    ChatNews {
        dms_only: bool,
    },
    /// The Qi lane finished a pass (see [`QiLane`]). Background: it never cuts a refresh short.
    QiSynced(QiDone),
    ExportPhrase(Zeroizing<String>),
    Backup {
        path: String,
        password: Zeroizing<String>,
    },
    Shutdown,
}

impl Cmd {
    /// A short name for traces. Never includes anything the command carries.
    fn kind(&self) -> &'static str {
        match self {
            Cmd::Order(_) => "order",
            Cmd::Refresh { .. } => "refresh",
            Cmd::UseKeys { .. } => "use_keys",
            Cmd::Lock => "lock",
            Cmd::Prepare(_) => "prepare",
            Cmd::Commit(_) => "commit",
            Cmd::Discard(_) => "discard",
            Cmd::Quote { .. } => "quote",
            Cmd::QiMax { .. } => "max_quote",
            Cmd::Pnl => "pnl",
            Cmd::SplitQuote { .. } => "split_quote",
            Cmd::InspectContract { .. } => "inspect_contract",
            Cmd::AddAccount(_) => "add_account",
            Cmd::ImportKey { .. } => "import_key",
            Cmd::WatchAddress { .. } => "watch_address",
            Cmd::RenameAccount { .. } => "rename_account",
            Cmd::NewQiAddress(_) => "new_qi_address",
            Cmd::ScanQi { .. } => "scan_qi",
            Cmd::QiSynced(_) => "qi_synced",
            Cmd::DiscoverMailbox => "discover_mailbox",
            Cmd::SaveContact { .. } => "save_contact",
            Cmd::ScanPeer(_) => "scan_peer",
            Cmd::AcceptOffer(_) => "accept_offer",
            Cmd::DeclineOffer(_) => "decline_offer",
            Cmd::RemoveContact(_) => "remove_contact",
            Cmd::ImportToken(_) => "import_token",
            Cmd::DiscoverTokens => "discover_tokens",
            Cmd::SwitchNetwork(_) => "switch_network",
            Cmd::SwitchWallet(_) => "switch_wallet",
            Cmd::ReadConversation { .. } => "read_conversation",
            Cmd::Chat(_) => "chat",
            Cmd::ChatNews { .. } => "chat_news",
            Cmd::MarkRead => "mark_read",
            Cmd::Committed => "committed",
            Cmd::Journal => "journal",
            Cmd::ExportPhrase(_) => "export_phrase",
            Cmd::Backup { .. } => "backup",
            Cmd::Shutdown => "shutdown",
        }
    }
}

/// Events back to the UI.
pub enum Ev {
    /// A new block at this height, as soon as the worker sees one.
    Head(u64),
    Orders {
        wallet: String,
        network: String,
        rows: Vec<wallet_core::plans::TradePlan>,
        /// Orders this check found reachable first, and announced (see `orders::observe`).
        announced: Vec<String>,
    },
    SplitQuote {
        key: u64,
        result: Result<Box<wallet_core::split_routes::SplitDecision>, String>,
    },
    QiMax {
        key: u64,
        result: Result<wallet_core::ops::QiSpecialMax, String>,
    },
    Pnl(Result<Box<wallet_core::pnl::Pnl>, String>),
    Dashboard(Box<Dashboard>),
    /// What [`Cmd::InspectContract`] found, with the address that was asked about so a late
    /// answer for an address the user has since edited away can be dropped.
    Contract {
        address: String,
        found: Box<Option<wallet_core::contracts::Discovered>>,
    },
    Review(Box<Review>),
    OrderReview(Box<Review>),
    Submitted(Submitted),
    Quote(Box<ConversionQuote>),
    Info(String),
    Error(String),
    /// Preparing a transaction failed: nothing was signed or sent.
    PrepareError(String),
    CommitError {
        op_id: String,
        message: String,
        /// The node may have the transaction: the outcome is unknown until the journal is
        /// reconciled. When false, nothing left this wallet.
        ambiguous: bool,
    },
    Unlocked,
    Locked,
    /// Keys arrived for a wallet this session no longer has open (its switch failed): they were
    /// dropped, and the screen must lock again rather than show a wallet that cannot sign.
    KeysRefused,
    Secret(Zeroizing<String>),
    Busy(Option<String>),
    /// The signing lane's own status. The worker's `Busy(None)` at the end of a sync must not
    /// take "preparing transaction…" off the screen while the review is still being built.
    SignBusy(Option<String>),
    /// A user-initiated command finished successfully (closes its form).
    Ack(String),
    /// Background status change worth a terminal/desktop notification.
    Notify {
        title: String,
        body: String,
        /// The same event was also written to the wallet's notification list, which a running
        /// daemon forwards to the desktop; the terminal then leaves the desktop to it.
        listed: bool,
    },
    /// Chat subscriptions and the pin as stored, and a line to show.
    Chat {
        subs: Vec<String>,
        pin: Option<String>,
        note: Option<String>,
    },
    /// Subscribed chats with news, as (title, body); each is already a notification.
    ChatNews(Vec<(String, String)>),
    /// The journal after the pending lane saw a sent transaction mined, read at `at`
    /// ([`ops_stamp`]), for the wallet and network it watches.
    Ops {
        wallet: String,
        network: String,
        ops: Vec<Operation>,
        at: u64,
    },
    /// A sealed conversation, opened with this wallet's key.
    Conversation {
        peer: String,
        result: Result<Vec<wallet_core::ops::SealedLine>, String>,
    },
}

pub struct Worker {
    pub tx: tokio::sync::mpsc::UnboundedSender<Cmd>,
    pub rx: Receiver<Ev>,
    /// Transactions: prepared, reviewed and sent on their own lane.
    sign: std::sync::mpsc::Sender<SignJob>,
    /// Sent transactions, watched until they are mined.
    pending: std::sync::mpsc::Sender<PendingJob>,
}

impl Worker {
    pub fn spawn(
        registry: Registry,
        config: AppConfig,
        meta: WalletMeta,
        network_id: String,
        wake: impl Fn() + Send + 'static,
    ) -> std::io::Result<Worker> {
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<Cmd>();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel::<Ev>();
        let lane_tx = cmd_tx.clone();
        let sign = SignLane::spawn(registry.clone(), config.clone(), meta.id.clone(), network_id.clone(), ev_tx.clone(), cmd_tx.clone());
        let pending =
            PendingLane::spawn(registry.clone(), config.clone(), meta.id.clone(), network_id.clone(), ev_tx.clone(), cmd_tx.clone());
        std::thread::Builder::new().name("wallet-worker".into()).spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = ev_tx.send(Ev::Error(format!("runtime: {e}")));
                    super::term::wake();
                    return;
                }
            };
            let lane = QiLane::spawn(registry.clone(), config.clone(), lane_tx);
            runtime.block_on(run(registry, config, meta, network_id, Inbox::new(cmd_rx), ev_tx, wake, lane));
        })?;
        Ok(Worker { tx: cmd_tx, rx: ev_rx, sign, pending })
    }

    /// Send a command where it runs. Preparing, committing and discarding go to the
    /// signing lane, which does nothing else, so they start at once. Keys, locking and switching
    /// go to both, so the lane signs as the wallet on screen and never after a lock.
    pub fn send(&self, cmd: Cmd) {
        match cmd {
            Cmd::Prepare(req) => {
                let _ = self.sign.send(SignJob::Prepare(req));
            }
            Cmd::Commit(id) => {
                let _ = self.sign.send(SignJob::Commit(id));
            }
            Cmd::Discard(id) => {
                let _ = self.sign.send(SignJob::Discard(id));
            }
            // Never the signing lane: that one is serial, and a gateway timing out in front of a
            // Prepare or a Lock would hold up a transaction — or an unlock — for as long as it
            // takes. The wallet worker already does background reads.
            Cmd::InspectContract { address } => {
                let _ = self.tx.send(Cmd::InspectContract { address });
            }
            Cmd::UseKeys { wallet, keys, password } => {
                if let Ok(copy) = keys.duplicate() {
                    let _ = self.sign.send(SignJob::Keys { wallet: wallet.clone(), keys: Box::new(copy) });
                }
                let _ = self.tx.send(Cmd::UseKeys { wallet, keys, password });
            }
            Cmd::Lock => {
                let _ = self.sign.send(SignJob::Lock);
                let _ = self.tx.send(Cmd::Lock);
            }
            Cmd::SwitchNetwork(network) => {
                let _ = self.pending.send(PendingJob::Network(network.clone()));
                let _ = self.sign.send(SignJob::Network(network.clone()));
                let _ = self.tx.send(Cmd::SwitchNetwork(network));
            }
            Cmd::SwitchWallet(wallet) => {
                let _ = self.pending.send(PendingJob::Wallet(wallet.clone()));
                let _ = self.sign.send(SignJob::Wallet(wallet.clone()));
                let _ = self.tx.send(Cmd::SwitchWallet(wallet));
            }
            Cmd::Shutdown => {
                let _ = self.pending.send(PendingJob::Shutdown);
                let _ = self.sign.send(SignJob::Shutdown);
                let _ = self.tx.send(Cmd::Shutdown);
            }
            other => {
                let _ = self.tx.send(other);
            }
        }
    }
}

#[cfg(test)]
impl Worker {
    /// A worker whose commands land in the returned receiver; the lanes' go nowhere.
    pub(crate) fn capture() -> (Worker, tokio::sync::mpsc::UnboundedReceiver<Cmd>) {
        let (tx, rx_cmd) = tokio::sync::mpsc::unbounded_channel::<Cmd>();
        let (_ev, rx) = std::sync::mpsc::channel::<Ev>();
        let (sign, _) = std::sync::mpsc::channel::<SignJob>();
        let (pending, _) = std::sync::mpsc::channel::<PendingJob>();
        (Worker { tx, rx, sign, pending }, rx_cmd)
    }

    /// A worker whose preparations land in the returned receiver; everything else goes nowhere.
    pub(crate) fn capture_prepares() -> (Worker, std::sync::mpsc::Receiver<Prepare>) {
        let (tx, _) = tokio::sync::mpsc::unbounded_channel::<Cmd>();
        let (_ev, rx) = std::sync::mpsc::channel::<Ev>();
        let (sign, jobs) = std::sync::mpsc::channel::<SignJob>();
        let (pending, _) = std::sync::mpsc::channel::<PendingJob>();
        let (prepares, out) = std::sync::mpsc::channel::<Prepare>();
        // Keep only the preparations, in order; the rest of the signing lane is dropped.
        std::thread::spawn(move || {
            for job in jobs {
                if let SignJob::Prepare(p) = job
                    && prepares.send(p).is_err()
                {
                    break;
                }
            }
        });
        (Worker { tx, rx, sign, pending }, out)
    }
}

/// Order of journal reads across the worker and the pending lane: a read stamped later saw the
/// journal no earlier than one stamped before it.
pub fn ops_stamp() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Work for the pending lane.
enum PendingJob {
    Wallet(String),
    Network(String),
    Shutdown,
}

/// Sent transactions, watched until they are mined, on their own thread with their own session.
/// The worker's tracking runs inside its refresh, behind whatever that refresh is doing (a
/// payment-channel scan can take a while), so a transaction mined in seconds could sit at
/// "submitted" for a minute or more. This lane does nothing else: while a Quai transaction waits
/// for a block it asks the node every few seconds, and otherwise reads one indexed row set from
/// the journal. The worker's own pass leaves these operations to it ([`OpScope::Rest`]).
///
/// [`OpScope::Rest`]: wallet_core::track::OpScope::Rest
struct PendingLane;

impl PendingLane {
    /// How often a transaction waiting for a block is checked (blocks come about every 5 s).
    const EVERY: Duration = Duration::from_secs(3);
    /// How often the journal is looked at while nothing waits: a local query, no network.
    const IDLE: Duration = Duration::from_secs(2);

    fn spawn(
        registry: Registry,
        config: AppConfig,
        wallet: String,
        network: String,
        events: Sender<Ev>,
        worker: tokio::sync::mpsc::UnboundedSender<Cmd>,
    ) -> std::sync::mpsc::Sender<PendingJob> {
        let (jobs, rx) = std::sync::mpsc::channel::<PendingJob>();
        let _ = std::thread::Builder::new().name("wallet-pending".into()).spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
            let open = |wallet: &str, network: &str| -> Option<Session> {
                let meta = registry.resolve(Some(wallet), None).ok()?;
                lane_session(&runtime, &registry, &config, meta, network)
            };
            let (mut wallet, mut network) = (wallet, network);
            let mut session = open(&wallet, &network);
            let mut waiting = false;
            loop {
                match rx.recv_timeout(if waiting { Self::EVERY } else { Self::IDLE }) {
                    Ok(PendingJob::Shutdown) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                    Ok(PendingJob::Wallet(id)) => {
                        wallet = id;
                        session = open(&wallet, &network);
                    }
                    Ok(PendingJob::Network(id)) => {
                        network = id;
                        session = open(&wallet, &network);
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                }
                let Some(s) = session.as_mut() else {
                    waiting = false;
                    continue;
                };
                recheck_monitor(&runtime, s);
                waiting = s
                    .app
                    .awaiting_inclusion(&s.network.id)
                    .is_ok_and(|ops| ops.iter().any(|o| wallet_core::track::OpScope::Inclusion.covers(o)));
                if !waiting {
                    continue;
                }
                let t = std::time::Instant::now();
                let tracked = runtime.block_on(s.track_inclusion());
                wallet_core::diag::timing("pending.track", t);
                let Ok(report) = tracked else { continue };
                if report.changes.is_empty() {
                    continue;
                }
                let at = ops_stamp();
                if let Ok(ops) = s.app.operations(&s.network.id, 200) {
                    let _ = events.send(Ev::Ops { wallet: s.meta.id.clone(), network: s.network.id.clone(), ops, at });
                    super::term::wake();
                }
                for c in report.changes {
                    let _ = events.send(Ev::Notify { title: format!("Transaction {}", c.to.as_str()), body: c.message, listed: true });
                    super::term::wake();
                }
                // Balances moved with it: a refresh, in the background, merged with any queued.
                let _ = worker.send(Cmd::Refresh { full: false });
            }
        });
        jobs
    }
}

/// A lane's own session of a wallet on a network. It reads from the monitoring node when that
/// checks out, like the worker's; whatever it broadcasts goes to the RPC endpoint regardless.
fn lane_session(
    runtime: &tokio::runtime::Runtime,
    registry: &Registry,
    config: &AppConfig,
    meta: WalletMeta,
    network: &str,
) -> Option<Session> {
    let profile = config.network(network).ok()?;
    let mut session = Session::open(registry.clone(), config.clone(), meta, profile).ok()?;
    let _ = runtime.block_on(session.use_monitor());
    Some(session)
}

/// When a monitoring node is configured but not in use (it failed its check, or stopped
/// answering), try it again every few minutes, as the worker does.
fn recheck_monitor(runtime: &tokio::runtime::Runtime, session: &mut Session) {
    const EVERY: u64 = 180;
    thread_local! {
        static LAST: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }
    if session.network.monitor.is_none() || session.monitoring() {
        return;
    }
    let now = wallet_core::registry::now();
    if LAST.with(|l| now.saturating_sub(l.get()) >= EVERY) {
        LAST.with(|l| l.set(now));
        let _ = runtime.block_on(session.use_monitor());
    }
}

/// Work for the signing lane.
enum SignJob {
    Prepare(Prepare),
    Commit(String),
    Discard(String),
    Keys { wallet: String, keys: Box<wallet_core::identity::Unlocked> },
    Lock,
    Network(String),
    Wallet(String),
    Shutdown,
}

/// Transactions on their own thread, with their own session of the open wallet and a copy of its
/// keys. The wallet worker refreshes balances, tracks, syncs payment channels, reads chats; none
/// of that is in the way here, and none of it needs to stop for a transaction. The two share the
/// wallet's databases, whose writes are transactions of their own (the same footing the Qi lane
/// is on). Reviews live in this session, so the commit or discard of one comes here too.
struct SignLane;

impl SignLane {
    fn spawn(
        registry: Registry,
        config: AppConfig,
        wallet: String,
        network: String,
        events: Sender<Ev>,
        worker: tokio::sync::mpsc::UnboundedSender<Cmd>,
    ) -> std::sync::mpsc::Sender<SignJob> {
        let (jobs, rx) = std::sync::mpsc::channel::<SignJob>();
        let _ = std::thread::Builder::new().name("wallet-sign".into()).spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
            let send = |ev: Ev| {
                let _ = events.send(ev);
                super::term::wake();
            };
            let open = |wallet: &str, network: &str| -> Option<Session> {
                let meta = registry.resolve(Some(wallet), None).ok()?;
                lane_session(&runtime, &registry, &config, meta, network)
            };
            let (mut wallet, mut network) = (wallet, network);
            let mut session = open(&wallet, &network);
            while let Ok(job) = rx.recv() {
                // Accounts can be added between jobs (by the worker); the wallet file is the truth.
                if let Some(s) = session.as_mut()
                    && let Ok(meta) = registry.resolve(Some(&wallet), None)
                {
                    s.meta = meta;
                }
                match job {
                    SignJob::Shutdown => {
                        if let Some(s) = session.as_mut() {
                            s.lock();
                        }
                        return;
                    }
                    SignJob::Keys { wallet: for_wallet, keys } => {
                        if for_wallet == wallet
                            && let Some(s) = session.as_mut()
                        {
                            s.use_keys(*keys);
                        }
                    }
                    SignJob::Lock => {
                        if let Some(s) = session.as_mut() {
                            s.lock();
                        }
                    }
                    SignJob::Wallet(id) => {
                        // Another wallet's keys arrive with its unlock; these go with the old one.
                        if let Some(s) = session.as_mut() {
                            s.lock();
                        }
                        wallet = id;
                        session = open(&wallet, &network);
                    }
                    SignJob::Network(id) => {
                        // Same wallet, same keys: carried over to the session on the new network.
                        let keys = session.as_ref().and_then(Session::duplicate_keys);
                        if let Some(s) = session.as_mut() {
                            s.lock();
                        }
                        network = id;
                        session = open(&wallet, &network);
                        if let (Some(s), Some(k)) = (session.as_mut(), keys) {
                            s.use_keys(k);
                        }
                    }
                    SignJob::Prepare(req) => {
                        let Some(s) = session.as_mut() else {
                            send(Ev::Error("the wallet could not be opened for signing".into()));
                            continue;
                        };
                        let t = std::time::Instant::now();
                        send(Ev::SignBusy(Some("preparing transaction…".into())));
                        let order = matches!(req, Prepare::OrderRun { .. });
                        // Whether the monitoring node this review reads from is keeping up with the
                        // RPC it will be broadcast through, asked while the review is prepared.
                        let probe = s.lag_probe();
                        let lagging = async {
                            match &probe {
                                Some(probe) => probe.warning().await,
                                None => None,
                            }
                        };
                        let (result, lag) = runtime.block_on(async { tokio::join!(prepare(s, req), lagging) });
                        wallet_core::diag::timing("sign.prepare", t);
                        send(Ev::SignBusy(None));
                        match result.map(|mut r| {
                            if let Some(warning) = lag {
                                r.warnings.insert(0, warning);
                            }
                            r
                        }) {
                            Ok(r) => send(if order { Ev::OrderReview(Box::new(r)) } else { Ev::Review(Box::new(r)) }),
                            // Nothing was signed: preparing is reading and building only.
                            Err(e) => send(Ev::PrepareError(e.to_string())),
                        }
                    }
                    SignJob::Commit(id) => {
                        let Some(s) = session.as_mut() else { continue };
                        send(Ev::SignBusy(Some("signing and broadcasting…".into())));
                        let result = runtime.block_on(s.commit(&id));
                        send(Ev::SignBusy(None));
                        match result {
                            Ok(sub) => send(Ev::Submitted(sub)),
                            Err(e) => {
                                let ambiguous =
                                    matches!(e, wallet_core::error::CoreError::Ambiguous(_) | wallet_core::error::CoreError::Timeout(_));
                                send(Ev::CommitError { op_id: id, message: e.to_string(), ambiguous })
                            }
                        }
                        let _ = worker.send(Cmd::Committed);
                    }
                    SignJob::Discard(id) => {
                        let Some(s) = session.as_mut() else { continue };
                        match s.discard(&id) {
                            Ok(()) => send(Ev::Info("rejected; nothing was signed".into())),
                            Err(e) => send(Ev::Error(e.to_string())),
                        }
                        let _ = worker.send(Cmd::Journal);
                    }
                }
            }
        });
        jobs
    }
}

/// Which wallet on which network a Qi pass was for. A result for anything else — the user switched
/// while it ran — is dropped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QiKey {
    wallet: String,
    network: String,
}

/// What the Qi lane was asked to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QiWork {
    /// The periodic incremental refresh.
    Refresh,
    /// A gap scan (first visit, a full refresh), or a deep scan to this raw index.
    Scan(Option<u32>),
}

/// A finished Qi pass, sent back to the wallet worker.
#[derive(Debug)]
pub struct QiDone {
    key: QiKey,
    /// Someone asked for this pass and wants to hear how it went.
    announce: bool,
    result: Result<u64, String>,
}

struct QiJob {
    key: QiKey,
    meta: WalletMeta,
    work: QiWork,
    announce: bool,
}

/// Qi sync, on its own thread with its own connection to the wallet's Qi store.
///
/// A Qi refresh is 1.4–5 s against ~5 s blocks, and a first scan 10–15 s. It used to run inside
/// the wallet worker, which does one thing at a time, so a QUAI send, a swap or a contact prepared
/// during it waited it out — and it cannot be cut short and resumed cheaply. The two do not share
/// anything but the database: a refresh reads public addresses and writes `qi.sqlite` in one
/// transaction at the end, guarded by the store's own generation check, and a send reserves coins
/// through that same check. So they run side by side: preparing a transaction never waits on Qi,
/// and Qi never waits on a transaction. No keys come here; scanning needs only the xpub.
pub struct QiLane {
    jobs: std::sync::mpsc::Sender<QiJob>,
    /// Passes asked for and not yet answered. The periodic refresh is skipped while one is out, so
    /// a slow node never stacks them up.
    in_flight: usize,
}

impl QiLane {
    fn spawn(registry: Registry, config: AppConfig, done: tokio::sync::mpsc::UnboundedSender<Cmd>) -> QiLane {
        let (jobs, rx) = std::sync::mpsc::channel::<QiJob>();
        let _ = std::thread::Builder::new().name("wallet-qi".into()).spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
            // One session per wallet and network, reopened when either changes.
            let mut open: Option<(QiKey, Session)> = None;
            while let Ok(job) = rx.recv() {
                let started = std::time::Instant::now();
                if open.as_ref().is_none_or(|(k, _)| *k != job.key) {
                    open = lane_session(&runtime, &registry, &config, job.meta.clone(), &job.key.network).map(|s| (job.key.clone(), s));
                }
                // Accounts and imported Qi keys can change between passes; the store is re-read
                // every pass anyway, and the wallet file travels with each job.
                if let Some((_, session)) = open.as_mut() {
                    session.meta = job.meta.clone();
                    recheck_monitor(&runtime, session);
                }
                let result = match open.as_mut() {
                    Some((_, session)) => runtime
                        .block_on(async {
                            match job.work {
                                QiWork::Refresh => session.refresh_qi().await,
                                QiWork::Scan(deep) => session.scan_qi(deep).await,
                            }
                        })
                        .map_err(|e| e.to_string()),
                    None => Err("could not open the Qi store".into()),
                };
                // Timed here, on the lane's own thread, so a trace shows where Qi work runs. Which
                // kind of pass it was and whether it got anywhere: the session retries a stale
                // snapshot up to 5 times (on top of the SDK's own attempts), so a failure is the only
                // sign from outside that those ran out, and a scan must never be read as a slow
                // incremental refresh.
                wallet_core::diag::timing("refresh.qi", started);
                wallet_core::diag::timing(
                    if matches!(job.work, QiWork::Refresh) { "refresh.qi.incremental" } else { "refresh.qi.scan" },
                    started,
                );
                if result.is_err() {
                    wallet_core::diag::timing("refresh.qi.failed", started);
                }
                let finished = QiDone { key: job.key, announce: job.announce, result };
                if done.send(Cmd::QiSynced(finished)).is_err() {
                    return;
                }
            }
        });
        QiLane { jobs, in_flight: 0 }
    }

    fn ask(&mut self, session: &Session, work: QiWork, announce: bool) {
        let key = QiKey { wallet: session.meta.id.clone(), network: session.network.id.clone() };
        if self.jobs.send(QiJob { key, meta: session.meta.clone(), work, announce }).is_ok() {
            self.in_flight += 1;
        }
    }

    /// The periodic pass, unless one is already out.
    fn refresh(&mut self, session: &mut Session, full: bool) {
        if self.in_flight > 0 {
            return;
        }
        let deep = full || session.qi_summary().map(|s| s.checkpoint_height.is_none()).unwrap_or(true);
        self.ask(session, if deep { QiWork::Scan(None) } else { QiWork::Refresh }, false);
    }
}

/// Commands the user is waiting on: they run before queued background work and cut a running
/// refresh short. Saving a contact is as much a wait as preparing a send, so only the work that
/// repeats on its own is background: refreshing, and the Board re-reading an open conversation
/// every few seconds — which, urgent, would cut every refresh short before it could finish.
fn urgent(cmd: &Cmd) -> bool {
    !matches!(cmd, Cmd::Refresh { .. } | Cmd::ReadConversation { .. } | Cmd::QiSynced(_) | Cmd::ChatNews { .. })
}

/// Prepare the review a request asks for (the signing lane's work).
fn selected_conversion_tolerance(quote: &ConversionQuote, manual: Option<u16>) -> wallet_core::Result<u16> {
    let selected = manual.unwrap_or(quote.suggested_slippage_bps);
    if selected > 10_000 || (!quote.discount_saturated && quote.implied_slippage_bps.is_some_and(|loss| loss > selected)) {
        return Err(wallet_core::CoreError::Rejected(
            "fresh conversion quote exceeds your manual tolerance; change it explicitly or use automatic".into(),
        ));
    }
    Ok(selected)
}
async fn conversion_tolerance(session: &Session, direction: &str, amount: &str, manual: Option<u16>) -> wallet_core::Result<u16> {
    let quote = session.conversion_quote(direction, amount).await?;
    selected_conversion_tolerance(&quote, manual)
}

async fn prepare(session: &mut Session, req: Prepare) -> wallet_core::Result<wallet_core::tx::Review> {
    match req {
        Prepare::OrderRun { id } => {
            session.track().await?;
            wallet_core::orders::prepare(session, &id).await?.ok_or_else(|| {
                wallet_core::CoreError::Rejected("order is waiting for its limit or an existing transaction; observe for details".into())
            })
        }
        Prepare::InPlan { id, request } => {
            session.begin_plan_preparation(&id)?;
            let result = Box::pin(prepare(session, *request)).await;
            session.end_plan_preparation();
            result
        }
        Prepare::Trading { intent } => intent.next_review(session).await,
        Prepare::SendQuai { from, to, amount, max_fee } => {
            session.review_send_quai(from.as_deref(), &to, &amount, max_fee.as_deref()).await
        }
        Prepare::SendQi { to, amount, max_fee } => session.review_send_qi(&to, &amount, max_fee.as_deref()).await,
        Prepare::SendToken { from, token, to, amount } => session.review_send_token(from.as_deref(), &token, &to, &amount, None).await,
        Prepare::ContractCall { account, address, signature, args, value } => {
            session.review_contract_call(account.as_deref(), &address, &signature, &args, value.as_deref(), None).await
        }
        Prepare::Approve { token, spender, amount } => session.review_approve(None, &token, &spender, amount.as_deref(), None).await,
        Prepare::ConvertQuaiToQi { from, amount, slippage } => {
            let slippage = conversion_tolerance(session, "quai_to_qi", &amount, slippage).await?;
            session.review_convert_quai_to_qi(from.as_deref(), &amount, slippage, None).await
        }
        Prepare::ConvertQiToQuai { to, amount, slippage } => {
            let slippage = conversion_tolerance(session, "qi_to_quai", &amount, slippage).await?;
            session.review_convert_qi_to_quai(to.as_deref(), &amount, slippage, None).await
        }
        Prepare::WrapQi { account, amount } => session.review_wrap_qi(account.as_deref(), &amount, None).await,
        Prepare::ClaimWqi { account } => session.review_claim_wqi(account.as_deref(), None).await,
        // Sequences with an approval first: the core hands back whichever step is next.
        Prepare::StakeNext { account, pair, gauge, amount } => {
            session.stake_next_in_gauge(account.as_deref(), &pair, gauge.as_deref(), &amount, None).await
        }
        Prepare::Unstake { account, pair, gauge, amount } => {
            session.review_unstake_in_gauge(account.as_deref(), &pair, gauge.as_deref(), &amount, None).await
        }
        Prepare::Harvest { account, pair, gauge, exit } => {
            session.review_harvest_in_gauge(account.as_deref(), &pair, gauge.as_deref(), exit, None).await
        }
        Prepare::CurveBuy { account, token, symbol, curve, amount, slippage, deadline } => {
            session
                .review_curve_buy(
                    account.as_deref(),
                    &token,
                    &symbol,
                    &curve,
                    &amount,
                    slippage,
                    deadline.unwrap_or(session.config.swap_deadline_minutes),
                    None,
                )
                .await
        }
        Prepare::CurveSellNext { account, token, symbol, curve, amount, slippage, deadline } => {
            session
                .curve_sell_next(
                    account.as_deref(),
                    &token,
                    &symbol,
                    &curve,
                    &amount,
                    slippage,
                    deadline.unwrap_or(session.config.swap_deadline_minutes),
                    None,
                )
                .await
        }
        Prepare::CurveClaim { account, token, symbol, curve } => {
            session.review_curve_claim(account.as_deref(), &token, &symbol, &curve, None).await
        }
        Prepare::IncentivizeNext { account, pair, token, amount, days } => {
            session.incentivize_next(account.as_deref(), &pair, &token, &amount, days, None).await
        }
        Prepare::AddLiquidityNext { account, pair, amount, token, slippage, deadline } => {
            session.add_liquidity_next(account.as_deref(), &pair, &amount, token.as_deref(), slippage, deadline, None).await
        }
        Prepare::RemoveLiquidityNext { account, pair, percent, slippage, deadline } => {
            session.remove_liquidity_next(account.as_deref(), &pair, percent, slippage, deadline, None).await
        }
        Prepare::UnwrapWqi { account, amount } => session.review_unwrap_wqi(account.as_deref(), &amount, None).await,
        Prepare::WrapQuai { account, amount } => session.review_wrap_quai(account.as_deref(), &amount, None).await,
        Prepare::UnwrapQuai { account, amount } => session.review_unwrap_quai(account.as_deref(), &amount, None).await,
        Prepare::Notify { from, peer } => session.review_notify(from.as_deref(), &peer, None).await,
        Prepare::BoardPost { from, channel, text } => session.review_post(from.as_deref(), &channel, &text, None).await,
        Prepare::BoardDm { from, peer, text } => session.review_dm(from.as_deref(), &peer, &text, None).await,
        Prepare::Consolidate { aggregate } => session.review_consolidate(aggregate, None).await,
        Prepare::SpeedUp { op } => session.prepare_speed_up(&op, 20).await,
        Prepare::FillGap { from } => session.review_fill_gap(from.as_deref()).await,
        Prepare::SwapNext { account, from, to, amount, slippage, deadline } => {
            match session.swap_quote(account.as_deref(), &from, &to, &amount, slippage, wallet_core::data::Trust::Cached).await {
                // Paying WQUAI the account lacks: wrap the shortfall from QUAI first, if it can.
                Ok(q) if q.insufficient => {
                    let prewrap = match wallet_core::amount::parse_amount(&amount, 18) {
                        Ok(atoms) if session.is_wquai(&from).await.unwrap_or(false) => {
                            session.prewrap_quai(account.as_deref(), &from, atoms, "swap", None).await
                        }
                        _ => Ok(None),
                    };
                    match prewrap {
                        Ok(Some(review)) => Ok(review),
                        Ok(None) => Err(wallet_core::CoreError::Insufficient(
                            q.warnings.iter().find(|w| w.starts_with("you have")).cloned().unwrap_or_else(|| "balance too low".into()),
                        )),
                        Err(e) => Err(e),
                    }
                }
                Ok(q) if q.approval_needed => session.review_swap_approval(account.as_deref(), &from, &to, &amount, None).await,
                Ok(_) => session.review_swap(account.as_deref(), &from, &to, &amount, slippage, deadline, None).await,
                Err(e) => Err(e),
            }
        }
        Prepare::NftBuyNext { account, contract, token_id, price } => {
            match session.check_listing(account.as_deref(), &contract, &token_id).await {
                Ok(c) if !c.valid => Err(wallet_core::CoreError::Rejected(format!("cannot buy: {}", c.problems.join("; ")))),
                Ok(c) if c.buyer_module_approval_needed => session.review_zora_module_approval(account.as_deref(), None).await,
                Ok(c) if c.buyer_token_approval_needed => {
                    session.review_zora_token_approval(account.as_deref(), &contract, &token_id, None).await
                }
                Ok(c) => {
                    let expected = price.or_else(|| c.ask.as_ref().map(|a| a.price.clone()));
                    session.review_nft_buy(account.as_deref(), &contract, &token_id, expected.as_deref(), None).await
                }
                Err(e) => Err(e),
            }
        }
        Prepare::NftListNext { account, contract, token_id, price, currency } => {
            match session.seller_state(account.as_deref(), &contract, &token_id).await {
                Ok(s) if !s.owns => Err(wallet_core::CoreError::Rejected("this account no longer owns the item".into())),
                Ok(_) if price.is_none() => session.review_nft_unlist(account.as_deref(), &contract, &token_id, None).await,
                Ok(s) if !s.module_approved => session.review_zora_module_approval(account.as_deref(), None).await,
                Ok(s) if !s.helper_approved => session.review_zora_collection_approval(account.as_deref(), &contract, None).await,
                Ok(_) => {
                    let price = price.unwrap_or_default();
                    session.review_nft_list(account.as_deref(), &contract, &token_id, &price, &currency, None).await
                }
                Err(e) => Err(e),
            }
        }
        Prepare::NftTransfer { account, contract, token_id, to, quantity } => {
            session.review_nft_transfer(account.as_deref(), &contract, &token_id, &to, quantity.as_deref(), None).await
        }
    }
}

/// Run a refresh step, dropping it the moment the user asks the worker for something. `None` means
/// it was dropped.
///
/// Only steps that are safe to stop at any await go through here: reads, and writes that record a
/// finished value (`INSERT OR IGNORE` activity, a key set after its read). A Qi refresh is not one
/// of them — cancelled, the SDK invalidates its checkpoint until a full rescan, and Qi cannot be
/// spent meanwhile — so it runs to completion between the checks.
async fn unless_urgent<T>(inbox: &mut Inbox, step: impl std::future::Future<Output = T>) -> Option<T> {
    tokio::select! {
        biased;
        value = step => Some(value),
        () = inbox.urgent_arrives() => None,
    }
}

/// Where new block heights come from between refreshes.
struct Heads {
    /// Heights pushed by the explorer's block stream.
    stream: Option<(tokio::sync::watch::Receiver<u64>, tokio::task::JoinHandle<()>)>,
    polled: u64,
    last_poll: std::time::Instant,
}

impl Heads {
    fn start(session: &Session) -> Heads {
        let explorer = wallet_core::explorer::Explorer::for_network(&session.network);
        let wanted =
            !session.monitoring() && session.config.data_policy().explorer && explorer.backend == wallet_core::explorer::Backend::Quai;
        let stream = wanted.then(|| {
            let (tx, rx) = tokio::sync::watch::channel(0u64);
            let url = explorer.absolute("/api/stream/live");
            let task = tokio::spawn(async move {
                loop {
                    let _ = wallet_core::http::event_stream(&url, |data| {
                        let height = serde_json::from_str::<serde_json::Value>(data)
                            .ok()
                            .and_then(|v| v["items"].as_array()?.iter().filter_map(|b| b["height"].as_str()?.parse::<u64>().ok()).max());
                        if let Some(h) = height {
                            tx.send_if_modified(|seen| std::mem::replace(seen, h.max(*seen)) < h);
                        }
                        !tx.is_closed()
                    })
                    .await;
                    if tx.is_closed() {
                        return;
                    }
                    tokio::time::sleep(Duration::from_secs(30)).await;
                }
            });
            (rx, task)
        });
        Heads { stream, polled: 0, last_poll: std::time::Instant::now() }
    }

    fn stop(&mut self) {
        if let Some((_, task)) = self.stream.take() {
            task.abort();
        }
    }

    /// The newest height seen: the explorer stream's when it runs, else polled from the node
    /// every 2 s (a ~1 ms read on a LAN node, one small call on the public RPC). Without either,
    /// a wallet with explorer lookups off only refreshed on the 15 s idle timer.
    async fn latest(&mut self, session: &Session) -> u64 {
        if session.monitoring() || self.stream.is_none() {
            let reader = &session.node;
            if self.last_poll.elapsed() >= Duration::from_secs(2) {
                self.last_poll = std::time::Instant::now();
                if let Ok(Ok(v)) = tokio::time::timeout(Duration::from_secs(1), reader.raw("quai_blockNumber", serde_json::json!([]))).await
                    && let Some(h) = v.as_str().and_then(|t| u64::from_str_radix(t.trim_start_matches("0x"), 16).ok())
                {
                    self.polled = self.polled.max(h);
                }
            }
            return self.polled;
        }
        self.stream.as_ref().map_or(0, |(rx, _)| *rx.borrow())
    }

    /// Least time between block-triggered refreshes: every block. A block every ~5 s is the
    /// pace the chain moves at, on a monitoring node or the public RPC alike.
    fn min_gap(&self, session: &Session) -> Duration {
        if session.monitoring() { Duration::from_secs(4) } else { Duration::from_secs(5) }
    }
}

impl Drop for Heads {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The worker's command queue.
struct Inbox {
    rx: tokio::sync::mpsc::UnboundedReceiver<Cmd>,
    backlog: VecDeque<Cmd>,
    /// When the oldest urgent command still queued arrived, for `QW_TIMING_LOG`.
    waiting_since: Option<std::time::Instant>,
}

impl Inbox {
    fn new(rx: tokio::sync::mpsc::UnboundedReceiver<Cmd>) -> Inbox {
        Inbox { rx, backlog: VecDeque::new(), waiting_since: None }
    }

    fn push(&mut self, cmd: Cmd) {
        if urgent(&cmd) && self.waiting_since.is_none() {
            self.waiting_since = Some(std::time::Instant::now());
        }
        self.backlog.push_back(cmd);
    }

    fn drain(&mut self) {
        while let Ok(c) = self.rx.try_recv() {
            self.push(c);
        }
    }

    /// Resolves once an urgent command is queued, or the UI is gone.
    async fn urgent_arrives(&mut self) {
        while !self.urgent() {
            match self.rx.recv().await {
                Some(c) => self.push(c),
                None => return,
            }
        }
    }

    /// Whether something urgent is waiting (checked between refresh steps).
    fn urgent(&mut self) -> bool {
        self.drain();
        self.backlog.iter().any(urgent)
    }

    /// The next command, urgent ones first; queued refreshes merge into one. `Ok(None)` when
    /// nothing arrived within `wait`, `Err(())` once the UI is gone.
    async fn next(&mut self, wait: Duration) -> Result<Option<Cmd>, ()> {
        self.drain();
        if self.backlog.is_empty() {
            match tokio::time::timeout(wait, self.rx.recv()).await {
                Ok(Some(c)) => self.push(c),
                Ok(None) => return Err(()),
                Err(_) => return Ok(None),
            }
            self.drain();
        }
        let at = self.backlog.iter().position(urgent).unwrap_or(0);
        let cmd = self.backlog.remove(at);
        if cmd.as_ref().is_some_and(urgent)
            && let Some(since) = self.waiting_since.take()
        {
            // How long the user waited for the worker to pick their command up, and for what:
            // a refresh that gets cut short for a command has to start again, so which commands
            // arrive mid-refresh is worth being able to see.
            wallet_core::diag::timing(&format!("worker.queue_wait.{}", cmd.as_ref().map_or("?", Cmd::kind)), since);
            if self.backlog.iter().any(urgent) {
                self.waiting_since = Some(std::time::Instant::now());
            }
        }
        if let Some(Cmd::Refresh { mut full }) = cmd {
            self.backlog.retain(|c| match c {
                Cmd::Refresh { full: f } => {
                    full |= *f;
                    false
                }
                _ => true,
            });
            return Ok(Some(Cmd::Refresh { full }));
        }
        Ok(cmd)
    }
}

async fn run(
    registry: Registry,
    config: AppConfig,
    meta: WalletMeta,
    network_id: String,
    mut inbox: Inbox,
    events: Sender<Ev>,
    wake: impl Fn() + Send + 'static,
    mut lane: QiLane,
) {
    let send = |ev: Ev| {
        let _ = events.send(ev);
        super::term::wake();
        wake();
    };
    let mut meta = meta;
    let open = |m: &wallet_core::registry::WalletMeta, net: &str| -> Result<Session, String> {
        let profile = config.network(net).map_err(|e| e.to_string())?;
        Session::open(registry.clone(), config.clone(), m.clone(), profile).map_err(|e| e.to_string())
    };
    let mut session = match open(&meta, &network_id) {
        Ok(s) => s,
        Err(e) => {
            send(Ev::Error(e));
            return;
        }
    };
    // Local state (activity, contacts) shows before any network answer.
    let mut dash = Dashboard {
        meta: Some(session.meta.clone()),
        network_id: session.network.id.clone(),
        network_name: session.network.name.clone(),
        explorer: session.network.explorer.clone(),
        networks: session.config.networks().into_iter().map(|n| (n.id, n.name)).collect(),
        activity: session.app.activity(&session.network.id, 200).unwrap_or_default(),
        ..Dashboard::default()
    };
    refresh_local(&mut session, &mut dash);
    restore_remembered(&session, &mut dash);
    wallet_core::diag::mark("startup.dashboard_local");
    send(Ev::Dashboard(Box::new(dash.clone())));
    let mut stages = Stages::default();
    if let Some(note) = session.use_monitor().await {
        send(Ev::Info(note));
    }
    // Kept only while unlocked so a network switch can re-open the vault for the new session.
    let mut unlock_password: Option<Zeroizing<String>> = None;
    let mut last_refresh = std::time::Instant::now() - std::time::Duration::from_secs(3600);
    let mut last_monitor_check = std::time::Instant::now();
    // New blocks trigger a refresh: from the monitoring node when there is one, else from the
    // explorer's block stream (when explorer lookups are allowed); a 15 s timer otherwise.
    let mut heads = Heads::start(&session);
    // The newest height when the last block-triggered refresh started (sources can lead the
    // main RPC by a block, so compare with what the watcher saw, not the dashboard).
    let mut refreshed_head = 0u64;
    // The newest height the screens have been told about.
    let mut announced_head = 0u64;
    loop {
        let Ok(cmd) = inbox.next(Duration::from_millis(250)).await else { return };
        let now = std::time::Instant::now();
        let seen = heads.latest(&session).await;
        // Every screen that shows chain state re-reads on a new block, so they hear about it the
        // moment it is seen, not when this worker's own refresh finishes.
        if seen > announced_head {
            announced_head = seen;
            send(Ev::Head(seen));
        }
        let new_block = seen > refreshed_head;
        let gap = now.duration_since(last_refresh);
        let cmd = match cmd {
            Some(c) => c,
            None if gap >= IDLE_REFRESH => Cmd::Refresh { full: false },
            None if new_block && gap >= heads.min_gap(&session) => {
                refreshed_head = seen;
                Cmd::Refresh { full: false }
            }
            None => continue,
        };
        let adds_account = matches!(cmd, Cmd::AddAccount(_) | Cmd::ImportKey { .. } | Cmd::WatchAddress { .. });
        // Local edits the dashboard should reflect right away (a lightweight refresh follows).
        let mutates = matches!(
            cmd,
            Cmd::AddAccount(_)
                | Cmd::ImportKey { .. }
                | Cmd::WatchAddress { .. }
                | Cmd::RenameAccount { .. }
                | Cmd::NewQiAddress(_)
                | Cmd::ScanQi { .. }
                | Cmd::DiscoverMailbox
                | Cmd::SaveContact { .. }
                | Cmd::ScanPeer(_)
                | Cmd::AcceptOffer(_)
                | Cmd::DeclineOffer(_)
                | Cmd::RemoveContact(_)
                | Cmd::ImportToken(_)
                | Cmd::DiscoverTokens
                | Cmd::MarkRead
                | Cmd::Journal
        );
        match cmd {
            Cmd::Order(request) => {
                // The background check says nothing unless an order becomes reachable.
                let quiet = matches!(request, super::order_ui::Request::Watch);
                if !quiet {
                    send(Ev::Busy(Some("checking limit orders…".into())));
                }
                match super::order_ui::handle(&mut session, request).await {
                    Ok(event) => send(event),
                    Err(e) if !quiet => send(Ev::Error(e.to_string())),
                    Err(_) => {}
                }
                if !quiet {
                    send(Ev::Busy(None));
                }
            }
            Cmd::Shutdown => {
                session.lock();
                return;
            }
            Cmd::Refresh { full } => {
                send(Ev::Busy(Some(if full { "refreshing everything…".into() } else { "syncing…".into() })));
                // A monitoring node that failed (or was down at start) is re-checked every few minutes.
                if session.network.monitor.is_some() && !session.monitoring() && last_monitor_check.elapsed() >= Duration::from_secs(180) {
                    last_monitor_check = std::time::Instant::now();
                    let _ = session.use_monitor().await;
                }
                // Notifications wait for the refreshed dashboard, so a notice never announces
                // something the screen does not show yet.
                let mut deferred: Vec<Ev> = Vec::new();
                last_refresh =
                    if refresh(&mut session, &mut dash, full, false, &mut inbox, &mut stages, &mut deferred, &send, &mut lane).await {
                        std::time::Instant::now()
                    } else {
                        // Cut short for the user: due again as soon as the queue is empty, not on a
                        // timer. Backdating by a fixed 12 s of the 15 s gap meant an interrupted
                        // refresh waited out the remaining 3 s doing nothing — and a command arriving
                        // a couple of hundred milliseconds into a launch is enough to hit it, which is
                        // exactly what put a 3.8 s tail on an otherwise 0.7 s warm start. The loop
                        // only refreshes when nothing is queued, so this cannot spin.
                        std::time::Instant::now() - IDLE_REFRESH
                    };
                send(Ev::Busy(None));
                send(Ev::Dashboard(Box::new(dash.clone())));
                for ev in deferred {
                    send(ev);
                }
            }
            Cmd::UseKeys { wallet, keys, password } => {
                // Queued behind the switch that opened this wallet, so a mismatch means that
                // switch failed; these keys are not this session's.
                if session.meta.id == wallet {
                    session.use_keys(*keys);
                    unlock_password = Some(password);
                    dash.unlocked = true;
                } else {
                    send(Ev::KeysRefused);
                }
            }
            Cmd::Lock => {
                session.lock();
                unlock_password = None;
                dash.unlocked = false;
                dash.peers.clear();
                dash.offers.clear();
                send(Ev::Locked);
            }
            // The signing lane's: the UI routes these to it, so a transaction is prepared beside
            // whatever this worker is doing rather than after it.
            Cmd::Prepare(_) | Cmd::Commit(_) | Cmd::Discard(_) => {}
            Cmd::Quote { direction, amount } => match session.conversion_quote(&direction, &amount).await {
                Ok(q) => send(Ev::Quote(Box::new(q))),
                Err(e) => send(Ev::Error(e.to_string())),
            },
            Cmd::QiMax { key, wrapping, account, slippage } => {
                let result = session.quote_qi_special_max(wrapping, account.as_deref(), slippage, None).await.map_err(|e| e.to_string());
                send(Ev::QiMax { key, result });
            }
            Cmd::Pnl => send(Ev::Pnl(session.pnl().await.map(Box::new).map_err(|e| e.to_string()))),
            Cmd::SplitQuote { key, account, from, to, amount, slippage } => {
                let result = session
                    .swap_split_quote(account.as_deref(), &from, &to, &amount, slippage, 20)
                    .await
                    .map(Box::new)
                    .map_err(|e| e.to_string());
                send(Ev::SplitQuote { key, result });
            }
            // What a send destination turned out to be. A failure is an answer too — "nothing
            // known" — and never an error banner over a form someone is still typing into.
            Cmd::InspectContract { address } => {
                let found = session.inspect_contract(&address).await.ok();
                send(Ev::Contract { address, found: Box::new(found) });
            }
            // A transaction went out: re-read now, so it shows as pending straight away.
            Cmd::Journal => {}
            Cmd::Committed => {
                refresh(&mut session, &mut dash, false, true, &mut inbox, &mut stages, &mut Vec::new(), &send, &mut lane).await;
                send(Ev::Dashboard(Box::new(dash.clone())));
            }
            Cmd::AddAccount(label) => {
                simple(&send, session.add_account(label.as_deref()).map(|a| format!("added {} {}", a.label, a.address)))
            }
            Cmd::ImportKey { label, key, password } => {
                let label = label.unwrap_or_else(|| {
                    format!("Imported {}", session.meta.quai_accounts.iter().filter(|a| a.hd_index.is_none()).count() + 1)
                });
                let r = session
                    .import_key(&password, &key, &label)
                    .map(|a| format!("imported {label} {a} · back the wallet up again: its phrase does not cover this key"));
                simple(&send, r);
            }
            Cmd::WatchAddress { address, label } => {
                simple(&send, session.add_watch_address(&address, label.as_deref()).map(|a| format!("watching {a}")))
            }
            Cmd::RenameAccount { account, label } => simple(&send, session.rename_account(&account, &label).map(|_| "renamed".into())),
            Cmd::NewQiAddress(label) => simple(&send, session.new_qi_address(label.as_deref()).map(|a| format!("new Qi address {a}"))),
            Cmd::ScanQi { deep } => {
                // In the Qi lane, so the wallet stays usable for the 10–15 s a scan takes.
                lane.ask(&session, QiWork::Scan(deep), true);
                send(Ev::Info(if deep.is_some() {
                    "deep scanning Qi in the background…".into()
                } else {
                    "scanning Qi (gap 50) in the background…".into()
                }));
            }
            Cmd::QiSynced(done) => {
                lane.in_flight = lane.in_flight.saturating_sub(1);
                // A pass for a wallet or network the user has since left answers nothing here.
                if done.key.wallet != session.meta.id || done.key.network != session.network.id {
                    continue;
                }
                wallet_core::diag::mark("startup.dashboard_qi");
                // The lane wrote the store; this session reads what it wrote.
                dash.qi = session.qi_summary().ok();
                let head = dash.health.as_ref().map_or(0, |h| h.height);
                // Locked Qi coins are part of this answer, so the lock list is rebuilt with it.
                read_locks(&mut session, &mut dash, head);
                send(Ev::Dashboard(Box::new(dash.clone())));
                if done.announce {
                    simple(&send, done.result.map(|h| format!("Qi scan complete at block {h}")).map_err(wallet_core::CoreError::Network));
                }
            }
            Cmd::DiscoverMailbox => {
                send(Ev::Busy(Some("reading payment mailbox…".into())));
                let r = session.discover_mailbox().await.map(|s| mailbox_note(&s));
                send(Ev::Busy(None));
                simple(&send, r);
            }
            Cmd::SaveContact { original, name, address, code, note } => {
                let editing = original.is_some();
                match session.save_contact(original.as_deref(), &name, address.as_deref(), code.as_deref(), &note) {
                    Ok(contact) => {
                        send(Ev::Ack(format!("{} {}", if editing { "updated" } else { "saved" }, contact.name)));
                        refresh_local(&mut session, &mut dash);
                        send(Ev::Dashboard(Box::new(dash.clone())));
                        // Look for payments already sent on a new code's channel.
                        if let Some(code) = contact.payment_code.filter(|_| session.is_unlocked()) {
                            send(Ev::Busy(Some(format!("scanning {}'s payment channel…", contact.name))));
                            if let Err(e) = session.scan_peer(&code, None).await {
                                send(Ev::Info(format!("channel scan: {e}")));
                            }
                            lane.ask(&session, QiWork::Refresh, false);
                            send(Ev::Busy(None));
                        }
                    }
                    Err(e) => send(Ev::Error(e.to_string())),
                }
            }
            Cmd::ScanPeer(code) => {
                send(Ev::Busy(Some("scanning payment channel…".into())));
                let r = session.scan_peer(&code, None).await.map(|(_, n)| format!("channel scanned ({n} new address(es))"));
                lane.ask(&session, QiWork::Refresh, false);
                send(Ev::Busy(None));
                simple(&send, r);
            }
            Cmd::AcceptOffer(code) => {
                send(Ev::Busy(Some("registering and scanning the channel…".into())));
                let r = session
                    .accept_channel_offer(&code)
                    .await
                    .map(|(_, n)| format!("channel accepted ({n} address(es) with payments) · its Qi arrives with the next scan"));
                lane.ask(&session, QiWork::Refresh, false);
                send(Ev::Busy(None));
                simple(&send, r);
            }
            Cmd::DeclineOffer(code) => simple(
                &send,
                session
                    .decline_channel_offer(&code)
                    .map(|()| format!("declined {} · it will not be offered again", wallet_core::session::short_code(&code))),
            ),
            Cmd::RemoveContact(name) => simple(&send, session.app.remove_contact(&name).map(|_| format!("removed {name}"))),
            Cmd::ImportToken(addr) => simple(&send, session.import_token(&addr).await.map(|t| format!("imported {}", t.symbol))),
            Cmd::DiscoverTokens => {
                send(Ev::Busy(Some("looking for tokens you hold…".into())));
                let result = async {
                    let data = session.data_ctx()?;
                    if !data.policy.explorer {
                        return Err(wallet_core::CoreError::Rejected("explorer lookups are turned off (System › Data sources)".into()));
                    }
                    let known: std::collections::HashSet<String> =
                        session.app.tokens(&session.network.id, true)?.into_iter().map(|t| t.address.to_lowercase()).collect();
                    let mut found = std::collections::BTreeSet::new();
                    for owner in session.quai_owner_addresses() {
                        for h in data.explorer.holdings(&owner).await? {
                            if h.kind == wallet_core::explorer::TokenKind::Erc20 && !known.contains(&h.token) {
                                found.insert(h.token);
                            }
                        }
                    }
                    let mut imported = Vec::new();
                    for address in found {
                        if let Ok(t) = session.import_token(&address).await {
                            imported.push(t.symbol);
                        }
                    }
                    Ok(if imported.is_empty() { "no new tokens found".to_string() } else { format!("imported {}", imported.join(", ")) })
                }
                .await;
                send(Ev::Busy(None));
                simple(&send, result);
            }
            Cmd::SwitchNetwork(net) => {
                send(Ev::Busy(Some(format!("connecting to {net}…"))));
                match open(&meta, &net) {
                    Ok(mut s) => {
                        session.lock();
                        if let Some(p) = &unlock_password {
                            let _ = s.unlock(p);
                        }
                        session = s;
                        if let Some(note) = session.use_monitor().await {
                            send(Ev::Info(note));
                        }
                        // A different wallet or network answers nothing the old stage timers
                        // knew: every stage is due again.
                        stages.clear();
                        heads.stop();
                        heads = Heads::start(&session);
                        // Show the new network at once, with no balances carried over from the old one.
                        dash = Dashboard {
                            meta: Some(session.meta.clone()),
                            network_id: session.network.id.clone(),
                            network_name: session.network.name.clone(),
                            unlocked: session.is_unlocked(),
                            networks: session.config.networks().into_iter().map(|n| (n.id, n.name)).collect(),
                            ..Dashboard::default()
                        };
                        send(Ev::Dashboard(Box::new(dash.clone())));
                        send(Ev::Busy(Some(format!("syncing {}… (first visit scans Qi)", session.network.name))));
                        refresh(&mut session, &mut dash, false, true, &mut inbox, &mut stages, &mut Vec::new(), &send, &mut lane).await;
                        last_refresh = std::time::Instant::now();
                        send(Ev::Busy(None));
                        send(Ev::Dashboard(Box::new(dash.clone())));
                        send(Ev::Ack(format!("now on {}", session.network.name)));
                    }
                    Err(e) => {
                        send(Ev::Busy(None));
                        send(Ev::Error(e));
                    }
                }
            }
            Cmd::SwitchWallet(id) => {
                send(Ev::Busy(Some("opening the wallet…".into())));
                let picked = registry.resolve(Some(&id), None).map_err(|e| e.to_string());
                match picked.and_then(|m| open(&m, &session.network.id).map(|s| (m, s))) {
                    Ok((new_meta, s)) => {
                        // The old wallet's keys go now; the new one's password is its own.
                        session.lock();
                        unlock_password = None;
                        meta = new_meta;
                        session = s;
                        // A watch-only wallet has no keys to unlock, so it must not be asked
                        // for a password it does not have.
                        let needs_password = session.meta.kind != wallet_core::registry::WalletKind::Watch;
                        dash = Dashboard {
                            meta: Some(session.meta.clone()),
                            network_id: session.network.id.clone(),
                            network_name: session.network.name.clone(),
                            explorer: session.network.explorer.clone(),
                            unlocked: !needs_password,
                            networks: session.config.networks().into_iter().map(|n| (n.id, n.name)).collect(),
                            activity: session.app.activity(&session.network.id, 200).unwrap_or_default(),
                            ..Dashboard::default()
                        };
                        send(Ev::Dashboard(Box::new(dash.clone())));
                        if needs_password {
                            send(Ev::Locked);
                        } else {
                            send(Ev::Unlocked);
                        }
                        // The new wallet is on screen before the network is consulted: checking
                        // the monitoring endpoint costs up to 2 s, and nothing here needs it.
                        if let Some(note) = session.use_monitor().await {
                            send(Ev::Info(note));
                        }
                        // A different wallet or network answers nothing the old stage timers
                        // knew: every stage is due again.
                        stages.clear();
                        heads.stop();
                        heads = Heads::start(&session);
                        refresh(&mut session, &mut dash, false, true, &mut inbox, &mut stages, &mut Vec::new(), &send, &mut lane).await;
                        last_refresh = std::time::Instant::now();
                        send(Ev::Busy(None));
                        send(Ev::Dashboard(Box::new(dash.clone())));
                        send(Ev::Ack(format!("opened {}", session.meta.name)));
                    }
                    Err(e) => {
                        send(Ev::Busy(None));
                        send(Ev::Error(e));
                    }
                }
            }
            Cmd::ReadConversation { peer, blocks } => {
                let result = session.read_conversation(&peer, blocks).await.map_err(|e| e.to_string());
                send(Ev::Conversation { peer, result });
            }
            Cmd::MarkRead => {
                let _ = session.app.mark_notifications_read();
            }
            Cmd::Chat(op) => {
                let note = match op {
                    ChatOp::Load => None,
                    ChatOp::Toggle { target, label } => Some(match session.toggle_chat_subscription(&target) {
                        Ok(true) => format!("notifying you of new messages in {label}"),
                        Ok(false) => format!("no more notifications from {label}"),
                        Err(e) => e.to_string(),
                    }),
                    ChatOp::Pin { target, label } => Some(match session.set_chat_pin(target.as_deref()) {
                        Ok(()) if target.is_some() => format!("{label} pinned beside every screen · ` writes to it"),
                        Ok(()) => format!("{label} unpinned"),
                        Err(e) => e.to_string(),
                    }),
                };
                send(Ev::Chat { subs: session.chat_subscriptions(), pin: session.chat_pin(), note });
            }
            Cmd::ChatNews { dms_only } => {
                if let Ok(news) = session.chat_news_where(dms_only).await
                    && !news.is_empty()
                {
                    send(Ev::ChatNews(news.into_iter().map(|n| (n.title, n.body)).collect()));
                }
            }
            Cmd::ExportPhrase(password) => match session.export_mnemonic(&password) {
                Ok((phrase, _)) => send(Ev::Secret(phrase)),
                Err(e) => send(Ev::Error(e.to_string())),
            },
            Cmd::Backup { path, password } => {
                let r = extras::create_backup(&session.registry, &session.config, &session.meta, std::path::Path::new(&path), &password)
                    .map(|i| format!("backup written ({} networks)", i.networks.len()));
                simple(&send, r);
            }
        }
        if mutates {
            refresh_local(&mut session, &mut dash);
            send(Ev::Dashboard(Box::new(dash.clone())));
        }
        // A new account has no balance until the network is read, and waiting for the next block
        // left it missing from Accounts for seconds after it was added: refresh on the next idle
        // turn of this loop instead.
        if adds_account {
            last_refresh = std::time::Instant::now() - IDLE_REFRESH;
        }
    }
}

/// Re-read local state only (no RPC) after an edit.
fn refresh_local(session: &mut Session, dash: &mut Dashboard) {
    dash.meta = Some(session.meta.clone());
    dash.ops_at = ops_stamp();
    dash.ops = session.app.operations(&session.network.id, 200).unwrap_or_default();
    dash.contacts = session.app.contacts().unwrap_or_default();
    dash.contact_addresses = dash
        .contacts
        .iter()
        .flat_map(|c| session.app.contact_addresses(c.id).unwrap_or_default().into_iter().map(move |a| (a, c.name.clone())))
        .collect();
    dash.notifications = session.app.notifications(50).unwrap_or_default();
    dash.qi_addresses = session.qi_receive_addresses().unwrap_or_default();
    dash.peers = if session.is_unlocked() { session.peers().unwrap_or_default() } else { Vec::new() };
    dash.offers = if session.is_unlocked() { session.channel_offers().unwrap_or_default() } else { Vec::new() };
    if let Ok(q) = session.qi_summary() {
        dash.qi = Some(q);
    }
}

pub(crate) fn incoming_amount(a: &Activity) -> String {
    let v: U256 = a.amount.parse().unwrap_or_default();
    match a.asset.as_str() {
        "QI" => wallet_core::amount::qi(v),
        "QUAI" => wallet_core::amount::quai(v),
        _ => wallet_core::amount::format_amount_short(v, a.detail.get("decimals").and_then(|d| d.as_u64()).unwrap_or(18) as u8, 6),
    }
}

/// What a mailbox read found, in a line.
fn mailbox_note(s: &wallet_core::ops::MailboxSummary) -> String {
    use wallet_core::amount::count;
    let mut note = format!("{} announced · {}", count(s.senders.len(), "sender"), count(s.registered.len(), "channel"));
    if s.pending > 0 {
        note.push_str(&format!(" · {} waiting for you", count(s.pending, "offer")));
    }
    if s.refused > 0 {
        note.push_str(&format!(" · {} refused: no room for more channels", s.refused));
    }
    note
}

fn simple(send: &impl Fn(Ev), r: wallet_core::Result<String>) {
    match r {
        Ok(m) => send(Ev::Ack(m)),
        Err(e) => send(Ev::Error(e.to_string())),
    }
}

/// How often each network stage runs when nothing asked for it: a new block, or the idle timer.
///
/// Every stage used to run on every block, which is why an idle session spent hundreds of calls
/// saying nothing had changed. The two the user actually watches — the node's height and their
/// QUAI balances — still run every time; the rest move at the speed they can actually change at.
/// A refresh the user asked for, and the one after a commit, ignore all of this (`force`).
/// Token and wrapped balances: one multicall, so every block, like QUAI.
const TOKENS_EVERY: Duration = Duration::from_secs(5);
const QI_EVERY: Duration = Duration::from_secs(15);
/// The price feed's own cache holds a minute; asking more often only reads the same answer.
const PRICE_EVERY: Duration = Duration::from_secs(60);
/// Locked conversion balances: three calls each, and they only move when a conversion settles.
const LOCKED_EVERY: Duration = Duration::from_secs(60);
/// The node's gas price, client version and block order: System-screen detail, not per-block news.
const NODE_DETAIL_EVERY: Duration = Duration::from_secs(60);
/// How long the wallet waits before refreshing on its own when no block has arrived.
const IDLE_REFRESH: Duration = Duration::from_secs(15);
/// Reconciling open operations and observing incoming activity.
const TRACK_EVERY: Duration = Duration::from_secs(5);
/// Scanning payment channels for senders never seen before.
const PAYMENT_SYNC_EVERY: Duration = Duration::from_secs(90);

/// When each refresh stage last finished.
#[derive(Default)]
struct Stages(std::collections::HashMap<&'static str, std::time::Instant>);

impl Stages {
    /// Whether a stage should run now: forced, never run, or last run longer than `every` ago.
    fn due(&self, stage: &'static str, every: Duration, force: bool) -> bool {
        force || self.0.get(stage).is_none_or(|at| at.elapsed() >= every)
    }

    fn done(&mut self, stage: &'static str) {
        self.0.insert(stage, std::time::Instant::now());
    }

    /// Mark a stage done, but as though it had run `by` ago — a pass that stopped early is not a
    /// completed one, so it comes round again soon rather than waiting out its full interval.
    fn done_backdated(&mut self, stage: &'static str, by: Duration) {
        self.0.insert(stage, std::time::Instant::now().checked_sub(by).unwrap_or_else(std::time::Instant::now));
    }

    /// Forget every stage (a wallet or network switch: none of it carries over).
    fn clear(&mut self) {
        self.0.clear();
    }
}

/// How old a remembered dashboard may be and still be worth painting.
///
/// A day, not the cache's usual week. These numbers appear with nothing marking them as old, and
/// they are only on screen for the half-second before the real ones land — so the question is
/// what is a better first impression than an empty wallet, and a week-old balance is not. With a
/// daemon running they are seconds old, because it keeps them current.
const REMEMBERED_MAX_AGE: u64 = 86_400;

/// Paint the last known numbers before the node has said anything.
///
/// Display only, and replaced stage by stage as the real values arrive. Without this the first
/// second of a launch shows an empty wallet, which reads as "everything is gone" rather than
/// "nothing has answered yet".
fn restore_remembered(session: &Session, dash: &mut Dashboard) {
    let Some((cache, age)) = session.remembered_dashboard(REMEMBERED_MAX_AGE) else { return };
    dash.accounts = cache.accounts;
    dash.tokens = cache.tokens;
    dash.locks = cache.locks;
    dash.wrap = cache.wrap;
    // `refreshed_at` stays zero: it gates the change animations, and numbers restored from a
    // cache must not make the first real refresh flash every row that moved while we were away.
    // How stale what we just painted was. Seconds means a daemon is keeping it current; hours
    // means this is the last session's dashboard.
    wallet_core::diag::count("startup.remembered_age_secs", age);
}

/// Keep what this refresh learned, for the next launch.
fn remember_dashboard(session: &Session, dash: &Dashboard) {
    session.remember_dashboard(&wallet_core::session::DashboardCache {
        accounts: dash.accounts.clone(),
        tokens: dash.tokens.clone(),
        locks: dash.locks.clone(),
        wrap: dash.wrap.clone(),
    });
}

/// Everything the dashboard shows that costs nothing: the wallet's own database and the Qi
/// summary from the last scan. Re-read on every refresh, because it is free.
fn read_local(session: &mut Session, dash: &mut Dashboard) {
    dash.meta = Some(session.meta.clone());
    dash.network_id = session.network.id.clone();
    dash.network_name = session.network.name.clone();
    dash.explorer = session.network.explorer.clone();
    dash.unlocked = session.is_unlocked();
    dash.networks = session.config.networks().into_iter().map(|n| (n.id, n.name)).collect();
    refresh_local(session, dash);
    dash.activity = session.app.activity(&session.network.id, 200).unwrap_or_default();
}

/// Time-locked balances, built from the head and the locked balances the balance stage already
/// read. Local work only, so it is recomputed whenever either of its inputs changes.
fn read_locks(session: &mut Session, dash: &mut Dashboard, head: u64) {
    let locked: Vec<(String, wallet_core::sdk::U256)> = dash.accounts.iter().map(|a| (a.label.clone(), a.locked)).collect();
    if let Ok(items) = session.locks_from(head, &locked) {
        dash.locks = items;
    }
}

/// Reconcile open operations, observe incoming activity and scan payment channels. Produces
/// notifications; the dashboard reads none of it.
///
/// This used to run before the dashboard's own reads, so every launch showed its balances a
/// couple of hundred milliseconds later than it could have, waiting on work about transactions
/// that had already happened. It runs after them now.
#[allow(clippy::too_many_arguments)]
async fn reconcile(
    session: &mut Session,
    head: u64,
    full: bool,
    force: bool,
    inbox: &mut Inbox,
    stages: &mut Stages,
    deferred: &mut Vec<Ev>,
    send: &dyn Fn(Ev),
    lane: &mut QiLane,
) {
    if !stages.due("track", TRACK_EVERY, force) || inbox.urgent() {
        return;
    }
    let t = std::time::Instant::now();
    // Sent Quai transactions waiting for a block are the pending lane's, checked every few seconds.
    let tracked = unless_urgent(inbox, session.track_scoped(head, wallet_core::track::OpScope::Rest)).await;
    let completed = tracked.is_some();
    wallet_core::diag::timing("refresh.track", t);
    if let Some(Ok(report)) = tracked {
        for sale in report.sales {
            deferred.push(Ev::Notify { title: "NFT sold".into(), body: sale, listed: true });
        }
        for c in report.changes {
            deferred.push(Ev::Notify { title: format!("Transaction {}", c.to.as_str()), body: c.message, listed: true });
        }
        match report.incoming.as_slice() {
            [] => {}
            [a] => {
                let body = if session.config.show_amounts_in_notifications {
                    format!("{} {} received", incoming_amount(a), a.asset)
                } else {
                    format!("{} received", a.asset)
                };
                deferred.push(Ev::Notify { title: "Incoming payment".into(), body, listed: true });
            }
            many => deferred.push(Ev::Notify {
                title: "Incoming payments".into(),
                body: format!("{} new incoming transfers", many.len()),
                listed: true,
            }),
        }
    }
    // Private payments from senders we have never seen arrive via the mailbox. It scans a chain
    // per channel, so it yields as soon as the user wants the wallet for something else; what it
    // did not reach, the next pass does.
    if session.is_unlocked() && stages.due("payment_sync", PAYMENT_SYNC_EVERY, full) && !inbox.urgent() {
        stages.done("payment_sync");
        let t = std::time::Instant::now();
        let synced = session.sync_payment_channels_until(&mut || inbox.urgent()).await;
        wallet_core::diag::timing("refresh.payment_sync", t);
        if synced.as_ref().is_ok_and(|s| s.stopped) {
            stages.done_backdated("payment_sync", PAYMENT_SYNC_EVERY.saturating_sub(Duration::from_secs(10)));
        }
        match synced {
            Ok(sync) => {
                // New channel addresses are in the store; their coins arrive with the next Qi
                // pass, which runs in the lane rather than here.
                if sync.scanned > 0 {
                    lane.ask(session, QiWork::Refresh, false);
                }
                for offer in &sync.new_offers {
                    let (title, body) = offer.notice();
                    // Offers are not written to the list on this side: the desktop is the terminal's.
                    deferred.push(Ev::Notify { title, body, listed: false });
                }
            }
            Err(e) => send(Ev::Info(format!("payment sync: {e}"))),
        }
    }
    // A pass cut short is not a finished one: it runs again with the next refresh rather than
    // waiting out the whole interval.
    if completed {
        stages.done("track");
    }
}

/// Re-read what the dashboard shows, publishing each stage as it lands rather than once at the
/// end. Returns false when cut short because the user is waiting on something (fields not
/// reached keep their previous values).
///
/// `full` scans Qi from scratch instead of refreshing it; `force` runs every stage regardless of
/// how recently it last ran.
#[allow(clippy::too_many_arguments)]
async fn refresh(
    session: &mut Session,
    dash: &mut Dashboard,
    full: bool,
    force: bool,
    inbox: &mut Inbox,
    stages: &mut Stages,
    deferred: &mut Vec<Ev>,
    send: &dyn Fn(Ev),
    lane: &mut QiLane,
) -> bool {
    let publish = |d: &Dashboard| send(Ev::Dashboard(Box::new(d.clone())));
    let force = force || full;
    let t = std::time::Instant::now();
    read_local(session, dash);
    wallet_core::diag::timing("refresh.local", t);
    let t = std::time::Instant::now();
    // The node's gas price, client version and block order are System-screen detail that does not
    // change between blocks; the refresh reads them on their own schedule and carries them meanwhile.
    // With a monitoring node, keep a block the network's RPC has confirmed ready for the next
    // review, off this refresh's path: reviews on the signing lane share it.
    if session.node.witness().is_some() {
        let (node, network) = (session.node.clone(), session.network.clone());
        tokio::spawn(async move { wallet_core::anchor::keep_warm(&node, &network).await });
    }
    let detail = stages.due("node_detail", NODE_DETAIL_EVERY, force);
    let check =
        tokio::time::timeout(std::time::Duration::from_secs(12), network::check_node_detail(&session.network, &session.node, detail));
    let Some(checked) = unless_urgent(inbox, check).await else { return false };
    let checked = checked.unwrap_or_else(|_| Err(wallet_core::CoreError::Network("node did not answer within 12s".into())));
    wallet_core::diag::timing("refresh.check_node", t);
    // The head every later stage uses, so the refresh asks for it once instead of once per stage.
    let head = match checked {
        Ok(mut h) => {
            dash.latency_history.push(h.latency_ms as u64);
            dash.height_history.push(h.height);
            if dash.latency_history.len() > 60 {
                dash.latency_history.remove(0);
                dash.height_history.remove(0);
            }
            if detail {
                stages.done("node_detail");
            } else if let Some(previous) = &dash.health {
                // A light check left these blank: keep what the last full one found, so the
                // System screen does not flicker between a value and nothing.
                h.gas_price = previous.gas_price.clone();
                h.client_version = previous.client_version.clone();
                h.order = previous.order;
            }
            dash.node_error = if h.identity_ok { None } else { Some("node identity mismatch".into()) };
            let height = h.height;
            dash.health = Some(h);
            height
        }
        Err(e) => {
            dash.node_error = Some(e.to_string());
            publish(dash);
            return true;
        }
    };
    publish(dash);
    if inbox.urgent() {
        return false;
    }
    // Balances first, and on their own: this is the number the wallet exists to show, and it
    // used to wait behind the Qi scan for no reason — nothing in it reads a Qi result.
    let t = std::time::Instant::now();
    // A locked balance is three calls and only moves when a conversion settles, so most refreshes
    // carry the last one rather than paying for it again.
    let carried: Vec<(String, wallet_core::sdk::U256)> = if stages.due("locked", LOCKED_EVERY, force) {
        Vec::new()
    } else {
        dash.accounts.iter().map(|a| (a.address.clone(), a.locked)).collect()
    };
    let Some(mut balances) = unless_urgent(inbox, session.quai_balances_with(&carried)).await else { return false };
    if balances.is_err() && session.monitoring() {
        // The monitoring node stopped answering: read from the main RPC until it is re-checked.
        session.drop_monitor();
        let Some(retried) = unless_urgent(inbox, session.quai_balances_with(&carried)).await else { return false };
        balances = retried;
    }
    if let Ok(b) = balances {
        dash.accounts = b;
        if carried.is_empty() {
            stages.done("locked");
        }
    }
    wallet_core::diag::timing("refresh.balances", t);
    read_locks(session, dash, head);
    stages.done("balances");
    wallet_core::diag::mark("startup.dashboard_balances");
    publish(dash);
    if inbox.urgent() {
        return false;
    }
    // Balances are on screen; everything from here can take its time.
    reconcile(session, head, full, force, inbox, stages, deferred, send, lane).await;
    read_local(session, dash);
    if inbox.urgent() {
        return false;
    }
    if stages.due("tokens", TOKENS_EVERY, force) {
        let t = std::time::Instant::now();
        let accounts: Vec<String> = session.meta.quai_accounts.iter().filter(|a| !a.archived).map(|a| a.address.clone()).collect();
        match unless_urgent(inbox, session.dashboard_balances(&accounts)).await {
            Some(Ok(read)) => {
                dash.tokens = read.tokens;
                (dash.wrap, dash.wrap_error) = (read.wrap, read.wrap_error);
                stages.done("tokens");
            }
            // A failed read leaves the previous values on screen rather than blanking them.
            Some(Err(_)) => {}
            None => return false,
        }
        wallet_core::diag::timing("refresh.tokens", t);
        publish(dash);
        if inbox.urgent() {
            return false;
        }
    }
    // Qi runs in its own lane (`QiLane`), beside everything else: this only asks for a pass, and
    // the answer arrives as `Cmd::QiSynced` whenever it lands. Nothing here waits for it.
    if (session.meta.qi_xpub.is_some() || !session.meta.qi_imported.is_empty()) && stages.due("qi", QI_EVERY, force) {
        lane.refresh(session, full);
        stages.done("qi");
    }
    if session.config.fetch_prices && session.network.chain_id == 9 && stages.due("price", PRICE_EVERY, force) {
        let t = std::time::Instant::now();
        let Some(price) = unless_urgent(inbox, extras::cached_price(&session.app, true)).await else { return false };
        dash.price = price;
        stages.done("price");
        wallet_core::diag::timing("refresh.price", t);
    }
    dash.refreshed_at = wallet_core::registry::now();
    remember_dashboard(session, dash);
    wallet_core::diag::mark("startup.dashboard_complete");
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn automatic_conversion_tolerance_refreshes_and_manual_limit_is_not_raised() {
        let mut quote = ConversionQuote {
            direction: "quai_to_qi".into(),
            amount: "100".into(),
            amount_display: "100".into(),
            quoted: Some("100".into()),
            quoted_display: Some("100".into()),
            expected: Some("99".into()),
            expected_display: Some("99".into()),
            implied_slippage_bps: Some(100),
            discount_saturated: false,
            hold: None,
            headline: String::new(),
            flow_amount: None,
            scenarios: vec![],
            suggested_slippage_bps: 150,
            minimum: None,
            notes: vec![],
            explorer_steps: None,
        };
        assert_eq!(selected_conversion_tolerance(&quote, None).unwrap(), 150);
        assert_eq!(selected_conversion_tolerance(&quote, Some(200)).unwrap(), 200);
        quote.suggested_slippage_bps = 300;
        quote.implied_slippage_bps = Some(250);
        assert_eq!(selected_conversion_tolerance(&quote, None).unwrap(), 300);
        assert!(selected_conversion_tolerance(&quote, Some(200)).is_err());
        assert!(selected_conversion_tolerance(&quote, Some(0)).is_err(), "manual zero is not an automatic sentinel");
    }

    /// Transactions never queue behind the worker: preparing, committing and discarding
    /// go to the signing lane; public quotes stay on the worker; locking and switching reach both; everything else the worker.
    #[test]
    fn transactions_go_to_their_own_lane() {
        let (tx, mut worker_rx) = tokio::sync::mpsc::unbounded_channel::<Cmd>();
        let (_ev_tx, rx) = std::sync::mpsc::channel::<Ev>();
        let (sign, sign_rx) = std::sync::mpsc::channel::<SignJob>();
        let (pending, pending_rx) = std::sync::mpsc::channel::<PendingJob>();
        let w = Worker { tx, rx, sign, pending };
        w.send(Cmd::Prepare(Prepare::FillGap { from: None }));
        w.send(Cmd::Commit("op".into()));
        w.send(Cmd::Discard("op".into()));
        w.send(Cmd::Quote { direction: "quai_to_qi".into(), amount: "1".into() });
        assert!(matches!(worker_rx.try_recv(), Ok(Cmd::Quote { .. })), "public quotes must not block signing or locking");
        assert!(worker_rx.try_recv().is_err(), "transaction work bypasses background reads");
        let lane: Vec<&str> = sign_rx
            .try_iter()
            .map(|j| match j {
                SignJob::Prepare(_) => "prepare",
                SignJob::Commit(_) => "commit",
                SignJob::Discard(_) => "discard",
                _ => "other",
            })
            .collect();
        assert_eq!(lane, ["prepare", "commit", "discard"]);
        // A lock reaches both, so neither signs afterwards.
        w.send(Cmd::Lock);
        assert!(matches!(sign_rx.try_recv(), Ok(SignJob::Lock)));
        assert!(matches!(worker_rx.try_recv(), Ok(Cmd::Lock)));
        // A refresh is the worker's alone.
        w.send(Cmd::Refresh { full: false });
        assert!(sign_rx.try_recv().is_err());
        assert!(matches!(worker_rx.try_recv(), Ok(Cmd::Refresh { .. })));
        // The pending lane follows the wallet and network on screen, and hears nothing else.
        assert!(pending_rx.try_recv().is_err(), "no transaction work reaches the pending lane");
        w.send(Cmd::SwitchWallet("w2".into()));
        w.send(Cmd::SwitchNetwork("orchard".into()));
        let followed: Vec<String> = pending_rx
            .try_iter()
            .map(|j| match j {
                PendingJob::Wallet(id) | PendingJob::Network(id) => id,
                PendingJob::Shutdown => "shutdown".into(),
            })
            .collect();
        assert_eq!(followed, ["w2", "orchard"]);
    }

    #[tokio::test]
    async fn urgent_commands_jump_the_queue_and_refreshes_merge() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut inbox = Inbox::new(rx);
        let wait = Duration::from_millis(10);
        assert!(matches!(inbox.next(wait).await, Ok(None)), "nothing queued");
        tx.send(Cmd::Refresh { full: false }).unwrap();
        tx.send(Cmd::Refresh { full: true }).unwrap();
        tx.send(Cmd::ReadConversation { peer: "code".into(), blocks: 1 }).unwrap();
        // A Qi pass finishing is background too: it must never cut a refresh short, or every
        // 30 s the lane would restart the worker's sync.
        let key = QiKey { wallet: "w".into(), network: "mainnet".into() };
        tx.send(Cmd::QiSynced(QiDone { key, announce: false, result: Ok(1) })).unwrap();
        assert!(!inbox.urgent(), "refreshing, polling a conversation and a finished Qi pass are background work");
        tx.send(Cmd::Commit("op".into())).unwrap();
        tx.send(Cmd::RemoveContact("ann".into())).unwrap();
        assert!(inbox.urgent(), "a refresh checks this between steps");
        // Every command the user sent runs before the refresh, in the order it was sent — a local
        // edit such as a contact is not left behind a sync.
        assert!(matches!(inbox.next(wait).await, Ok(Some(Cmd::Commit(id))) if id == "op"));
        assert!(matches!(inbox.next(wait).await, Ok(Some(Cmd::RemoveContact(name))) if name == "ann"));
        assert!(matches!(inbox.next(wait).await, Ok(Some(Cmd::Refresh { full: true }))), "queued refreshes run once");
        assert!(matches!(inbox.next(wait).await, Ok(Some(Cmd::ReadConversation { .. }))));
        assert!(matches!(inbox.next(wait).await, Ok(Some(Cmd::QiSynced(_)))));
        assert!(matches!(inbox.next(wait).await, Ok(None)));
        drop(tx);
        assert!(inbox.next(wait).await.is_err(), "the UI is gone");
    }

    /// A step in flight is dropped as soon as the user sends something, rather than finishing
    /// first; one that finishes before anything arrives keeps its value.
    #[tokio::test]
    async fn a_running_step_gives_way_to_the_user() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut inbox = Inbox::new(rx);
        assert_eq!(unless_urgent(&mut inbox, async { 7 }).await, Some(7));
        // A background refresh arriving does not interrupt.
        tx.send(Cmd::Refresh { full: false }).unwrap();
        let quick = unless_urgent(&mut inbox, tokio::time::sleep(Duration::from_millis(20))).await;
        assert_eq!(quick, Some(()));
        let slow = tokio::time::sleep(Duration::from_secs(30));
        let sender = tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            sender.send(Cmd::SaveContact { original: None, name: "ann".into(), address: None, code: None, note: String::new() }).unwrap();
        });
        let started = std::time::Instant::now();
        assert_eq!(unless_urgent(&mut inbox, slow).await, None, "dropped for the contact");
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
        // The command that interrupted is still queued, ahead of the refresh.
        assert!(matches!(inbox.next(Duration::from_millis(10)).await, Ok(Some(Cmd::SaveContact { .. }))));
        assert!(matches!(inbox.next(Duration::from_millis(10)).await, Ok(Some(Cmd::Refresh { .. }))));
    }
}
