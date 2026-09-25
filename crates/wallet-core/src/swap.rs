//! Token swaps through Quainance's two exchanges (both UniswapV2 Router02): the main one, and the
//! launch AMM that tokens graduate into from their bonding curve.
//!
//! Quotes read the pinned routers and pair reserves on-chain. A router only trades its own
//! factory's pairs, so each swap stays on one exchange; a pair only the two together connect is
//! quoted as two swaps through a hub, each reviewed and signed on its own. Reviews re-quote at
//! preparation time, require an exact prior approval for token inputs (never unlimited), set a
//! minimum output from the user's slippage and a deadline, and are simulated by the SDK before
//! signing.

use crate::journal::OpKind;
use crate::amount::{self, QUAI_DECIMALS};
use crate::appdb::AppDb;
use crate::chain::{addr, interface};
use crate::data::{DataCtx, READ_CALLER, Trust, verify_pinned_all, with_access_list};
use crate::error::{CoreError, Result};
use crate::markets::Venue;
use crate::network::{NetworkProfile, Node, PinnedContract};
use crate::registry::now;
use crate::session::Session;
use crate::tx::{AccountRequest, Review, field};
use quai_sdk::contracts::{Contract, Erc20};
use quai_sdk::{BlockTag, QuaiAddress, U256};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Router ABI subset.
pub const ROUTER_ABI: &[&str] = &[
    "function WETH() view returns (address)",
    "function factory() view returns (address)",
    "function getAmountsOut(uint256 amountIn, address[] path) view returns (uint256[] amounts)",
    "function getAmountsIn(uint256 amountOut, address[] path) view returns (uint256[] amounts)",
    "function swapTokensForExactTokens(uint256 amountOut, uint256 amountInMax, address[] path, address to, uint256 deadline) returns (uint256[] amounts)",
    "function swapETHForExactTokens(uint256 amountOut, address[] path, address to, uint256 deadline) payable returns (uint256[] amounts)",
    "function swapTokensForExactETH(uint256 amountOut, uint256 amountInMax, address[] path, address to, uint256 deadline) returns (uint256[] amounts)",
    "function swapExactTokensForTokensSupportingFeeOnTransferTokens(uint256 amountIn, uint256 amountOutMin, address[] path, address to, uint256 deadline)",
    "function swapExactETHForTokensSupportingFeeOnTransferTokens(uint256 amountOutMin, address[] path, address to, uint256 deadline) payable",
    "function swapExactTokensForETHSupportingFeeOnTransferTokens(uint256 amountIn, uint256 amountOutMin, address[] path, address to, uint256 deadline)",
    "function swapExactTokensForTokens(uint256 amountIn, uint256 amountOutMin, address[] path, address to, uint256 deadline) returns (uint256[] amounts)",
    "function swapExactETHForTokens(uint256 amountOutMin, address[] path, address to, uint256 deadline) payable returns (uint256[] amounts)",
    "function swapExactTokensForETH(uint256 amountIn, uint256 amountOutMin, address[] path, address to, uint256 deadline) returns (uint256[] amounts)",
];

/// Factory ABI subset.
pub const FACTORY_ABI: &[&str] = &["function getPair(address tokenA, address tokenB) view returns (address pair)"];

/// Pair ABI subset.
pub const PAIR_ABI: &[&str] = &[
    "function token0() view returns (address)",
    "function getReserves() view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast)",
];

/// Warn above this price impact (basis points).
pub const IMPACT_WARN_BPS: u64 = 200;
/// Refuse above this price impact (basis points).
pub const IMPACT_REFUSE_BPS: u64 = 5000;
/// UniswapV2 LP fee per hop.
pub const LP_FEE_BPS: u64 = 30;

/// One side of a swap.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SwapAsset {
    /// Native QUAI (routed through WQUAI).
    Quai,
    /// ERC-20.
    Token {
        /// Contract (lowercase).
        address: String,
        /// Symbol (untrusted).
        symbol: String,
        /// Decimals.
        decimals: u8,
    },
}

impl SwapAsset {
    /// Display symbol.
    pub fn symbol(&self) -> &str {
        match self {
            SwapAsset::Quai => "QUAI",
            SwapAsset::Token { symbol, .. } => symbol,
        }
    }
    /// Decimals.
    pub fn decimals(&self) -> u8 {
        match self {
            SwapAsset::Quai => QUAI_DECIMALS,
            SwapAsset::Token { decimals, .. } => *decimals,
        }
    }
    /// Router path address (WQUAI for native).
    pub fn path_address(&self, wquai: &str) -> String {
        match self {
            SwapAsset::Quai => wquai.to_lowercase(),
            SwapAsset::Token { address, .. } => address.to_lowercase(),
        }
    }
    fn is_native(&self) -> bool {
        matches!(self, SwapAsset::Quai)
    }
}

/// A pool on the route.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PoolHop {
    /// Pair address.
    pub pair: String,
    /// Input-side reserve.
    pub reserve_in: String,
    /// Output-side reserve.
    pub reserve_out: String,
    /// Pool TVL in USD from the explorer (reference only; reserves are read on-chain).
    #[serde(default)]
    pub tvl_usd: Option<f64>,
}

/// One transaction of a route: a swap on one exchange.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SwapLeg {
    /// The exchange it trades on.
    pub venue: Venue,
    /// That exchange's router.
    pub router: String,
    /// Router path (lowercase addresses) and the same as symbols.
    pub path: Vec<String>,
    pub route: Vec<String>,
    /// Pools crossed.
    pub pools: Vec<PoolHop>,
    /// Input and expected output, base units.
    pub amount_in: String,
    pub amount_out: String,
    /// Least output accepted: from the slippage, and for a second swap from the first swap's own
    /// minimum.
    pub minimum_out: String,
    /// Decimals of what it pays out: the receiving asset's, or the hub's for a first swap.
    #[serde(default)]
    pub output_decimals: u8,
}

/// A single-router exact-output quote. Maximum input is a caller-supplied authorization cap,
/// never a rounded display value or a promise about a later sequential transaction.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExactOutputQuote {
    pub from: SwapAsset,
    pub to: SwapAsset,
    pub amount_out: String,
    pub required_input: String,
    pub maximum_input: String,
    pub path: Vec<String>,
    pub router: String,
    pub venue: Venue,
    pub allowance: Option<String>,
    pub approval_needed: bool,
    pub balance: Option<String>,
    pub observed_at: u64,
}

/// UniswapV2 exact-output input rounds upward, including the pool fee. Reserves are uint112.
pub fn amount_in_for_output(output: U256, reserve_in: U256, reserve_out: U256) -> Option<U256> {
    if output.is_zero() || reserve_in.is_zero() || output >= reserve_out || reserve_in.bit_len() > 112 || reserve_out.bit_len() > 112 {
        return None;
    }
    reserve_in
        .checked_mul(output)?
        .checked_mul(U256::from(1000))?
        .checked_div(reserve_out.checked_sub(output)?.checked_mul(U256::from(997))?)?
        .checked_add(U256::from(1))
}

fn exact_output_method(from: &SwapAsset, to: &SwapAsset) -> &'static str {
    match (from, to) {
        (SwapAsset::Quai, _) => "swapETHForExactTokens",
        (_, SwapAsset::Quai) => "swapTokensForExactETH",
        _ => "swapTokensForExactTokens",
    }
}

/// Freeze the exact ABI bound and native value for either router mode.
#[allow(clippy::too_many_arguments)]
fn router_call_parameters(
    from: &SwapAsset,
    to: &SwapAsset,
    input: U256,
    output: U256,
    path: &[String],
    recipient: &str,
    deadline: u64,
    exact_output: bool,
) -> (&'static str, Vec<Value>, U256) {
    let method = if exact_output {
        exact_output_method(from, to)
    } else {
        match (from, to) {
            (SwapAsset::Quai, _) => "swapExactETHForTokensSupportingFeeOnTransferTokens",
            (_, SwapAsset::Quai) => "swapExactTokensForETHSupportingFeeOnTransferTokens",
            _ => "swapExactTokensForTokensSupportingFeeOnTransferTokens",
        }
    };
    let mut args = if from.is_native() {
        vec![json!(output.to_string())]
    } else if exact_output {
        vec![json!(output.to_string()), json!(input.to_string())]
    } else {
        vec![json!(input.to_string()), json!(output.to_string())]
    };
    args.extend([json!(path), json!(recipient), json!(deadline.to_string())]);
    (method, args, if from.is_native() { input } else { U256::ZERO })
}

/// Optional caller bounds, preserved across approval and restart.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SwapBounds {
    pub minimum_output: Option<String>,
    pub maximum_impact_bps: Option<u16>,
}
impl SwapBounds {
    pub fn apply(&self, quote: &mut SwapQuote) -> Result<()> {
        if let Some(cap) = self.maximum_impact_bps {
            if cap > 10_000 {
                return Err(CoreError::Invalid("maximum impact exceeds 10000 basis points".into()));
            }
            if quote.impact_bps > u64::from(cap) {
                return Err(CoreError::Rejected(format!("price impact {} bps exceeds your {} bps limit", quote.impact_bps, cap)));
            }
        }
        if let Some(minimum) = &self.minimum_output {
            let minimum = amount::parse_amount(minimum, quote.to.decimals())?;
            require_minimum(minimum)?;
            if minimum > u(&quote.amount_out) {
                return Err(CoreError::Rejected("fresh quote is below your explicit minimum output".into()));
            }
            quote.minimum_out = minimum.max(u(&quote.minimum_out)).to_string();
        }
        Ok(())
    }
}

/// A swap quote.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SwapQuote {
    /// Pay.
    pub from: SwapAsset,
    /// Receive.
    pub to: SwapAsset,
    /// Input base units.
    pub amount_in: String,
    /// Expected output base units (router `getAmountsOut`).
    pub amount_out: String,
    /// Minimum output after slippage.
    pub minimum_out: String,
    /// Slippage (basis points).
    pub slippage_bps: u16,
    /// Router path (lowercase addresses).
    pub path: Vec<String>,
    /// Route as symbols.
    pub route: Vec<String>,
    /// Pools used.
    pub pools: Vec<PoolHop>,
    /// Price impact excluding LP fees (basis points).
    pub impact_bps: u64,
    /// LP fees along the route (basis points).
    pub fee_bps: u64,
    /// Router address.
    pub router: String,
    /// Current allowance of the router for the owner (token inputs).
    pub allowance: Option<String>,
    /// An approval of exactly `amount_in` is required first.
    pub approval_needed: bool,
    /// The owner's balance of the paying asset (base units), when an owner was given.
    #[serde(default)]
    pub balance: Option<String>,
    /// The owner cannot pay `amount_in`; no approval or swap should be prepared.
    #[serde(default)]
    pub insufficient: bool,
    /// Warnings to show.
    pub warnings: Vec<String>,
    /// Quote time (unix seconds).
    pub observed_at: u64,
    /// When the explorer observed pool liquidity (unix seconds), if it was looked up.
    #[serde(default)]
    pub liquidity_at: Option<u64>,
    /// The transactions the route takes, in order: one, or two when it spans both exchanges.
    #[serde(default)]
    pub legs: Vec<SwapLeg>,
}

/// Explicit alternatives; every quote remains a separately reviewed atomic or sequential plan.
#[derive(Clone, Debug, Serialize)]
pub struct QuoteAlternatives {
    pub quotes: Vec<SwapQuote>,
    pub omitted: Vec<String>,
}

/// Retain an ordinary-quote failure as an omission, then independently try alternatives.
/// In particular, an excessive-impact best-gross route must not suppress a safer venue.
async fn collect_alternatives<F, Fut>(standard: Result<SwapQuote>, venues: &[Venue], search: F) -> Result<QuoteAlternatives>
where
    F: Fn(bool, Option<Venue>) -> Fut,
    Fut: std::future::Future<Output = Result<SwapQuote>>,
{
    let mut result = QuoteAlternatives { quotes: vec![], omitted: vec![] };
    let mut standard_venue = None;
    let mut already_cross = false;
    match standard {
        Ok(quote) => {
            standard_venue = (quote.legs.len() == 1).then(|| quote.legs[0].venue);
            already_cross = quote.legs.len() > 1;
            result.quotes.push(quote);
        }
        Err(error) => result.omitted.push(format!("ordinary quote unavailable: {error}")),
    }
    let mut modes: Vec<_> = venues.iter().filter(|v| Some(**v) != standard_venue).map(|v| (false, Some(*v))).collect();
    if !already_cross {
        modes.push((true, None));
    }
    use futures::StreamExt;
    let mut searches = futures::stream::iter(modes.into_iter().map(|(cross, venue)| search(cross, venue))).buffer_unordered(2);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(8);
    loop {
        match tokio::time::timeout_at(deadline, searches.next()).await {
            Ok(Some(Ok(quote))) => result.quotes.push(quote),
            Ok(Some(Err(error))) => result.omitted.push(error.to_string()),
            Ok(None) => break,
            Err(_) => {
                result.omitted.push("alternative searches reached their shared 8 s budget".into());
                break;
            }
        }
    }
    if result.quotes.is_empty() {
        return Err(CoreError::NotFound(format!("no usable route alternative: {}", result.omitted.join("; "))));
    }
    Ok(result)
}

impl SwapQuote {
    /// The hub a two-swap route hands between exchanges: (address, symbol).
    pub fn hub(&self) -> Option<(String, String)> {
        let first = self.legs.first().filter(|_| self.legs.len() > 1)?;
        Some((first.path.last()?.clone(), first.route.last()?.clone()))
    }

    /// `WQI → WQUAI → LAPTOP`, or `QOGE → WQUAI, then WQUAI → USDT` for two swaps.
    pub fn route_text(&self) -> String {
        if self.legs.len() > 1 {
            self.legs.iter().map(|l| l.route.join(" → ")).collect::<Vec<_>>().join(", then ")
        } else {
            self.route.join(" → ")
        }
    }

    /// Pool liquidity line: `$3.26k TVL · $3.02k TVL · read 5m ago`, or None when not looked up.
    pub fn liquidity_text(&self) -> Option<String> {
        let at = self.liquidity_at?;
        let tvl: Vec<String> =
            self.pools.iter().map(|p| p.tvl_usd.map_or_else(|| "not indexed".to_string(), |v| format!("{} TVL", usd_compact(v)))).collect();
        // How long ago the TVL was read — not the pool's age. A reading older than a week is a
        // clock problem, not information.
        let secs = now().saturating_sub(at);
        let age = if at == 0 || secs > 7 * 86_400 {
            String::new()
        } else {
            format!(" · read {} ago", crate::track::human_duration(secs.max(1)))
        };
        Some(format!("{}{age}", tvl.join(" · ")))
    }

