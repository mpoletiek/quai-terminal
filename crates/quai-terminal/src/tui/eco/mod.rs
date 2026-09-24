//! Ecosystem state and interaction: portfolio, images, exchange cards (swap, convert, wrap),
//! NFTs, listings and the detail stack. Rendering lives in `views`.

use super::app::{App, Detail, FormKind, Modal, Screen};
use super::data::{DataCmd, DataEv};
use super::worker::{Cmd, Prepare};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use wallet_core::amount;
use wallet_core::explorer::{Collection, NftItem, TokenInfo, TokenMarket};
use wallet_core::market::{AskCheck, Listing, OwnedNft};
use wallet_core::media::Rendition;
use wallet_core::ops::ConversionQuote;
use wallet_core::portfolio::{AssetKey, Known, Portfolio};
use wallet_core::routes::{RouteGraph, RouteInfo};
use wallet_core::sdk::U256;
use wallet_core::swap::{SwapAsset, SwapQuote};

/// An image being fetched or ready.
pub enum ImageSlot {
    Loading,
    Ready(Arc<Rendition>, Instant),
    /// When it may be requested again.
    Failed(Instant),
}

/// How long a failed image waits before it is requested again.
pub const IMAGE_RETRY: Duration = Duration::from_secs(30);
/// The same after a passing failure (busy request budget, rate limit, timeout).
pub const IMAGE_RETRY_SOON: Duration = Duration::from_secs(4);

/// Exchange card for swaps.
pub struct SwapCard {
    pub from: SwapAsset,
    pub to: Option<SwapAsset>,
    pub amount: String,
    pub slippage_bps: u16,
    pub deadline_minutes: u32,
    /// 0 pay token, 1 amount, 2 receive token, 3 slippage, 4 deadline, 5 none (keys stay global).
    pub field: usize,
    pub quote: Option<Result<SwapQuote, String>>,
    pub quote_key: u64,
    pub requested_key: u64,
    pub requested_input: Option<u64>,
    pub request_sequence: u64,
    pub edited: Option<Instant>,
    pub quoted_at: Option<Instant>,
    /// Approval submitted; waiting for it to confirm before step 2.
    pub approving: bool,
    /// The share of the spendable maximum last set with `%` (25, 50, 75, 100), until the amount
    /// is typed over.
    pub preset: Option<u8>,
    /// The same trade on the token's bonding curve, quoted with `quote`.
    pub curve: Option<Result<wallet_core::curve::CurveOffer, String>>,
}

impl Default for SwapCard {
    fn default() -> Self {
        SwapCard {
            from: SwapAsset::Quai,
            to: None,
            amount: String::new(),
            slippage_bps: 50,
            deadline_minutes: 10,
            field: 5,
            quote: None,
            quote_key: 0,
            requested_key: 0,
            requested_input: None,
            request_sequence: 0,
            edited: None,
            quoted_at: None,
            approving: false,
            preset: None,
            curve: None,
        }
    }
}

/// Exchange card for QUAI ↔ Qi: the protocol conversion and the market route side by side.
#[derive(Default)]
pub struct ConvertCard {
    pub protocol_key: std::cell::Cell<u64>,
    pub qi_to_quai: bool,
    pub amount: String,
    pub slippage_bps: u16,
    pub manual_slippage: bool,
    /// 0 direction, 1 amount, 2 slippage, 3 route, 4 none.
    pub field: usize,
    pub quote: Option<ConversionQuote>,
    pub quoted_for: Option<(bool, String)>,
    /// The market route is selected (false: the protocol conversion).
    pub market: bool,
    /// Both markets for the amount on the card.
    pub routes: Option<Result<wallet_core::qi_market::Comparison, String>>,
    /// Inputs the held comparison belongs to, and the one asked for.
    pub routes_key: u64,
    pub requested_key: u64,
    /// Last edit, for debouncing the quote.
    pub edited: Option<Instant>,
    /// When the held comparison was asked for.
    pub quoted_at: Option<Instant>,
}

/// Wrap modes.
pub const WRAP_MODES: [(&str, &str, &str); 5] = [
    ("Qi → WQI", "QI", "wrap Qi into WQI backing (step 1 of 2)"),
    ("Claim WQI", "", "claim WQI once the wrap settles (step 2 of 2)"),
    ("WQI → Qi", "QI", "redeem WQI for Qi (whole Qi; locked briefly)"),
    ("QUAI → WQUAI", "QUAI", "instant deposit"),
    ("WQUAI → QUAI", "WQUAI", "instant withdrawal"),
];

/// Exchange card for wraps.
#[derive(Default)]
pub struct WrapCard {
    pub mode: usize,
    pub amount: String,
    /// 0 mode, 1 amount, 2 none.
    pub field: usize,
}

/// Said when a pool is in neither the Quainance PoolGauge nor any pinned launch-zone gauge.
pub const GAUGE_ABSENT: &str = "this pool is in no gauge — it earns swap fees only";

/// Said when funding is asked of a launch-zone pool: its campaign was funded when the token
/// launched, and the wallet only funds the core gauge's allowlisted streams.
pub const ZONE_NOT_FUNDABLE: &str =
    "launch-zone campaigns are funded when a token launches — only Quainance gauge pools can be funded here";

/// What the Pools screen is acting on.
pub struct PoolFocus {
    pub pair: String,
    /// `WQI/WQUAI`.
    pub name: String,
    /// The pool's two tokens; a deposit is typed in either of them.
    pub tokens: (wallet_core::markets::PoolToken, wallet_core::markets::PoolToken),
    /// The position held in this pool, when there is one.
    pub position: Option<wallet_core::liquidity::LpPosition>,
}

/// Trade › Pools state: what this wallet provides, and what the gauge pays for it.
#[derive(Default)]
pub struct PoolsView {
    /// Positions with the gauge folded in. None until the first read.
    pub positions: Option<Result<Vec<wallet_core::liquidity::LpPosition>, String>>,
    pub loading: bool,
    pub loaded_at: Option<Instant>,
    /// The gauge's pools, for APR and reward figures.
    pub gauge: Option<wallet_core::gauge::GaugeView>,
    /// Launch-zone campaigns, which pay on the same pairs through their own gauges.
    pub zone: Option<wallet_core::zone::ZoneView>,
    /// Focused position row (pane 0).
    pub selected: usize,
    /// Focused row in the pool directory (pane 1), where a new position is opened.
    pub pool_selected: usize,
    /// The open deposit card, while one is being composed.
    pub add: Option<AddCard>,
}

/// Composing a deposit: one side is typed, the pool prices the other, and both are on screen
/// before anything is signed — the same shape as the swap card, because it is the same question.
pub struct AddCard {
    pub pair: String,
    /// `SMOL/WQI`.
    pub name: String,
    pub token0: wallet_core::markets::PoolToken,
    pub token1: wallet_core::markets::PoolToken,
    /// The typed side is token1 (else token0).
    pub side1: bool,
    pub amount: String,
    pub slippage_bps: u16,
    pub account: Option<String>,
    /// 0 account, 1 amount, 2 slippage.
    pub field: usize,
    pub quote: Option<Result<wallet_core::liquidity::AddLiquidityQuote, String>>,
    /// The inputs the held quote belongs to, and the ones asked for.
    pub quote_key: u64,
    pub requested_key: u64,
    /// Last edit, for debouncing the quote.
    pub edited: Option<Instant>,
}

impl AddCard {
    /// The side being typed, and the side the pool derives.
    pub fn typed(&self) -> &wallet_core::markets::PoolToken {
        if self.side1 { &self.token1 } else { &self.token0 }
    }

    pub fn paired(&self) -> &wallet_core::markets::PoolToken {
        if self.side1 { &self.token0 } else { &self.token1 }
    }

    /// The paired amount from the held quote, when it is the answer to what is typed.
    pub fn paired_text(&self) -> Option<String> {
        let Some(Ok(q)) = self.quote.as_ref().filter(|_| self.quote_key == self.requested_key) else { return None };
        let (amount, decimals) = if self.side1 { (q.amount0, q.token0.decimals) } else { (q.amount1, q.token1.decimals) };
        Some(amount::group_thousands(&amount::format_amount_short(amount, decimals, 6)))
    }
}

