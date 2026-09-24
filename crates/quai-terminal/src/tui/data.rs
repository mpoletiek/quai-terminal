//! Data worker: third-party and read-only chain data on its own thread (explorer, prices,
//! images, swap quotes, NFTs, listings), so slow lookups never delay signing, locking or the
//! wallet worker. It never holds keys.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};
use wallet_core::appdb::AppDb;
use wallet_core::config::{DataPolicy, Feature};
use wallet_core::data::DataCtx;
use wallet_core::explorer::{Collection, NftItem, TokenInfo, TokenMarket};
use wallet_core::market::{AskCheck, Listing, OwnedNft};
use wallet_core::media::Rendition;
use wallet_core::network::NetworkProfile;
use wallet_core::portfolio::{Known, Portfolio};
use wallet_core::swap::{SwapAsset, SwapQuote};

/// What to do with the alerts.
#[derive(Clone, Debug)]
pub enum AlertOp {
    Load,
    /// Check them; pair alerts only with trading on (`pairs`), gas alerts always. With
    /// `unless_daemon`, skipped while the background daemon reports it checked them lately.
    Check {
        pairs: bool,
        unless_daemon: bool,
    },
    Add(Box<wallet_core::alerts::Alert>),
    Remove(u64),
    ToggleWatch {
        pool: String,
        name: String,
    },
}

/// Requests to the data worker.
pub enum DataCmd {
    ProtocolQuote {
        key: u64,
        direction: String,
        amount: String,
        card: bool,
    },
    /// Rebind to a network and policy (network switch, settings change).
    Configure {
        network: NetworkProfile,
        policy: DataPolicy,
        /// The wallet's cache database, when switching wallets (each has its own).
        app_db: Option<std::path::PathBuf>,
    },
    Portfolio(Known),
    /// DEX pools and overview (Trade › Markets).
    MarketPools,
    /// The zone's current gas price, so MAX can hold back what a transaction will cost.
    GasPrice,
    /// Ready-bucketed candles from Quainance's indexer, so a chart draws before the logs land.
    PairCandles {
        pool: String,
        bucket: u64,
        count: usize,
    },
    /// LP positions with the gauge folded in (Trade › Pools).
    LpPositions {
        owners: Vec<String>,
        pools: Vec<wallet_core::markets::Pool>,
    },
    /// Swap/Sync events of one pool since a time (unix seconds).
    PoolEvents {
        pool: Box<wallet_core::markets::Pool>,
        since: u64,
        /// Read the node's tail up to this block: the head the screens were told about.
        at: Option<u64>,
    },
    /// Channels with messages on the board, followed or not.
    BoardChannels {
        blocks: u64,
    },
    /// One channel of the message board.
    Board {
        channel: String,
        blocks: u64,
    },
    /// Live reserves for the pools on screen, read from the node in one multicall. Price and TVL
    /// come off these, and the explorer publishes its own copy only every 30 s.
    PoolReserves {
        /// Read at this block when given: the head the screens were told about.
        at: Option<u64>,
        pools: Vec<wallet_core::markets::Pool>,
    },
    /// Swaps across every pool over the last `blocks` blocks (Trade › Markets, the flow column).
    DexFlow {
        pools: Vec<wallet_core::markets::Pool>,
        blocks: u64,
        /// End the tape at this block: the head the screens were told about.
        at: Option<u64>,
    },
    Images(Vec<(String, u32)>),
    /// Both QUAI ⇄ Qi markets for an amount (Trade › Convert).
    QiRoutes {
        key: u64,
        direction: wallet_core::qi_market::Direction,
        amount: String,
        owner: Option<String>,
        slippage: u16,
    },
    Markets,
    /// A deposit priced against a pool's reserves, as the add-liquidity card is typed into.
    LiquidityQuote {
        key: u64,
        pair: String,
        amount: String,
        /// The token `amount` is in; the pool derives the other side.
        token: String,
        slippage: u16,
        /// The account depositing, so the quote knows which sides are already approved.
        owner: Option<String>,
    },
    SwapQuote {
        key: u64,
        from: SwapAsset,
        to: SwapAsset,
        amount: String,
        slippage: u16,
        owner: Option<String>,
        /// The traded token's bonding curve, when it has one: quoted beside the exchanges.
        curve: Option<(wallet_core::markets::PoolToken, String)>,
    },
    Nfts {
        owners: Vec<String>,
        /// Skip cached indexer results (user reload, or an NFT operation just confirmed).
        refresh: bool,
    },
    Collections {
        query: Option<String>,
    },
    /// One page of a collection's items, from `offset`.
    CollectionItems {
        contract: String,
        offset: usize,
    },
    /// Floors, volume, trades and listing counts for every collection (the marketplace indexer).
    CollectionStats,
    /// Filled sales across the marketplace, newest first.
    NftTrades,
    Listings {
        collection: Option<String>,
    },
    /// Active listings by this wallet's accounts.
    MyListings {
        sellers: Vec<String>,
    },
    Nft {
        contract: String,
        token_id: String,
        /// This item came out of the public marketplace feed — the listings grid, which every
        /// wallet loads and which says nothing about who is looking. Its metadata is then cached
        /// once for the whole data directory instead of once per wallet. An item opened from
        /// Collected is not public: what a wallet holds is the wallet's own.
        public: bool,
    },
    CheckAsk {
        contract: String,
        token_id: String,
        buyer: Option<String>,
    },
    TokenInfo(String),
    /// What a transaction this wallet only observed carried and cost, from the node.
    TxCost(String),
    /// Quainance's launch zone.
    Launches,
    /// Hashrate, transactions and gas over time, for System › Network.
    ChainStats,
    /// Live QUAI for every wallet on this computer: (wallet id, its public Quai addresses).
    WalletQuai(Vec<(String, Vec<String>)>),
    /// Read or change this wallet's alerts and watchlist, or check the alerts.
    Alerts(AlertOp),
    /// One token's bonding curve, with what these owners hold and are owed on it.
    CurveMarket {
        token: String,
        curve: String,
        owners: Vec<String>,
    },
    Lockups(Vec<String>),
    Test,
    Shutdown,
    /// The screen now in front, as the job names it needs (`portfolio`, `market_pools`, …). Those
    /// jobs jump the queue: a background preload never holds up what the user is looking at.
    Focus(&'static [&'static str]),
}

