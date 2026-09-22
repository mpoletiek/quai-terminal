//! DEX market data for the trading view: Quainance pools with their price candles, volume and
//! trades. explorer.qu.ai supplies the pool directory (`/api/stats/tvl`) and pool event logs;
//! without an explorer the factory and `quai_getLogs` on the node (the monitoring endpoint when
//! one is set) are used. Everything here is display data: swaps re-quote on-chain at review.

use crate::data::{DataCtx, READ_CALLER};
use crate::error::{CoreError, Result};
use crate::explorer::{clean_text, parse_timestamp};
use crate::registry::now;
use quai_sdk::abi::AbiInterface;
use quai_sdk::contracts::Contract;
use quai_sdk::{BlockTag, QuaiAddress, U256};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// `Swap(address,uint256,uint256,uint256,uint256,address)`.
pub const SWAP_TOPIC: &str = "0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822";
/// `Sync(uint112,uint112)`.
pub const SYNC_TOPIC: &str = "0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1";

/// Chart timeframes: (label, bucket seconds).
pub const TIMEFRAMES: &[(&str, u64)] = &[("15m", 900), ("1h", 3600), ("4h", 14_400), ("1d", 86_400)];

/// One side of a pool.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PoolToken {
    /// Contract (lowercase).
    pub address: String,
    /// Symbol (untrusted display text).
    pub symbol: String,
    /// Decimals (18 until read).
    pub decimals: u8,
}

/// Where a market trades. Quainance runs three venues, and a router only reaches its own.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Venue {
    /// The main exchange: the pinned Quainance factory and router.
    #[default]
    Main,
    /// The exchange a bonding curve graduates into: its own factory and router.
    LaunchAmm,
    /// A launch still on its bonding curve. It is bought and sold on the curve, never routed.
    Curve,
    /// The older UniswapV2 deployment, which Quainance's own frontend calls `legacyFactory` and
    /// GeckoTerminal lists as `quaiswap`. Still traded, and its own router serves it.
    ///
    /// Only the pairs named in `ecosystem.legacy_pairs` are read. The factory holds eighteen, two
    /// symbols appear on it twice at different addresses, and a directory that cannot tell those
    /// apart is worse than one that admits it is a shortlist.
    Legacy,
    /// Hartii separate UniswapV2 router/factory, independently pinned.
    HartiiAmm,
}

impl Venue {
    /// How lists name it.
    pub fn label(self) -> &'static str {
        match self {
            Venue::Main => "Quainance",
            Venue::LaunchAmm => "launch AMM",
            Venue::Curve => "bonding curve",
            Venue::Legacy => "QuaiSwap",
            Venue::HartiiAmm => "Hartii AMM",
        }
    }

    /// `on Quainance`, `on the launch AMM`: where a swap happens, in a sentence.
    pub fn on(self) -> &'static str {
        match self {
            Venue::Main => "on Quainance",
            Venue::LaunchAmm => "on the launch AMM",
            Venue::Curve => "on its bonding curve",
            Venue::Legacy => "on QuaiSwap",
            Venue::HartiiAmm => "on Hartii AMM",
        }
    }

    /// Whether a swap router trades here.
    pub fn routable(self) -> bool {
        self != Venue::Curve
    }
}

/// What a displayed QUAI/token number measures. Values with different bases are not directly comparable.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PriceBasis {
    #[default]
    Unknown,
    IndexedLastTrade,
    /// Inverse of quoteBuy(1 QUAI), including fees and finite-trade price impact.
    OneQuaiBuyQuote,
}
impl PriceBasis {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "price basis unknown",
            Self::IndexedLastTrade => "last indexed trade",
            Self::OneQuaiBuyQuote => "1 QUAI buy quote (fee included)",
        }
    }
}

/// Where a bonding curve stands, for a market that has no reserves to read a price from.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct CurveMark {
    /// Latest price in QUAI per whole token.
    pub price_quai: Option<f64>,
    #[serde(default)]
    pub price_basis: PriceBasis,
    /// QUAI raised, and what graduation needs.
    pub raised_quai: f64,
    pub target_quai: Option<f64>,
    /// Progress toward graduation, in basis points.
    pub progress_bps: Option<u64>,
    /// Which launchpad runs this curve, when it is not Quainance's own. Two launchpads' curves sit
    /// in one list and they are different contracts with different operators, so a row says which.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launchpad: Option<String>,
    /// Typed adapter identity; the display label never selects a signing adapter.
    #[serde(default)]
    pub venue_kind: Option<crate::capabilities::Family>,
}

/// A market: a liquidity pool, or a token on its bonding curve (`venue`).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Pool {
    /// Pair contract (lowercase).
    pub address: String,
    /// `token0` of the pair.
    pub token0: PoolToken,
    /// `token1` of the pair.
    pub token1: PoolToken,
    /// Reserve of token0 in token units.
    pub reserve0: f64,
    /// Reserve of token1 in token units.
    pub reserve1: f64,
    /// Pool TVL (USD), when the explorer reports it.
    pub tvl_usd: Option<f64>,
    /// 24h volume (USD), when the explorer reports it.
    pub volume_24h_usd: Option<f64>,
    /// Which venue it trades on.
    #[serde(default)]
    pub venue: Venue,
    /// A bonding curve's standing (`Venue::Curve` only). Its `address` is the curve contract,
    /// token0 the launched token and token1 WQUAI, standing in for the native QUAI it is bought
    /// with.
    #[serde(default)]
    pub curve: Option<CurveMark>,
    /// Token1 per token0 a day ago, from the indexer's hourly buckets, so every row of a market
    /// list can show its 24h change without reading each pool's logs.
    #[serde(default)]
    pub spot_24h_ago: Option<f64>,
}

impl Pool {
    /// Change in the token1-per-token0 price over the last day, in percent.
    pub fn change_24h(&self) -> Option<f64> {
        let (now, then) = (self.spot_price()?, self.spot_24h_ago?);
        (then > 0.0).then(|| (now / then - 1.0) * 100.0).filter(|c| c.is_finite())
    }

    /// Latest price in token1 per token0 from the reserves, or a curve's own mark.
    pub fn spot_price(&self) -> Option<f64> {
        match &self.curve {
            Some(mark) => mark.price_quai,
            None => (self.reserve0 > 0.0).then(|| self.reserve1 / self.reserve0),
        }
        .filter(|p| p.is_finite() && *p > 0.0)
    }
}

/// DEX-wide figures.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct DexOverview {
    /// Total value locked (USD).
    pub tvl_usd: Option<f64>,
    /// 24h volume (USD).
    pub volume_24h_usd: Option<f64>,
    /// Hourly (bucket start, TVL USD, cumulative volume USD).
    pub history: Vec<(u64, f64, f64)>,
    /// When the source observed it.
    pub observed_at: u64,
    /// `explorer.qu.ai` or `chain`.
    pub source: String,
    /// Sources a ceiling kept markets out of. Empty when every market is listed.
    ///
    /// A silently partial list is worse than one that admits it: the data that disappears first is
    /// the newest, which is exactly what someone opened the wallet to find.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omitted: Vec<Omitted>,
    /// Per-source freshness; a successful refresh of another venue cannot make this source fresh.
    #[serde(default)]
    pub sources: Vec<MarketSource>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MarketSource {
    pub venue: Venue,
    pub source: String,
    pub observed_at: u64,
    pub fetched_at: u64,
    pub stale: bool,
    pub complete: bool,
    pub error: Option<String>,
}

impl MarketSource {
    pub fn fresh_at(&self, at: u64) -> bool {
        !self.stale
            && self.complete
            && self.error.is_none()
            && self.observed_at > 0
            && at.saturating_sub(self.observed_at) <= DIRECTORY_TTL * 3
    }
}

/// One source that had more markets than the wallet read.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Omitted {
    /// What was being read (`Quainance factory`, `launch AMM factory`).
    pub source: String,
    /// How many it read, newest first.
    pub read: usize,
    /// How many the source has.
    pub total: usize,
}

impl Omitted {
    /// `Quainance factory: newest 600 of 812`.
    pub fn text(&self) -> String {
        format!("{}: newest {} of {}", self.source, self.read, self.total)
    }

    /// How many a ceiling kept out.
    pub fn missing(&self) -> usize {
        self.total.saturating_sub(self.read)
    }

    /// The short form a panel title can carry: `⚠ 212 not listed`, across every source. Empty when
    /// nothing was kept out. Short on purpose — a title is clipped by its panel, and this is the
    /// part that must survive, so it goes first and says only the number.
    pub fn badge(omitted: &[Omitted], noun: &str) -> Option<String> {
        let missing: usize = omitted.iter().map(Omitted::missing).sum();
        (missing > 0).then(|| format!("⚠ {missing} {noun}"))
    }
}

/// A decoded pool event.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum PoolEvent {
    /// A swap through the pool.
    Swap {
        /// Unix seconds.
        at: u64,
        /// Block height.
        block: u64,
        /// Transaction hash.
        tx: String,
        /// Log position in the block (ordering within a block).
        index: u64,
        #[serde(with = "crate::explorer::u256_string")]
        amount0_in: U256,
        #[serde(with = "crate::explorer::u256_string")]
        amount1_in: U256,
        #[serde(with = "crate::explorer::u256_string")]
        amount0_out: U256,
        #[serde(with = "crate::explorer::u256_string")]
        amount1_out: U256,
        /// Recipient of the output.
        to: String,
    },
    /// Reserves after an interaction.
    Sync {
        /// Unix seconds.
        at: u64,
        /// Block height.
        block: u64,
        /// Transaction hash.
        tx: String,
        /// Log position in the block.
        index: u64,
        #[serde(with = "crate::explorer::u256_string")]
        reserve0: U256,
        #[serde(with = "crate::explorer::u256_string")]
        reserve1: U256,
    },
}

impl PoolEvent {
    /// (time, block, index) for ordering.
    pub fn position(&self) -> (u64, u64, u64) {
        match self {
            PoolEvent::Swap { at, block, index, .. } | PoolEvent::Sync { at, block, index, .. } => (*at, *block, *index),
        }
    }
    /// Transaction hash.
    pub fn tx(&self) -> &str {
        match self {
            PoolEvent::Swap { tx, .. } | PoolEvent::Sync { tx, .. } => tx,
        }
    }
}

/// An OHLC candle in quote-per-base units.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Candle {
    /// Bucket start (unix seconds).
    pub start: u64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    /// Traded volume in quote units.
    pub volume: f64,
    /// Swaps in the bucket.
    pub trades: u32,
}

/// A trade from the base token's point of view.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Trade {
    /// Unix seconds.
    pub at: u64,
    /// Transaction hash.
    pub tx: String,
    /// The trader bought the base token.
    pub buy: bool,
    /// Base amount.
    pub base: f64,
    /// Quote amount.
    pub quote: f64,
    /// Quote per base.
    pub price: f64,
    /// Output recipient.
    pub trader: String,
}

/// 24-hour pair figures.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct PairStats {
    /// Latest price (quote per base).
    pub price: Option<f64>,
    /// Change over 24h (percent).
    pub change_24h: Option<f64>,
    pub high_24h: Option<f64>,
    pub low_24h: Option<f64>,
    /// Quote volume over 24h.
    pub volume_24h: f64,
    pub trades_24h: u32,
}

/// Which side is the base. The quote is the most "money-like" token: USDT, then WQUAI (shown
/// as QUAI), then WQI; otherwise token1.
pub fn base_is_token0(pool: &Pool, usdt: Option<&str>, wquai: Option<&str>, wqi: Option<&str>) -> bool {
    let rank = |a: &str| {
        let is = |x: Option<&str>| x.is_some_and(|x| x.eq_ignore_ascii_case(a));
        if is(usdt) {
            3
        } else if is(wquai) {
            2
        } else if is(wqi) {
            1
        } else {
            0
        }
    };
    rank(&pool.token1.address) >= rank(&pool.token0.address)
}

fn units(v: U256, decimals: u8) -> f64 {
    crate::amount::to_f64(v, decimals)
}

/// Price (quote per base) from raw reserves.
pub fn reserve_price(pool: &Pool, base0: bool, r0: U256, r1: U256) -> Option<f64> {
    let (a, b) = (units(r0, pool.token0.decimals), units(r1, pool.token1.decimals));
    let (base, quote) = if base0 { (a, b) } else { (b, a) };
    (base > 0.0).then(|| quote / base).filter(|p| p.is_finite())
}

fn sorted(events: &[PoolEvent]) -> Vec<&PoolEvent> {
    let mut v: Vec<&PoolEvent> = events.iter().collect();
    v.sort_by_key(|e| e.position());
    v
}

/// Trades, newest first.
pub fn trades(events: &[PoolEvent], pool: &Pool, base0: bool) -> Vec<Trade> {
    let mut out: Vec<Trade> = sorted(events)
        .into_iter()
        .filter_map(|e| match e {
            PoolEvent::Swap { at, tx, amount0_in, amount1_in, amount0_out, amount1_out, to, .. } => {
                let (d0, d1) = (pool.token0.decimals, pool.token1.decimals);
                let (b_in, b_out, q_in, q_out) = if base0 {
                    (units(*amount0_in, d0), units(*amount0_out, d0), units(*amount1_in, d1), units(*amount1_out, d1))
                } else {
                    (units(*amount1_in, d1), units(*amount1_out, d1), units(*amount0_in, d0), units(*amount0_out, d0))
                };
                let buy = b_out > b_in;
                let (base, quote) = if buy { (b_out - b_in, q_in - q_out) } else { (b_in - b_out, q_out - q_in) };
                (base > 0.0 && quote > 0.0).then(|| Trade {
                    at: *at,
                    tx: tx.clone(),
                    buy,
                    base,
                    quote,
                    price: quote / base,
                    trader: to.clone(),
                })
            }
            _ => None,
        })
        .collect();
    out.reverse();
    out
}

/// `count` candles of `bucket` seconds ending at the bucket containing `now`. Prices come from
/// Sync reserves; empty buckets carry the previous close. Buckets before the first known price
/// are omitted.
pub fn candles(events: &[PoolEvent], pool: &Pool, base0: bool, bucket: u64, now: u64, count: usize) -> Vec<Candle> {
    candles_in_zone(events, pool, base0, bucket, 0, now, count)
}

/// [`candles`] with bucket boundaries aligned to a UTC offset (seconds east), so 4h and daily
/// candles start at local midnight.
pub fn candles_in_zone(
    events: &[PoolEvent],
    pool: &Pool,
    base0: bool,
    bucket: u64,
    utc_offset: i64,
    now: u64,
    count: usize,
) -> Vec<Candle> {
    if bucket == 0 || count == 0 {
        return Vec::new();
    }
    let shift = utc_offset.rem_euclid(bucket as i64) as u64;
    let last_start = now - (now + shift) % bucket;
    let first_start = last_start.saturating_sub(bucket * (count as u64 - 1));
    let events = sorted(events);
    let mut price: Option<f64> = None;
    let mut i = 0;
    // Price before the window.
    while i < events.len() && events[i].position().0 < first_start {
        if let PoolEvent::Sync { reserve0, reserve1, .. } = events[i] {
            price = reserve_price(pool, base0, *reserve0, *reserve1).or(price);
        }
        i += 1;
    }
    let mut out = Vec::new();
    for k in 0..count as u64 {
        let start = first_start + k * bucket;
        let end = start + bucket;
        let mut c = price.map(|p| Candle { start, open: p, high: p, low: p, close: p, volume: 0.0, trades: 0 });
        while i < events.len() && events[i].position().0 < end {
            match events[i] {
                PoolEvent::Sync { reserve0, reserve1, .. } => {
                    if let Some(p) = reserve_price(pool, base0, *reserve0, *reserve1) {
                        let cc = c.get_or_insert(Candle { start, open: p, high: p, low: p, close: p, volume: 0.0, trades: 0 });
                        cc.high = cc.high.max(p);
                        cc.low = cc.low.min(p);
                        cc.close = p;
                        price = Some(p);
                    }
                }
                swap @ PoolEvent::Swap { .. } => {
                    if let Some(t) = trades(std::slice::from_ref(swap), pool, base0).first() {
                        let cc = c.get_or_insert(Candle {
                            start,
                            open: t.price,
                            high: t.price,
                            low: t.price,
                            close: t.price,
                            volume: 0.0,
                            trades: 0,
                        });
                        cc.volume += t.quote;
                        cc.trades += 1;
                    }
                }
            }
            i += 1;
        }
        if let Some(c) = c {
            out.push(c);
        }
    }
    out
}