/// Trade › Markets state.
/// How the pairs list is ordered. `Default` is the directory's own order, watched pairs first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MarketSort {
    #[default]
    Default,
    /// Deepest pools first.
    TvlDesc,
    /// Shallowest first.
    TvlAsc,
    /// Biggest 24-hour gainers first.
    ChangeDesc,
    /// Biggest 24-hour losers first.
    ChangeAsc,
}

impl MarketSort {
    pub fn label(self) -> &'static str {
        match self {
            MarketSort::Default => "watched first",
            MarketSort::TvlDesc => "deepest TVL",
            MarketSort::TvlAsc => "shallowest TVL",
            MarketSort::ChangeDesc => "24h gainers",
            MarketSort::ChangeAsc => "24h losers",
        }
    }

    /// `L`: by TVL, deepest, then shallowest, then back to the default order.
    pub fn next_tvl(self) -> Self {
        match self {
            MarketSort::TvlDesc => MarketSort::TvlAsc,
            MarketSort::TvlAsc => MarketSort::Default,
            _ => MarketSort::TvlDesc,
        }
    }

    /// `M`: by 24-hour move, gainers, then losers, then back to the default order.
    pub fn next_change(self) -> Self {
        match self {
            MarketSort::ChangeDesc => MarketSort::ChangeAsc,
            MarketSort::ChangeAsc => MarketSort::Default,
            _ => MarketSort::ChangeDesc,
        }
    }
}

pub struct MarketsView {
    pub pools: Option<Result<(Vec<wallet_core::markets::Pool>, wallet_core::markets::DexOverview), String>>,
    pub pools_loading: bool,
    pub pools_at: Option<Instant>,
    pub pools_attempted: Option<Instant>,
    /// A live reserve read is in flight, and when the last one landed. Reserves refresh far
    /// faster than the directory that discovered the pools, so they keep their own clock.
    pub reserves_loading: bool,
    pub reserves_at: Option<Instant>,
    pub reserves_attempted: Option<Instant>,
    /// The block the reserve read in flight was asked for, and the block of the reserves shown.
    pub reserves_asked_block: Option<u64>,
    pub reserves_block: Option<u64>,
    pub events: HashMap<String, Result<Vec<wallet_core::markets::PoolEvent>, String>>,
    pub events_loading: Option<String>,
    /// A neighbour of the selected pair whose chart is loading before the cursor reaches it. It
    /// has its own slot so the pair the user is looking at never waits behind it.
    pub events_prefetching: Option<String>,
    pub history_coverage: HashMap<String, wallet_core::markets::HistoryCoverage>,
    /// Ready-bucketed candles from the indexer, keyed by (pool, bucket seconds). The chart uses
    /// these while the pool's logs are still loading, then keeps whichever covers more.
    pub candles: HashMap<(String, u64), Vec<wallet_core::markets::Candle>>,
    pub candles_requested: HashMap<(String, u64), Instant>,
    pub events_at: HashMap<String, (Instant, u64)>,
    /// Index into `markets::TIMEFRAMES` (1h by default).
    pub timeframe: usize,
    /// How many candles the chart is dragged back from now (0: it ends now), and the pair and
    /// timeframe that pan belongs to (another resets it).
    pub pan: (usize, String, usize),
    /// Pools whose base/quote orientation the user flipped.
    pub flipped: std::collections::HashSet<String>,
    /// The pair the chart shows, kept while the flow column has the cursor.
    pub pair_selected: usize,
    /// The row the flow column has under its cursor, kept while the pairs list has it.
    pub flow_selected: usize,
    /// Hide swaps worth less than this in USD (0 shows every one).
    pub flow_min_usd: f64,
    /// How the pairs list is ordered.
    pub sort: MarketSort,
    /// Swaps across every pool, newest first (the flow column).
    pub flow: Vec<wallet_core::markets::DexSwap>,
    pub flow_loading: bool,
    /// When the tape was last asked for, so a request that never answers can be let go.
    pub flow_asked: Option<Instant>,
    pub flow_at: Option<Instant>,
    /// Why the last flow refresh failed, while the tape still shows what it has.
    pub flow_error: Option<String>,
    /// The pool under the cursor and when it got there, so scrolling past a row does not fetch
    /// it. Only a selection that has settled for [`SELECTION_SETTLES`] is asked about.
    pub selected_at: Option<(String, Instant)>,
    derived: RefCell<DerivedMarkets>,
    revisions: HashMap<String, u64>,
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct DerivedKey {
    pool: String,
    revision: u64,
    events: (usize, usize),
    indexed: (usize, usize),
    decimals: (u8, u8),
    reserves: (u64, u64),
    base0: bool,
    bucket: u64,
    boundary: u64,
    count: usize,
}

#[derive(Default)]
struct DerivedMarkets {
    candles: HashMap<DerivedKey, Arc<Vec<wallet_core::markets::Candle>>>,
    stats: HashMap<DerivedKey, wallet_core::markets::PairStats>,
    trades: HashMap<DerivedKey, Arc<Vec<wallet_core::markets::Trade>>>,
}

impl MarketsView {
    fn changed(&mut self, pool: &str) {
        let revision = self.revisions.entry(pool.to_owned()).or_default();
        *revision = revision.wrapping_add(1);
        let cache = self.derived.get_mut();
        cache.candles.retain(|key, _| key.pool != pool);
        cache.stats.retain(|key, _| key.pool != pool);
        cache.trades.retain(|key, _| key.pool != pool);
    }

    fn derived_key(&self, pool: &wallet_core::markets::Pool, base0: bool, bucket: u64, count: usize, now: u64) -> DerivedKey {
        let events = self.events.get(&pool.address).and_then(|value| value.as_ref().ok());
        let indexed = self.candles.get(&(pool.address.clone(), bucket));
        DerivedKey {
            pool: pool.address.clone(),
            revision: *self.revisions.get(&pool.address).unwrap_or(&0),
            events: events.map_or((0, 0), |v| (v.as_ptr() as usize, v.len())),
            indexed: indexed.map_or((0, 0), |v| (v.as_ptr() as usize, v.len())),
            decimals: (pool.token0.decimals, pool.token1.decimals),
            reserves: (pool.reserve0.to_bits(), pool.reserve1.to_bits()),
            base0,
            bucket,
            boundary: now.checked_div(bucket).unwrap_or(0),
            count,
        }
    }
}

impl Default for MarketsView {
    fn default() -> Self {
        MarketsView {
            candles: HashMap::new(),
            candles_requested: HashMap::new(),
            pools: None,
            pools_loading: false,
            pools_at: None,
            pools_attempted: None,
            reserves_loading: false,
            reserves_at: None,
            reserves_attempted: None,
            reserves_asked_block: None,
            reserves_block: None,
            events: HashMap::new(),
            events_loading: None,
            events_prefetching: None,
            history_coverage: HashMap::new(),
            events_at: HashMap::new(),
            timeframe: 1,
            pan: Default::default(),
            flipped: Default::default(),
            pair_selected: 0,
            flow_selected: 0,
            flow_min_usd: 0.0,
            sort: MarketSort::default(),
            flow: Vec::new(),
            flow_loading: false,
            flow_asked: None,
            flow_at: None,
            flow_error: None,
            selected_at: None,
            derived: RefCell::default(),
            revisions: HashMap::new(),
        }
    }
}

/// People › Board state: one channel's messages at a time, kept per channel.
#[derive(Default)]
pub struct BoardView {
    /// Messages per channel, newest first as the reader returns them.
    pub posts: HashMap<String, Result<Vec<wallet_core::messages::Post>, String>>,
    /// The channel a request is in flight for.
    pub loading: Option<String>,
    /// When each channel was last read.
    pub at: HashMap<String, Instant>,
    /// Sealed conversations by peer payment code, opened by the wallet worker.
    pub dms: HashMap<String, Result<Vec<wallet_core::ops::SealedLine>, String>>,
    /// The conversation a read is in flight for.
    pub dm_loading: Option<String>,
    /// When each conversation was last read.
    pub dm_at: HashMap<String, Instant>,
    /// Channels seen on the board, followed or not.
    pub known: Vec<wallet_core::messages::ChannelSummary>,
    pub known_at: Option<Instant>,
    pub known_loading: bool,
    /// Typed filter over the list; `Some("")` means the filter is open and empty.
    pub filter: Option<String>,
    /// The newest message height this wallet has looked at, per channel. Seeded from the first
    /// scan so opening the wallet does not announce everything already on the board.
    pub seen: HashMap<String, u64>,
    /// How many unread each channel had when it was last announced, so a toast says a thing once.
    pub announced: HashMap<String, u32>,
    /// Chats that notify (`#channel` / `dm:<code>`), and the one docked beside every screen.
    pub subs: Vec<String>,
    pub pin: Option<String>,
    pub chat_loaded: bool,
    /// When this window last checked subscribed chats (only when no daemon does).
    pub news_checked: Option<Instant>,
    /// Private messages (v3): where messaging stands, conversations and requests. Decrypted, so
    /// it goes at every lock and switch ([`App::forget_private`]).
    pub msg: Option<Result<MessagingView, String>>,
    /// One conversation's messages by peer address, oldest first.
    pub msg_lines: HashMap<String, Result<Vec<wallet_core::messaging::service::Line>, String>>,
    pub msg_loading: bool,
    pub msg_at: Option<Instant>,
    /// The unlock already offered this week's key, so it is said once.
    pub msg_offered: bool,
}

/// What the wallet worker says about private messages.
#[derive(Clone, Debug)]
pub struct MessagingView {
    pub status: wallet_core::messaging::service::Status,
    pub conversations: Vec<wallet_core::messaging::service::Conversation>,
    pub requests: Vec<wallet_core::messaging::service::Conversation>,
}

/// What the board's left column lists: a public channel, or a person to write to in private.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BoardRow {
    Channel(String),
    /// A channel seen on the board that this wallet does not follow, and how many messages it has.
    Unfollowed(String, u32),
    /// An old (v1/v2) payment-code conversation, read-only: code, and the contact name.
    Peer(String, Option<String>),
    /// The messaging account: which account messages go from, its balance and keys, and the
    /// list to choose it from.
    Messaging,
    /// A private conversation: messaging address, and the contact name.
    Chat(String, Option<String>),
    /// Someone who wrote first and waits to be accepted: messaging address.
    Request(String),
}