/// Results from the data worker.
pub enum DataEv {
    ProtocolQuote {
        key: u64,
        card: bool,
        result: Result<Box<wallet_core::ops::ConversionQuote>, String>,
    },
    Portfolio(Result<Box<Portfolio>, String>),
    Image {
        url: String,
        edge: u32,
        rendition: Option<Arc<Rendition>>,
        /// The failure was passing (busy budget, rate limit, timeout): retry soon.
        transient: bool,
    },
    Markets(Vec<TokenMarket>),
    QiRoutes {
        key: u64,
        result: Result<Box<wallet_core::qi_market::Comparison>, String>,
    },
    SwapQuote {
        key: u64,
        result: Result<Box<SwapQuote>, String>,
        /// The same trade on the token's curve, when it has one.
        curve: Option<Result<Box<wallet_core::curve::CurveOffer>, String>>,
    },
    LiquidityQuote {
        key: u64,
        result: Result<Box<wallet_core::liquidity::AddLiquidityQuote>, String>,
    },
    Nfts(Result<Vec<OwnedNft>, String>),
    Collections {
        result: Result<Vec<Collection>, String>,
    },
    CollectionItems {
        contract: String,
        offset: usize,
        result: Result<wallet_core::explorer::CollectionPage, String>,
    },
    CollectionStats {
        result: Result<Vec<wallet_core::market::CollectionStats>, String>,
    },
    NftTrades {
        result: Result<Vec<wallet_core::market::Trade>, String>,
    },
    Listings {
        collection: Option<String>,
        result: Result<Vec<Listing>, String>,
    },
    MyListings(Result<Vec<Listing>, String>),
    Nft {
        contract: String,
        token_id: String,
        result: Result<Box<NftItem>, String>,
    },
    Ask {
        contract: String,
        token_id: String,
        result: Result<Box<AskCheck>, String>,
    },
    TokenInfo {
        address: String,
        result: Result<(TokenInfo, Option<bool>), String>,
    },
    TxCost {
        hash: String,
        result: Result<wallet_core::track::TxCost, String>,
    },
    Launches(Result<Vec<wallet_core::launches::Launch>, String>),
    /// Logo URLs for launch tokens, by token address. Sent after the list it belongs to.
    LaunchLogos(std::collections::HashMap<String, String>),
    ChainStats(Result<Box<wallet_core::chainstats::ChainStats>, String>),
    WalletQuai(Vec<(String, wallet_core::sdk::U256)>),
    /// The alerts and watchlist as stored after the operation; what fired; a line to show.
    Alerts {
        alerts: Vec<wallet_core::alerts::Alert>,
        watchlist: Vec<String>,
        fired: Vec<(String, String)>,
        note: Option<String>,
    },
    CurveMarket {
        token: String,
        result: Result<Box<wallet_core::curve::CurveMarket>, String>,
    },
    Lockups(Result<u64, String>),
    Test(Vec<(String, Result<String, String>, u128)>),
    MarketPools(Result<(Vec<wallet_core::markets::Pool>, wallet_core::markets::DexOverview), String>),
    /// Current gas price in wei, as a decimal string.
    GasPrice(Result<String, String>),
    PairCandles {
        pool: String,
        bucket: u64,
        candles: Vec<wallet_core::markets::Candle>,
    },
    /// LP positions and the gauge they were read against.
    LpPositions {
        result: Result<Vec<wallet_core::liquidity::LpPosition>, String>,
        gauge: Option<Box<wallet_core::gauge::GaugeView>>,
        /// Launch-zone campaigns, when the network has any gauge carrying them.
        zone: Option<Box<wallet_core::zone::ZoneView>>,
    },
    PoolEvents {
        pool: String,
        coverage: Option<wallet_core::markets::HistoryCoverage>,
        result: Result<Vec<wallet_core::markets::PoolEvent>, String>,
    },
    DexFlow(Result<Vec<wallet_core::markets::DexSwap>, String>),
    /// Fresh reserves: (pool address, reserve0, reserve1).
    PoolReserves(Result<Vec<(String, f64, f64)>, String>),
    Board {
        channel: String,
        result: Result<Vec<wallet_core::messages::Post>, String>,
    },
    BoardChannels(Result<Vec<wallet_core::messages::ChannelSummary>, String>),
    /// Something the user should know about the data sources (e.g. an unusable monitor endpoint).
    Notice(String),
}

pub struct DataWorker {
    pub tx: tokio::sync::mpsc::UnboundedSender<DataCmd>,
    pub rx: Receiver<DataEv>,
}

impl DataWorker {
    pub fn spawn(
        app_db: std::path::PathBuf,
        shared_db: std::path::PathBuf,
        network: NetworkProfile,
        policy: DataPolicy,
    ) -> std::io::Result<DataWorker> {
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<DataCmd>();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel::<DataEv>();
        std::thread::Builder::new().name("wallet-data".into()).spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
            runtime.block_on(run(Stores { wallet: app_db, shared: shared_db }, network, policy, cmd_rx, ev_tx));
        })?;
        Ok(DataWorker { tx: cmd_tx, rx: ev_rx })
    }
}

/// The two files a data context reads: this wallet's own, and the data directory's shared
/// display cache. Each context opens its own connections to both.
#[derive(Clone)]
struct Stores {
    wallet: std::path::PathBuf,
    shared: std::path::PathBuf,
}

fn open(paths: &Stores, network: NetworkProfile, policy: DataPolicy) -> Option<DataCtx> {
    let app = AppDb::open(&paths.wallet).ok()?;
    let shared = AppDb::open_shared(&paths.shared).ok();
    DataCtx::with_stores(app, shared, network, policy).ok()
}

/// A second context on its own connection that answers only from the cache.
fn open_cache(paths: &Stores, network: NetworkProfile, policy: DataPolicy) -> Option<DataCtx> {
    open(paths, network, policy).map(|c| DataCtx { cache_only: true, ..c })
}

/// Point the cache-pass context at the node the main one reads from: the few reads it makes
/// that bypass the cache (a gas price) must not go to the RPC endpoint while a monitoring node
/// is in use.
fn follow(cache: &mut DataCtx, ctx: &DataCtx) {
    cache.node = ctx.node.clone();
    cache.monitored = ctx.monitored;
}

