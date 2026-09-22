//! Ecosystem state and interaction: portfolio, images, exchange cards (swap, convert, wrap),
//! NFTs, listings and the detail stack. Rendering lives in `views`.

use super::app::{App, Detail, FormKind, Modal, Screen};
use super::data::{DataCmd, DataEv};
use super::worker::{Cmd, Prepare};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::cell::RefCell;
use std::collections::HashMap;
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
    /// The pair chart last asked for, and when, so a sitting card does not re-ask every tick.
    pub chart_asked: Option<(String, Instant)>,
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
            chart_asked: None,
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
    pub events: HashMap<String, Result<Vec<wallet_core::markets::PoolEvent>, String>>,
    pub events_loading: Option<String>,
    pub history_coverage: HashMap<String, wallet_core::markets::HistoryCoverage>,
    /// Ready-bucketed candles from the indexer, keyed by (pool, bucket seconds). The chart uses
    /// these while the pool's logs are still loading, then keeps whichever covers more.
    pub candles: HashMap<(String, u64), Vec<wallet_core::markets::Candle>>,
    pub candles_requested: HashMap<(String, u64), Instant>,
    pub events_at: HashMap<String, (Instant, u64)>,
    /// Index into `markets::TIMEFRAMES` (1h by default).
    pub timeframe: usize,
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
            events: HashMap::new(),
            events_loading: None,
            history_coverage: HashMap::new(),
            events_at: HashMap::new(),
            timeframe: 1,
            flipped: Default::default(),
            pair_selected: 0,
            flow_selected: 0,
            flow_min_usd: 0.0,
            sort: MarketSort::default(),
            flow: Vec::new(),
            flow_loading: false,
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
}

/// What the board's left column lists: a public channel, or a person to write to in private.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BoardRow {
    Channel(String),
    /// A channel seen on the board that this wallet does not follow, and how many messages it has.
    Unfollowed(String, u32),
    /// Payment code, and the contact name when there is one.
    Peer(String, Option<String>),
}

/// Candles shown on the chart.
pub const MARKET_CANDLES: usize = 64;

/// How often the Markets screen asks for fresh numbers.
///
/// It matches the zone's block time, so the screen moves at the speed the chain does. The request
/// is not what costs — each source's own TTL decides whether a tick reaches the network at all,
/// and a tick inside that window is served from the store.
pub const MARKET_REFRESH: Duration = Duration::from_secs(5);
/// How long a PnL answer is shown before opening the screen reads it again.
pub const PNL_TTL: Duration = Duration::from_secs(30);
/// The swap card's pair chart: hourly, which the indexer buckets, so it is one query.
pub const SWAP_CHART_BUCKET: u64 = 3_600;