    /// Human input amount with symbol.
    pub fn pay_text(&self) -> String {
        format!("{} {}", amount::format_amount(u(&self.amount_in), self.from.decimals()), self.from.symbol())
    }
    /// Human expected output with symbol.
    pub fn receive_text(&self) -> String {
        format!("{} {}", amount::format_amount_short(u(&self.amount_out), self.to.decimals(), 6), self.to.symbol())
    }
    /// Human minimum output with symbol.
    pub fn minimum_text(&self) -> String {
        format!("{} {}", amount::format_amount_short(u(&self.minimum_out), self.to.decimals(), 6), self.to.symbol())
    }
}

fn u(text: &str) -> U256 {
    U256::from_str_radix(text, 10).unwrap_or_default()
}

fn uint(values: &[Value], index: usize) -> Result<U256> {
    values
        .get(index)
        .and_then(Value::as_str)
        .and_then(|t| U256::from_str_radix(t, 10).ok())
        .ok_or_else(|| CoreError::Network("unexpected contract result".into()))
}

/// `amount_out × (10000 − slippage) / 10000`, rounded down.
pub fn minimum_out(amount_out: U256, slippage_bps: u16) -> U256 {
    amount::mul_div(amount_out, U256::from(10_000u64 - u64::from(slippage_bps.min(10_000))), U256::from(10_000u64))
        .expect("a slippage floor never exceeds the input")
}

/// Shared execution bounds: reject invalid values instead of displaying a value different from
/// the protection encoded on chain.
pub fn validate_slippage(slippage_bps: u16) -> Result<()> {
    if slippage_bps > 5_000 {
        return Err(CoreError::Invalid("slippage above 50% is refused".into()));
    }
    Ok(())
}

pub fn validate_deadline(deadline_minutes: u32) -> Result<()> {
    if !(1..=1_440).contains(&deadline_minutes) {
        return Err(CoreError::Invalid("deadline must be between 1 and 1440 minutes".into()));
    }
    Ok(())
}

/// The least disagreement between a quote and its proven pools that refuses a review: reserves
/// are read a block or two apart from the router's quote, and a trade in between moves them.
pub const POOL_CHECK_MIN_BPS: u64 = 50;

/// How a router's quote compares with the output its proven pools give.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuoteVerdict {
    /// Within the tolerance: the slippage the user accepted, and at least [`POOL_CHECK_MIN_BPS`].
    Agrees,
    /// Below it: a minimum taken from this quote gives the difference away.
    Understated,
    /// Above it: the swap may revert at its minimum.
    Overstated,
}

pub fn quote_verdict(quoted: U256, proven: U256, slippage_bps: u16) -> QuoteVerdict {
    let tolerance = U256::from(u64::from(slippage_bps).max(POOL_CHECK_MIN_BPS));
    let scale = U256::from(10_000u64);
    if quoted < proven.saturating_mul(scale.saturating_sub(tolerance)) / scale {
        QuoteVerdict::Understated
    } else if quoted > proven.saturating_mul(scale.saturating_add(tolerance)) / scale {
        QuoteVerdict::Overstated
    } else {
        QuoteVerdict::Agrees
    }
}

pub(crate) fn require_minimum(value: U256) -> Result<()> {
    if value.is_zero() {
        return Err(CoreError::Invalid("the protected output rounds to zero; increase the amount or reduce slippage".into()));
    }
    Ok(())
}

/// UniswapV2 `getAmountOut` with the 0.3% fee.
pub fn amount_out(amount_in: U256, reserve_in: U256, reserve_out: U256) -> U256 {
    if amount_in.is_zero() || reserve_in.is_zero() || reserve_out.is_zero() {
        return U256::ZERO;
    }
    let Some(with_fee) = amount_in.checked_mul(U256::from(997u64)) else { return U256::ZERO };
    let Some(denominator) = reserve_in.checked_mul(U256::from(1000u64)).and_then(|v| v.checked_add(with_fee)) else {
        return U256::ZERO;
    };
    // Match the router's checked arithmetic: an impossible estimate is not a candidate.
    with_fee.checked_mul(reserve_out).map_or(U256::ZERO, |numerator| numerator / denominator)
}

/// The least a later swap can pay: what its pools give for the earlier swap's own minimum, less
/// slippage. The earlier swap may pay anything down to its minimum, and the later one is sized
/// from what actually arrived.
pub fn chained_minimum(earlier_minimum: U256, hops: &[(U256, U256)], slippage_bps: u16) -> U256 {
    minimum_out(hops.iter().fold(earlier_minimum, |x, (rin, rout)| amount_out(x, *rin, *rout)), slippage_bps)
}

/// Price impact excluding LP fees, in basis points, for a route of (reserve_in, reserve_out) hops.
pub fn impact_bps(amount_in: U256, hops: &[(U256, U256)]) -> u64 {
    let mut actual = amount_in;
    let mut mid = amount::to_f64(amount_in, 0);
    for (rin, rout) in hops {
        actual = amount_out(actual, *rin, *rout);
        let (fin, fout) = (amount::to_f64(*rin, 0), amount::to_f64(*rout, 0));
        if fin <= 0.0 {
            return 10_000;
        }
        mid = mid * fout / fin * 0.997;
    }
    if mid <= 0.0 {
        return 10_000;
    }
    let ratio = amount::to_f64(actual, 0) / mid;
    ((1.0 - ratio).max(0.0) * 10_000.0).round() as u64
}

/// A candidate route: output, (address, symbol) path, pools and (reserve in, reserve out) hops.
type Route = (U256, Vec<(String, String)>, Vec<PoolHop>, Vec<(U256, U256)>);

/// One resolved hop: its pair and the reserves oriented (in, out). None when no pool exists.
type Leg = Option<(QuaiAddress, U256, U256)>;

/// Hops resolved once per quote and shared by every candidate that crosses them, per exchange.
type Legs = std::collections::HashMap<(Venue, String, String), Leg>;

/// How long a pair's absence is believed. A missing pair may be created at any time.
const NO_PAIR_TTL: u64 = 300;

/// How long a *found* pair address is believed. A pair, once created, never moves, so this is
/// long — but it is a policy rather than "forever". The address is not a call destination (the
/// router derives the pair itself on-chain from the token path), so a wrong one cannot redirect
/// funds; what it would corrupt is the reserve read behind a quote, and therefore the route, the
/// output shown and the `amountOutMin` that is the user's slippage protection. A quote feeding a
/// review reads it first-hand instead — see `docs/REVIEW_TRUST.md` §3.
const PAIR_TTL: u64 = 7 * 86_400;

/// One exchange's router and factory, verified against their pins.
struct VenueRouter {
    venue: Venue,
    router: QuaiAddress,
    factory: QuaiAddress,
}

/// The pins of an exchange's router and factory on a network.
pub fn venue_pins(network: &NetworkProfile, venue: Venue) -> Option<(&PinnedContract, &PinnedContract)> {
    let eco = &network.ecosystem;
    match venue {
        Venue::Main => Some((eco.quainance_router.as_ref()?, eco.quainance_factory.as_ref()?)),
        Venue::LaunchAmm => Some((eco.launch_amm_router.as_ref()?, eco.launch_amm_factory.as_ref()?)),
        Venue::Legacy => Some((eco.legacy_router.as_ref()?, eco.legacy_factory.as_ref()?)),
        Venue::HartiiAmm => Some((eco.hartii_amm_router.as_ref()?, eco.hartii_amm_factory.as_ref()?)),
        Venue::Curve => None,
    }
}

/// Where a UniswapV2 factory keeps `getPair[a][b]`: slot 2 on Quainance and the legacy exchange,
/// slot 4 on the launch AMM and Hartii's. Read against `getPair` at the same block on mainnet
/// (2026-09-23); the live test `a_route_s_output_is_proven_from_its_pools` reads it again.
pub fn get_pair_slot(venue: Venue) -> Option<u64> {
    match venue {
        Venue::Main | Venue::Legacy => Some(2),
        Venue::LaunchAmm | Venue::HartiiAmm => Some(4),
        Venue::Curve => None,
    }
}
/// A UniswapV2 pair's `token0`, `token1`, and `reserve0 | reserve1 | blockTimestampLast` packed
/// 112/112/32; the same on every venue here.
pub const PAIR_TOKEN0_SLOT: u64 = 6;
pub const PAIR_TOKEN1_SLOT: u64 = 7;
pub const PAIR_RESERVES_SLOT: u64 = 8;

/// A route's output computed from pools proven at one block, and how far it is vouched for.
#[derive(Clone, Debug, PartialEq)]
pub struct ProvenRoute {
    /// What the proven reserves give for the input, along the path.
    pub amount_out: U256,
    /// Whether the block was confirmed by a second node.
    pub confirmation: crate::anchor::Confirmation,
    /// The block the pools were proven at.
    pub block: u64,
}

/// `(reserve0, reserve1)` from a pair's packed slot 8.
pub fn unpack_reserves(word: U256) -> (U256, U256) {
    let mask = (U256::from(1u64) << 112) - U256::from(1u64);
    (word & mask, (word >> 112) & mask)
}

/// The slot of `getPair[a][b]` in a factory whose mapping sits at `base`.
fn pair_slot(a: QuaiAddress, b: QuaiAddress, base: u64) -> quai_sdk::primitives::Hash32 {
    use quai_sdk::provider::state_proof::{address_word, solidity_mapping_slot};
    let inner = solidity_mapping_slot(address_word(a), U256::from(base));
    solidity_mapping_slot(address_word(b), U256::from_be_bytes(inner.into_bytes()))
}

/// Compute a route's output from its pools as proven at the node's anchor: the factory's own
/// record of each pair, and each pair's tokens and reserves.
///
/// `Ok(None)` when it cannot be proven here (a venue without a factory pin, a path the pools
/// do not match, or a header this SDK cannot hash); the review then rests on the router's quote,
/// as it did before proofs.
pub async fn prove_route(
    node: &Node,
    network: &NetworkProfile,
    venue: Venue,
    path: &[String],
    pairs: &[String],
    amount_in: U256,
) -> Result<Option<ProvenRoute>> {
    let (Some((_, factory)), Some(base)) = (venue_pins(network, venue), get_pair_slot(venue)) else { return Ok(None) };
    if factory.code_hash.is_none() || pairs.is_empty() || path.len() != pairs.len() + 1 {
        return Ok(None);
    }
    let factory = addr(&factory.address)?;
    let tokens: Vec<QuaiAddress> = path.iter().map(|t| addr(t)).collect::<Result<_>>()?;
    let pair_addresses: Vec<QuaiAddress> = pairs.iter().map(|p| addr(p)).collect::<Result<_>>()?;
    let hop_slots: Vec<quai_sdk::primitives::Hash32> = tokens.windows(2).map(|w| pair_slot(w[0], w[1], base)).collect();
    let pair_fields = [PAIR_TOKEN0_SLOT, PAIR_TOKEN1_SLOT, PAIR_RESERVES_SLOT]
        .map(|slot| quai_sdk::primitives::Hash32::from_bytes(U256::from(slot).to_be_bytes::<32>()));
    let mut targets: Vec<(QuaiAddress, &[quai_sdk::primitives::Hash32])> = vec![(factory, hop_slots.as_slice())];
    targets.extend(pair_addresses.iter().map(|p| (*p, &pair_fields[..])));
    let Some((proven, confirmation)) = crate::anchor::prove_state(node, network, &targets, "the swap's pools").await? else {
        return Ok(None);
    };
    let mut amount = amount_in;
    for (hop, (pair, slot)) in pair_addresses.iter().zip(&hop_slots).enumerate() {
        let named = proven[0].storage_value(*slot).map(crate::anchor::word_address).unwrap_or_default();
        if !named.eq_ignore_ascii_case(&pair.to_string()) {
            return Err(CoreError::Rejected(format!(
                "the {} factory's own records do not list {} for this pair; refusing to quote through it",
                venue.label(),
                pair
            )));
        }
        let state = &proven[hop + 1];
        let value = |i: usize| state.storage_value(pair_fields[i]).unwrap_or_default();
        let (token0, (reserve0, reserve1)) = (crate::anchor::word_address(value(0)), unpack_reserves(value(2)));
        let (reserve_in, reserve_out) =
            if token0.eq_ignore_ascii_case(&tokens[hop].to_string()) { (reserve0, reserve1) } else { (reserve1, reserve0) };
        amount = amount_out(amount, reserve_in, reserve_out);
    }
    Ok(Some(ProvenRoute { amount_out: amount, confirmation, block: proven[0].block.number }))
}

/// The reserves along a path, oriented input to output, as the venue's factory and pairs prove
/// them at one block: the factory's own `getPair` for each hop, then each pair's tokens and
/// reserves. `Ok(None)` when this cannot be proven here (no factory pin, a header this SDK
/// cannot hash).
pub async fn prove_path_reserves(
    node: &Node,
    network: &NetworkProfile,
    venue: Venue,
    path: &[String],
) -> Result<Option<(Vec<(U256, U256)>, crate::anchor::Anchored)>> {
    let (Some((_, factory)), Some(base)) = (venue_pins(network, venue), get_pair_slot(venue)) else { return Ok(None) };
    if factory.code_hash.is_none() || path.len() < 2 {
        return Ok(None);
    }
    let what = "the swap's pools";
    let Some(anchored) = crate::anchor::review_anchor(node, network, what).await? else { return Ok(None) };
    let factory = addr(&factory.address)?;
    let tokens: Vec<QuaiAddress> = path.iter().map(|t| addr(t)).collect::<Result<_>>()?;
    let hop_slots: Vec<quai_sdk::primitives::Hash32> = tokens.windows(2).map(|w| pair_slot(w[0], w[1], base)).collect();
    let listed = crate::anchor::prove_at(node, network, &anchored, &[(factory, hop_slots.as_slice())], what).await?;
    let pairs: Vec<QuaiAddress> = hop_slots
        .iter()
        .map(|slot| {
            let pair = listed[0].storage_value(*slot).map(crate::anchor::word_address).unwrap_or_default();
            if crate::chain::is_zero_address(&pair) {
                return Err(CoreError::Rejected(format!("the {} factory lists no pool for a hop of this route", venue.label())));
            }
            addr(&pair)
        })
        .collect::<Result<_>>()?;
    let fields = [crate::anchor::slot(PAIR_TOKEN0_SLOT), crate::anchor::slot(PAIR_RESERVES_SLOT)];
    let targets: Vec<(QuaiAddress, &[quai_sdk::primitives::Hash32])> = pairs.iter().map(|p| (*p, &fields[..])).collect();
    let states = crate::anchor::prove_at(node, network, &anchored, &targets, what).await?;
    let reserves = states
        .iter()
        .zip(&tokens)
        .map(|(state, token_in)| {
            let token0 = crate::anchor::word_address(state.storage_value(fields[0]).unwrap_or_default());
            let (r0, r1) = unpack_reserves(state.storage_value(fields[1]).unwrap_or_default());
            if token0.eq_ignore_ascii_case(&token_in.to_string()) { (r0, r1) } else { (r1, r0) }
        })
        .collect();
    Ok(Some((reserves, anchored)))
}