/// View loads answer twice: first from the cache, whatever its age, so a screen shows the last
/// known data at once; then from the sources. Returns the request for the cache pass.
fn cache_pass(cmd: &DataCmd) -> Option<DataCmd> {
    Some(match cmd {
        DataCmd::Portfolio(k) => DataCmd::Portfolio(k.clone()),
        DataCmd::Markets => DataCmd::Markets,
        DataCmd::MarketPools => DataCmd::MarketPools,
        DataCmd::Launches => DataCmd::Launches,
        DataCmd::ChainStats => DataCmd::ChainStats,
        DataCmd::PairCandles { pool, bucket, count } => DataCmd::PairCandles { pool: pool.clone(), bucket: *bucket, count: *count },
        DataCmd::DexFlow { pools, blocks, at } => DataCmd::DexFlow { pools: pools.clone(), blocks: *blocks, at: *at },
        DataCmd::Board { channel, blocks } => DataCmd::Board { channel: channel.clone(), blocks: *blocks },
        DataCmd::BoardChannels { blocks } => DataCmd::BoardChannels { blocks: *blocks },
        DataCmd::Nfts { owners, refresh: false } => DataCmd::Nfts { owners: owners.clone(), refresh: false },
        DataCmd::Collections { query } => DataCmd::Collections { query: query.clone() },
        DataCmd::CollectionItems { contract, offset } => DataCmd::CollectionItems { contract: contract.clone(), offset: *offset },
        DataCmd::CollectionStats => DataCmd::CollectionStats,
        DataCmd::NftTrades => DataCmd::NftTrades,
        DataCmd::Listings { collection } => DataCmd::Listings { collection: collection.clone() },
        DataCmd::MyListings { sellers } => DataCmd::MyListings { sellers: sellers.clone() },
        DataCmd::Nft { contract, token_id, public } => {
            DataCmd::Nft { contract: contract.clone(), token_id: token_id.clone(), public: *public }
        }
        DataCmd::TokenInfo(a) => DataCmd::TokenInfo(a.clone()),
        _ => return None,
    })
}

/// A cache pass result worth showing: failures (usually "not cached yet") and a portfolio
/// without prices wait for the real answer.
fn worth_showing(ev: &DataEv) -> bool {
    !matches!(
        ev,
        DataEv::Portfolio(Err(_))
            | DataEv::Nfts(Err(_))
            | DataEv::Collections { result: Err(_) }
            | DataEv::CollectionItems { result: Err(_), .. }
            | DataEv::Listings { result: Err(_), .. }
            | DataEv::MyListings(Err(_))
            | DataEv::Nft { result: Err(_), .. }
            | DataEv::TokenInfo { result: Err(_), .. }
            | DataEv::MarketPools(Err(_))
            | DataEv::Launches(Err(_))
            | DataEv::ChainStats(Err(_))
            | DataEv::DexFlow(Err(_))
            | DataEv::PoolReserves(Err(_))
            | DataEv::Board { result: Err(_), .. }
            | DataEv::BoardChannels(Err(_))
    ) && !matches!(ev, DataEv::Portfolio(Ok(p)) if p.prices.is_none())
}

/// Work runs in three lanes so a slow source never holds up the rest: quick lookups the user is
/// waiting on (quotes, asks, metadata), view loads (portfolio, NFTs, listings, markets) and images.
/// Each lane runs a few jobs at once; the per-host limiter in `wallet_core::http` paces requests.
const LANES: [usize; 3] = [3, 3, 4];
const QUICK: usize = 0;
const VIEWS: usize = 1;
const IMAGES: usize = 2;

fn lane(cmd: &DataCmd) -> usize {
    match cmd {
        DataCmd::ProtocolQuote { .. }
        | DataCmd::SwapQuote { .. }
        | DataCmd::LiquidityQuote { .. }
        | DataCmd::CheckAsk { .. }
        | DataCmd::Nft { .. }
        | DataCmd::TokenInfo(_)
        | DataCmd::TxCost(_)
        | DataCmd::CurveMarket { .. }
        | DataCmd::QiRoutes { .. }
        | DataCmd::Test => QUICK,
        _ => VIEWS,
    }
}

/// Identity for single-flight: a job is not started while an identical one runs, and queued
/// duplicates collapse into the newest. None: every request runs (quotes are matched by key).
fn flight_key(cmd: &DataCmd) -> Option<String> {
    Some(match cmd {
        DataCmd::Portfolio(_) => "portfolio".into(),
        DataCmd::MarketPools => "pools".into(),
        DataCmd::GasPrice => "gas_price".into(),
        DataCmd::PairCandles { pool, bucket, .. } => format!("candles:{pool}:{bucket}"),
        DataCmd::LpPositions { .. } => "lp_positions".into(),
        DataCmd::PoolEvents { pool, .. } => format!("events:{}", pool.address),
        DataCmd::DexFlow { .. } => "dex_flow".into(),
        DataCmd::PoolReserves { .. } => "pool_reserves".into(),
        DataCmd::Board { channel, .. } => format!("board:{channel}"),
        DataCmd::BoardChannels { .. } => "board_channels".into(),
        DataCmd::Markets => "markets".into(),
        DataCmd::Nfts { .. } => "nfts".into(),
        DataCmd::Collections { query } => format!("collections:{}", query.clone().unwrap_or_default()),
        DataCmd::CollectionItems { contract, offset } => format!("items:{contract}:{offset}"),
        DataCmd::CollectionStats => "collection_stats".into(),
        DataCmd::NftTrades => "nft_trades".into(),
        DataCmd::Listings { collection } => format!("listings:{}", collection.clone().unwrap_or_default()),
        DataCmd::MyListings { .. } => "my_listings".into(),
        DataCmd::Nft { contract, token_id, .. } => format!("nft:{contract}:{token_id}"),
        DataCmd::CheckAsk { contract, token_id, .. } => format!("ask:{contract}:{token_id}"),
        DataCmd::TokenInfo(a) => format!("token:{a}"),
        DataCmd::TxCost(h) => format!("tx_cost:{h}"),
        DataCmd::Launches => "launches".into(),
        DataCmd::ChainStats => "chain_stats".into(),
        DataCmd::WalletQuai(_) => "wallet_quai".into(),
        // Every operation counts: two edits are not one.
        DataCmd::Alerts(_) => return None,
        DataCmd::CurveMarket { token, .. } => format!("curve:{token}"),
        DataCmd::Lockups(_) => "lockups".into(),
        DataCmd::Test => "test".into(),
        DataCmd::ProtocolQuote { .. } => "protocol_quote".into(),
        DataCmd::SwapQuote { .. } => "quote".into(),
        DataCmd::LiquidityQuote { .. } => "liquidity_quote".into(),
        DataCmd::QiRoutes { .. } => "qi_routes".into(),
        DataCmd::Images(_) | DataCmd::Configure { .. } | DataCmd::Shutdown | DataCmd::Focus(_) => return None,
    })
}