/// How long the cursor has to rest on a market row before the wallet fetches anything for it.
///
/// Long enough that holding a cursor key does not fetch every row it passes, short enough that
/// stopping on a row feels immediate.
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
    pub nft_trades_at: Option<Instant>,
    /// How Explore is ordered (`S`).
    pub collection_sort: CollectionSort,
    /// Collection search text; `Some` while typing.
    pub search: Option<String>,
    pub search_text: String,
    pub collection_items: HashMap<String, Result<Vec<NftItem>, String>>,
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

    pub fn collections_filtered(&self) -> Vec<&Collection> {
        let q = self.search_text.to_lowercase();
        // The same window the rows are labelled with, so sorting by recent volume orders by the
        // number actually on screen.
        let window = self.trade_window_days();
        let mut rows: Vec<&Collection> = match &self.collections {
            Some(Ok(v)) => {
                v.iter().filter(|c| q.is_empty() || c.name.to_lowercase().contains(&q) || c.symbol.to_lowercase().contains(&q)).collect()
            }
            _ => Vec::new(),
        };
        // A collection with nothing to show for a measure sorts last, so the rows that carry the
        // number the user asked for are the ones at the top.
        let key = |c: &Collection| -> (bool, f64) {
            let stats = self.nft_stats.get(&c.address.to_lowercase());
            let value = match self.collection_sort {
                CollectionSort::Volume7d => self.nft_window(&c.address, window).0,
                CollectionSort::Volume => stats.and_then(|s| s.volume_quai).unwrap_or(0.0),
                CollectionSort::Floor => stats.filter(|s| s.floor_is_native()).and_then(|s| s.floor).unwrap_or(0.0),
                CollectionSort::Listings => stats.and_then(|s| s.active_listings).unwrap_or(0) as f64,
                CollectionSort::Holders => stats.and_then(|s| s.holders).or(c.holders).unwrap_or(0) as f64,
                CollectionSort::Name => 0.0,
            };
            (value > 0.0, value)
        };
        if self.collection_sort == CollectionSort::Name {
            rows.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        } else {
            rows.sort_by(|a, b| {
                let (ka, kb) = (key(a), key(b));
                kb.0.cmp(&ka.0).then(kb.1.total_cmp(&ka.1)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
        }
        rows
    }

    /// Marketplace volume in QUAI and number of sales for one collection over the last `days`.
    pub fn nft_window(&self, contract: &str, days: u64) -> (f64, usize) {
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
        let now = wallet_core::registry::now();
        TRADE_WINDOWS
            .iter()
            .copied()
            .find(|d| wallet_core::market::trade_window(&self.nft_trades, *d, now).1 > 0)
            .unwrap_or(TRADE_WINDOWS[TRADE_WINDOWS.len() - 1])
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
#[derive(Clone, Debug)]
pub struct PickerEntry {
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

impl App {
    pub fn send_data(&self, cmd: DataCmd) {
        // A feature turned off reads nothing for it, whichever path asked.
        if cmd.feature().is_some_and(|f| !self.config.features.on(f)) {
            return;
        }
        if let Some(d) = &self.data {
            let _ = d.tx.send(cmd);
        }
    }

    /// Start the data worker once a wallet is known.
    pub fn start_data_worker(&mut self) {
        if self.data.is_some() {
            return;
        }
        let Some(meta) = &self.meta else { return };
        let Ok(network) = self.config.network(&self.network_id) else { return };
        let path = self.paths.wallet_dir(&meta.id).join("app.sqlite");
        let shared = self.paths.shared_cache();
        if let Ok(worker) = super::data::DataWorker::spawn(path, shared, network, self.config.data_policy()) {
            self.data = Some(worker);
            self.on_view_opened();
            self.preload();
        }
    }

    /// Rebind the data worker after a network switch or a data-source change.
    pub fn data_policy_changed(&mut self) {
        if let Ok(network) = self.config.network(&self.network_id) {
            self.send_data(DataCmd::Configure { network, policy: self.config.data_policy(), app_db: None });
        }
        self.eco.portfolio_requested = None;
        self.eco.portfolio_signature = None;
        self.eco.images.retain(|_, s| matches!(s, ImageSlot::Ready(..)));
        self.maybe_refresh_portfolio(true);
    }

    /// Clear network-bound ecosystem data (after a network switch).
    pub fn reset_eco_for_network(&mut self) {
        let swap_prefs = (self.eco.swap.slippage_bps, self.eco.swap.deadline_minutes);
        let images = std::mem::take(&mut self.eco.images);
        self.eco = super::eco::Eco::default();
        self.eco.images = images;
        (self.eco.swap.slippage_bps, self.eco.swap.deadline_minutes) = swap_prefs;
        self.detail.clear();
        self.data_policy_changed();
    }

    /// Open another wallet: the session follows, the keys of the old one are dropped, and every
    /// cached view belongs to the wallet that is leaving, so all of it goes. The new wallet's
    /// cache database is its own, so the data worker is rebound to it.
    pub fn switch_wallet(&mut self, id: &str) {
        let Ok(meta) = self.registry.resolve(Some(id), None) else {
            return self.toast(format!("no wallet `{id}`"), true);
        };
        if self.meta.as_ref().is_some_and(|m| m.id == meta.id) {
            return self.toast(format!("already on `{}`", meta.name), false);
        }
        let name = meta.name.clone();
        self.config.default_wallet = Some(name.clone());
        self.save_config();
        self.busy = Some(format!("opening {name}…"));
        self.send(Cmd::SwitchWallet(meta.id.clone()));
        // Images are keyed by URL, not by wallet, but everything else is this wallet's.
        self.eco = super::eco::Eco::default();
        // The screen stays where it is, so nothing re-opens it to ask for the new wallet's data.
        // The dashboard that brings the new accounts does it instead.
        self.reload_view_on_accounts = true;
        self.detail.clear();
        self.selected = 0;
        self.dash = super::worker::Dashboard {
            meta: Some(meta.clone()),
            network_id: self.network_id.clone(),
            network_name: self.dash.network_name.clone(),
            networks: std::mem::take(&mut self.dash.networks),
            ..super::worker::Dashboard::default()
        };
        self.meta = Some(meta.clone());
        // The new wallet's lock screen shows now, not when the worker reaches the switch: it may
        // be in a sync step that cannot stop, and the password can be checked meanwhile.
        if meta.kind != wallet_core::registry::WalletKind::Watch {
            self.enter_lock(None);
            self.switch_lock_pending = true;
        }
        if let Ok(network) = self.config.network(&self.network_id) {
            let app_db = self.paths.wallet_dir(&meta.id).join("app.sqlite");
            self.send_data(DataCmd::Configure { network, policy: self.config.data_policy(), app_db: Some(app_db) });
        }
    }

    /// Exact balances the portfolio starts from, taken from the wallet worker's dashboard.
    pub fn known_from_dash(&self) -> Known {
        let owners: Vec<String> = self.dash.accounts.iter().map(|a| a.address.clone()).collect();
        let quai = self.dash.accounts.iter().fold(U256::ZERO, |s, a| s.saturating_add(a.balance));
        let mut tokens: std::collections::BTreeMap<String, (String, String, u8, U256)> = std::collections::BTreeMap::new();
        for t in &self.dash.tokens {
            let e = tokens.entry(t.token.address.to_lowercase()).or_insert((
                t.token.symbol.clone(),
                t.token.name.clone(),
                t.token.decimals,
                U256::ZERO,
            ));
            e.3 = e.3.saturating_add(t.balance);
        }
        Known {
            owners,
            quai,
            qi: self.dash.qi.as_ref().map(|q| q.balance.total),
            tokens: tokens.into_iter().map(|(a, (s, n, d, b))| (a, s, n, d, b)).collect(),
        }
    }

    /// Ask for a new portfolio when balances changed or it is older than a minute.
    pub fn maybe_refresh_portfolio(&mut self, force: bool) {
        if self.dash.accounts.is_empty() || self.data.is_none() {
            return;
        }
        let known = self.known_from_dash();
        let signature =
            format!("{}:{}:{:?}:{:?}", self.dash.network_id, known.quai, known.qi, known.tokens.iter().map(|t| t.4).collect::<Vec<_>>());
        let stale = self.eco.portfolio_requested.is_none_or(|t| t.elapsed() > Duration::from_secs(60));
        if force || stale || self.eco.portfolio_signature.as_deref() != Some(signature.as_str()) {
            self.eco.portfolio_requested = Some(Instant::now());
            self.eco.portfolio_signature = Some(signature);
            self.send_data(DataCmd::Portfolio(known));
        }
    }

    /// Load what a view needs when it opens.
    pub fn on_view_opened(&mut self) {
        // What this screen waits on goes to the front of the data worker's queue.
        self.send_data(DataCmd::Focus(focus_jobs(self.screen)));
        match self.screen {
            Screen::Home => self.maybe_refresh_portfolio(false),
            Screen::Collected if self.eco.nfts.is_none() && !self.eco.nfts_loading => {
                self.load_nfts(false);
                self.load_my_listings();
            }
            Screen::Markets => {
                if self.eco.markets.is_empty() {
                    self.send_data(DataCmd::Markets);
                }
                self.tick_markets();
            }
            Screen::Board => self.tick_board(),
            Screen::Launches => self.load_launches(false),
            Screen::Pnl => self.load_pnl(false),
            Screen::Network => self.tick_chain_stats(),
            Screen::Wallets => self.load_wallets(),
            Screen::Explore => {
                if self.eco.collections.is_none() && !self.eco.collections_loading {
                    self.eco.collections_loading = true;
                    self.send_data(DataCmd::Collections { query: None });
                }
                self.load_nft_market(false);
            }
            Screen::Listings => {
                if !self.eco.listings.contains_key(&None) && !self.eco.listings_loading {
                    self.eco.listings_loading = true;
                    self.send_data(DataCmd::Listings { collection: None });
                }
                self.load_nft_market(false);
            }
            Screen::Swap => {
                if self.eco.markets.is_empty() {
                    self.send_data(DataCmd::Markets);
                }
                self.maybe_refresh_portfolio(false);
                if self.eco.swap.to.is_none() {
                    self.eco.swap.to = self.default_receive_asset();
                }
            }
            // Settled wrapped Qi waiting for its claim: open the card on Claim WQI.
            Screen::Wrap
                if self.eco.wrap.amount.is_empty()
                    && self
                        .dash
                        .wrap
                        .as_ref()
                        .and_then(|w| w.unclaimed_qits.as_deref())
                        .is_some_and(|q| q.parse::<u128>().is_ok_and(|v| v > 0)) =>
            {
                self.eco.wrap.mode = 1;
            }
            Screen::Locks if self.eco.lockups.is_none() && !self.dash.accounts.is_empty() => {
                self.send_data(DataCmd::Lockups(self.dash.accounts.iter().map(|a| a.address.clone()).collect()));
            }
            _ => {}
        }
    }

    /// Warm every section once the wallet's accounts are known, so screens open on loaded data.
    /// The data worker answers from its cache first and paces third-party requests.
    pub fn preload(&mut self) {
        let owners = self.owner_addresses();
        if self.eco.preloaded || owners.is_empty() || self.data.is_none() {
            return;
        }
        self.eco.preloaded = true;
        self.maybe_refresh_portfolio(false);
        // Only the features that are on: their loading flags would otherwise wait for an answer
        // that never comes.
        let features = self.config.features;
        if features.nfts {
            if self.eco.nfts.is_none() && !self.eco.nfts_loading {
                self.load_nfts(false);
            }
            if self.eco.my_listings.is_none() {
                self.load_my_listings();
            }
            if self.eco.collections.is_none() && !self.eco.collections_loading {
                self.eco.collections_loading = true;
                self.send_data(DataCmd::Collections { query: None });
            }
            if !self.eco.listings.contains_key(&None) && !self.eco.listings_loading {
                self.eco.listings_loading = true;
                self.send_data(DataCmd::Listings { collection: None });
            }
        }
        if features.trading {
            if self.eco.markets.is_empty() {
                self.send_data(DataCmd::Markets);
            }
            let mv = &mut self.eco.markets_view;
            if mv.pools.is_none() && !mv.pools_loading {
                mv.pools_loading = true;
                mv.pools_at = Some(Instant::now());
                self.send_data(DataCmd::MarketPools);
            }
        }
        // MAX on native QUAI cannot be honest without this, and the picker's route badges want
        // the pools, so both load before the user opens Trade rather than when they do.
        if self.eco.gas_price.is_none() {
            self.send_data(DataCmd::GasPrice);
        }
        if self.eco.lockups.is_none() {
            self.send_data(DataCmd::Lockups(owners));
        }
        // The launch zone brings its tokens' logos, which Markets uses for graduated and on-curve
        // tokens too: loaded now, both screens open with their pictures.
        self.load_launches(false);
    }

    /// The wallet's Quai addresses: from the dashboard once it has loaded, else from the wallet
    /// file (known before any network call).
    pub fn owner_addresses(&self) -> Vec<String> {
        if !self.dash.accounts.is_empty() {
            return self.dash.accounts.iter().map(|a| a.address.clone()).collect();
        }
        self.meta.as_ref().map(|m| m.quai_owner_addresses()).unwrap_or_default()
    }

    /// Fetch images a screen is likely to show before it is opened (loaded from the wallet's
    /// image cache when seen before). Queued behind what is on screen now.
    fn preload_images(&mut self, wants: impl IntoIterator<Item = (Option<String>, u32)>) {
        use wallet_core::media::{ICON, is_native_icon};
        let mut batch = Vec::new();
        for (url, edge) in wants {
            let Some(url) = url else { continue };
            let allowed =
                if edge > ICON && !is_native_icon(&url) { self.config.images } else { self.config.token_icons || is_native_icon(&url) };
            // IPFS gateways are slow and strictly paced: those load when shown, not ahead.
            let gateway = wallet_core::ipfs::is_gateway_url(&url);
            let key = (url, edge);
            if allowed && !gateway && !batch.contains(&key) && !self.eco.images.contains_key(&key) {
                self.eco.images.insert(key.clone(), ImageSlot::Loading);
                batch.push(key);
            }
        }
        if !batch.is_empty() {
            // Reversed: the image lane takes the newest want first, so the first listed loads first.
            batch.reverse();
            self.send_data(DataCmd::Images(batch));
        }
    }

    /// Fetch this wallet's own listings.
    pub fn load_my_listings(&mut self) {
        let sellers = self.owner_addresses();
        if !sellers.is_empty() {
            self.send_data(DataCmd::MyListings { sellers });
        }
    }

    /// This wallet's listing of an item, when the indexer has it.
    pub fn my_listing(&self, contract: &str, token_id: &str) -> Option<Listing> {
        match &self.eco.my_listings {
            Some(Ok(v)) => v.iter().find(|l| l.contract.eq_ignore_ascii_case(contract) && l.token_id == token_id).cloned(),
            _ => None,
        }
    }

    /// Keep the marketplace's own numbers current: floors and listing counts every 5 minutes,
    /// the sales history every 2. Both are one request for the whole market.
    pub fn load_nft_market(&mut self, force: bool) {
        use std::time::Duration;
        if force || self.eco.nft_stats_at.is_none_or(|t| t.elapsed() >= Duration::from_secs(300)) {
            self.eco.nft_stats_at = Some(Instant::now());
            self.send_data(DataCmd::CollectionStats);
        }
        if force || self.eco.nft_trades_at.is_none_or(|t| t.elapsed() >= Duration::from_secs(120)) {
            self.eco.nft_trades_at = Some(Instant::now());
            self.send_data(DataCmd::NftTrades);
        }
    }

    pub fn load_nfts(&mut self, refresh: bool) {
        let owners = self.owner_addresses();
        if owners.is_empty() || !self.config.features.nfts {
            return;
        }
        self.eco.nfts_loading = true;
        self.send_data(DataCmd::Nfts { owners, refresh });
    }

    fn default_receive_asset(&self) -> Option<SwapAsset> {
        let network = self.config.network(&self.network_id).ok()?;
        let usdt = network.ecosystem.usdt.as_ref().map(|u| u.address.to_lowercase());
        let pick = |address: &str, fallback: &str| {
            let market = self.eco.markets.iter().find(|m| m.address == address);
            SwapAsset::Token {
                address: address.to_string(),
                symbol: market.map(|m| m.symbol.clone()).unwrap_or_else(|| fallback.to_string()),
                decimals: if fallback == "USDT" { 6 } else { 18 },
            }
        };
        usdt.map(|u| pick(&u, "USDT")).or_else(|| network.wqi.as_ref().map(|w| pick(&w.to_lowercase(), "WQI")))
    }

    /// Handle a data worker event.
    pub fn on_data_event(&mut self, ev: DataEv) {
        self.dirty = true;
        match ev {
            DataEv::LpPositions { result, gauge, zone } => {
                let pv = &mut self.eco.pools_view;
                pv.loading = false;
                pv.loaded_at = Some(Instant::now());
                if let Some(g) = gauge {
                    pv.gauge = Some(*g);
                }
                if let Some(z) = zone {
                    pv.zone = Some(*z);
                }
                // A failed refresh keeps the last good list rather than blanking the screen.
                if result.is_ok() || pv.positions.as_ref().is_none_or(|p| p.is_err()) {
                    pv.positions = Some(result);
                }
                let len = pv.positions.as_ref().and_then(|r| r.as_ref().ok()).map_or(0, Vec::len);
                pv.selected = pv.selected.min(len.saturating_sub(1));
            }
            DataEv::PairCandles { pool, bucket, candles } => {
                if !candles.is_empty() {
                    // How long a chart took to draw from a standing start: measured from the
                    // moment the cursor settled on this pool, which is when it was asked for.
                    if !self.eco.markets_view.candles.contains_key(&(pool.clone(), bucket))
                        && let Some((settled, at)) = &self.eco.markets_view.selected_at
                        && *settled == pool
                    {
                        wallet_core::diag::timing("chart.cold", *at);
                    }
                    if self.eco.markets_view.candles.get(&(pool.clone(), bucket)) != Some(&candles) {
                        self.eco.markets_view.changed(&pool);
                        self.eco.markets_view.candles.insert((pool, bucket), candles);
                    }
                }
            }
            DataEv::Launches(result) => {
                self.eco.launches_at = Some(Instant::now());
                // Keep the last good list when a refresh fails.
                if result.is_ok() || !matches!(self.eco.launches, Some(Ok(_))) {
                    self.eco.launches = Some(result);
                }
            }
            DataEv::LaunchLogos(logos) => self.eco.launch_logos.extend(logos),
            DataEv::WalletQuai(totals) => self.wallet_quai.extend(totals),
            DataEv::Alerts { alerts, watchlist, fired, note } => {
                self.eco.alerts = alerts;
                let reorder = self.eco.watchlist != watchlist;
                // The pair under the cursor, read before the watchlist moves it.
                let holding = reorder.then(|| self.selected_pool().map(|p| p.address)).flatten();
                self.eco.watchlist = watchlist;
                self.eco.alerts_loaded = true;
                if reorder {
                    self.keep_cursor_on(holding);
                }
                if let Some(note) = note {
                    self.toast(note, false);
                }
                for (title, body) in fired {
                    self.toast(format!("◆ {title} · {body}"), false);
                    self.send(super::worker::Cmd::Refresh { full: false });
                }
            }
            DataEv::ChainStats(result) => {
                // Keep the last good figures when a refresh fails.
                if result.is_ok() || !matches!(self.eco.chain_stats, Some(Ok(_))) {
                    self.eco.chain_stats = Some(result.map(|b| *b));
                }
            }
            DataEv::CurveMarket { token, result } => {
                if result.is_ok() || !matches!(self.eco.curves.get(&token), Some(Ok(_))) {
                    self.eco.curves.insert(token, result.map(|b| *b));
                }
            }
            DataEv::TxCost { hash, result } => {
                self.eco.tx_costs.insert(hash, result);
            }
            DataEv::GasPrice(r) => {
                if let Ok(price) = r.and_then(|p| U256::from_str_radix(&p, 10).map_err(|e| e.to_string())) {
                    self.eco.gas_price = Some(price);
                }
            }
            DataEv::Portfolio(Ok(p)) => {
                let first = self.eco.portfolio.is_none();
                let has_nfts = p.nfts.items > 0;
                use wallet_core::media::{ICON, ICON_LARGE};
                let icons: Vec<(Option<String>, u32)> = p
                    .rows
                    .iter()
                    .flat_map(|r| {
                        let url = self.row_icon(r);
                        [(url.clone(), ICON), (url, ICON_LARGE)]
                    })
                    .collect();
                // Leave this wallet's summary for the cockpit, which shows every wallet at once.
                if let Some(m) = &self.meta {
                    wallet_core::cockpit::save_summary(&self.paths, &m.id, &p);
                    self.wallet_summaries
                        .insert(m.id.clone(), wallet_core::cockpit::load_summary(&self.paths, &m.id, &p.network).unwrap_or_default());
                }
                self.eco.portfolio = Some(*p);
                self.eco.portfolio_error = None;
                self.preload_images(icons);
                // Home shows a few NFT thumbnails when the wallet holds any and images are on.
                if has_nfts && self.screen == Screen::Home && self.config.images && self.eco.nfts.is_none() && !self.eco.nfts_loading {
                    self.load_nfts(false);
                }
                if first && !self.config.data_disclosure_shown && self.config.explorer_lookups && self.dash.network_id == "mainnet" {
                    self.config.data_disclosure_shown = true;
                    self.save_config();
                    self.toast(
                        "Portfolio data comes from explorer.qu.ai, which can see your addresses and IP. System › Data sources to turn off.",
                        false,
                    );
                }
            }
            DataEv::Portfolio(Err(e)) => self.eco.portfolio_error = Some(e),
            DataEv::Notice(text) => self.toast(text, true),
            DataEv::MarketPools(r) => {
                self.eco.markets_view.pools_loading = false;
                if r.is_ok() {
                    self.eco.markets_view.pools_at = Some(Instant::now());
                }
                // Keep showing the last good directory when a refresh fails.
                if r.is_ok() || self.eco.markets_view.pools.as_ref().is_none_or(|p| p.is_err()) {
                    let holding = self.selected_pool().map(|p| p.address);
                    self.eco.markets_view.pools = Some(r);
                    self.keep_cursor_on(holding);
                }
            }
            DataEv::PoolReserves(result) => {
                self.eco.markets_view.reserves_loading = false;
                if result.as_ref().is_ok_and(|fresh| !fresh.is_empty()) {
                    self.eco.markets_view.reserves_at = Some(Instant::now());
                }
                // Silent on failure: the directory's own numbers are still on screen, only a few
                // seconds older. A node that cannot answer must not paint an error over a working
                // market list.
                if let Ok(fresh) = result
                    && !fresh.is_empty()
                    && let Some(Ok((pools, _))) = self.eco.markets_view.pools.as_mut().map(|r| r.as_mut())
                {
                    let network = self.config.network(&self.network_id).ok();
                    let wquai = network.as_ref().and_then(|n| n.wquai.clone());
                    // The price feed, not the on-chain USDT pool: that pool holds about four
                    // thousand dollars, and pricing the whole exchange off its spot put every
                    // computed TVL 1.8% under the explorer's. The feed is what the explorer uses.
                    let usd = self.eco.portfolio.as_ref().and_then(|p| p.prices.as_ref()).and_then(|b| b.quai_usd);
                    wallet_core::markets::apply_reserves(pools, &fresh, wquai.as_deref(), usd);
                    self.dirty = true;
                }
            }
            DataEv::DexFlow(result) => {
                self.eco.markets_view.flow_loading = false;
                self.eco.markets_view.flow_at = Some(Instant::now());
                match result {
                    // The tape keeps what it has when a refresh fails; the column says so.
                    Ok(flow) => {
                        self.eco.markets_view.flow_error = None;
                        if !flow.is_empty() || self.eco.markets_view.flow.is_empty() {
                            self.eco.markets_view.flow = flow;
                        }
                    }
                    Err(e) => self.eco.markets_view.flow_error = Some(e),
                }
            }
            DataEv::Board { channel, result } => {
                if self.eco.board.loading.as_deref() == Some(channel.as_str()) {
                    self.eco.board.loading = None;
                }
                self.eco.board.at.insert(channel.clone(), Instant::now());
                // Reading a channel is what makes it read.
                if self.screen == Screen::Board && self.board_channel().as_deref() == Some(channel.as_str()) {
                    self.mark_board_seen(&channel);
                    self.eco.board.announced.remove(&channel);
                }
                // Keep the messages already read when a refresh fails.
                if result.is_ok() || !matches!(self.eco.board.posts.get(&channel), Some(Ok(_))) {
                    self.eco.board.posts.insert(channel, result);
                }
            }
            DataEv::BoardChannels(result) => {
                self.eco.board.known_loading = false;
                self.eco.board.known_at = Some(Instant::now());
                // Keep what was found when a scan fails; an empty board is not news.
                if let Ok(found) = result {
                    let first = self.eco.board.seen.is_empty();
                    self.eco.board.known = found;
                    self.announce_board(first);
                }
            }
            DataEv::PoolEvents { pool, coverage, result } => {
                if let Some(coverage) = coverage {
                    self.eco.markets_view.history_coverage.insert(pool.clone(), coverage);
                } else {
                    self.eco.markets_view.history_coverage.remove(&pool);
                }
                if self.eco.markets_view.events_loading.as_deref() == Some(pool.as_str()) {
                    self.eco.markets_view.events_loading = None;
                }
                if (result.is_ok() || !matches!(self.eco.markets_view.events.get(&pool), Some(Ok(_))))
                    && self.eco.markets_view.events.get(&pool) != Some(&result)
                {
                    self.eco.markets_view.changed(&pool);
                    self.eco.markets_view.events.insert(pool, result);
                }
            }
            DataEv::Image { url, edge, rendition, transient } => {
                let slot = match rendition {
                    Some(r) => {
                        ImageSlot::Ready(r, if self.motion().effects() { Instant::now() } else { Instant::now() - Duration::from_secs(1) })
                    }
                    None => ImageSlot::Failed(Instant::now() + if transient { IMAGE_RETRY_SOON } else { IMAGE_RETRY }),
                };
                self.eco.images.insert((url, edge), slot);
            }
            DataEv::QiRoutes { key, result } => {
                if key == self.eco.convert.requested_key {
                    self.eco.convert.routes_key = key;
                    self.eco.convert.routes = Some(result.map(|c| *c));
                }
            }
            DataEv::ProtocolQuote { key, card, result } => {
                if key == self.eco.convert.protocol_key.get() {
                    match result {
                        Ok(quote) if card && self.screen == Screen::Convert => self.on_event(super::worker::Ev::Quote(quote), (0, 0)),
                        Ok(quote) if !card => self.modal = Modal::Quote(quote),
                        Ok(_) => {}
                        Err(error) => self.toast(error, true),
                    }
                }
            }
            DataEv::Markets(m) => {
                self.preload_images(m.iter().take(40).map(|t| (t.icon_url.clone(), wallet_core::media::ICON)));
                self.eco.markets = m;
                if self.eco.swap.to.is_none() {
                    self.eco.swap.to = self.default_receive_asset();
                }
            }
            DataEv::SwapQuote { key, result } => {
                if key != 0 && key == self.eco.swap.requested_key && self.swap_input_key() == self.eco.swap.requested_input {
                    let approval_done = self.eco.swap.approving && matches!(&result, Ok(q) if !q.approval_needed);
                    self.eco.swap.quote = Some(result.map(|b| *b));
                    self.eco.swap.quote_key = key;
                    self.eco.swap.quoted_at = Some(Instant::now());
                    if approval_done && self.eco.flow.is_none() {
                        self.eco.swap.approving = false;
                    }
                }
            }
            DataEv::LiquidityQuote { key, result } => {
                if let Some(card) = self.eco.pools_view.add.as_mut()
                    && key == card.requested_key
                {
                    card.quote = Some(result.map(|b| *b));
                    card.quote_key = key;
                }
            }
            DataEv::Nfts(r) => {
                if let Ok(v) = &r {
                    self.preload_images(v.iter().map(|n| (n.item.image.clone(), wallet_core::media::THUMB)));
                }
                self.eco.nfts_loading = false;
                self.eco.nfts = Some(r);
            }
            DataEv::Collections { result } => {
                if let Ok(v) = &result {
                    self.preload_images(v.iter().take(30).map(|c| (c.preview.clone(), wallet_core::media::THUMB)));
                }
                self.eco.collections_loading = false;
                self.eco.collections = Some(result);
            }
            DataEv::CollectionItems { contract, result } => {
                self.eco.collection_items.insert(contract, result);
            }
            DataEv::CollectionStats { result } => match result {
                Ok(rows) => {
                    self.eco.nft_stats = rows.into_iter().map(|c| (c.address.clone(), c)).collect();
                    self.eco.nft_stats_error = None;
                }
                Err(e) => self.eco.nft_stats_error = Some(e),
            },
            DataEv::NftTrades { result } => {
                if let Ok(rows) = result {
                    self.eco.nft_trades = rows;
                }
            }
            DataEv::Listings { collection, result } => {
                // The indexer's images are usually raw IPFS files; the explorer's metadata points
                // at its resized media proxy, so look that up for the first rows.
                if let Ok(v) = &result {
                    for l in v.iter().take(20) {
                        self.eco.want_meta(&l.contract, &l.token_id);
                    }
                    self.flush_meta_wants();
                }
                if collection.is_none() {
                    self.eco.listings_loading = false;
                }
                self.eco.listings.insert(collection, result);
            }
            DataEv::MyListings(result) => self.eco.my_listings = Some(result),
            DataEv::Nft { contract, token_id, result } => {
                if let Ok(item) = &result {
                    self.preload_images([(item.image.clone(), wallet_core::media::THUMB)]);
                }
                self.eco.nft_meta.insert((contract.to_lowercase(), token_id), result.map(|b| *b));
            }
            DataEv::Ask { contract, token_id, result } => {
                self.eco.asks.insert((contract, token_id), result.map(|b| *b));
            }
            DataEv::TokenInfo { address, result } => {
                if let Ok((info, _)) = &result
                    && let Some(d) = info.decimals
                {
                    let card = &mut self.eco.swap;
                    for asset in [Some(&mut card.from), card.to.as_mut()].into_iter().flatten() {
                        if let SwapAsset::Token { address: a, decimals, .. } = asset
                            && *a == address
                            && *decimals == UNKNOWN_DECIMALS
                        {
                            *decimals = d;
                            card.edited = Some(Instant::now());
                        }
                    }
                }
                self.eco.token_info.insert(address, result);
            }
            DataEv::Lockups(r) => self.eco.lockups = Some(r),
            DataEv::Test(results) => {
                self.eco.testing = false;
                let failed = results.iter().filter(|(_, r, _)| r.is_err()).count();
                self.toast(
                    if failed == 0 { "all data sources answered".to_string() } else { format!("{failed} data source(s) did not answer") },
                    failed > 0,
                );
                self.eco.test = Some(results);
            }
        }
    }

    /// Send newly wanted images to the data worker (called after each frame).
    pub fn flush_image_wants(&mut self) {
        let wants: Vec<(String, u32)> = self.eco.wants.borrow_mut().drain(..).collect();
        let mut batch = Vec::new();
        for (url, edge) in wants {
            let key = (url.clone(), edge);
            let due = match self.eco.images.get(&key) {
                None => true,
                Some(ImageSlot::Failed(retry)) => Instant::now() >= *retry,
                Some(_) => false,
            };
            if due {
                self.eco.images.insert(key, ImageSlot::Loading);
                batch.push((url, edge));
            }
        }
        if !batch.is_empty() {
            self.send_data(DataCmd::Images(batch));
        }
        self.flush_meta_wants();
    }

    /// Request NFT metadata wanted since the last flush. Marked loading by inserting nothing:
    /// the data worker's single-flight keeps repeats from reaching the explorer.
    ///
    /// Every caller of `want_meta` is a marketplace listings row, so these are public: the same
    /// rows every wallet loads, cached once for the whole data directory.
    pub fn flush_meta_wants(&mut self) {
        let wants: Vec<(String, String)> = self.eco.meta_wants.borrow_mut().drain(..).collect();
        for (contract, token_id) in wants {
            if self.eco.meta_requested.insert((contract.clone(), token_id.clone())) {
                self.send_data(DataCmd::Nft { contract, token_id, public: true });
            }
        }
    }

    /// Ask for network statistics when the Network screen has none or they are five minutes old
    /// (the feed's own freshness window; asking sooner would only read the cache).
    pub fn tick_chain_stats(&mut self) {
        if self.eco.chain_stats_at.is_none_or(|t| t.elapsed() >= Duration::from_secs(300)) {
            self.eco.chain_stats_at = Some(Instant::now());
            self.send_data(DataCmd::ChainStats);
        }
    }

    /// Periodic ecosystem work: debounced swap quotes and re-quotes while waiting on approval.
    pub fn tick_eco(&mut self) {
        self.advance_flow();
        self.poll_handoff();
        self.tick_chat();
        self.tick_alerts();
        self.tick_tx_cost();
        if self.screen == Screen::Markets && !self.locked {
            self.tick_markets();
        }
        if self.screen == Screen::Board && !self.locked {
            self.tick_board();
        }
        self.tick_board_watch();
        if self.screen == Screen::Convert && !self.locked {
            self.tick_qi_routes();
        }
        if self.screen == Screen::Swap && !self.locked {
            self.tick_swap();
        }
        // Side by side, each half keeps its own data coming.
        if self.trader && !self.locked {
            match self.screen {
                Screen::Markets => self.tick_swap(),
                Screen::Swap => self.tick_markets(),
                _ => {}
            }
        }
        if self.screen == Screen::Network && !self.locked {
            self.tick_chain_stats();
        }
        if self.screen == Screen::Launches && !self.locked {
            self.load_launches(false);
            self.tick_curve();
        }
        if self.screen == Screen::Pools && !self.locked {
            self.tick_pools();
            self.tick_add_card();
        }
        if self.screen != Screen::Swap || self.locked {
            return;
        }
        let card = &self.eco.swap;
        let Some(to) = card.to.clone() else { return };
        if card.from.decimals() == UNKNOWN_DECIMALS || to.decimals() == UNKNOWN_DECIMALS {
            return;
        }
        let decimals = card.from.decimals();
        let Ok(atoms) = amount::parse_amount(&card.amount, decimals) else { return };
        if atoms.is_zero() {
            return;
        }
        let Some(input) = self.swap_input_key() else { return };
        let debounced = card.edited.is_none_or(|t| t.elapsed() > Duration::from_millis(450));
        let refresh = card.quoted_at.is_some_and(|t| t.elapsed() > Duration::from_secs(if card.approving { 6 } else { 20 }));
        if debounced && (card.requested_key == 0 || Some(input) != card.requested_input || refresh) {
            let owner = self.dash.accounts.first().map(|a| a.address.clone());
            let from = card.from.clone();
            let slippage = card.slippage_bps;
            let key = self.eco.swap.request_sequence.wrapping_add(1).max(1);
            self.eco.swap.request_sequence = key;
            self.eco.swap.requested_key = key;
            self.eco.swap.requested_input = Some(input);
            self.eco.swap.quoted_at = Some(Instant::now());
            self.send_data(DataCmd::SwapQuote { key, from, to, amount: atoms.to_string(), slippage, owner });
        }
    }

    /// Load LP positions when Pools opens, and refresh them slowly afterwards. A position only
    /// moves when the user acts or the pool's reserves shift, so this is not a hot poll.
    fn tick_pools(&mut self) {
        let pv = &self.eco.pools_view;
        let stale = pv.loaded_at.is_none_or(|t| t.elapsed() > Duration::from_secs(45));
        if pv.loading || !stale {
            return;
        }
        let Some(Ok((pools, _))) = self.eco.markets_view.pools.as_ref().map(|r| r.as_ref()) else {
            // Pools drive position discovery; `preload` has already asked for them.
            return;
        };
        // Both exchanges that hold real pairs. Filtering to the main one hid every launch-AMM pool
        // from position discovery, so a graduated token's LP could be neither seen nor staked even
        // when a launch-zone gauge was paying rewards on it — CHEEZ/QUAI being the case that found
        // this. A curve holds no LP at all, so it stays out.
        let pools: Vec<_> = pools
            .iter()
            .filter(|p| {
                matches!(
                    p.venue,
                    wallet_core::markets::Venue::Main
                        | wallet_core::markets::Venue::LaunchAmm
                        | wallet_core::markets::Venue::Legacy
                        | wallet_core::markets::Venue::HartiiAmm
                )
            })
            .cloned()
            .collect();
        let owners = self.owner_addresses();
        if owners.is_empty() {
            return;
        }
        self.eco.pools_view.loading = true;
        self.send_data(DataCmd::LpPositions { owners, pools });
    }

    /// Open the deposit card for a pool, sized by whichever side the user types.
    fn open_add_card(&mut self, pair: String, name: String, tokens: (wallet_core::markets::PoolToken, wallet_core::markets::PoolToken)) {
        self.eco.pools_view.add = Some(AddCard {
            pair,
            name,
            token0: tokens.0,
            token1: tokens.1,
            side1: false,
            amount: String::new(),
            slippage_bps: self.config.swap_slippage_bps,
            account: self.dash.accounts.first().map(|a| a.address.clone()),
            field: 1,
            quote: None,
            quote_key: 0,
            requested_key: 0,
            edited: None,
        });
    }

    /// Price the open deposit card once typing settles, so the other side keeps up without a
    /// request per keystroke.
    fn tick_add_card(&mut self) {
        let Some(card) = self.eco.pools_view.add.as_ref() else { return };
        let Ok(atoms) = amount::parse_amount(&card.amount, card.typed().decimals) else { return };
        if atoms.is_zero() {
            return;
        }
        let key = hash_key(&[&card.pair, &card.typed().symbol, &atoms.to_string(), &card.slippage_bps.to_string()]);
        let debounced = card.edited.is_none_or(|t| t.elapsed() > Duration::from_millis(350));
        if !debounced || key == card.requested_key {
            return;
        }
        let (pair, token, slippage) = (card.pair.clone(), card.typed().symbol.clone(), card.slippage_bps);
        let (amount, owner) = (card.amount.clone(), card.account.clone());
        if let Some(card) = self.eco.pools_view.add.as_mut() {
            card.requested_key = key;
        }
        self.send_data(DataCmd::LiquidityQuote { key, pair, amount, token, slippage, owner });
    }

    /// Deposit-card keys. Either amount row can be the one you type: the other is the pool's
    /// answer, so the pair stays balanced whichever token you happen to have.
    fn add_card_key(&mut self, key: KeyEvent) -> bool {
        let accounts: Vec<String> = self.dash.accounts.iter().map(|a| a.address.clone()).collect();
        let Some(card) = self.eco.pools_view.add.as_mut() else { return false };
        // Fields: 0 account, 1 token0 amount, 2 token1 amount, 3 slippage.
        match key.code {
            KeyCode::Esc => {
                self.eco.pools_view.add = None;
                return true;
            }
            KeyCode::Tab | KeyCode::Down => card.field = (card.field + 1) % 4,
            KeyCode::BackTab | KeyCode::Up => card.field = (card.field + 3) % 4,
            KeyCode::Left | KeyCode::Right if card.field == 0 && !accounts.is_empty() => {
                let at = accounts.iter().position(|a| Some(a) == card.account.as_ref()).unwrap_or(0);
                let step = if key.code == KeyCode::Left { accounts.len() - 1 } else { 1 };
                card.account = Some(accounts[(at + step) % accounts.len()].clone());
            }
            KeyCode::Char('m') if card.field == 1 || card.field == 2 => {
                let side1 = card.field == 2;
                self.fill_add_max(side1);
                return true;
            }
            KeyCode::Enter => {
                self.submit_add_card();
                return true;
            }
            KeyCode::Backspace if card.field == 1 || card.field == 2 => {
                // Typing into a row makes it the side that is typed; the other becomes the
                // pool's answer, so a half-edited pair is never sent anywhere.
                card.side1 = card.field == 2;
                card.amount.pop();
                card.edited = Some(Instant::now());
                card.quote = None;
            }
            KeyCode::Backspace if card.field == 3 => {
                let mut text = card.slippage_bps.to_string();
                text.pop();
                card.slippage_bps = text.parse().unwrap_or(0);
            }
            KeyCode::Char(c) if (card.field == 1 || card.field == 2) && (c.is_ascii_digit() || c == '.') => {
                if card.side1 != (card.field == 2) {
                    card.side1 = card.field == 2;
                    card.amount.clear();
                    card.quote = None;
                }
                let decimals = card.typed().decimals;
                let mut text = std::mem::take(&mut card.amount);
                if digits_input(&mut text, &key, decimals) {
                    card.amount = text;
                    card.edited = Some(Instant::now());
                } else {
                    card.amount = text;
                }
            }
            KeyCode::Char(c) if card.field == 3 && c.is_ascii_digit() => {
                let text = format!("{}{c}", card.slippage_bps);
                if let Ok(bps) = text.parse::<u16>().map(|b| b.min(10_000)) {
                    card.slippage_bps = bps;
                }
            }
            _ => return false,
        }
        true
    }

    /// `m` on a deposit row: everything of that token the wallet can actually spend.
    fn fill_add_max(&mut self, side1: bool) {
        use wallet_core::spendable::token_max;
        let Some(card) = self.eco.pools_view.add.as_ref() else { return };
        let token = if side1 { card.token1.clone() } else { card.token0.clone() };
        let asset = SwapAsset::Token { address: token.address.clone(), symbol: token.symbol.clone(), decimals: token.decimals };
        let Some((balance, decimals)) = self.exact_balance(&asset) else {
            self.toast(format!("no exact {} balance yet — it is still loading", token.symbol), true);
            return;
        };
        if balance.is_zero() {
            self.toast(format!("no {} to deposit", token.symbol), true);
            return;
        }
        let m = token_max(balance, decimals);
        let (text, note) = (m.text(), m.note());
        if let Some(card) = self.eco.pools_view.add.as_mut() {
            card.side1 = side1;
            card.field = if side1 { 2 } else { 1 };
            card.amount = text;
            card.edited = Some(Instant::now());
            card.quote = None;
        }
        if let Some(note) = note {
            self.toast(note, false);
        }
    }

    /// Send the composed deposit to the review, the same three-step path the CLI takes.
    fn submit_add_card(&mut self) {
        let Some(card) = self.eco.pools_view.add.as_ref() else { return };
        if amount::parse_amount(&card.amount, card.typed().decimals).is_ok_and(|a| a.is_zero()) || card.amount.is_empty() {
            self.toast("enter an amount for one of the two sides", true);
            return;
        }
        if let Some(Err(e)) = card.quote.as_ref() {
            self.toast(wallet_core::session::short_code(e), true);
            return;
        }
        // Three reviews in a row — one exact approval per side, then the deposit — so it goes
        // through the sequence driver: each step is asked for again once the last one confirms.
        let flow = FlowKind::Steps {
            prepare: Box::new(Prepare::AddLiquidityNext {
                account: card.account.clone(),
                pair: card.pair.clone(),
                amount: card.amount.clone(),
                token: Some(card.typed().symbol.clone()),
                slippage: card.slippage_bps,
                deadline: self.config.swap_deadline_minutes,
            }),
            label: format!("add liquidity to {}", card.name),
        };
        self.eco.pools_view.add = None;
        self.start_flow(flow);
    }

    /// Trade › Pools keys: move between positions, then act on the focused one.
    fn pools_key(&mut self, key: KeyEvent) -> bool {
        // A deposit being composed takes the keys: it is a form, not a list.
        if self.eco.pools_view.add.is_some() {
            return self.add_card_key(key);
        }
        // Pane 0 is what you hold; pane 1 is every pool, which is where a new position starts.
        let len = if self.pane == 0 { self.position_rows().len() } else { self.directory_rows().len() };
        let cursor = if self.pane == 0 { &mut self.eco.pools_view.selected } else { &mut self.eco.pools_view.pool_selected };
        match key.code {
            KeyCode::Char('j') | KeyCode::Down if len > 0 => *cursor = (*cursor + 1).min(len - 1),
            KeyCode::Char('k') | KeyCode::Up => *cursor = cursor.saturating_sub(1),
            KeyCode::Char('R') => {
                self.eco.pools_view.loaded_at = None;
                self.tick_pools();
            }
            KeyCode::Char('a') => self.pool_action('a'),
            KeyCode::Char('r') => self.pool_action('r'),
            KeyCode::Char('s') => self.pool_action('s'),
            KeyCode::Char('u') => self.pool_action('u'),
            KeyCode::Char('h') => self.pool_action('h'),
            KeyCode::Char('e') => self.pool_action('e'),
            KeyCode::Char('i') => self.pool_action('i'),
            _ => return false,
        }
        true
    }

    /// Act on the focused position. Every path ends in a review, so nothing here moves value.
    fn pool_action(&mut self, action: char) {
        if !self.can_sign() {
            self.toast("this wallet is watch-only", true);
            return;
        }
        let Some(focus) = self.focused_pool() else {
            self.toast("no pool selected", true);
            return;
        };
        let (pair, name, tokens) = (focus.pair.clone(), focus.name.clone(), focus.tokens.clone());
        // Adding liquidity is the one action that does not need an existing position — it is how
        // a position starts. Everything else acts on something you already hold.
        if action == 'a' {
            self.open_add_card(pair, name, tokens);
            return;
        }
        let Some(position) = focus.position else {
            self.toast(format!("you have no liquidity in {name} yet — press a to add some"), true);
            return;
        };
        let account = self.dash.accounts.first().map(|a| a.address.clone());
        let lp = |v: U256| amount::format_amount(v, 18);
        let staked = position.pid.is_some();
        let gauge = position.gauge_address.clone();
        match action {
            'r' => self.open_form(FormKind::RemoveLiquidity { pair, name }),
            's' if !staked => self.toast(GAUGE_ABSENT, true),
            's' if position.lp_wallet.is_zero() => self.toast("no unstaked LP in the wallet", true),
            's' => {
                let amount = lp(position.lp_wallet);
                self.open_form(FormKind::StakePosition { pair, gauge, name, amount, stake: true });
            }
            'u' if position.lp_staked.is_zero() => self.toast("nothing staked in this pool", true),
            'u' => {
                let amount = lp(position.lp_staked);
                self.open_form(FormKind::StakePosition { pair, gauge, name, amount, stake: false });
            }
            'i' if !staked => self.toast(GAUGE_ABSENT, true),
            'i' if position.gauge == Some(wallet_core::gauge::GaugeKind::Zone) => self.toast(ZONE_NOT_FUNDABLE, true),
            'i' => self.open_form(FormKind::Incentivize { pair, name }),
            'h' | 'e' if !staked => self.toast(GAUGE_ABSENT, true),
            'h' => self.send(Cmd::Prepare(Prepare::Harvest { account, pair, gauge, exit: false })),
            'e' => self.send(Cmd::Prepare(Prepare::Harvest { account, pair, gauge, exit: true })),
            _ => {}
        }
    }

    /// Read what the observed transaction on screen carried and cost, once per hash. Only rows the
    /// wallet did not send need it — its own operations recorded their value and fee.
    fn tick_tx_cost(&mut self) {
        let key = match self.detail.last() {
            Some(Detail::Activity(k)) => Some(k.clone()),
            Some(_) => None,
            None if self.screen == Screen::Activity => match self.activity_rows().get(self.selected) {
                Some((_, false, i)) => self.dash.activity.get(*i).map(|a| format!("act:{}", a.key)),
                _ => None,
            },
            None => None,
        };
        let Some(k) = key.as_deref().and_then(|k| k.strip_prefix("act:")) else { return };
        let Some(hash) = self.dash.activity.iter().find(|a| a.key == k && a.asset != "QI").and_then(|a| a.tx_hash.clone()) else {
            return;
        };
        if self.eco.tx_costs_asked.insert(hash.clone()) {
            self.send_data(DataCmd::TxCost(hash));
        }
    }

    /// Ask for the launch zone when it has never loaded or is a minute old (the indexer's own
    /// cache is a minute too), or now with `force`.
    /// Ask the wallet worker for PnL: on opening the screen when the last answer is older than
    /// [`PNL_TTL`], and on `R`. Trades move it, so a stale answer is re-read rather than kept.
    /// Nothing is marked loading without a worker to answer: the request would go nowhere.
    pub fn load_pnl(&mut self, force: bool) {
        if self.worker.is_none() || self.eco.pnl_loading || !(force || self.eco.pnl_at.is_none_or(|t| t.elapsed() >= PNL_TTL)) {
            return;
        }
        self.eco.pnl_at = Some(Instant::now());
        self.eco.pnl_loading = true;
        self.send(Cmd::Pnl);
    }

    /// Positions the PnL screen lists, in its order.
    pub fn pnl_positions(&self) -> &[wallet_core::pnl::Position] {
        match &self.eco.pnl {
            Some(Ok(p)) => &p.positions,
            _ => &[],
        }
    }

    fn pnl_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('R') => {
                self.load_pnl(true);
                self.toast("re-reading trades and prices", false);
                true
            }
            // Trade the focused token against QUAI on the swap card.
            KeyCode::Char('t') => {
                let Some(p) = self.pnl_positions().get(self.selected).cloned() else { return true };
                self.eco.swap.from = SwapAsset::Quai;
                self.eco.swap.to = Some(SwapAsset::Token { address: p.token.clone(), symbol: p.symbol.clone(), decimals: p.decimals });
                self.eco.swap.amount.clear();
                self.eco.swap.quote = None;
                self.eco.swap.field = 1;
                self.switch(Screen::Swap);
                self.toast(format!("buy {} with QUAI · f flips to sell", p.symbol), false);
                true
            }
            _ => false,
        }
    }

    pub fn load_launches(&mut self, force: bool) {
        let due = Duration::from_secs(wallet_core::launches::LAUNCH_TTL);
        if force || self.eco.launches_at.is_none_or(|t| t.elapsed() >= due) {
            self.eco.launches_at = Some(Instant::now());
            self.send_data(DataCmd::Launches);
        }
    }

    /// Keep the focused launch's curve fresh (every 15 s): a curve moves with every trade.
    fn tick_curve(&mut self) {
        let Some(l) = self.launch_rows().get(self.selected).cloned() else { return };
        let Some(curve) = l.curve.clone().filter(|_| {
            l.phase == wallet_core::launches::Phase::Bonding || l.venue_kind == Some(wallet_core::capabilities::Family::HartiiCurve)
        }) else {
            return;
        };
        if self.eco.curves_at.get(&l.token).is_none_or(|t| t.elapsed() >= Duration::from_secs(15)) {
            self.eco.curves_at.insert(l.token.clone(), Instant::now());
            let owners = self.dash.accounts.first().map(|a| vec![a.address.clone()]).unwrap_or_default();
            self.send_data(DataCmd::CurveMarket { token: l.token, curve, owners });
        }
    }

    /// The focused launch's curve, when it has been read.
    pub fn focused_curve(&self) -> Option<&wallet_core::curve::CurveMarket> {
        let token = self.launch_rows().get(self.selected)?.token.clone();
        self.eco.curves.get(&token).and_then(|r| r.as_ref().ok())
    }

    /// The launches shown, newest first.
    pub fn launch_rows(&self) -> Vec<wallet_core::launches::Launch> {
        use wallet_core::launches::Phase;
        let listed: &[wallet_core::launches::Launch] = match &self.eco.launches {
            Some(Ok(list)) => list,
            _ => &[],
        };
        let mut rows: Vec<wallet_core::launches::Launch> = listed
            .iter()
            .filter(|l| match l.phase {
                // A launch that has left its curve and can be found on an exchange is a market,
                // not a launch: Markets carries it with depth, a chart and the tape, and its buy
                // key here only opens the swap card anyway. One that has left its curve and cannot
                // be found stays — it has nowhere else to appear, and `c` is the only way left to
                // claim credit out of a curve that has already graduated.
                Phase::Pooled | Phase::Graduated => !self.market_lists_token(&l.token),
                Phase::Bonding | Phase::Other => true,
            })
            .cloned()
            .collect();
        // Live curves first, nearest graduation at the top: the stage bar is what this screen is
        // for, and a curve at 90% is the row worth looking at.
        //
        // Phase leads the sort rather than the stage alone, because a token that has finished its
        // curve reads as 100% and would otherwise outrank every curve still raising — the screen
        // would open on a launch that is over. Anything past its curve that survived the filter
        // above (nowhere else to appear) sits below the live ones, and a launch whose stage could
        // not be read sits below those again rather than among them reading as 0%.
        let rank = |l: &wallet_core::launches::Launch| u8::from(l.phase != Phase::Bonding);
        rows.sort_by(|a, b| {
            rank(a)
                .cmp(&rank(b))
                .then(a.progress_bps.is_none().cmp(&b.progress_bps.is_none()))
                .then(b.progress_bps.unwrap_or(0).cmp(&a.progress_bps.unwrap_or(0)))
                .then(a.symbol.cmp(&b.symbol))
        });
        rows
    }

    /// Whether the market directory already carries a pool holding this token, on any exchange.
    ///
    /// False while the directory is still loading, so a row is never hidden on the strength of
    /// data the wallet does not have yet: the list fills in and then settles, rather than starting
    /// short and growing.
    fn market_lists_token(&self, token: &str) -> bool {
        let Some(Ok((pools, _))) = self.eco.markets_view.pools.as_ref().map(|r| r.as_ref()) else { return false };
        pools
            .iter()
            .filter(|p| p.venue.routable())
            .any(|p| p.token0.address.eq_ignore_ascii_case(token) || p.token1.address.eq_ignore_ascii_case(token))
    }

    fn launches_key(&mut self, key: KeyEvent) -> bool {
        use wallet_core::launches::Phase;
        match key.code {
            KeyCode::Char('R') => {
                self.load_launches(true);
                self.toast("reloading launches", false);
                true
            }
            KeyCode::Char(c @ ('b' | 'S' | 'c')) => {
                let Some(l) = self.launch_rows().get(self.selected).cloned() else { return true };
                let Some(curve) = l.curve.clone() else {
                    self.toast(format!("{} has no bonding curve", l.symbol), true);
                    return true;
                };
                if !self.can_sign() {
                    self.toast("this wallet is watch-only", true);
                    return true;
                }
                let (token, symbol) = (l.token.clone(), l.symbol.clone());
                match c {
                    'c' if l.venue_kind == Some(wallet_core::capabilities::Family::HartiiCurve) => {
                        self.toast("Hartii pays QUAI directly; no separate claim is needed", false);
                    }
                    'c' => {
                        let account = self.dash.accounts.first().map(|a| a.address.clone());
                        self.send(Cmd::Prepare(Prepare::CurveClaim { account, token, symbol, curve }));
                    }
                    _ if l.phase != wallet_core::launches::Phase::Bonding
                        && l.venue_kind != Some(wallet_core::capabilities::Family::HartiiCurve) =>
                    {
                        self.toast(format!("{symbol} has left its curve ({}); c still claims any credit", l.phase.text()), true);
                    }
                    'b' => self.open_form(FormKind::CurveBuy { token, symbol, curve }),
                    _ => {
                        let held = self.focused_curve().map(|m| amount::format_amount(m.held, m.token_decimals)).unwrap_or_default();
                        self.open_form(FormKind::CurveSell { token, symbol, curve, held });
                    }
                }
                true
            }
            KeyCode::Char('t') | KeyCode::Enter => {
                let Some(l) = self.launch_rows().get(self.selected).cloned() else { return true };
                if l.venue_kind == Some(wallet_core::capabilities::Family::HartiiCurve) {
                    if let Some(curve) = l.curve.clone() {
                        self.open_form(FormKind::CurveBuy { token: l.token, symbol: l.symbol, curve });
                    }
                    return true;
                }
                match l.phase {
                    // Pooled on the main AMM or graduated into the launch AMM: the swap card's
                    // router trades both.
                    Phase::Pooled | Phase::Graduated => {
                        self.eco.swap.from = SwapAsset::Quai;
                        self.eco.swap.to = Some(SwapAsset::Token { address: l.token.clone(), symbol: l.symbol.clone(), decimals: 18 });
                        self.eco.swap.amount.clear();
                        self.eco.swap.quote = None;
                        self.eco.swap.field = 1;
                        self.switch(Screen::Swap);
                        self.toast(format!("buy {} with QUAI · f flips to sell", l.symbol), false);
                    }
                    Phase::Bonding => {
                        let curve = l.curve.clone().unwrap_or_default();
                        self.open_form(FormKind::CurveBuy { token: l.token.clone(), symbol: l.symbol.clone(), curve });
                    }
                    Phase::Other => self.toast(format!("{} has no market the wallet can trade", l.symbol), true),
                }
                true
            }
            _ => false,
        }
    }

    /// Positions this wallet holds, newest read.
    pub fn position_rows(&self) -> &[wallet_core::liquidity::LpPosition] {
        match self.eco.pools_view.positions.as_ref() {
            Some(Ok(list)) => list,
            _ => &[],
        }
    }

    /// Every main-exchange pool, deepest first — the directory a new position is opened from.
    /// Liquidity is added through the main router, so the launch AMM's pairs and the curves the
    /// Markets view also lists are not offered here.
    pub fn directory_rows(&self) -> Vec<wallet_core::markets::Pool> {
        match self.eco.markets_view.pools.as_ref().map(|r| r.as_ref()) {
            // Both exchanges that hold LP. A launch-AMM pair is a real pool with real reserves and,
            // often, a launch-zone gauge paying rewards on it; leaving it out of the directory is
            // what made CHEEZ/QUAI impossible to stake from this screen. A curve has no LP token.
            Some(Ok((pools, _))) => pools
                .iter()
                .filter(|p| {
                    matches!(
                        p.venue,
                        wallet_core::markets::Venue::Main
                            | wallet_core::markets::Venue::LaunchAmm
                            | wallet_core::markets::Venue::Legacy
                            | wallet_core::markets::Venue::HartiiAmm
                    )
                })
                .cloned()
                .collect(),
            _ => Vec::new(),
        }
    }

    /// What the Pools screen is acting on: a pair, and the position in it when there is one.
    pub fn focused_pool(&self) -> Option<PoolFocus> {
        if self.pane == 0 {
            let p = self.position_rows().get(self.eco.pools_view.selected)?;
            return Some(PoolFocus {
                pair: p.pair.clone(),
                name: p.name(),
                tokens: (p.token0.clone(), p.token1.clone()),
                position: Some(p.clone()),
            });
        }
        let pool = self.directory_rows().into_iter().nth(self.eco.pools_view.pool_selected)?;
        Some(PoolFocus {
            pair: pool.address.clone(),
            name: format!("{}/{}", pool.token0.symbol, pool.token1.symbol),
            tokens: (pool.token0.clone(), pool.token1.clone()),
            // A pool in the directory may also be one we hold; carry that so the actions work.
            position: self.position_rows().iter().find(|p| p.pair.eq_ignore_ascii_case(&pool.address)).cloned(),
        })
    }

    /// The gauge pool behind a pair, when it has one.
    pub fn gauge_pool_for(&self, pair: &str) -> Option<&wallet_core::gauge::GaugePool> {
        self.eco.pools_view.gauge.as_ref()?.pool_for(pair)
    }

    /// The launch-zone campaign on a pair, when one exists.
    pub fn zone_pool_for(&self, pair: &str) -> Option<&wallet_core::zone::ZonePool> {
        self.eco.pools_view.zone.as_ref()?.pool_for(pair)
    }

    /// A launch-zone campaign's APR, priced the same way the core gauge's is.
    pub fn zone_apr(&self, pool: &wallet_core::zone::ZonePool, tvl_usd: Option<f64>) -> Option<f64> {
        pool.apr(wallet_core::registry::now(), &|t| self.token_usd(t), tvl_usd)
    }

    /// APR for a pair's gauge pool, priced from the portfolio's own token prices.
    ///
    /// The denominator is everyone's stake, not ours: APR is the pool's, not this wallet's.
    pub fn pool_apr(&self, position: &wallet_core::liquidity::LpPosition) -> Option<f64> {
        let tvl = self.pool_tvl(&position.pair);
        match self.gauge_pool_for(&position.pair) {
            Some(gauge) => gauge.apr_from_tvl(wallet_core::registry::now(), &|t| self.token_usd(t), tvl),
            None => self.zone_apr(self.zone_pool_for(&position.pair)?, tvl),
        }
    }

    /// A pool's TVL from the markets list.
    pub fn pool_tvl(&self, pair: &str) -> Option<f64> {
        let Some(Ok((pools, _))) = self.eco.markets_view.pools.as_ref().map(|r| r.as_ref()) else { return None };
        pools.iter().find(|p| p.address.eq_ignore_ascii_case(pair)).and_then(|p| p.tvl_usd)
    }

    /// Ask for both QUAI ⇄ Qi markets once the amount stops changing.
    fn tick_qi_routes(&mut self) {
        use wallet_core::qi_market::Direction;
        let card = &self.eco.convert;
        let direction = if card.qi_to_quai { Direction::QiToQuai } else { Direction::QuaiToQi };
        let Ok(atoms) = wallet_core::amount::parse_amount(&card.amount, direction.pay_decimals()) else { return };
        if atoms.is_zero() {
            return;
        }
        let slippage = self.swap_slippage();
        let key = hash_key(&[direction.as_str(), &atoms.to_string(), &slippage.to_string()]);
        let debounced = card.edited.is_none_or(|t| t.elapsed() > Duration::from_millis(450));
        // Rates move with the pools and the block's conversion flow.
        let stale = card.routes_key == key
            && card.routes.is_some()
            && self.eco.convert_quoted_at().is_some_and(|t| t.elapsed() > Duration::from_secs(20));
        // The protocol route's own quote is a separate read from the two-market comparison, and it
        // is the one that carries what the discount costs at this instant, the batch scenarios and
        // the suggested tolerance. Without asking for it here the panel beside the card stays empty
        // until the user presses Enter, so the estimate they most need is the one they never see.
        let want_quote = (!card.market).then(|| (card.qi_to_quai, card.amount.clone()));
        let quote_wanted = want_quote.as_ref().is_some_and(|w| card.quoted_for.as_ref() != Some(w)) || stale;
        if debounced && (key != card.requested_key || stale) {
            let owner = self.dash.accounts.first().map(|a| a.address.clone());
            self.eco.convert.requested_key = key;
            self.eco.convert.quoted_at = Some(Instant::now());
            self.send_data(DataCmd::QiRoutes { key, direction, amount: atoms.to_string(), owner, slippage });
        }
        if debounced
            && quote_wanted
            && let Some((qi_to_quai, amount)) = want_quote
        {
            self.eco.convert.quoted_for = Some((qi_to_quai, amount.clone()));
            self.eco.convert.quote = None;
            self.send(Cmd::Quote { direction: direction.as_str().into(), amount });
        }
    }

    /// The slippage the market route quotes with (the swap card's setting).
    pub fn swap_slippage(&self) -> u16 {
        self.eco.swap.slippage_bps
    }

    pub(crate) fn swap_input_key(&self) -> Option<u64> {
        let card = &self.eco.swap;
        let to = card.to.as_ref()?;
        let atoms = amount::parse_amount(&card.amount, card.from.decimals()).ok()?;
        Some(hash_key(&[
            &format!("{:?}", card.from),
            &format!("{to:?}"),
            &atoms.to_string(),
            &card.slippage_bps.to_string(),
            &card.deadline_minutes.to_string(),
            &self.network_id,
            &self.dash.accounts.first().map(|a| a.address.clone()).unwrap_or_default(),
        ]))
    }

    /// Current means matching normalized intent, owner/network, latest request and age.
    pub fn swap_quote_current(&self) -> bool {
        self.eco.swap.quote_key != 0
            && self.eco.swap.quote_key == self.eco.swap.requested_key
            && self.swap_input_key().is_some_and(|input| Some(input) == self.eco.swap.requested_input)
            && self.eco.swap.quoted_at.is_some_and(|at| at.elapsed() < Duration::from_secs(20))
    }

    /// `t` from anywhere: open Swap with the focused token as the pay side.
    pub fn open_trade(&mut self) {
        let focused = match (self.detail.last(), self.screen) {
            (Some(Detail::Asset(id)), _) => Some(id.clone()),
            (None, Screen::Home) if self.pane == 0 => {
                self.eco.portfolio.as_ref().and_then(|p| p.rows.get(self.selected)).map(|r| r.key.id())
            }
            _ => None,
        };
        if let Some(id) = focused
            && let Some(asset) = self.swap_asset_for(&id)
        {
            if self.eco.swap.to.as_ref() == Some(&asset) {
                self.eco.swap.to = Some(self.eco.swap.from.clone());
            }
            self.eco.swap.from = asset;
            self.eco.swap.quote = None;
        }
        self.switch(Screen::Swap);
        self.eco.swap.field = 1;
    }

    fn swap_asset_for(&self, id: &str) -> Option<SwapAsset> {
        match id {
            "quai" => Some(SwapAsset::Quai),
            "qi" => None,
            address => {
                let row = self.eco.portfolio.as_ref().and_then(|p| p.rows.iter().find(|r| r.key.id() == address));
                Some(SwapAsset::Token {
                    address: address.to_string(),
                    symbol: row.map(|r| r.symbol.clone()).unwrap_or_else(|| wallet_core::session::short_address(address)),
                    decimals: row.map(|r| r.decimals).unwrap_or(18),
                })
            }
        }
    }

    /// Open the swap card on this asset: `buy` pays with the counter asset to get it, otherwise
    /// it sells the asset for the counter asset. The counter is QUAI, or WQI when the asset is
    /// QUAI itself, so both sides are never the same thing.
    fn quick_swap(&mut self, id: &str, buy: bool) -> bool {
        let Some(asset) = self.swap_asset_for(id) else {
            self.toast("Qi is not traded on Quainance — use Convert", true);
            return true;
        };
        let counter = if id == "quai" {
            let wqi = self.config.network(&self.network_id).ok().and_then(|n| n.wqi.clone());
            match wqi {
                Some(address) => SwapAsset::Token { address: address.to_lowercase(), symbol: "WQI".into(), decimals: 18 },
                None => {
                    self.toast("no WQI on this network", true);
                    return true;
                }
            }
        } else {
            SwapAsset::Quai
        };
        let (from, to) = if buy { (counter, asset) } else { (asset, counter) };
        self.detail.clear();
        self.switch(Screen::Swap);
        let card = &mut self.eco.swap;
        card.from = from;
        card.to = Some(to);
        card.amount.clear();
        card.quote = None;
        card.preset = None;
        card.approving = false;
        card.field = 1;
        card.edited = Some(Instant::now());
        true
    }

    /// Candidate tokens for the picker: holdings first, then market tokens.
    /// The pool graph behind the picker's route badges. Empty until pools load, which
    /// `RouteState::Unknown` handles rather than filtering everything away.
    pub fn route_graph(&self) -> RouteGraph {
        let Some(Ok((pools, _))) = self.eco.markets_view.pools.as_ref().map(|r| r.as_ref()) else {
            return RouteGraph::default();
        };
        let hubs: Vec<String> = self
            .config
            .network(&self.network_id)
            .ok()
            .map(|n| {
                [n.wquai.clone(), n.wqi.clone(), n.ecosystem.usdt.as_ref().map(|u| u.address.clone())]
                    .into_iter()
                    .flatten()
                    .map(|a| a.to_lowercase())
                    .collect()
            })
            .unwrap_or_default();
        RouteGraph::new(pools, &hubs)
    }

    /// The address a swap side occupies in the pool graph: native QUAI trades as WQUAI.
    fn graph_address(&self, asset: &SwapAsset) -> Option<String> {
        match asset {
            SwapAsset::Quai => self.config.network(&self.network_id).ok().and_then(|n| n.wquai.clone()).map(|a| a.to_lowercase()),
            SwapAsset::Token { address, .. } => Some(address.to_lowercase()),
        }
    }

    /// Token picker rows. `pay` picks which side is being chosen, so each row can say whether the
    /// router could actually fill it against the *other* side.
    pub fn picker_entries(&self, query: &str, pay: bool) -> Vec<PickerEntry> {
        let q = query.to_lowercase();
        let network = self.config.network(&self.network_id).ok();
        let graph = self.route_graph();
        // The side that is staying put. Picking the pay side is judged against the receive side.
        let counterpart =
            if pay { self.eco.swap.to.clone() } else { Some(self.eco.swap.from.clone()) }.and_then(|a| self.graph_address(&a));
        let route_for = |asset: &SwapAsset| -> RouteState {
            if graph.is_empty() {
                return RouteState::Unknown;
            }
            let (Some(other), Some(this)) = (counterpart.as_deref(), self.graph_address(asset)) else {
                return RouteState::Unknown;
            };
            // The same token on both sides is a wrap or a no-op, not a dead route; the card
            // already refuses it with a clearer message than the picker could.
            if this == other {
                return RouteState::Unknown;
            }
            let (from, to) = if pay { (this.as_str(), other) } else { (other, this.as_str()) };
            match graph.route(from, to) {
                Some(info) => RouteState::Fillable(info),
                None => RouteState::Dead,
            }
        };
        let curated: Vec<String> = network
            .as_ref()
            .map(|n| {
                [n.wqi.clone(), n.wquai.clone(), n.ecosystem.usdt.as_ref().map(|u| u.address.clone())]
                    .into_iter()
                    .flatten()
                    .map(|a| a.to_lowercase())
                    .collect()
            })
            .unwrap_or_default();
        let row = |asset: SwapAsset, info: String, verified: bool, holders: Option<u64>, icon: Option<String>| PickerEntry {
            route: route_for(&asset),
            asset,
            info,
            verified,
            holders,
            icon,
        };
        let mut out: Vec<PickerEntry> = vec![row(SwapAsset::Quai, "native".into(), true, None, None)];
        let mut seen = std::collections::HashSet::new();
        if let Some(p) = &self.eco.portfolio {
            for r in &p.rows {
                if let AssetKey::Token(address) = &r.key
                    && seen.insert(address.clone())
                {
                    let bal = format!("bal {}", amount::group_thousands(&amount::format_amount_short(r.amount(), r.decimals, 4)));
                    out.push(row(
                        SwapAsset::Token { address: address.clone(), symbol: r.symbol.clone(), decimals: r.decimals },
                        bal,
                        r.trust == wallet_core::portfolio::Trust::Verified,
                        r.holders,
                        r.icon_url.clone(),
                    ));
                }
            }
        }
        for m in &self.eco.markets {
            if seen.insert(m.address.clone()) {
                let usdt =
                    network.as_ref().and_then(|n| n.ecosystem.usdt.as_ref()).is_some_and(|u| u.address.eq_ignore_ascii_case(&m.address));
                let decimals = match self.eco.token_info.get(&m.address) {
                    Some(Ok((info, _))) => info.decimals.unwrap_or(UNKNOWN_DECIMALS),
                    _ if usdt => 6,
                    _ if curated.contains(&m.address) => 18,
                    _ => UNKNOWN_DECIMALS,
                };
                out.push(row(
                    SwapAsset::Token { address: m.address.clone(), symbol: m.symbol.clone(), decimals },
                    m.price_usd.map(amount::usd_price).unwrap_or_else(|| "unpriced".into()),
                    curated.contains(&m.address),
                    m.holders,
                    m.icon_url.clone(),
                ));
            }
        }
        // Tokens that exist only in a pool: the explorer's market list does not carry every one,
        // and a token with a pool is by definition swappable, so it belongs in the picker.
        if let Some(Ok((pools, _))) = self.eco.markets_view.pools.as_ref().map(|r| r.as_ref()) {
            for token in pools.iter().flat_map(|p| [&p.token0, &p.token1]) {
                if !token.address.is_empty() && seen.insert(token.address.clone()) {
                    out.push(row(
                        SwapAsset::Token { address: token.address.clone(), symbol: token.symbol.clone(), decimals: token.decimals },
                        "in a pool".into(),
                        curated.contains(&token.address),
                        None,
                        None,
                    ));
                }
            }
        }
        out.retain(|e| {
            q.is_empty()
                || e.asset.symbol().to_lowercase().contains(&q)
                || matches!(&e.asset, SwapAsset::Token { address, .. } if address.contains(&q))
        });
        // Fillable first, then unknown, then dead — a token that cannot be reached is still listed
        // (so search finds it and says why) but never sits above one that can.
        out.sort_by_key(|e| match &e.route {
            RouteState::Fillable(info) if !info.thin() => 0,
            RouteState::Fillable(_) => 1,
            RouteState::Unknown => 2,
            RouteState::Dead => 3,
        });
        out
    }

    /// The balance of a swap side, only when the wallet knows it exactly. An indexer's rounded
    /// figure is refused rather than filled: MAX from a rounded-up balance builds a transaction
    /// that reverts for insufficient funds.
    fn exact_balance(&self, asset: &SwapAsset) -> Option<(U256, u8)> {
        let rows = &self.eco.portfolio.as_ref()?.rows;
        let row = rows
            .iter()
            .find(|r| match (&r.key, asset) {
                (AssetKey::Quai, SwapAsset::Quai) => true,
                (AssetKey::Token(a), SwapAsset::Token { address, .. }) => a.eq_ignore_ascii_case(address),
                _ => false,
            })
            .filter(|r| r.exact)?;
        Some((row.amount(), row.decimals))
    }

    /// A configured wrapper token as a swap asset, so MAX can look its balance up.
    fn wrapper_asset(&self, wqi: bool) -> Option<SwapAsset> {
        let n = self.config.network(&self.network_id).ok()?;
        let address = if wqi { n.wqi.clone()? } else { n.wquai.clone()? };
        Some(SwapAsset::Token { address: address.to_lowercase(), symbol: if wqi { "WQI".into() } else { "WQUAI".into() }, decimals: 18 })
    }

    /// Qi coins this wallet could actually spend now: not reserved by a pending operation, and
    /// past their unlock height.
    fn spendable_coins(&self) -> Vec<wallet_core::spendable::Coin> {
        let Some(qi) = self.dash.qi.as_ref() else { return Vec::new() };
        let head = U256::from(qi.checkpoint_height.unwrap_or(u64::MAX));
        qi.coins
            .iter()
            .filter(|c| !c.reserved && c.unlock_height <= head)
            .map(|c| wallet_core::spendable::Coin { qits: c.qits, denomination: c.denomination })
            .collect()
    }

    /// `m` on an amount field: fill it with everything that can actually be spent.
    ///
    /// Three ledgers, three answers — an ERC-20 spends its whole balance, native QUAI must keep
    /// back what the transaction costs, and Qi is capped by how many coins one transaction holds.
    pub(crate) fn max_identity(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        format!("{:?}", self.screen).hash(&mut h);
        self.meta.as_ref().map(|m| &m.id).hash(&mut h);
        self.network_id.hash(&mut h);
        self.dash.accounts.first().map(|a| &a.address).hash(&mut h);
        self.eco.convert.amount.hash(&mut h);
        self.eco.convert.qi_to_quai.hash(&mut h);
        self.eco.convert.slippage_bps.hash(&mut h);
        self.eco.wrap.amount.hash(&mut h);
        self.eco.wrap.mode.hash(&mut h);
        h.finish()
    }

    pub fn fill_max(&mut self) {
        if (self.screen == Screen::Convert && self.eco.convert.qi_to_quai) || (self.screen == Screen::Wrap && self.eco.wrap.mode == 0) {
            self.eco.max_sequence = self.eco.max_sequence.wrapping_add(1);
            let key = self.eco.max_sequence;
            self.eco.max_request = Some((key, self.max_identity()));
            self.send(Cmd::QiMax {
                key,
                wrapping: self.screen == Screen::Wrap,
                account: self.dash.accounts.first().map(|a| a.address.clone()),
                slippage: self.eco.convert.slippage_bps,
            });
            self.toast("quoting a spendable Qi amount with current fees…", false);
            return;
        }
        use wallet_core::spendable::{MAX_ROUTE_HOPS, qi_max, quai_max, swap_max_gas, token_max};
        // Which card is being edited, and what it pays with.
        enum Pay {
            Asset(SwapAsset),
            Qi { whole: bool },
        }
        let pay = match self.screen {
            Screen::Swap => Pay::Asset(self.eco.swap.from.clone()),
            // Qi → QUAI spends Qi coins. The protocol conversion takes fractional Qi
            // (`review_convert_qi_to_quai` parses 3 decimals), so MAX must not round down.
            Screen::Convert if self.eco.convert.qi_to_quai => Pay::Qi { whole: false },
            Screen::Convert => Pay::Asset(SwapAsset::Quai),
            Screen::Wrap => match self.eco.wrap.mode {
                // Wrapping takes fractional Qi too; only redemption (mode 2) is whole-Qi, and that
                // side spends WQI, handled as a token below.
                0 => Pay::Qi { whole: false },
                // Claim takes no amount at all.
                1 => return,
                // WQI → Qi and WQUAI → QUAI spend the wrapper token, an ordinary ERC-20.
                2 => match self.wrapper_asset(true) {
                    Some(a) => Pay::Asset(a),
                    None => return,
                },
                3 => Pay::Asset(SwapAsset::Quai),
                _ => match self.wrapper_asset(false) {
                    Some(a) => Pay::Asset(a),
                    None => return,
                },
            },
            _ => return,
        };
        let (text, note) = match pay {
            Pay::Qi { whole } => {
                let coins = self.spendable_coins();
                if coins.is_empty() {
                    self.toast("no spendable Qi coins yet", true);
                    return;
                }
                let m = qi_max(&coins, whole);
                if m.amount.is_zero() {
                    self.toast("the spendable Qi coins do not cover a whole Qi plus its fee", true);
                    return;
                }
                (m.text(), m.note())
            }
            Pay::Asset(asset) => {
                let Some((balance, decimals)) = self.exact_balance(&asset) else {
                    self.toast("balance is still loading", true);
                    return;
                };
                if balance.is_zero() {
                    self.toast(format!("no {} to spend", asset.symbol()), true);
                    return;
                }
                match asset {
                    SwapAsset::Quai => {
                        let Some(price) = self.eco.gas_price else {
                            self.toast("waiting for the gas price before MAX can keep fees back", true);
                            return;
                        };
                        // Budget for the deepest route: the receive token can still change.
                        let m = quai_max(balance, price, swap_max_gas(MAX_ROUTE_HOPS));
                        if m.is_zero() {
                            self.toast("this balance cannot cover its own fee", true);
                            return;
                        }
                        (m.text(), m.note())
                    }
                    SwapAsset::Token { .. } => {
                        let mut m = token_max(balance, decimals);
                        // Redeeming WQI pays out whole Qi, so offering a fractional MAX would
                        // only be rounded away by the contract.
                        if self.screen == Screen::Wrap && self.eco.wrap.mode == 2 {
                            let unit = U256::from(10u64).pow(U256::from(decimals));
                            m.amount -= m.amount % unit;
                            if m.amount.is_zero() {
                                self.toast("less than one whole WQI to redeem", true);
                                return;
                            }
                        }
                        (m.text(), m.note())
                    }
                }
            }
        };
        match self.screen {
            Screen::Swap => {
                self.eco.swap.amount = text;
                self.eco.swap.field = 1;
                self.eco.swap.edited = Some(Instant::now());
                self.eco.swap.requested_key = 0;
                self.eco.swap.approving = false;
            }
            Screen::Convert => {
                self.eco.convert.amount = text;
                self.eco.convert.field = 1;
                self.eco.convert.edited = Some(Instant::now());
            }
            Screen::Wrap => {
                self.eco.wrap.amount = text;
                self.eco.wrap.field = 1;
            }
            _ => return,
        }
        if let Some(note) = note {
            self.toast(&note, false);
        }
    }

    /// Keys for views with inline inputs. Returns true when consumed.
    pub fn view_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return false;
        }
        match self.screen {
            Screen::Markets => self.markets_key(key),
            Screen::Board => self.board_key(key),
            Screen::Pools => self.pools_key(key),
            Screen::Launches => self.launches_key(key),
            Screen::Pnl => self.pnl_key(key),
            Screen::Swap => self.swap_key(key),
            Screen::Convert => self.convert_key(key),
            Screen::Wrap => self.wrap_key(key),
            Screen::Explore => self.explore_key(key),
            Screen::Collected => match key.code {
                KeyCode::Char('h') | KeyCode::Left => {
                    self.move_selection(-1);
                    true
                }
                KeyCode::Char('l') | KeyCode::Right if self.eco.nft_len() > 0 => {
                    self.move_selection(1);
                    true
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    let cols = (*self.eco.grid_columns.borrow()).max(1);
                    let len = self.eco.nft_len();
                    if len > 0 {
                        self.selected = (self.selected + cols).min(len - 1);
                    }
                    true
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    let cols = (*self.eco.grid_columns.borrow()).max(1);
                    self.selected = self.selected.saturating_sub(cols);
                    true
                }
                KeyCode::Char('R') => {
                    self.load_nfts(true);
                    true
                }
                KeyCode::Char('T') => {
                    self.transfer_selected_nft();
                    true
                }
                KeyCode::Char('L') => {
                    if let Some((c, id)) = self.selected_nft() {
                        self.open_nft_list(&c, &id);
                    }
                    true
                }
                KeyCode::Char('X') => {
                    if let Some((c, id)) = self.selected_nft() {
                        self.cancel_nft_listing(&c, &id);
                    }
                    true
                }
                _ => false,
            },
            Screen::Listings => match key.code {
                KeyCode::Char('m') => {
                    self.eco.listings_mine = !self.eco.listings_mine;
                    self.selected = 0;
                    if self.eco.listings_mine {
                        self.load_my_listings();
                    }
                    self.toast(if self.eco.listings_mine { "listings: yours" } else { "listings: everyone's" }, false);
                    true
                }
                KeyCode::Char('S') => {
                    self.eco.listing_sort = self.eco.listing_sort.next();
                    self.selected = 0;
                    let label = self.eco.listing_sort.label();
                    self.toast(format!("listings: {label}"), false);
                    true
                }
                KeyCode::Char(c @ ('f' | 'F')) => {
                    let collections = match self.eco.listings.get(&None) {
                        Some(Ok(all)) => wallet_core::market::listing_collections(all),
                        _ => Vec::new(),
                    };
                    if collections.is_empty() {
                        return true;
                    }
                    // Positions: 0 = all collections, then each collection by listing count.
                    let len = collections.len() + 1;
                    let current = self
                        .eco
                        .listing_filter
                        .as_ref()
                        .and_then(|f| collections.iter().position(|(a, _)| a == f).map(|p| p + 1))
                        .unwrap_or(0);
                    let next = if c == 'f' { (current + 1) % len } else { (current + len - 1) % len };
                    self.eco.listing_filter = (next > 0).then(|| collections[next - 1].0.clone());
                    self.selected = 0;
                    let text = match &self.eco.listing_filter {
                        Some(a) => format!("listings: {} ({} listed)", self.eco.collection_name(a), collections[next - 1].1),
                        None => "listings: all collections".into(),
                    };
                    self.toast(text, false);
                    true
                }
                KeyCode::Char('R') => {
                    self.eco.listings_loading = true;
                    self.send_data(DataCmd::Listings { collection: None });
                    true
                }
                KeyCode::Char('b') => {
                    if let Some(l) = self.eco.visible_listings().get(self.selected).cloned() {
                        self.push_detail(Detail::Nft(l.contract.clone(), l.token_id.clone()));
                        self.buy_listing(&l);
                    }
                    true
                }
                _ => false,
            },
            Screen::Home if key.code == KeyCode::Char('i') => {
                self.eco.info_open = !self.eco.info_open;
                true
            }
            Screen::Network if key.code == KeyCode::Char('m') => {
                if let Some((id, _)) = self.dash.networks.get(self.selected).cloned() {
                    self.open_form(super::app::FormKind::Monitor { network: id });
                }
                true
            }
            _ => false,
        }
    }

    fn swap_key(&mut self, key: KeyEvent) -> bool {
        if key.code == KeyCode::Char('B') {
            let asset = |a: &SwapAsset| match a {
                SwapAsset::Quai => "quai".into(),
                SwapAsset::Token { address, .. } => address.clone(),
            };
            if let Some(to) = &self.eco.swap.to {
                self.open_form(FormKind::BoundedSwap {
                    from: asset(&self.eco.swap.from),
                    to: asset(to),
                    input: self.eco.swap.amount.clone(),
                });
            } else {
                self.toast("choose a receive token first", true);
            }
            return true;
        }
        if key.code == KeyCode::Char('L') {
            self.send(Cmd::Order(super::order_ui::Request::List));
            return true;
        }
        if key.code == KeyCode::Char('O') {
            let asset = |a: &SwapAsset| match a {
                SwapAsset::Quai => "quai".into(),
                SwapAsset::Token { address, .. } => address.clone(),
            };
            if let Some(to) = &self.eco.swap.to {
                self.open_form(FormKind::OrderCreate {
                    from: asset(&self.eco.swap.from),
                    to: asset(to),
                    input: self.eco.swap.amount.clone(),
                    slippage: self.eco.swap.slippage_bps,
                });
            } else {
                self.toast("choose a receive token and input amount first", true);
            }
            return true;
        }
        if key.code == KeyCode::Char('P') {
            let asset = |a: &SwapAsset| match a {
                SwapAsset::Quai => "quai".into(),
                SwapAsset::Token { address, .. } => address.clone(),
            };
            if let Some(identity) = self.swap_input_key()
                && let Some(to) = &self.eco.swap.to
            {
                self.eco.max_sequence = self.eco.max_sequence.wrapping_add(1);
                let key = self.eco.max_sequence;
                self.eco.split_request = Some((key, identity));
                self.send(Cmd::SplitQuote {
                    key,
                    account: self.dash.accounts.first().map(|a| a.address.clone()),
                    from: asset(&self.eco.swap.from),
                    to: asset(to),
                    amount: self.eco.swap.amount.clone(),
                    slippage: self.eco.swap.slippage_bps,
                });
                self.toast("comparing split allocations and added fees…", false);
            } else {
                self.toast("choose both tokens and enter an amount first", true);
            }
            return true;
        }
        if key.code == KeyCode::Char('E') {
            let asset = |a: &SwapAsset| match a {
                SwapAsset::Quai => "quai".into(),
                SwapAsset::Token { address, .. } => address.clone(),
            };
            if let Some(to) = &self.eco.swap.to {
                self.open_form(FormKind::ExactOutput { from: asset(&self.eco.swap.from), to: asset(to) });
            } else {
                self.toast("choose a receive token first", true);
            }
            return true;
        }
        // MAX is about the balance, not the cursor, so it works focused or not.
        if key.code == KeyCode::Char('m') {
            self.fill_max();
            self.eco.swap.preset = if self.eco.swap.amount.is_empty() { None } else { Some(100) };
            return true;
        }
        // A quarter, half, three quarters, all: each press takes the next share of MAX.
        if key.code == KeyCode::Char('%') {
            self.fill_share();
            return true;
        }
        let card = &mut self.eco.swap;
        if card.field == 5 {
            // Unfocused: only Tab / Enter / picker / flip enter the card; everything else is global.
            match key.code {
                KeyCode::Tab | KeyCode::Enter => card.field = 1,
                KeyCode::BackTab => card.field = 4,
                KeyCode::Char('/') | KeyCode::Char('f') => {}
                _ => return false,
            }
            if matches!(key.code, KeyCode::Tab | KeyCode::Enter | KeyCode::BackTab) {
                return true;
            }
        }
        match key.code {
            KeyCode::Esc => card.field = 5,
            KeyCode::Tab => card.field = (card.field + 1) % 5,
            KeyCode::BackTab => card.field = (card.field + 4) % 5,
            KeyCode::Down if card.field < 4 => card.field += 1,
            KeyCode::Up if card.field > 0 => card.field -= 1,
            KeyCode::Char('/') => {
                let pay = card.field != 2;
                self.modal = Modal::TokenPicker { pay, query: String::new(), selected: 0 };
            }
            KeyCode::Char('f') => {
                if let Some(to) = card.to.take() {
                    card.to = Some(std::mem::replace(&mut card.from, to));
                    card.amount.clear();
                    card.quote = None;
                    card.edited = Some(Instant::now());
                    card.requested_key = 0;
                }
            }
            KeyCode::Left | KeyCode::Right if card.field == 3 => {
                let steps = [10u16, 30, 50, 100, 200, 300, 500, 1000];
                let i = steps.iter().position(|s| *s >= card.slippage_bps).unwrap_or(2) as i32;
                let d = if key.code == KeyCode::Left { -1 } else { 1 };
                card.slippage_bps = steps[(i + d).clamp(0, steps.len() as i32 - 1) as usize];
                card.edited = Some(Instant::now());
                card.requested_key = 0;
            }
            KeyCode::Left | KeyCode::Right if card.field == 4 => {
                let steps = [2u32, 5, 10, 20, 30, 60];
                let i = steps.iter().position(|s| *s >= card.deadline_minutes).unwrap_or(2) as i32;
                let d = if key.code == KeyCode::Left { -1 } else { 1 };
                card.deadline_minutes = steps[(i + d).clamp(0, steps.len() as i32 - 1) as usize];
                card.requested_key = 0;
                card.edited = Some(Instant::now());
            }
            KeyCode::Enter if matches!(card.field, 0 | 2) => {
                let pay = card.field == 0;
                self.modal = Modal::TokenPicker { pay, query: String::new(), selected: 0 };
            }
            KeyCode::Enter => self.swap_submit(),
            _ if card.field == 1 => {
                let decimals = card.from.decimals();
                if digits_input(&mut card.amount, &key, decimals) {
                    card.field = 1;
                    card.edited = Some(Instant::now());
                    card.requested_key = 0;
                    card.approving = false;
                    card.preset = None;
                    return true;
                }
                return false;
            }
            _ => return false,
        }
        true
    }

    /// The next of 25/50/75/100% of what MAX would fill, on the swap card.
    fn fill_share(&mut self) {
        let next = match self.eco.swap.preset {
            Some(25) => 50,
            Some(50) => 75,
            Some(75) => 100,
            _ => 25,
        };
        self.fill_max();
        let card = &mut self.eco.swap;
        let decimals = card.from.decimals();
        let Ok(max) = amount::parse_amount(&card.amount, decimals) else { return };
        if max.is_zero() {
            return;
        }
        let share = max * U256::from(next) / U256::from(100u8);
        card.amount = amount::format_amount(share, decimals);
        card.preset = Some(next);
        card.edited = Some(Instant::now());
    }

    /// Watched pools first, in the order they were watched; the rest keep their order. The cursor
    /// stays on the pool it was on.
    /// Put the cursor back on `address` after the list is re-ordered (a pair watched or
    /// unwatched, a refreshed directory). [`App::market_rows`] decides the order itself, so
    /// nothing here moves the pools.
    pub fn keep_cursor_on(&mut self, address: Option<String>) {
        let Some(addr) = address else { return };
        let Some(i) = self.market_rows().iter().position(|p| p.address == addr) else { return };
        if self.screen == Screen::Markets && self.pane == 0 {
            self.selected = i;
        }
        self.eco.markets_view.pair_selected = i;
    }

    /// Alerts: read them once, and check them every minute while no daemon is running (the
    /// daemon checks them itself, and two checkers would each fire).
    fn tick_alerts(&mut self) {
        if self.locked || self.meta.is_none() {
            return;
        }
        if !self.eco.alerts_loaded {
            self.eco.alerts_loaded = true;
            self.send_data(DataCmd::Alerts(super::data::AlertOp::Load));
            return;
        }
        if self.eco.alerts.is_empty() || self.eco.alerts_checked.is_some_and(|t| t.elapsed() < Duration::from_secs(60)) {
            return;
        }
        self.eco.alerts_checked = Some(Instant::now());
        let healthy = self
            .flow_db()
            .ok()
            .and_then(|db| db.kv(&format!("alerts_heartbeat:{}", self.network_id)).ok().flatten())
            .and_then(|value| serde_json::from_str::<serde_json::Value>(&value).ok())
            .is_some_and(|value| {
                value["error"].is_null()
                    && value["completed_at"].as_u64().is_some_and(|at| wallet_core::registry::now().saturating_sub(at) < 180)
            });
        if !healthy {
            self.send_data(DataCmd::Alerts(super::data::AlertOp::Check { pairs: self.config.features.trading }));
        }
    }

    /// The pool that trades the swap card's pair directly (the deepest, if several do), and
    /// whether the pay token is its token0. Drawn from the Markets list, so it is a display aid:
    /// the quote reads the router on-chain regardless.
    pub fn swap_pool(&self) -> Option<(wallet_core::markets::Pool, bool)> {
        let to = self.eco.swap.to.as_ref()?;
        let wquai = self.config.network(&self.network_id).ok().and_then(|n| n.wquai).map(|w| w.to_lowercase());
        let address = |a: &SwapAsset| match a {
            SwapAsset::Quai => wquai.clone(),
            SwapAsset::Token { address, .. } => Some(address.to_lowercase()),
        };
        let (pay, get) = (address(&self.eco.swap.from)?, address(to)?);
        let Some(Ok((pools, _))) = &self.eco.markets_view.pools else { return None };
        pools
            .iter()
            .filter(|p| p.venue != wallet_core::markets::Venue::Curve)
            .filter(|p| {
                let (a, b) = (p.token0.address.to_lowercase(), p.token1.address.to_lowercase());
                (a == pay && b == get) || (a == get && b == pay)
            })
            .max_by(|a, b| a.tvl_usd.unwrap_or(0.0).total_cmp(&b.tvl_usd.unwrap_or(0.0)))
            .map(|p| (p.clone(), p.token0.address.eq_ignore_ascii_case(&pay)))
    }

    /// Keep the swap card's rate and chart fed: the market list, then the pair's hourly candles.
    pub(crate) fn tick_swap(&mut self) {
        let mv = &self.eco.markets_view;
        if !mv.pools_loading
            && mv.pools_attempted.is_none_or(|at| at.elapsed() >= MARKET_REFRESH)
            && (mv.pools.is_none() || mv.pools_at.is_none_or(|t| t.elapsed().as_secs() > wallet_core::markets::DIRECTORY_TTL))
        {
            self.eco.markets_view.pools_loading = true;
            self.eco.markets_view.pools_attempted = Some(Instant::now());
            self.send_data(DataCmd::MarketPools);
            return;
        }
        let Some((pool, _)) = self.swap_pool() else { return };
        let fresh = matches!(&self.eco.swap.chart_asked, Some((a, at)) if *a == pool.address && at.elapsed() < MARKET_REFRESH);
        if !fresh {
            self.eco.swap.chart_asked = Some((pool.address.clone(), Instant::now()));
            self.send_data(DataCmd::PairCandles { pool: pool.address, bucket: SWAP_CHART_BUCKET, count: MARKET_CANDLES });
        }
    }

    pub(crate) fn swap_submit(&mut self) {
        if !self.can_sign() {
            self.toast("this wallet is watch-only", true);
            return;
        }
        let card = &self.eco.swap;
        let Some(to) = card.to.clone() else {
            self.toast("pick a token to receive (/)", true);
            return;
        };
        if card.from.decimals() == UNKNOWN_DECIMALS || to.decimals() == UNKNOWN_DECIMALS {
            self.toast("still reading the token's decimals…", false);
            return;
        }
        if amount::parse_amount(&card.amount, card.from.decimals()).map_or(true, |a| a.is_zero()) {
            self.toast("enter an amount to pay", true);
            return;
        }
        let quote = match (&card.quote, self.swap_quote_current()) {
            (Some(Ok(q)), true) => q.clone(),
            (Some(Err(e)), true) => {
                let e = e.clone();
                self.toast(e, true);
                return;
            }
            _ => {
                self.toast("waiting for a fresh quote…", false);
                return;
            }
        };
        let from_id = match &card.from {
            SwapAsset::Quai => "quai".to_string(),
            SwapAsset::Token { address, .. } => address.clone(),
        };
        let to_id = match &to {
            SwapAsset::Quai => "quai".to_string(),
            SwapAsset::Token { address, .. } => address.clone(),
        };
        let network = self.config.network(&self.network_id).ok();
        let wquai = network.as_ref().and_then(|n| n.wquai.clone()).unwrap_or_default();
        let paying_wquai = !wquai.is_empty() && from_id.eq_ignore_ascii_case(&wquai);
        // Paying a WQUAI pool from QUAI: wrap what is missing first (each step still reviewed).
        let mut prewrap = None;
        if quote.insufficient && paying_wquai {
            let needed = amount::parse_amount(&card.amount, 18).unwrap_or(U256::ZERO);
            let missing = needed.saturating_sub(U256::from(self.wrapped_atoms(false)));
            // The swap is signed by the first account, so only its QUAI can be wrapped.
            let quai = self.dash.accounts.first().map_or(U256::ZERO, |a| a.balance);
            if quai > missing {
                prewrap = Some(amount::format_amount(missing, 18));
            }
        }
        if quote.insufficient && prewrap.is_none() {
            let wqi = network.as_ref().and_then(|n| n.wqi.clone()).is_some_and(|w| from_id.eq_ignore_ascii_case(&w));
            self.toast(
                format!(
                    "not enough {} to pay this amount{}",
                    card.from.symbol(),
                    if wqi { " · wrapped Qi must be claimed first (Trade › Wrap › Claim WQI)" } else { "" }
                ),
                true,
            );
            return;
        }
        let account = self.dash.accounts.first().map(|a| a.address.clone());
        // A swap that pays out WQUAI offers to redeem it for QUAI afterwards.
        let redeem = !wquai.is_empty() && to_id.eq_ignore_ascii_case(&wquai);
        // Across both exchanges: swap to the hub first; the second swap is sized once it confirms.
        let (first_to, then, unwrap_after) = match (quote.hub(), quote.legs.first()) {
            (Some((hub, _)), Some(first)) => {
                let hub_decimals = if first.output_decimals == 0 { 18 } else { first.output_decimals };
                (hub, Some(NextSwap { to: to_id.clone(), unwrap_after: redeem, hub_decimals, first: None, polls: 0 }), false)
            }
            _ => (to_id.clone(), None, redeem),
        };
        let label =
            format!("swap {} {} → {}{}", card.amount, card.from.symbol(), to.symbol(), if then.is_some() { " (two swaps)" } else { "" });
        let (amount, slippage, deadline) = (card.amount.clone(), card.slippage_bps, card.deadline_minutes);
        if prewrap.is_none() && !redeem {
            let Some(owner) = account.clone() else {
                self.toast("select a signing account", true);
                return;
            };
            let action = if then.is_some() {
                wallet_core::execution::TradingAction::CrossVenue {
                    from: from_id,
                    to: to_id,
                    hub: first_to,
                    amount,
                    stage: 0,
                    slippage,
                    deadline,
                }
            } else {
                wallet_core::execution::TradingAction::Swap { from: from_id, to: to_id, amount, slippage, deadline }
            };
            self.start_flow(FlowKind::Steps {
                prepare: Box::new(Prepare::Trading {
                    intent: wallet_core::execution::TradingIntent { account: owner, max_fee: None, action },
                }),
                label,
            });
            return;
        }
        if let Some(missing) = &prewrap {
            self.toast(format!("wrapping {missing} QUAI first, then the swap"), false);
        }
        self.start_flow(FlowKind::Swap {
            account,
            from: from_id,
            to: first_to,
            amount,
            slippage,
            deadline,
            label,
            prewrap,
            unwrap_after,
            baseline: self.wrapped_atoms(false).to_string(),
            then,
        });
    }

    // ------------------------------------------------------------------ markets

    /// Which pair the chart is showing: the cursor while the pairs list has it, else the pair
    /// the cursor left behind when it moved to the flow column.
    pub fn markets_pair(&self) -> usize {
        // Off Markets (the trader layout draws it beside the swap card) the cursor belongs to that
        // screen, so the chart keeps the pair it was left on.
        if self.screen != Screen::Markets || self.pane == 1 { self.eco.markets_view.pair_selected } else { self.selected }
    }

    /// Re-order the pairs list and keep the cursor on the pair it was on.
    fn sort_markets(&mut self, next: impl Fn(MarketSort) -> MarketSort) -> bool {
        let holding = self.selected_pool().map(|p| p.address);
        self.eco.markets_view.sort = next(self.eco.markets_view.sort);
        let label = self.eco.markets_view.sort.label();
        if let Some(address) = holding
            && let Some(i) = self.market_rows().iter().position(|p| p.address == address)
        {
            // Only the pairs list owns `selected`; while the flow column has the cursor, moving it
            // here would drag that column's cursor to a row number that means nothing in it.
            if self.screen == Screen::Markets && self.pane == 0 {
                self.selected = i;
            }
            self.eco.markets_view.pair_selected = i;
        }
        self.toast(format!("pairs by {label}"), false);
        true
    }

    /// The pairs list in the order it is shown. A pair with no figure to sort on goes last, so
    /// the rows carrying the number the user asked for are the ones at the top.
    pub fn market_rows(&self) -> Vec<wallet_core::markets::Pool> {
        let mv = &self.eco.markets_view;
        let Some(Ok((pools, _))) = &mv.pools else { return Vec::new() };
        let mut rows = pools.clone();
        let key = |p: &wallet_core::markets::Pool| match mv.sort {
            MarketSort::TvlDesc | MarketSort::TvlAsc => p.tvl_usd,
            MarketSort::ChangeDesc | MarketSort::ChangeAsc => p.change_24h(),
            MarketSort::Default => None,
        };
        match mv.sort {
            MarketSort::Default => {}
            MarketSort::TvlDesc | MarketSort::ChangeDesc => {
                rows.sort_by(|a, b| key(b).is_some().cmp(&key(a).is_some()).then(key(b).unwrap_or(0.0).total_cmp(&key(a).unwrap_or(0.0))));
            }
            MarketSort::TvlAsc | MarketSort::ChangeAsc => {
                rows.sort_by(|a, b| key(b).is_some().cmp(&key(a).is_some()).then(key(a).unwrap_or(0.0).total_cmp(&key(b).unwrap_or(0.0))));
            }
        }
        // Watched pairs stay at the top whatever the order: watching one is the user saying it
        // belongs in front. The sort still decides the order within each group (a stable sort).
        if !self.eco.watchlist.is_empty() {
            let watched = |p: &wallet_core::markets::Pool| self.eco.watchlist.iter().any(|w| w.eq_ignore_ascii_case(&p.address));
            rows.sort_by_key(|p| !watched(p));
        }
        rows
    }

    /// The pool the chart is showing.
    pub fn selected_pool(&self) -> Option<wallet_core::markets::Pool> {
        let rows = self.market_rows();
        rows.get(self.markets_pair().min(rows.len().saturating_sub(1))).cloned()
    }

    /// A token's USD price: USDT is a dollar, wrapped QUAI takes the portfolio's QUAI price,
    /// and anything else is worth what the token market says, if it says anything.
    pub fn token_usd(&self, token: &wallet_core::markets::PoolToken) -> Option<f64> {
        // A network the config cannot name still has tokens the market prices: only the two
        // the profile identifies depend on it.
        if let Ok(network) = self.config.network(&self.network_id) {
            if network.ecosystem.usdt.as_ref().is_some_and(|u| u.address.eq_ignore_ascii_case(&token.address)) {
                return Some(1.0);
            }
            if network.wquai.as_ref().is_some_and(|w| w.eq_ignore_ascii_case(&token.address)) {
                return self.eco.portfolio.as_ref().and_then(|p| p.prices.as_ref()).and_then(|b| b.quai_usd);
            }
        }
        self.eco.markets.iter().find(|m| m.address.eq_ignore_ascii_case(&token.address)).and_then(|m| m.price_usd)
    }

    /// What a swap was worth, priced from whichever side has a price.
    pub fn swap_usd(&self, swap: &wallet_core::markets::DexSwap) -> Option<f64> {
        self.token_usd(&swap.token_in)
            .map(|p| p * swap.amount_in)
            .or_else(|| self.token_usd(&swap.token_out).map(|p| p * swap.amount_out))
            .filter(|v| v.is_finite())
    }

    /// The flow rows the column shows: every swap worth at least the threshold. A swap nobody
    /// can price is kept — hiding what cannot be judged would drop real trades silently.
    pub fn flow_rows(&self) -> Vec<&wallet_core::markets::DexSwap> {
        let min = self.eco.markets_view.flow_min_usd;
        self.eco.markets_view.flow.iter().filter(|s| min <= 0.0 || self.swap_usd(s).is_none_or(|v| v >= min)).collect()
    }

    /// The base of a tape row — the side the row is about, and the side a green or red arrow
    /// refers to.
    ///
    /// A pool the directory carries decides it exactly as the pairs list does. A curve trade has
    /// no pair to look up: it happened on the curve contract, and only a bonded HartiiLabs curve
    /// is listed as a pool at all. So the base falls back to whichever side is not the money —
    /// the launch token, which is what was bought or sold. Without this every curve row drew in
    /// plain text with no price, as though the wallet could not tell which way it went.
    pub fn flow_base<'a>(
        &self,
        swap: &'a wallet_core::markets::DexSwap,
        pool: Option<&'a wallet_core::markets::Pool>,
    ) -> Option<&'a wallet_core::markets::PoolToken> {
        if let Some(p) = pool {
            return Some(if self.pool_base0(p) { &p.token0 } else { &p.token1 });
        }
        let network = self.config.network(&self.network_id).ok()?;
        let money = |t: &wallet_core::markets::PoolToken| {
            network.wquai.as_deref().is_some_and(|w| w.eq_ignore_ascii_case(&t.address))
                || network.ecosystem.usdt.as_ref().is_some_and(|u| u.address.eq_ignore_ascii_case(&t.address))
        };
        // Both sides money, or neither: nothing here says which one the row is about.
        match (money(&swap.token_in), money(&swap.token_out)) {
            (true, false) => Some(&swap.token_out),
            (false, true) => Some(&swap.token_in),
            _ => None,
        }
    }

    /// Whether token0 is the base (priced in the quote), honoring the user's flip.
    pub fn pool_base0(&self, pool: &wallet_core::markets::Pool) -> bool {
        let network = self.config.network(&self.network_id).ok();
        let usdt = network.as_ref().and_then(|n| n.ecosystem.usdt.as_ref().map(|u| u.address.clone()));
        let natural = wallet_core::markets::base_is_token0(
            pool,
            usdt.as_deref(),
            network.as_ref().and_then(|n| n.wquai.as_deref()),
            network.as_ref().and_then(|n| n.wqi.as_deref()),
        );
        natural != self.eco.markets_view.flipped.contains(&pool.address)
    }

    /// Display symbol for a pool token (WQUAI trades as native QUAI through the router).
    pub fn market_symbol(&self, token: &wallet_core::markets::PoolToken) -> String {
        let wquai = self.config.network(&self.network_id).ok().and_then(|n| n.wquai).map(|w| w.to_lowercase());
        if wquai.as_deref() == Some(token.address.as_str()) { "QUAI".into() } else { token.symbol.clone() }
    }

    /// Refresh the pool directory, the DEX-wide tape, live reserves and the selected pool's own
    /// trades. A tick that falls inside a source's cache TTL is served from the store and never
    /// reaches the network, so this paces the screen rather than the network.
    pub(crate) fn tick_markets(&mut self) {
        let mv = &self.eco.markets_view;
        let stale_pools = mv.pools_at.is_none_or(|t| t.elapsed().as_secs() > wallet_core::markets::DIRECTORY_TTL);
        if !mv.pools_loading && (mv.pools.is_none() || stale_pools) && mv.pools_attempted.is_none_or(|at| at.elapsed() >= MARKET_REFRESH) {
            self.eco.markets_view.pools_loading = true;
            self.eco.markets_view.pools_attempted = Some(Instant::now());
            self.send_data(DataCmd::MarketPools);
            return;
        }
        self.tick_dex_flow();
        self.tick_reserves();
        let Some(pool) = self.selected_pool() else { return };
        // Scrolling the list is not a request for every row it passes over. A row is only asked
        // about once the cursor has rested on it, which is what turns a 26-row scroll from one
        // fetch per row into one fetch for the row the user stopped at.
        let settled = match &self.eco.markets_view.selected_at {
            Some((address, at)) if *address == pool.address => at.elapsed() >= SELECTION_SETTLES,
            _ => {
                self.eco.markets_view.selected_at = Some((pool.address.clone(), Instant::now()));
                false
            }
        };
        if !settled {
            return;
        }
        let bucket = wallet_core::markets::TIMEFRAMES[self.eco.markets_view.timeframe].1;
        let since = wallet_core::registry::now()
            .saturating_sub(bucket * (MARKET_CANDLES as u64 + 1))
            .max(wallet_core::registry::now().saturating_sub(30 * 86_400));
        let mv = &self.eco.markets_view;
        let due = match mv.events_at.get(&pool.address) {
            None => true,
            Some((at, window)) => at.elapsed() > MARKET_REFRESH || *window > since,
        };
        // The indexer already has this timeframe bucketed, so ask for it alongside the logs: the
        // chart can draw from whichever lands first, and the logs are still needed for the tape.
        let want_candles = wallet_core::subgraph::interval_for(bucket).is_some()
            && mv.candles_requested.get(&(pool.address.clone(), bucket)).is_none_or(|at| at.elapsed() >= Duration::from_secs(5));
        let events_idle = mv.events_loading.is_none();
        if want_candles {
            self.eco.markets_view.candles_requested.insert((pool.address.clone(), bucket), Instant::now());
            self.send_data(DataCmd::PairCandles { pool: pool.address.clone(), bucket, count: MARKET_CANDLES });
        }
        if due && events_idle {
            self.eco.markets_view.events_loading = Some(pool.address.clone());
            self.eco.markets_view.events_at.insert(pool.address.clone(), (Instant::now(), since));
            self.send_data(DataCmd::PoolEvents { pool: Box::new(pool), since });
        }
    }

    /// Candles for the chart: the indexer's when it has this pool and timeframe, else the ones
    /// built from the pool's own logs. Both are display data, and they agree within a percent.
    pub fn chart_candles(
        &self,
        pool: &wallet_core::markets::Pool,
        base0: bool,
        bucket: u64,
        count: usize,
    ) -> Arc<Vec<wallet_core::markets::Candle>> {
        self.chart_candles_at(pool, base0, bucket, count, wallet_core::registry::now())
    }

    pub(crate) fn chart_candles_at(
        &self,
        pool: &wallet_core::markets::Pool,
        base0: bool,
        bucket: u64,
        count: usize,
        now: u64,
    ) -> Arc<Vec<wallet_core::markets::Candle>> {
        let key = self.eco.markets_view.derived_key(pool, base0, bucket, count, now);
        if let Some(found) = self.eco.markets_view.derived.borrow().candles.get(&key) {
            return found.clone();
        }
        let value = Arc::new(self.build_chart_candles(pool, base0, bucket, count, now));
        let mut cache = self.eco.markets_view.derived.borrow_mut();
        if cache.candles.len() >= 32 {
            cache.candles.clear();
        }
        cache.candles.insert(key, value.clone());
        value
    }

    pub fn market_stats(&self, pool: &wallet_core::markets::Pool, base0: bool, now: u64) -> wallet_core::markets::PairStats {
        let key = self.eco.markets_view.derived_key(pool, base0, 3600, 24, now);
        if let Some(found) = self.eco.markets_view.derived.borrow().stats.get(&key) {
            return found.clone();
        }
        let events = self.eco.markets_view.events.get(&pool.address).and_then(|v| v.as_ref().ok()).map_or(&[][..], Vec::as_slice);
        let value = wallet_core::markets::pair_stats(events, pool, base0, now);
        let mut cache = self.eco.markets_view.derived.borrow_mut();
        if cache.stats.len() >= 128 {
            cache.stats.clear();
        }
        cache.stats.insert(key, value.clone());
        value
    }

    pub fn market_trades(&self, pool: &wallet_core::markets::Pool, base0: bool) -> Arc<Vec<wallet_core::markets::Trade>> {
        let key = self.eco.markets_view.derived_key(pool, base0, 0, 0, 0);
        if let Some(found) = self.eco.markets_view.derived.borrow().trades.get(&key) {
            return found.clone();
        }
        let events = self.eco.markets_view.events.get(&pool.address).and_then(|v| v.as_ref().ok()).map_or(&[][..], Vec::as_slice);
        let value = Arc::new(wallet_core::markets::trades(events, pool, base0));
        let mut cache = self.eco.markets_view.derived.borrow_mut();
        if cache.trades.len() >= 8 {
            cache.trades.clear();
        }
        cache.trades.insert(key, value.clone());
        value
    }

    fn build_chart_candles(
        &self,
        pool: &wallet_core::markets::Pool,
        base0: bool,
        bucket: u64,
        count: usize,
        now: u64,
    ) -> Vec<wallet_core::markets::Candle> {
        let events = match self.eco.markets_view.events.get(&pool.address) {
            Some(Ok(ev)) => ev.as_slice(),
            _ => &[],
        };
        let local = wallet_core::markets::candles_in_zone(events, pool, base0, bucket, 0, now, count);
        let Some(indexed) = self.eco.markets_view.candles.get(&(pool.address.clone(), bucket)).filter(|v| !v.is_empty()) else {
            return local;
        };
        let fix = |v: f64| if !base0 && v > 0.0 { 1.0 / v } else { v };
        let mut merged: std::collections::BTreeMap<_, _> = indexed
            .iter()
            .map(|c| {
                (
                    c.start,
                    wallet_core::markets::Candle {
                        start: c.start,
                        open: fix(c.open),
                        close: fix(c.close),
                        high: if !base0 { fix(c.low) } else { c.high },
                        low: if !base0 { fix(c.high) } else { c.low },
                        volume: c.volume,
                        trades: c.trades,
                    },
                )
            })
            .collect();
        // A full local bucket replaces the indexed version, avoiding duplicate volume.
        // For a partial bucket retain indexed OHLC and update its last observed price only;
        // covered history, rather than receipt time, decides what can replace the indexer.
        let first_event = events.iter().map(|e| e.position().0).filter(|at| *at > 0).min();
        for c in local {
            if first_event.is_some_and(|at| at <= c.start) || !merged.contains_key(&c.start) {
                merged.insert(c.start, c);
            } else if let Some(held) = merged.get_mut(&c.start) {
                held.close = c.close;
                held.high = held.high.max(c.high);
                held.low = held.low.min(c.low);
            }
        }
        merged.into_values().rev().take(count).collect::<Vec<_>>().into_iter().rev().collect()
    }

    /// The DEX-wide tape, from one `quai_getLogs` over every pool. The first pass reads a window
    /// of blocks; later ones only what the chain added, so the request stays small.
    /// Live reserves for the pools on screen.
    ///
    /// This is the one market read that is not behind somebody else's cache: the explorer serves
    /// its pool page `max-age=30`, so no client-side tuning gets price or TVL under half a minute.
    /// The node has no such floor, and one multicall covers the whole directory.
    fn tick_reserves(&mut self) {
        let Some(Ok((pools, _))) = self.eco.markets_view.pools.as_ref().map(|r| r.as_ref()) else { return };
        if pools.is_empty() {
            return;
        }
        let mv = &self.eco.markets_view;
        if mv.reserves_loading || mv.reserves_attempted.is_some_and(|t| t.elapsed() < MARKET_REFRESH) {
            return;
        }
        let pools = pools.clone();
        self.eco.markets_view.reserves_loading = true;
        self.eco.markets_view.reserves_attempted = Some(Instant::now());
        self.send_data(DataCmd::PoolReserves { pools });
    }

    fn tick_dex_flow(&mut self) {
        use wallet_core::markets::FLOW_BLOCKS;
        let Some(Ok((pools, _))) = &self.eco.markets_view.pools else { return };
        if pools.is_empty() {
            return;
        }
        let mv = &self.eco.markets_view;
        if mv.flow_loading || mv.flow_at.is_some_and(|t| t.elapsed() < MARKET_REFRESH) {
            return;
        }
        let pools = pools.clone();
        self.eco.markets_view.flow_loading = true;
        self.send_data(DataCmd::DexFlow { pools, blocks: FLOW_BLOCKS });
    }

    // -------------------------------------------------------------------- board

    /// The board's left column: the channels this wallet follows, the people it can write to in
    /// private, then the channels seen on the board that it does not follow. A typed filter
    /// narrows all three by name.
    pub fn board_rows(&self) -> Vec<BoardRow> {
        let followed = &self.config.board_channels;
        let mut rows: Vec<BoardRow> = followed.iter().cloned().map(BoardRow::Channel).collect();
        // Anyone we can hold a sealed conversation with: established payment channels, plus any
        // contact who has a payment code. A conversation needs only the two codes — waiting for a
        // payment channel to exist would hide messages that have already arrived.
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for p in &self.dash.peers {
            if seen.insert(p.code.clone()) {
                rows.push(BoardRow::Peer(p.code.clone(), p.contact.clone()));
            }
        }
        for c in &self.dash.contacts {
            if let Some(code) = c.payment_code.as_ref().filter(|code| seen.insert((*code).clone())) {
                rows.push(BoardRow::Peer(code.clone(), Some(c.name.clone())));
            }
        }
        rows.extend(
            self.eco
                .board
                .known
                .iter()
                .filter(|c| !followed.iter().any(|f| f == &c.name))
                .map(|c| BoardRow::Unfollowed(c.name.clone(), c.messages)),
        );
        let Some(filter) = self.eco.board.filter.as_ref().map(|f| f.trim().to_lowercase()).filter(|f| !f.is_empty()) else {
            return rows;
        };
        rows.retain(|r| match r {
            BoardRow::Channel(name) | BoardRow::Unfollowed(name, _) => name.to_lowercase().contains(&filter),
            // People match on the name you gave them, or on their code.
            BoardRow::Peer(code, contact) => {
                contact.as_ref().is_some_and(|c| c.to_lowercase().contains(&filter)) || code.to_lowercase().contains(&filter)
            }
        });
        rows
    }

    /// A contact's name for a Quai address, so the board reads as people rather than hex.
    ///
    /// Matches the contact's own address and any address the payment-channel scan has attributed
    /// to a peer, since a peer posting from a fresh payment address is still that peer.
    pub fn contact_name_for(&self, address: &str) -> Option<String> {
        let a = address.to_lowercase();
        if let Some(c) = self.dash.contacts.iter().find(|c| c.address.as_ref().is_some_and(|x| x.to_lowercase() == a)) {
            return Some(c.name.clone());
        }
        // Any other account the same person has written from.
        self.dash.contact_addresses.iter().find(|(addr, _)| *addr == a).map(|(_, name)| name.clone())
    }

    /// A board row as a chat target (`#channel` / `dm:<code>`) and how it reads.
    pub fn chat_target(row: &BoardRow) -> (String, String) {
        match row {
            BoardRow::Channel(c) | BoardRow::Unfollowed(c, _) => (wallet_core::chat::channel_target(c), format!("#{c}")),
            BoardRow::Peer(code, name) => {
                (wallet_core::chat::dm_target(code), name.clone().unwrap_or_else(|| wallet_core::session::short_code(code)))
            }
        }
    }

    /// How a chat target reads: `#general`, or the contact's name for a conversation.
    pub fn chat_label(&self, target: &str) -> String {
        match target.strip_prefix("dm:") {
            Some(code) => self
                .dash
                .contacts
                .iter()
                .find(|c| c.payment_code.as_deref() == Some(code))
                .map(|c| c.name.clone())
                .unwrap_or_else(|| wallet_core::session::short_code(code)),
            None => target.to_string(),
        }
    }

    /// Keep the pinned chat current wherever the user is, and check subscriptions every half
    /// minute while no daemon does (two checkers would each notify).
    pub(crate) fn tick_chat(&mut self) {
        if self.locked || self.meta.is_none() || !self.config.features.messaging {
            return;
        }
        if !self.eco.board.chat_loaded {
            self.eco.board.chat_loaded = true;
            self.send(Cmd::Chat(super::worker::ChatOp::Load));
            return;
        }
        if let Some(pin) = self.eco.board.pin.clone()
            && self.screen != Screen::Board
        {
            let blocks = wallet_core::messages::BOARD_BLOCKS;
            match pin.strip_prefix("dm:") {
                Some(code) => {
                    let b = &self.eco.board;
                    if b.dm_loading.is_none() && b.dm_at.get(code).is_none_or(|t| t.elapsed() >= Duration::from_secs(10)) {
                        self.eco.board.dm_loading = Some(code.to_string());
                        self.send(Cmd::ReadConversation { peer: code.to_string(), blocks });
                    }
                }
                None => {
                    let channel = pin.trim_start_matches('#').to_string();
                    let b = &self.eco.board;
                    if b.loading.is_none() && b.at.get(&channel).is_none_or(|t| t.elapsed() >= Duration::from_secs(10)) {
                        self.eco.board.loading = Some(channel.clone());
                        self.send_data(DataCmd::Board { channel, blocks });
                    }
                }
            }
        }
        if !self.eco.board.subs.is_empty() && self.eco.board.news_checked.is_none_or(|t| t.elapsed() >= Duration::from_secs(30)) {
            self.eco.board.news_checked = Some(Instant::now());
            // A daemon reads the channels; it reads this wallet's sealed chats too only if it
            // holds the wallet unlocked. Whatever it cannot read, this window does.
            let id = self.meta.as_ref().map(|m| m.id.clone()).unwrap_or_default();
            match crate::daemon::state(&self.paths) {
                None => self.send(Cmd::ChatNews { dms_only: false }),
                Some(d) if !d.unlocked(&id) => self.send(Cmd::ChatNews { dms_only: true }),
                Some(_) => {}
            }
        }
    }

    /// Whether Tab, pressed now, would wrap back to the start of this screen: the pinned chat
    /// is the next stop instead.
    pub(crate) fn tab_reaches_dock(&self) -> bool {
        if let Some(d) = self.detail.last() {
            return !matches!(d, super::app::Detail::Collection(_));
        }
        match self.screen {
            // Exchange cards: from their last field (an unfocused card takes Tab to enter).
            Screen::Swap => self.eco.swap.field == 4,
            Screen::Convert => self.eco.convert.field == 3,
            Screen::Wrap => self.eco.wrap.field == 1,
            Screen::Pools if self.eco.pools_view.add.is_some() => self.eco.pools_view.add.as_ref().is_some_and(|a| a.field == 3),
            // A typed filter keeps its Tab.
            Screen::Board if self.eco.board.filter.is_some() => false,
            s => self.pane + 1 >= s.panes().max(1),
        }
    }

    /// Keys while the pinned chat has the keyboard: type, Enter to post (the review still
    /// decides), Tab or Esc to go back to the screen with the draft kept.
    pub(crate) fn dock_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::BackTab => self.dock_focus = false,
            KeyCode::Tab => {
                self.dock_focus = false;
                // On round to the screen's first stop, as its own Tab would.
                if !self.view_key(key) {
                    self.pane = 0;
                }
            }
            KeyCode::Enter => self.post_dock_draft(),
            KeyCode::Backspace => {
                self.dock_draft.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => self.dock_draft.clear(),
            // The chain takes 1024 bytes; the box stops where the post would be refused.
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) && self.dock_draft.len() + c.len_utf8() <= 1024 => {
                self.dock_draft.push(c);
            }
            _ => {}
        }
    }

    /// Post what is in the pinned chat's box: the same form and review as `p` on the Board, with
    /// the text already in it.
    fn post_dock_draft(&mut self) {
        let text = self.dock_draft.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.write_pinned();
        let super::app::Modal::Form(mut form) = std::mem::replace(&mut self.modal, super::app::Modal::None) else { return };
        if let Some(field) = form.fields.iter_mut().find(|f| f.label.starts_with("Message")) {
            field.value = text;
        }
        form.focus = form.fields.len() - 1;
        self.modal = self.form_key(form, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        // Sent for review: the box empties, and focus stays in the chat for the next line.
        if matches!(&self.modal, super::app::Modal::Form(f) if f.pending) || matches!(self.modal, super::app::Modal::None) {
            self.dock_draft.clear();
        }
    }

    /// Write to the pinned chat, from any screen.
    pub fn write_pinned(&mut self) {
        let Some(pin) = self.eco.board.pin.clone() else {
            return self.toast("pin a chat first: Board (4 ]]), then P", true);
        };
        if !self.can_sign() {
            return self.toast("this wallet is watch-only", true);
        }
        match pin.strip_prefix("dm:") {
            Some(code) => {
                let name = Some(self.chat_label(&pin));
                self.open_form(FormKind::BoardDm { peer: code.to_string(), name });
            }
            None => self.open_form(FormKind::BoardPost { channel: pin.trim_start_matches('#').to_string() }),
        }
    }

    /// The row under the cursor.
    pub fn board_row(&self) -> Option<BoardRow> {
        let rows = self.board_rows();
        let i = if self.pane == 1 { self.eco.board_channel_selected } else { self.selected };
        rows.get(i.min(rows.len().saturating_sub(1))).cloned()
    }

    /// The channel under the cursor, followed or merely seen.
    pub fn board_channel(&self) -> Option<String> {
        match self.board_row() {
            Some(BoardRow::Channel(c)) | Some(BoardRow::Unfollowed(c, _)) => Some(c),
            _ => None,
        }
    }

    /// Messages in the open channel, oldest first — a conversation reads downwards.
    pub fn board_posts(&self) -> Vec<&wallet_core::messages::Post> {
        let Some(channel) = self.board_channel() else { return Vec::new() };
        match self.eco.board.posts.get(&channel) {
            Some(Ok(posts)) => posts.iter().rev().collect(),
            _ => Vec::new(),
        }
    }

    /// The open conversation's messages, oldest first.
    pub fn board_dm_lines(&self) -> Vec<&wallet_core::ops::SealedLine> {
        let Some(BoardRow::Peer(code, _)) = self.board_row() else { return Vec::new() };
        match self.eco.board.dms.get(&code) {
            Some(Ok(lines)) => lines.iter().collect(),
            _ => Vec::new(),
        }
    }

    /// The address that wrote the selected message, when the messages pane has the cursor.
    pub fn board_sender(&self) -> Option<String> {
        if self.pane != 1 {
            return None;
        }
        match self.board_row() {
            Some(BoardRow::Peer(..)) => self.board_dm_lines().get(self.selected).map(|l| l.from.clone()),
            _ => self.board_posts().get(self.selected).map(|p| p.from.clone()),
        }
    }

    /// Rows in whichever the cursor has open, for the message pane's own cursor.
    pub fn board_message_count(&self) -> usize {
        match self.board_row() {
            Some(BoardRow::Peer(..)) => self.board_dm_lines().len(),
            _ => self.board_posts().len(),
        }
    }

    /// Re-read whatever the board has open every 10 s. A channel is a plain log query and goes
    /// to the data worker; a conversation needs this wallet's payment key, so it goes to the
    /// wallet worker, which is the only one holding keys.
    fn tick_board(&mut self) {
        let blocks = wallet_core::messages::BOARD_BLOCKS;
        match self.board_row() {
            Some(BoardRow::Channel(channel)) | Some(BoardRow::Unfollowed(channel, _)) => {
                let board = &self.eco.board;
                let fresh = board.at.get(&channel).is_some_and(|t| t.elapsed() < Duration::from_secs(10));
                if board.loading.is_some() || fresh {
                    return;
                }
                self.eco.board.loading = Some(channel.clone());
                self.send_data(DataCmd::Board { channel, blocks });
            }
            Some(BoardRow::Peer(code, _)) => {
                if self.locked {
                    return;
                }
                let board = &self.eco.board;
                let fresh = board.dm_at.get(&code).is_some_and(|t| t.elapsed() < Duration::from_secs(10));
                if board.dm_loading.is_some() || fresh {
                    return;
                }
                self.eco.board.dm_loading = Some(code.clone());
                self.send(Cmd::ReadConversation { peer: code, blocks });
            }
            None => {}
        }
    }

    /// Scan the board for what is on it, wherever the user happens to be. One `quai_getLogs`
    /// covers every channel at once, so following a channel means something on any screen: often
    /// while the board is open, rarely otherwise.
    fn tick_board_watch(&mut self) {
        if self.locked || !self.config.features.messaging || self.config.board_channels.is_empty() {
            return;
        }
        if self.config.network(&self.network_id).is_ok_and(|n| n.ecosystem.messages.is_none()) {
            return;
        }
        let looking = self.screen == Screen::Board;
        let every = Duration::from_secs(if looking { 10 } else { 45 });
        let board = &self.eco.board;
        if board.known_loading || board.known_at.is_some_and(|t| t.elapsed() < every) {
            return;
        }
        self.eco.board.known_loading = true;
        self.eco.board.known_at = Some(Instant::now());
        self.send_data(DataCmd::BoardChannels { blocks: wallet_core::messages::BOARD_BLOCKS });
    }

    /// Say what arrived in the followed channels. The first scan of a session only records
    /// where the board stands: opening the wallet is not news, everything after it is.
    fn announce_board(&mut self, seed: bool) {
        let followed = self.config.board_channels.clone();
        if seed {
            for channel in &followed {
                self.mark_board_seen(channel);
            }
            return;
        }
        // The channel on screen is being read, so it is caught up rather than announced.
        let open = (self.screen == Screen::Board).then(|| self.board_channel()).flatten();
        let mut arrived: Vec<(String, u32)> = Vec::new();
        for channel in followed {
            if open.as_deref() == Some(channel.as_str()) {
                self.mark_board_seen(&channel);
                continue;
            }
            match self.board_unread(&channel) {
                0 => {}
                n => arrived.push((channel, n)),
            }
        }
        // Only newly arrived messages are worth a toast; a count that has not moved is not news.
        let mut announce: Vec<String> = Vec::new();
        for (channel, n) in arrived {
            if self.eco.board.announced.get(&channel).copied().unwrap_or(0) < n {
                announce.push(if n == 1 { format!("1 new message in #{channel}") } else { format!("{n} new messages in #{channel}") });
            }
            self.eco.board.announced.insert(channel, n);
        }
        for text in announce {
            self.toast(text, false);
        }
    }

    /// Messages in a followed channel newer than the one last looked at.
    pub fn board_unread(&self, channel: &str) -> u32 {
        let Some(seen) = self.eco.board.seen.get(channel) else { return 0 };
        self.eco.board.known.iter().find(|c| c.name == channel).map_or(0, |c| c.recent_blocks.iter().filter(|b| *b > seen).count() as u32)
    }

    /// Everything on the board right now counts as looked at: what arrives afterwards is news,
    /// what was already there is not.
    fn mark_board_seen(&mut self, channel: &str) {
        let newest = self.eco.board.known.iter().find(|c| c.name == channel).map_or(0, |c| c.last_block);
        if newest > 0 {
            self.eco.board.seen.insert(channel.to_string(), newest);
        }
    }

    /// Follow a channel by name. Following is local: it only decides what this wallet lists,
    /// and a channel exists on the board whether anyone follows it or not.
    pub fn follow_channel(&mut self, name: &str) {
        let name = name.trim().to_string();
        if let Err(e) = wallet_core::messages::channel_tag(&name) {
            return self.toast(super::app::friendly_error(&e.to_string()), true);
        }
        if self.config.board_channels.iter().any(|c| c == &name) {
            return self.toast(format!("already following #{name}"), false);
        }
        self.config.board_channels.push(name.clone());
        self.save_config();
        self.toast(format!("following #{name}"), false);
    }

    fn board_key(&mut self, key: KeyEvent) -> bool {
        // While the filter is open it takes the typing, so a name with p, a or x in it is not
        // read as a command. Esc closes it and shows everything again.
        if let Some(filter) = self.eco.board.filter.as_mut() {
            match key.code {
                KeyCode::Esc => {
                    self.eco.board.filter = None;
                    self.selected = 0;
                    return true;
                }
                KeyCode::Enter | KeyCode::Down | KeyCode::Up | KeyCode::Tab | KeyCode::BackTab => {}
                KeyCode::Backspace => {
                    filter.pop();
                    self.selected = 0;
                    return true;
                }
                KeyCode::Char(c) => {
                    filter.push(c);
                    self.selected = 0;
                    return true;
                }
                _ => return false,
            }
        }
        match key.code {
            // The cursor belongs to one pane at a time, as on the markets screen.
            KeyCode::Tab | KeyCode::BackTab => {
                if self.pane == 0 {
                    self.eco.board_channel_selected = self.selected;
                } else {
                    self.eco.board_post_selected = self.selected;
                }
                self.pane = 1 - self.pane;
                self.selected = if self.pane == 0 {
                    self.eco.board_channel_selected
                } else {
                    self.eco.board_post_selected.min(self.board_message_count().saturating_sub(1))
                };
                true
            }
            KeyCode::Char('p') => {
                match self.board_row() {
                    Some(BoardRow::Channel(channel)) => self.open_form(FormKind::BoardPost { channel }),
                    Some(BoardRow::Unfollowed(channel, _)) => {
                        // Writing somewhere is reason enough to keep it in the list.
                        self.follow_channel(&channel);
                        self.open_form(FormKind::BoardPost { channel });
                    }
                    Some(BoardRow::Peer(code, name)) => self.open_form(FormKind::BoardDm { peer: code, name }),
                    None => self.toast("add a channel first (a)", true),
                }
                true
            }
            // On a channel the board knows but this wallet does not follow, `a` takes it; other-
            // wise it asks for a name, which is all it takes to start one.
            KeyCode::Char('a') => {
                match self.board_row() {
                    Some(BoardRow::Unfollowed(channel, _)) => self.follow_channel(&channel),
                    _ => self.open_form(FormKind::FollowChannel),
                }
                true
            }
            KeyCode::Char('/') => {
                self.eco.board.filter = Some(String::new());
                self.selected = 0;
                true
            }
            KeyCode::Char('x') => {
                let i = if self.pane == 1 { self.eco.board_channel_selected } else { self.selected };
                // Only a channel is followed; a person is here because they are a payment peer.
                if i < self.config.board_channels.len() {
                    let name = self.config.board_channels.remove(i);
                    self.save_config();
                    self.selected = self.selected.min(self.list_len().saturating_sub(1));
                    self.toast(format!("unfollowed #{name}"), false);
                }
                true
            }
            // Pin the chat beside every screen (again unpins it).
            KeyCode::Char('P') => {
                if let Some(row) = self.board_row() {
                    let (target, label) = Self::chat_target(&row);
                    let unpin = self.eco.board.pin.as_deref() == Some(target.as_str());
                    self.send(Cmd::Chat(super::worker::ChatOp::Pin { target: (!unpin).then_some(target), label }));
                }
                true
            }
            // Notify me when someone says something here (again stops).
            KeyCode::Char('n') => {
                if let Some(row) = self.board_row() {
                    let (target, label) = Self::chat_target(&row);
                    self.send(Cmd::Chat(super::worker::ChatOp::Toggle { target, label }));
                }
                true
            }
            KeyCode::Char('R') => {
                self.eco.board.at.clear();
                self.eco.board.dm_at.clear();
                self.eco.board.known_at = None;
                self.tick_board();
                true
            }
            // Whoever wrote the selected message, into the address book. In a sealed
            // conversation the payment code is known too — that is the identity, and the
            // address is merely the account this message came from.
            KeyCode::Char('c') if self.pane == 1 => {
                let sender = self.board_sender();
                match (self.board_row(), sender) {
                    (Some(BoardRow::Peer(code, _)), address) => {
                        let mine = self.owner_addresses();
                        let address = address.filter(|a| !mine.iter().any(|m| m.eq_ignore_ascii_case(a)));
                        self.open_form(FormKind::ContactFromPeer { code, address });
                    }
                    (_, Some(address)) => {
                        self.open_form(FormKind::Contact(None));
                        if let Modal::Form(f) = &mut self.modal {
                            f.fields[1].value = address;
                            f.focus = 0;
                        }
                    }
                    (_, None) => self.toast("select a message first (tab)", true),
                }
                true
            }
            _ => false,
        }
    }

    fn markets_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            // Watch the pair: it moves to the top and stays there.
            KeyCode::Char('w') if self.pane == 0 => {
                if let Some(pool) = self.selected_pool() {
                    let name = self.pair_name(&pool);
                    self.send_data(DataCmd::Alerts(super::data::AlertOp::ToggleWatch { pool: pool.address, name }));
                }
                true
            }
            // Set an alert on the pair, starting from its price now.
            KeyCode::Char('A') if self.pane == 0 => {
                if let Some(pool) = self.selected_pool() {
                    let base0 = self.pool_base0(&pool);
                    let name = self.pair_name(&pool);
                    let now = pool.spot_price().map(|p| if base0 { p } else { 1.0 / p });
                    self.open_form(super::app::FormKind::Alert { pool: pool.address.clone(), name, inverted: !base0 });
                    if let (super::app::Modal::Form(f), Some(p)) = (&mut self.modal, now) {
                        f.fields[1].value = format!("{p:.6}").trim_end_matches('0').trim_end_matches('.').to_string();
                    }
                }
                true
            }
            KeyCode::Char('T') => {
                let mv = &mut self.eco.markets_view;
                mv.timeframe = (mv.timeframe + 1) % wallet_core::markets::TIMEFRAMES.len();
                let label = wallet_core::markets::TIMEFRAMES[mv.timeframe].0;
                self.toast(format!("chart: {label} candles"), false);
                true
            }
            // Order by depth, then by how far the pair moved today.
            KeyCode::Char('L') => self.sort_markets(MarketSort::next_tvl),
            KeyCode::Char('M') => self.sort_markets(MarketSort::next_change),
            KeyCode::Char('f') => {
                if let Some(pool) = self.selected_pool() {
                    let flipped = &mut self.eco.markets_view.flipped;
                    if !flipped.remove(&pool.address) {
                        flipped.insert(pool.address);
                    }
                }
                true
            }
            KeyCode::Char('R') => {
                self.eco.markets_view.pools_at = None;
                self.eco.markets_view.events_at.clear();
                self.eco.markets_view.flow_at = None;
                self.tick_markets();
                true
            }
            // The cursor belongs to one pane at a time; each keeps its place while the other has it.
            KeyCode::Tab | KeyCode::BackTab => {
                let mv = &mut self.eco.markets_view;
                if self.pane == 0 {
                    mv.pair_selected = self.selected;
                } else {
                    mv.flow_selected = self.selected;
                }
                self.pane = 1 - self.pane;
                self.selected = if self.pane == 0 {
                    self.eco.markets_view.pair_selected
                } else {
                    self.eco.markets_view.flow_selected.min(self.flow_rows().len().saturating_sub(1))
                };
                true
            }
            // Dust is most of a busy tape: step the floor up until the trades that matter show.
            KeyCode::Char('m') => {
                let mv = &mut self.eco.markets_view;
                mv.flow_min_usd = match mv.flow_min_usd {
                    v if v < 1.0 => 1.0,
                    v if v < 10.0 => 10.0,
                    v if v < 100.0 => 100.0,
                    _ => 0.0,
                };
                let floor = mv.flow_min_usd;
                if self.pane == 1 {
                    self.selected = self.selected.min(self.flow_rows().len().saturating_sub(1));
                }
                self.toast(
                    if floor <= 0.0 { "flow: every swap".to_string() } else { format!("flow: swaps over {}", amount::usd(floor)) },
                    false,
                );
                true
            }
            KeyCode::Char('t') | KeyCode::Enter if self.pane == 1 => {
                // A swap in the flow names its pair: take the chart there.
                let Some(pool) = self.flow_rows().get(self.selected).map(|s| s.pool.clone()) else { return true };
                if let Some(Ok((pools, _))) = &self.eco.markets_view.pools {
                    match pools.iter().position(|p| p.address == pool) {
                        Some(i) => {
                            self.eco.markets_view.pair_selected = i;
                            let name = pools.get(i).map(|p| self.pair_name(p)).unwrap_or_default();
                            self.toast(format!("chart: {name}"), false);
                        }
                        None => self.toast("that pool is not in the directory", true),
                    }
                }
                true
            }
            KeyCode::Char('t') | KeyCode::Enter => {
                self.trade_selected_pool();
                true
            }
            _ => false,
        }
    }

    /// A pair as the lists name it, base first.
    pub fn pair_name(&self, pool: &wallet_core::markets::Pool) -> String {
        let (base, quote) = if self.pool_base0(pool) { (&pool.token0, &pool.token1) } else { (&pool.token1, &pool.token0) };
        format!("{}/{}", self.market_symbol(base), self.market_symbol(quote))
    }

    /// Open the swap card to buy the pair's base with its quote.
    fn trade_selected_pool(&mut self) {
        let Some(pool) = self.selected_pool() else { return };
        // A token on its bonding curve is bought on the curve, not through a router.
        if pool.venue == wallet_core::markets::Venue::Curve {
            if !self.can_sign() {
                self.toast("this wallet is watch-only", true);
                return;
            }
            let (token, symbol) = (pool.token0.address.clone(), pool.token0.symbol.clone());
            self.open_form(FormKind::CurveBuy { token, symbol, curve: pool.address.clone() });
            return;
        }
        let base0 = self.pool_base0(&pool);
        let (base, quote) = if base0 { (&pool.token0, &pool.token1) } else { (&pool.token1, &pool.token0) };
        let wquai = self.config.network(&self.network_id).ok().and_then(|n| n.wquai).map(|w| w.to_lowercase());
        let asset = |t: &wallet_core::markets::PoolToken| {
            if wquai.as_deref() == Some(t.address.as_str()) {
                SwapAsset::Quai
            } else {
                SwapAsset::Token { address: t.address.clone(), symbol: t.symbol.clone(), decimals: t.decimals }
            }
        };
        self.eco.swap.from = asset(quote);
        self.eco.swap.to = Some(asset(base));
        self.eco.swap.amount.clear();
        self.eco.swap.quote = None;
        self.eco.swap.field = 1;
        let text = format!("buy {} with {} · f flips to sell", self.market_symbol(base), self.market_symbol(quote));
        self.eco.markets_view.pair_selected = self.markets_pair();
        self.switch(Screen::Swap);
        self.toast(text, false);
    }

    // ------------------------------------------------------------------ signing sequences

    fn flow_db(&self) -> wallet_core::Result<wallet_core::appdb::AppDb> {
        let meta = self.meta.as_ref().ok_or_else(|| wallet_core::CoreError::Invalid("no wallet selected".into()))?;
        wallet_core::appdb::AppDb::open(&self.paths.wallet_dir(&meta.id).join("app.sqlite"))
    }

    /// Persist a non-secret checkpoint before requesting or exposing an executable review.
    pub(crate) fn checkpoint_flow(&mut self) -> bool {
        let Some(mut flow) = self.eco.flow.take() else { return true };
        let result = (|| -> wallet_core::Result<()> {
            let db = self.flow_db()?;
            let intent = serde_json::to_value(&flow)?;
            let mut plan = match flow.checkpoint.clone() {
                Some(plan) => plan,
                None => wallet_core::plans::TradePlan::new(
                    self.network_id.clone(),
                    self.meta.as_ref().unwrap().id.clone(),
                    flow.kind.label(),
                    intent.clone(),
                )?,
            };
            if flow.lease.is_none() {
                let directory = self.paths.wallet_dir(&plan.owner).join("plan-locks");
                flow.lease = Some(Arc::new(wallet_core::plans::claim(&directory, &plan.id)?));
            }
            let old = plan.clone();
            plan.intent = intent;
            plan.state =
                if flow.waiting.is_some() { wallet_core::plans::PlanState::Waiting } else { wallet_core::plans::PlanState::Review };
            for id in [&flow.review_op, &flow.waiting, &flow.last_operation].into_iter().flatten() {
                if !plan.operations.contains(id) {
                    plan.operations.push(id.clone());
                }
            }
            if plan.revision == 0 || plan != old {
                db.save_trade_plan(&mut plan)?;
            }
            flow.checkpoint = Some(plan);
            Ok(())
        })();
        self.eco.flow = Some(flow);
        if let Err(error) = result {
            if !self.locked {
                self.toast(format!("trade checkpoint failed: {error}; execution paused"), true);
            }
            return false;
        }
        true
    }

    fn close_flow_checkpoint(&mut self, state: wallet_core::plans::PlanState, reason: &str) {
        if !self.checkpoint_flow() {
            return;
        }
        let result = (|| -> wallet_core::Result<()> {
            let db = self.flow_db()?;
            if let Some(plan) = self.eco.flow.as_mut().and_then(|flow| flow.checkpoint.as_mut()) {
                plan.state = state;
                plan.reason = reason.into();
                if state == wallet_core::plans::PlanState::Waiting {
                    plan.intent["final_submitted"] = serde_json::json!(true);
                }
                db.save_trade_plan(plan)?;
            }
            Ok(())
        })();
        if let Err(error) = result
            && !self.locked
        {
            self.toast(format!("could not update trade checkpoint: {error}"), true);
        }
    }

    /// Explicitly resume the most recent unfinished trade, always through a new review.
    pub fn resume_trade_plan(&mut self) {
        if self.eco.flow.is_some() {
            self.toast("a trade is already active", true);
            return;
        }
        let result = (|| -> wallet_core::Result<Option<ResumableFlow>> {
            use wallet_core::plans::{PlanReadiness, PlanState};
            let db = self.flow_db()?;
            let Some(mut plan) = db
                .trade_plans(&self.network_id)?
                .into_iter()
                .filter(|plan| self.meta.as_ref().is_some_and(|meta| plan.owner == meta.id))
                .find(|plan| !matches!(plan.state, PlanState::Complete | PlanState::Cancelled))
            else {
                return Ok(None);
            };
            if self.meta.as_ref().is_none_or(|meta| plan.owner != meta.id) || plan.network != self.network_id {
                return Err(wallet_core::CoreError::Rejected("trade checkpoint belongs to another wallet or network".into()));
            }
            let lease = Arc::new(wallet_core::plans::claim(&self.paths.wallet_dir(&plan.owner).join("plan-locks"), &plan.id)?);
            if plan.intent["final_submitted"] == true {
                if plan.readiness(&db)? == PlanReadiness::ReviewRequired {
                    plan.state = PlanState::Complete;
                    plan.reason =
                        "final step receipt observed; provisional chain results remain subject to canonical reconciliation".into();
                    db.save_trade_plan(&mut plan)?;
                    return Ok(None);
                }
                return Err(wallet_core::CoreError::Invalid(
                    "final step still requires transaction reconciliation; refresh Activity".into(),
                ));
            }
            let mut flow: Flow = serde_json::from_value(plan.intent.clone())?;
            for op in db.operations_for_plan(&self.network_id, &plan.id)? {
                if !plan.operations.contains(&op.id) {
                    plan.operations.push(op.id.clone());
                    flow.review_op = Some(op.id);
                }
            }
            let mut submitted = None;
            if let Some(id) = flow.review_op.take() {
                let op = db
                    .operation(&id)?
                    .ok_or_else(|| wallet_core::CoreError::Invalid("review operation is missing; reconcile the wallet first".into()))?;
                if op.tx_hash.is_some() {
                    flow.review_op = Some(id.clone());
                    submitted = Some((id, op.kind));
                } else if op.status == wallet_core::appdb::OpStatus::Prepared {
                    let meta = self.meta.as_ref().unwrap().clone();
                    let mut session = wallet_core::session::Session::open(
                        self.registry.clone(),
                        self.config.clone(),
                        meta,
                        self.config.network(&self.network_id)?.clone(),
                    )?;
                    session.abandon(&id)?;
                    plan.operations.retain(|operation| operation != &id);
                }
            }
            if plan.readiness(&db)? == PlanReadiness::Stopped {
                return Err(wallet_core::CoreError::Invalid(
                    "a trade step failed or was refunded; inspect holdings and create a fresh trade".into(),
                ));
            }
            flow.requested = false;
            flow.lease = Some(lease);
            flow.checkpoint = Some(plan);
            Ok(Some((flow, submitted)))
        })();
        match result {
            Ok(Some((flow, submitted))) => {
                self.eco.flow = Some(flow);
                if let Some((id, kind)) = submitted {
                    self.flow_on_submitted(&id, &kind);
                }
                self.toast("trade restored; reconcile previous receipts and review each remaining step", false);
                self.advance_flow();
            }
            Ok(None) => self.toast("no unfinished trade remains", false),
            Err(error) => self.toast(error.to_string(), true),
        }
    }

    /// Start a sequence (replacing none: one runs at a time).
    pub fn start_flow(&mut self, kind: FlowKind) {
        if let Some(f) = &self.eco.flow {
            let text = format!("finish or cancel “{}” first", f.kind.label());
            self.toast(text, true);
            return;
        }
        self.eco.flow = Some(Flow {
            checkpoint: None,
            lease: None,
            kind,
            swapped: false,
            requested: false,
            review_op: None,
            waiting: None,
            last_operation: None,
            steps: 0,
            last_poll: Instant::now(),
        });
        if self.checkpoint_flow() {
            self.advance_flow();
        }
    }

    /// Drive the sequence: request the next review, or wait for the last step to confirm.
    /// Runs every frame, whatever screen is showing.
    pub fn advance_flow(&mut self) {
        let may_prepare = !self.locked && self.can_sign() && matches!(self.modal, Modal::None);
        let Some(mut flow) = self.eco.flow.clone() else {
            if may_prepare {
                self.maybe_prompt_claim();
            }
            return;
        };
        if flow.requested || flow.review_op.is_some() {
            return;
        }
        if let Some(op_id) = flow.waiting.clone() {
            use wallet_core::appdb::OpStatus;
            match self.dash.ops.iter().find(|o| o.id == op_id).map(|o| o.status) {
                Some(OpStatus::Confirmed | OpStatus::Settled) => {
                    if let FlowKind::Steps { prepare, .. } = &mut flow.kind
                        && let Prepare::Trading { intent } = prepare.as_mut()
                        && intent.has_more_allocations()
                        && let Some(op) = self.dash.ops.iter().find(|op| op.id == op_id && !wallet_core::flows::is_step_kind(&op.kind))
                    {
                        if op.kind == "wrap_qi" && op.status != OpStatus::Settled {
                            if flow.last_poll.elapsed() > Duration::from_secs(5) {
                                flow.last_poll = Instant::now();
                                self.send(Cmd::Refresh { full: false });
                            }
                            self.eco.flow = Some(flow);
                            return;
                        }
                        match intent.advance_allocation(op) {
                            Ok(true) => {}
                            Ok(false) => {
                                let residual = match &intent.action {
                                    wallet_core::execution::TradingAction::MarketConversion { residual_atoms, .. } => {
                                        format!("market conversion complete; {} WQI atoms remain below one redeemable Qi", residual_atoms)
                                    }
                                    _ => "trading steps complete".into(),
                                };
                                self.eco.flow = Some(flow);
                                self.close_flow_checkpoint(wallet_core::plans::PlanState::Complete, &residual);
                                self.eco.flow = None;
                                if !self.locked {
                                    self.toast(residual, false);
                                }
                                return;
                            }
                            Err(error) => {
                                if flow.last_poll.elapsed() > Duration::from_secs(5) {
                                    flow.last_poll = Instant::now();
                                    if !self.locked {
                                        self.toast(error.to_string(), false);
                                    }
                                    self.send(Cmd::Refresh { full: false });
                                }
                                self.eco.flow = Some(flow);
                                return;
                            }
                        }
                    }
                    flow.waiting = None;
                    if let FlowKind::Swap { .. } = flow.kind {
                        self.eco.swap.approving = false;
                    }
                }
                Some(OpStatus::Failed | OpStatus::Cancelled | OpStatus::Refunded | OpStatus::Replaced) => {
                    let text = format!("{} stopped: a step did not confirm", flow.kind.label());
                    self.eco.flow = None;
                    self.eco.swap.approving = false;
                    if !self.locked {
                        self.toast(text, true);
                    }
                    return;
                }
                _ => {
                    // Poll the node faster than the idle refresh while a step is confirming.
                    if flow.last_poll.elapsed() > Duration::from_secs(5) {
                        flow.last_poll = Instant::now();
                        self.send(Cmd::Refresh { full: false });
                    }
                    self.eco.flow = Some(flow);
                    return;
                }
            }
        }
        // The first swap of a two-exchange route confirmed: the second takes exactly what it paid.
        if matches!(&flow.kind, FlowKind::Swap { then: Some(NextSwap { first: Some(_), .. }), .. }) {
            if self.begin_second_swap(&mut flow) {
                let text = format!("first swap confirmed · reviewing the second: {}", flow.kind.label());
                if !self.locked {
                    self.toast(text, false);
                }
            } else {
                let label = flow.kind.label();
                let FlowKind::Swap { then: Some(next), .. } = &mut flow.kind else { return };
                if next.polls >= SECOND_SWAP_POLLS {
                    self.eco.flow = None;
                    if !self.locked {
                        self.toast(
                            format!(
                                "{label}: the first swap confirmed, but what it paid could not be read · finish the route from Trade › Swap"
                            ),
                            true,
                        );
                    }
                    return;
                }
                if flow.last_poll.elapsed() > Duration::from_secs(3) {
                    next.polls += 1;
                    flow.last_poll = Instant::now();
                    self.send(Cmd::Refresh { full: false });
                }
                self.eco.flow = Some(flow);
                return;
            }
        }
        if !may_prepare {
            self.eco.flow = Some(flow);
            self.checkpoint_flow();
            return;
        }
        let prepare = match &flow.kind {
            // Wrap what the pool needs, swap, then redeem WQUAI the swap paid out.
            FlowKind::Swap { account, prewrap: Some(missing), .. } => {
                Prepare::WrapQuai { account: account.clone(), amount: missing.clone() }
            }
            FlowKind::Swap { account, unwrap_after: true, .. } if flow.steps > 0 && self.swap_done(&flow) => {
                match self.receipt_output(flow.last_operation.as_deref()).filter(|v| !v.is_zero()) {
                    Some(atoms) => Prepare::UnwrapQuai { account: account.clone(), amount: wallet_core::amount::format_amount(atoms, 18) },
                    None => {
                        if flow.last_poll.elapsed() > Duration::from_secs(3) {
                            flow.last_poll = Instant::now();
                            self.send(Cmd::Refresh { full: false });
                        }
                        self.eco.flow = Some(flow);
                        return;
                    }
                }
            }
            FlowKind::Swap { account, from, to, amount, slippage, deadline, .. } => Prepare::SwapNext {
                account: account.clone(),
                from: from.clone(),
                to: to.clone(),
                amount: amount.clone(),
                slippage: *slippage,
                deadline: *deadline,
            },
            FlowKind::NftBuy { account, contract, token_id, price, .. } => Prepare::NftBuyNext {
                account: account.clone(),
                contract: contract.clone(),
                token_id: token_id.clone(),
                price: price.clone(),
            },
            FlowKind::NftList { account, contract, token_id, price, currency, .. } => Prepare::NftListNext {
                account: account.clone(),
                contract: contract.clone(),
                token_id: token_id.clone(),
                price: price.clone(),
                currency: currency.clone(),
            },
            FlowKind::Claim { account, .. } => Prepare::ClaimWqi { account: account.clone() },
            FlowKind::Steps { prepare, .. } => (**prepare).clone(),
        };
        flow.requested = true;
        self.eco.flow = Some(flow);
        if self.checkpoint_flow()
            && let Some(id) = self.eco.flow.as_ref().and_then(|flow| flow.checkpoint.as_ref()).map(|plan| plan.id.clone())
        {
            self.send(Cmd::Prepare(Prepare::InPlan { id, request: Box::new(prepare) }));
        }
    }

    /// Turn a two-exchange route whose first swap confirmed into its second swap, sized from
    /// exactly what the first paid out (recorded from its receipt). False while that output is not
    /// visible yet.
    pub(crate) fn begin_second_swap(&self, flow: &mut Flow) -> bool {
        let FlowKind::Swap { from, to, amount, unwrap_after, then, .. } = &mut flow.kind else { return false };
        let Some(next) = then.clone() else { return false };
        let Some(first) = next.first.as_deref() else { return false };
        let paid = self
            .dash
            .ops
            .iter()
            .find(|o| o.id == first)
            .and_then(|o| o.detail["actual_out"].as_str())
            .and_then(|v| U256::from_str_radix(v, 10).ok())
            .filter(|v| !v.is_zero());
        let Some(paid) = paid else { return false };
        *amount = amount::format_amount(paid, next.hub_decimals);
        *from = std::mem::replace(to, next.to);
        *unwrap_after = next.unwrap_after;
        *then = None;
        flow.swapped = false;
        true
    }

    /// A wrapped balance in atoms: WQI when `qi`, else WQUAI.
    fn wrapped_atoms(&self, qi: bool) -> u128 {
        let wrap = self.dash.wrap.as_ref();
        let value = if qi { wrap.and_then(|w| w.wqi_atoms.as_ref()) } else { wrap.and_then(|w| w.wquai_atoms.as_ref()) };
        value.and_then(|s| s.parse::<u128>().ok()).unwrap_or(0)
    }

    /// Whether this swap sequence already submitted its swap (the unwrap comes after).
    fn swap_done(&self, flow: &Flow) -> bool {
        flow.swapped
    }

    fn receipt_output(&self, op_id: Option<&str>) -> Option<U256> {
        let op = self.dash.ops.iter().find(|op| Some(op.id.as_str()) == op_id)?;
        if !matches!(op.status, wallet_core::appdb::OpStatus::Confirmed | wallet_core::appdb::OpStatus::Settled) {
            return None;
        }
        op.detail["actual_out"].as_str().and_then(|s| U256::from_str_radix(s, 10).ok())
    }

    /// Start the market route for the amount on the Convert card.
    pub fn start_qi_route(&mut self, direction: wallet_core::qi_market::Direction, amount: String, slippage: u16) {
        let Some(account) = self.dash.accounts.first().map(|a| a.address.clone()) else {
            self.toast("select a signing account", true);
            return;
        };
        let Some(wqi) = self.config.network(&self.network_id).ok().and_then(|n| n.wqi.clone()) else {
            self.toast("WQI is not configured", true);
            return;
        };
        let (pay, receive) = direction.assets();
        let label = format!("{amount} {pay} → {receive} through the market");
        let intent = wallet_core::execution::TradingIntent {
            account,
            max_fee: None,
            action: wallet_core::execution::TradingAction::MarketConversion {
                direction,
                amount,
                stage: 0,
                wqi,
                slippage,
                deadline: self.config.swap_deadline_minutes,
                residual_atoms: "0".into(),
            },
        };
        self.start_flow(FlowKind::Steps { prepare: Box::new(Prepare::Trading { intent }), label });
    }

    pub fn start_protocol_conversion(
        &mut self,
        direction: wallet_core::qi_market::Direction,
        amount: String,
        slippage: Option<u16>,
        account: Option<String>,
    ) {
        let Some(account) = account.or_else(|| self.dash.accounts.first().map(|a| a.address.clone())) else {
            self.toast("select a signing account", true);
            return;
        };
        let intent = wallet_core::execution::TradingIntent {
            account,
            max_fee: None,
            action: wallet_core::execution::TradingAction::ProtocolConversion { direction, amount, slippage },
        };
        self.start_flow(FlowKind::Steps { prepare: Box::new(Prepare::Trading { intent }), label: "protocol conversion".into() });
    }

    /// Settled wrapped Qi waiting for its claim: open the claim review (once per amount).
    fn maybe_prompt_claim(&mut self) {
        if !self.dash.unlocked {
            return;
        }
        let Some(qits) = self.dash.wrap.as_ref().and_then(|w| w.unclaimed_qits.clone()) else { return };
        if qits.parse::<u128>().map_or(true, |v| v == 0) || self.eco.claim_declined.as_deref() == Some(qits.as_str()) {
            return;
        }
        // A claim already on its way.
        if self.dash.ops.iter().any(|o| o.kind == "claim_wqi" && !o.status.is_terminal()) {
            return;
        }
        let account = self.dash.wrap.as_ref().map(|w| w.account.clone());
        self.toast(
            format!("{} Qi of wrapped Qi is ready · review the claim to receive WQI", amount::qi(qits.parse().unwrap_or_default())),
            false,
        );
        self.start_flow(FlowKind::Claim { account, qits });
    }

    /// Claim on request (Wrap card, palette): the same sequence the automatic prompt uses.
    pub fn claim_now(&mut self, account: Option<String>) {
        let qits = self.dash.wrap.as_ref().and_then(|w| w.unclaimed_qits.clone()).unwrap_or_default();
        if qits.parse::<u128>().map_or(true, |v| v == 0) {
            self.toast("nothing to claim yet · wrapped Qi can be claimed once the wrap settles", true);
            return;
        }
        self.eco.claim_declined = None;
        self.start_flow(FlowKind::Claim { account, qits });
    }

    /// A review arrived; attach it to the sequence that asked for it.
    pub fn flow_on_review(&mut self, op_id: &str) -> bool {
        if let Some(flow) = &mut self.eco.flow
            && flow.requested
        {
            flow.requested = false;
            flow.review_op = Some(op_id.to_string());
            flow.steps += 1;
        }
        self.checkpoint_flow()
    }

    /// The worker could not prepare the next step.
    pub fn flow_on_error(&mut self) {
        if let Some(flow) = &self.eco.flow
            && flow.requested
        {
            if let FlowKind::Claim { qits, .. } = &flow.kind {
                self.eco.claim_declined = Some(qits.clone());
            }
            self.close_flow_checkpoint(
                wallet_core::plans::PlanState::Paused,
                "execution paused; inspect completed assets and allowances before resuming",
            );
            self.eco.flow = None;
            self.eco.swap.approving = false;
        }
    }

    /// A review was rejected: the sequence ends (nothing further is signed).
    pub fn flow_on_rejected(&mut self, op_id: &str) {
        if let Some(flow) = &self.eco.flow
            && flow.review_op.as_deref() == Some(op_id)
        {
            let label = flow.kind.label();
            if let FlowKind::Claim { qits, .. } = &flow.kind {
                self.eco.claim_declined = Some(qits.clone());
            }
            self.close_flow_checkpoint(
                wallet_core::plans::PlanState::Paused,
                "execution paused; inspect completed assets and allowances before resuming",
            );
            self.eco.flow = None;
            self.eco.swap.approving = false;
            self.toast(format!("{label} cancelled; nothing further will be signed"), false);
        }
    }

    /// A step was signed and broadcast. Returns true when the sequence continues (so the caller
    /// shows a toast instead of the result dialog).
    pub fn flow_on_submitted(&mut self, op_id: &str, kind: &str) -> bool {
        let Some(mut flow) = self.eco.flow.clone() else { return false };
        if flow.review_op.as_deref() != Some(op_id) {
            return false;
        }
        flow.review_op = None;
        flow.last_operation = Some(op_id.to_string());
        if !wallet_core::flows::is_step_kind(kind)
            && let FlowKind::Steps { prepare, .. } = &flow.kind
            && let Prepare::Trading { intent } = prepare.as_ref()
            && intent.has_more_allocations()
        {
            flow.waiting = Some(op_id.to_string());
            flow.last_poll = Instant::now();
            self.eco.flow = Some(flow);
            self.checkpoint_flow();
            self.toast("step submitted; the next review waits for its attributed receipt and required settlement", false);
            return true;
        }
        // The first of two swaps: wait for it, then size the second from what it paid.
        if kind == "swap"
            && let FlowKind::Swap { then: Some(next), .. } = &mut flow.kind
            && next.first.is_none()
        {
            next.first = Some(op_id.to_string());
            flow.waiting = Some(op_id.to_string());
            flow.last_poll = Instant::now();
            let label = flow.kind.label();
            self.eco.flow = Some(flow);
            self.checkpoint_flow();
            self.toast(format!("{label}: first swap sent · the second review opens when it confirms"), false);
            return true;
        }
        // The pre-wrap and the swap both continue the sequence.
        if let FlowKind::Swap { prewrap, unwrap_after, .. } = &mut flow.kind {
            let wrapped = kind == "wrap_quai" && prewrap.is_some();
            if wrapped {
                *prewrap = None;
            }
            let swapped = kind == "swap" && *unwrap_after;
            if wrapped || swapped {
                flow.swapped |= swapped;
                flow.waiting = Some(op_id.to_string());
                flow.last_poll = Instant::now();
                let label = flow.kind.label();
                let note = if wrapped {
                    "wrapped · the swap review opens when it confirms"
                } else {
                    "swapped · the redemption review opens when it confirms"
                };
                self.eco.flow = Some(flow);
                self.checkpoint_flow();
                self.toast(format!("{label}: {note}"), false);
                return true;
            }
        }
        if wallet_core::flows::is_step_kind(kind) {
            if kind == "approve"
                && let FlowKind::Swap { .. } = flow.kind
            {
                self.eco.swap.approving = true;
            }
            flow.waiting = Some(op_id.to_string());
            flow.last_poll = Instant::now();
            let label = flow.kind.label();
            self.eco.flow = Some(flow);
            self.checkpoint_flow();
            let step = if kind == "approve" { "approval" } else { "wrap" };
            self.toast(format!("{step} sent · {label} continues when it confirms (you can keep using the wallet)"), false);
            true
        } else {
            flow.waiting = Some(op_id.to_string());
            self.eco.flow = Some(flow);
            self.checkpoint_flow();
            self.close_flow_checkpoint(
                wallet_core::plans::PlanState::Waiting,
                "final step submitted; reconcile its receipt before marking complete",
            );
            self.eco.flow = None;
            false
        }
    }

    /// Locking drops open reviews; ask again after unlocking.
    pub fn flow_on_lock(&mut self) {
        if self.committing_kind.is_some() {
            return;
        }
        if let Some(flow) = &mut self.eco.flow {
            flow.requested = false;
            flow.review_op = None;
        }
    }

    /// Picker choice.
    pub fn pick_swap_asset(&mut self, pay: bool, asset: SwapAsset) {
        if let SwapAsset::Token { address, decimals: UNKNOWN_DECIMALS, .. } = &asset {
            self.send_data(DataCmd::TokenInfo(address.clone()));
        }
        let card = &mut self.eco.swap;
        if pay {
            if card.to.as_ref() == Some(&asset) {
                card.to = Some(card.from.clone());
            }
            card.from = asset;
            card.amount.clear();
        } else {
            if card.from == asset {
                card.from = card.to.clone().unwrap_or(SwapAsset::Quai);
            }
            card.to = Some(asset);
        }
        card.quote = None;
        card.approving = false;
        card.edited = Some(Instant::now());
        card.field = 1;
    }

    /// Unfocus exchange cards when their view is opened by navigation.
    pub fn unfocus_cards(&mut self) {
        self.eco.swap.field = 5;
        self.eco.convert.field = 4;
        self.eco.wrap.field = 2;
    }

    fn convert_key(&mut self, key: KeyEvent) -> bool {
        if key.code == KeyCode::Char('m') {
            self.fill_max();
            return true;
        }
        let card = &mut self.eco.convert;
        if card.field >= 4 {
            match key.code {
                KeyCode::Tab | KeyCode::Enter => card.field = 1,
                KeyCode::BackTab => card.field = 3,
                KeyCode::Char('f') => {
                    card.qi_to_quai = !card.qi_to_quai;
                    card.amount.clear();
                    card.quote = None;
                    card.routes = None;
                }
                KeyCode::Char('r') => card.market = !card.market,
                _ => return false,
            }
            return true;
        }
        match key.code {
            KeyCode::Esc => card.field = 4,
            KeyCode::Tab => card.field = (card.field + 1) % 4,
            KeyCode::BackTab => card.field = (card.field + 3) % 4,
            KeyCode::Down if card.field < 3 => card.field += 1,
            KeyCode::Up if card.field > 0 => card.field -= 1,
            KeyCode::Char('f') => {
                card.qi_to_quai = !card.qi_to_quai;
                card.amount.clear();
                card.quote = None;
                card.routes = None;
            }
            KeyCode::Char('r') => card.market = !card.market,
            KeyCode::Left | KeyCode::Right if card.field == 0 => {
                card.qi_to_quai = !card.qi_to_quai;
                card.amount.clear();
                card.quote = None;
                card.routes = None;
            }
            KeyCode::Left | KeyCode::Right if card.field == 3 => card.market = !card.market,
            KeyCode::Left | KeyCode::Right if card.field == 2 => {
                // Up to the 9000 the node clamps to: a saturated conversion needs the maximum, and
                // before this the arrows could not reach past 20%.
                let steps = [50u16, 100, 200, 300, 500, 1000, 2000, 3000, 5000, 9000];
                let i = steps.iter().position(|s| *s >= card.slippage_bps).unwrap_or(3) as i32;
                let d = if key.code == KeyCode::Left { -1 } else { 1 };
                card.slippage_bps = steps[(i + d).clamp(0, steps.len() as i32 - 1) as usize];
                card.manual_slippage = true;
            }
            KeyCode::Enter => {
                let direction = if card.qi_to_quai { "qi_to_quai" } else { "quai_to_qi" };
                let (qi_to_quai, amount, slippage) =
                    (card.qi_to_quai, card.amount.clone(), card.manual_slippage.then_some(card.slippage_bps));
                if amount.is_empty() {
                    self.toast("enter an amount", true);
                    return true;
                }
                // The market route: wrap, swap and unwrap, each step reviewed.
                if card.market {
                    let usable = match &card.routes {
                        Some(Ok(c)) => c.market.usable(),
                        _ => false,
                    };
                    if !usable {
                        self.toast("no market route for this amount yet", true);
                        return true;
                    }
                    if !self.can_sign() {
                        self.toast("this wallet is watch-only", true);
                        return true;
                    }
                    let direction =
                        wallet_core::qi_market::Direction::parse(direction).unwrap_or(wallet_core::qi_market::Direction::QuaiToQi);
                    let slippage = self.swap_slippage();
                    self.start_qi_route(direction, amount, slippage);
                    return true;
                }
                let current = card.quoted_for.as_ref() == Some(&(qi_to_quai, amount.clone())) && card.quote.is_some();
                if !current {
                    card.quoted_for = Some((qi_to_quai, amount.clone()));
                    card.quote = None;
                    self.send(Cmd::Quote { direction: direction.into(), amount });
                } else if !self.can_sign() {
                    self.toast("this wallet is watch-only", true);
                } else {
                    let account = self.dash.accounts.first().map(|a| a.address.clone());
                    let direction =
                        if qi_to_quai { wallet_core::qi_market::Direction::QiToQuai } else { wallet_core::qi_market::Direction::QuaiToQi };
                    self.start_protocol_conversion(direction, amount, slippage, account);
                }
            }
            _ if card.field != 1 => return false,
            _ => {
                let decimals = if card.qi_to_quai { 3 } else { 18 };
                if digits_input(&mut card.amount, &key, decimals) {
                    card.field = 1;
                    card.quote = None;
                    return true;
                }
                return false;
            }
        }
        true
    }

    fn wrap_key(&mut self, key: KeyEvent) -> bool {
        if key.code == KeyCode::Char('m') && self.eco.wrap.mode != 1 {
            self.fill_max();
            return true;
        }
        let card = &mut self.eco.wrap;
        if card.field >= 2 {
            match key.code {
                KeyCode::Tab | KeyCode::Enter => card.field = if card.mode == 1 { 0 } else { 1 },
                KeyCode::BackTab => card.field = 0,
                _ => return false,
            }
            return true;
        }
        match key.code {
            KeyCode::Esc => card.field = 2,
            KeyCode::Tab | KeyCode::BackTab => card.field = 1 - card.field.min(1),
            KeyCode::Down if card.field == 0 => card.field = 1,
            KeyCode::Up if card.field == 1 => card.field = 0,
            KeyCode::Left | KeyCode::Right if card.field == 0 => {
                let d: i32 = if key.code == KeyCode::Left { -1 } else { 1 };
                card.mode = (card.mode as i32 + d).rem_euclid(WRAP_MODES.len() as i32) as usize;
                card.amount.clear();
            }
            KeyCode::Enter => {
                let (mode, amount) = (card.mode, card.amount.clone());
                if !self.can_sign() {
                    self.toast("this wallet is watch-only", true);
                    return true;
                }
                if mode != 1 && amount.is_empty() {
                    self.toast("enter an amount", true);
                    return true;
                }
                let account = self.dash.accounts.first().map(|a| a.address.clone());
                if mode == 1 {
                    self.claim_now(account);
                    return true;
                }
                let prepare = match mode {
                    0 => Prepare::WrapQi { account, amount },
                    2 => Prepare::UnwrapWqi { account, amount },
                    3 => Prepare::WrapQuai { account, amount },
                    _ => Prepare::UnwrapQuai { account, amount },
                };
                self.send(Cmd::Prepare(prepare));
            }
            _ => {
                if card.mode == 1 || card.field != 1 {
                    return false;
                }
                let decimals = if matches!(card.mode, 0 | 2) { 3 } else { 18 };
                if digits_input(&mut card.amount, &key, decimals) {
                    card.field = 1;
                    return true;
                }
                return false;
            }
        }
        true
    }

    fn explore_key(&mut self, key: KeyEvent) -> bool {
        if let Some(text) = &mut self.eco.search {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => {
                    self.eco.search_text = text.clone();
                    self.eco.search = None;
                    self.selected = 0;
                }
                KeyCode::Backspace => {
                    text.pop();
                    self.eco.search_text = text.clone();
                    self.selected = 0;
                }
                KeyCode::Char(c) if text.len() < 40 => {
                    text.push(c);
                    self.eco.search_text = text.clone();
                    self.selected = 0;
                }
                _ => {}
            }
            return true;
        }
        match key.code {
            KeyCode::Char('/') => {
                self.eco.search = Some(self.eco.search_text.clone());
                true
            }
            KeyCode::Char('R') => {
                self.eco.collections_loading = true;
                self.send_data(DataCmd::Collections { query: None });
                self.load_nft_market(true);
                true
            }
            KeyCode::Char('S') => {
                self.eco.collection_sort = self.eco.collection_sort.next();
                self.selected = 0;
                let by = self.eco.collection_sort.label();
                self.toast(format!("collections by {by}"), false);
                true
            }
            _ => false,
        }
    }

    /// Enter on ecosystem views.
    pub fn enter_eco(&mut self) {
        match self.screen {
            Screen::Home if self.pane == 1 => {
                if let Some(key) = self.activity_key(self.selected) {
                    self.push_detail(Detail::Activity(key));
                }
            }
            Screen::Home => {
                if let Some(row) = self.eco.portfolio.as_ref().and_then(|p| p.rows.get(self.selected)) {
                    self.push_detail(Detail::Asset(row.key.id()));
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
                    self.send_data(DataCmd::CollectionItems(c.clone()));
                }
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
                let network = self.config.network(&self.network_id).ok();
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

    fn transfer_selected_nft(&mut self) {
        if let Some(Ok(items)) = &self.eco.nfts
            && let Some(n) = items.get(self.selected)
        {
            let (c, id) = (n.item.contract.clone(), n.item.token_id.clone());
            self.open_nft_transfer(&c, &id);
        }
    }

    fn open_nft_transfer(&mut self, contract: &str, token_id: &str) {
        if !self.can_sign() {
            self.toast("this wallet is watch-only", true);
            return;
        }
        let held = matches!(&self.eco.nfts, Some(Ok(v)) if v.iter().any(|n| n.item.contract == contract && n.item.token_id == token_id));
        if !held {
            self.toast("this wallet does not hold that NFT", true);
            return;
        }
        let multi = matches!(&self.eco.nfts, Some(Ok(v)) if v.iter().any(|n| n.item.contract == contract && n.item.token_id == token_id && n.kind == wallet_core::explorer::TokenKind::Erc1155));
        self.open_form(FormKind::NftTransfer { contract: contract.to_string(), token_id: token_id.to_string(), multi });
    }

    /// Next step of buying the NFT in the top detail view (each step is its own review).
    pub fn buy_step(&mut self) {
        let Some(Detail::Nft(c, id)) = self.detail.last().cloned() else { return };
        if !self.can_sign() {
            self.toast("this wallet is watch-only", true);
            return;
        }
        let listing = self.listing_for(&c, &id);
        match listing {
            Some(l) if !l.buyable() => {
                self.toast("Seaport listing — press o to copy its Bazarr link", false);
                return;
            }
            None => {
                self.toast("this item is not listed", true);
                return;
            }
            _ => {}
        }
        let account = self.dash.accounts.first().map(|a| a.address.clone());
        match self.eco.asks.get(&(c.clone(), id.clone())) {
            None => self.toast("checking the listing on-chain…", false),
            Some(Err(e)) => {
                let e = e.clone();
                self.toast(e, true);
            }
            Some(Ok(check)) if !check.valid => {
                let problems = check.problems.join("; ");
                self.toast(format!("cannot buy: {problems}"), true);
            }
            Some(Ok(check)) => {
                let price = check.ask.as_ref().map(|a| a.price.clone());
                let name = self.listing_for(&c, &id).and_then(|l| l.name).unwrap_or_else(|| format!("#{id}"));
                self.start_flow(FlowKind::NftBuy { account, contract: c, token_id: id, price, label: format!("buy {name}") });
            }
        }
    }

    /// Buy a listing directly (from a listings table): the purchase sequence re-checks the ask
    /// on-chain before every step, at the price shown.
    pub fn buy_listing(&mut self, l: &Listing) {
        if !self.can_sign() {
            self.toast("this wallet is watch-only", true);
            return;
        }
        if !l.buyable() {
            self.toast("Seaport listing — press o to copy its Bazarr link", false);
            return;
        }
        let account = self.dash.accounts.first().map(|a| a.address.clone());
        let name = l.name.clone().unwrap_or_else(|| format!("#{}", l.token_id));
        self.start_flow(FlowKind::NftBuy {
            account,
            contract: l.contract.clone(),
            token_id: l.token_id.clone(),
            price: Some(l.price.clone()),
            label: format!("buy {name}"),
        });
    }

    /// Re-check asks and holdings after an NFT operation is submitted.
    pub fn after_submit(&mut self, kind: &str) {
        match kind {
            "approve" if self.screen == Screen::Swap => {
                self.eco.swap.approving = true;
                self.eco.swap.quoted_at = Some(Instant::now());
            }
            "approve" | "nft_buy" | "nft_transfer" => {
                if let Some(Detail::Nft(c, id)) = self.detail.last().cloned() {
                    let buyer = self.dash.accounts.first().map(|a| a.address.clone());
                    self.send_data(DataCmd::CheckAsk { contract: c, token_id: id, buyer });
                }
                // Holdings reload when the operation confirms (see `after_confirm`): reloading
                // now would cache a list from before the transfer was mined.
                if kind != "approve" {
                    self.eco.listings.clear();
                }
            }
            "swap" => {
                self.eco.swap.amount.clear();
                self.eco.swap.quote = None;
                self.eco.portfolio_signature = None;
            }
            _ => {}
        }
    }

    /// Reload what a confirmed operation changed.
    pub fn after_confirm(&mut self, kind: &str) {
        if matches!(kind, "nft_list" | "nft_reprice" | "nft_unlist") {
            self.eco.listings.clear();
            self.load_my_listings();
        }
        if matches!(kind, "nft_buy" | "nft_transfer") {
            self.eco.listings.clear();
            if self.screen == Screen::Collected || self.eco.nfts.is_some() {
                self.load_nfts(true);
            } else {
                self.eco.nfts = None;
            }
        }
    }

    /// Icon URL for a token contract, from the portfolio or market data; `quai` and `qi` get the
    /// bundled logos.
    pub fn asset_icon_url(&self, contract: &str) -> Option<String> {
        if let Some(native) = wallet_core::media::native_icon(contract) {
            return Some(native.to_string());
        }
        let rows = self.eco.portfolio.as_ref().map(|p| p.rows.as_slice()).unwrap_or_default();
        let from_rows = rows
            .iter()
            .find(|r| match &r.key {
                AssetKey::Quai => contract.eq_ignore_ascii_case("quai"),
                AssetKey::Token(a) => a.eq_ignore_ascii_case(contract),
                _ => false,
            })
            .and_then(|r| r.icon_url.clone());
        from_rows
            .or_else(|| self.eco.markets.iter().find(|m| m.address.eq_ignore_ascii_case(contract)).and_then(|m| m.icon_url.clone()))
            // Launch-zone tokens are too new for the explorer's icon set; Quainance has their logos.
            .or_else(|| self.eco.launch_logos.get(&contract.to_lowercase()).cloned())
    }

    /// Icon URL for a portfolio row (bundled logos for QUAI and Qi).
    pub fn row_icon(&self, r: &wallet_core::portfolio::AssetRow) -> Option<String> {
        match &r.key {
            AssetKey::Token(_) => r.icon_url.clone(),
            key => wallet_core::media::native_icon(&key.id()).map(str::to_string),
        }
    }

    /// What this wallet holds of a pool's token, for the deposit card's "have" figure.
    pub fn pool_token_balance(&self, token: &wallet_core::markets::PoolToken) -> Option<String> {
        let asset = SwapAsset::Token { address: token.address.clone(), symbol: token.symbol.clone(), decimals: token.decimals };
        let (amount, decimals) = self.exact_balance(&asset)?;
        Some(amount::group_thousands(&amount::format_amount_short(amount, decimals, 4)))
    }

    /// Icon contract for a pool token: wrapped QUAI is shown as QUAI.
    pub fn pool_icon_contract(&self, token: &wallet_core::markets::PoolToken) -> String {
        let wquai = self.config.network(&self.network_id).ok().and_then(|n| n.wquai);
        if wquai.is_some_and(|w| w.eq_ignore_ascii_case(&token.address)) { "quai".into() } else { token.address.clone() }
    }

    /// Image URL for an NFT, from loaded metadata, holdings or listings.
    pub fn nft_image_url(&self, contract: &str, token_id: &str) -> Option<String> {
        let key = (contract.to_lowercase(), token_id.to_string());
        if let Some(Ok(item)) = self.eco.nft_meta.get(&key)
            && item.image.is_some()
        {
            return item.image.clone();
        }
        if let Some(Ok(v)) = &self.eco.nfts
            && let Some(n) = v.iter().find(|n| n.item.contract.eq_ignore_ascii_case(contract) && n.item.token_id == token_id)
            && n.item.image.is_some()
        {
            return n.item.image.clone();
        }
        self.listing_for(&key.0, token_id).and_then(|l| l.image)
    }

    /// Listing price with known token currencies named.
    pub fn listing_price(&self, l: &Listing) -> String {
        match self.config.network(&self.network_id) {
            Ok(n) => l.price_text_on(&n),
            Err(_) => l.price_text(),
        }
    }

    pub fn listing_for(&self, contract: &str, token_id: &str) -> Option<Listing> {
        self.eco
            .listings
            .values()
            .filter_map(|r| r.as_ref().ok())
            .flat_map(|v| v.iter())
            .find(|l| l.contract == contract && l.token_id == token_id)
            .cloned()
            .or_else(|| self.my_listing(contract, token_id))
    }

    /// Open the list-for-sale form (or change the price of this wallet's listing).
    pub fn open_nft_list(&mut self, contract: &str, token_id: &str) {
        if !self.can_sign() {
            self.toast("this wallet is watch-only", true);
            return;
        }
        let held = match &self.eco.nfts {
            Some(Ok(v)) => v.iter().find(|n| n.item.contract == contract && n.item.token_id == token_id).cloned(),
            _ => None,
        };
        let Some(held) = held else {
            self.toast("this wallet does not hold that NFT", true);
            return;
        };
        if held.kind != wallet_core::explorer::TokenKind::Erc721 {
            self.toast("only ERC-721 items can be listed on Zora asks (Bazarr lists ERC-1155 through Seaport)", true);
            return;
        }
        let network = self.config.network(&self.network_id).ok();
        let current = self.my_listing(contract, token_id).map(|l| {
            let (symbol, decimals) =
                network.as_ref().and_then(|n| wallet_core::market::known_currency(n, &l.currency)).unwrap_or(("QUAI", 18));
            (amount::format_amount(l.price_amount(), decimals), symbol.to_string())
        });
        self.open_form(super::app::FormKind::NftList {
            contract: contract.to_string(),
            token_id: token_id.to_string(),
            owner: held.owner.clone(),
            name: held.item.name.clone(),
            current,
        });
    }

    /// Cancel this wallet's listing of an item (one review; ownership re-checked on-chain).
    pub fn cancel_nft_listing(&mut self, contract: &str, token_id: &str) {
        if !self.can_sign() {
            self.toast("this wallet is watch-only", true);
            return;
        }
        let held = match &self.eco.nfts {
            Some(Ok(v)) => v.iter().find(|n| n.item.contract == contract && n.item.token_id == token_id).cloned(),
            _ => None,
        };
        let Some(held) = held else {
            self.toast("this wallet does not hold that NFT", true);
            return;
        };
        self.start_flow(FlowKind::NftList {
            account: Some(held.owner.clone()),
            contract: contract.to_string(),
            token_id: token_id.to_string(),
            price: None,
            currency: "QUAI".into(),
            label: format!("cancel the listing of {}", held.item.name),
        });
    }

    fn selected_nft(&self) -> Option<(String, String)> {
        match &self.eco.nfts {
            Some(Ok(items)) => items.get(self.selected).map(|n| (n.item.contract.clone(), n.item.token_id.clone())),
            _ => None,
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
            _ => None,
        }
    }

    fn receive_address(&self, qi: bool) -> Option<String> {
        if qi { self.meta.as_ref().and_then(|m| m.payment_code.clone()) } else { self.dash.accounts.first().map(|a| a.address.clone()) }
    }

    /// Link for `o` on the focused item.
    pub fn link_for_focus(&self) -> Option<String> {
        let network = self.config.network(&self.network_id).ok()?;
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

    /// Token picker keys. Returns the modal to keep open, if any.
    pub fn picker_key(&mut self, pay: bool, mut query: String, mut selected: usize, key: KeyEvent) -> Modal {
        let entries = self.picker_entries(&query, pay);
        match key.code {
            KeyCode::Esc => return Modal::None,
            KeyCode::Down | KeyCode::Tab => selected = (selected + 1).min(entries.len().saturating_sub(1)),
            KeyCode::Up | KeyCode::BackTab => selected = selected.saturating_sub(1),
            KeyCode::Enter => {
                match entries.get(selected) {
                    // Refusing here is the whole point: the alternative is letting the user build a
                    // pair and meeting them with "no Quainance pool route" after the fact.
                    Some(entry) if !entry.route.choosable() => {
                        let other = if pay { self.eco.swap.to.as_ref().map(SwapAsset::symbol) } else { Some(self.eco.swap.from.symbol()) };
                        self.toast(
                            format!("no pool route between {} and {}", entry.asset.symbol(), other.unwrap_or("the other side")),
                            true,
                        );
                        return Modal::TokenPicker { pay, query, selected };
                    }
                    Some(entry) => self.pick_swap_asset(pay, entry.asset.clone()),
                    None => {}
                }
                return Modal::None;
            }
            KeyCode::Backspace => {
                query.pop();
                selected = 0;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) && query.len() < 42 => {
                query.push(c);
                selected = 0;
            }
            _ => {}
        }
        Modal::TokenPicker { pay, query, selected }
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
        Screen::Locks => &["lockups"],
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