/// What an exact output costs through `reserves` (oriented input to output): the router's
/// `getAmountsIn`, hop by hop from the end.
pub fn input_for_output(output: U256, reserves: &[(U256, U256)]) -> Option<U256> {
    reserves.iter().rev().try_fold(output, |out, (reserve_in, reserve_out)| amount_in_for_output(out, *reserve_in, *reserve_out))
}

/// Verify only the exchange selected for an LP action. An unrelated unavailable exchange must
/// not prevent withdrawing from a healthy, authenticated pool.
pub(crate) async fn verified_pool_router(ctx: &DataCtx, venue: Venue) -> Result<QuaiAddress> {
    let (router, factory) =
        venue_pins(&ctx.network, venue).ok_or_else(|| CoreError::NotFound(format!("{} liquidity is not configured", venue.label())))?;
    let wquai = ctx.network.wquai.as_deref().ok_or_else(|| CoreError::Network("WQUAI is not configured".into()))?.to_lowercase();
    Ok(verify_venue(&ctx.app, &ctx.node, &ctx.network, venue, router, factory, &wquai, ctx.trust).await?.router)
}

/// Read-only router access bound to a network: every exchange it can verify.
pub struct Router<'a> {
    app: &'a AppDb,
    node: &'a Node,
    network: NetworkProfile,
    venues: Vec<VenueRouter>,
    wquai: String,
    hubs: Vec<(String, String)>,
    trust: Trust,
    verification_warnings: Vec<String>,
    /// Verified once here rather than per venue: on a review path opening it is itself a code
    /// read, and a quote opens it twice.
    multicall: Option<crate::multicall::Multicall<'a>>,
}

/// The factory's answer as an address, or `None` for the zero address it returns when the pair
/// does not exist.
fn found_pair(text: &str) -> Option<String> {
    let pair = text.trim().to_lowercase();
    (!(pair.is_empty() || pair.trim_start_matches("0x").chars().all(|c| c == '0'))).then_some(pair)
}

/// What a remembered `getPair` answer is still worth: `Some(Some(address))` a pair that was found,
/// `Some(None)` an absence still believed, `None` ask the factory again.
///
/// A review's quote ([`Trust::FirstHand`]) is always `None`: the pair behind a reserve read is what
/// sets the route, the output shown and the minimum output, so it is read now.
fn remembered_pair(trust: Trust, known: &str, age: u64) -> Option<Option<&str>> {
    if !trust.may_cache() {
        return None;
    }
    match (known.is_empty(), age) {
        (false, age) if age < PAIR_TTL => Some(Some(known)),
        (true, age) if age < NO_PAIR_TTL => Some(None),
        _ => None,
    }
}