/// 24-hour figures.
pub fn pair_stats(events: &[PoolEvent], pool: &Pool, base0: bool, now: u64) -> PairStats {
    let day = candles(events, pool, base0, 3600, now, 24);
    let mut stats = PairStats {
        price: day.last().map(|c| c.close).or_else(|| {
            let (b, q) = if base0 { (pool.reserve0, pool.reserve1) } else { (pool.reserve1, pool.reserve0) };
            (b > 0.0).then(|| q / b)
        }),
        ..PairStats::default()
    };
    if let (Some(first), Some(last)) = (day.first(), day.last())
        && first.open > 0.0
    {
        stats.change_24h = Some((last.close / first.open - 1.0) * 100.0);
        stats.high_24h = day.iter().map(|c| c.high).reduce(f64::max);
        stats.low_24h = day.iter().map(|c| c.low).reduce(f64::min);
    }
    stats.volume_24h = day.iter().map(|c| c.volume).sum();
    stats.trades_24h = day.iter().map(|c| c.trades).sum();
    stats
}

fn word(data: &[u8], i: usize) -> Option<U256> {
    data.get(i * 32..(i + 1) * 32).map(U256::from_be_slice)
}

fn topic_address(topic: &str) -> String {
    let t = topic.trim_start_matches("0x");
    if t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit()) { format!("0x{}", &t[24..]) } else { String::new() }
}

/// Decode one pool log from its parts.
pub fn decode_log(topics: &[String], data_hex: &str, at: u64, block: u64, tx: &str, index: u64) -> Option<PoolEvent> {
    let data = hex::decode(data_hex.trim_start_matches("0x")).ok()?;
    let topic0 = topics.first()?.to_lowercase();
    if topic0 == SWAP_TOPIC {
        Some(PoolEvent::Swap {
            at,
            block,
            tx: tx.to_lowercase(),
            index,
            amount0_in: word(&data, 0)?,
            amount1_in: word(&data, 1)?,
            amount0_out: word(&data, 2)?,
            amount1_out: word(&data, 3)?,
            to: topics.get(2).map(|t| topic_address(t)).unwrap_or_default(),
        })
    } else if topic0 == SYNC_TOPIC {
        Some(PoolEvent::Sync { at, block, tx: tx.to_lowercase(), index, reserve0: word(&data, 0)?, reserve1: word(&data, 1)? })
    } else {
        None
    }
}

/// `/api/address/{pair}/logs` page: decoded Swap/Sync events and the next cursor.
pub fn parse_explorer_logs(v: &Value) -> (Vec<PoolEvent>, Option<String>) {
    let events = v["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|l| {
                    let topics: Vec<String> = l["topics"].as_array()?.iter().filter_map(|t| t.as_str().map(str::to_string)).collect();
                    let at = l["timestamp"].as_str().and_then(parse_timestamp).unwrap_or(0);
                    let block = l["block_height"].as_str().and_then(|b| b.parse().ok()).or(l["block_height"].as_u64()).unwrap_or(0);
                    decode_log(&topics, l["data"].as_str()?, at, block, l["tx_hash"].as_str()?, l["idx"].as_u64().unwrap_or(0))
                })
                .collect()
        })
        .unwrap_or_default();
    let cursor = if v["hasMore"].as_bool() == Some(true) { v["nextCursor"].as_str().map(str::to_string) } else { None };
    (events, cursor)
}

fn f(v: &Value) -> Option<f64> {
    match v {
        Value::String(s) => s.trim().parse().ok(),
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
    .filter(|x: &f64| x.is_finite())
}

/// Pools and the DEX overview from `/api/stats/tvl` (decimals default to 18 until read).
pub fn parse_tvl_pools(v: &Value) -> (Vec<Pool>, DexOverview) {
    let token = |t: &Value| PoolToken {
        address: t["address"].as_str().unwrap_or_default().to_lowercase(),
        symbol: clean_text(t["symbol"].as_str().unwrap_or("?")),
        decimals: 18,
    };
    let pools = v["pools"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|p| p["address"].as_str().is_some_and(|x| x.starts_with("0x")))
                .map(|p| Pool {
                    address: p["address"].as_str().unwrap_or_default().to_lowercase(),
                    token0: token(&p["token0"]),
                    token1: token(&p["token1"]),
                    reserve0: f(&p["reserve0"]).unwrap_or(0.0),
                    reserve1: f(&p["reserve1"]).unwrap_or(0.0),
                    tvl_usd: f(&p["tvlUsd"]),
                    volume_24h_usd: f(&p["volume24hUsd"]),
                    venue: Venue::Main,
                    curve: None,
                    spot_24h_ago: None,
                })
                .collect()
        })
        .unwrap_or_default();
    let history = v["history"]
        .as_array()
        .map(|h| {
            h.iter()
                .filter_map(|b| Some((parse_timestamp(b["bucket"].as_str()?)?, f(&b["tvlUsd"])?, f(&b["totalVolumeUsd"]).unwrap_or(0.0))))
                .collect()
        })
        .unwrap_or_default();
    let overview = DexOverview {
        tvl_usd: f(&v["current"]["tvlUsd"]),
        volume_24h_usd: f(&v["current"]["volume24hUsd"]),
        history,
        observed_at: v["freshness"]["observedAt"].as_str().and_then(parse_timestamp).unwrap_or(0),
        source: "explorer.qu.ai".into(),
        omitted: Vec::new(),
        sources: Vec::new(),
    };
    (pools, overview)
}

const PAIR_READ_ABI: &[&str] = &[
    "function token0() view returns (address)",
    "function token1() view returns (address)",
    "function getReserves() view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast)",
];
const FACTORY_READ_ABI: &[&str] =
    &["function allPairsLength() view returns (uint256)", "function allPairs(uint256) view returns (address)"];

/// A token's symbol and decimals, cached for 30 days (short address and 18 when unreadable).
pub(crate) async fn token_meta(ctx: &DataCtx, address: &str) -> PoolToken {
    let key = token_meta_key(address);
    let addr = address.to_string();
    let meta = ctx.cached(&key, TOKEN_META_TTL, || async move { ctx.erc20_metadata(&addr, READ_CALLER).await }).await;
    match meta {
        Ok(m) => PoolToken { address: address.to_lowercase(), symbol: m.value.0, decimals: m.value.2 },
        Err(_) => unnamed(address),
    }
}

/// Required financial metadata is always read first-hand and never substitutes a decimal scale.
pub(crate) async fn token_meta_required(ctx: &DataCtx, address: &str) -> Result<PoolToken> {
    let (symbol, _, decimals) = ctx.erc20_metadata(address, READ_CALLER).await?;
    Ok(PoolToken { address: address.to_lowercase(), symbol, decimals })
}

/// A token nothing could be read about: its own short address, and the common 18 decimals.
fn unnamed(address: &str) -> PoolToken {
    PoolToken { address: address.to_lowercase(), symbol: crate::session::short_address(address), decimals: 18 }
}

fn token_meta_key(address: &str) -> String {
    format!("token_meta:{}", address.to_lowercase())
}

/// How long a token's symbol and decimals are believed. They are immutable in every ERC-20 the
/// wallet trades, so this is long; it expires at all only so a mis-read is not permanent.
const TOKEN_META_TTL: u64 = 86_400 * 30;

/// Symbol and decimals for many tokens at once.
///
/// A pool directory is two tokens per pair, and reading each one on its own is three sequential
/// calls — 1 800 of them for a 600-pair factory, which is minutes. Multicall3 answers all three
/// fields for every unknown token in one round. Known tokens never leave the cache, so a warm
/// directory asks for nothing.
pub(crate) async fn token_meta_all(ctx: &DataCtx, addresses: &[String]) -> std::collections::HashMap<String, PoolToken> {
    use crate::multicall::{Call, Multicall, word};
    let wanted: std::collections::BTreeSet<String> = addresses.iter().map(|a| a.to_lowercase()).filter(|a| !a.is_empty()).collect();
    let mut out: std::collections::HashMap<String, PoolToken> = std::collections::HashMap::new();
    let mut missing: Vec<String> = Vec::new();
    for address in wanted {
        let cached = ctx.trust.may_cache().then(|| remembered_meta(ctx, &address)).flatten();
        match cached {
            Some(token) => {
                out.insert(address, token);
            }
            None => missing.push(address),
        }
    }
    if missing.is_empty() {
        return out;
    }
    if ctx.cache_only {
        for address in missing {
            out.insert(address.clone(), unnamed(&address));
        }
        return out;
    }
    let Some(mc) = Multicall::open(ctx).await else {
        out.extend(token_meta_fallback(ctx, missing).await);
        return out;
    };
    let mut calls = Vec::with_capacity(missing.len() * 3);
    for address in &missing {
        calls.push(Call::view(address, "symbol()", &[]));
        calls.push(Call::view(address, "name()", &[]));
        calls.push(Call::view(address, "decimals()", &[]));
    }
    let Ok(answers) = mc.try_all(&calls).await else {
        out.extend(token_meta_fallback(ctx, missing).await);
        return out;
    };
    for (i, address) in missing.into_iter().enumerate() {
        let at = |k: usize| answers.get(i * 3 + k).and_then(Option::as_ref);
        // Decimals are the one field an amount depends on, so a token that will not answer them is
        // left unnamed rather than assumed — and not remembered as if it had answered.
        let decimals = at(2).filter(|d| d.len() >= 32).map(|d| word(d, 0)).and_then(|w| u8::try_from(w).ok()).filter(|d| *d <= 77);
        let Some(decimals) = decimals else {
            out.insert(address.clone(), unnamed(&address));
            continue;
        };
        let symbol = at(0).and_then(|d| solidity_string(d)).unwrap_or_else(|| crate::session::short_address(&address));
        let name = at(1).and_then(|d| solidity_string(d)).unwrap_or_else(|| symbol.clone());
        if ctx.trust.may_cache()
            && let Ok(text) = serde_json::to_string(&(&symbol, &name, decimals))
        {
            let key = token_meta_key(&address);
            let _ = ctx.store(&key).cache_put(&format!("{}:{key}", ctx.network.id), &text);
        }
        out.insert(address.clone(), PoolToken { address, symbol, decimals });
    }
    out
}

async fn token_meta_fallback(ctx: &DataCtx, addresses: Vec<String>) -> std::collections::HashMap<String, PoolToken> {
    use futures::StreamExt;
    futures::stream::iter(addresses.into_iter().map(|address| async move {
        let token = token_meta(ctx, &address).await;
        (address, token)
    }))
    .buffer_unordered(4)
    .collect()
    .await
}

/// A token's cached symbol and decimals, when one is stored and still fresh.
fn remembered_meta(ctx: &DataCtx, address: &str) -> Option<PoolToken> {
    let feed = token_meta_key(address);
    let key = format!("{}:{feed}", ctx.network.id);
    let (text, at) = ctx.store(&feed).cache_get(&key).ok().flatten()?;
    if now().saturating_sub(at) >= TOKEN_META_TTL {
        return None;
    }
    let (symbol, _name, decimals): (String, String, u8) = serde_json::from_str(&text).ok()?;
    Some(PoolToken { address: address.to_lowercase(), symbol, decimals })
}

/// An ABI-encoded return read as text: a `string` (offset, length, bytes) or the `bytes32` some
/// older tokens return instead. Untrusted display text, so it is sanitized like any other.
pub(crate) fn solidity_string(data: &[u8]) -> Option<String> {
    let at = |i: usize| usize::try_from(crate::multicall::word(data, i)).ok();
    let text = if data.len() == 32 {
        let end = data.iter().position(|b| *b == 0).unwrap_or(32);
        String::from_utf8_lossy(&data[..end]).into_owned()
    } else {
        let offset = at(0)?;
        if offset % 32 != 0 {
            return None;
        }
        let length = at(offset / 32)?.min(256);
        let start = offset.checked_add(32)?;
        String::from_utf8_lossy(data.get(start..start.checked_add(length)?)?).into_owned()
    };
    let text = crate::ops::sanitize_display(&text);
    (!text.trim().is_empty()).then_some(text)
}

/// How long a pool directory read is served from cache.
///
/// This is discovery — which pools exist, their tokens and decimals, and the 24h volume only the
/// explorer computes. Price and TVL do *not* wait for it: [`refresh_reserves`] reads those off the
/// node every few seconds.
///
/// 30 s because that is the explorer's own figure. `/api/stats/tvl` answers
/// `cache-control: public, max-age=30`, and two reads eight seconds apart come back byte for byte
/// identical, so asking more often buys nothing that is not already on the wire — it only spends
/// requests against a budget the rest of the screen shares.
pub const DIRECTORY_TTL: u64 = 30;

/// The same, for directories read from a factory over RPC rather than from the explorer.
///
/// These are multicall batches against the node, not a metered third party, but they are several
/// round-trips each and the set of pools a factory holds changes when someone deploys a pair —
/// not every block. Reserves inside them are what move, and 15 s is close enough to keep a
/// launch-AMM or QuaiSwap row honest without re-enumerating two factories every tick.
pub const FACTORY_TTL: u64 = 15;

/// Pools with token decimals, largest TVL first, and the DEX overview.
pub async fn pools(ctx: &DataCtx) -> Result<(Vec<Pool>, DexOverview)> {
    let (mut pools, mut overview) = if ctx.policy.market && ctx.explorer.source() == "explorer.qu.ai" {
        let explorer = ctx.explorer.clone();
        let base = explorer.absolute("/api/stats/tvl?days=7");
        match ctx.cached("dex_pools", DIRECTORY_TTL, || async move { Ok(parse_tvl_pools(&crate::http::get_json(&base).await?)) }).await {
            Ok(cached) => {
                let (pools, mut overview) = cached.value;
                overview.sources = vec![MarketSource {
                    venue: Venue::Main,
                    source: overview.source.clone(),
                    observed_at: overview.observed_at,
                    fetched_at: cached.fetched_at,
                    stale: cached.stale,
                    complete: true,
                    error: None,
                }];
                (pools, overview)
            }
            Err(error) if !ctx.cache_only => {
                let (pools, mut overview) = chain_pools(ctx).await?;
                overview.sources.push(MarketSource {
                    venue: Venue::Main,
                    source: "explorer.qu.ai".into(),
                    stale: true,
                    error: Some(error.to_string()),
                    ..MarketSource::default()
                });
                (pools, overview)
            }
            Err(error) => return Err(error),
        }
    } else {
        ctx.online()?;
        chain_pools(ctx).await?
    };
    let addresses: Vec<String> = pools.iter().flat_map(|p| [p.token0.address.clone(), p.token1.address.clone()]).collect();
    let metadata = token_meta_all(ctx, &addresses).await;
    for p in &mut pools {
        for t in [&mut p.token0, &mut p.token1] {
            if let Some(meta) = metadata.get(&t.address.to_lowercase()) {
                t.decimals = meta.decimals;
                if t.symbol.is_empty() || t.symbol == "?" {
                    t.symbol = meta.symbol.clone();
                }
            }
        }
    }
    if overview.sources.is_empty() {
        overview.sources.push(MarketSource {
            venue: Venue::Main,
            source: overview.source.clone(),
            observed_at: overview.observed_at,
            fetched_at: now(),
            complete: overview.omitted.is_empty(),
            ..MarketSource::default()
        });
    }
    pools.sort_by(|a, b| b.tvl_usd.unwrap_or(0.0).total_cmp(&a.tvl_usd.unwrap_or(0.0)).then(a.address.cmp(&b.address)));
    Ok((pools, overview))
}

/// Pools straight from the factory (networks without an explorer), newest pair first.
async fn chain_pools(ctx: &DataCtx) -> Result<(Vec<Pool>, DexOverview)> {
    let pin = ctx
        .network
        .ecosystem
        .quainance_factory
        .clone()
        .ok_or_else(|| CoreError::NotFound(format!("no DEX factory configured on {}", ctx.network.name)))?;
    let directory = factory_pools(ctx, &pin, "Quainance factory", Venue::Main).await?;
    let omitted = directory.omitted("Quainance factory").into_iter().collect();
    Ok((
        directory.pools,
        DexOverview {
            source: "chain".into(),
            observed_at: directory.fetched_at,
            omitted,
            sources: vec![MarketSource {
                venue: Venue::Main,
                source: "chain".into(),
                observed_at: directory.fetched_at,
                fetched_at: directory.fetched_at,
                stale: directory.stale,
                complete: directory.read >= directory.total,
                ..MarketSource::default()
            }],
            ..DexOverview::default()
        },
    ))
}