type Job = std::pin::Pin<Box<dyn std::future::Future<Output = (usize, Option<String>)>>>;
type MonitorJob = std::pin::Pin<Box<dyn std::future::Future<Output = (u64, Option<DataCtx>)>>>;

/// Images from one source loading at once. A slow, strictly paced source (a public IPFS gateway)
/// holds one slot, so it cannot block images from the explorer or the local cache. The user's own
/// node gets two: it is not rate-limited, but it may still be fetching from the IPFS network.
fn image_slots(source: &str) -> usize {
    let gateway = wallet_core::ipfs::gateway(wallet_core::ipfs::Content::Media);
    match source {
        "cache" => LANES[IMAGES],
        s if s == gateway.host() || s.ends_with(&format!(".{}", gateway.host())) => {
            if gateway.is_local() {
                2
            } else {
                1
            }
        }
        s if s.ends_with("ipfs.io") => 1,
        _ => 3,
    }
}

/// Where an image will come from: the wallet's cache, or the host it is fetched from.
fn image_source(app: &AppDb, url: &str) -> String {
    if url.starts_with("data:") || app.media_get(url).ok().flatten().is_some_and(|(hash, _, _)| hash.is_some()) {
        return "cache".into();
    }
    match wallet_core::media::resolve(url) {
        Ok(wallet_core::media::Source::Remote(u)) => wallet_core::http::host_of(&u).unwrap_or_default(),
        Ok(wallet_core::media::Source::Ipfs(l)) => wallet_core::http::host_of(&l.url).unwrap_or_default(),
        _ => "cache".into(),
    }
}

