//! The HartiiLabs launchpad: tokens on their bonding curve, and the ones that have bonded.
//!
//! A second launchpad in the shape of Quainance's own launch zone, and read the same way — except
//! that it has no subgraph, so every figure here comes from the chain. The launcher enumerates its
//! tokens and names each one's curve; the curves are EIP-1167 clones of one pinned implementation,
//! which is what makes reading them by a fixed ABI safe.
//!
//! **Bonding is not migration here.** Graduation zeroes the curve's raise and seeds a pool *inside
//! the curve* (`poolQuaiReserve`/`poolTokenReserve`) with no LP token, so that liquidity is locked
//! for good. Buys and sells keep going to the curve, x*y=k against that pool, with the same fee.
//! So a bonded token is a live market, and its curve stays the canonical one even when someone
//! seeds a separate pair for it elsewhere.

use crate::data::DataCtx;
use crate::error::{CoreError, Result};
use crate::markets::{CurveMark, Pool, PoolToken, Venue};
use quai_sdk::U256;
use serde::{Deserialize, Serialize};

/// Reads the launcher needs. The curve half is answered by the clones.
pub const LAUNCHER_ABI: &[&str] = &[
    "function tokenCount() view returns (uint256)",
    "function tokens(uint256 index) view returns (address)",
    "function curveOf(address token) view returns (address)",
    "function curveImplementation() view returns (address)",
    "function tokenImplementation() view returns (address)",
    "function treasury() view returns (address)",
];

/// The bonding curve's own interface, read from the pinned implementation's selectors.
pub const CURVE_ABI: &[&str] = &[
    "function token() view returns (address)",
    "function factory() view returns (address)",
    "function graduated() view returns (bool)",
    "function creator() view returns (address)",
    "function feeBps() view returns (uint256)",
    "function tokensSold() view returns (uint256)",
    "function curveSupply() view returns (uint256)",
    "function realQuaiReserve() view returns (uint256)",
    "function virtualQuaiReserve() view returns (uint256)",
    "function virtualTokenReserve() view returns (uint256)",
    "function poolQuaiReserve() view returns (uint256)",
    "function poolTokenReserve() view returns (uint256)",
    "function quoteBuy(uint256 quaiIn) view returns (uint256 tokensOut)",
    "function quoteSell(uint256 tokensIn) view returns (uint256 quaiOut)",
    "function buy(uint256 minTokensOut) payable returns (uint256 tokensOut)",
    "function sell(uint256 tokensIn, uint256 minQuaiOut) returns (uint256 quaiOut)",
];

/// A Hartii curve is a constant-product market over *virtual* reserves: there is no graduation
/// target field and no `cumulativeTokensSold` sampler, which is why a Quainance-shaped read of one
/// returns nothing to draw. Its whole curve is nonetheless determined by its reserves, so it can be
/// drawn from its own mechanics instead — provided the model is confirmed against the contract.
///
/// Tokens received for `quai_in` at the given current reserves. `None` on overflow or a reserve
/// shape that cannot fill the trade.
pub fn constant_product_out(quai_reserve: U256, token_reserve: U256, quai_in: U256) -> Option<U256> {
    let k = quai_reserve.checked_mul(token_reserve)?;
    let next = quai_reserve.checked_add(quai_in)?;
    if next.is_zero() {
        return None;
    }
    token_reserve.checked_sub(k / next)
}

/// Whether `(quai_reserve, token_reserve)` reproduces the contract's own `quoteBuy` answer.
///
/// The two plausible readings of `virtualQuaiReserve`/`virtualTokenReserve` — already folded with
/// the real reserve and tokens sold, or pure launch offsets to be combined — cannot be told apart
/// from their names, and guessing wrong would misdraw a real token's curve. So neither is assumed:
/// each is checked against an answer only the contract can give, and a curve whose model is not
/// confirmed is left undrawn. One atom of slack absorbs the contract's own integer rounding.
pub fn reserves_reproduce_quote(quai_reserve: U256, token_reserve: U256, quai_in: U256, quoted_out: U256) -> bool {
    match constant_product_out(quai_reserve, token_reserve, quai_in) {
        Some(out) => out.abs_diff(quoted_out) <= U256::from(1u64),
        None => false,
    }
}