/// Every market Quainance runs: the main exchange's pools, the launch AMM's pools and the tokens
/// still on their bonding curves. Independent sources have bounded deadlines; one unavailable
/// venue cannot discard the directories that answered.
pub async fn all_markets(ctx: &DataCtx) -> Result<(Vec<Pool>, DexOverview)> {
    let (main, launch, legacy, hartii, hartii_amm) = futures::join!(
        venue_deadline(pools(ctx)),
        venue_deadline(launch_amm_pools(ctx)),
        venue_deadline(legacy_pools(ctx)),
        venue_deadline(crate::hartii::launches_observed(ctx)),
        venue_deadline(hartii_amm_pools(ctx)),
    );
    let mut healthy = main.is_ok();
    let (mut markets, mut overview) = match main {
        Ok(value) => value,
        Err(error) => (
            Vec::new(),
            DexOverview {
                sources: vec![MarketSource {
                    venue: Venue::Main,
                    source: "Quainance".into(),
                    stale: true,
                    error: Some(error.to_string()),
                    ..MarketSource::default()
                }],
                ..DexOverview::default()
            },
        ),
    };
    let usd = wquai_usd(&markets, ctx.network.wquai.as_deref(), ctx.network.ecosystem.usdt.as_ref().map(|u| u.address.as_str()));
    for (venue, name, configured, result) in [
        (Venue::LaunchAmm, "launch AMM factory", ctx.network.ecosystem.launch_amm_factory.is_some(), launch),
        (Venue::Legacy, "QuaiSwap factory", ctx.network.ecosystem.legacy_factory.is_some(), legacy),
        (Venue::HartiiAmm, "Hartii AMM factory", ctx.network.ecosystem.hartii_amm_factory.is_some(), hartii_amm),
    ] {
        if !configured {
            continue;
        }
        match result {
            Ok(mut directory) => {
                healthy = true;
                overview.sources.push(MarketSource {
                    venue,
                    source: name.into(),
                    observed_at: directory.fetched_at,
                    fetched_at: directory.fetched_at,
                    stale: directory.stale,
                    complete: directory.read >= directory.total,
                    error: None,
                });
                overview.omitted.extend(directory.omitted(name));
                for p in &mut directory.pools {
                    p.tvl_usd = amm_tvl(p, ctx.network.wquai.as_deref(), usd);
                }
                markets.extend(directory.pools);
            }
            Err(error) => overview.sources.push(MarketSource {
                venue,
                source: name.into(),
                stale: true,
                error: Some(error.to_string()),
                ..MarketSource::default()
            }),
        }
    }
    if let Ok(rows) = hartii {
        healthy = true;
        if let Some(wquai) = ctx.network.wquai.as_deref() {
            markets.extend(crate::hartii::curve_pools(&rows.value, wquai));
        }
        overview.sources.push(MarketSource {
            venue: Venue::Curve,
            source: "HartiiLabs".into(),
            observed_at: rows.fetched_at,
            fetched_at: rows.fetched_at,
            stale: rows.stale,
            complete: rows.value.len() < crate::hartii::MAX_TOKENS as usize,
            error: None,
        });
    }
    if ctx.network.ecosystem.hartii_launcher.is_some() && !overview.sources.iter().any(|source| source.source == "HartiiLabs") {
        overview.sources.push(MarketSource {
            venue: Venue::Curve,
            source: "HartiiLabs".into(),
            stale: true,
            error: Some("HartiiLabs directory unavailable".into()),
            ..MarketSource::default()
        });
    }
    if !healthy {
        return Err(CoreError::Network("no market venue answered; retry when a source recovers".into()));
    }
    // A day-ago price for every pool that has reserves, in one query; without it a row shows no
    // change at all until it is selected and its own trades are read, which is why a launch-AMM or
    // QuaiSwap row used to sit blank while the main exchange's rows all moved. Asking for a pair
    // the subgraph does not index costs nothing — it is one more alias in the same request, and an
    // unindexed pair simply comes back empty. A curve has no pair to ask about.
    let indexed: Vec<String> = markets.iter().filter(|p| p.venue.routable()).map(|p| p.address.clone()).collect();
    if let Ok(then) = venue_deadline(crate::subgraph::spot_24h_ago(ctx, &indexed)).await {
        for p in &mut markets {
            p.spot_24h_ago = then.get(&p.address.to_lowercase()).copied();
        }
    }
    // Pools deepest first, then the curves nearest graduation.
    let progress = |p: &Pool| p.curve.as_ref().and_then(|c| c.progress_bps).unwrap_or(0);
    markets.sort_by(|a, b| {
        (a.venue == Venue::Curve)
            .cmp(&(b.venue == Venue::Curve))
            .then(b.tvl_usd.unwrap_or(0.0).total_cmp(&a.tvl_usd.unwrap_or(0.0)))
            .then(progress(b).cmp(&progress(a)))
            .then(a.address.cmp(&b.address))
    });
    Ok((markets, overview))
}

/// Optional sources have a bounded share of a directory refresh, including multi-call walks.
async fn venue_deadline<T>(read: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout(std::time::Duration::from_secs(3), read)
        .await
        .unwrap_or_else(|_| Err(CoreError::Network("market source timed out after 3 s".into())))
}

/// USD per WQUAI from the main exchange's WQUAI/USDT pool (USDT taken at a dollar). Display data:
/// it prices the launch AMM's pools, which the explorer does not index.
pub fn wquai_usd(pools: &[Pool], wquai: Option<&str>, usdt: Option<&str>) -> Option<f64> {
    let (wquai, usdt) = (wquai?, usdt?);
    pools
        .iter()
        .filter(|p| p.venue == Venue::Main)
        .find_map(|p| {
            let is = |t: &PoolToken, a: &str| t.address.eq_ignore_ascii_case(a);
            if is(&p.token0, wquai) && is(&p.token1, usdt) {
                (p.reserve0 > 0.0).then(|| p.reserve1 / p.reserve0)
            } else if is(&p.token1, wquai) && is(&p.token0, usdt) {
                (p.reserve1 > 0.0).then(|| p.reserve0 / p.reserve1)
            } else {
                None
            }
        })
        .filter(|v| v.is_finite() && *v > 0.0)
}

/// A WQUAI pool's TVL: twice its WQUAI side.
fn amm_tvl(pool: &Pool, wquai: Option<&str>, usd_per_wquai: Option<f64>) -> Option<f64> {
    let (wquai, usd) = (wquai?, usd_per_wquai?);
    let side = if pool.token0.address.eq_ignore_ascii_case(wquai) {
        pool.reserve0
    } else if pool.token1.address.eq_ignore_ascii_case(wquai) {
        pool.reserve1
    } else {
        return None;
    };
    Some(2.0 * side * usd)
}

/// The launch AMM's pools, read from its pinned factory. Graduated curves land here, so it grows
/// with the launch zone; a minute of cache is all a refreshing Markets view needs.
/// The shortlisted pairs of the older exchange, when this network names any.
fn directory_key(prefix: &str, factory: &crate::network::PinnedContract, pairs: &[String]) -> String {
    use sha2::{Digest, Sha256};
    let mut pairs: Vec<_> = pairs.iter().map(|p| p.to_lowercase()).collect();
    pairs.sort();
    pairs.dedup();
    let identity = json!([factory.address.to_lowercase(), factory.code_hash, pairs]).to_string();
    format!("{prefix}:{}", hex::encode(Sha256::digest(identity.as_bytes())))
}

pub async fn legacy_pools(ctx: &DataCtx) -> Result<Directory> {
    let factory = ctx
        .network
        .ecosystem
        .legacy_factory
        .clone()
        .ok_or_else(|| CoreError::NotFound(format!("no legacy exchange on {}", ctx.network.name)))?;
    let pairs = ctx.network.ecosystem.legacy_pairs.clone();
    let key = directory_key("legacy_pools", &factory, &pairs);
    let cached = ctx
        .cached(&key, FACTORY_TTL, || async move { allowlisted_pools(ctx, &factory, "QuaiSwap factory", Venue::Legacy, &pairs).await })
        .await?;
    Ok(Directory { fetched_at: cached.fetched_at, stale: cached.stale, ..cached.value })
}

/// Hartii AMM directory comes only from its authenticated factory, independent of launch labels.
pub async fn hartii_amm_pools(ctx: &DataCtx) -> Result<Directory> {
    let factory =
        ctx.network.ecosystem.hartii_amm_factory.clone().ok_or_else(|| CoreError::NotFound("Hartii AMM is not configured".into()))?;
    let key = directory_key("hartii_amm_pools", &factory, &[]);
    let cached =
        ctx.cached(&key, FACTORY_TTL, || async move { factory_pools(ctx, &factory, "Hartii AMM factory", Venue::HartiiAmm).await }).await?;
    Ok(Directory { fetched_at: cached.fetched_at, stale: cached.stale, ..cached.value })
}

pub async fn launch_amm_pools(ctx: &DataCtx) -> Result<Directory> {
    let factory = ctx
        .network
        .ecosystem
        .launch_amm_factory
        .clone()
        .ok_or_else(|| CoreError::NotFound(format!("no launch AMM on {}", ctx.network.name)))?;
    let key = directory_key("launch_amm_pools_v2", &factory, &[]);
    let cached =
        ctx.cached(&key, FACTORY_TTL, || async move { factory_pools(ctx, &factory, "launch AMM factory", Venue::LaunchAmm).await }).await?;
    Ok(Directory { fetched_at: cached.fetched_at, stale: cached.stale, ..cached.value })
}

/// Launches still on their bonding curve, as markets priced by the launch index.
pub async fn curve_markets(ctx: &DataCtx) -> Result<Vec<Pool>> {
    let wquai = ctx.network.wquai.clone().ok_or_else(|| CoreError::NotFound("WQUAI is not configured".into()))?.to_lowercase();
    let launches = crate::launches::launches(ctx, 200).await?;
    Ok(curve_pools(&launches, &wquai))
}

/// Pure part of [`curve_markets`].
pub fn curve_pools(launches: &[crate::launches::Launch], wquai: &str) -> Vec<Pool> {
    launches
        .iter()
        .filter(|l| l.phase == crate::launches::Phase::Bonding)
        .filter_map(|l| {
            let curve = l.curve.clone()?;
            Some(Pool {
                address: curve,
                token0: PoolToken { address: l.token.clone(), symbol: l.symbol.clone(), decimals: l.decimals },
                token1: PoolToken { address: wquai.to_lowercase(), symbol: "WQUAI".into(), decimals: 18 },
                venue: Venue::Curve,
                curve: Some(CurveMark {
                    price_quai: l.price_quai,
                    price_basis: l.price_basis,
                    raised_quai: l.raised_quai,
                    target_quai: l.target_quai,
                    progress_bps: l.progress_bps,
                    launchpad: None,
                    venue_kind: l.venue_kind,
                }),
                ..Pool::default()
            })
        })
        .collect()
}

/// Pairs one factory's directory is read down to, newest first.
///
/// `allPairs` is append-only, so index 0 is the oldest pair and the last index the newest. The
/// wallet used to keep the first 60, which on a growing DEX means hiding exactly the tokens people
/// opened it to find — and hiding them silently. It now reads from the end, pages through
/// Multicall3, and says when a ceiling applied.
pub const MAX_FACTORY_PAIRS: u64 = 600;

/// Without a Multicall3 every index is its own round trip, so the ceiling is much lower: a
/// thousand serial calls is not a directory read, it is a hang.
pub const MAX_FACTORY_PAIRS_SEQUENTIAL: u64 = 60;

/// A factory's directory: its pools, and how many pairs it actually holds.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Directory {
    /// The pools read, newest pair first.
    pub pools: Vec<Pool>,
    /// Pairs the factory reports in `allPairsLength()`.
    pub total: usize,
    /// Indices read. Fewer than `total` means a ceiling applied.
    pub read: usize,
    #[serde(default)]
    pub fetched_at: u64,
    #[serde(default)]
    pub stale: bool,
}

impl Directory {
    /// What a ceiling kept out, when it kept anything out.
    pub fn omitted(&self, source: &str) -> Option<Omitted> {
        (self.total > self.read).then(|| Omitted { source: source.into(), read: self.read, total: self.total })
    }
}

/// A factory's pools, newest pair first, marked with the venue they trade on.
///
/// Four reads per pair, which is why this batches when a Multicall3 is configured: 600 pairs is
/// 2 400 sequential calls otherwise, against 25 batched round trips.
/// The named pairs of a factory, rather than everything it has ever built.
///
/// Each pair is asked for its own `factory()` and dropped unless it answers with the pinned one, so
/// an address in the allowlist can only ever name a pair that factory really built. Used for the
/// legacy exchange, where two symbols appear twice and the shortlist is the point.
pub async fn allowlisted_pools(
    ctx: &DataCtx,
    factory: &crate::network::PinnedContract,
    what: &str,
    venue: Venue,
    pairs: &[String],
) -> Result<Directory> {
    if pairs.is_empty() {
        return Ok(Directory::default());
    }
    let factory = crate::data::verify_pinned(&ctx.app, &ctx.node, &ctx.network, factory, what, ctx.trust).await?;
    let expected = factory.to_string().to_lowercase();
    let wanted: Vec<String> = pairs.iter().map(|p| p.to_lowercase()).collect();
    let mut kept = Vec::new();
    if let Some(mc) = crate::multicall::Multicall::open(ctx).await {
        use crate::multicall::{Call, address_word};
        let calls: Vec<Call> = wanted.iter().map(|p| Call::view(p, "factory()", &[])).collect();
        for (pair, data) in wanted.iter().zip(mc.try_all(&calls).await?) {
            if data.as_ref().map(|d| address_word(d, 0)).is_some_and(|f| f == expected) {
                kept.push(pair.clone());
            }
        }
    } else {
        let caller: QuaiAddress = READ_CALLER.parse().map_err(|_| CoreError::Invalid("caller".into()))?;
        for pair in &wanted {
            let Ok(pa) = pair.parse::<QuaiAddress>() else { continue };
            let interface = AbiInterface::from_human_readable(&["function factory() view returns (address)"][..])
                .map_err(|_| CoreError::Invalid("pair abi".into()))?;
            let answered = Contract::new(pa, interface, &ctx.node.provider)
                .call(caller, "factory", &[], BlockTag::Latest)
                .await
                .ok()
                .and_then(|v| v.first().and_then(Value::as_str).map(str::to_lowercase));
            if answered.as_deref() == Some(expected.as_str()) {
                kept.push(pair.clone());
            }
        }
    }
    pools_from_pairs(ctx, kept, wanted.len(), venue).await
}

async fn factory_pools(ctx: &DataCtx, factory: &crate::network::PinnedContract, what: &str, venue: Venue) -> Result<Directory> {
    let factory = crate::data::verify_pinned(&ctx.app, &ctx.node, &ctx.network, factory, what, ctx.trust).await?;
    let caller: QuaiAddress = READ_CALLER.parse().map_err(|_| CoreError::Invalid("caller".into()))?;
    let fc = Contract::new(
        factory,
        AbiInterface::from_human_readable(FACTORY_READ_ABI).map_err(|e| CoreError::Invalid(format!("factory abi: {e}")))?,
        &ctx.node.provider,
    );
    let n: u64 = fc
        .call(caller, "allPairsLength", &[], BlockTag::Latest)
        .await?
        .first()
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let batched = crate::multicall::Multicall::open(ctx).await;
    let cap = if batched.is_some() { MAX_FACTORY_PAIRS } else { MAX_FACTORY_PAIRS_SEQUENTIAL };
    // Newest first, so that if a ceiling ever applies the half kept is the useful half.
    let indices: Vec<u64> = (0..n).rev().take(usize::try_from(cap).unwrap_or(usize::MAX)).collect();
    let mut pairs = Vec::new();
    if let Some(mc) = &batched {
        use crate::multicall::{Arg, Call, address_word};
        let factory_hex = factory.to_string().to_lowercase();
        let calls: Vec<Call> =
            indices.iter().map(|i| Call::view(&factory_hex, "allPairs(uint256)", &[Arg::Uint(U256::from(*i))])).collect();
        for data in mc.try_all(&calls).await?.into_iter().flatten() {
            let a = address_word(&data, 0);
            if !a.is_empty() {
                pairs.push(a);
            }
        }
    } else {
        for i in &indices {
            if let Some(pair) = fc
                .call(caller, "allPairs", &[json!(i.to_string())], BlockTag::Latest)
                .await?
                .first()
                .and_then(Value::as_str)
                .map(str::to_lowercase)
            {
                pairs.push(pair);
            }
        }
    }
    pools_from_pairs(ctx, pairs, usize::try_from(n).unwrap_or(usize::MAX), venue).await
}