/// Candles shown on the chart.
pub const MARKET_CANDLES: usize = 64;

/// How often the Markets screen asks for fresh numbers.
///
/// It matches the zone's block time, so the screen moves at the speed the chain does. The request
/// is not what costs — each source's own TTL decides whether a tick reaches the network at all,
/// and a tick inside that window is served from the store.
pub const MARKET_REFRESH: Duration = Duration::from_secs(wallet_core::markets::MARKET_TICK_SECS);
/// A market read still unanswered after this is taken as lost and asked again. Every source gives
/// up well before it: the HTTP client after 20 s, a slow venue after `markets::VENUE_DEADLINE`.
pub const MARKET_STUCK: Duration = Duration::from_secs(45);
/// While blocks are arriving, a chain-backed feed is re-read on each block (`App::on_block`) and
/// its own clock is only the safety net: between blocks the chain has not moved, and a read then
/// returns what is already on screen. This is that net's interval.
pub const BLOCK_PACED_FALLBACK: Duration = Duration::from_secs(20);
/// How long a PnL answer is shown before opening the screen reads it again.
pub const PNL_TTL: Duration = Duration::from_secs(30);
/// The swap card's pair chart: hourly, which the indexer buckets, so it is one query.
pub const SWAP_CHART_BUCKET: u64 = 3_600;

/// How long the cursor has to rest on a market row before the wallet fetches anything for it.
///
/// Long enough that holding a cursor key does not fetch every row it passes, short enough that
/// stopping on a row feels immediate.
/// How far from the cursor, in rows, charts are loaded before the cursor gets there: the order
/// is below, above, two below, and so on. One loads at a time, only after the selected pair's own.
pub const PREFETCH_ROWS: [isize; 6] = [1, -1, 2, 3, -2, 4];
pub const SELECTION_SETTLES: Duration = Duration::from_millis(200);

/// A signing sequence the TUI drives to completion across screens: every step is still its own
/// review, and the next review opens only after the previous transaction confirms.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum FlowKind {
    /// Exact approval(s) while needed, then the swap. A pool that trades WQUAI can be paid from
    /// QUAI: `prewrap` wraps what is missing first, and `unwrap_after` redeems WQUAI the swap
    /// paid out (with the WQUAI balance held before it in `baseline`).
    Swap {
        account: Option<String>,
        from: String,
        to: String,
        amount: String,
        slippage: u16,
        deadline: u32,
        label: String,
        prewrap: Option<String>,
        unwrap_after: bool,
        baseline: String,
        /// A route across both exchanges: this swap ends on the hub (`to`), and `then` swaps
        /// exactly what it paid onward.
        then: Option<NextSwap>,
    },
    /// Marketplace module approval, token approval, then the buy.
    NftBuy { account: Option<String>, contract: String, token_id: String, price: Option<String>, label: String },
    /// Module approval, collection approval, then the listing (or its new price). `price: None`
    /// cancels the listing.
    NftList { account: Option<String>, contract: String, token_id: String, price: Option<String>, currency: String, label: String },
    /// Claim settled wrapped Qi as WQI.
    Claim { account: Option<String>, qits: String },
    /// A sequence the worker walks one review at a time: the same request is made again after
    /// each step, and answers with the next exact approval while one is needed, then with the
    /// operation itself. Deposits, withdrawals, staking and gauge funding all take this shape.
    Steps { prepare: Box<Prepare>, label: String },
}

/// Where a flow's step stands, for the stepper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepState {
    Done,
    Now,
    Next,
}

impl Flow {
    /// The steps of this sequence as the stepper draws them: those sent, the one under way, and
    /// what is known to follow. `current` is the step an open review is for (an approval the
    /// worker asked for is only known then); approvals are never guessed ahead of time.
    pub fn stepper(&self, current: Option<&str>) -> Vec<(String, StepState)> {
        let mut out: Vec<(String, StepState)> = self.done.iter().map(|d| (d.clone(), StepState::Done)).collect();
        let sent = |name: &str| self.done.iter().filter(|d| *d == name).count();
        let mut ahead: Vec<String> = Vec::new();
        match &self.kind {
            FlowKind::Swap { prewrap, unwrap_after, then, .. } => {
                if prewrap.is_some() {
                    ahead.push("wrap QUAI".into());
                }
                let swaps = if then.is_some() { 2 } else { 1 };
                for i in sent("swap").max(usize::from(self.swapped))..swaps {
                    ahead.push(if i == 0 { "swap".into() } else { "swap on".into() });
                }
                if *unwrap_after && sent("unwrap WQUAI") == 0 {
                    ahead.push("unwrap WQUAI".into());
                }
            }
            FlowKind::NftBuy { .. } => ahead.push("buy".into()),
            FlowKind::NftList { price: None, .. } => ahead.push("cancel listing".into()),
            FlowKind::NftList { .. } => ahead.push("list".into()),
            FlowKind::Claim { .. } => ahead.push("claim".into()),
            FlowKind::Steps { label, .. } => ahead.push(label.clone()),
        }
        if let Some(now) = current {
            if ahead.first().is_some_and(|a| a == now) {
                ahead.remove(0);
            }
            out.push((now.to_string(), StepState::Now));
            out.extend(ahead.into_iter().map(|a| (a, StepState::Next)));
        } else {
            for (i, a) in ahead.into_iter().enumerate() {
                out.push((a, if i == 0 { StepState::Now } else { StepState::Next }));
            }
        }
        out
    }
}

/// A step, in the words the stepper uses, from the operation kind that was sent.
pub fn step_name(kind: &str) -> String {
    match kind {
        "approve" => "approve".into(),
        "wrap_quai" => "wrap QUAI".into(),
        "unwrap_quai" => "unwrap WQUAI".into(),
        "wrap_qi" => "wrap Qi".into(),
        "unwrap_wqi" => "redeem WQI".into(),
        "claim_wqi" => "claim".into(),
        k if k.starts_with("nft_buy") => "buy".into(),
        k if k.starts_with("nft_list") => "list".into(),
        k => k.replace('_', " "),
    }
}

impl FlowKind {
    pub fn label(&self) -> String {
        match self {
            FlowKind::Swap { label, .. }
            | FlowKind::NftBuy { label, .. }
            | FlowKind::NftList { label, .. }
            | FlowKind::Steps { label, .. } => label.clone(),
            FlowKind::Claim { qits, .. } => {
                format!("claim {} Qi as WQI", amount::qi(qits.parse().unwrap_or_default()))
            }
        }
    }
}