/// The reserves a curve is trading against right now, confirmed by its own quote.
///
/// A curve still selling trades x*y=k over its virtual reserves; a bonded one over the pool its
/// graduation seeded (`poolQuaiReserve`/`poolTokenReserve`), with the virtual fields left at their
/// launch values. Each state has its own candidates, and a reading is used only once it reproduces
/// the `quote` the curve gave for `net`, so a misread field prices nothing rather than wrongly.
#[allow(clippy::too_many_arguments)]
pub fn live_reserves(
    bonded: bool,
    virtual_quai: U256,
    virtual_token: U256,
    raised: U256,
    sold: U256,
    pool_quai: U256,
    pool_token: U256,
    net: U256,
    quoted: U256,
) -> Option<(U256, U256)> {
    let candidates: Vec<(U256, U256)> = if bonded {
        vec![(pool_quai, pool_token)]
    } else {
        vec![(virtual_quai.saturating_add(raised), virtual_token.saturating_sub(sold)), (virtual_quai, virtual_token)]
    };
    candidates.into_iter().filter(|(q, t)| !q.is_zero() && !t.is_zero()).find(|(q, t)| reserves_reproduce_quote(*q, *t, net, quoted))
}

/// QUAI per whole token at the margin: the reserve ratio, before the fee and any price impact.
pub fn reserve_spot(quai_reserve: U256, token_reserve: U256, decimals: u8) -> Option<f64> {
    let t = crate::amount::to_f64(token_reserve, decimals);
    Some(crate::amount::to_f64(quai_reserve, crate::amount::QUAI_DECIMALS) / t).filter(|p| p.is_finite() && *p > 0.0)
}

/// The launch-state reserves behind current ones, from which the entire curve follows.
pub fn launch_reserves(quai_reserve: U256, token_reserve: U256, raised: U256, sold: U256) -> Option<(U256, U256)> {
    Some((quai_reserve.checked_sub(raised)?, token_reserve.checked_add(sold)?))
}

/// QUAI raised once `supply` tokens have been sold off the curve — Hartii's analogue of a
/// graduation target, computed rather than read, because the contract exposes no such field.
pub fn raise_at_sellout(launch_quai: U256, launch_token: U256, supply: U256) -> Option<U256> {
    let k = launch_quai.checked_mul(launch_token)?;
    let remaining = launch_token.checked_sub(supply)?;
    if remaining.is_zero() {
        return None;
    }
    (k / remaining).checked_sub(launch_quai)
}

/// Cumulative tokens sold at each evenly spaced amount raised, launch to sell-out: the same
/// samples `cumulativeTokensSold` gives for a Quainance curve, so both families draw alike.
pub fn sellout_samples(launch_quai: U256, launch_token: U256, target: U256, points: usize, decimals: u8) -> Option<Vec<f64>> {
    let k = launch_quai.checked_mul(launch_token)?;
    let n = U256::from(u64::try_from(points).ok()?.max(1));
    (0..=points)
        .map(|i| {
            let raised = target * U256::from(u64::try_from(i).ok()?) / n;
            let reserve = launch_quai.checked_add(raised)?;
            if reserve.is_zero() {
                return None;
            }
            let sold = launch_token.checked_sub(k / reserve)?;
            Some(crate::amount::to_f64(sold, decimals))
        })
        .collect()
}

/// Progress toward graduation, on the same basis as the target it is shown beside.
///
/// A bonding curve is convex, so the share of tokens sold and the share of the raise reached are
/// different numbers: 18% of QHUB's tokens were gone by 5% of its raise. Both are true, but the
/// one displayed next to `raised / target` has to be the quote share or it reads as an arithmetic
/// error, and it would duplicate the token share already shown on its own line. Without a
/// confirmed target there is no quote basis, so the token share is all there is.
pub fn progress_bps(raised: U256, target: U256, sold: U256, supply: U256) -> u64 {
    if target.is_zero() { crate::liquidity::share_bps(sold, supply) } else { crate::liquidity::share_bps(raised, target) }
}

/// How many of the launcher's tokens one pass reads. It held 27 on 2026-09-21; the ceiling is here
/// so a launcher that grows without bound cannot turn one refresh into thousands of calls.
pub const MAX_TOKENS: u64 = 250;

/// One token on the HartiiLabs launchpad.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct HartiiLaunch {
    /// Token contract, lowercase.
    pub token: String,
    /// Its bonding curve, lowercase.
    pub curve: String,
    pub symbol: String,
    pub decimals: u8,
    /// Sold out its curve and had its QUAI swept. Not a promise that a market exists.
    pub bonded: bool,
    /// QUAI still held by the curve. Zero once it has bonded.
    pub raised_quai: f64,
    /// Share of the curve's sale supply taken, in basis points.
    pub progress_bps: Option<u64>,
    /// QUAI per whole token: the reserve spot once the reserves reproduce the curve's own quote,
    /// else the inverse of quoteBuy(1 QUAI). `price_basis` says which.
    pub price_quai: Option<f64>,
    #[serde(default)]
    pub price_basis: crate::markets::PriceBasis,
    /// A bonded curve's pool QUAI reserve: locked depth, since graduation mints no LP token.
    #[serde(default)]
    pub locked_quai: Option<f64>,
}