/// Re-read the reserves of pools already in the directory, in one batched call.
///
/// The directory answers two different questions at two different speeds. *Which pools exist*,
/// with their tokens and decimals, changes when somebody deploys a pair. *What is in them* changes
/// every block, and that is what price and TVL are read off. Rebuilding the whole directory to
/// pick up a price move meant the fast number moved at the slow number's pace — and on the
/// explorer that pace is a hard floor, because `/api/stats/tvl` is served `max-age=30`.
///
/// So reserves come from the node instead: one `getReserves()` per pool in a single multicall,
/// which is one round trip whatever the directory holds. Verified against the explorer on the
/// WQI/WQUAI pair, the two agree to the atom.
///
/// Failed or malformed reads are omitted; successful zero reserves explicitly empty the pool.
pub async fn refresh_reserves(ctx: &DataCtx, pools: &[Pool]) -> Result<Vec<(String, f64, f64)>> {
    use crate::multicall::Call;
    ctx.online()?;
    // A curve has no pair to ask, and no reserves to read.
    let pairs: Vec<&Pool> = pools.iter().filter(|p| p.venue.routable()).collect();
    if pairs.is_empty() {
        return Ok(Vec::new());
    }
    let mc = crate::multicall::Multicall::open(ctx).await.ok_or_else(|| CoreError::NotFound("live reserves need Multicall3".into()))?;
    let calls: Vec<Call> = pairs.iter().map(|p| Call::view(&p.address, "getReserves()", &[])).collect();
    let out = mc.try_all(&calls).await?;
    Ok(pairs
        .iter()
        .zip(out)
        .filter_map(|(p, data)| {
            let (r0, r1) = decoded_reserves(data.as_deref()?, p.token0.decimals, p.token1.decimals)?;
            Some((p.address.clone(), r0, r1))
        })
        .collect())
}

fn decoded_reserves(data: &[u8], decimals0: u8, decimals1: u8) -> Option<(f64, f64)> {
    if data.len() < 96 {
        return None;
    }
    let (r0, r1) = (units(crate::multicall::word(data, 0), decimals0), units(crate::multicall::word(data, 1), decimals1));
    (r0.is_finite() && r1.is_finite()).then_some((r0, r1))
}

/// Apply freshly read reserves, and reprice what is computed from them.
///
/// Returns how many pools were updated, so a caller can tell a refresh that landed from one that
/// matched nothing.
pub fn apply_reserves(pools: &mut [Pool], fresh: &[(String, f64, f64)], wquai: Option<&str>, usd_per_quai: Option<f64>) -> usize {
    let by: std::collections::HashMap<&str, (f64, f64)> = fresh.iter().map(|(a, r0, r1)| (a.as_str(), (*r0, *r1))).collect();
    let mut hit = 0;
    for p in pools.iter_mut() {
        if let Some((r0, r1)) = by.get(p.address.as_str()) {
            (p.reserve0, p.reserve1) = (*r0, *r1);
            hit += 1;
        }
        // Only where the identity holds. A pair with no QUAI side (WQI/USDT) keeps whatever the
        // directory gave it: stale by a refresh, but right, which beats fresh and invented.
        if let Some(tvl) = amm_tvl(p, wquai, usd_per_quai) {
            p.tvl_usd = Some(tvl);
        }
    }
    hit
}

/// Turn a list of pair addresses into priced [`Pool`] rows. `total` is how many the source
/// holds, so a directory can say when it is showing fewer.
async fn pools_from_pairs(ctx: &DataCtx, pairs: Vec<String>, total: usize, venue: Venue) -> Result<Directory> {
    let batched = crate::multicall::Multicall::open(ctx).await;
    let caller: QuaiAddress = READ_CALLER.parse().map_err(|_| CoreError::Invalid("caller".into()))?;
    let read = pairs.len();
    // Each pair's two tokens and its reserves: one batch, or four calls each.
    let mut raw: Vec<(String, String, String, U256, U256)> = Vec::new();
    if let Some(mc) = &batched {
        use crate::multicall::{Call, address_word, word};
        let mut calls = Vec::with_capacity(pairs.len() * 3);
        for pair in &pairs {
            calls.push(Call::view(pair, "token0()", &[]));
            calls.push(Call::view(pair, "token1()", &[]));
            calls.push(Call::view(pair, "getReserves()", &[]));
        }
        let out = mc.try_all(&calls).await?;
        for (i, pair) in pairs.iter().enumerate() {
            let t0 = out.get(i * 3).and_then(Option::as_ref).map(|d| address_word(d, 0)).unwrap_or_default();
            let t1 = out.get(i * 3 + 1).and_then(Option::as_ref).map(|d| address_word(d, 0)).unwrap_or_default();
            let reserves = out.get(i * 3 + 2).and_then(Option::as_ref);
            if t0.is_empty() || t1.is_empty() {
                continue;
            }
            let (r0, r1) = reserves.map_or((U256::ZERO, U256::ZERO), |d| (word(d, 0), word(d, 1)));
            raw.push((pair.clone(), t0, t1, r0, r1));
        }
    } else {
        for pair in &pairs {
            let pa: QuaiAddress = pair.parse().map_err(|_| CoreError::Invalid("pair address".into()))?;
            let pc = Contract::new(
                pa,
                AbiInterface::from_human_readable(PAIR_READ_ABI).map_err(|e| CoreError::Invalid(format!("pair abi: {e}")))?,
                &ctx.node.provider,
            );
            let t0 =
                pc.call(caller, "token0", &[], BlockTag::Latest).await?.first().and_then(Value::as_str).unwrap_or_default().to_lowercase();
            let t1 =
                pc.call(caller, "token1", &[], BlockTag::Latest).await?.first().and_then(Value::as_str).unwrap_or_default().to_lowercase();
            let reserves = pc.call(caller, "getReserves", &[], BlockTag::Latest).await?;
            let r = |i: usize| reserves.get(i).and_then(Value::as_str).and_then(|s| U256::from_str_radix(s, 10).ok()).unwrap_or_default();
            raw.push((pair.clone(), t0, t1, r(0), r(1)));
        }
    }
    // Every token's symbol and decimals in one round, rather than three calls per side per pair.
    let tokens: Vec<String> = raw.iter().flat_map(|(_, t0, t1, ..)| [t0.clone(), t1.clone()]).collect();
    let meta = token_meta_all(ctx, &tokens).await;
    let named = |address: &str| meta.get(&address.to_lowercase()).cloned().unwrap_or_else(|| unnamed(address));
    let mut pools = Vec::new();
    for (pair, t0, t1, r0, r1) in raw {
        let (token0, token1) = (named(&t0), named(&t1));
        pools.push(Pool {
            address: pair,
            reserve0: units(r0, token0.decimals),
            reserve1: units(r1, token1.decimals),
            token0,
            token1,
            tvl_usd: None,
            volume_24h_usd: None,
            venue,
            curve: None,
            spot_24h_ago: None,
        });
    }
    Ok(Directory { pools, total, read, fetched_at: now(), stale: false })
}

/// Swap and Sync events of a pool since `since` (unix seconds), oldest first. Explorer pages stop
/// at what is already recorded, so each refresh only fetches what is new; at most `max_pages`
/// pages (100 logs each) are read per call.
///
/// What was read before lives in `pool_events` rows, added by log position. Recording a page the
/// wallet (or another process) already has is a no-op rather than a rewrite of the whole history.
pub async fn pool_events(ctx: &DataCtx, pool: &Pool, since: u64, max_pages: usize) -> Result<Vec<PoolEvent>> {
    if ctx.cache_only {
        return read_pool_events(ctx, pool, since);
    }
    shared_history_refresh(ctx, pool, since, || refresh_pool_events(ctx, pool, since, max_pages)).await
}

struct HistoryLease<'a> {
    store: &'a crate::appdb::AppDb,
    key: String,
}

impl Drop for HistoryLease<'_> {
    fn drop(&mut self) {
        let _ = self.store.release_fetch(&self.key);
    }
}

async fn shared_history_refresh<F, Fut>(ctx: &DataCtx, pool: &Pool, since: u64, refresh: F) -> Result<Vec<PoolEvent>>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<Vec<PoolEvent>>>,
{
    let key = format!("{}:refresh", history_key(ctx, pool));
    let fresh = || -> Result<bool> {
        Ok(ctx.feeds().cache_get(&key)?.is_some_and(|(covered_since, at)| {
            now().saturating_sub(at) < 5 && covered_since.parse::<u64>().is_ok_and(|covered| covered <= since)
        }))
    };
    if fresh()? {
        return read_pool_events(ctx, pool, since);
    }
    if !ctx.feeds().claim_fetch(&key)? {
        // Empty intervals also get a completion stamp, so a second wallet can reuse them.
        for _ in 0..10 {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            if fresh()? {
                return read_pool_events(ctx, pool, since);
            }
        }
        return Err(CoreError::NotFound("pool history refresh is in progress; retry shortly".into()));
    }
    let _lease = HistoryLease { store: ctx.feeds(), key: key.clone() };
    if fresh()? {
        return read_pool_events(ctx, pool, since);
    }
    let events = refresh().await?;
    ctx.feeds().cache_put(&key, &since.to_string())?;
    Ok(events)
}

async fn refresh_pool_events(ctx: &DataCtx, pool: &Pool, since: u64, max_pages: usize) -> Result<Vec<PoolEvent>> {
    let network = &ctx.network.id;
    let horizon = now().saturating_sub(crate::appdb::FEED_KEEP);
    let known = read_pool_events(ctx, pool, horizon.min(since))?;
    let fresh = if pool.venue == Venue::Curve {
        curve_events(ctx, pool, since, max_pages, &known).await?
    } else if ctx.policy.market && ctx.explorer.source() == "explorer.qu.ai" {
        explorer_events(ctx, pool, since, max_pages, &known).await?
    } else {
        chain_events(ctx, pool, since).await?
    };
    let rows: Vec<(u64, u64, u64, String)> = fresh
        .iter()
        .filter(|e| e.position().0 >= horizon)
        .filter_map(|e| {
            let (at, block, index) = e.position();
            serde_json::to_string(e).ok().map(|text| (block, index, at, text))
        })
        .collect();
    ctx.feeds().add_pool_events(network, &pool.address, &rows)?;
    if pool.venue == Venue::Curve || (pool.venue.routable() && ctx.policy.market && ctx.explorer.source() == "explorer.qu.ai") {
        canonical_pool_tail(ctx, pool).await?;
    }
    read_pool_events(ctx, pool, since)
}

/// Recorded events for a pool at or after `since`, oldest first. A row that no longer parses (an
/// older shape) is skipped rather than failing the read: the chart draws what it can.
fn read_pool_events(ctx: &DataCtx, pool: &Pool, since: u64) -> Result<Vec<PoolEvent>> {
    Ok(ctx.feeds().pool_events(&ctx.network.id, &pool.address, since)?.iter().filter_map(|text| serde_json::from_str(text).ok()).collect())
}

/// Persisted scan coverage, independent of whether any trade happened in the scanned interval.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryCoverage {
    pub since: u64,
    pub complete: bool,
    pub cursor: Option<String>,
    pub tail_cursor: Option<String>,
    pub tail_until: Option<(u64, u64, u64)>,
    pub through: u64,
    pub upper: u64,
    #[serde(default)]
    pub canonical: Option<CanonicalCoverage>,
}

/// Canonical positions checked so far; index pagination and event-amount verification are
/// separate claims. Curves retain indexed economic values, with canonical transaction/log
/// inclusion and exact block timestamps checked by the node.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CanonicalCoverage {
    pub from_block: u64,
    pub through_block: u64,
    pub through_hash: String,
    pub checked_at: u64,
    pub dirty: bool,
    pub revision: u64,
    pub amounts_verified: bool,
    pub oldest_recorded_block: Option<u64>,
    pub newest_recorded_block: Option<u64>,
}

impl CanonicalCoverage {
    pub fn covers_recorded_positions(&self) -> bool {
        !self.dirty
            && self.oldest_recorded_block.is_none_or(|b| b >= self.from_block)
            && self.newest_recorded_block.is_none_or(|b| b <= self.through_block)
    }
}

fn canonical_key(ctx: &DataCtx, pool: &Pool) -> String {
    format!("{}:pool_canonical_v2:{}", ctx.network.id, pool.address.to_lowercase())
}

fn history_key(ctx: &DataCtx, pool: &Pool) -> String {
    use sha2::{Digest, Sha256};
    let source = if pool.venue == Venue::Curve {
        ctx.network.ecosystem.launch_subgraph.as_deref().unwrap_or_default().to_owned()
    } else {
        ctx.explorer.absolute("")
    };
    format!("{}:history_scan_v2:{}:{}", ctx.network.id, pool.address.to_lowercase(), hex::encode(Sha256::digest(source.as_bytes())))
}

pub fn pool_history_coverage(ctx: &DataCtx, pool: &Pool) -> Result<Option<HistoryCoverage>> {
    let mut state: Option<HistoryCoverage> =
        ctx.feeds().cache_get(&history_key(ctx, pool))?.and_then(|(s, _)| serde_json::from_str(&s).ok());
    if let Some(state) = &mut state {
        state.canonical = ctx.feeds().cache_get(&canonical_key(ctx, pool))?.and_then(|(s, _)| serde_json::from_str(&s).ok());
        if let Some(canonical) = &mut state.canonical {
            let blocks = ctx.feeds().pool_event_blocks(&ctx.network.id, &pool.address)?;
            canonical.oldest_recorded_block = blocks.map(|b| b.0);
            canonical.newest_recorded_block = blocks.map(|b| b.1);
        }
    }
    Ok(state)
}

fn history_rows(events: &[PoolEvent]) -> Result<Vec<(u64, u64, u64, String)>> {
    events
        .iter()
        .map(|event| {
            let (at, block, index) = event.position();
            Ok((block, index, at, serde_json::to_string(event).map_err(|e| CoreError::Invalid(format!("event serialization: {e}")))?))
        })
        .collect()
}

fn save_history_page(ctx: &DataCtx, pool: &Pool, state: &HistoryCoverage, events: &[PoolEvent]) -> Result<()> {
    let key = history_key(ctx, pool);
    let checkpoint = serde_json::to_string(state).map_err(|e| CoreError::Invalid(e.to_string()))?;
    ctx.feeds().record_pool_scan(&ctx.network.id, &pool.address, &history_rows(events)?, None, Some((&key, &checkpoint)))?;
    Ok(())
}

/// Advance only after the page is durable. A page with no decoded swaps can still have more logs.
fn advance_backfill(state: &mut HistoryCoverage, next: Option<String>, oldest: Option<u64>) {
    if next.is_none() || oldest.is_some_and(|at| at < state.since) {
        state.complete = true;
        state.cursor = None;
    } else {
        state.cursor = next;
    }
}