async fn run(
    path: Stores,
    network: NetworkProfile,
    policy: DataPolicy,
    mut cmds: tokio::sync::mpsc::UnboundedReceiver<DataCmd>,
    events: Sender<DataEv>,
) {
    use futures::StreamExt;
    use std::cell::Cell;
    use std::rc::Rc;
    let mut path = path;
    let Some(mut cached) = open_cache(&path, network.clone(), policy) else { return };
    let Some(mut first) = open(&path, network.clone(), policy) else { return };
    // With a monitoring node, adopt it before the first job starts (bounded to 2 s, a couple of
    // milliseconds on a LAN node). Jobs started before it answered read the public RPC, one
    // round trip after another, and the first screen waited a second on them.
    if network.monitor.is_some() {
        let _ = first.use_monitor().await;
    }
    let mut current = (network, policy);
    // A monitoring node is checked every minute: reads fall back to the main RPC while it does
    // not answer and return to it once it does again.
    let mut monitor_tick = tokio::time::interval(std::time::Duration::from_secs(60));
    monitor_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut monitor_job: Option<MonitorJob> = None;
    let mut ticks = 0u64;
    // Old third-party data is cleared once per run, after the first screens have loaded.
    let prune = tokio::time::sleep(std::time::Duration::from_secs(20));
    tokio::pin!(prune);
    let mut pruned = false;
    follow(&mut cached, &first);
    let mut ctx = Rc::new(first);
    let mut cache = Rc::new(cached);
    // Results from before a network or policy change are dropped.
    let generation = Rc::new(Cell::new(0u64));
    let mut queue: Vec<DataCmd> = Vec::new();
    // Newest wants last: what is on screen now loads first.
    let mut images: Vec<(String, u32)> = Vec::new();
    let mut running: futures::stream::FuturesUnordered<Job> = futures::stream::FuturesUnordered::new();
    let mut busy = [0usize; 3];
    let mut flying: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut previews: std::collections::HashMap<String, futures::future::AbortHandle> = std::collections::HashMap::new();
    let mut image_hosts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    // What the visible screen needs; see `DataCmd::Focus`.
    let mut focus: &'static [&'static str] = &[];
    loop {
        while let Ok(c) = cmds.try_recv() {
            match c {
                DataCmd::Focus(names) => focus = names,
                c => queue.push(c),
            }
        }
        // Control commands first.
        if queue.iter().any(|c| matches!(c, DataCmd::Shutdown)) {
            return;
        }
        while let Some(i) = queue.iter().position(|c| matches!(c, DataCmd::Configure { .. })) {
            let DataCmd::Configure { network, policy, app_db } = queue.remove(i) else { continue };
            current = (network.clone(), policy);
            if let Some(db) = app_db {
                path.wallet = db;
            }
            if let (Some(mut c), Some(mut cached)) = (open(&path, network.clone(), policy), open_cache(&path, network, policy)) {
                if current.0.monitor.is_some() {
                    let _ = c.use_monitor().await;
                }
                monitor_job = None;
                ticks = 0;
                monitor_tick.reset_immediately();
                for (_, handle) in previews.drain() {
                    handle.abort();
                }
                follow(&mut cached, &c);
                ctx = Rc::new(c);
                cache = Rc::new(cached);
                generation.set(generation.get() + 1);
            }
        }
        for batch in queue.iter_mut().filter_map(|c| match c {
            DataCmd::Images(w) => Some(std::mem::take(w)),
            _ => None,
        }) {
            for want in batch {
                images.retain(|w| *w != want);
                images.push(want);
            }
        }
        queue.retain(|c| !matches!(c, DataCmd::Images(_)));
        // Queued duplicates collapse into the newest request.
        let mut seen = std::collections::HashSet::new();
        let mut keep = vec![true; queue.len()];
        for (i, c) in queue.iter().enumerate().rev() {
            if let Some(k) = flight_key(c)
                && !seen.insert(k)
            {
                keep[i] = false;
            }
        }
        let mut flags = keep.into_iter();
        queue.retain(|_| flags.next().unwrap_or(true));

        // Public previews are replaceable. Cancelling an obsolete read frees its lane and
        // cannot cancel a prepared or submitted transaction (those live on the signer).
        for cmd in &queue {
            if matches!(
                cmd,
                DataCmd::ProtocolQuote { .. } | DataCmd::SwapQuote { .. } | DataCmd::LiquidityQuote { .. } | DataCmd::QiRoutes { .. }
            ) && let Some(key) = flight_key(cmd)
                && let Some(handle) = previews.remove(&key)
            {
                handle.abort();
            }
        }

        // Start what the lanes have room for: what the user is looking at first, then the rest in
        // the order it was asked for. A job for the visible screen may take one slot beyond the
        // lane's share, so a lane full of preloads cannot keep it waiting.
        queue.sort_by_key(|c| u8::from(!urgent(c, focus)));
        let mut i = 0;
        while i < queue.len() {
            let l = lane(&queue[i]);
            let key = flight_key(&queue[i]);
            let room = LANES[l] + usize::from(urgent(&queue[i], focus));
            if busy[l] >= room || key.as_ref().is_some_and(|k| flying.contains(k)) {
                i += 1;
                continue;
            }
            let cmd = queue.remove(i);
            busy[l] += 1;
            if let Some(k) = &key {
                flying.insert(k.clone());
            }
            let (ctx, cache, events, generation) = (ctx.clone(), cache.clone(), events.clone(), generation.clone());
            let born = generation.get();
            let (abort, registration) = futures::future::AbortHandle::new_pair();
            if matches!(
                &cmd,
                DataCmd::ProtocolQuote { .. } | DataCmd::SwapQuote { .. } | DataCmd::LiquidityQuote { .. } | DataCmd::QiRoutes { .. }
            ) && let Some(key) = &key
            {
                previews.insert(key.clone(), abort);
            }
            let completion = (l, key.clone());
            let work = async move {
                let started = std::time::Instant::now();
                let label = format!("data.{}", cmd_name(&cmd));
                wallet_core::diag::mark(&format!("data.start.{}", cmd_name(&cmd)));
                let send = |ev: DataEv| {
                    if generation.get() == born {
                        let _ = events.send(ev);
                        super::term::wake();
                    }
                };
                if let Some(first) = cache_pass(&cmd) {
                    handle(&cache, first, &|ev| {
                        if worth_showing(&ev) {
                            send(ev)
                        }
                    })
                    .await;
                }
                handle(&ctx, cmd, &send).await;
                wallet_core::diag::timing(&label, started);
                (l, key)
            };
            running.push(Box::pin(async move { futures::future::Abortable::new(work, registration).await.unwrap_or(completion) }));
        }
        // Newest wants first, skipping sources that are already at their share.
        let mut scan = images.len();
        let floor = images.len().saturating_sub(64);
        while busy[IMAGES] < LANES[IMAGES] && scan > floor {
            scan -= 1;
            let source = image_source(&ctx.app, &images[scan].0);
            if image_hosts.get(&source).copied().unwrap_or(0) >= image_slots(&source) {
                continue;
            }
            let (url, edge) = images.remove(scan);
            *image_hosts.entry(source.clone()).or_default() += 1;
            busy[IMAGES] += 1;
            let key = Some(format!("image:{source}"));
            let (ctx, events) = (ctx.clone(), events.clone());
            running.push(Box::pin(async move {
                let allowed =
                    if edge <= wallet_core::media::ICON { ctx.policy.icons } else { ctx.policy.images } || url.starts_with("data:");
                let (rendition, transient) = match allowed {
                    true => match wallet_core::media::load(&ctx.app, &url, edge).await {
                        Ok(r) => (r, false),
                        Err(e) => (None, matches!(e, wallet_core::CoreError::Network(_))),
                    },
                    false => (None, false),
                };
                let _ = events.send(DataEv::Image { url, edge, rendition: rendition.map(Arc::new), transient });
                super::term::wake();
                (IMAGES, key)
            }));
        }

        tokio::select! {
                    c = cmds.recv() => match c {
                        Some(DataCmd::Focus(names)) => focus = names,
                        Some(c) => queue.push(c),
                        None => return,
                    },
                    _ = &mut prune, if !pruned => {
                        pruned = true;
                        let started = std::time::Instant::now();
                        let _ = ctx.app.prune(30 * 86_400);
                        // The shared feeds age out on the same schedule, and a wallet opened before the
                        // shared cache existed drops its own copies of them once.
                        if let Some(shared) = &ctx.shared {
                            let _ = shared.prune(30 * 86_400);
                        }
                        let _ = ctx.app.drop_shared_feeds();
                        wallet_core::diag::timing("data.prune", started);
                    }
                    _ = monitor_tick.tick(), if current.0.monitor.is_some() && monitor_job.is_none() => {
                        ticks += 1;
                        let (observed, paths, profile, policy, born) = (ctx.clone(), path.clone(), current.0.clone(), current.1, generation.get());
                        monitor_job = Some(Box::pin(async move {
                            let switched = if observed.monitoring() && !observed.node_answers(std::time::Duration::from_secs(3)).await {
                                open(&paths, profile, policy)
                            } else if !observed.monitoring() && (ticks == 1 || ticks.is_multiple_of(3)) {
                                if let Some(mut next) = open(&paths, profile, policy) {
                                    if next.use_monitor().await.is_none() { Some(next) } else { None }
                                } else { None }
                            } else { None };
                            (born, switched)
                        }));
                    }
                    (born, switched) = async { monitor_job.as_mut().expect("guarded monitor job").await }, if monitor_job.is_some() => {
                        monitor_job = None;
                        if born == generation.get()
                            && let Some(next) = switched
                            && let Some(mut cached) = open_cache(&path, current.0.clone(), current.1)
                        {
                            if next.monitoring() != ctx.monitoring() {
                                let note = if next.monitoring() { "your node is answering again; reads use it" } else { "your node stopped answering; reads use the network RPC until it is back" };
                                let _ = events.send(DataEv::Notice(note.into()));
        super::term::wake();
                            }
                            follow(&mut cached, &next);
                            ctx = Rc::new(next);
                            cache = Rc::new(cached);
                        }
                    }
                    Some((l, key)) = running.next(), if !running.is_empty() => {
                        busy[l] -= 1;
                        match key {
                            Some(k) if l == IMAGES => {
                                if let Some(n) = image_hosts.get_mut(k.trim_start_matches("image:")) {
                                    *n = n.saturating_sub(1);
                                }
                            }
                            Some(k) => {
                                flying.remove(&k);
                                previews.remove(&k);
                            }
                            None => {}
                        }
                    }
                }
    }
}