impl HartiiLaunch {
    /// Where it can be traded, in a sentence. A bonded token keeps trading on its curve, against
    /// the locked pool its graduation seeded, which is where Quainance's own app trades it too.
    pub fn market_note(&self) -> &'static str {
        if self.bonded { "bonded; bought and sold on its curve's locked pool" } else { "on its bonding curve" }
    }
}

/// Hartii applies floor(amount * feeBps / 10_000) before buying and after gross sell quotes.
/// Qualified by local execution of the pinned implementation, including strict-minimum reverts.
pub fn after_fee(amount: U256, fee_bps: u16) -> Result<(U256, U256)> {
    if fee_bps > 10_000 {
        return Err(CoreError::Invalid("Hartii curve fee exceeds 100%".into()));
    }
    let fee = crate::amount::mul_div(amount, U256::from(fee_bps), U256::from(10_000u64))
        .ok_or_else(|| CoreError::Invalid("Hartii fee arithmetic overflow".into()))?;
    Ok((amount - fee, fee))
}

fn to_f64(v: U256, decimals: u8) -> f64 {
    let s = crate::amount::format_amount(v, decimals);
    s.parse().unwrap_or(0.0)
}

/// Every token the launcher has created, with its curve's state.
///
/// One Multicall batch per level: the token list, then each token's curve, then each curve's state
/// and each token's symbol. Without Multicall this would be hundreds of calls, so it is required
/// rather than emulated — the launchpad is a directory, not a balance anyone is waiting on.
pub async fn launches(ctx: &DataCtx) -> Result<Vec<HartiiLaunch>> {
    // Cached with the other on-chain directories, and for the same reason: this is four sequential
    // multicall round-trips, the last of them seven calls per token, and the market directory that
    // calls it refreshes every few seconds. Uncached it re-read the whole launchpad on every tick.
    launches_observed(ctx).await.map(|c| c.value)
}

/// Preserve observation age and failed-refresh state for directory/alert consumers.
pub async fn launches_observed(ctx: &DataCtx) -> Result<crate::data::Cached<Vec<HartiiLaunch>>> {
    let pin =
        ctx.network.ecosystem.hartii_launcher.as_ref().ok_or_else(|| CoreError::NotFound("no Hartii launcher on this network".into()))?;
    let key = format!("hartii_launches:{}:{}", pin.address.to_lowercase(), pin.code_hash.as_deref().unwrap_or("configured"));
    ctx.cached(&key, crate::markets::FACTORY_TTL, || read_launches(ctx)).await
}

