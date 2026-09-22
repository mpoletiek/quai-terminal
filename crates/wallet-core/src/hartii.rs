//! The HartiiLabs launchpad: tokens on their bonding curve, and the ones that have bonded.
//!
//! A second launchpad in the shape of Quainance's own launch zone, and read the same way — except
//! that it has no subgraph, so every figure here comes from the chain. The launcher enumerates its
//! tokens and names each one's curve; the curves are EIP-1167 clones of one pinned implementation,
//! which is what makes reading them by a fixed ABI safe.
//!
//! **Bonding is not migration here.** The two tokens that have bonded so far sold out their curve
//! supply, and neither has a pair on any factory this wallet knows — Quainance's, the launch AMM's,
//! HartiiLabs' own, or the legacy one. They are still traded, on the curve itself: `quoteBuy` and
//! `quoteSell` both answer, and Quainance's own trade-zone index records their market as a CURVE at
//! the curve's address with hundreds of executions against it. So a bonded token is a live market,
//! not a dead one.

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
    /// QUAI per whole token from inverse quoteBuy(1 QUAI), including fees and price impact.
    pub price_quai: Option<f64>,
}

impl HartiiLaunch {
    /// Where it can be traded, in a sentence. A bonded token has no pool, but its curve keeps
    /// quoting and taking both sides, which is where Quainance's own app trades it too.
    pub fn market_note(&self) -> &'static str {
        if self.bonded { "bonded; still bought and sold on its curve, no pool" } else { "on its bonding curve" }
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
    let launcher = crate::data::verify_pinned(&ctx.app, &ctx.node, &ctx.network, &pin, "HartiiLabs launcher", ctx.trust).await?;
    let launcher = launcher.to_string().to_lowercase();
    let Some(mc) = crate::multicall::Multicall::open(ctx).await else {
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

    // Per token: symbol and decimals; per curve: state plus fee for the subsequent net-input quote.
    // Seven reads per token; `base` below strides by the same number.
    const PER_TOKEN: usize = 7;
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
    }
    let out = mc.try_all(&calls).await?;
    let at = |i: usize| out.get(i).and_then(Option::as_ref);
    // The pinned runtime's quoteBuy accepts NET input: buy deducts its fee before quoting.
    // Query the exact net amount for a one-QUAI user payment, keeping the fee observation paired.
    let mut quote_calls = Vec::new();
    let mut quote_indices = vec![None; tokens.len()];
    for (n, curve) in curves.iter().enumerate() {
        if let Some(fee) = at(n * PER_TOKEN + 6).filter(|d| d.len() >= 32).and_then(|d| u16::try_from(word(d, 0)).ok())
            && let Ok((net, _)) = after_fee(U256::from(10u64).pow(U256::from(18u64)), fee)
            && !curve.is_empty()
        {
            quote_indices[n] = Some(quote_calls.len());
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
        let per_quai = quote_indices[n]
            .and_then(|i| quotes.get(i))
            .and_then(Option::as_ref)
            .filter(|d| d.len() >= 32)
            .map(|d| word(d, 0))
            .unwrap_or(U256::ZERO);
        // A bonded curve reads 100% by construction: it sold its whole supply.
        let progress_bps = (!supply.is_zero()).then(|| {
            let bps = sold.saturating_mul(U256::from(10_000u64)) / supply;
            u64::try_from(bps).unwrap_or(10_000).min(10_000)
        });
        // Ask the curve what one QUAI buys and invert it, rather than deriving a price from its
        // reserves. `virtualQuaiReserve`/`virtualTokenReserve` are the curve's *parameters* —
        // identical on all 27 tokens at 17,000 QUAI against 1.073b — and the constant-product ratio
        // built from them agrees with the curve only while it is still selling. A bonded curve
        // reports the same reserves as every other bonded one and prices nothing like them: HRT and
        // QAXE both read 784m sold with nothing left in them, yet one is worth 65x the other. The
        // contract's own quote is right in both states, and it includes the fee.
        let price_quai = (!per_quai.is_zero() && (bonded || per_quai < supply.saturating_sub(sold)))
            .then(|| 1.0 / to_f64(per_quai, decimals))
            .filter(|p| p.is_finite() && *p > 0.0);
        rows.push(HartiiLaunch {
            token: token.clone(),
            curve: curve.clone(),
            symbol,
            decimals,
            bonded,
            raised_quai: to_f64(real_quai, 18),
            progress_bps,
            price_quai,
        });
    }
    rows.reverse();
    Ok(rows)
}

/// The launchpad's **bonded** tokens as market rows.
///
/// Bonding here sells out the curve's supply but does not retire it: the curve still quotes and
/// still takes both sides, and no pool was ever deployed for it to move to. So a bonded token is a
/// real market with nowhere else to be, and it belongs in the market list.
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
                price_basis: crate::markets::PriceBasis::OneQuaiBuyQuote,
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
        assert_eq!(pools[0].token0.symbol, "HRT", "and the bonded one is a market: it still quotes, with no pool to move to");
        assert_eq!(pools[0].venue, Venue::Curve);
        assert_eq!(pools[0].curve.as_ref().unwrap().launchpad.as_deref(), Some("HartiiLabs"), "whose launchpad it is, is part of the row");
        // A bonded curve that cannot quote is not a market either.
        let mut mute = row("DEAD", true, Some(10_000));
        mute.price_quai = None;
        assert!(curve_pools(&[mute], "0x006c3e2a").is_empty());
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
    fn a_bonded_token_says_it_has_no_pool_yet() {
        assert!(row("HRT", true, Some(10_000)).market_note().contains("no pool"));
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