async fn handle(ctx: &DataCtx, cmd: DataCmd, send: &dyn Fn(DataEv)) {
    match cmd {
        DataCmd::ProtocolQuote { key, direction, amount, card } => {
            let result =
                wallet_core::ops::quote_conversion(ctx, &direction, &amount, None, None).await.map(Box::new).map_err(|e| e.to_string());
            send(DataEv::ProtocolQuote { key, card, result });
        }
        DataCmd::Shutdown | DataCmd::Configure { .. } | DataCmd::Images(_) | DataCmd::Focus(_) => {}
        DataCmd::Portfolio(known) => {
            let r = wallet_core::portfolio::build(ctx, &known).await.map(Box::new).map_err(|e| e.to_string());
            send(DataEv::Portfolio(r));
        }
        DataCmd::QiRoutes { key, direction, amount, owner, slippage } => {
            let result = async {
                let amount =
                    wallet_core::sdk::U256::from_str_radix(&amount, 10).map_err(|_| wallet_core::CoreError::Invalid("amount".into()))?;
                wallet_core::qi_market::compare(ctx, direction, amount, owner.as_deref(), slippage).await
            }
            .await
            .map(Box::new)
            .map_err(|e| e.to_string());
            send(DataEv::QiRoutes { key, result });
        }
        DataCmd::Markets => {
            if ctx.policy.market {
                let explorer = &ctx.explorer;
                if let Ok(c) = ctx.cached("token_markets", 300, || explorer.token_markets()).await {
                    send(DataEv::Markets(c.value));
                }
            }
        }
        DataCmd::SwapQuote { key, from, to, amount, slippage, owner, curve } => {
            let atoms = wallet_core::sdk::U256::from_str_radix(&amount, 10);
            // The exchanges and the token's curve, at once: a curve can be where the token's
            // liquidity is while a shallow pool is all the router sees.
            let routed = async {
                let amount = atoms.map_err(|_| wallet_core::CoreError::Invalid("amount".into()))?;
                let router = wallet_core::swap::Router::open(&ctx.app, &ctx.node, &ctx.network, ctx.trust).await?;
                let mut quote = router.quote(&from, &to, amount, slippage, owner.as_deref()).await?;
                wallet_core::swap::attach_liquidity(ctx, &mut quote).await;
                Ok::<_, wallet_core::CoreError>(quote)
            };
            let offered = async {
                let (token, address) = curve.as_ref()?;
                let amount = atoms.ok()?;
                match wallet_core::curve::curve_offer(ctx, token, address, &from, &to, amount).await {
                    Ok(Some(offer)) => Some(Ok(Box::new(offer))),
                    Ok(None) => None,
                    Err(e) => Some(Err(e.to_string())),
                }
            };
            let (result, curve) = tokio::join!(routed, offered);
            send(DataEv::SwapQuote { key, result: result.map(Box::new).map_err(|e| e.to_string()), curve });
        }
        DataCmd::LiquidityQuote { key, pair, amount, token, slippage, owner } => {
            let result = wallet_core::liquidity::quote(ctx, &pair, &amount, Some(token.as_str()), slippage, owner.as_deref())
                .await
                .map(Box::new)
                .map_err(|e| e.to_string());
            send(DataEv::LiquidityQuote { key, result });
        }
        DataCmd::Nfts { owners, refresh } => {
            let r = wallet_core::market::holdings(ctx, &owners, 200, refresh).await.map_err(|e| e.to_string());
            send(DataEv::Nfts(r));
        }
        DataCmd::Collections { query } => {
            let explorer = &ctx.explorer;
            let q = query.clone();
            let r = if ctx.policy.market {
                ctx.cached(&format!("collections:{}", q.clone().unwrap_or_default()), 600, || explorer.collections(q.as_deref(), 100))
                    .await
                    .map(|c| c.value)
                    .map_err(|e| e.to_string())
            } else {
                Err("market data is turned off (System › Data sources)".into())
            };
            let _ = query;
            send(DataEv::Collections { result: r });
        }
        DataCmd::CollectionItems { contract, offset } => {
            let explorer = &ctx.explorer;
            let c = contract.clone();
            let r = ctx
                .cached(&format!("collection_items:{c}:{offset}"), 600, || explorer.collection_items(&c, offset))
                .await
                .map(|c| c.value)
                .map_err(|e| e.to_string());
            send(DataEv::CollectionItems { contract, offset, result: r });
        }
        DataCmd::CollectionStats => {
            let r = wallet_core::market::collection_stats(ctx).await.map_err(|e| e.to_string());
            send(DataEv::CollectionStats { result: r });
        }
        DataCmd::NftTrades => {
            // The whole marketplace history is a few hundred sales, so one read covers every
            // collection and every window is counted from it.
            let r = wallet_core::market::trades(ctx, None).await.map_err(|e| e.to_string());
            send(DataEv::NftTrades { result: r });
        }
        DataCmd::Listings { collection } => {
            let r = wallet_core::market::listings(ctx, collection.as_deref()).await.map_err(|e| e.to_string());
            send(DataEv::Listings { collection, result: r });
        }
        DataCmd::MyListings { sellers } => {
            let r = wallet_core::market::listings_by(ctx, &sellers).await.map_err(|e| e.to_string());
            send(DataEv::MyListings(r));
        }
        DataCmd::Nft { contract, token_id, public } => {
            let explorer = &ctx.explorer;
            let (c, id) = (contract.clone(), token_id.clone());
            let key = if public { format!("listing_nft:{c}:{id}") } else { format!("nft:{c}:{id}") };
            let r = match ctx.cached(&key, 86_400, || explorer.nft(&c, &id)).await {
                Ok(c) => {
                    let kind = c.value.kind.unwrap_or(wallet_core::explorer::TokenKind::Erc721);
                    Ok(Box::new(ctx.with_own_metadata(c.value, kind).await))
                }
                Err(e) => Err(e.to_string()),
            };
            send(DataEv::Nft { contract, token_id, result: r });
        }
        DataCmd::CheckAsk { contract, token_id, buyer } => {
            let result = async {
                let zora = wallet_core::market::Zora::open(&ctx.app, &ctx.node, &ctx.network).await?;
                zora.check(&ctx.node, &contract, &token_id, buyer.as_deref()).await
            }
            .await
            .map(Box::new)
            .map_err(|e| e.to_string());
            send(DataEv::Ask { contract, token_id, result });
        }
        DataCmd::TokenInfo(address) => {
            let explorer = &ctx.explorer;
            let a = address.clone();
            let result = if ctx.policy.explorer {
                match ctx.cached(&format!("token_info:{a}"), 3600, || explorer.token_info(&a)).await {
                    Ok(info) => {
                        let verified =
                            ctx.cached(&format!("verified:{a}"), 86_400, || explorer.contract_verified(&a)).await.ok().map(|c| c.value);
                        Ok((info.value, verified))
                    }
                    Err(e) => Err(e.to_string()),
                }
            } else {
                Err("explorer lookups are off".into())
            };
            send(DataEv::TokenInfo { address, result });
        }
        DataCmd::MarketPools => {
            let r = wallet_core::markets::all_markets(ctx).await.map_err(|e| e.to_string());
            send(DataEv::MarketPools(r));
        }
        DataCmd::LpPositions { owners, pools } => {
            // The gauge is optional: without it positions still read, they just cannot be staked.
            let gauge = wallet_core::gauge::open(ctx, &owners).await.ok();
            // Launch-zone campaigns are a second, independent set of gauges: absent or unreadable
            // is normal (most networks have none), and never costs the positions.
            let zone = wallet_core::zone::open(ctx, &owners).await.ok();
            let positions = wallet_core::liquidity::positions(ctx, &owners, &pools, gauge.as_ref(), zone.as_ref()).await;
            send(DataEv::LpPositions { result: Ok(positions), gauge: gauge.map(Box::new), zone: zone.map(Box::new) });
        }
        DataCmd::PairCandles { pool, bucket, count } => {
            // Absent or unindexed is normal, not an error: the chart falls back to the logs.
            if let Ok(candles) = wallet_core::subgraph::candles(ctx, &pool, bucket, count).await {
                send(DataEv::PairCandles { pool, bucket, candles });
            }
        }
        DataCmd::CurveMarket { token, curve, owners } => {
            let result = wallet_core::curve::market(ctx, &token, &curve, &owners).await.map(Box::new).map_err(|e| e.to_string());
            send(DataEv::CurveMarket { token, result });
        }
        DataCmd::Launches => {
            let listed = wallet_core::launches::launches(ctx, 200).await;
            // The list goes on screen first; logos follow once resolved (each is cached for a
            // month, so after the first visit this answers from the shared cache at once).
            let rows = listed.as_ref().map(|l| l.clone()).unwrap_or_default();
            send(DataEv::Launches(listed.map_err(|e| e.to_string())));
            // In batches, top of the list first, so the rows on screen fill in while the rest resolve.
            for batch in rows.chunks(8) {
                let logos = wallet_core::launches::logos(ctx, batch).await;
                if !logos.is_empty() {
                    send(DataEv::LaunchLogos(logos));
                }
            }
        }
        DataCmd::WalletQuai(wallets) => send(DataEv::WalletQuai(wallet_core::cockpit::quai_totals(ctx, &wallets).await)),
        DataCmd::Alerts(op) => {
            use wallet_core::alerts;
            let network = ctx.network.id.clone();
            let mut fired = Vec::new();
            let note = match op {
                AlertOp::Load => None,
                AlertOp::Check { pairs, unless_daemon } => {
                    // The daemon checks alerts for every wallet; when its heartbeat is fresh and
                    // clean, checking here too would only fire them twice.
                    let daemon_checked = unless_daemon
                        && ctx
                            .app
                            .kv(&format!("alerts_heartbeat:{network}"))
                            .ok()
                            .flatten()
                            .and_then(|value| serde_json::from_str::<serde_json::Value>(&value).ok())
                            .is_some_and(|value| {
                                value["error"].is_null()
                                    && value["completed_at"]
                                        .as_u64()
                                        .is_some_and(|at| wallet_core::registry::now().saturating_sub(at) < 180)
                            });
                    if !daemon_checked {
                        fired = alerts::run(ctx, pairs).await.unwrap_or_default();
                    }
                    None
                }
                AlertOp::Add(a) => {
                    let what = a.describe();
                    Some(alerts::add(&ctx.app, &network, *a).map_or_else(|e| e.to_string(), |_| format!("alert set: {what}")))
                }
                AlertOp::Remove(id) => Some(match alerts::remove(&ctx.app, &network, id) {
                    Ok(true) => "alert removed".into(),
                    Ok(false) => "no such alert".into(),
                    Err(e) => e.to_string(),
                }),
                AlertOp::ToggleWatch { pool, name } => Some(match alerts::toggle_watch(&ctx.app, &network, &pool) {
                    Ok(true) => format!("watching {name}"),
                    Ok(false) => format!("stopped watching {name}"),
                    Err(e) => e.to_string(),
                }),
            };
            send(DataEv::Alerts {
                alerts: alerts::load(&ctx.app, &network),
                watchlist: alerts::watchlist(&ctx.app, &network),
                fired,
                note,
            });
        }
        DataCmd::ChainStats => {
            send(DataEv::ChainStats(wallet_core::chainstats::chain_stats(ctx).await.map(|c| Box::new(c.value)).map_err(|e| e.to_string())));
        }
        DataCmd::TxCost(hash) => {
            let result = wallet_core::track::chain_cost(&ctx.node.provider, &hash).await.map_err(|e| e.to_string());
            send(DataEv::TxCost { hash, result });
        }
        DataCmd::GasPrice => {
            let r = ctx.node.provider.gas_price(wallet_core::network::ZONE).await.map(|p| p.to_string()).map_err(|e| e.to_string());
            send(DataEv::GasPrice(r));
        }
        DataCmd::PoolEvents { pool, since, at } => {
            let r = wallet_core::markets::pool_events_at(ctx, &pool, since, 10, at).await.map_err(|e| e.to_string());
            let coverage = if r.is_ok() { wallet_core::markets::pool_history_coverage(ctx, &pool).ok().flatten() } else { None };
            send(DataEv::PoolEvents { pool: pool.address.clone(), coverage, result: r });
        }
        DataCmd::DexFlow { pools, blocks, at } => {
            send(DataEv::DexFlow(wallet_core::markets::dex_flow_at(ctx, &pools, blocks, at).await.map_err(|e| e.to_string())));
        }
        DataCmd::PoolReserves { pools, at } => {
            send(DataEv::PoolReserves(wallet_core::markets::refresh_reserves_at(ctx, &pools, at).await.map_err(|e| e.to_string())));
        }
        DataCmd::Board { channel, blocks } => {
            let result = match wallet_core::messages::channel_tag(&channel) {
                Ok(tag) => wallet_core::messages::channel(ctx, &tag, blocks).await.map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            };
            send(DataEv::Board { channel, result });
        }
        DataCmd::BoardChannels { blocks } => {
            send(DataEv::BoardChannels(wallet_core::messages::channels(ctx, blocks).await.map_err(|e| e.to_string())));
        }
        DataCmd::Lockups(owners) => {
            if ctx.policy.explorer {
                let mut total = 0u64;
                let mut err = None;
                for o in owners {
                    match ctx.explorer.lockups(&o).await {
                        Ok(s) => total += s.total,
                        Err(e) => err = Some(e.to_string()),
                    }
                }
                send(DataEv::Lockups(match err {
                    Some(e) => Err(e),
                    None => Ok(total),
                }));
            }
        }
        DataCmd::Test => {
            let mut results = Vec::new();
            let t = std::time::Instant::now();
            match ctx.explorer.backend {
                wallet_core::explorer::Backend::Quai => {
                    let r = ctx
                        .explorer
                        .prices()
                        .await
                        .map(|p| format!("QUAI {}", p.quai_usd.map(wallet_core::amount::usd_price).unwrap_or_default()));
                    results.push((ctx.explorer.source(), r.map_err(|e| e.to_string()), t.elapsed().as_millis()));
                }
                wallet_core::explorer::Backend::Blockscout => {
                    let r = ctx
                        .explorer
                        .token_info(ctx.network.wquai.as_deref().unwrap_or_default())
                        .await
                        .map(|i| format!("token {}", i.symbol));
                    results.push((ctx.explorer.source(), r.map_err(|e| e.to_string()), t.elapsed().as_millis()));
                }
                wallet_core::explorer::Backend::ChainOnly => {}
            }
            if let Some(base) = ctx.network.ecosystem.bazarr_indexer.clone() {
                let t = std::time::Instant::now();
                let r = wallet_core::http::get_json(&format!("{}/listings", base.trim_end_matches('/')))
                    .await
                    .map(|v| wallet_core::amount::count(wallet_core::market::parse_listings(&v).len(), "listing"));
                results.push(("marketplace indexer".into(), r.map_err(|e| e.to_string()), t.elapsed().as_millis()));
            }
            let t = std::time::Instant::now();
            let r = wallet_core::network::check_node(&ctx.network, &ctx.node).await.map(|h| format!("block {}", h.height));
            results.push(("node".into(), r.map_err(|e| e.to_string()), t.elapsed().as_millis()));
            if ctx.policy.images {
                let t = std::time::Instant::now();
                for (label, r) in wallet_core::ipfs::test_lines().await {
                    results.push((label, r.map_err(|e| e.to_string()), t.elapsed().as_millis()));
                }
            }
            send(DataEv::Test(results));
        }
    }
}