async fn explorer_page(ctx: &DataCtx, pool: &Pool, cursor: Option<&str>) -> Result<(Vec<PoolEvent>, Option<String>, Option<u64>)> {
    let mut url = reqwest::Url::parse(&ctx.explorer.absolute(&format!("/api/address/{}/logs", pool.address)))
        .map_err(|e| CoreError::Invalid(format!("explorer URL: {e}")))?;
    url.query_pairs_mut().append_pair("limit", "100");
    if let Some(cursor) = cursor {
        url.query_pairs_mut().append_pair("cursor", cursor);
    }
    let body = crate::http::get_json(url.as_str()).await?;
    let items = body["items"].as_array().ok_or_else(|| CoreError::Network("explorer log page has no items".into()))?;
    let oldest = items.iter().filter_map(|item| item["timestamp"].as_str().and_then(parse_timestamp)).min();
    let (events, next) = parse_explorer_logs(&body);
    if body["hasMore"].as_bool() == Some(true) && next.is_none() {
        return Err(CoreError::Network("explorer log page omitted its continuation cursor".into()));
    }
    if cursor.is_some() && next.as_deref() == cursor {
        return Err(CoreError::Network("explorer log cursor did not advance".into()));
    }
    Ok((events, next, oldest))
}

async fn explorer_events(ctx: &DataCtx, pool: &Pool, since: u64, max_pages: usize, cached: &[PoolEvent]) -> Result<Vec<PoolEvent>> {
    scan_explorer_pages(ctx, pool, since, max_pages, cached, |cursor| async move { explorer_page(ctx, pool, cursor.as_deref()).await })
        .await
}

async fn scan_explorer_pages<F, Fut>(
    ctx: &DataCtx,
    pool: &Pool,
    since: u64,
    max_pages: usize,
    cached: &[PoolEvent],
    mut page: F,
) -> Result<Vec<PoolEvent>>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: std::future::Future<Output = Result<(Vec<PoolEvent>, Option<String>, Option<u64>)>>,
{
    let mut state =
        pool_history_coverage(ctx, pool)?.filter(|s| s.since <= since).unwrap_or(HistoryCoverage { since, ..HistoryCoverage::default() });
    let budget = max_pages.max(1);
    let mut used = 0;
    // Tail has its own stable overlap marker while a multi-page catch-up is in progress.
    // The other half of the budget continues historical coverage instead of restarting page one.
    if state.complete || (budget > 1 && !cached.is_empty()) {
        if state.tail_cursor.is_none() {
            state.tail_until = cached.iter().map(PoolEvent::position).max();
        }
        let tail_budget = if state.complete { budget } else { (budget / 2).max(1) };
        for _ in 0..tail_budget {
            let (events, next, oldest) = page(state.tail_cursor.clone()).await?;
            used += 1;
            let overlap = state.tail_until.is_some_and(|at| events.iter().any(|e| e.position() <= at));
            let done = overlap || next.is_none() || oldest.is_some_and(|at| at < state.since);
            state.tail_cursor = if done { None } else { next };
            if done {
                state.tail_until = None;
                state.through = now();
            }
            save_history_page(ctx, pool, &state, &events)?;
            if done {
                break;
            }
        }
    }
    while !state.complete && used < budget {
        let (events, next, oldest) = page(state.cursor.clone()).await?;
        used += 1;
        advance_backfill(&mut state, next, oldest);
        if state.complete {
            state.through = now();
        }
        save_history_page(ctx, pool, &state, &events)?;
    }
    Ok(Vec::new()) // Each page and its progress were already committed together.
}

/// A fixed time window and stable entity ID cursor avoid offset shifts while new trades arrive.
async fn curve_events(ctx: &DataCtx, pool: &Pool, since: u64, max_pages: usize, _cached: &[PoolEvent]) -> Result<Vec<PoolEvent>> {
    if !ctx.policy.market {
        return Err(CoreError::NotFound("market data is off".into()));
    }
    let url = ctx
        .network
        .ecosystem
        .launch_subgraph
        .clone()
        .ok_or_else(|| CoreError::NotFound(format!("no launch index on {}", ctx.network.name)))?;
    let mut state = pool_history_coverage(ctx, pool)?.filter(|s| s.since <= since).unwrap_or(HistoryCoverage {
        since,
        upper: now(),
        ..HistoryCoverage::default()
    });
    if state.cursor.is_none() {
        state.upper = now();
    }
    let from = if state.complete { state.through.saturating_sub(60).max(state.since) } else { state.since };
    for _ in 0..max_pages.max(1) {
        let query = json!({"query": CURVE_TRADES, "variables": {
            "curve": pool.address.to_lowercase(), "since": from.to_string(), "until": state.upper.to_string(),
            "first": CURVE_PAGE, "after": state.cursor.as_deref().unwrap_or("")
        }});
        let body = crate::http::post_json(&url, &query).await?;
        if let Some(errors) = body["errors"].as_array().filter(|e| !e.is_empty()) {
            return Err(CoreError::Network(format!(
                "launch index: {}",
                clean_text(errors[0]["message"].as_str().unwrap_or("query failed"))
            )));
        }
        let rows =
            body["data"]["tradeCurveTrades"].as_array().ok_or_else(|| CoreError::Network("launch index omitted trade rows".into()))?;
        let events = parse_curve_trades(&body, pool);
        if events.iter().filter(|e| matches!(e, PoolEvent::Swap { .. })).count() != rows.len() {
            return Err(CoreError::Network("launch index returned malformed trades; cursor was not advanced".into()));
        }
        let done = rows.len() < CURVE_PAGE;
        if done {
            state.complete = true;
            state.through = state.upper;
            state.cursor = None;
        } else {
            let next =
                rows.last().and_then(|r| r["id"].as_str()).ok_or_else(|| CoreError::Network("launch index omitted trade cursor".into()))?;
            if state.cursor.as_deref().is_some_and(|old| next <= old) {
                return Err(CoreError::Network("launch index trade cursor did not advance".into()));
            }
            state.cursor = Some(next.to_string());
        }
        save_history_page(ctx, pool, &state, &events)?;
        if done {
            break;
        }
    }
    Ok(Vec::new())
}

const CURVE_PAGE: usize = 1000;
const CURVE_TRADES: &str = "query($curve: String!, $since: BigInt!, $until: BigInt!, $first: Int!, $after: String!) { tradeCurveTrades(where: {sourceContract: $curve, timestamp_gte: $since, timestamp_lte: $until, id_gt: $after}, orderBy: id, orderDirection: asc, first: $first) { id side account tokenAmount quoteAmount priceQuoteE12 transactionHash blockNumber timestamp logIndex } }";

pub fn parse_curve_trades(body: &Value, pool: &Pool) -> Vec<PoolEvent> {
    let int = |v: &Value| v.as_str().and_then(|s| U256::from_str_radix(s, 10).ok());
    let num = |v: &Value| v.as_str().and_then(|s| s.parse::<u64>().ok());
    let Some(unit0) = U256::from(10u64).checked_pow(U256::from(12 + u64::from(pool.token0.decimals))) else { return Vec::new() };
    let Some(scale1) = U256::from(10u64).checked_pow(U256::from(u64::from(pool.token1.decimals))) else { return Vec::new() };
    body["data"]["tradeCurveTrades"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|t| {
                    let (tokens, quote) = (int(&t["tokenAmount"])?, int(&t["quoteAmount"])?);
                    let (at, block, index) = (num(&t["timestamp"])?, num(&t["blockNumber"])?, num(&t["logIndex"])?);
                    let index = index.checked_mul(2)?;
                    let tx = t["transactionHash"].as_str()?.to_lowercase();
                    let buy = match t["side"].as_str()? {
                        "BUY" => true,
                        "SELL" => false,
                        _ => return None,
                    };
                    let trader = t["account"].as_str().unwrap_or_default().to_lowercase();
                    let (amount0_in, amount1_in, amount0_out, amount1_out) =
                        if buy { (U256::ZERO, quote, tokens, U256::ZERO) } else { (tokens, U256::ZERO, U256::ZERO, quote) };
                    let mut events = vec![PoolEvent::Swap {
                        at,
                        block,
                        tx: tx.clone(),
                        index,
                        amount0_in,
                        amount1_in,
                        amount0_out,
                        amount1_out,
                        to: trader,
                    }];
                    if let Some(reserve1) = int(&t["priceQuoteE12"]).filter(|p| !p.is_zero()).and_then(|price| price.checked_mul(scale1)) {
                        events.push(PoolEvent::Sync { at, block, tx, index: index.checked_add(1)?, reserve0: unit0, reserve1 });
                    }
                    Some(events)
                })
                .flatten()
                .collect()
        })
        .unwrap_or_default()
}

/// Replay recent blocks authoritatively, including empty replacement blocks. A deeper fork
/// detected at the previous checkpoint widens replay to the retained recent scan window.
const REORG_OVERLAP: u64 = 32;

/// Each call extends canonical history by a bounded older chunk while replaying the tail.
/// On a fork, discard the old coverage claim and progressively rebuild it.
fn canonical_ranges(head: u64, previous: Option<&CanonicalCoverage>, oldest: Option<u64>) -> Vec<(u64, u64)> {
    let tail = previous
        .map_or(head.saturating_sub(REORG_OVERLAP), |p| {
            p.through_block.saturating_sub(REORG_OVERLAP).min(head.saturating_sub(REORG_OVERLAP))
        })
        .max(head.saturating_sub(9_999));
    let covered_from = previous.map_or(tail, |p| p.from_block.min(tail));
    let floor = oldest.unwrap_or(covered_from);
    if floor >= covered_from {
        return vec![(tail, head)];
    }
    let older = covered_from.saturating_sub(256).max(floor);
    if older < covered_from && covered_from < tail { vec![(older, covered_from - 1), (tail, head)] } else { vec![(older.min(tail), head)] }
}

async fn canonical_pool_tail(ctx: &DataCtx, pool: &Pool) -> Result<()> {
    let head = ctx
        .node
        .provider
        .latest_header(crate::network::ZONE)
        .await?
        .ok_or_else(|| CoreError::Network("no head for history reconciliation".into()))?;
    let key = canonical_key(ctx, pool);
    let original = ctx.feeds().cache_get(&key)?.map(|(value, _)| value);
    let mut previous: Option<CanonicalCoverage> = original.as_deref().and_then(|value| serde_json::from_str(value).ok());
    // A coverage record with `through_block` 0 carries no anchor. `add_pool_events` deliberately
    // writes a bare `{dirty, revision}` record when an index page lands on a scan that has not
    // checkpointed yet, so that the scan cannot later call its ranges checked; `through_block`
    // then defaults to 0. Treating that as a real anchor asked the node for block 0, and the
    // genesis header's location is empty by design (the ordinary header parser refuses it), so
    // every market-history scan died with "invalid zone header location".
    if previous.as_ref().is_some_and(|p| p.through_block == 0) {
        previous = None;
    }
    if let Some(p) = &previous {
        let canonical = ctx.node.provider.header_at(crate::network::ZONE, p.through_block).await?;
        if head.number.saturating_sub(p.through_block) > 9_999 || canonical.is_none_or(|h| h.hash.to_string() != p.through_hash) {
            previous = None;
        }
    }
    let blocks = ctx.feeds().pool_event_blocks(&ctx.network.id, &pool.address)?;
    let ranges = canonical_ranges(head.number, previous.as_ref(), blocks.map(|b| b.0));
    let mut events = Vec::new();
    for (from, through) in &ranges {
        events.extend(read_chain_range(ctx, pool, *from, *through).await?);
    }
    let checked = ctx.node.provider.header_at(crate::network::ZONE, head.number).await?;
    if checked.is_none_or(|h| h.hash != head.hash) {
        return Err(CoreError::Network("chain changed during market history scan; retry".into()));
    }
    let coverage = CanonicalCoverage {
        from_block: ranges.iter().map(|r| r.0).min().unwrap_or(head.number).min(previous.as_ref().map_or(head.number, |p| p.from_block)),
        through_block: head.number,
        through_hash: head.hash.to_string(),
        checked_at: now(),
        dirty: false,
        revision: previous.as_ref().map_or(0, |p| p.revision),
        amounts_verified: pool.venue != Venue::Curve,
        oldest_recorded_block: blocks.map(|b| b.0),
        newest_recorded_block: blocks.map(|b| b.1),
    };
    let checkpoint = serde_json::to_string(&coverage).map_err(|e| CoreError::Invalid(e.to_string()))?;
    ctx.feeds().record_pool_scan_ranges_checked(
        &ctx.network.id,
        &pool.address,
        &history_rows(&events)?,
        &ranges,
        Some((&key, &checkpoint)),
        Some((&key, original.as_deref())),
    )?;
    Ok(())
}

/// Match the indexed curve trade's real log position (two display rows share one chain log).
/// Economic amounts remain explicitly indexed; this proves inclusion and repairs timestamps.
fn reconcile_curve_positions(events: Vec<PoolEvent>, canonical: &std::collections::HashMap<(u64, String, u64), u64>) -> Vec<PoolEvent> {
    events
        .into_iter()
        .filter_map(|mut event| {
            let (at, block, tx, index) = match &mut event {
                PoolEvent::Swap { at, block, tx, index, .. } | PoolEvent::Sync { at, block, tx, index, .. } => (at, block, tx, index),
            };
            *at = *canonical.get(&(*block, tx.to_ascii_lowercase(), *index / 2))?;
            Some(event)
        })
        .collect()
}

async fn read_chain_range(ctx: &DataCtx, pool: &Pool, from: u64, to: u64) -> Result<Vec<PoolEvent>> {
    use futures::StreamExt;
    use quai_sdk::provider::{LogFilter, LogRange, TopicMatch};
    let address: QuaiAddress = pool.address.parse().map_err(|_| CoreError::Invalid("pair address".into()))?;
    let filter = LogFilter {
        zone: crate::network::ZONE,
        range: LogRange::Inclusive { from, to },
        addresses: vec![address.address()],
        topics: if pool.venue == Venue::Curve {
            Vec::new()
        } else {
            vec![TopicMatch::AnyOf([SWAP_TOPIC, SYNC_TOPIC].iter().filter_map(|t| t.parse().ok()).collect())]
        },
    };
    let logs = ctx.node.provider.logs(&filter).await?;
    let mut blocks: Vec<u64> = logs.iter().filter(|l| !l.removed).map(|l| l.inclusion.block_number).collect();
    blocks.sort_unstable();
    blocks.dedup();
    let headers = futures::stream::iter(blocks.into_iter().map(|block| async move {
        let header = ctx
            .node
            .provider
            .header_at(crate::network::ZONE, block)
            .await?
            .ok_or_else(|| CoreError::Network("missing market event block".into()))?;
        let at = crate::network::header_time(&header).ok_or_else(|| CoreError::Network("missing market event timestamp".into()))?;
        Ok::<_, CoreError>((block, (at, header.hash)))
    }))
    .buffer_unordered(4)
    .collect::<Vec<_>>()
    .await;
    let times: std::collections::HashMap<_, _> = headers.into_iter().collect::<Result<_>>()?;
    let mut events = Vec::new();
    let mut curve_positions = std::collections::HashMap::new();
    for log in logs.iter().filter(|l| !l.removed) {
        let Some((at, hash)) = times.get(&log.inclusion.block_number) else { continue };
        if log.inclusion.block_hash != *hash {
            return Err(CoreError::Network("market log is no longer canonical; retry".into()));
        }
        if pool.venue == Venue::Curve {
            curve_positions.insert((log.inclusion.block_number, log.transaction_hash.to_string().to_ascii_lowercase(), log.log_index), *at);
            continue;
        }
        let topics: Vec<String> = log.topics.iter().map(ToString::to_string).collect();
        let event =
            decode_log(&topics, &log.data.to_hex(), *at, log.inclusion.block_number, &log.transaction_hash.to_string(), log.log_index)
                .ok_or_else(|| CoreError::Network("malformed market event".into()))?;
        events.push(event);
    }
    if pool.venue == Venue::Curve {
        let indexed = ctx.feeds().pool_events_in_blocks(&ctx.network.id, &pool.address, from, to)?;
        let indexed = indexed
            .into_iter()
            .map(|text| serde_json::from_str(&text).map_err(|e| CoreError::Invalid(format!("indexed curve event: {e}"))))
            .collect::<Result<Vec<_>>>()?;
        return Ok(reconcile_curve_positions(indexed, &curve_positions));
    }
    Ok(events)
}