async fn read_launches(ctx: &DataCtx) -> Result<Vec<HartiiLaunch>> {
    let pin = ctx
        .network
        .ecosystem
        .hartii_launcher
        .clone()
        .ok_or_else(|| CoreError::NotFound(format!("no HartiiLabs launchpad on {}", ctx.network.name)))?;
    // Two independent pin checks, five sequential reads each: side by side, not one after another.
    let (launcher, mc) = tokio::join!(
        crate::data::verify_pinned(&ctx.app, &ctx.node, &ctx.network, &pin, "HartiiLabs launcher", ctx.trust),
        crate::multicall::Multicall::open(ctx),
    );
    let launcher = launcher?.to_string().to_lowercase();
    let Some(mc) = mc else {
        return Err(CoreError::NotFound("the HartiiLabs launchpad needs Multicall3 to be read".into()));
    };
    use crate::multicall::{Arg, Call, address_word, word};

    let count = mc
        .try_all(&[Call::view(&launcher, "tokenCount()", &[])])
        .await?
        .first()
        .and_then(Option::as_ref)
        .map(|d| word(d, 0))
        .unwrap_or(U256::ZERO);
    let count = u64::try_from(count).unwrap_or(0).min(MAX_TOKENS);
    if count == 0 {
        return Ok(Vec::new());
    }
    // Newest last is how the launcher stores them, and newest first is what a directory wants.
    let calls: Vec<Call> = (0..count).map(|i| Call::view(&launcher, "tokens(uint256)", &[Arg::Uint(U256::from(i))])).collect();
    let tokens: Vec<String> =
        mc.try_all(&calls).await?.into_iter().filter_map(|d| d.map(|d| address_word(&d, 0))).filter(|a| !a.is_empty()).collect();

    let calls: Vec<Call> = tokens.iter().map(|t| Call::view(&launcher, "curveOf(address)", &[Arg::Addr(t.clone())])).collect();
    let curves: Vec<String> = mc.try_all(&calls).await?.into_iter().map(|d| d.map(|d| address_word(&d, 0)).unwrap_or_default()).collect();

    // Per token: symbol and decimals; per curve: state plus fee for the subsequent net-input quote,
    // and both reserve pairs, one of which the quote then confirms. Eleven reads per token; `base`
    // below strides by the same number.
    const PER_TOKEN: usize = 11;
    let mut calls = Vec::with_capacity(tokens.len() * PER_TOKEN);
    for (token, curve) in tokens.iter().zip(&curves) {
        calls.push(Call::view(token, "symbol()", &[]));
        calls.push(Call::view(token, "decimals()", &[]));
        calls.push(Call::view(curve, "graduated()", &[]));
        calls.push(Call::view(curve, "tokensSold()", &[]));
        calls.push(Call::view(curve, "curveSupply()", &[]));
        calls.push(Call::view(curve, "realQuaiReserve()", &[]));
        // The quote functions exclude this fee; buy deducts it before calling quoteBuy.
        calls.push(Call::view(curve, "feeBps()", &[]));
        calls.push(Call::view(curve, "virtualQuaiReserve()", &[]));
        calls.push(Call::view(curve, "virtualTokenReserve()", &[]));
        calls.push(Call::view(curve, "poolQuaiReserve()", &[]));
        calls.push(Call::view(curve, "poolTokenReserve()", &[]));
    }
    let out = mc.try_all(&calls).await?;
    let at = |i: usize| out.get(i).and_then(Option::as_ref);
    // The pinned runtime's quoteBuy accepts NET input: buy deducts its fee before quoting.
    // Query the exact net amount for a one-QUAI user payment, keeping the fee observation paired.
    let mut quote_calls = Vec::new();
    let mut quote_indices = vec![None; tokens.len()];
    let mut nets = vec![U256::ZERO; tokens.len()];
    for (n, curve) in curves.iter().enumerate() {
        if let Some(fee) = at(n * PER_TOKEN + 6).filter(|d| d.len() >= 32).and_then(|d| u16::try_from(word(d, 0)).ok())
            && let Ok((net, _)) = after_fee(U256::from(10u64).pow(U256::from(18u64)), fee)
            && !curve.is_empty()
        {
            quote_indices[n] = Some(quote_calls.len());
            nets[n] = net;
            quote_calls.push(Call::view(curve, "quoteBuy(uint256)", &[Arg::Uint(net)]));
        }
    }
    let quotes = mc.try_all(&quote_calls).await?;

    let mut rows = Vec::new();
    for (n, (token, curve)) in tokens.iter().zip(&curves).enumerate() {
        if curve.is_empty() {
            continue;
        }
        let base = n * PER_TOKEN;
        let symbol = at(base).and_then(|d| crate::markets::solidity_string(d)).unwrap_or_default();
        let decimals = at(base + 1).map(|d| word(d, 0)).and_then(|v| u8::try_from(v).ok()).unwrap_or(18);
        let bonded = at(base + 2).is_some_and(|d| !word(d, 0).is_zero());
        let sold = at(base + 3).map(|d| word(d, 0)).unwrap_or(U256::ZERO);
        let supply = at(base + 4).map(|d| word(d, 0)).unwrap_or(U256::ZERO);
        let real_quai = at(base + 5).map(|d| word(d, 0)).unwrap_or(U256::ZERO);
        let read = |i: usize| at(base + i).filter(|d| d.len() >= 32).map(|d| word(d, 0)).unwrap_or(U256::ZERO);
        let (virtual_quai, virtual_token, pool_quai, pool_token) = (read(7), read(8), read(9), read(10));
        let per_quai = quote_indices[n]
            .and_then(|i| quotes.get(i))
            .and_then(Option::as_ref)
            .filter(|d| d.len() >= 32)
            .map(|d| word(d, 0))
            .unwrap_or(U256::ZERO);
        let quotable_now = !per_quai.is_zero() && (bonded || per_quai < supply.saturating_sub(sold));
        let reserves_now = quotable_now
            .then(|| live_reserves(bonded, virtual_quai, virtual_token, real_quai, sold, pool_quai, pool_token, nets[n], per_quai))
            .flatten();
        // How far toward graduation, as the curve card and Quainance's own launches say it: the QUAI
        // raised against what the curve raises by selling out. Counting tokens sold instead put
        // ART at 87% in the list beside "64.5% to graduation" on its card. A bonded curve reads
        // 100%; a curve whose reserves do not reproduce its quote falls back to tokens sold.
        let progress_bps = (!supply.is_zero()).then(|| {
            if bonded {
                return 10_000;
            }
            let target = reserves_now
                .and_then(|(q, t)| launch_reserves(q, t, real_quai, sold))
                .and_then(|(q0, t0)| raise_at_sellout(q0, t0, supply))
                .unwrap_or(U256::ZERO);
            progress_bps(real_quai, target, sold, supply).min(10_000)
        });
        // The price is the reserve ratio the curve trades against, as HartiiLabs' own pricing
        // defines it: (virtualQuai + raised) / (virtualToken - sold) while selling, and
        // poolQuaiReserve / poolTokenReserve once bonded. The virtual fields alone are launch
        // parameters, identical on every token, which is why reading them priced HRT and QAXE alike.
        // So the reserves are used only when they reproduce the curve's own one-QUAI quote; when
        // they do not, the inverted quote stands in, fee and price impact included, and says so.
        let quotable = !per_quai.is_zero() && (bonded || per_quai < supply.saturating_sub(sold));
        let live = quotable
            .then(|| live_reserves(bonded, virtual_quai, virtual_token, real_quai, sold, pool_quai, pool_token, nets[n], per_quai))
            .flatten();
        let (price_quai, price_basis) = match live.and_then(|(q, t)| reserve_spot(q, t, decimals)) {
            Some(spot) => (Some(spot), crate::markets::PriceBasis::ReserveSpot),
            None => (
                quotable.then(|| 1.0 / to_f64(per_quai, decimals)).filter(|p| p.is_finite() && *p > 0.0),
                crate::markets::PriceBasis::OneQuaiBuyQuote,
            ),
        };
        let locked_quai = (bonded && live.is_some()).then(|| to_f64(pool_quai, 18));
        rows.push(HartiiLaunch {
            token: token.clone(),
            curve: curve.clone(),
            symbol,
            decimals,
            bonded,
            raised_quai: to_f64(real_quai, 18),
            progress_bps,
            price_quai,
            price_basis,
            locked_quai,
        });
    }
    rows.reverse();
    Ok(rows)
}