/// Whether a job is for what the user is looking at. The portfolio always is: it is the number the
/// wallet opens on, and every other screen's header leans on its prices.
fn urgent(cmd: &DataCmd, focus: &[&str]) -> bool {
    matches!(cmd, DataCmd::Portfolio(_)) || focus.contains(&cmd_name(cmd))
}

impl DataCmd {
    /// The optional feature this job serves; the app never sends one whose feature is off.
    pub fn feature(&self) -> Option<Feature> {
        match self {
            DataCmd::MarketPools
            | DataCmd::PairCandles { .. }
            | DataCmd::LpPositions { .. }
            | DataCmd::PoolEvents { .. }
            | DataCmd::DexFlow { .. }
            | DataCmd::PoolReserves { .. }
            | DataCmd::Markets
            | DataCmd::SwapQuote { .. }
            | DataCmd::LiquidityQuote { .. }
            | DataCmd::Launches => Some(Feature::Trading),
            DataCmd::Nfts { .. }
            | DataCmd::Collections { .. }
            | DataCmd::CollectionItems { .. }
            | DataCmd::Listings { .. }
            | DataCmd::MyListings { .. }
            | DataCmd::Nft { .. }
            | DataCmd::CollectionStats
            | DataCmd::NftTrades
            | DataCmd::CheckAsk { .. } => Some(Feature::Nfts),
            DataCmd::Board { .. } | DataCmd::BoardChannels { .. } => Some(Feature::Messaging),
            _ => None,
        }
    }
}