/// Events from the node over the recent block range (≤ 10,000 blocks), with block timestamps.
async fn chain_events(ctx: &DataCtx, pool: &Pool, since: u64) -> Result<Vec<PoolEvent>> {
    let key = canonical_key(ctx, pool);
    let previous = ctx.feeds().cache_get(&key)?.map(|(value, _)| value);
    let bootstrapped = previous
        .as_deref()
        .and_then(|value| serde_json::from_str::<CanonicalCoverage>(value).ok())
        .is_some_and(|coverage| !coverage.through_hash.is_empty());
    if bootstrapped {
        canonical_pool_tail(ctx, pool).await?;
        return Ok(Vec::new()); // Durable canonical rows are returned by the shared reader.
    }
    let head = ctx.node.provider.latest_header(crate::network::ZONE).await?.ok_or_else(|| CoreError::Network("no head".into()))?;
    let from = head.number.saturating_sub(9_999);
    let events = read_chain_range(ctx, pool, from, head.number).await?;
    let checked = ctx.node.provider.header_at(crate::network::ZONE, head.number).await?;
    if checked.is_none_or(|h| h.hash != head.hash) {
        return Err(CoreError::Network("chain changed during market scan; retry".into()));
    }
    let coverage = CanonicalCoverage {
        from_block: from,
        through_block: head.number,
        through_hash: head.hash.to_string(),
        checked_at: now(),
        amounts_verified: true,
        ..CanonicalCoverage::default()
    };
    let checkpoint = serde_json::to_string(&coverage).map_err(|e| CoreError::Invalid(e.to_string()))?;
    ctx.feeds().record_pool_scan_ranges_checked(
        &ctx.network.id,
        &pool.address,
        &history_rows(&events)?,
        &[(from, head.number)],
        Some((&key, &checkpoint)),
        Some((&key, previous.as_deref())),
    )?;
    Ok(events.into_iter().filter(|e| e.position().0 >= since).collect())
}

// ------------------------------------------------------------------- the DEX-wide tape

/// Blocks read when the tape is built from nothing (Cyprus-1 mines about one every 5 s).
///
/// 150 blocks is about twelve minutes, which is plenty for the main exchange and far too little
/// for everything else: a launch-AMM pair or one of the shortlisted QuaiSwap pairs can easily go
/// that long without a trade, so the tape looked like the main pools were the only market on the
/// network. 900 blocks is roughly the last hour and a quarter, which is long enough for a quiet
/// pair to show its last trades.
///
/// The width costs only on the first build. After that the read starts from the block after the
/// newest one already recorded, so a refresh covers whatever arrived since — normally one block.
pub const FLOW_BLOCKS: u64 = 900;
/// Swaps kept in the tape.
pub const FLOW_KEEP: usize = 200;
/// Block headers read per refresh to put exact times on new swaps.
const FLOW_TIMES: usize = 20;
/// Pool addresses one `quai_getLogs` filter may name. A node's own limit, so a directory with more
/// pools than this is read in several filtered calls rather than truncated.
pub const FLOW_FILTER_ADDRESSES: usize = 128;
/// Zone block spacing, for timing a swap before its header is read.
const BLOCK_SECONDS: u64 = 5;

/// One swap anywhere on the DEX, in the direction the trader took it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DexSwap {
    /// Unix seconds; estimated from the height until the block's header is read (`timed`).
    pub at: u64,
    /// `at` came from the block header, not from an estimate.
    pub timed: bool,
    /// Block height.
    pub block: u64,
    /// Transaction hash.
    pub tx: String,
    /// Log position in the block.
    pub index: u64,
    /// Pair contract (lowercase).
    pub pool: String,
    /// What the trader paid.
    pub token_in: PoolToken,
    /// What the trader received.
    pub token_out: PoolToken,
    /// Paid, in token units.
    pub amount_in: f64,
    /// Received, in token units.
    pub amount_out: f64,
    /// Recipient of the output.
    pub trader: String,
}

impl DexSwap {
    /// Newest first: descending block, then log position.
    pub fn position(&self) -> (u64, u64) {
        (self.block, self.index)
    }

    /// Whether this trade bought the pair's base token.
    pub fn buys(&self, base: &str) -> bool {
        self.token_out.address.eq_ignore_ascii_case(base)
    }

    /// Amounts as (base, quote) for the pair's base token.
    pub fn sides(&self, base: &str) -> (f64, f64) {
        if self.buys(base) { (self.amount_out, self.amount_in) } else { (self.amount_in, self.amount_out) }
    }

    /// Price in quote units per base unit.
    pub fn price(&self, base: &str) -> Option<f64> {
        let (b, q) = self.sides(base);
        (b > 0.0).then(|| q / b).filter(|p| p.is_finite())
    }
}

/// The trader's side of one pool swap: what they paid and what they received. A pool reports
/// both sides of a hop, so each side is its net amount; `None` when neither side moved.
pub fn swap_sides(pool: &Pool, event: &PoolEvent) -> Option<(PoolToken, f64, PoolToken, f64)> {
    let PoolEvent::Swap { amount0_in, amount1_in, amount0_out, amount1_out, .. } = event else { return None };
    let (d0, d1) = (pool.token0.decimals, pool.token1.decimals);
    let (in0, out0) = (units(*amount0_in, d0), units(*amount0_out, d0));
    let (in1, out1) = (units(*amount1_in, d1), units(*amount1_out, d1));
    let (token_in, amount_in, token_out, amount_out) = if in0 > out0 {
        (pool.token0.clone(), in0 - out0, pool.token1.clone(), out1 - in1)
    } else {
        (pool.token1.clone(), in1 - out1, pool.token0.clone(), out0 - in0)
    };
    (amount_in > 0.0 && amount_out > 0.0).then_some((token_in, amount_in, token_out, amount_out))
}

/// Newest transaction first, but each route's hops in the order the trader took them: sorting
/// by log position alone would show a QGIRL → WQI → QUAI route ending first.
fn order_routes(tape: &mut [DexSwap]) {
    let mut start = 0;
    while start < tape.len() {
        let mut end = start + 1;
        while end < tape.len() && tape[end].tx == tape[start].tx {
            end += 1;
        }
        tape[start..end].reverse();
        start = end;
    }
}

/// Recent swaps across every pool, newest first: one `quai_getLogs` over all pair addresses,
/// merged into the cached tape so a refresh only reads the blocks since the last one. Times come
/// from the blocks' headers, at most [`FLOW_TIMES`] a call; the rest are estimated from the head
/// and corrected on a later refresh. Multi-hop routes appear as one row per pool they crossed.
pub async fn dex_flow(ctx: &DataCtx, pools: &[Pool], blocks: u64) -> Result<Vec<DexSwap>> {
    if ctx.cache_only || pools.is_empty() {
        return read_dex_flow(ctx);
    }
    use sha2::{Digest, Sha256};
    let mut addresses: Vec<_> = pools.iter().map(|p| p.address.to_ascii_lowercase()).collect();
    addresses.sort();
    addresses.dedup();
    let key = format!("dex_flow:{}:{}", hex::encode(Sha256::digest(addresses.join(",").as_bytes())), blocks);
    let result = ctx.cached(&key, 5, || refresh_dex_flow(ctx, pools, blocks)).await?;
    if result.stale {
        return Err(CoreError::Network("DEX flow source is not updating; showing the previous tape".into()));
    }
    Ok(result.value)
}

async fn refresh_dex_flow(ctx: &DataCtx, pools: &[Pool], blocks: u64) -> Result<Vec<DexSwap>> {
    use quai_sdk::provider::{LogFilter, LogRange, TopicMatch};
    let network = ctx.network.id.clone();
    if ctx.cache_only || pools.is_empty() {
        return read_dex_flow(ctx);
    }
    let head = ctx.node.provider.latest_header(crate::network::ZONE).await?.ok_or_else(|| CoreError::Network("no head".into()))?;
    let head_time = crate::network::header_time(&head).unwrap_or_else(now);
    let to = head.number;
    let floor = to.saturating_sub(blocks.max(1));
    use sha2::{Digest, Sha256};
    let mut pool_addresses: Vec<String> = pools.iter().filter(|p| p.venue.routable()).map(|p| p.address.to_lowercase()).collect();
    pool_addresses.sort();
    pool_addresses.dedup();
    let digest = hex::encode(Sha256::digest(pool_addresses.join(",").as_bytes()));
    let scan_key = format!("{network}:dex_scan_v1:{digest}");
    let checkpoint: Option<(u64, String)> = ctx.feeds().cache_get(&scan_key)?.and_then(|(text, _)| serde_json::from_str(&text).ok());
    let mut from = floor;
    if let Some((number, hash)) = checkpoint {
        let canonical = ctx.node.provider.header_at(crate::network::ZONE, number).await?;
        if canonical.is_some_and(|h| h.hash.to_string() == hash) {
            from = number.saturating_sub(REORG_OVERLAP).clamp(floor, to);
        }
    }
    let addresses: Vec<_> =
        pools.iter().filter(|p| p.venue.routable()).filter_map(|p| p.address.parse::<QuaiAddress>().ok().map(|a| a.address())).collect();
    // A node caps how many addresses one `quai_getLogs` filter may name, so a directory larger than
    // that is asked for in several filtered reads rather than quietly clipped to the first
    // [`FLOW_FILTER_ADDRESSES`] pools. They cover disjoint pools over the same block range, so they
    // run together and their logs merge.
    let reads = addresses.chunks(FLOW_FILTER_ADDRESSES).map(|chunk| {
        let filter = LogFilter {
            zone: crate::network::ZONE,
            range: LogRange::Inclusive { from, to },
            addresses: chunk.to_vec(),
            topics: vec![TopicMatch::AnyOf([SWAP_TOPIC].iter().filter_map(|t| t.parse().ok()).collect())],
        };
        async move { ctx.node.provider.logs(&filter).await }
    });
    use futures::StreamExt;
    let batches = futures::stream::iter(reads).buffer_unordered(4).collect::<Vec<_>>().await;
    let logs: Vec<_> = batches.into_iter().collect::<std::result::Result<Vec<_>, _>>()?.concat();
    let by_address: std::collections::HashMap<&str, &Pool> = pools.iter().map(|p| (p.address.as_str(), p)).collect();
    let mut fresh = Vec::new();
    for log in logs.iter().filter(|l| !l.removed) {
        let address = log.address.to_string().to_lowercase();
        let Some(pool) = by_address.get(address.as_str()) else { continue };
        let topics: Vec<String> = log.topics.iter().map(|t| t.to_string()).collect();
        let block = log.inclusion.block_number;
        let Some(event) = decode_log(&topics, &log.data.to_hex(), 0, block, &log.transaction_hash.to_string(), log.log_index) else {
            continue;
        };
        let PoolEvent::Swap { tx, index, to: trader, .. } = &event else { continue };
        let Some((token_in, amount_in, token_out, amount_out)) = swap_sides(pool, &event) else { continue };
        let (tx, index, trader) = (tx.clone(), *index, trader.clone());
        fresh.push(DexSwap {
            at: 0,
            timed: false,
            block,
            tx,
            index,
            pool: pool.address.clone(),
            token_in,
            token_out,
            amount_in,
            amount_out,
            trader,
        });
    }
    // Block times: a budget of headers, newest blocks first, for blocks that arrived in this
    // read and for ones recorded earlier whose time is still only an estimate.
    let mut times: std::collections::HashMap<u64, u64> = std::collections::HashMap::from([(to, head_time)]);
    let mut want: Vec<u64> = fresh
        .iter()
        .map(|s| s.block)
        .chain(ctx.feeds().dex_untimed_blocks(&network, FLOW_TIMES)?)
        .filter(|b| !times.contains_key(b))
        .collect();
    want.sort_unstable_by(|a, b| b.cmp(a));
    want.dedup();
    for block in want.into_iter().take(FLOW_TIMES) {
        let header = ctx.node.provider.header_at(crate::network::ZONE, block).await.ok().flatten();
        if let Some(t) = header.as_ref().and_then(crate::network::header_time) {
            times.insert(block, t);
        }
    }
    let estimate = |block: u64| head_time.saturating_sub(to.saturating_sub(block) * BLOCK_SECONDS);
    for s in &mut fresh {
        match times.get(&s.block) {
            Some(t) => (s.at, s.timed) = (*t, true),
            None => s.at = estimate(s.block),
        }
    }
    // Bonding-curve buys and sells, which no pool log carries. Best effort: the index being down
    // or switched off costs the curve rows, never the pool ones that were just read from the chain.
    // They land in the same store, so the tape orders them among the pool swaps by block and log
    // position rather than keeping a second list.
    if let Ok(curve) = crate::launches::curve_trades(ctx, FLOW_KEEP).await {
        fresh.extend(curve);
    }
    let rows: Vec<(String, u64, u64, u64, bool, String)> = fresh
        .iter()
        .filter_map(|s| serde_json::to_string(s).ok().map(|text| (s.tx.clone(), s.index, s.block, s.at, s.timed, text)))
        .collect();
    let checked = ctx.node.provider.header_at(crate::network::ZONE, to).await?;
    if checked.is_none_or(|h| h.hash != head.hash) {
        return Err(CoreError::Network("chain changed during DEX scan; retry".into()));
    }
    let checkpoint = serde_json::to_string(&(to, head.hash.to_string())).map_err(|e| CoreError::Invalid(e.to_string()))?;
    ctx.feeds().record_dex_scan(&network, &pool_addresses, from, to, &rows, (&scan_key, &checkpoint))?;
    // A block whose real time arrived late corrects every swap already recorded in it.
    for (block, at) in &times {
        let _ = ctx.feeds().time_dex_block(&network, *block, *at);
    }
    read_dex_flow(ctx)
}