/// How long HartiiLabs' token directory is believed: its own `max-age`.
pub const CHANGES_TTL: u64 = 60;

/// Each launchpad token's 24h price change in percent, from HartiiLabs' public read API, keyed by
/// (token, curve), both lowercase.
///
/// The chain has no day-old price to compare with, and reading one would mean a day of logs per
/// curve. HartiiLabs indexes its own trades and prices them on the same reserve basis the wallet
/// now uses (its `lastPriceWei` for QAXE was the wallet's reserve spot to the wei on 2026-09-23),
/// so its change is comparable. It is display data only: it moves no price and sizes no trade.
pub async fn changes_24h(ctx: &DataCtx) -> Result<std::collections::HashMap<(String, String), f64>> {
    if !ctx.policy.market {
        return Err(CoreError::NotFound("market data is off".into()));
    }
    let base = ctx.network.ecosystem.hartii_api.clone().ok_or_else(|| CoreError::NotFound("no HartiiLabs API on this network".into()))?;
    let url = format!("{}/api/tokens", base.trim_end_matches('/'));
    let listed = ctx
        .cached(&format!("hartii_changes:{}", base.to_lowercase()), CHANGES_TTL, || async move {
            let body = crate::http::get_json(&url).await?;
            Ok(parse_changes(&body))
        })
        .await?;
    Ok(listed.value.into_iter().map(|(token, curve, change)| ((token, curve), change)).collect())
}

/// `(token, curve, change %)` for each item that names both contracts and a plausible change.
fn parse_changes(body: &serde_json::Value) -> Vec<(String, String, f64)> {
    let items = body["items"].as_array().map(Vec::as_slice).unwrap_or_default();
    items
        .iter()
        .filter_map(|item| {
            let token = item["address"].as_str().filter(|a| crate::chain::addr(a).is_ok())?.to_lowercase();
            let curve = item["curveAddress"].as_str().filter(|a| crate::chain::addr(a).is_ok())?.to_lowercase();
            let change = item["change24h"].as_f64().or_else(|| item["priceChange24h"].as_f64())?;
            // A price cannot fall more than 100%, and a figure that is not a number is no figure.
            // Nor is a rise of more than a million percent in a day, which is a broken feed.
            (change.is_finite() && change > -100.0 && change < 1e6).then_some((token, curve, change))
        })
        .collect()
}

/// Give each HartiiLabs curve row the price it had a day ago, from its 24h change, so the list's
/// 24h column fills in for it as it does for a pool. Bound to both the token and the curve.
pub fn apply_changes(pools: &mut [Pool], changes: &std::collections::HashMap<(String, String), f64>) {
    for p in pools.iter_mut().filter(|p| p.venue == Venue::Curve) {
        let Some(price) = p.spot_price() else { continue };
        if p.curve.as_ref().and_then(|c| c.launchpad.as_deref()) != Some("HartiiLabs") {
            continue;
        }
        if let Some(change) = changes.get(&(p.token0.address.to_lowercase(), p.address.to_lowercase())) {
            p.spot_24h_ago = Some(price / (1.0 + change / 100.0)).filter(|v| v.is_finite() && *v > 0.0);
        }
    }
}