fn cmd_name(cmd: &DataCmd) -> &'static str {
    match cmd {
        DataCmd::Configure { .. } => "configure",
        DataCmd::Portfolio(_) => "portfolio",
        DataCmd::MarketPools => "market_pools",
        DataCmd::GasPrice => "gas_price",
        DataCmd::PairCandles { .. } => "pair_candles",
        DataCmd::LpPositions { .. } => "lp_positions",
        DataCmd::PoolEvents { .. } => "pool_events",
        DataCmd::DexFlow { .. } => "dex_flow",
        DataCmd::PoolReserves { .. } => "pool_reserves",
        DataCmd::Board { .. } => "board",
        DataCmd::BoardChannels { .. } => "board_channels",
        DataCmd::Images(_) => "images",
        DataCmd::QiRoutes { .. } => "qi_routes",
        DataCmd::Markets => "markets",
        DataCmd::ProtocolQuote { .. } => "protocol_quote",
        DataCmd::SwapQuote { .. } => "swap_quote",
        DataCmd::LiquidityQuote { .. } => "liquidity_quote",
        DataCmd::Nfts { .. } => "nfts",
        DataCmd::Collections { .. } => "collections",
        DataCmd::CollectionItems { .. } => "collection_items",
        DataCmd::CollectionStats => "collection_stats",
        DataCmd::NftTrades => "nft_trades",
        DataCmd::Listings { .. } => "listings",
        DataCmd::MyListings { .. } => "my_listings",
        DataCmd::Nft { .. } => "nft",
        DataCmd::CheckAsk { .. } => "check_ask",
        DataCmd::TokenInfo(_) => "token_info",
        DataCmd::TxCost(_) => "tx_cost",
        DataCmd::Launches => "launches",
        DataCmd::ChainStats => "chain_stats",
        DataCmd::WalletQuai(_) => "wallet_quai",
        DataCmd::Alerts(_) => "alerts",
        DataCmd::CurveMarket { .. } => "curve_market",
        DataCmd::Lockups(_) => "lockups",
        DataCmd::Test => "test",
        DataCmd::Shutdown => "shutdown",
        DataCmd::Focus(_) => "focus",
    }
}