/// The second swap of a route across both exchanges.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NextSwap {
    /// Where the route ends: `quai` or a token address.
    pub to: String,
    /// Whether that end is WQUAI, to redeem for QUAI afterwards.
    pub unwrap_after: bool,
    /// The hub's decimals, to size the second swap from what the first paid.
    pub hub_decimals: u8,
    /// The first swap once sent; its receipt records what it paid out.
    pub first: Option<String>,
    /// Refreshes asked for while that output was not visible yet.
    pub polls: u8,
}

/// Refreshes to wait for a confirmed first swap's output before giving the route back to the user.
const SECOND_SWAP_POLLS: u8 = 20;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Flow {
    #[serde(skip)]
    pub checkpoint: Option<wallet_core::plans::TradePlan>,
    #[serde(skip)]
    pub lease: Option<Arc<std::fs::File>>,
    pub kind: FlowKind,
    /// A swap sequence has sent its swap; what remains is redeeming the output.
    pub swapped: bool,
    /// A review was requested from the worker and has not arrived yet.
    pub requested: bool,
    /// The review currently open for this flow.
    pub review_op: Option<String>,
    /// A submitted step (an approval) that must confirm before the next review.
    pub waiting: Option<String>,
    /// Last submitted step, retained after inclusion for receipt-attributed continuation.
    pub last_operation: Option<String>,
    /// Reviews shown so far.
    pub steps: u8,
    /// The steps sent so far, in words ("approve", "swap"): what the stepper shows as done.
    #[serde(default)]
    pub done: Vec<String>,
    #[serde(skip, default = "Instant::now")]
    pub last_poll: Instant,
}

type ResumableFlow = (Flow, Option<(String, String)>);

/// A PNG ready for the terminal and its content key (kitty transmits each key once).
pub type KittyPng = (Arc<Vec<u8>>, u64);

/// A bitmap queued for this frame: cell area, PNG and z.
pub type KittyItem = (ratatui::layout::Rect, KittyPng, i32);

/// Fitted-picture cache key: rendition, NFT art (may get a card), canvas width and height, theme.
pub type FittedKey = (u64, bool, u32, u32, u64);

/// Windows the marketplace view widens to, in order, when a shorter one holds no sales.
pub const TRADE_WINDOWS: [u64; 4] = [7, 30, 90, 365];

/// How Explore orders collections.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CollectionSort {
    /// Traded most over the shown window: what is moving now.
    #[default]
    Volume7d,
    /// Traded most since the marketplace opened.
    Volume,
    /// Highest floor first.
    Floor,
    /// Most items listed for sale.
    Listings,
    /// Most holders.
    Holders,
    Name,
}

impl CollectionSort {
    pub fn label(self) -> &'static str {
        match self {
            CollectionSort::Volume7d => "recent volume",
            CollectionSort::Volume => "all-time volume",
            CollectionSort::Floor => "floor",
            CollectionSort::Listings => "listings",
            CollectionSort::Holders => "holders",
            CollectionSort::Name => "name",
        }
    }

    pub fn next(self) -> Self {
        match self {
            CollectionSort::Volume7d => CollectionSort::Volume,
            CollectionSort::Volume => CollectionSort::Floor,
            CollectionSort::Floor => CollectionSort::Listings,
            CollectionSort::Listings => CollectionSort::Holders,
            CollectionSort::Holders => CollectionSort::Name,
            CollectionSort::Name => CollectionSort::Volume7d,
        }
    }
}

/// Ecosystem state.
/// Explore's view model: worked out when its inputs change, never per frame. Sorting used to call
/// `nft_window` inside the comparator, and each call cloned every trade of that collection; on
/// real data that was 8.5 ms a frame (21 ms p99).
pub struct ExploreIndex {
    /// Fingerprint of everything below was built from (see `Eco::explore_key`).
    key: u64,
    /// The shortest trade window with any sale in it.
    window: u64,
    /// (volume in QUAI, sales) per contract over `window`.
    by_contract: HashMap<String, (f64, usize)>,
    /// Collections shown, in order: indices into `collections`.
    rows: Vec<usize>,
}

#[derive(Default)]
pub struct Eco {
    pub split_request: Option<(u64, u64)>,
    pub max_sequence: u64,
    pub max_request: Option<(u64, u64)>,
    pub portfolio: Option<Portfolio>,
    pub portfolio_error: Option<String>,
    pub portfolio_requested: Option<Instant>,
    pub portfolio_signature: Option<String>,
    pub markets: Vec<TokenMarket>,
    pub images: HashMap<(String, u32), ImageSlot>,
    pub wants: RefCell<Vec<(String, u32)>>,
    /// Bitmaps to place after this frame: cell area, PNG (already fitted to the area) and z.
    pub kitty: RefCell<Vec<KittyItem>>,
    /// Ambient animation clock (ms): advances only while something animates on its own and the
    /// terminal has focus, so a resumed animation continues where it paused.
    pub anim_ms: u64,
    pub anim_last: Option<Instant>,
    /// Frame step requested by the last frame (see `edge`), and the step index drawn.
    pub anim_step: std::cell::Cell<Option<u64>>,
    pub anim_drawn: std::cell::Cell<u64>,
    /// Inline badges drawn this frame that have an icon ready: letters, badge color, icon.
    pub inline_icons: RefCell<Vec<(String, ratatui::style::Color, Arc<Rendition>)>>,
    /// Pictures padded to their cell aspect (and carded where needed), encoded once.
    pub fitted: RefCell<HashMap<FittedKey, KittyPng>>,
    pub nfts: Option<Result<Vec<OwnedNft>, String>>,
    pub nfts_loading: bool,
    pub collections: Option<Result<Vec<Collection>, String>>,
    pub collections_loading: bool,
    /// Floors, volume and listing counts per collection (the marketplace indexer), by contract.
    pub nft_stats: HashMap<String, wallet_core::market::CollectionStats>,
    pub nft_stats_error: Option<String>,
    pub nft_stats_at: Option<Instant>,
    /// Every marketplace sale the indexer has, newest first; windows are counted from it.
    pub nft_trades: Vec<wallet_core::market::Trade>,
    /// The sequence the last result belongs to: its name and the steps sent, for the result
    /// dialog.
    pub flow_summary: Option<(String, Vec<String>)>,
    /// Limit orders (Trade › Orders), once read.
    pub orders: Option<Vec<wallet_core::plans::TradePlan>>,
    /// When the open terminal last re-checked its active limit orders.
    pub orders_watched_at: Option<std::time::Instant>,
    /// Explore's rows and trade windows, rebuilt when their inputs change.
    pub explore_index: RefCell<Option<ExploreIndex>>,
    /// Markets' row order (indices into the pool directory) and the fingerprint it was sorted for.
    pub market_order: RefCell<Option<(u64, Vec<usize>)>>,
    pub nft_trades_at: Option<Instant>,
    /// How Explore is ordered (`S`).
    pub collection_sort: CollectionSort,
    /// Collection search text; `Some` while typing.
    pub search: Option<String>,
    pub search_text: String,
    pub collection_items: HashMap<String, Result<Vec<NftItem>, String>>,
    /// How many items each collection has in all, when the explorer said.
    pub collection_total: HashMap<String, u64>,
    /// Collections with a further page on its way, and those whose last one failed (not asked
    /// again until the view is reopened).
    pub collection_paging: HashSet<String>,
    pub collection_more_failed: HashSet<String>,
    pub listings: HashMap<Option<String>, Result<Vec<Listing>, String>>,
    pub listings_loading: bool,
    /// Listings by this wallet's accounts (Bazarr indexer; re-checked on-chain before changes).
    pub my_listings: Option<Result<Vec<Listing>, String>>,
    /// Listings screen shows only this wallet's listings (`m`).
    pub listings_mine: bool,
    /// Listings order (`S`).
    pub listing_sort: wallet_core::market::ListingSort,
    /// Listings collection filter (`f` / `F` cycle; None = all).
    pub listing_filter: Option<String>,
    pub nft_meta: HashMap<(String, String), Result<NftItem, String>>,
    pub asks: HashMap<(String, String), Result<AskCheck, String>>,
    pub token_info: HashMap<String, Result<(TokenInfo, Option<bool>), String>>,
    /// This wallet's trading performance, when it was asked for, and whether an answer is due.
    pub pnl: Option<Result<wallet_core::pnl::Pnl, String>>,
    pub pnl_at: Option<Instant>,
    pub pnl_loading: bool,
    /// Quainance's launch zone, newest first.
    pub launches: Option<Result<Vec<wallet_core::launches::Launch>, String>>,
    pub launches_at: Option<Instant>,
    /// Launch tokens' logos by token address (Quainance's media proxy), kept across list refreshes.
    pub launch_logos: HashMap<String, String>,
    /// Hashrate, transactions and gas over time for System › Network, and when it was asked for.
    pub chain_stats: Option<Result<wallet_core::chainstats::ChainStats, String>>,
    pub chain_stats_at: Option<Instant>,
    /// Bonding curves by token, and when each was last asked for.
    pub curves: HashMap<String, Result<wallet_core::curve::CurveMarket, String>>,
    pub curves_at: HashMap<String, Instant>,
    /// Value and fee of observed transactions, by hash, read when their detail is shown.
    pub tx_costs: HashMap<String, Result<wallet_core::track::TxCost, String>>,
    pub tx_costs_asked: std::collections::HashSet<String>,
    pub lockups: Option<Result<u64, String>>,
    pub test: Option<TestResults>,
    pub testing: bool,
    pub swap: SwapCard,
    pub convert: ConvertCard,
    pub wrap: WrapCard,
    /// Home price-source line toggled with `i`.
    pub info_open: bool,
    /// The signing sequence in progress, if any.
    pub flow: Option<Flow>,
    /// Trade › Markets.
    pub markets_view: MarketsView,
    /// This wallet's alerts and watched pools on this network, as last read.
    pub alerts: Vec<wallet_core::alerts::Alert>,
    pub watchlist: Vec<String>,
    pub alerts_loaded: bool,
    /// When the TUI last checked alerts itself (it does only when no daemon runs).
    pub alerts_checked: Option<Instant>,
    /// The newest block height the wallet worker has seen (`Ev::Head`), and when it arrived.
    pub head: u64,
    pub head_at: Option<Instant>,
    pub pools_view: PoolsView,
    /// Zone gas price in wei, for the reserve MAX holds back. None until it loads, which makes
    /// MAX on native QUAI say so rather than guess.
    pub gas_price: Option<U256>,
    pub board: BoardView,
    /// The channel the cursor left behind while the messages pane holds it.
    pub board_channel_selected: usize,
    /// The message the cursor left behind while the channel list holds it.
    pub board_post_selected: usize,
    /// Collection detail: focused pane (false = item grid, true = its listings) and listing row.
    pub collection_listings_focused: bool,
    pub collection_listing: usize,
    /// Unclaimed wrapped Qi (Qits) whose claim review was rejected this session; not prompted again
    /// until the amount changes.
    pub claim_declined: Option<String>,
    /// Grid columns of the last Collected render (for j/k by rows).
    pub grid_columns: RefCell<usize>,
    /// Every section's data was requested in the background for this network.
    pub preloaded: bool,
    /// NFT metadata wanted by this frame (listing rows show the explorer's thumbnail).
    pub meta_wants: RefCell<Vec<(String, String)>>,
    /// NFT metadata already requested this session.
    pub meta_requested: std::collections::HashSet<(String, String)>,
}