/// The recorded tape, newest first, with each route's hops back in the order the trader took them.
fn read_dex_flow(ctx: &DataCtx) -> Result<Vec<DexSwap>> {
    let mut tape: Vec<DexSwap> =
        ctx.feeds().dex_swaps(&ctx.network.id, FLOW_KEEP)?.iter().filter_map(|text| serde_json::from_str(text).ok()).collect();
    tape.sort_by(|a, b| b.position().cmp(&a.position()));
    order_routes(&mut tape);
    Ok(tape)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn concurrent_wallets_share_an_empty_history_scan_and_cancelled_leases_release() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let dir = tempfile::tempdir().unwrap();
        let open = || {
            DataCtx::with_stores(
                crate::appdb::AppDb::memory().unwrap(),
                Some(crate::appdb::AppDb::open_shared(&dir.path().join("feeds.sqlite")).unwrap()),
                crate::network::NetworkProfile::builtins()[1].clone(),
                crate::config::DataPolicy::OFFLINE,
            )
            .unwrap()
        };
        let (a, b) = (open(), open());
        let pool = Pool { address: "0xpool".into(), ..Pool::default() };
        let count = Arc::new(AtomicUsize::new(0));
        let first = shared_history_refresh(&a, &pool, 100, || async {
            count.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(60)).await;
            Ok(Vec::new())
        });
        let second = async {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            shared_history_refresh(&b, &pool, 100, || async { panic!("follower must reuse the empty completed scan") }).await
        };
        let (x, y) = futures::join!(first, second);
        assert!(x.unwrap().is_empty() && y.unwrap().is_empty());
        assert_eq!(count.load(Ordering::SeqCst), 1);
        // A larger requested window cannot be hidden behind the smaller window's fresh stamp.
        shared_history_refresh(&b, &pool, 50, || async {
            count.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        })
        .await
        .unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 2);
        let cancelled = tokio::time::timeout(
            std::time::Duration::from_millis(10),
            shared_history_refresh(&a, &pool, 0, || async { futures::future::pending::<Result<Vec<PoolEvent>>>().await }),
        )
        .await;
        assert!(cancelled.is_err());
        shared_history_refresh(&b, &pool, 0, || async { Ok(Vec::new()) }).await.unwrap();
    }

    #[test]
    fn malformed_non_ascii_address_topics_never_slice_utf8() {
        assert_eq!(topic_address(&format!("0xa{}", "€".repeat(21))), "");
        assert_eq!(topic_address(&format!("0x{}", "z".repeat(64))), "");
        assert_eq!(topic_address(&format!("0x{}{}", "0".repeat(24), "a".repeat(40))), format!("0x{}", "a".repeat(40)));
    }

    #[test]
    fn canonical_backfill_is_bounded_contiguous_and_marks_unchecked_positions() {
        let initial = canonical_ranges(20_000, None, Some(100));
        assert_eq!(initial, [(19_712, 20_000)]);
        let previous = CanonicalCoverage {
            from_block: 19_000,
            through_block: 20_000,
            oldest_recorded_block: Some(100),
            newest_recorded_block: Some(20_000),
            ..CanonicalCoverage::default()
        };
        let resumed = canonical_ranges(20_010, Some(&previous), Some(100));
        assert_eq!(resumed, [(18_744, 18_999), (19_968, 20_010)]);
        assert!(!previous.covers_recorded_positions());
        let complete = CanonicalCoverage { from_block: 100, ..previous };
        assert!(complete.covers_recorded_positions());
        assert_eq!(canonical_ranges(20_010, Some(&complete), Some(100)), [(19_968, 20_010)]);
        let genesis_covered = CanonicalCoverage { from_block: 0, ..complete.clone() };
        assert_eq!(
            canonical_ranges(20_010, Some(&genesis_covered), Some(100)),
            [(19_968, 20_010)],
            "retained events inside covered history do not restart its scan"
        );
        assert!(!CanonicalCoverage { dirty: true, ..complete.clone() }.covers_recorded_positions());
        assert!(!CanonicalCoverage { newest_recorded_block: Some(20_001), ..complete }.covers_recorded_positions());
        assert_eq!(canonical_ranges(10, None, Some(0)), [(0, 10)]);
    }

    /// The bare record `add_pool_events` writes to invalidate an uncheckpointed scan carries no
    /// anchor, so `through_block` defaults to 0. Block 0 is the genesis header, whose location is
    /// empty by design and refused by the ordinary header parser, so using it as an anchor failed
    /// every market-history scan with "invalid zone header location".
    #[test]
    fn an_invalidation_record_carries_no_anchor_to_ask_the_node_about() {
        let db = crate::appdb::AppDb::memory().unwrap();
        let key = "net:pool_canonical_v2:pool";
        db.add_pool_events("net", "pool", &[(50, 0, 500, "trade".into())]).unwrap();
        let stored = db.cache_get(key).unwrap().expect("an uncheckpointed scan is invalidated").0;
        let coverage: CanonicalCoverage = serde_json::from_str(&stored).unwrap();
        assert!(coverage.dirty, "the record exists to mark the scan dirty");
        assert_eq!(coverage.through_block, 0, "and it establishes no canonical prefix");
        assert!(coverage.through_hash.is_empty(), "so there is nothing to compare a header against");
    }

    #[test]
    fn curve_history_malformed_indices_and_extreme_decimals_do_not_overflow() {
        let mut pool = Pool { venue: Venue::Curve, ..Pool::default() };
        let mut body = json!({"data":{"tradeCurveTrades":[{"side":"BUY", "tokenAmount":"1", "quoteAmount":"1",
            "transactionHash":"0x00", "blockNumber":"1", "timestamp":"1", "logIndex":u64::MAX.to_string()}]}});
        assert!(parse_curve_trades(&body, &pool).is_empty());
        body["data"]["tradeCurveTrades"][0]["logIndex"] = "0".into();
        pool.token0.decimals = 255;
        assert!(parse_curve_trades(&body, &pool).is_empty());
    }

    #[tokio::test]
    async fn capped_history_resumes_after_restart_and_tail_catches_multiple_pages() {
        use std::{cell::RefCell, rc::Rc};
        let dir = tempfile::tempdir().unwrap();
        let open = || {
            DataCtx::with_stores(
                crate::appdb::AppDb::memory().unwrap(),
                Some(crate::appdb::AppDb::open_shared(&dir.path().join("history.sqlite")).unwrap()),
                crate::network::NetworkProfile::builtins()[1].clone(),
                crate::config::DataPolicy::OFFLINE,
            )
            .unwrap()
        };
        let pool = Pool { address: "pool".into(), ..Pool::default() };
        let event =
            |at: u64| PoolEvent::Sync { at, block: at, tx: format!("tx{at}"), index: 0, reserve0: U256::from(1), reserve1: U256::from(at) };
        let seen = Rc::new(RefCell::new(Vec::new()));
        let page = |cursor: Option<String>| {
            seen.borrow_mut().push(cursor.clone());
            let response = match cursor.as_deref() {
                None => (vec![event(300)], Some("page2".into()), Some(300)),
                Some("page2") => (vec![event(200)], Some("page3".into()), Some(200)),
                Some("page3") => (vec![event(100)], None, Some(100)),
                _ => panic!("unexpected cursor"),
            };
            std::future::ready(Ok(response))
        };
        for _ in 0..3 {
            let ctx = open();
            let known = read_pool_events(&ctx, &pool, 100).unwrap();
            scan_explorer_pages(&ctx, &pool, 100, 1, &known, page).await.unwrap();
        }
        assert_eq!(*seen.borrow(), vec![None, Some("page2".into()), Some("page3".into())]);
        let ctx = open();
        assert!(pool_history_coverage(&ctx, &pool).unwrap().unwrap().complete);
        assert_eq!(read_pool_events(&ctx, &pool, 100).unwrap().len(), 3);
        let tail = |cursor: Option<String>| {
            std::future::ready(Ok(match cursor.as_deref() {
                None => (vec![event(500)], Some("tail2".into()), Some(500)),
                Some("tail2") => (vec![event(400)], Some("tail3".into()), Some(400)),
                Some("tail3") => (vec![event(300)], None, Some(300)),
                _ => panic!("unexpected tail cursor"),
            }))
        };
        for _ in 0..3 {
            let ctx = open();
            let known = read_pool_events(&ctx, &pool, 100).unwrap();
            scan_explorer_pages(&ctx, &pool, 100, 1, &known, tail).await.unwrap();
        }
        let ctx = open();
        let coverage = pool_history_coverage(&ctx, &pool).unwrap().unwrap();
        assert!(coverage.complete && coverage.tail_cursor.is_none());
        assert_eq!(read_pool_events(&ctx, &pool, 100).unwrap().len(), 5);
    }

    #[test]
    fn history_coverage_does_not_confuse_a_page_cap_or_empty_events_with_exhaustion() {
        let mut state = HistoryCoverage { since: 100, ..HistoryCoverage::default() };
        advance_backfill(&mut state, Some("page2".into()), Some(200));
        assert!(!state.complete);
        assert_eq!(state.cursor.as_deref(), Some("page2"));
        // This can be a page of non-Swap logs; its cursor must still be followed.
        advance_backfill(&mut state, Some("page3".into()), None);
        assert!(!state.complete);
        advance_backfill(&mut state, None, None);
        assert!(state.complete);
        assert!(state.cursor.is_none());
    }

    #[test]
    fn zero_reserves_clear_a_previously_positive_pool() {
        assert_eq!(decoded_reserves(&[0; 96], 18, 18), Some((0.0, 0.0)));
        assert_eq!(decoded_reserves(&[0; 64], 18, 18), None, "truncated ABI is not zero liquidity");
        let mut pools = vec![Pool { address: "pool".into(), reserve0: 100.0, reserve1: 200.0, ..Pool::default() }];
        assert_eq!(apply_reserves(&mut pools, &[("pool".into(), 0.0, 0.0)], None, None), 1);
        assert_eq!((pools[0].reserve0, pools[0].reserve1), (0.0, 0.0));
        assert!(pools[0].spot_price().is_none());
    }

    #[test]
    fn market_source_freshness_requires_an_actual_complete_observation() {
        let fresh = MarketSource { observed_at: 100, fetched_at: 110, complete: true, ..MarketSource::default() };
        assert!(fresh.fresh_at(111));
        assert!(!fresh.fresh_at(191));
        assert!(!MarketSource { stale: true, ..fresh.clone() }.fresh_at(111));
        assert!(!MarketSource { complete: false, ..fresh.clone() }.fresh_at(111));
        assert!(!MarketSource { observed_at: 0, ..fresh }.fresh_at(111));
    }

    #[tokio::test]
    async fn token_metadata_is_shared_and_cache_only_never_attempts_rpc() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = DataCtx::with_stores(
            crate::appdb::AppDb::memory().unwrap(),
            Some(crate::appdb::AppDb::open_shared(&dir.path().join("feeds.sqlite")).unwrap()),
            crate::network::NetworkProfile::builtins()[1].clone(),
            crate::config::DataPolicy::OFFLINE,
        )
        .unwrap();
        let address = "0x0000000000000000000000000000000000000001";
        let feed = token_meta_key(address);
        ctx.store(&feed).cache_put(&format!("{}:{feed}", ctx.network.id), &serde_json::json!(["USDT", "Tether", 6]).to_string()).unwrap();
        assert!(ctx.app.cache_get(&format!("{}:{feed}", ctx.network.id)).unwrap().is_none());
        let ctx = DataCtx { cache_only: true, ..ctx };
        let values = token_meta_all(&ctx, &[address.into(), "not-a-contract".into()]).await;
        assert_eq!(values[address].decimals, 6);
        assert!(values.contains_key("not-a-contract"));
        assert!(token_meta_required(&ctx, address).await.is_err());
    }

    /// Fresh reserves replace what the directory had, and TVL is recomputed from them.
    ///
    /// The figures are the live WQI/WQUAI pair, read from a monitoring node and from the explorer
    /// within seconds of each other: they agreed to the atom, which is what makes it safe to price
    /// the screen off the node instead of waiting on the explorer's 30-second page.
    #[test]
    fn live_reserves_replace_the_directorys_and_reprice_it() {
        let wquai = "0x006c3e2aaae5db1bcd11a1a097ce572312eaddbb";
        let tok = |a: &str, s: &str| PoolToken { address: a.into(), symbol: s.into(), decimals: 18 };
        let pair = |addr: &str, a: PoolToken, b: PoolToken, tvl: Option<f64>| Pool {
            address: addr.into(),
            token0: a,
            token1: b,
            tvl_usd: tvl,
            ..Pool::default()
        };
        let mut pools = vec![
            pair("0x00602f", tok("0x00wqi", "WQI"), tok(wquai, "WQUAI"), Some(1.0)),
            // No QUAI side: nothing here can be repriced from reserves alone.
            pair("0x007b1b", tok("0x00wqi", "WQI"), tok("0x00usdt", "USDT"), Some(12_953.20)),
            // A curve has no pair to read.
            Pool { address: "0x00curve".into(), venue: Venue::Curve, ..Pool::default() },
        ];
        let fresh = vec![("0x00602f".to_string(), 32_120.851_110, 3_761_291.551_678)];
        // The QUAI price is the feed's, which is what the explorer prices TVL with.
        let hit = apply_reserves(&mut pools, &fresh, Some(wquai), Some(0.009_697));
        assert_eq!(hit, 1, "one pool had fresh reserves");
        assert_eq!((pools[0].reserve0, pools[0].reserve1), (32_120.851_110, 3_761_291.551_678));

        // 2 x the QUAI side x the QUAI price — the explorer published $73,076.79 for this pair at
        // the same moment, so the two bases agree to a fifth of a percent.
        let tvl = pools[0].tvl_usd.expect("a QUAI pair can be priced");
        assert!((tvl / 73_076.79 - 1.0).abs() < 0.005, "{tvl} against the explorer's 73076.79");

        // A pair with no QUAI side keeps the directory's figure: stale by a refresh, but right.
        assert_eq!(pools[1].tvl_usd, Some(12_953.20), "invented freshness is worse than honest age");
        // And a curve is untouched — it has no reserves to read.
        assert_eq!(pools[2].tvl_usd, None);

        // No price, no repricing: the directory's numbers stand rather than being zeroed.
        let mut pools = vec![pair("0x00602f", tok("0x00wqi", "WQI"), tok(wquai, "WQUAI"), Some(1.0))];
        apply_reserves(&mut pools, &fresh, Some(wquai), None);
        assert_eq!(pools[0].tvl_usd, Some(1.0), "a missing QUAI price must not wipe TVL");
        assert_eq!(pools[0].reserve1, 3_761_291.551_678, "reserves still land");
    }

    /// A directory read admits when a ceiling applied, and says nothing when it did not.
    #[test]
    fn a_shortened_directory_says_so() {
        let full = Directory { pools: Vec::new(), total: 25, read: 25, ..Directory::default() };
        assert!(full.omitted("Quainance factory").is_none());
        let clipped = Directory { pools: Vec::new(), total: 812, read: 600, ..Directory::default() };
        let omitted = clipped.omitted("Quainance factory").expect("a partial list must be marked");
        assert_eq!(omitted.text(), "Quainance factory: newest 600 of 812");
        // The sequential ceiling is far lower because each index is its own round trip.
        const { assert!(MAX_FACTORY_PAIRS > MAX_FACTORY_PAIRS_SEQUENTIAL) };
    }

    /// `allPairs` is append-only, so the newest pair is the last index. A ceiling must keep that
    /// end: the markets that vanish first are the ones someone opened the wallet to find.
    #[test]
    fn a_directory_is_read_newest_first() {
        let indices = |n: u64, cap: u64| (0..n).rev().take(cap as usize).collect::<Vec<u64>>();
        assert_eq!(indices(4, 600), vec![3, 2, 1, 0], "a small factory is read whole");
        assert_eq!(indices(812, 600).first(), Some(&811), "the newest pair is first");
        assert_eq!(indices(812, 600).last(), Some(&212), "and the oldest 212 are what is dropped");
        assert_eq!(indices(0, 600), Vec::<u64>::new());
    }

    /// A node caps the addresses one log filter may name, so a bigger directory is several reads
    /// rather than a silently clipped one.
    #[test]
    fn the_flow_filter_splits_instead_of_truncating() {
        let chunks = |n: usize| (0..n).collect::<Vec<_>>().chunks(FLOW_FILTER_ADDRESSES).map(<[usize]>::len).collect::<Vec<_>>();
        assert_eq!(chunks(26), vec![26], "today's directory is one read");
        assert_eq!(chunks(128), vec![128]);
        assert_eq!(chunks(129), vec![128, 1], "and one more pool is a second read, not a lost pool");
        assert_eq!(chunks(600).iter().sum::<usize>(), 600, "every pool is covered");
    }

    /// Token symbols come back as a `string` from most contracts and a `bytes32` from a few, and
    /// both are untrusted display text.
    #[test]
    fn token_text_decodes_both_encodings() {
        let mut dynamic = vec![0u8; 96];
        dynamic[31] = 32; // offset
        dynamic[63] = 4; // length
        dynamic[64..68].copy_from_slice(b"WQAI");
        assert_eq!(solidity_string(&dynamic).as_deref(), Some("WQAI"));
        // bytes32, zero-padded.
        let mut fixed = [0u8; 32];
        fixed[..3].copy_from_slice(b"WQI");
        assert_eq!(solidity_string(&fixed).as_deref(), Some("WQI"));
        // Nothing readable is `None`, so the caller falls back to the short address.
        assert_eq!(solidity_string(&[]), None);
        assert_eq!(solidity_string(&[0u8; 32]), None);
        // A length that runs past the data is not a panic and not a partial read.
        let mut lying = vec![0u8; 96];
        lying[31] = 32;
        lying[63] = 200;
        assert_eq!(solidity_string(&lying), None);
        // Control characters are stripped, the way every other on-chain string is.
        let mut sneaky = vec![0u8; 96];
        sneaky[31] = 32;
        sneaky[63] = 5;
        sneaky[64..69].copy_from_slice(b"A\x1b[2m");
        assert_eq!(solidity_string(&sneaky).as_deref(), Some("A[2m"));
    }

    fn pool() -> Pool {
        Pool {
            address: "0x0021f5cc862ebb0252ba209266f2fabbc7592e83".into(),
            token0: PoolToken { address: "0x0049f7cbca3556c2dfae62aafa7015f99de1b8f5".into(), symbol: "USDT".into(), decimals: 6 },
            token1: PoolToken { address: "0x006c3e2aaae5db1bcd11a1a097ce572312eaddbb".into(), symbol: "WQUAI".into(), decimals: 18 },
            reserve0: 1665.46,
            reserve1: 182_985.5,
            tvl_usd: Some(3330.9),
            volume_24h_usd: Some(312.6),

            ..Default::default()
        }
    }

    fn e18(n: f64) -> U256 {
        U256::from((n * 1e6) as u128) * U256::from(10u128.pow(12))
    }
    fn e6(n: f64) -> U256 {
        U256::from((n * 1e6) as u128)
    }

    #[test]
    fn explorer_logs_decode() {
        let v: Value = serde_json::from_str(include_str!("fixtures/quai_pool_logs.json")).unwrap();
        let (events, cursor) = parse_explorer_logs(&v);
        assert!(cursor.is_some());
        let swaps = events.iter().filter(|e| matches!(e, PoolEvent::Swap { .. })).count();
        let syncs = events.iter().filter(|e| matches!(e, PoolEvent::Sync { .. })).count();
        assert!(swaps > 0 && syncs >= swaps, "{swaps} swaps, {syncs} syncs");
        // The newest log in the fixture is a router swap at 14:17:31.
        let newest = events.iter().max_by_key(|e| e.position()).unwrap();
        assert_eq!(newest.position().0, parse_timestamp("2026-09-15T14:17:31.000Z").unwrap());
        let p = pool();
        let base0 = base_is_token0(&p, Some(&p.token0.address), Some(&p.token1.address), None);
        assert!(!base0, "USDT is the quote, WQUAI the base");
        for t in trades(&events, &p, base0) {
            // WQUAI trades around a cent.
            assert!(t.price > 0.001 && t.price < 0.1, "{t:?}");
        }
    }

    #[test]
    fn candles_volume_and_stats() {
        let p = pool();
        let hour = 3600;
        let now = 100 * hour + 1800;
        let sync = |at: u64, usdt: f64, wquai: f64| PoolEvent::Sync {
            at,
            block: at,
            tx: format!("0x{at:x}"),
            index: 1,
            reserve0: e6(usdt),
            reserve1: e18(wquai),
        };
        let swap_buy_base = |at: u64, usdt_in: f64, wquai_out: f64| PoolEvent::Swap {
            at,
            block: at,
            tx: format!("0x{at:x}"),
            index: 0,
            amount0_in: e6(usdt_in),
            amount1_in: U256::ZERO,
            amount0_out: U256::ZERO,
            amount1_out: e18(wquai_out),
            to: "0x00aa".into(),
        };
        let events = vec![
            sync(90 * hour, 1000.0, 100_000.0), // 0.01 before the window
            swap_buy_base(98 * hour + 10, 10.0, 980.0),
            sync(98 * hour + 10, 1010.0, 99_020.0), // ≈0.0102
            swap_buy_base(100 * hour + 5, 20.0, 1900.0),
            sync(100 * hour + 5, 1030.0, 97_120.0), // ≈0.0106
        ];
        let c = candles(&events, &p, false, hour, now, 4);
        assert_eq!(c.len(), 4);
        assert_eq!(c[0].start, 97 * hour);
        assert!((c[0].open - 0.01).abs() < 1e-9 && c[0].volume == 0.0, "carried close: {:?}", c[0]);
        assert!(c[1].close > 0.0101 && c[1].trades == 1 && (c[1].volume - 10.0).abs() < 1e-6);
        assert_eq!(c[2].trades, 0, "empty bucket carries the close");
        assert!((c[3].volume - 20.0).abs() < 1e-6 && c[3].high >= c[3].low);
        // Aligned to UTC+2, hourly buckets are unchanged and daily buckets start at 22:00 UTC.
        let day = 86_400;
        let local = candles_in_zone(&events, &p, false, day, 7_200, now, 2);
        assert_eq!((local.last().unwrap().start + 7_200) % day, 0, "local midnight");
        assert_eq!(candles_in_zone(&events, &p, false, hour, 7_200, now, 4), c);
        let t = trades(&events, &p, false);
        assert_eq!(t.len(), 2);
        assert!(t[0].buy && t[0].at > t[1].at, "newest first, buying the base");
        assert!((t[0].base - 1900.0).abs() < 1e-6 && (t[0].quote - 20.0).abs() < 1e-6);
        let s = pair_stats(&events, &p, false, now);
        assert_eq!(s.trades_24h, 2);
        assert!(s.change_24h.unwrap() > 5.0, "{s:?}");
        assert!((s.volume_24h - 30.0).abs() < 1e-6);
    }

    #[test]
    fn dex_flow_rows_read_from_the_trader_side() {
        let v: Value = serde_json::from_str(include_str!("fixtures/quai_pool_logs.json")).unwrap();
        let (events, _) = parse_explorer_logs(&v);
        let p = pool();
        let swaps: Vec<DexSwap> = events
            .iter()
            .filter_map(|e| {
                let PoolEvent::Swap { at, block, tx, index, to, .. } = e else { return None };
                let (token_in, amount_in, token_out, amount_out) = swap_sides(&p, e)?;
                Some(DexSwap {
                    at: *at,
                    timed: true,
                    block: *block,
                    tx: tx.clone(),
                    index: *index,
                    pool: p.address.clone(),
                    token_in,
                    token_out,
                    amount_in,
                    amount_out,
                    trader: to.clone(),
                })
            })
            .collect();
        assert!(!swaps.is_empty(), "the fixture has swaps");
        let base = &p.token1.address; // WQUAI is the base, USDT the quote.
        for s in &swaps {
            // Every row moves both ways, and the two sides are the pool's two tokens.
            assert!(s.amount_in > 0.0 && s.amount_out > 0.0, "{s:?}");
            assert_ne!(s.token_in.address, s.token_out.address);
            // The price is the pair's, whichever way the trade went: WQUAI is worth about a cent.
            let price = s.price(base).unwrap();
            assert!(price > 0.001 && price < 0.1, "{price} from {s:?}");
            // Buying the base means the base is what came out.
            let (b, q) = s.sides(base);
            if s.buys(base) {
                assert_eq!((b, q), (s.amount_out, s.amount_in));
            } else {
                assert_eq!((b, q), (s.amount_in, s.amount_out));
            }
        }
        // A trade in each direction has the tokens the other way round.
        let bought = swaps.iter().find(|s| s.buys(base));
        let sold = swaps.iter().find(|s| !s.buys(base));
        if let (Some(b), Some(s)) = (bought, sold) {
            assert_eq!(b.token_in.address, s.token_out.address);
        }
    }

    /// A two-hop route reads in the order it happened, under the newest transaction.
    #[test]
    fn routes_read_first_hop_down() {
        let swap = |tx: &str, block, index| DexSwap {
            at: block,
            timed: true,
            block,
            tx: tx.into(),
            index,
            pool: "0x0021".into(),
            token_in: PoolToken::default(),
            token_out: PoolToken::default(),
            amount_in: 1.0,
            amount_out: 1.0,
            trader: "0x00".into(),
        };
        // As the log order gives them: newest block first, and the last hop before the first.
        let mut tape = vec![swap("0xb", 101, 3), swap("0xa", 100, 7), swap("0xa", 100, 2), swap("0xc", 99, 1)];
        order_routes(&mut tape);
        let seen: Vec<(&str, u64)> = tape.iter().map(|s| (s.tx.as_str(), s.index)).collect();
        assert_eq!(seen, [("0xb", 3), ("0xa", 2), ("0xa", 7), ("0xc", 1)]);
    }

    /// Curve trades as the index returned them for CHEEZ on 2026-09-17: each becomes a swap and
    /// the price after it, and the chart reads the index's own price back out of those.
    #[test]
    fn curve_trades_read_as_a_pool_would_log_them() {
        let curve = Pool {
            address: "0x004ce1cbb33cad511b79d52c6e1118ce4eb60db3".into(),
            token0: PoolToken { address: "0x0016c3221b6a1707427d660945cd284a9be58cec".into(), symbol: "CHEEZ".into(), decimals: 18 },
            token1: PoolToken { address: "0x006c3e2aaae5db1bcd11a1a097ce572312eaddbb".into(), symbol: "WQUAI".into(), decimals: 18 },
            venue: Venue::Curve,
            ..Default::default()
        };
        let body = json!({"data": {"tradeCurveTrades": [
            {"side": "BUY", "account": "0x001C039F068F7EE0A76FD8A089493C6A2BCED7FD", "tokenAmount": "752440808497827709990481",
             "quoteAmount": "55474030000000000000", "priceQuoteE12": "73439879",
             "transactionHash": "0x006d0013b0daf81ed264f3340af2e8e3fe90d372e9c8c37d5becf0d5a942b791",
             "blockNumber": "10148441", "timestamp": "1789666524", "logIndex": "2"},
            {"side": "SELL", "account": "0x002917f6b8b971363cdc79666d6dfb982a00ef86", "tokenAmount": "2408432702690239595933998",
             "quoteAmount": "175367138763406208539", "priceQuoteE12": "72754009",
             "transactionHash": "0x005e0058b7de95a3e47e3982af8ea7c7b407b188b4a1e718dbf90dfa41e6c4c5",
             "blockNumber": "10148308", "timestamp": "1789665800", "logIndex": "11"},
            {"side": "SIDEWAYS", "tokenAmount": "1", "quoteAmount": "1", "transactionHash": "0x00", "blockNumber": "1", "timestamp": "1", "logIndex": "0"}
        ]}});
        let events = parse_curve_trades(&body, &curve);
        assert_eq!(events.len(), 4, "two trades, each a swap and a price; an unknown side is dropped");
        let t = trades(&events, &curve, true);
        assert_eq!(t.len(), 2);
        assert!(t[0].buy && !t[1].buy, "newest first: the buy, then the sell");
        assert!((t[0].base - 752_440.808).abs() < 0.01 && (t[0].quote - 55.474).abs() < 0.001, "{:?}", t[0]);
        assert!((t[1].base - 2_408_432.702).abs() < 0.01 && (t[1].quote - 175.367).abs() < 0.001, "{:?}", t[1]);
        // The price after each trade is the index's, not the trade's average.
        let stats = pair_stats(&events, &curve, true, 1_789_666_600);
        assert!((stats.price.unwrap() - 0.000_073_439_879).abs() < 1e-12, "{stats:?}");
        assert_eq!(stats.trades_24h, 2);
        let canonical = std::collections::HashMap::from([(
            (10_148_441, "0x006d0013b0daf81ed264f3340af2e8e3fe90d372e9c8c37d5becf0d5a942b791".into(), 2),
            1_789_666_525,
        )]);
        let reconciled = reconcile_curve_positions(events.clone(), &canonical);
        assert_eq!(reconciled.len(), 2, "orphan trade and its synthetic price disappear together");
        assert!(reconciled.iter().all(|e| e.position().0 == 1_789_666_525));
        let wrong_log = std::collections::HashMap::from([(
            (10_148_441, "0x006d0013b0daf81ed264f3340af2e8e3fe90d372e9c8c37d5becf0d5a942b791".into(), 3),
            1_789_666_525,
        )]);
        assert!(reconcile_curve_positions(events.clone(), &wrong_log).is_empty());
        let events_after_sell = [&events[2], &events[3]].map(|e| e.clone());
        let after_sell = pair_stats(&events_after_sell, &curve, true, 1_789_665_900);
        assert!((after_sell.price.unwrap() - 0.000_072_754_009).abs() < 1e-12);
        // Swap and price stay ordered and distinct within one log.
        assert!(events.iter().any(|e| matches!(e, PoolEvent::Swap { index: 4, .. })));
        assert!(events.iter().any(|e| matches!(e, PoolEvent::Sync { index: 5, .. })));
    }

    #[test]
    fn launches_on_their_curve_become_markets_and_the_launch_amm_is_priced() {
        use crate::launches::{Launch, Phase};
        let wquai = "0x006c3e2aaae5db1bcd11a1a097ce572312eaddbb";
        let launch = |symbol: &str, phase, curve: Option<&str>| Launch {
            token: format!("0x00{symbol}"),
            symbol: symbol.into(),
            decimals: 18,
            phase,
            curve: curve.map(str::to_string),
            price_quai: Some(0.00007),
            raised_quai: 12_000.0,
            target_quai: Some(25_000.0),
            progress_bps: Some(4_800),
            ..Default::default()
        };
        let launches = [
            launch("CHEEZ", Phase::Bonding, Some("0x00c1")),
            launch("QOGE", Phase::Graduated, None),
            launch("SMOL", Phase::Pooled, None),
            launch("ODD", Phase::Bonding, None),
        ];
        let curves = curve_pools(&launches, wquai);
        assert_eq!(curves.len(), 1, "only launches still on a curve they name: {curves:?}");
        let c = &curves[0];
        assert_eq!(
            (c.venue, c.address.as_str(), c.token0.symbol.as_str(), c.token1.address.as_str()),
            (Venue::Curve, "0x00c1", "CHEEZ", wquai)
        );
        assert_eq!(c.spot_price(), Some(0.00007));
        assert!(!c.venue.routable());
        // USD per WQUAI from the main WQUAI/USDT pool prices a launch AMM pool at twice its WQUAI.
        let main = pool();
        let usd = wquai_usd(std::slice::from_ref(&main), Some(wquai), Some(&main.token0.address)).unwrap();
        assert!((usd - 1665.46 / 182_985.5).abs() < 1e-12);
        let qoge = Pool {
            token0: PoolToken { address: "0x0048".into(), symbol: "QOGE".into(), decimals: 18 },
            token1: PoolToken { address: wquai.into(), symbol: "WQUAI".into(), decimals: 18 },
            reserve0: 153_868_445.9,
            reserve1: 62_501.1,
            venue: Venue::LaunchAmm,
            ..Default::default()
        };
        let tvl = amm_tvl(&qoge, Some(wquai), Some(usd)).unwrap();
        assert!((tvl - 2.0 * 62_501.1 * usd).abs() < 1e-6);
        // A launch AMM pool never prices the network's WQUAI, however it is shaped.
        assert!(
            wquai_usd(&[Pool { venue: Venue::LaunchAmm, ..main }], Some(wquai), Some("0x0049f7cbca3556c2dfae62aafa7015f99de1b8f5"))
                .is_none()
        );
    }

    #[test]
    fn tvl_pools_and_orientation() {
        let v: Value = serde_json::from_str(include_str!("fixtures/quai_stats_tvl.json")).unwrap();
        let (pools, overview) = parse_tvl_pools(&v);
        assert!(!pools.is_empty() && overview.tvl_usd.is_some() && !overview.history.is_empty());
        let wqi = "0x002b2596ecf05c93a31ff916e8b456df6c77c750";
        let p = pools.iter().find(|p| p.token0.address == wqi).unwrap();
        // WQI/QOWBOY: WQI is the quote, so QOWBOY (token1) is the base.
        assert!(!base_is_token0(p, None, None, Some(wqi)));
        assert!(decode_log(&["0xdead".into()], "0x", 0, 0, "0x", 0).is_none());
    }

    /// The older exchange is a venue like the others: named, routable, and its own router.
    #[test]
    fn the_legacy_exchange_is_a_venue_with_its_own_router() {
        assert_eq!(Venue::Legacy.label(), "QuaiSwap");
        assert_eq!(Venue::Legacy.on(), "on QuaiSwap");
        assert!(Venue::Legacy.routable(), "it is a UniswapV2 router like the others");
        assert!(!Venue::Curve.routable(), "a curve still is not");
        let mainnet = crate::network::NetworkProfile::builtins().into_iter().find(|n| n.id == "mainnet").unwrap();
        let (router, factory) = crate::swap::venue_pins(&mainnet, Venue::Legacy).expect("mainnet pins the legacy exchange");
        assert_eq!(router.address.to_lowercase(), "0x006432ea8c46cbf981f6e710d2439c941cebe2d0");
        assert_eq!(factory.address.to_lowercase(), "0x0006112e89ee10615273ed72fe035cc068bc57a9");
        // A shortlist, not the whole factory: the eighteen pairs include two duplicated symbols.
        let pairs = &mainnet.ecosystem.legacy_pairs;
        assert_eq!(pairs.len(), 3, "BOSS/WQUAI, QIQI/WQUAI and BARRY/WQUAI");
        assert!(pairs.iter().all(|p| p.len() == 42 && p.starts_with("0x") && p.chars().all(|c| !c.is_ascii_uppercase())));
        let unique: std::collections::BTreeSet<&String> = pairs.iter().collect();
        assert_eq!(unique.len(), pairs.len(), "no pair is named twice");
    }

    /// Routing reaches every venue that has a router, and only those.
    #[test]
    fn routing_covers_the_three_routable_venues() {
        assert_eq!(crate::routes::VENUES, [Venue::Main, Venue::LaunchAmm, Venue::Legacy, Venue::HartiiAmm]);
        assert!(crate::routes::VENUES.iter().all(|v| v.routable()));
    }
}