impl<'a> Router<'a> {
    /// Verify the pinned routers and factories and bind them. The main exchange is required. The
    /// launch AMM is added when it verifies; one that does not is left out rather than failing
    /// every swap, and its tokens then read as unroutable.
    ///
    /// `trust` says what this router is for. A router address is the spender of every swap
    /// approval and the destination of every swap, so a quote that will be shown in a review
    /// passes [`Trust::FirstHand`] and the pins are read from the chain rather than remembered.
    pub async fn open(app: &'a AppDb, node: &'a Node, network: &NetworkProfile, trust: Trust) -> Result<Router<'a>> {
        let wquai = network.wquai.clone().ok_or_else(|| CoreError::Network("WQUAI is not configured".into()))?.to_lowercase();
        let verify = |venue| {
            let wquai = &wquai;
            async move {
                let Some((router, factory)) = venue_pins(network, venue) else { return Ok(None) };
                verify_venue(app, node, network, venue, router, factory, wquai, trust).await.map(Some)
            }
        };
        let (main, launch, legacy, hartii, multicall) = futures::join!(
            verify(Venue::Main),
            verify(Venue::LaunchAmm),
            verify(Venue::Legacy),
            verify(Venue::HartiiAmm),
            crate::multicall::Multicall::on(app, node, network, trust),
        );
        let mut venues = Vec::new();
        let mut verification_warnings = Vec::new();
        for (venue, result) in [(Venue::Main, main), (Venue::LaunchAmm, launch), (Venue::Legacy, legacy), (Venue::HartiiAmm, hartii)] {
            match result {
                Ok(Some(router)) => venues.push(router),
                Ok(None) => {}
                Err(error) => verification_warnings.push(format!("{} unavailable: {error}", venue.label())),
            }
        }
        if venues.is_empty() {
            return Err(CoreError::Network(format!("no configured swap venue verified: {}", verification_warnings.join("; "))));
        }
        let mut hubs = vec![(wquai.clone(), "WQUAI".to_string())];
        if let Some(wqi) = &network.wqi {
            hubs.push((wqi.to_lowercase(), "WQI".into()));
        }
        if let Some(usdt) = &network.ecosystem.usdt {
            hubs.push((usdt.address.to_lowercase(), "USDT".into()));
        }
        Ok(Router { app, node, network: network.clone(), venues, wquai, hubs, trust, verification_warnings, multicall })
    }

    /// The first available verified router (prefer the main exchange).
    pub fn address(&self) -> QuaiAddress {
        self.venues[0].router
    }

    /// An exchange's router, when it verified.
    pub fn router_for(&self, venue: Venue) -> Option<QuaiAddress> {
        self.venues.iter().find(|v| v.venue == venue).map(|v| v.router)
    }

    /// The router that can move this pool's pair, or why it cannot be reached.
    ///
    /// A router only serves its own factory's pairs, so approving or depositing through the wrong
    /// one does not fail politely — it addresses a pair that router has never heard of. Liquidity
    /// paths take the venue from the pool rather than defaulting to the main exchange.
    pub fn router_for_pool(&self, venue: Venue) -> Result<QuaiAddress> {
        self.router_for(venue).ok_or_else(|| match venue {
            Venue::Curve => CoreError::Invalid("a bonding curve holds no liquidity to add or remove".into()),
            other => CoreError::Network(format!("the {} router is not available on {}", other.label(), self.network.name)),
        })
    }

    /// Wrapped QUAI address.
    pub fn wquai(&self) -> &str {
        &self.wquai
    }

    /// Cache key for one factory's answer about a token couple, order-independent.
    fn pair_key(&self, factory: QuaiAddress, a: &str, b: &str) -> String {
        let (x, y) = if a <= b { (a, b) } else { (b, a) };
        format!("{}:pair:{}:{x}:{y}", self.network.id, factory.to_string().to_lowercase())
    }

    /// What this router already knows about a couple, or `None` when it must ask the factory.
    fn remembered(&self, factory: QuaiAddress, a: &str, b: &str) -> Option<Option<QuaiAddress>> {
        let (known, at) = self.app.cache_get(&self.pair_key(factory, a, b)).ok().flatten()?;
        let answer = remembered_pair(self.trust, &known, now().saturating_sub(at))?;
        match answer {
            Some(text) => addr(text).ok().map(Some),
            None => Some(None),
        }
    }

    /// Record what the factory said. A review writes nothing: what it read is for this review.
    fn record_pair(&self, factory: QuaiAddress, a: &str, b: &str, pair: Option<&str>) {
        if self.trust.may_cache() {
            let _ = self.app.cache_put(&self.pair_key(factory, a, b), pair.unwrap_or_default());
        }
    }

    async fn pair(&self, factory: QuaiAddress, a: &str, b: &str) -> Result<Option<QuaiAddress>> {
        if let Some(answer) = self.remembered(factory, a, b) {
            return Ok(answer);
        }
        let f = Contract::new(factory, interface(FACTORY_ABI)?, &self.node.provider);
        let out = f.call(addr(READ_CALLER)?, "getPair", &[json!(a), json!(b)], BlockTag::Latest).await?;
        let pair = found_pair(out.first().and_then(Value::as_str).unwrap_or_default());
        self.record_pair(factory, a, b, pair.as_deref());
        pair.map(|p| addr(&p)).transpose()
    }

    async fn reserves(&self, pair: QuaiAddress, token_in: &str) -> Result<(U256, U256)> {
        let p = Contract::new(pair, interface(PAIR_ABI)?, &self.node.provider);
        let caller = addr(READ_CALLER)?;
        let token0 =
            p.call(caller, "token0", &[], BlockTag::Latest).await?.first().and_then(Value::as_str).unwrap_or_default().to_lowercase();
        let r = p.call(caller, "getReserves", &[], BlockTag::Latest).await?;
        let (r0, r1) = (uint(&r, 0)?, uint(&r, 1)?);
        Ok(if token0 == token_in.to_lowercase() { (r0, r1) } else { (r1, r0) })
    }

    async fn amounts_out(&self, router: QuaiAddress, amount_in: U256, path: &[String]) -> Result<Vec<U256>> {
        let r = Contract::new(router, interface(ROUTER_ABI)?, &self.node.provider);
        let path_json: Vec<Value> = path.iter().map(|p| json!(p)).collect();
        let out =
            r.call(addr(READ_CALLER)?, "getAmountsOut", &[json!(amount_in.to_string()), Value::Array(path_json)], BlockTag::Latest).await?;
        let amounts = out.first().and_then(Value::as_array).ok_or_else(|| CoreError::Network("router returned no amounts".into()))?;
        amounts
            .iter()
            .map(|v| {
                v.as_str().and_then(|t| U256::from_str_radix(t, 10).ok()).ok_or_else(|| CoreError::Network("bad router amount".into()))
            })
            .collect()
    }

    /// Resolve every hop a quote needs — the pair behind each token couple and that pair's two
    /// reserves — filling `legs`.
    ///
    /// A quote considers the direct path, one hub and two hubs, which is about a dozen distinct
    /// couples. One at a time that is three round trips each (`getPair`, `token0`, `getReserves`),
    /// so a cold quote against a public RPC spent well over a second just asking. Multicall3
    /// answers all the `getPair`s in one round and all the reserves in a second. Without a
    /// Multicall3 on the network it falls back to the sequential reads, which are correct and
    /// merely chattier.
    async fn resolve_legs(&self, v: &VenueRouter, wanted: &[(String, String)], legs: &mut Legs) -> Result<()> {
        let unresolved: Vec<&(String, String)> =
            wanted.iter().filter(|(a, b)| !legs.contains_key(&(v.venue, a.clone(), b.clone()))).collect();
        if unresolved.is_empty() {
            return Ok(());
        }
        let Some(mc) = self.multicall.as_ref() else {
            for (a, b) in unresolved {
                let leg = self.leg_sequentially(v, a, b).await?;
                legs.insert((v.venue, a.clone(), b.clone()), leg);
            }
            return Ok(());
        };
        use crate::multicall::{Arg, Call, address_word, word};
        // Round 1: the pairs this router has not been told about yet.
        let ask: Vec<(String, String)> =
            unresolved.iter().filter(|(a, b)| self.remembered(v.factory, a, b).is_none()).map(|c| (*c).clone()).collect();
        let factory = v.factory.to_string().to_lowercase();
        let calls: Vec<Call> = ask
            .iter()
            .map(|(a, b)| Call::view(&factory, "getPair(address,address)", &[Arg::Addr(a.clone()), Arg::Addr(b.clone())]))
            .collect();
        let answers = mc.try_all(&calls).await?;
        let mut pairs: std::collections::HashMap<(String, String), Option<QuaiAddress>> = std::collections::HashMap::new();
        for ((a, b), data) in ask.into_iter().zip(answers) {
            // A row the batch could not answer is asked directly rather than read as "no pair":
            // a missing market and an unanswered question are not the same thing.
            let found = match data {
                Some(data) => found_pair(&address_word(&data, 0)),
                None => {
                    let direct = self.pair(v.factory, &a, &b).await?;
                    pairs.insert((a.clone(), b.clone()), direct);
                    continue;
                }
            };
            self.record_pair(v.factory, &a, &b, found.as_deref());
            pairs.insert((a.clone(), b.clone()), found.map(|p| addr(&p)).transpose()?);
        }
        // Round 2: every found pair's token0 and reserves, in one batch.
        let mut found: Vec<(&(String, String), QuaiAddress)> = Vec::new();
        for couple in &unresolved {
            let known = match pairs.get(*couple) {
                Some(answer) => *answer,
                None => self.remembered(v.factory, &couple.0, &couple.1).flatten(),
            };
            match known {
                Some(address) => found.push((couple, address)),
                None => {
                    legs.insert((v.venue, couple.0.clone(), couple.1.clone()), None);
                }
            }
        }
        let mut calls = Vec::with_capacity(found.len() * 2);
        for (_, address) in &found {
            let pair = address.to_string().to_lowercase();
            calls.push(Call::view(&pair, "token0()", &[]));
            calls.push(Call::view(&pair, "getReserves()", &[]));
        }
        let answers = mc.try_all(&calls).await?;
        for (i, ((a, b), address)) in found.into_iter().enumerate() {
            let token0 = answers.get(i * 2).and_then(Option::as_ref).map(|d| address_word(d, 0));
            let reserves = answers.get(i * 2 + 1).and_then(Option::as_ref);
            let leg = match (token0, reserves) {
                (Some(token0), Some(data)) => {
                    let (r0, r1) = (word(data, 0), word(data, 1));
                    let (rin, rout) = if token0.eq_ignore_ascii_case(a) { (r0, r1) } else { (r1, r0) };
                    (!rin.is_zero() && !rout.is_zero()).then_some((address, rin, rout))
                }
                // Same rule as above: an unanswered pair is read directly, not assumed empty.
                _ => {
                    let (rin, rout) = self.reserves(address, a).await?;
                    (!rin.is_zero() && !rout.is_zero()).then_some((address, rin, rout))
                }
            };
            legs.insert((v.venue, a.clone(), b.clone()), leg);
        }
        Ok(())
    }

    /// One hop the slow way: the factory, then the pair's token order and reserves.
    async fn leg_sequentially(&self, v: &VenueRouter, a: &str, b: &str) -> Result<Option<(QuaiAddress, U256, U256)>> {
        Ok(match self.pair(v.factory, a, b).await? {
            Some(address) => {
                let (rin, rout) = self.reserves(address, a).await?;
                (!rin.is_zero() && !rout.is_zero()).then_some((address, rin, rout))
            }
            None => None,
        })
    }

    /// Exact output is qualified only for network-pinned, conventional wrapped/native and USDT
    /// tokens. Unknown transfer-tax/rebasing semantics cannot promise a recipient amount.
    async fn qualify_conventional_asset(&self, asset: &SwapAsset) -> Result<()> {
        let address = asset.path_address(&self.wquai);
        let eco = &self.network.ecosystem;
        let pin = if address.eq_ignore_ascii_case(&self.wquai) {
            Some(PinnedContract { address, code_hash: eco.wquai_code_hash.clone() })
        } else if self.network.wqi.as_ref().is_some_and(|a| a.eq_ignore_ascii_case(&address)) {
            Some(PinnedContract { address, code_hash: eco.wqi_code_hash.clone() })
        } else {
            eco.usdt.clone().filter(|pin| pin.address.eq_ignore_ascii_case(&address))
        };
        let pin = pin.filter(|pin| pin.code_hash.is_some()).ok_or_else(|| {
            CoreError::Rejected(
                "this router mode supports only pinned conventional wrappers and USDT; unknown/taxed/rebasing tokens are unsupported"
                    .into(),
            )
        })?;
        crate::data::verify_pinned(self.app, self.node, &self.network, &pin, "conventional router token", self.trust).await?;
        Ok(())
    }

    async fn qualify_exact_output_router(&self, venue: &VenueRouter, method: &str) -> Result<()> {
        if !venue_pins(&self.network, venue.venue).is_some_and(|(r, f)| r.code_hash.is_some() && f.code_hash.is_some()) {
            return Err(CoreError::Rejected("exact-output requires a pinned router and factory".into()));
        }
        let runtime = self.node.raw("quai_getCode", json!([venue.router.to_string(), "latest"])).await?;
        let runtime = runtime.as_str().ok_or_else(|| CoreError::Network("router code response".into()))?;
        let runtime = hex::decode(runtime.trim_start_matches("0x")).map_err(|_| CoreError::Network("router code encoding".into()))?;
        let declared = crate::contracts::selectors(&runtime);
        let signature = match method {
            "swapETHForExactTokens" => "swapETHForExactTokens(uint256,address[],address,uint256)",
            "swapTokensForExactETH" => "swapTokensForExactETH(uint256,uint256,address[],address,uint256)",
            _ => "swapTokensForExactTokens(uint256,uint256,address[],address,uint256)",
        };
        for signature in ["getAmountsIn(uint256,address[])", signature] {
            let selector = quai_sdk::abi::function_selector(signature).map_err(|e| CoreError::Invalid(e.to_string()))?;
            if !declared.contains(&selector) {
                return Err(CoreError::Rejected(format!("pinned router does not expose required exact-output selector {signature}")));
            }
        }
        Ok(())
    }

    pub async fn quote_exact_output(
        &self,
        from: &SwapAsset,
        to: &SwapAsset,
        amount_out: U256,
        maximum_input: U256,
        owner: Option<&str>,
    ) -> Result<ExactOutputQuote> {
        require_minimum(amount_out)?;
        require_minimum(maximum_input)?;
        self.qualify_conventional_asset(from).await?;
        self.qualify_conventional_asset(to).await?;
        let (a, b) = (from.path_address(&self.wquai), to.path_address(&self.wquai));
        if a == b {
            return Err(CoreError::Invalid("same-asset output is a wrap, not a swap".into()));
        }
        let hubs: Vec<_> = self.hubs.iter().map(|h| h.0.clone()).collect();
        let paths = crate::routes::candidate_paths(&a, &b, &hubs);
        let mut legs = Legs::new();
        let mut best: Option<(U256, Vec<String>, &VenueRouter)> = None;
        let mut failures = Vec::new();
        for venue in &self.venues {
            if let Err(error) = self.qualify_exact_output_router(venue, exact_output_method(from, to)).await {
                failures.push(error.to_string());
                continue;
            }
            let wanted: Vec<_> = paths.iter().flat_map(|path| path.windows(2).map(|w| (w[0].clone(), w[1].clone()))).collect();
            if let Err(error) = self.resolve_legs(venue, &wanted, &mut legs).await {
                failures.push(error.to_string());
                continue;
            }
            let mut ranked = Vec::new();
            for path in &paths {
                let mut needed = Some(amount_out);
                for hop in path.windows(2).rev() {
                    needed = needed.and_then(|out| {
                        legs.get(&(venue.venue, hop[0].clone(), hop[1].clone()))
                            .copied()
                            .flatten()
                            .and_then(|(_, rin, rout)| amount_in_for_output(out, rin, rout))
                    });
                }
                if let Some(needed) = needed {
                    ranked.push((needed, path));
                }
            }
            ranked.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.len().cmp(&b.1.len())));
            for (_, path) in ranked.into_iter().take(2) {
                let contract = Contract::new(venue.router, interface(ROUTER_ABI)?, &self.node.provider);
                let response = match contract
                    .call(addr(READ_CALLER)?, "getAmountsIn", &[json!(amount_out.to_string()), json!(path)], BlockTag::Latest)
                    .await
                {
                    Ok(value) => value,
                    Err(error) => {
                        failures.push(error.to_string());
                        continue;
                    }
                };
                let Some(amounts) = response.first().and_then(Value::as_array) else { continue };
                if amounts.len() != path.len() {
                    continue;
                }
                let (Ok(required), Ok(quoted_output)) = (uint(amounts, 0), uint(amounts, amounts.len() - 1)) else { continue };
                if required.is_zero() || quoted_output != amount_out || required > maximum_input {
                    continue;
                }
                if best.as_ref().is_none_or(|(prior, ..)| required < *prior) {
                    best = Some((required, path.clone(), venue));
                }
            }
        }
        let (required, path, venue) = best.ok_or_else(|| {
            CoreError::Rejected(format!("no qualified single-router exact-output route within maximum input; {}", failures.join("; ")))
        })?;
        for intermediate in path.iter().skip(1).take(path.len().saturating_sub(2)) {
            self.qualify_conventional_asset(&SwapAsset::Token { address: intermediate.clone(), symbol: String::new(), decimals: 18 })
                .await?;
        }
        let (allowance, balance) = match owner {
            Some(owner) => match from {
                SwapAsset::Quai => (None, Some(self.node.provider.balance(addr(owner)?, BlockTag::Latest).await?)),
                SwapAsset::Token { address, .. } => {
                    let erc = Erc20::new(addr(address)?, &self.node.provider)?;
                    let (allowance, balance) = futures::try_join!(
                        erc.allowance(addr(READ_CALLER)?, addr(owner)?, venue.router, BlockTag::Latest),
                        erc.balance_of(addr(READ_CALLER)?, addr(owner)?, BlockTag::Latest),
                    )?;
                    (Some(allowance), Some(balance))
                }
            },
            None => (None, None),
        };
        if balance.is_some_and(|balance| balance < maximum_input) {
            return Err(CoreError::Insufficient("balance is below the authorized maximum input".into()));
        }
        Ok(ExactOutputQuote {
            from: from.clone(),
            to: to.clone(),
            amount_out: amount_out.to_string(),
            required_input: required.to_string(),
            maximum_input: maximum_input.to_string(),
            path,
            router: venue.router.to_string(),
            venue: venue.venue,
            allowance: allowance.map(|a| a.to_string()),
            approval_needed: allowance.is_some_and(|a| a < maximum_input),
            balance: balance.map(|a| a.to_string()),
            observed_at: now(),
        })
    }

    /// The best route one exchange offers from `a` to `b` for an exact input, confirmed by that
    /// exchange's router; None when it has no route.
    async fn best_on_venue(
        &self,
        v: &VenueRouter,
        a: &str,
        b: &str,
        amount_in: U256,
        legs: &mut Legs,
        name: &dyn Fn(&str) -> String,
    ) -> Result<Option<Route>> {
        // Candidates: direct, one hub, then two hubs — the same paths `routes::RouteGraph` offers
        // the token picker, so a pair the picker allows is a pair the router will try. The two-hub
        // paths are what let a WQI-side token reach a WQUAI-side one.
        let hub_addresses: Vec<String> = self.hubs.iter().map(|(h, _)| h.clone()).collect();
        let candidates: Vec<Vec<(String, String)>> = crate::routes::candidate_paths(a, b, &hub_addresses)
            .into_iter()
            .map(|path| path.into_iter().map(|t| (t.clone(), name(&t))).collect())
            .collect();
        // Resolve every distinct hop once. Candidates share hops heavily (every path starts at `a`
        // and the hubs interconnect), so this is ~13 couples rather than one per hop per candidate,
        // and they are read together rather than one at a time.
        let mut wanted: Vec<(String, String)> = Vec::new();
        for route in &candidates {
            for pair in route.windows(2) {
                let couple = (pair[0].0.clone(), pair[1].0.clone());
                if !wanted.contains(&couple) {
                    wanted.push(couple);
                }
            }
        }
        self.resolve_legs(v, &wanted, legs).await?;
        // Rank locally on the reserves just read, so only the front-runners cost a router call.
        let mut ranked: Vec<Route> = Vec::new();
        for route in candidates {
            let mut pools = Vec::new();
            let mut hops = Vec::new();
            let mut out = amount_in;
            let mut ok = true;
            for pair in route.windows(2) {
                match legs.get(&(v.venue, pair[0].0.clone(), pair[1].0.clone())).copied().flatten() {
                    Some((address, rin, rout)) => {
                        pools.push(PoolHop {
                            pair: address.to_string().to_lowercase(),
                            reserve_in: rin.to_string(),
                            reserve_out: rout.to_string(),
                            tvl_usd: None,
                        });
                        hops.push((rin, rout));
                        out = amount_out(out, rin, rout);
                    }
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok && !out.is_zero() {
                ranked.push((out, route, pools, hops));
            }
        }
        // Shorter routes first among equals: fewer hops is less LP fee and less compounding impact.
        ranked.sort_by(|x, y| y.0.cmp(&x.0).then(x.1.len().cmp(&y.1.len())));
        // The router is the authority on the amount, so confirm the front-runners with it. Two
        // covers the case where a longer route through a deeper pool genuinely wins.
        // Both at once: they are independent reads, and in order they were two round trips.
        let front: Vec<Route> = ranked.into_iter().take(2).collect();
        let confirmed = futures::future::join_all(front.iter().map(|(_, route, ..)| {
            let path: Vec<String> = route.iter().map(|(p, _)| p.clone()).collect();
            async move { self.amounts_out(v.router, amount_in, &path).await }
        }))
        .await;
        let mut best: Option<Route> = None;
        for ((_, route, pools, hops), amounts) in front.into_iter().zip(confirmed) {
            let Some(out) = amounts.ok().and_then(|a| a.last().copied()) else { continue };
            if best.as_ref().is_none_or(|(o, ..)| out > *o) {
                best = Some((out, route, pools, hops));
            }
        }
        Ok(best)
    }

    /// Best route for an exact input: on one exchange when either fills the pair, else two swaps
    /// through a hub, one on each.
    pub async fn quote(
        &self,
        from: &SwapAsset,
        to: &SwapAsset,
        amount_in: U256,
        slippage_bps: u16,
        owner: Option<&str>,
    ) -> Result<SwapQuote> {
        self.quote_mode(from, to, amount_in, slippage_bps, owner, false, None).await
    }

    /// Compare a sequential alternative even when a single-venue route is available.
    /// The additional search has a hard time budget and its failure cannot hide the normal quote.
    pub async fn quote_alternatives(
        &self,
        from: &SwapAsset,
        to: &SwapAsset,
        amount_in: U256,
        slippage_bps: u16,
        owner: Option<&str>,
    ) -> Result<QuoteAlternatives> {
        let standard = self.quote(from, to, amount_in, slippage_bps, owner).await;
        let venues: Vec<_> = self.venues.iter().map(|v| v.venue).collect();
        collect_alternatives(standard, &venues, |cross, venue| self.quote_mode(from, to, amount_in, slippage_bps, owner, cross, venue))
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn quote_mode(
        &self,
        from: &SwapAsset,
        to: &SwapAsset,
        amount_in: U256,
        slippage_bps: u16,
        owner: Option<&str>,
        cross_only: bool,
        only_venue: Option<Venue>,
    ) -> Result<SwapQuote> {
        if amount_in.is_zero() {
            return Err(CoreError::Invalid("amount must be greater than zero".into()));
        }
        validate_slippage(slippage_bps)?;
        let a = from.path_address(&self.wquai);
        let b = to.path_address(&self.wquai);
        if a == b {
            return Err(CoreError::Invalid(if from.is_native() || to.is_native() {
                "QUAI ↔ WQUAI is a wrap, not a swap (Trade › Wrap)".into()
            } else {
                "choose two different tokens".into()
            }));
        }
        let name = |address: &str| {
            if address == a {
                from.symbol().to_string()
            } else if address == b {
                to.symbol().to_string()
            } else {
                self.hubs.iter().find(|(h, _)| h == address).map_or_else(|| address.to_string(), |(_, s)| s.clone())
            }
        };
        let mut legs = Legs::new();
        let mut single: Option<(usize, Route)> = None;
        let mut unavailable = Vec::new();
        let mut venue_warnings = self.verification_warnings.clone();
        // Every exchange at once. Each resolves its own hops (the legs are keyed by exchange, so
        // they share nothing), and one after another they were four chains of round trips.
        // Results are taken in exchange order, so a tie still goes to the one listed first.
        let asked = self.venues.iter().enumerate().filter(|(_, v)| only_venue.is_none_or(|venue| v.venue == venue));
        let answered = futures::future::join_all(asked.map(|(i, v)| {
            let (a, b, name) = (&a, &b, &name);
            async move {
                let mut own = Legs::new();
                let result = self.best_on_venue(v, a, b, amount_in, &mut own, name).await;
                (i, v, result, own)
            }
        }))
        .await;
        for (i, v, result, own) in answered {
            legs.extend(own);
            match result {
                Ok(Some(route)) if single.as_ref().is_none_or(|(_, best)| route.0 > best.0) => single = Some((i, route)),
                Ok(_) => {}
                Err(error) => {
                    unavailable.push(i);
                    venue_warnings.push(format!("{} quote unavailable: {error}", v.venue.label()));
                }
            }
        }
        if unavailable.len() == self.venues.len() {
            return Err(CoreError::Network(venue_warnings.join("; ")));
        }
        if only_venue.is_some() && single.is_none() {
            return Err(CoreError::NotFound("selected venue has no usable route".into()));
        }
        // Two swaps only when no exchange fills the pair alone — the same rule the picker's graph
        // applies (`routes::RouteGraph::route`).
        let plan: Vec<(usize, Route)> = match single.filter(|_| !cross_only) {
            Some(one) => vec![one],
            None => {
                let hub_addresses: Vec<String> = self.hubs.iter().map(|(h, _)| h.clone()).collect();
                let mut best: Option<Vec<(usize, Route)>> = None;
                for hub in crate::routes::two_swap_hubs(&a, &b, &hub_addresses) {
                    for (first_venue, second_venue) in crate::routes::venue_pairs() {
                        let (Some(i), Some(j)) = (
                            self.venues.iter().position(|v| v.venue == first_venue),
                            self.venues.iter().position(|v| v.venue == second_venue),
                        ) else {
                            continue;
                        };
                        if unavailable.contains(&i) || unavailable.contains(&j) {
                            continue;
                        }
                        let first = match self.best_on_venue(&self.venues[i], &a, &hub, amount_in, &mut legs, &name).await {
                            Ok(Some(route)) => route,
                            Ok(None) => continue,
                            Err(error) => {
                                venue_warnings.push(format!("{} alternative unavailable: {error}", first_venue.label()));
                                continue;
                            }
                        };
                        let second = match self.best_on_venue(&self.venues[j], &hub, &b, first.0, &mut legs, &name).await {
                            Ok(Some(route)) => route,
                            Ok(None) => continue,
                            Err(error) => {
                                venue_warnings.push(format!("{} alternative unavailable: {error}", second_venue.label()));
                                continue;
                            }
                        };
                        let combined: Vec<_> = first.1.iter().map(|p| &p.0).chain(second.1.iter().skip(1).map(|p| &p.0)).collect();
                        if combined.iter().collect::<std::collections::HashSet<_>>().len() != combined.len() {
                            continue;
                        }
                        if best.as_ref().is_none_or(|plan| second.0 > plan[1].1.0) {
                            best = Some(vec![(i, first), (j, second)]);
                        }
                    }
                }
                best.ok_or_else(|| CoreError::NotFound(format!("no Quainance pool route from {} to {}", from.symbol(), to.symbol())))?
            }
        };
        let out = plan.last().map(|(_, r)| r.0).unwrap_or_default();
        if out.is_zero() {
            return Err(CoreError::Insufficient("the pools return nothing for this amount".into()));
        }
        let all_hops: Vec<(U256, U256)> = plan.iter().flat_map(|(_, r)| r.3.iter().copied()).collect();
        let impact = impact_bps(amount_in, &all_hops);
        let mut warnings = venue_warnings;
        if impact >= IMPACT_REFUSE_BPS {
            return Err(CoreError::Rejected(format!("price impact {:.1}% is too high; try a smaller amount", impact as f64 / 100.0)));
        }
        if impact >= IMPACT_WARN_BPS {
            warnings.push(format!("price impact is {:.2}% — the pools are thin for this amount", impact as f64 / 100.0));
        }
        // Each swap's minimum. A second swap starts from whatever the first actually pays, so its
        // floor is what the second route gives for the first swap's own minimum, less slippage.
        let mut swap_legs: Vec<SwapLeg> = Vec::new();
        let mut leg_in = amount_in;
        let hub_decimals = match plan.as_slice() {
            [(_, first), _] => {
                let hub = first.1.last().map(|(h, _)| h.clone()).unwrap_or_default();
                let erc = Erc20::new(addr(&hub)?, &self.node.provider)?;
                let d = erc.contract().call(addr(READ_CALLER)?, "decimals", &[], BlockTag::Latest).await?;
                d.first()
                    .and_then(Value::as_str)
                    .and_then(|v| v.parse::<u8>().ok())
                    .ok_or_else(|| CoreError::Network("hub decimals".into()))?
            }
            _ => to.decimals(),
        };
        for (k, (i, (leg_out, route, pools, hops))) in plan.iter().enumerate() {
            let minimum = if k == 0 {
                minimum_out(*leg_out, slippage_bps)
            } else {
                let floor_in = swap_legs.last().map(|l| u(&l.minimum_out)).unwrap_or_default();
                chained_minimum(floor_in, hops, slippage_bps)
            };
            swap_legs.push(SwapLeg {
                venue: self.venues[*i].venue,
                router: self.venues[*i].router.to_string(),
                path: route.iter().map(|(p, _)| p.clone()).collect(),
                route: route.iter().map(|(_, s)| s.clone()).collect(),
                pools: pools.clone(),
                amount_in: leg_in.to_string(),
                amount_out: leg_out.to_string(),
                minimum_out: minimum.to_string(),
                output_decimals: if k + 1 == plan.len() { to.decimals() } else { hub_decimals },
            });
            leg_in = *leg_out;
        }
        if let [first, second] = swap_legs.as_slice() {
            warnings.push(format!(
                "two swaps: {} {}, then {} {} — each is reviewed, and the second is quoted again for what the first pays",
                first.route.join(" → "),
                first.venue.on(),
                second.route.join(" → "),
                second.venue.on()
            ));
        }
        let spender = self.venues[plan[0].0].router;
        let (allowance, approval_needed) = match (from, owner) {
            (SwapAsset::Token { address, .. }, Some(owner)) => {
                let erc = Erc20::new(addr(address)?, &self.node.provider)?;
                let allowance = erc.allowance(addr(READ_CALLER)?, addr(owner)?, spender, BlockTag::Latest).await?;
                (Some(allowance.to_string()), allowance < amount_in)
            }
            _ => (None, false),
        };
        // The paying balance (node state). A shortfall is flagged, never silently approved.
        let balance = match owner {
            Some(owner) => Some(match from {
                SwapAsset::Quai => self.node.provider.balance(addr(owner)?, BlockTag::Latest).await?,
                SwapAsset::Token { address, .. } => {
                    Erc20::new(addr(address)?, &self.node.provider)?.balance_of(addr(READ_CALLER)?, addr(owner)?, BlockTag::Latest).await?
                }
            }),
            None => None,
        };
        let insufficient = balance.is_some_and(|b| b < amount_in);
        if let Some(b) = balance.filter(|_| insufficient) {
            warnings.push(format!(
                "you have {} {}; not enough to pay {}",
                amount::format_amount(b, from.decimals()),
                from.symbol(),
                amount::format_amount(amount_in, from.decimals())
            ));
        }
        // The whole route, the hub between two swaps listed once.
        let mut path: Vec<String> = Vec::new();
        let mut route: Vec<String> = Vec::new();
        for leg in &swap_legs {
            let skip = usize::from(!path.is_empty());
            path.extend(leg.path.iter().skip(skip).cloned());
            route.extend(leg.route.iter().skip(skip).cloned());
        }
        Ok(SwapQuote {
            from: from.clone(),
            to: to.clone(),
            amount_in: amount_in.to_string(),
            amount_out: out.to_string(),
            minimum_out: swap_legs.last().map(|l| l.minimum_out.clone()).unwrap_or_default(),
            slippage_bps,
            path,
            route,
            pools: swap_legs.iter().flat_map(|l| l.pools.iter().cloned()).collect(),
            impact_bps: impact,
            fee_bps: LP_FEE_BPS * all_hops.len() as u64,
            router: spender.to_string(),
            allowance,
            approval_needed,
            balance: balance.map(|b| b.to_string()),
            insufficient,
            warnings,
            observed_at: now(),
            liquidity_at: None,
            legs: swap_legs,
        })
    }
}

/// Check one exchange's router and factory against their pins, and against each other: the
/// router's wrapped native must be the network's WQUAI and its factory the pinned one.
#[allow(clippy::too_many_arguments)]
async fn verify_venue(
    app: &AppDb,
    node: &Node,
    network: &NetworkProfile,
    venue: Venue,
    router_pin: &PinnedContract,
    factory_pin: &PinnedContract,
    wquai: &str,
    trust: Trust,
) -> Result<VenueRouter> {
    let label = venue.label();
    let pins = [(router_pin, format!("{label} router")), (factory_pin, format!("{label} factory"))];
    let pins: Vec<(&PinnedContract, &str)> = pins.iter().map(|(p, what)| (*p, what.as_str())).collect();
    let verified = verify_pinned_all(app, node, network, &pins, trust).await?;
    let (router, factory) = (verified[0], verified[1]);
    let caller = addr(READ_CALLER)?;
    let r = Contract::new(router, interface(ROUTER_ABI)?, &node.provider);
    let (weth, f) =
        futures::future::join(r.call(caller, "WETH", &[], BlockTag::Latest), r.call(caller, "factory", &[], BlockTag::Latest)).await;
    if weth?.first().and_then(Value::as_str).map(str::to_lowercase).as_deref() != Some(wquai) {
        return Err(CoreError::Rejected(format!("the {label} router's wrapped QUAI does not match this network's WQUAI")));
    }
    if f?.first().and_then(Value::as_str).map(str::to_lowercase) != Some(factory.to_string().to_lowercase()) {
        return Err(CoreError::Rejected(format!("the {label} router's factory does not match the pinned factory")));
    }
    Ok(VenueRouter { venue, router, factory })
}

/// A router's review label: `0x… (launch AMM, ✓ pinned bytecode)`.
pub(crate) fn router_field(network: &NetworkProfile, node: &Node, venue: Venue, router: &str) -> String {
    let trust = venue_pins(network, venue).map_or("", |(r, _)| r.trust_label_on(node));
    format!("{router} ({}, {trust})", venue.label())
}

/// Pools below this TVL (USD) get a thin-liquidity warning.
pub const THIN_POOL_USD: f64 = 1_000.0;

/// Add explorer pool liquidity (TVL) to a quote's pools, when market data is allowed. Missing
/// data leaves the quote unchanged: liquidity is context, never an input to amounts.
pub async fn attach_liquidity(ctx: &crate::data::DataCtx, quote: &mut SwapQuote) {
    if !ctx.policy.market {
        return;
    }
    let explorer = ctx.explorer.clone();
    // Decoration on a quote, from an endpoint that takes 3-15 s cold: it waits a moment, then uses
    // the last board it has. Waiting in full held a CLI swap at its quote for up to 15 s.
    let fresh = tokio::time::timeout(LIQUIDITY_WAIT, ctx.cached("pool_tvl", 300, || async move { explorer.pool_stats().await })).await;
    let board = match fresh {
        Ok(Ok(board)) => board.value,
        _ => match ctx.peek_cached::<crate::explorer::PoolBoard>("pool_tvl") {
            Some(board) => board.value,
            None => return,
        },
    };
    apply_liquidity(quote, &board);
}

/// How long a quote waits for the explorer's pool TVL before using the last one it has.
pub const LIQUIDITY_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// Pure part of [`attach_liquidity`].
pub fn apply_liquidity(quote: &mut SwapQuote, board: &crate::explorer::PoolBoard) {
    quote.liquidity_at = Some(board.observed_at);
    for hop in &mut quote.pools {
        hop.tvl_usd = board.pools.iter().find(|p| p.address.eq_ignore_ascii_case(&hop.pair)).and_then(|p| p.tvl_usd);
        if let Some(tvl) = hop.tvl_usd
            && tvl < THIN_POOL_USD
        {
            quote.warnings.push(format!(
                "thin liquidity: pool {} holds only {}",
                crate::session::short_address(&hop.pair),
                amount::usd(tvl)
            ));
        }
    }
}

/// Compact USD: `$950.20`, `$11.4k`, `$2.3M`.
pub fn usd_compact(v: f64) -> String {
    if v >= 1e6 {
        format!("${:.1}M", v / 1e6)
    } else if v >= 1e4 {
        format!("${:.1}k", v / 1e3)
    } else if v >= 1e3 {
        format!("${:.2}k", v / 1e3)
    } else {
        amount::usd(v)
    }
}

/// Symbols that must never be impersonated by another contract.
pub fn lookalike_warning(symbol: &str, address: &str, known: &[(String, String)]) -> Option<String> {
    // Contract identity wins over spelling; an imported alias must not accuse a canonical token.
    if known.iter().any(|(_, canonical)| canonical.eq_ignore_ascii_case(address)) {
        return None;
    }
    let fold = |s: &str| {
        s.chars()
            .take(64)
            .flat_map(char::to_uppercase)
            .map(|c| match c {
                '0' | 'Ο' | 'О' => 'O',
                '1' | 'I' | 'Ι' | 'І' | 'Ӏ' => 'L',
                'Ԝ' => 'W',
                'Ԛ' => 'Q',
                'Ѕ' => 'S',
                'Τ' | 'Т' => 'T',
                'Α' | 'А' => 'A',
                'Β' | 'В' => 'B',
                'Ε' | 'Е' => 'E',
                'Μ' | 'М' => 'M',
                other => other,
            })
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
    };
    let target = fold(symbol);
    if let Some((canonical, contract)) = known.iter().find(|(name, _)| fold(name) == target) {
        return Some(format!("{symbol} at {address} is not the configured {canonical} ({contract}) — possible lookalike"));
    }
    (!symbol.is_ascii()).then(|| format!("token at {address} uses a non-ASCII symbol; verify its contract address, not its spelling"))
}

pub(crate) fn canonical_tokens(network: &NetworkProfile) -> Vec<(String, String)> {
    let mut known = Vec::new();
    if let Some(address) = &network.wquai {
        known.push(("WQUAI".into(), address.clone()));
    }
    if let Some(address) = &network.wqi {
        known.push(("WQI".into(), address.clone()));
    }
    if let Some(token) = &network.ecosystem.usdt {
        known.push(("USDT".into(), token.address.clone()));
    }
    known
}

impl Session {
    /// Resolve a swap asset from `QUAI`, a local token symbol or a contract address.
    pub async fn swap_asset(&mut self, text: &str) -> Result<SwapAsset> {
        let t = text.trim();
        if t.eq_ignore_ascii_case("quai") {
            return Ok(SwapAsset::Quai);
        }
        self.ensure_default_tokens()?;
        if let Some(usdt) = self.network.ecosystem.usdt.clone()
            && self.app.token(&self.network.id, &usdt.address).is_err()
        {
            let _ = self.import_token(&usdt.address).await;
        }
        let token = match self.app.token(&self.network.id, t) {
            Ok(tok) => tok,
            Err(_) if t.starts_with("0x") => self.import_token(t).await?,
            Err(e) => return Err(e),
        };
        Ok(SwapAsset::Token { address: token.address.to_lowercase(), symbol: token.symbol, decimals: token.decimals })
    }

    /// Quote a swap for an account (includes its current router allowance).
    ///
    /// `trust` says whether this quote is going on screen or into a review. A review's quote is
    /// what sets `amountOutMin`, so it re-reads the pins and every pair address from the chain.
    pub async fn swap_quote(
        &mut self,
        account: Option<&str>,
        from: &str,
        to: &str,
        value: &str,
        slippage_bps: u16,
        trust: Trust,
    ) -> Result<SwapQuote> {
        let from_asset = self.swap_asset(from).await?;
        let to_asset = self.swap_asset(to).await?;
        let amount_in = amount::parse_amount(value, from_asset.decimals())?;
        let owner = match account {
            Some(_) => Some(self.account(account)?.address),
            None => self.account(None).ok().map(|a| a.address).or_else(|| self.quai_owner_addresses().into_iter().next()),
        };
        let router = Router::open(&self.app, &self.node, &self.network, trust).await?;
        let mut quote = router.quote(&from_asset, &to_asset, amount_in, slippage_bps, owner.as_deref()).await?;
        let known = canonical_tokens(&self.network);
        for asset in [&from_asset, &to_asset] {
            if let SwapAsset::Token { address, symbol, .. } = asset
                && let Some(w) = lookalike_warning(symbol, address, &known)
            {
                quote.warnings.push(w);
            }
        }
        if trust.may_cache()
            && let Ok(ctx) = self.data_ctx()
        {
            attach_liquidity(&ctx, &mut quote).await;
        }
        Ok(quote)
    }

    /// Quote an exact recipient output under a caller-supplied input cap. This is a distinct
    /// mode; no exact-input fallback and no sequential route can satisfy its atomic guarantee.
    #[allow(clippy::too_many_arguments)]
    pub async fn swap_exact_output_quote(
        &mut self,
        account: Option<&str>,
        from: &str,
        to: &str,
        output: &str,
        maximum_input: &str,
        trust: Trust,
    ) -> Result<ExactOutputQuote> {
        let from_asset = self.swap_asset(from).await?;
        let to_asset = self.swap_asset(to).await?;
        let output = amount::parse_amount(output, to_asset.decimals())?;
        let maximum = amount::parse_amount(maximum_input, from_asset.decimals())?;
        let owner = match account {
            Some(_) => Some(self.account(account)?.address),
            None => self.account(None).ok().map(|a| a.address),
        };
        let router = Router::open(&self.app, &self.node, &self.network, trust).await?;
        tokio::time::timeout(
            std::time::Duration::from_secs(12),
            router.quote_exact_output(&from_asset, &to_asset, output, maximum, owner.as_deref()),
        )
        .await
        .map_err(|_| CoreError::Network("exact-output quote exceeded its 12 s budget".into()))?
    }

    /// One review at a time: clear incompatible nonzero allowance, approve the authorized cap,
    /// then execute. Re-quote after each receipt; a moved price cannot increase the input cap.
    #[allow(clippy::too_many_arguments)]
    pub async fn swap_exact_output_next(
        &mut self,
        account: Option<&str>,
        from: &str,
        to: &str,
        output: &str,
        maximum_input: &str,
        deadline_minutes: u32,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        self.require_execution_source()?;
        validate_deadline(deadline_minutes)?;
        let quote = self.swap_exact_output_quote(account, from, to, output, maximum_input, Trust::FirstHand).await?;
        let owner = self.account(account)?;
        let maximum = u(&quote.maximum_input);
        // The input the node's router quoted is a call result. What the pools need for this output,
        // proven at the anchor, is not: a quote above it by more than half a percent is refused, so
        // an inflated price cannot become the cap the user is invited to sign.
        let mut exact_warnings = Vec::new();
        if let Some((reserves, anchored)) = prove_path_reserves(&self.node, &self.network, quote.venue, &quote.path).await?
            && let Some(needed) = input_for_output(u(&quote.amount_out), &reserves)
        {
            let quoted = u(&quote.required_input);
            let scale = U256::from(10_000u64);
            if quoted.saturating_mul(scale) > needed.saturating_mul(scale + U256::from(POOL_CHECK_MIN_BPS)) {
                return Err(CoreError::Rejected(format!(
                    "the node's quote needs {} {} for this output, more than the pools do ({}, {}); refusing to review it",
                    amount::format_amount(quoted, quote.from.decimals()),
                    quote.from.symbol(),
                    amount::format_amount(needed, quote.from.decimals()),
                    anchored.confirmation.text()
                )));
            }
            if maximum < needed {
                exact_warnings.push(format!(
                    "the pools need {} {} for this output, above your maximum; the swap would revert",
                    amount::format_amount(needed, quote.from.decimals()),
                    quote.from.symbol()
                ));
            }
        }
        if quote.approval_needed {
            let SwapAsset::Token { address, symbol, decimals } = &quote.from else {
                return Err(CoreError::Invalid("native input cannot require allowance".into()));
            };
            let reset = quote.allowance.as_deref().is_some_and(|a| !u(a).is_zero());
            let allowance = if reset { U256::ZERO } else { maximum };
            let call = Erc20::new(addr(address)?, &self.node.provider)?.approve(addr(&quote.router)?, allowance)?;
            let call = with_access_list(&self.node.provider, addr(&owner.address)?, call).await?;
            return self.prepare_account(AccountRequest {
                from: owner, intent: call.into_account_intent(), kind: OpKind::Approve,
                title: if reset { format!("Clear {symbol} allowance before exact-output swap") } else { format!("Approve {symbol} exact-output input cap") },
                asset: symbol.clone(), amount: allowance, decimals: *decimals, counterparty: quote.router.clone(),
                fields: vec![field("Token contract", address.clone()), field("Spender", quote.router.clone()),
                    field("Allowance", format!("{} {symbol}", amount::format_amount(allowance, *decimals))),
                    field("Maximum swap input", amount::format_amount(maximum, *decimals))],
                warnings: vec!["This approval does not execute the swap; review again after its receipt. Unspent authorized allowance may remain after execution.".into()],
                detail: json!({"token": address, "decimals": decimals, "purpose": "swap_exact_output", "spender": quote.router}).into(),
                max_gas: 120_000, max_fee: self.parse_fee_cap(max_fee, QUAI_DECIMALS)?,
            }).await;
        }
        let recipient = owner.address.clone();
        let deadline = now().checked_add(u64::from(deadline_minutes) * 60).ok_or_else(|| CoreError::Invalid("deadline overflow".into()))?;
        let contract = Contract::new(addr(&quote.router)?, interface(ROUTER_ABI)?, &self.node.provider);
        let (method, args, value) =
            router_call_parameters(&quote.from, &quote.to, maximum, u(&quote.amount_out), &quote.path, &recipient, deadline, true);
        let call = contract.prepare(method, &args, value)?;
        let call = with_access_list(&self.node.provider, addr(&recipient)?, call).await?;
        let token = |asset: &SwapAsset| match asset {
            SwapAsset::Quai => "quai".into(),
            SwapAsset::Token { address, .. } => address.clone(),
        };
        let refund = if quote.from.is_native() {
            "Unused native input is refunded by the same router transaction."
        } else {
            "Only required input is transferred; the unused cap remains in your wallet and some allowance may remain."
        };
        self.prepare_account(AccountRequest {
            from: owner, intent: call.into_account_intent(), kind: OpKind::SwapExactOutput,
            title: format!("Receive exactly {} {}", amount::format_amount(u(&quote.amount_out), quote.to.decimals()), quote.to.symbol()),
            asset: quote.from.symbol().into(), amount: maximum, decimals: quote.from.decimals(), counterparty: quote.router.clone(),
            fields: vec![field("Mode", "Exact output; atomic maximum input"),
                field("Maximum input", format!("{} {}", amount::format_amount(maximum, quote.from.decimals()), quote.from.symbol())),
                field("Quoted input", amount::format_amount(u(&quote.required_input), quote.from.decimals())),
                field("Exact recipient output", format!("{} {}", amount::format_amount(u(&quote.amount_out), quote.to.decimals()), quote.to.symbol())),
                field("Recipient", recipient.clone()), field("Path contracts", quote.path.join(" → ")),
                field("Router", router_field(&self.network, &self.node, quote.venue, &quote.router)), field("Deadline", deadline.to_string())],
            warnings: [exact_warnings, vec![refund.into(), "Conventional pinned-token semantics only. A price move beyond the input cap reverts the transaction; fees may still be spent.".into()]].concat(),
            detail: json!({"expires_at": deadline, "decimals": quote.from.decimals(), "from_token": token(&quote.from), "to_token": token(&quote.to),
                "to_decimals": quote.to.decimals(), "to_symbol": quote.to.symbol(), "expected_out": quote.amount_out,
                "minimum_out": quote.amount_out, "maximum_input": quote.maximum_input, "required_input": quote.required_input,
                "path": quote.path, "venue": quote.venue, "router": quote.router, "recipient": recipient, "quote_mode": "exact_output",
                "financial_effects": [
                    {"direction": "out", "asset": quote.from.symbol(), "token": token(&quote.from), "decimals": quote.from.decimals(),
                        "amount": quote.maximum_input, "estimated": false, "note": "at most; unused input stays or is refunded"},
                    {"direction": "in", "asset": quote.to.symbol(), "token": token(&quote.to), "decimals": quote.to.decimals(),
                        "amount": quote.amount_out, "minimum": quote.amount_out, "estimated": false, "note": "exact recipient output"}
                ]}).into(),
            max_gas: 400_000 + 150_000 * quote.path.len().saturating_sub(1) as u64,
            max_fee: self.parse_fee_cap(max_fee, QUAI_DECIMALS)?,
        }).await
    }

    /// Estimate a two-route sequential split. Native/WQUAI output has an exact fee conversion;
    /// other output assets retain an explicit unknown-price result until a qualified conversion is supplied.
    pub async fn swap_split_quote(
        &mut self,
        account: Option<&str>,
        from: &str,
        to: &str,
        value: &str,
        slippage_bps: u16,
        slices: u16,
    ) -> Result<crate::split_routes::SplitDecision> {
        let from_asset = self.swap_asset(from).await?;
        let to_asset = self.swap_asset(to).await?;
        let total = amount::parse_amount(value, from_asset.decimals())?;
        if total < U256::from(2) {
            return Err(CoreError::Invalid("split input must contain at least two base units".into()));
        }
        let owner = match account {
            Some(_) => Some(self.account(account)?.address),
            None => self.account(None).ok().map(|a| a.address),
        };
        let native_balance = match owner.as_deref() {
            Some(owner) => Some(self.node.provider.balance(addr(owner)?, BlockTag::Latest).await?),
            None => None,
        };
        let router = Router::open(&self.app, &self.node, &self.network, Trust::Cached).await?;
        router.qualify_conventional_asset(&from_asset).await?;
        router.qualify_conventional_asset(&to_asset).await?;
        // Half-size probes expose viable branches even when the unsplit trade would exceed the impact limit.
        let probe = total / U256::from(2) + total % U256::from(2);
        let alternatives = router.quote_alternatives(&from_asset, &to_asset, probe, slippage_bps, owner.as_deref()).await?;
        let gas = self.node.provider.gas_price(crate::network::ZONE).await?;
        let fee_ratio = (to_asset.path_address(&router.wquai) == router.wquai).then_some((U256::from(1), U256::from(1)));
        let mut candidates = Vec::new();
        for quote in alternatives.quotes.into_iter().filter(|q| q.legs.len() == 1).take(crate::split_routes::MAX_CANDIDATES) {
            if !venue_pins(&self.network, quote.legs[0].venue).is_some_and(|(r, f)| r.code_hash.is_some() && f.code_hash.is_some()) {
                continue;
            }
            if quote.balance.as_deref().is_some_and(|balance| u(balance) < total) {
                return Err(CoreError::Insufficient("balance cannot fund the complete split input".into()));
            }
            for intermediate in quote.path.iter().skip(1).take(quote.path.len().saturating_sub(2)) {
                router
                    .qualify_conventional_asset(&SwapAsset::Token { address: intermediate.clone(), symbol: String::new(), decimals: 18 })
                    .await?;
            }
            let mut full_cap = quote.clone();
            full_cap.amount_in = total.to_string();
            if matches!(full_cap.from, SwapAsset::Token { .. }) {
                // A quote at a smaller probe must not underestimate approvals needed by an allocation.
                full_cap.approval_needed = full_cap.allowance.as_deref().is_none_or(|allowance| u(allowance) < total);
            }
            let cost = crate::routes::estimate_cost(&full_cap, gas, fee_ratio, native_balance)?;
            candidates.push(crate::split_routes::SplitCandidate {
                quote,
                fee_native_estimate: cost.fee_native_estimate,
                fee_output_estimate: cost.fee_output_estimate,
            });
        }
        let mut decision = crate::split_routes::optimize(&candidates, total, slices, now(), native_balance)?;
        if !alternatives.omitted.is_empty() {
            decision.reason.push_str("; some route alternatives were unavailable");
        }
        Ok(decision)
    }

    /// Review one sequential allocation. The caller must hold the durable plan claim, reconcile
    /// prior receipts and persist the returned operation before authorization. A saved plan is
    /// never signing authority and this method does not choose/replay the caller's stage index.
    pub async fn review_split_allocation_next(
        &mut self,
        account: Option<&str>,
        plan: &crate::split_routes::SplitPlan,
        index: usize,
        deadline_minutes: u32,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        self.require_execution_source()?;
        validate_deadline(deadline_minutes)?;
        crate::split_routes::validate_plan(plan)?;
        let selected = plan.allocations.get(index).ok_or_else(|| CoreError::Invalid("invalid split allocation index".into()))?;
        let identify = |asset: &SwapAsset| match asset {
            SwapAsset::Quai => "QUAI".into(),
            SwapAsset::Token { address, .. } => address.clone(),
        };
        let from = self.swap_asset(&identify(&plan.from)).await?;
        let to = self.swap_asset(&identify(&plan.to)).await?;
        if from.decimals() != plan.from.decimals() || to.decimals() != plan.to.decimals() {
            return Err(CoreError::Rejected("split token precision changed; construct a new reviewed plan".into()));
        }
        let owner = self.account(account)?;
        let input = U256::from_str_radix(&selected.amount_in, 10).map_err(|_| CoreError::Invalid("invalid split input".into()))?;
        let router = Router::open(&self.app, &self.node, &self.network, Trust::FirstHand).await?;
        router.qualify_conventional_asset(&from).await?;
        router.qualify_conventional_asset(&to).await?;
        if !venue_pins(&self.network, selected.venue).is_some_and(|(r, f)| r.code_hash.is_some() && f.code_hash.is_some()) {
            return Err(CoreError::Rejected("split execution requires a pinned router and factory".into()));
        }
        let mut quote = router.quote_mode(&from, &to, input, plan.slippage_bps, Some(&owner.address), false, Some(selected.venue)).await?;
        for intermediate in quote.path.iter().skip(1).take(quote.path.len().saturating_sub(2)) {
            router
                .qualify_conventional_asset(&SwapAsset::Token { address: intermediate.clone(), symbol: String::new(), decimals: 18 })
                .await?;
        }
        if !quote.router.eq_ignore_ascii_case(&selected.router)
            || quote.path != selected.path
            || quote.pools.iter().map(|p| p.pair.to_lowercase()).collect::<Vec<_>>() != selected.pools
        {
            return Err(CoreError::Rejected("selected split route changed; re-plan the unfilled allocation".into()));
        }
        let protected = U256::from_str_radix(&selected.minimum_out, 10).map_err(|_| CoreError::Invalid("invalid split minimum".into()))?;
        require_minimum(protected)?;
        if u(&quote.amount_out) < protected {
            return Err(CoreError::Rejected("split allocation price moved beyond its authorized minimum".into()));
        }
        quote.minimum_out = protected.max(u(&quote.minimum_out)).to_string();
        let [leg] = quote.legs.as_mut_slice() else {
            return Err(CoreError::Rejected("split review requires exactly one router transaction".into()));
        };
        leg.minimum_out = quote.minimum_out.clone();
        if quote.insufficient {
            return Err(CoreError::Insufficient("balance cannot fund the unfilled split allocation".into()));
        }
        let context = json!({"allocation_index": index, "allocation_count": plan.allocations.len(), "total_input": plan.total_input,
            "allocation_input": selected.amount_in, "allocation_minimum": quote.minimum_out, "pool_contracts": selected.pools});
        if quote.approval_needed {
            let SwapAsset::Token { address, symbol, decimals } = &from else { return Err(CoreError::Invalid("native approval".into())) };
            let reset = quote.allowance.as_deref().is_some_and(|a| !u(a).is_zero());
            let approved = if reset { U256::ZERO } else { input };
            let call = Erc20::new(addr(address)?, &self.node.provider)?.approve(addr(&quote.router)?, approved)?;
            let call = with_access_list(&self.node.provider, addr(&owner.address)?, call).await?;
            return self.prepare_account(AccountRequest {
                from: owner, intent: call.into_account_intent(), kind: OpKind::Approve,
                title: format!("{} {} for split allocation {} of {}", if reset { "Clear allowance for" } else { "Approve" }, symbol, index + 1, plan.allocations.len()),
                asset: symbol.clone(), amount: approved, decimals: *decimals, counterparty: quote.router.clone(),
                fields: vec![field("Token contract", address.clone()), field("Spender", quote.router.clone()),
                    field("Exact allowance", amount::format_amount(approved, *decimals)), field("Split allocation", format!("{} of {}", index + 1, plan.allocations.len()))],
                warnings: vec!["This is a separately reviewed sequential allocation; earlier confirmed fills cannot be rolled back.".into()],
                detail: json!({"token": address, "decimals": decimals, "spender": quote.router, "purpose": "split_swap", "split": context}).into(),
                max_gas: 120_000, max_fee: self.parse_fee_cap(max_fee, QUAI_DECIMALS)?,
            }).await;
        }
        quote.warnings.push(format!(
            "Split allocation {} of {}: independently confirmed; no atomic combined-output guarantee. Preserve earlier fills on failure.",
            index + 1,
            plan.allocations.len()
        ));
        self.prepare_swap_quote(account, quote, deadline_minutes, max_fee, Some(context)).await
    }

    /// Explicit route alternatives; includes a cross-venue candidate even if a direct route exists.
    pub async fn swap_alternatives(
        &mut self,
        account: Option<&str>,
        from: &str,
        to: &str,
        value: &str,
        slippage_bps: u16,
    ) -> Result<QuoteAlternatives> {
        let from_asset = self.swap_asset(from).await?;
        let to_asset = self.swap_asset(to).await?;
        let atoms = amount::parse_amount(value, from_asset.decimals())?;
        let owner = match account {
            Some(_) => Some(self.account(account)?.address),
            None => self.account(None).ok().map(|a| a.address),
        };
        let router = Router::open(&self.app, &self.node, &self.network, Trust::Cached).await?;
        router.quote_alternatives(&from_asset, &to_asset, atoms, slippage_bps, owner.as_deref()).await
    }

    /// Review the exact approval a token-input swap needs (step 1 of 2), for the router of the
    /// exchange the route starts on.
    /// Hold a review's quote against its proven pools. A router quote below what the pools give,
    /// by more than the slippage the user accepted, would lower the signed minimum and hand the
    /// difference to whoever trades first: that refuses the review. A quote above them only risks
    /// a revert, and says so.
    async fn check_quote_against_pools(&self, quote: &mut SwapQuote, venue: Venue) -> Result<()> {
        let (amount_in, quoted) = (u(&quote.amount_in), u(&quote.amount_out));
        let pairs: Vec<String> = quote.pools.iter().map(|p| p.pair.clone()).collect();
        let Some(proven) = prove_route(&self.node, &self.network, venue, &quote.path, &pairs, amount_in).await? else { return Ok(()) };
        let verdict = quote_verdict(quoted, proven.amount_out, quote.slippage_bps);
        if verdict == QuoteVerdict::Understated {
            return Err(CoreError::Rejected(format!(
                "the node's quote ({}) is below what the pools hold for this trade ({}, {}); refusing a minimum that low",
                quote.receive_text(),
                amount::format_amount(proven.amount_out, quote.to.decimals()),
                proven.confirmation.text()
            )));
        }
        if verdict == QuoteVerdict::Overstated {
            quote.warnings.push(format!(
                "the node's quote is above what the pools hold ({}); the swap may revert at its minimum",
                amount::format_amount(proven.amount_out, quote.to.decimals())
            ));
        }
        Ok(())
    }

    pub async fn review_swap_approval(
        &mut self,
        account: Option<&str>,
        from: &str,
        to: &str,
        value: &str,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        let asset = self.swap_asset(from).await?;
        let SwapAsset::Token { address, symbol, decimals } = asset else {
            return Err(CoreError::Invalid("QUAI needs no approval".into()));
        };
        let atoms = amount::parse_amount(value, decimals)?;
        let from_account = self.account(account)?;
        // Never approve a token the account cannot pay with (wrapped Qi, for one, needs its claim first).
        let balance = Erc20::new(addr(&address)?, &self.node.provider)?
            .balance_of(addr(&from_account.address)?, addr(&from_account.address)?, BlockTag::Latest)
            .await?;
        if balance < atoms {
            return Err(CoreError::Insufficient(format!(
                "{symbol} balance is {}; the swap needs {}{}",
                amount::format_amount(balance, decimals),
                amount::format_amount(atoms, decimals),
                if self.network.wqi.as_ref().is_some_and(|w| w.eq_ignore_ascii_case(&address)) {
                    " (wrapped Qi arrives after `wrap claim`)"
                } else {
                    ""
                }
            )));
        }
        let quote = self.swap_quote(account, from, to, value, 50, Trust::FirstHand).await?;
        let venue = quote.legs.first().map_or(Venue::Main, |l| l.venue);
        self.verify_protected_exact_input(&quote).await?;
        let router = addr(&quote.router)?;
        let atoms = self.bounded_allowance(&address, addr(&from_account.address)?, router, atoms).await?;
        let call = Erc20::new(addr(&address)?, &self.node.provider)?.approve(router, atoms)?;
        let call = with_access_list(&self.node.provider, addr(&from_account.address)?, call).await?;
        self.prepare_account(AccountRequest {
            from: from_account,
            intent: call.into_account_intent(),
            kind: OpKind::Approve,
            title: format!("Approve {symbol} for swap (step 1 of 2)"),
            asset: symbol.clone(),
            amount: atoms,
            decimals,
            counterparty: router.to_string(),
            fields: vec![
                field("Token contract", address.clone()),
                field("Spender", router_field(&self.network, &self.node, venue, &router.to_string())),
                field("Allowance", format!("exactly {} {symbol}", amount::format_amount(atoms, decimals))),
            ],
            warnings: quote.warnings.clone(),
            detail: json!({"token": address, "decimals": decimals, "purpose": "swap", "spender": router.to_string()}).into(),
            max_gas: 120_000,
            max_fee: self.parse_fee_cap(max_fee, QUAI_DECIMALS)?,
        })
        .await
    }

    /// Review a swap (re-quotes on-chain; token inputs must already be approved).
    pub async fn review_swap(
        &mut self,
        account: Option<&str>,
        from: &str,
        to: &str,
        value: &str,
        slippage_bps: u16,
        deadline_minutes: u32,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        validate_deadline(deadline_minutes)?;
        let quote = self.swap_quote(account, from, to, value, slippage_bps, Trust::FirstHand).await?;
        self.prepare_swap_quote(account, quote, deadline_minutes, max_fee, None).await
    }

    /// A durable bounded swap step: validate fresh limits before any approval or spend.
    #[allow(clippy::too_many_arguments)]
    pub async fn swap_bounded_next(
        &mut self,
        account: Option<&str>,
        from: &str,
        to: &str,
        value: &str,
        slippage: u16,
        deadline: u32,
        max_fee: Option<&str>,
        bounds: &SwapBounds,
    ) -> Result<Review> {
        let mut quote = self.swap_quote(account, from, to, value, slippage, Trust::FirstHand).await?;
        if quote.legs.len() > 1 {
            return Err(CoreError::Rejected("explicit swap bounds require a single atomic route".into()));
        }
        bounds.apply(&mut quote)?;
        if quote.approval_needed {
            self.review_swap_approval(account, from, to, value, max_fee).await
        } else {
            self.prepare_swap_quote(account, quote, deadline, max_fee, None).await
        }
    }

    async fn verify_protected_exact_input(&self, quote: &SwapQuote) -> Result<()> {
        let venue = quote.legs.first().map_or(Venue::Main, |leg| leg.venue);
        if !venue_pins(&self.network, venue).is_some_and(|(router, factory)| router.code_hash.is_some() && factory.code_hash.is_some()) {
            return Err(CoreError::Rejected("recipient-protected swaps require a pinned Router02 and factory".into()));
        }
        let (method, _, _) = router_call_parameters(&quote.from, &quote.to, U256::ZERO, U256::ZERO, &quote.path, &quote.router, 0, false);
        let signature = if quote.from.is_native() {
            format!("{method}(uint256,address[],address,uint256)")
        } else {
            format!("{method}(uint256,uint256,address[],address,uint256)")
        };
        let runtime = self.node.raw("quai_getCode", json!([quote.router, "latest"])).await?;
        let runtime = runtime.as_str().ok_or_else(|| CoreError::Network("missing router runtime".into()))?;
        let bytes = hex::decode(runtime.trim_start_matches("0x")).map_err(|_| CoreError::Network("invalid router runtime".into()))?;
        let selector = quai_sdk::abi::function_selector(&signature).map_err(|e| CoreError::Invalid(e.to_string()))?;
        if !crate::contracts::selectors(&bytes).contains(&selector) {
            return Err(CoreError::Rejected("this router lacks the recipient-balance protected exact-input method".into()));
        }
        Ok(())
    }

    async fn prepare_swap_quote(
        &mut self,
        account: Option<&str>,
        quote: SwapQuote,
        deadline_minutes: u32,
        max_fee: Option<&str>,
        split: Option<Value>,
    ) -> Result<Review> {
        validate_deadline(deadline_minutes)?;
        self.verify_protected_exact_input(&quote).await?;
        let slippage_bps = quote.slippage_bps;
        require_minimum(U256::from_str_radix(&quote.minimum_out, 10).map_err(|_| CoreError::Invalid("invalid protected output".into()))?)?;
        if quote.approval_needed {
            let token = match &quote.from {
                SwapAsset::Token { address, .. } => address.clone(),
                _ => String::new(),
            };
            return Err(crate::error::approval_needed(
                token,
                format!("approve exactly {} for the Quainance router first (step 1 of 2)", quote.pay_text()),
            ));
        }
        // A route across both exchanges is two transactions: each is its own swap, to the hub and
        // then from it, so nothing ever signs half a route by accident.
        if let Some((_, hub)) = quote.hub() {
            return Err(CoreError::Invalid(format!(
                "{} takes two swaps; swap {} → {hub} first, then {hub} → {}",
                quote.route_text(),
                quote.from.symbol(),
                quote.to.symbol()
            )));
        }
        let venue = quote.legs.first().map_or(Venue::Main, |l| l.venue);
        let mut quote = quote;
        self.check_quote_against_pools(&mut quote, venue).await?;
        let quote = quote;
        let from_account = self.account(account)?;
        let recipient = from_account.address.clone();
        let contract = Contract::new(addr(&quote.router)?, interface(ROUTER_ABI)?, &self.node.provider);
        let deadline = now() + u64::from(deadline_minutes) * 60;
        let amount_in = u(&quote.amount_in);
        let min_out = u(&quote.minimum_out);
        // Balance check on the paying side (node state, not the indexer).
        match &quote.from {
            SwapAsset::Quai => {
                let bal = self.node.provider.balance(addr(&recipient)?, BlockTag::Latest).await?;
                if bal < amount_in {
                    return Err(CoreError::Insufficient(format!("QUAI balance is {}", amount::quai(bal))));
                }
            }
            SwapAsset::Token { address, symbol, decimals } => {
                let bal = Erc20::new(addr(address)?, &self.node.provider)?
                    .balance_of(addr(&recipient)?, addr(&recipient)?, BlockTag::Latest)
                    .await?;
                if bal < amount_in {
                    return Err(CoreError::Insufficient(format!("{symbol} balance is {}", amount::format_amount(bal, *decimals))));
                }
            }
        }
        let (method, args, value) =
            router_call_parameters(&quote.from, &quote.to, amount_in, min_out, &quote.path, &recipient, deadline, false);
        let call = contract.prepare(method, &args, value)?;
        let call = with_access_list(&self.node.provider, addr(&recipient)?, call).await?;
        let mut fields = vec![
            field("Route", quote.route.join(" → ")),
            field("You pay", quote.pay_text()),
            field("Expected", format!("≈ {}", quote.receive_text())),
            field("Minimum received", format!("{}  (slippage {:.2}%)", quote.minimum_text(), f64::from(slippage_bps) / 100.0)),
            field("Price impact", format!("{:.2}%", quote.impact_bps as f64 / 100.0)),
            field("Output protection", "router checks received balance; transfer fees reduce the quoted estimate"),
            field("LP fee", format!("{:.1}%", quote.fee_bps as f64 / 100.0)),
            field("Router", router_field(&self.network, &self.node, venue, &quote.router)),
            field("Deadline", format!("{deadline_minutes} min")),
        ];
        for pool in &quote.pools {
            fields.push(field("Pool", pool.pair.clone()));
        }
        let (to_address, to_decimals) = match &quote.to {
            SwapAsset::Quai => ("quai".to_string(), QUAI_DECIMALS),
            SwapAsset::Token { address, decimals, .. } => (address.clone(), *decimals),
        };
        self.prepare_account(AccountRequest {
            from: from_account,
            intent: call.into_account_intent(),
            kind: OpKind::Swap,
            title: format!("Swap {} → {}", quote.from.symbol(), quote.to.symbol()),
            asset: quote.from.symbol().to_string(),
            amount: amount_in,
            decimals: quote.from.decimals(),
            counterparty: quote.router.clone(),
            fields: std::mem::take(&mut fields),
            warnings: quote.warnings.clone(),
            detail: json!({"expires_at": deadline,
                "decimals": quote.from.decimals(),
                "from_token": match &quote.from { SwapAsset::Quai => "quai".to_string(), SwapAsset::Token { address, .. } => address.clone() },
                "to_symbol": quote.to.symbol(),
                "to_token": to_address,
                "to_decimals": to_decimals,
                "expected_out": quote.amount_out,
                "minimum_out": quote.minimum_out,
                "path": quote.path,
                "route": quote.route,
                "venue": venue,
                "router": quote.router,
                "recipient": recipient,
                "financial_effects": [
                    {"direction":"out","asset":quote.from.symbol(),"token":match &quote.from {SwapAsset::Quai=>"quai".to_owned(),SwapAsset::Token{address,..}=>address.clone()},"decimals":quote.from.decimals(),"amount":quote.amount_in},
                    {"direction":"in","asset":quote.to.symbol(),"token":to_address,"decimals":to_decimals,"amount":quote.amount_out,"minimum":quote.minimum_out,"estimated":true,"note":"router enforces received balance"}
                ],
                "split": split,
            }).into(),
            max_gas: 400_000 + 150_000 * quote.pools.len() as u64,
            max_fee: self.parse_fee_cap(max_fee, QUAI_DECIMALS)?,
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A quote under its proven pools by more than the slippage refuses; within it agrees; above
    /// it warns. Reserves a block apart never refuse inside half a percent.
    #[test]
    fn a_quote_is_held_to_its_proven_pools() {
        let proven = U256::from(1_000_000u64);
        assert_eq!(quote_verdict(U256::from(996_000u64), proven, 50), QuoteVerdict::Agrees);
        assert_eq!(quote_verdict(U256::from(994_000u64), proven, 50), QuoteVerdict::Understated);
        assert_eq!(quote_verdict(U256::from(994_000u64), proven, 100), QuoteVerdict::Agrees, "1% slippage accepts 0.6%");
        assert_eq!(quote_verdict(U256::from(997_000u64), proven, 10), QuoteVerdict::Agrees, "never tighter than half a percent");
        assert_eq!(quote_verdict(U256::from(1_010_000u64), proven, 50), QuoteVerdict::Overstated);
        let packed = (U256::from(7u64) << 224) | (U256::from(5u64) << 112) | U256::from(3u64);
        assert_eq!(unpack_reserves(packed), (U256::from(3u64), U256::from(5u64)), "reserve0 low, reserve1 next, time on top");
    }

    /// A found pair is remembered for a long time but not forever, an absence only briefly, and a
    /// quote that is feeding a review reads every one of them again.
    #[tokio::test]
    async fn impact_rejection_of_default_quote_does_not_hide_a_healthy_venue() {
        let fixture = || SwapQuote {
            from: SwapAsset::Quai,
            to: SwapAsset::Quai,
            amount_in: "1".into(),
            amount_out: "2".into(),
            minimum_out: "1".into(),
            slippage_bps: 50,
            path: vec![],
            route: vec![],
            pools: vec![],
            impact_bps: 0,
            fee_bps: 0,
            router: "legacy".into(),
            allowance: None,
            approval_needed: false,
            balance: None,
            insufficient: false,
            warnings: vec![],
            observed_at: 1,
            liquidity_at: None,
            legs: vec![],
        };
        let result = collect_alternatives(
            Err(CoreError::Rejected("price impact too high".into())),
            &[Venue::Main, Venue::Legacy],
            |cross, venue| {
                let quote = fixture();
                async move {
                    if !cross && venue == Some(Venue::Legacy) { Ok(quote) } else { Err(CoreError::NotFound("no usable route".into())) }
                }
            },
        )
        .await
        .unwrap();
        assert_eq!(result.quotes.len(), 1);
        assert_eq!(result.quotes[0].router, "legacy");
        assert!(result.omitted.iter().any(|s| s.contains("price impact")));
        assert!(
            collect_alternatives(Err(CoreError::NotFound("none".into())), &[Venue::Main], |_, _| async {
                Err(CoreError::NotFound("none".into()))
            })
            .await
            .is_err()
        );
    }

    #[test]
    fn prepared_router_calls_bind_native_value_recipient_deadline_and_mode_bounds() {
        let network = NetworkProfile::builtins().remove(0);
        let node = network.node().unwrap();
        let address = network.wquai.clone().unwrap();
        let recipient = "0x0000000000000000000000000000000000000011";
        let contract = Contract::new(addr(&address).unwrap(), interface(ROUTER_ABI).unwrap(), &node.provider);
        let token = SwapAsset::Token { address: address.clone(), symbol: "WQUAI".into(), decimals: 18 };
        let path = vec![address.clone(), recipient.into()];
        for exact_output in [false, true] {
            for (from, to) in [(SwapAsset::Quai, token.clone()), (token.clone(), SwapAsset::Quai), (token.clone(), token.clone())] {
                let (method, args, value) =
                    router_call_parameters(&from, &to, U256::from(123), U256::from(45), &path, recipient, 999, exact_output);
                let prepared = contract.prepare(method, &args, value).unwrap();
                assert_eq!(prepared.value(), if from.is_native() { U256::from(123) } else { U256::ZERO });
                let decoded = prepared.arguments().unwrap();
                assert_eq!(decoded[decoded.len() - 1], json!("999"));
                assert_eq!(decoded[decoded.len() - 2], json!(recipient));
                assert_eq!(decoded[0], json!(if from.is_native() || exact_output { "45" } else { "123" }));
                if !from.is_native() {
                    assert_eq!(decoded[1], json!(if exact_output { "123" } else { "45" }));
                }
                assert_eq!(prepared.data().bytes()[..4], quai_sdk::abi::function_selector(prepared.signature()).unwrap());
            }
        }
    }

    #[test]
    fn exact_output_rounds_up_and_does_not_emulate_an_exact_input_guarantee() {
        for decimals in [6, 8, 18] {
            let unit = U256::from(10).pow(U256::from(decimals));
            let rin = unit * U256::from(1_000_000);
            let rout = unit * U256::from(2_000_000);
            let target = unit * U256::from(123) + U256::from(1);
            let required = amount_in_for_output(target, rin, rout).unwrap();
            assert!(amount_out(required, rin, rout) >= target);
            assert!(amount_out(required - U256::from(1), rin, rout) < target);
        }
        assert!(amount_in_for_output(U256::from(10), U256::from(5), U256::from(10)).is_none());
        assert!(amount_in_for_output(U256::ZERO, U256::from(5), U256::from(10)).is_none());
        assert!(amount_in_for_output(U256::from(1), U256::MAX, U256::from(10)).is_none());
        let reserve = (U256::from(1) << 112) - U256::from(1);
        let required = amount_in_for_output(reserve - U256::from(1), reserve, reserve).unwrap();
        assert!(!required.is_zero());
    }

    #[test]
    fn router_modes_have_distinct_native_and_exact_output_selectors() {
        let token = SwapAsset::Token { address: "0x0000000000000000000000000000000000000011".into(), symbol: "T".into(), decimals: 18 };
        assert_eq!(exact_output_method(&SwapAsset::Quai, &token), "swapETHForExactTokens");
        assert_eq!(exact_output_method(&token, &SwapAsset::Quai), "swapTokensForExactETH");
        assert_eq!(exact_output_method(&token, &token), "swapTokensForExactTokens");
        let abi = interface(ROUTER_ABI).unwrap();
        let signatures: Vec<_> = abi.functions().map(|f| f.signature()).collect();
        for signature in [
            "swapExactETHForTokens(uint256,address[],address,uint256)",
            "swapExactTokensForETH(uint256,uint256,address[],address,uint256)",
            "swapETHForExactTokens(uint256,address[],address,uint256)",
            "swapTokensForExactETH(uint256,uint256,address[],address,uint256)",
        ] {
            assert!(signatures.contains(&signature), "{signature}");
        }
    }

    #[test]
    fn a_remembered_pair_expires_and_a_review_never_uses_one() {
        let pair = "0x0018a110b6ca369dcf5ab062c72f049e93b9ede2";
        assert_eq!(remembered_pair(Trust::Cached, pair, 0), Some(Some(pair)));
        assert_eq!(remembered_pair(Trust::Cached, pair, PAIR_TTL - 1), Some(Some(pair)));
        assert_eq!(remembered_pair(Trust::Cached, pair, PAIR_TTL), None, "a found pair has a policy, not `forever`");
        // An absence is believed only while a new pair is unlikely to have appeared.
        assert_eq!(remembered_pair(Trust::Cached, "", NO_PAIR_TTL - 1), Some(None));
        assert_eq!(remembered_pair(Trust::Cached, "", NO_PAIR_TTL), None);
        // Nothing remembered answers a quote that will be shown in a review.
        for (known, age) in [(pair, 0), (pair, PAIR_TTL - 1), ("", 0)] {
            assert_eq!(remembered_pair(Trust::FirstHand, known, age), None, "first-hand: {known} at {age}s");
        }
    }

    #[test]
    fn uniswap_math() {
        let e18 = |n: u128| U256::from(n * 10u128.pow(18));
        // 1000/1000 pool, 10 in → 9.87… out (fee + impact).
        let out = amount_out(e18(10), e18(1000), e18(1000));
        assert_eq!(out.to_string(), "9871580343970612988");
        assert_eq!(minimum_out(U256::from(10_000u64), 50), U256::from(9950u64));
        assert_eq!(minimum_out(U256::from(10_000u64), 0), U256::from(10_000u64));
        // Impact excludes the LP fee: 1% of the pool ≈ 0.99% impact.
        let impact = impact_bps(e18(10), &[(e18(1000), e18(1000))]);
        assert!((95..=105).contains(&impact), "{impact}");
        // A tiny trade has ~0 impact; a huge one is refused territory.
        assert_eq!(impact_bps(U256::from(10u128.pow(12)), &[(e18(1000), e18(1000))]), 0);
        assert!(impact_bps(e18(2000), &[(e18(1000), e18(1000))]) >= IMPACT_REFUSE_BPS);
        // Two hops compound.
        let two = impact_bps(e18(10), &[(e18(1000), e18(1000)), (e18(1000), e18(1000))]);
        assert!(two > impact);
    }

    #[test]
    fn a_second_swap_is_floored_at_the_first_swaps_minimum() {
        let e18 = |n: u128| U256::from(n * 10u128.pow(18));
        let pool = [(e18(60_000), e18(150_000_000))];
        let first_min = minimum_out(e18(100), 50);
        let floor = chained_minimum(first_min, &pool, 50);
        // Below what the full expected amount would give, and below that less slippage too.
        let at_expected = amount_out(e18(100), pool[0].0, pool[0].1);
        assert!(floor < minimum_out(at_expected, 50), "{floor} vs {at_expected}");
        assert_eq!(floor, minimum_out(amount_out(first_min, pool[0].0, pool[0].1), 50));
        // No slippage and no hops leaves the earlier minimum as it was.
        assert_eq!(chained_minimum(first_min, &[], 0), first_min);
    }

    /// A two-swap quote names its hub and reads as two routes; a single swap has no hub.
    #[test]
    fn two_swap_quotes_name_their_hub() {
        let leg = |venue, path: &[&str], route: &[&str]| SwapLeg {
            venue,
            router: "0x00r".into(),
            path: path.iter().map(|s| s.to_string()).collect(),
            route: route.iter().map(|s| s.to_string()).collect(),
            pools: vec![],
            amount_in: "1".into(),
            amount_out: "1".into(),
            minimum_out: "1".into(),
            output_decimals: 18,
        };
        let mut q = SwapQuote {
            from: SwapAsset::Quai,
            to: SwapAsset::Quai,
            amount_in: "1".into(),
            amount_out: "1".into(),
            minimum_out: "1".into(),
            slippage_bps: 50,
            path: vec![],
            route: vec!["QOGE".into(), "WQUAI".into()],
            pools: vec![],
            impact_bps: 0,
            fee_bps: 30,
            router: String::new(),
            allowance: None,
            approval_needed: false,
            balance: None,
            insufficient: false,
            warnings: vec![],
            observed_at: 0,
            liquidity_at: None,
            legs: vec![leg(Venue::LaunchAmm, &["0x00q", "0x00w"], &["QOGE", "WQUAI"])],
        };
        assert_eq!((q.hub(), q.route_text()), (None, "QOGE → WQUAI".to_string()));
        q.legs.push(leg(Venue::Main, &["0x00w", "0x00u"], &["WQUAI", "USDT"]));
        assert_eq!(q.hub(), Some(("0x00w".to_string(), "WQUAI".to_string())));
        assert_eq!(q.route_text(), "QOGE → WQUAI, then WQUAI → USDT");
    }

    #[test]
    fn lookalikes() {
        let known = vec![("USDT".to_string(), "0x0049f7cbca3556c2dfae62aafa7015f99de1b8f5".to_string())];
        assert!(lookalike_warning("USDT", "0x00ff", &known).is_some());
        assert!(lookalike_warning("U$DT", "0x00ff", &known).is_some() || lookalike_warning("USD7", "0x00ff", &known).is_none());
        assert!(lookalike_warning("usdt", "0x0049F7cbCa3556C2DfaE62Aafa7015F99de1b8f5", &known).is_none());
        assert!(lookalike_warning("WQI", "0x00ff", &known).is_none());
    }

    #[test]
    fn pool_liquidity_applies() {
        use crate::explorer::{PoolBoard, PoolStats};
        let hop = |pair: &str| PoolHop { pair: pair.into(), reserve_in: "1".into(), reserve_out: "1".into(), tvl_usd: None };
        let mut q = SwapQuote {
            from: SwapAsset::Quai,
            to: SwapAsset::Quai,
            amount_in: "1".into(),
            amount_out: "1".into(),
            minimum_out: "1".into(),
            slippage_bps: 50,
            path: vec![],
            route: vec![],
            pools: vec![hop("0x00aa"), hop("0x00bb"), hop("0x00cc")],
            impact_bps: 0,
            fee_bps: 30,
            router: String::new(),
            allowance: None,
            approval_needed: false,
            balance: None,
            insufficient: false,
            warnings: vec![],
            observed_at: 0,
            liquidity_at: None,
            legs: vec![],
        };
        assert!(q.liquidity_text().is_none());
        let board = PoolBoard {
            pools: vec![
                PoolStats { address: "0x00AA".into(), name: "A/B".into(), tvl_usd: Some(11_285.0), volume_24h_usd: None },
                PoolStats { address: "0x00bb".into(), name: "B/C".into(), tvl_usd: Some(420.0), volume_24h_usd: None },
            ],
            observed_at: 0,
            stale: false,
        };
        apply_liquidity(&mut q, &board);
        assert_eq!(q.pools[0].tvl_usd, Some(11_285.0));
        assert_eq!(q.liquidity_text().unwrap(), "$11.3k TVL · $420.00 TVL · not indexed");
        assert_eq!(q.warnings.len(), 1, "only the thin pool warns: {:?}", q.warnings);
        assert!(q.warnings[0].contains("$420.00"));
        assert_eq!(usd_compact(2_300_000.0), "$2.3M");
        assert_eq!(usd_compact(3_261.58), "$3.26k");
    }

    #[test]
    fn router_abi_parses() {
        interface(ROUTER_ABI).unwrap();
        interface(FACTORY_ABI).unwrap();
        interface(PAIR_ABI).unwrap();
    }
}

#[cfg(test)]
mod trading_protection_regressions {
    use super::*;

    #[test]
    fn extreme_and_dust_values_cannot_silently_remove_protection() {
        assert_eq!(minimum_out(U256::MAX, 0), U256::MAX);
        assert_eq!(minimum_out(U256::from(1), 50), U256::ZERO);
        assert!(require_minimum(minimum_out(U256::from(1), 50)).is_err());
        assert_eq!(amount_out(U256::MAX, U256::from(1000), U256::from(1000)), U256::ZERO);
        assert!(validate_slippage(5000).is_ok());
        assert!(validate_slippage(5001).is_err());
        assert!(validate_deadline(0).is_err());
        assert!(validate_deadline(1441).is_err());
        assert!(validate_deadline(1440).is_ok());
    }

    #[test]
    fn canonical_contracts_cannot_be_impersonated_by_imported_spellings() {
        let known = vec![("USDT".into(), "0x00canonical".into())];
        assert!(lookalike_warning("UЅDT", "0x00fake", &known).is_some());
        assert!(lookalike_warning("USDT", "0x00canonical", &known).is_none());
        assert!(lookalike_warning("UЅDT", "0x00canonical", &known).is_none());
        assert!(lookalike_warning("unfamiliar非", "0x00other", &known).is_some());
    }
}