impl Eco {
    /// When the Convert card's comparison was asked for.
    pub fn convert_quoted_at(&self) -> Option<Instant> {
        self.convert.quoted_at
    }

    pub fn nft_len(&self) -> usize {
        match &self.nfts {
            Some(Ok(v)) => v.len(),
            _ => 0,
        }
    }

    pub fn listings_len(&self) -> usize {
        self.visible_listings().len()
    }

    /// Listings as shown: filtered to the chosen collection, in the chosen order.
    pub fn visible_listings(&self) -> Vec<Listing> {
        if self.listings_mine {
            let mut rows = match &self.my_listings {
                Some(Ok(v)) => v.clone(),
                _ => Vec::new(),
            };
            wallet_core::market::sort_listings(&mut rows, self.listing_sort);
            return rows;
        }
        let Some(Ok(all)) = self.listings.get(&None) else { return Vec::new() };
        let mut rows: Vec<Listing> =
            all.iter().filter(|l| self.listing_filter.as_ref().is_none_or(|c| c.eq_ignore_ascii_case(&l.contract))).cloned().collect();
        wallet_core::market::sort_listings(&mut rows, self.listing_sort);
        rows
    }

    /// Display name for a collection contract (explorer directory, else the short address).
    pub fn collection_name(&self, contract: &str) -> String {
        match &self.collections {
            Some(Ok(v)) => v.iter().find(|c| c.address.eq_ignore_ascii_case(contract)).map(|c| c.name.clone()),
            _ => None,
        }
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| wallet_core::session::short_address(contract))
    }

    /// What Explore's view model depends on, cheaply: the data, the search, the sort, and the
    /// minute (trade windows slide with the clock).
    fn explore_key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (wallet_core::registry::now() / 60).hash(&mut h);
        self.search_text.hash(&mut h);
        (self.collection_sort as u8).hash(&mut h);
        for t in &self.nft_trades {
            (t.at, t.contract.as_str(), t.price_quai.map(f64::to_bits)).hash(&mut h);
        }
        if let Some(Ok(v)) = &self.collections {
            for c in v {
                (c.address.as_str(), c.holders).hash(&mut h);
            }
        }
        let mut stats: Vec<(&String, u64, u64, u64)> = self
            .nft_stats
            .iter()
            .map(|(k, s)| (k, s.volume_quai.map_or(0, f64::to_bits), s.floor.map_or(0, f64::to_bits), s.active_listings.unwrap_or(0)))
            .collect();
        stats.sort_unstable();
        stats.hash(&mut h);
        h.finish()
    }

    /// Explore's view model, rebuilt only when `explore_key` changes.
    fn explore_index(&self) -> std::cell::Ref<'_, ExploreIndex> {
        let key = self.explore_key();
        if self.explore_index.borrow().as_ref().is_none_or(|i| i.key != key) {
            *self.explore_index.borrow_mut() = Some(self.build_explore_index(key));
        }
        std::cell::Ref::map(self.explore_index.borrow(), |i| i.as_ref().expect("built above"))
    }

    fn build_explore_index(&self, key: u64) -> ExploreIndex {
        let now = wallet_core::registry::now();
        // The same window the rows are labelled with, so sorting by recent volume orders by the
        // number actually on screen.
        let window = TRADE_WINDOWS
            .iter()
            .copied()
            .find(|d| wallet_core::market::trade_window(&self.nft_trades, *d, now).1 > 0)
            .unwrap_or(TRADE_WINDOWS[TRADE_WINDOWS.len() - 1]);
        // One pass over the trades, grouped by contract.
        let since = now.saturating_sub(window * 86_400);
        let mut by_contract: HashMap<String, (f64, usize)> = HashMap::new();
        for t in self.nft_trades.iter().filter(|t| t.at >= since) {
            let e = by_contract.entry(t.contract.to_lowercase()).or_default();
            e.1 += 1;
            if t.is_native() {
                e.0 += t.price_quai.unwrap_or(0.0);
            }
        }
        let q = self.search_text.to_lowercase();
        let all: &[Collection] = match &self.collections {
            Some(Ok(v)) => v,
            _ => &[],
        };
        let mut rows: Vec<usize> = (0..all.len())
            .filter(|i| q.is_empty() || all[*i].name.to_lowercase().contains(&q) || all[*i].symbol.to_lowercase().contains(&q))
            .collect();
        // A collection with nothing to show for a measure sorts last, so the rows that carry the
        // number the user asked for are the ones at the top. Keys are worked out once per row.
        let key_of = |c: &Collection| -> (bool, f64) {
            let stats = self.nft_stats.get(&c.address.to_lowercase());
            let value = match self.collection_sort {
                CollectionSort::Volume7d => by_contract.get(&c.address.to_lowercase()).map_or(0.0, |v| v.0),
                CollectionSort::Volume => stats.and_then(|s| s.volume_quai).unwrap_or(0.0),
                CollectionSort::Floor => stats.filter(|s| s.floor_is_native()).and_then(|s| s.floor).unwrap_or(0.0),
                CollectionSort::Listings => stats.and_then(|s| s.active_listings).unwrap_or(0) as f64,
                CollectionSort::Holders => stats.and_then(|s| s.holders).or(c.holders).unwrap_or(0) as f64,
                CollectionSort::Name => 0.0,
            };
            (value > 0.0, value)
        };
        let names: Vec<String> = all.iter().map(|c| c.name.to_lowercase()).collect();
        if self.collection_sort == CollectionSort::Name {
            rows.sort_by(|a, b| names[*a].cmp(&names[*b]));
        } else {
            let keys: HashMap<usize, (bool, f64)> = rows.iter().map(|i| (*i, key_of(&all[*i]))).collect();
            rows.sort_by(|a, b| {
                let (ka, kb) = (keys[a], keys[b]);
                kb.0.cmp(&ka.0).then(kb.1.total_cmp(&ka.1)).then_with(|| names[*a].cmp(&names[*b]))
            });
        }
        ExploreIndex { key, window, by_contract, rows }
    }

    pub fn collections_filtered(&self) -> Vec<&Collection> {
        let rows = self.explore_index().rows.clone();
        match &self.collections {
            Some(Ok(v)) => rows.into_iter().filter_map(|i| v.get(i)).collect(),
            _ => Vec::new(),
        }
    }

    /// Marketplace volume in QUAI and number of sales for one collection over the last `days`.
    pub fn nft_window(&self, contract: &str, days: u64) -> (f64, usize) {
        let index = self.explore_index();
        if days == index.window {
            return index.by_contract.get(&contract.to_lowercase()).copied().unwrap_or((0.0, 0));
        }
        drop(index);
        let c = contract.to_lowercase();
        let rows: Vec<wallet_core::market::Trade> = self.nft_trades.iter().filter(|t| t.contract == c).cloned().collect();
        wallet_core::market::trade_window(&rows, days, wallet_core::registry::now())
    }

    /// The shortest of [`TRADE_WINDOWS`] that has any sale in it, market-wide.
    ///
    /// This marketplace does a handful of sales a week — three in the seven days to 2026-09-21,
    /// against twelve in thirty — so a fixed week left almost every row reading "—" with perfectly
    /// good history behind it. The window is chosen once for the whole screen, so the rows stay
    /// comparable with each other, and every label says which window it is rather than claiming a
    /// week. Falls back to the longest when nothing has traded at all.
    pub fn trade_window_days(&self) -> u64 {
        self.explore_index().window
    }

    /// Column heading for what a collection traded. With no sales anywhere there is no window to
    /// name — saying "365d volume" over a column of dashes claims a measurement that was never
    /// made — so the heading is bare until there is something to have measured.
    pub fn trade_window_label(&self) -> String {
        if self.nft_trades.is_empty() { "volume".into() } else { format!("{}d volume", self.trade_window_days()) }
    }

    /// Record a wanted image and return it when ready (with fade-in progress 0..=1).
    /// A loaded rendition without requesting it.
    pub fn cached_image(&self, url: &str, edge: u32) -> Option<Arc<Rendition>> {
        match self.images.get(&(url.to_string(), edge)) {
            Some(ImageSlot::Ready(r, _)) => Some(r.clone()),
            _ => None,
        }
    }

    pub fn image(&self, url: &str, edge: u32) -> Option<(Arc<Rendition>, f32)> {
        match self.images.get(&(url.to_string(), edge)) {
            Some(ImageSlot::Ready(r, at)) => Some((r.clone(), (at.elapsed().as_millis() as f32 / 300.0).min(1.0))),
            Some(ImageSlot::Loading) => None,
            Some(ImageSlot::Failed(retry)) if Instant::now() < *retry => None,
            _ => {
                let mut wants = self.wants.borrow_mut();
                if wants.len() < 64 && !wants.iter().any(|(u, e)| u == url && *e == edge) {
                    wants.push((url.to_string(), edge));
                }
                None
            }
        }
    }

    /// Explorer metadata for an NFT, requested once when not loaded (see `flush_image_wants`).
    pub fn want_meta(&self, contract: &str, token_id: &str) {
        let key = (contract.to_lowercase(), token_id.to_string());
        if !self.nft_meta.contains_key(&key) {
            let mut wants = self.meta_wants.borrow_mut();
            if wants.len() < 64 && !wants.contains(&key) {
                wants.push(key);
            }
        }
    }

    /// Any image still fading in (drives redraws).
    pub fn fading(&self) -> bool {
        self.images.values().any(|s| matches!(s, ImageSlot::Ready(_, at) if at.elapsed() < Duration::from_millis(320)))
    }
}