/// The launchpad's **bonded** tokens as market rows.
///
/// Bonding here sells out the curve's supply but does not retire it: the curve still quotes and
/// still takes both sides, against a locked pool of its own. So a bonded token is a real market
/// whose canonical venue is its curve, and it belongs in the market list.
///
/// A token still raising is not. It has no depth but the curve and no other side, and the Launches
/// screen is built to show exactly that — progress, what it has raised, and what it needs. Listing
/// it as a market too would put the same thing in two places and call one of them a price.
pub fn curve_pools(rows: &[HartiiLaunch], wquai: &str) -> Vec<Pool> {
    rows.iter()
        .filter(|l| l.bonded && l.price_quai.is_some())
        .map(|l| Pool {
            address: l.curve.clone(),
            token0: PoolToken { address: l.token.clone(), symbol: l.symbol.clone(), decimals: l.decimals },
            token1: PoolToken { address: wquai.to_lowercase(), symbol: "WQUAI".into(), decimals: 18 },
            venue: Venue::Curve,
            curve: Some(CurveMark {
                price_quai: l.price_quai,
                price_basis: l.price_basis,
                locked_quai: l.locked_quai,
                raised_quai: l.raised_quai,
                target_quai: None,
                progress_bps: l.progress_bps,
                launchpad: Some("HartiiLabs".into()),
                venue_kind: Some(crate::capabilities::Family::HartiiCurve),
            }),
            ..Pool::default()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(symbol: &str, bonded: bool, progress: Option<u64>) -> HartiiLaunch {
        HartiiLaunch {
            token: "0x00aa".into(),
            curve: "0x00bb".into(),
            symbol: symbol.into(),
            decimals: 18,
            bonded,
            raised_quai: if bonded { 0.0 } else { 2.5 },
            progress_bps: progress,
            price_quai: Some(0.000015),
            ..HartiiLaunch::default()
        }
    }

    /// A bonded token is still a market. Bonding sells out the curve's supply but does not retire
    /// the curve: HRT and QAXE both quote and take both sides on chain, and Quainance's own trade
    /// index records their market as a CURVE at the curve address with hundreds of executions.
    /// Excluding them hid two live markets.
    #[test]
    fn only_a_bonded_curve_is_a_market() {
        let rows = vec![row("HRT", true, Some(10_000)), row("PEPE", false, Some(21))];
        let pools = curve_pools(&rows, "0x006c3e2a");
        assert_eq!(pools.len(), 1, "the one still raising belongs on Launches, not in the market list");
        assert_eq!(pools[0].token0.symbol, "HRT", "and the bonded one is a market: it still quotes, on its own locked pool");
        assert_eq!(pools[0].venue, Venue::Curve);
        assert_eq!(pools[0].curve.as_ref().unwrap().launchpad.as_deref(), Some("HartiiLabs"), "whose launchpad it is, is part of the row");
        // A bonded curve that cannot quote is not a market either.
        let mut mute = row("DEAD", true, Some(10_000));
        mute.price_quai = None;
        assert!(curve_pools(&[mute], "0x006c3e2a").is_empty());
    }

    /// QAXE on chain 2026-09-23: bonded, its virtual fields still at the launch parameters, and its
    /// pool at 190,560.68 QUAI against 47,434,019.08 tokens. quoteBuy(0.99 QUAI) answered
    /// 246.427728351405685537 tokens, which that pool reproduces to the wei and the virtual fields
    /// do not. Its price is the pool ratio, 0.0040174, not the 0.00406 the inverted quote gave.
    #[test]
    fn a_bonded_curve_is_priced_from_the_pool_its_quote_confirms() {
        let n = |s: &str| U256::from_str_radix(s, 10).unwrap();
        let (pool_quai, pool_token) = (n("190560677731240720000000"), n("47434019080671735000000000"));
        let net = n("990000000000000000");
        let quoted = constant_product_out(pool_quai, pool_token, net).unwrap();
        let (vq, vt) = (n("17000000000000000000000"), n("1073000000000000000000000000"));
        let sold = n("784000000000000000000000000");
        let live = live_reserves(true, vq, vt, U256::ZERO, sold, pool_quai, pool_token, net, quoted);
        assert_eq!(live, Some((pool_quai, pool_token)));
        let spot = reserve_spot(pool_quai, pool_token, 18).unwrap();
        assert!((spot - 0.0040174).abs() < 1e-7, "{spot}");
        assert!(spot < 1.0 / crate::amount::to_f64(quoted, 18), "the spot is below the fee-inclusive quote");
        // A quote the pool does not reproduce is not priced from the pool.
        assert_eq!(live_reserves(true, vq, vt, U256::ZERO, sold, pool_quai, pool_token, net, quoted / U256::from(2u64)), None);
        // An unseeded pool (zero reserves) is never a reading.
        assert_eq!(live_reserves(true, vq, vt, U256::ZERO, sold, U256::ZERO, U256::ZERO, net, U256::ZERO), None);
    }

    /// A curve still selling trades over (virtualQuai + raised, virtualToken - sold), the reading
    /// HartiiLabs' pricing page gives, and its pool fields are ignored even if they are nonzero.
    #[test]
    fn a_selling_curve_is_priced_from_its_virtual_reserves() {
        let (q0, t0) = launch();
        let raised = crate::amount::parse_quai("5").unwrap();
        let sold = constant_product_out(q0, t0, raised).unwrap();
        let (q, t) = (q0 + raised, t0 - sold);
        let net = crate::amount::parse_quai("0.99").unwrap();
        let quoted = constant_product_out(q, t, net).unwrap();
        assert_eq!(live_reserves(false, q0, t0, raised, sold, U256::from(7u64), U256::from(7u64), net, quoted), Some((q, t)));
        assert!((reserve_spot(q, t, 18).unwrap() - crate::amount::to_f64(q, 18) / crate::amount::to_f64(t, 18)).abs() < 1e-18);
    }

    /// HartiiLabs' directory as it answered on 2026-09-23, trimmed: a row's change lands on its own
    /// curve and nowhere else, and a figure no price could have is dropped.
    #[test]
    fn a_curve_takes_its_24h_change_from_hartii_by_token_and_curve() {
        let body = serde_json::json!({"items": [
            {"address": "0x0035187a7660f595d93cd53a4d16c635d6cffc8f", "curveAddress": "0x004bc407903a51506bcf0b1ab423958c5991c237", "change24h": 47.61},
            {"address": "0x00aa000000000000000000000000000000000001", "curveAddress": "0x00bb000000000000000000000000000000000001", "change24h": -100.0},
            {"address": "not an address", "curveAddress": "0x00bb000000000000000000000000000000000002", "change24h": 3.0},
            {"address": "0x00aa000000000000000000000000000000000003", "curveAddress": "0x00bb000000000000000000000000000000000003", "change24h": null},
            {"address": "0x00aa000000000000000000000000000000000004", "curveAddress": "0x00bb000000000000000000000000000000000004", "change24h": 1e308}
        ]});
        let rows = parse_changes(&body);
        assert_eq!(rows.len(), 1, "{rows:?}");
        let changes: std::collections::HashMap<(String, String), f64> = rows.into_iter().map(|(t, c, x)| ((t, c), x)).collect();
        let mut qaxe = row("QAXE", true, Some(10_000));
        qaxe.token = "0x0035187a7660f595d93cd53a4d16c635d6cffc8f".into();
        qaxe.curve = "0x004bc407903a51506bcf0b1ab423958c5991c237".into();
        qaxe.price_quai = Some(0.0040174);
        let mut other = qaxe.clone();
        other.curve = "0x00cc000000000000000000000000000000000009".into();
        let mut pools = curve_pools(&[qaxe, other], "0x006c3e2a");
        apply_changes(&mut pools, &changes);
        let change = pools[0].change_24h().unwrap();
        assert!((change - 47.61).abs() < 1e-9, "{change}");
        assert_eq!(pools[1].change_24h(), None, "the same token on another curve is not the same market");
    }

    /// The price is the curve's own quote inverted, not a ratio of its reserves.
    ///
    /// Reserves cannot do this job. `virtualQuaiReserve`/`virtualTokenReserve` are parameters,
    /// identical on all 27 tokens at 17,000 QUAI against 1.073b, and every bonded curve reports the
    /// same 784m sold with nothing left in it — yet on 2026-09-21 HRT quoted 15,838 tokens per QUAI
    /// and QAXE quoted 260.81, a 61x difference no reserve ratio can express. `quoteBuy` is right
    /// in both states and includes the fee.
    #[test]
    fn the_price_is_the_curves_own_quote() {
        let price = |tokens_per_quai: f64| 1.0 / tokens_per_quai;
        assert!((price(15838.1049) - 6.314e-5).abs() < 1e-8, "HRT");
        assert!((price(260.81) - 3.8342e-3).abs() < 1e-6, "QAXE");
        assert!((price(15206.4318) - 6.576e-5).abs() < 1e-8, "KISHORE, still selling");
        // Two curves with identical reserves priced 61x apart, which is the whole point.
        assert!(price(260.81) / price(15838.1049) > 60.0);
    }

    /// The two states a row can be in read differently, because they mean different things.
    #[test]
    fn a_bonded_token_says_where_it_trades() {
        assert!(row("HRT", true, Some(10_000)).market_note().contains("locked pool"));
        assert_eq!(row("PEPE", false, Some(21)).market_note(), "on its bonding curve");
    }

    /// Launch offsets chosen so the arithmetic is checkable by hand: k = 30 * 1,073,000,000.
    fn launch() -> (U256, U256) {
        (crate::amount::parse_quai("30").unwrap(), U256::from(1_073_000_000u64) * U256::from(10u64).pow(U256::from(18u64)))
    }

    /// The model is decided by the contract's own answer, never by the field names: whichever
    /// reserve reading reproduces `quoteBuy` is the live one, and neither matching draws nothing.
    #[test]
    fn only_the_reserve_reading_that_reproduces_the_quote_is_used() {
        let (q0, t0) = launch();
        let quai_in = crate::amount::parse_quai("1").unwrap();
        let out = constant_product_out(q0, t0, quai_in).unwrap();
        assert!(reserves_reproduce_quote(q0, t0, quai_in, out), "the reserves that produced it must confirm it");
        assert!(reserves_reproduce_quote(q0, t0, quai_in, out + U256::from(1u64)), "one atom of rounding is tolerated");
        assert!(!reserves_reproduce_quote(q0, t0, quai_in, out + U256::from(1_000u64)), "a different curve is not");
        let (shifted_q, shifted_t) = (q0 * U256::from(2u64), t0);
        assert!(!reserves_reproduce_quote(shifted_q, shifted_t, quai_in, out), "the wrong reading is rejected");
    }

    /// Hartii has no graduation-target field, so the raise at sell-out is computed from the curve
    /// and the resulting chart has the same shape a Quainance curve draws: rising, left to right.
    #[test]
    fn a_hartii_curve_draws_a_rising_chart_from_its_own_reserves() {
        let (q0, t0) = launch();
        let supply = U256::from(800_000_000u64) * U256::from(10u64).pow(U256::from(18u64));
        let target = raise_at_sellout(q0, t0, supply).expect("a curve that sells out has a raise");
        assert!(target > q0, "selling 800M of 1.073B tokens costs more than the offset: {target}");
        // Selling `supply` at that raise is the invariant the target was derived from. Both steps
        // floor, so the raise lands just under the exact figure and the round trip sells slightly
        // less than the whole supply — the safe direction, and never more than it.
        let sold = constant_product_out(q0, t0, target).unwrap();
        assert!(sold <= supply, "a floored raise never oversells the curve: {sold} vs {supply}");
        assert!(supply - sold < supply / U256::from(1_000_000_000_000u64), "and lands within rounding: {sold} vs {supply}");
        let samples = sellout_samples(q0, t0, target, crate::curve::CURVE_POINTS, 18).unwrap();
        assert_eq!(samples.len(), crate::curve::CURVE_POINTS + 1);
        assert_eq!(samples[0], 0.0, "nothing is sold before anything is raised");
        assert!(samples.windows(2).all(|w| w[1] >= w[0]), "cumulative sales never go backwards");
        let points = crate::curve::price_points(crate::amount::to_f64(target, crate::amount::QUAI_DECIMALS), &samples);
        assert_eq!(points.len(), crate::curve::CURVE_POINTS, "every step prices");
        assert!(points.windows(2).all(|w| w[1].1 >= w[0].1 && w[1].0 > w[0].0), "the curve rises left to right");
    }

    /// The percentage beside `raised / target` must be the quote share, not the token share: on a
    /// convex curve they differ, and QHUB stood at 18% of its tokens by 5% of its raise.
    #[test]
    fn progress_follows_the_basis_it_is_displayed_with() {
        let quai = |n: u64| U256::from(n) * U256::from(10u64).pow(U256::from(18u64));
        assert_eq!(progress_bps(quai(2_638), quai(49_817), U256::from(18u64), U256::from(100u64)), 529);
        // With no confirmed target there is no quote basis, so the token share is all there is.
        assert_eq!(progress_bps(quai(2_638), U256::ZERO, U256::from(18u64), U256::from(100u64)), 1_800);
    }

    /// A curve whose remaining supply cannot be sold has no sell-out raise to draw against.
    #[test]
    fn a_supply_the_curve_cannot_sell_has_no_target() {
        let (q0, t0) = launch();
        assert!(raise_at_sellout(q0, t0, t0).is_none(), "selling the whole token reserve costs infinity");
        assert!(raise_at_sellout(q0, t0, t0 + U256::from(1u64)).is_none(), "and more than it is not a curve");
    }
}