/// Decimals not yet known for a picked token (quotes wait until the explorer answers).
pub const UNKNOWN_DECIMALS: u8 = u8::MAX;

/// Whether the router could fill a swap between this token and the other side of the card.
#[derive(Clone, Debug, PartialEq)]
pub enum RouteState {
    /// Pools have not loaded yet. Nothing is filtered — the picker never refuses on ignorance.
    Unknown,
    /// The router has a path; `thin()` says whether every useful size would move it hard.
    Fillable(RouteInfo),
    /// No path exists. Picking this token could only produce "no pool route".
    Dead,
}

impl RouteState {
    /// The row can be chosen. Unknown counts as choosable: the quote will say otherwise.
    pub fn choosable(&self) -> bool {
        !matches!(self, RouteState::Dead)
    }
}

/// One token picker row.
/// What the exchange trades: Qi, or anything the swap router knows (QUAI and tokens).
#[derive(Clone, Debug, PartialEq)]
pub enum ExAsset {
    Qi,
    Swap(SwapAsset),
}

impl ExAsset {
    pub fn symbol(&self) -> &str {
        match self {
            ExAsset::Qi => "Qi",
            ExAsset::Swap(a) => a.symbol(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct PickerEntry {
    /// The Qi row (then `asset` is not used): the exchange routes it to a conversion or a wrap.
    pub qi: bool,
    pub asset: SwapAsset,
    /// Balance or price text.
    pub info: String,
    pub verified: bool,
    pub holders: Option<u64>,
    pub icon: Option<String>,
    /// How this token would be reached from the other side of the swap.
    pub route: RouteState,
}

/// Data-source test results: service, outcome, milliseconds.
pub type TestResults = Vec<(String, Result<String, String>, u128)>;

fn digits_input(text: &mut String, key: &KeyEvent, max_decimals: u8) -> bool {
    match key.code {
        KeyCode::Char(c) if c.is_ascii_digit() => {
            if let Some((_, frac)) = text.split_once('.')
                && frac.len() >= usize::from(max_decimals)
            {
                return true;
            }
            if text.len() < 40 {
                text.push(c);
            }
            true
        }
        KeyCode::Char('.') if !text.contains('.') && max_decimals > 0 => {
            if text.is_empty() {
                text.push('0');
            }
            text.push('.');
            true
        }
        KeyCode::Backspace => {
            text.pop();
            true
        }
        _ => false,
    }
}

fn hash_key(parts: &[&str]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for p in parts {
        p.hash(&mut h);
    }
    h.finish().max(1)
}

mod board;
mod cards;
mod data_events;
mod flows;
mod launches;
mod markets;
mod nfts;
mod pools;

impl App {
    // ------------------------------------------------------------------ markets

    // -------------------------------------------------------------------- board

    // ------------------------------------------------------------------ signing sequences

    /// Enter on ecosystem views.
    pub fn enter_eco(&mut self) {
        match self.screen {
            Screen::Home if self.pane == 1 => {
                if let Some(key) = self.activity_key(self.selected) {
                    self.push_detail(Detail::Activity(key));
                }
            }
            Screen::Home => {
                let tokens = self.eco.portfolio.as_ref().map_or(0, |p| p.rows.len());
                if let Some(row) = self.eco.portfolio.as_ref().and_then(|p| p.rows.get(self.selected)) {
                    self.push_detail(Detail::Asset(row.key.id()));
                } else if let Some(pair) = self.home_positions().get(self.selected - tokens).map(|p| p.pair.clone()) {
                    // A position opens where it can be worked on: Pools, with it under the cursor.
                    self.switch(Screen::Pools);
                    self.selected = self.position_rows().iter().position(|p| p.pair == pair).unwrap_or(0);
                }
            }
            Screen::Collected => {
                if let Some(Ok(items)) = &self.eco.nfts
                    && let Some(n) = items.get(self.selected)
                {
                    let (c, id) = (n.item.contract.clone(), n.item.token_id.clone());
                    self.push_detail(Detail::Nft(c, id));
                }
            }
            Screen::Explore => {
                if let Some(c) = self.eco.collections_filtered().get(self.selected).map(|c| c.address.clone()) {
                    self.push_detail(Detail::Collection(c));
                }
            }
            Screen::Listings => {
                if let Some(l) = self.eco.visible_listings().get(self.selected) {
                    let (c, id) = (l.contract.clone(), l.token_id.clone());
                    self.push_detail(Detail::Nft(c, id));
                }
            }
            _ => {}
        }
    }

    /// Push a detail view and load what it needs.
    pub fn push_detail(&mut self, detail: Detail) {
        // Enter on a page that is already open is a no-op, not another copy of it on the stack:
        // otherwise every press adds an Esc the user has to press to get back out.
        if self.detail.last() == Some(&detail) {
            return;
        }
        match &detail {
            Detail::Asset(id) if id.starts_with("0x") => {
                if !self.eco.token_info.contains_key(id) {
                    self.send_data(DataCmd::TokenInfo(id.clone()));
                }
            }
            Detail::Nft(c, id) => {
                let key = (c.to_lowercase(), id.clone());
                if !self.eco.nft_meta.contains_key(&key) && !self.eco.nft_meta.contains_key(&(c.clone(), id.clone())) {
                    // One item the user opened. It may be theirs, so it is not public.
                    self.send_data(DataCmd::Nft { contract: c.clone(), token_id: id.clone(), public: false });
                }
                let buyer = self.dash.accounts.first().map(|a| a.address.clone());
                self.send_data(DataCmd::CheckAsk { contract: c.clone(), token_id: id.clone(), buyer });
            }
            Detail::Collection(c) => {
                if !self.eco.collection_items.contains_key(c) {
                    self.send_data(DataCmd::CollectionItems { contract: c.clone(), offset: 0 });
                }
                // A page that failed last time is worth another try on a fresh visit.
                self.eco.collection_more_failed.remove(c);
                self.send_data(DataCmd::Listings { collection: Some(c.clone()) });
                self.eco.collection_listings_focused = false;
                self.eco.collection_listing = 0;
            }
            _ => {}
        }
        self.kitty.clear(self.caps.tmux);
        self.detail.push(detail);
        self.detail_selected = 0;
    }

    pub fn detail_title(&self, d: &Detail) -> String {
        match d {
            Detail::Asset(id) => match id.as_str() {
                "quai" => "QUAI".into(),
                "qi" => "Qi".into(),
                address => self
                    .eco
                    .portfolio
                    .as_ref()
                    .and_then(|p| p.rows.iter().find(|r| r.key.id() == address))
                    .map(|r| r.symbol.clone())
                    .unwrap_or_else(|| wallet_core::session::short_address(address)),
            },
            Detail::Nft(c, id) => match self.eco.nft_meta.get(&(c.to_lowercase(), id.clone())) {
                Some(Ok(item)) => item.name.clone(),
                _ => format!("#{id}"),
            },
            Detail::Collection(c) => self
                .eco
                .collections
                .as_ref()
                .and_then(|r| r.as_ref().ok())
                .and_then(|v| v.iter().find(|x| x.address == *c))
                .map(|x| x.name.clone())
                .unwrap_or_else(|| wallet_core::session::short_address(c)),
            Detail::Activity(_) => "Detail".into(),
        }
    }

    /// A page of a collection's items arrived. The first replaces what was shown (keeping any
    /// later pages already loaded behind it); a later one is added only where it continues the
    /// list, so a repeated or late answer never doubles items.
    pub(crate) fn collection_page(
        &mut self,
        contract: String,
        offset: usize,
        result: Result<wallet_core::explorer::CollectionPage, String>,
    ) {
        if offset > 0 {
            self.eco.collection_paging.remove(&contract);
        }
        let page = match result {
            Ok(page) => page,
            Err(e) if offset == 0 => {
                self.eco.collection_items.insert(contract, Err(e));
                return;
            }
            Err(_) => {
                self.eco.collection_more_failed.insert(contract);
                return;
            }
        };
        if let Some(total) = page.total {
            self.eco.collection_total.insert(contract.clone(), total);
        }
        match self.eco.collection_items.get_mut(&contract) {
            Some(Ok(items)) if offset > 0 => {
                if items.len() == offset {
                    items.extend(page.items);
                }
            }
            _ if offset > 0 => {}
            Some(Ok(items)) if items.len() > page.items.len() => {
                let n = page.items.len();
                items.splice(..n, page.items);
            }
            _ => {
                self.eco.collection_items.insert(contract, Ok(page.items));
            }
        }
    }

    /// The next page of the open collection, once the selection comes within half a page of the
    /// end of what is loaded. Pages come as they are needed: a collection of thousands is not
    /// fetched whole to show its first screen.
    pub(crate) fn page_collection(&mut self) {
        let Some(Detail::Collection(c)) = self.detail.last() else { return };
        let Some(Ok(items)) = self.eco.collection_items.get(c) else { return };
        let (loaded, total) = (items.len(), self.eco.collection_total.get(c).copied().unwrap_or(0) as usize);
        let page = wallet_core::explorer::COLLECTION_PAGE;
        if loaded >= total
            || loaded == 0
            || self.detail_selected + page / 2 < loaded
            || self.eco.collection_paging.contains(c)
            || self.eco.collection_more_failed.contains(c)
        {
            return;
        }
        let c = c.clone();
        self.eco.collection_paging.insert(c.clone());
        self.send_data(DataCmd::CollectionItems { contract: c, offset: loaded });
    }

    /// Items selectable inside the top detail view.
    pub fn detail_len(&self) -> usize {
        match self.detail.last() {
            Some(Detail::Collection(c)) => match self.eco.collection_items.get(c) {
                Some(Ok(v)) => v.len(),
                _ => 0,
            },
            _ => 0,
        }
    }

    /// Keys inside detail views. Returns true when consumed.
    pub fn detail_key(&mut self, key: KeyEvent) -> bool {
        let Some(top) = self.detail.last().cloned() else { return false };
        match (top, key.code) {
            (Detail::Asset(id), KeyCode::Char('s')) => {
                match id.as_str() {
                    "quai" => self.run_action("send_quai"),
                    "qi" => self.run_action("send_qi"),
                    address => {
                        self.run_action("send_token");
                        if let Modal::Form(f) = &mut self.modal {
                            f.fields[1].value = address.to_string();
                            f.focus = 2;
                        }
                    }
                }
                true
            }
            (Detail::Asset(id), KeyCode::Char('r')) => {
                self.modal = Modal::Receive { asset_qi: id == "qi", account: 0 };
                true
            }
            // Buy this asset, or sell it, against QUAI (against WQI when the asset is QUAI
            // itself). The swap card opens with both sides filled in, ready for an amount.
            (Detail::Asset(id), KeyCode::Char('b' | 'B')) if self.config.features.trading => self.quick_swap(&id, true),
            (Detail::Asset(id), KeyCode::Char('S')) if self.config.features.trading => self.quick_swap(&id, false),
            (Detail::Asset(id), KeyCode::Char('c')) if id == "quai" || id == "qi" => {
                self.eco.convert.qi_to_quai = id == "qi";
                self.switch(Screen::Convert);
                true
            }
            (Detail::Asset(id), KeyCode::Char('w')) => {
                let network = self.net();
                let wqi = network.as_ref().and_then(|n| n.wqi.clone()).map(|a| a.to_lowercase());
                let wquai = network.as_ref().and_then(|n| n.wquai.clone()).map(|a| a.to_lowercase());
                self.eco.wrap.mode = match id.as_str() {
                    "qi" => 0,
                    "quai" => 3,
                    a if Some(a.to_string()) == wqi => 2,
                    a if Some(a.to_string()) == wquai => 4,
                    _ => return false,
                };
                self.switch(Screen::Wrap);
                true
            }
            (Detail::Nft(c, id), KeyCode::Char('T')) => {
                self.open_nft_transfer(&c, &id);
                true
            }
            (Detail::Nft(c, id), KeyCode::Char('L')) => {
                self.open_nft_list(&c, &id);
                true
            }
            (Detail::Nft(c, id), KeyCode::Char('X')) => {
                self.cancel_nft_listing(&c, &id);
                true
            }
            (Detail::Nft(..), KeyCode::Char('b')) => {
                self.buy_step();
                true
            }
            // Tab moves between the item grid and the collection's listings.
            (Detail::Collection(_), KeyCode::Tab | KeyCode::BackTab) => {
                self.eco.collection_listings_focused = !self.eco.collection_listings_focused;
                true
            }
            (Detail::Collection(c), code) if self.eco.collection_listings_focused => {
                let listings = match self.eco.listings.get(&Some(c.clone())) {
                    Some(Ok(v)) => v.clone(),
                    _ => Vec::new(),
                };
                let n = listings.len();
                match code {
                    KeyCode::Char('j') | KeyCode::Down if n > 0 => self.eco.collection_listing = (self.eco.collection_listing + 1) % n,
                    KeyCode::Char('k') | KeyCode::Up if n > 0 => self.eco.collection_listing = (self.eco.collection_listing + n - 1) % n,
                    KeyCode::Enter | KeyCode::Char('b') => {
                        let Some(l) = listings.get(self.eco.collection_listing.min(n.saturating_sub(1))).cloned() else {
                            self.toast("nothing listed in this collection right now", true);
                            return true;
                        };
                        self.push_detail(Detail::Nft(l.contract.clone(), l.token_id.clone()));
                        if code == KeyCode::Char('b') {
                            self.buy_listing(&l);
                        }
                    }
                    _ => return false,
                }
                true
            }
            (Detail::Collection(c), KeyCode::Enter) => {
                if let Some(Ok(items)) = self.eco.collection_items.get(&c)
                    && let Some(item) = items.get(self.detail_selected)
                {
                    let id = item.token_id.clone();
                    self.push_detail(Detail::Nft(c, id));
                }
                true
            }
            (Detail::Collection(_), KeyCode::Char('h') | KeyCode::Left) => {
                self.move_selection(-1);
                true
            }
            (Detail::Collection(_), KeyCode::Char('l') | KeyCode::Right) => {
                self.move_selection(1);
                true
            }
            _ => false,
        }
    }

    /// Value copied by `y` on ecosystem views.
    pub fn eco_selected_value(&self) -> Option<String> {
        match (self.detail.last(), self.screen) {
            (Some(Detail::Asset(id)), _) if id.starts_with("0x") => Some(id.clone()),
            (Some(Detail::Nft(c, _)), _) | (Some(Detail::Collection(c)), _) => Some(c.clone()),
            (None, Screen::Home) if self.pane == 0 => {
                self.eco.portfolio.as_ref().and_then(|p| p.rows.get(self.selected)).map(|r| match &r.key {
                    AssetKey::Token(a) => a.clone(),
                    _ => self.receive_address(r.key == AssetKey::Qi).unwrap_or_default(),
                })
            }
            (None, Screen::Collected) => match &self.eco.nfts {
                Some(Ok(v)) => v.get(self.selected).map(|n| n.item.contract.clone()),
                _ => None,
            },
            (None, Screen::Listings) => self.eco.visible_listings().get(self.selected).map(|l| l.contract.clone()),
            (None, Screen::Explore) => self.eco.collections_filtered().get(self.selected).map(|c| c.address.clone()),
            (None, Screen::Launches) => self.launch_rows().get(self.selected).map(|l| l.token.clone()),
            (None, Screen::Pnl) => self.pnl_positions().get(self.selected).map(|p| p.token.clone()),
            // In the pairs list, the token the market prices (not WQUAI or USDT beside it).
            (None, Screen::Markets) if self.pane == 0 => self.selected_pool().map(|p| {
                let base = if self.pool_base0(&p) { p.token0 } else { p.token1 };
                base.address
            }),
            _ => None,
        }
    }

    fn receive_address(&self, qi: bool) -> Option<String> {
        if qi { self.meta.as_ref().and_then(|m| m.payment_code.clone()) } else { self.dash.accounts.first().map(|a| a.address.clone()) }
    }

    /// Link for `o` on the focused item.
    pub fn link_for_focus(&self) -> Option<String> {
        let network = self.net()?;
        match (self.detail.last(), self.screen) {
            (Some(Detail::Asset(id)), _) if id.starts_with("0x") => network.token_url(id),
            (Some(Detail::Nft(c, id)), _) => wallet_core::market::bazarr_url(&network, c, id).or_else(|| network.token_url(c)),
            (Some(Detail::Collection(c)), _) => network.token_url(c),
            (Some(Detail::Activity(key)), _) => self.activity_tx(key).and_then(|h| network.tx_url(&h)),
            (None, Screen::Activity) => {
                self.activity_key(self.selected).and_then(|k| self.activity_tx(&k)).and_then(|h| network.tx_url(&h))
            }
            (None, Screen::Launches) => self.launch_rows().get(self.selected).and_then(|l| network.token_url(&l.token)),
            (None, Screen::Home) if self.pane == 0 => {
                match self.eco.portfolio.as_ref().and_then(|p| p.rows.get(self.selected)).map(|r| r.key.clone()) {
                    Some(AssetKey::Token(a)) => network.token_url(&a),
                    _ => None,
                }
            }
            (None, Screen::Collected) => match &self.eco.nfts {
                Some(Ok(v)) => {
                    v.get(self.selected).and_then(|n| wallet_core::market::bazarr_url(&network, &n.item.contract, &n.item.token_id))
                }
                _ => None,
            },
            (None, Screen::Listings) => self
                .eco
                .visible_listings()
                .get(self.selected)
                .and_then(|l| wallet_core::market::bazarr_url(&network, &l.contract, &l.token_id)),
            (None, Screen::Explore) => self.eco.collections_filtered().get(self.selected).and_then(|c| network.token_url(&c.address)),
            // In the flow, the transaction that made the swap; in the pairs list, the pool.
            (None, Screen::Markets) if self.pane == 1 => self.flow_rows().get(self.selected).and_then(|s| network.tx_url(&s.tx)),
            (None, Screen::Markets) => self.selected_pool().and_then(|p| network.token_url(&p.address)),
            _ => None,
        }
    }

    fn activity_tx(&self, key: &str) -> Option<String> {
        if let Some(id) = key.strip_prefix("op:") {
            return self.dash.ops.iter().find(|o| o.id == id).and_then(|o| o.tx_hash.clone());
        }
        let k = key.strip_prefix("act:")?;
        self.dash.activity.iter().find(|a| a.key == k).and_then(|a| a.tx_hash.clone())
    }
}

/// The data jobs a screen waits on, by the names the data worker gives them.
pub fn focus_jobs(screen: Screen) -> &'static [&'static str] {
    match screen {
        Screen::Home => &["portfolio", "nfts"],
        Screen::Markets => &["market_pools", "pool_reserves", "markets", "pair_candles", "pool_events", "dex_flow"],
        Screen::Swap => &["swap_quote", "markets", "market_pools"],
        Screen::Pools => &["lp_positions", "market_pools", "liquidity_quote"],
        Screen::Convert => &["qi_routes"],
        Screen::Launches => &["launches", "curve_market"],
        Screen::Collected => &["nfts", "my_listings", "nft"],
        Screen::Explore => &["collections", "collection_items"],
        Screen::Listings => &["listings", "nft", "check_ask"],
        Screen::Board | Screen::Channels => &["board", "board_channels"],
        Screen::Accounts => &["lockups"],
        Screen::Network => &["chain_stats"],
        Screen::Wallets => &["wallet_quai"],
        Screen::Activity => &["tx_cost"],
        _ => &[],
    }
}

impl App {
    /// An incoming transfer of dust (or nothing) from an address this wallet has never sent to and
    /// does not hold as a contact or account — the plant in an address-poisoning attack.
    pub fn dust_from_stranger(&self, a: &wallet_core::appdb::Activity) -> bool {
        if a.direction != "in" {
            return false;
        }
        let Some(from) = a.detail["counterparty"].as_str().map(str::to_lowercase) else { return false };
        if !wallet_core::recipient::is_dust(a) {
            return false;
        }
        let known = |addr: &str| addr.eq_ignore_ascii_case(&from);
        let own = self.meta.as_ref().is_some_and(|m| m.quai_accounts.iter().any(|x| known(&x.address)));
        let contact = self.dash.contacts.iter().any(|c| c.address.as_deref().is_some_and(known));
        let sent_to = self.dash.ops.iter().any(|o| known(&o.counterparty))
            || self.dash.activity.iter().any(|x| x.direction == "out" && x.detail["counterparty"].as_str().is_some_and(known));
        !(own || contact || sent_to)
    }
}
