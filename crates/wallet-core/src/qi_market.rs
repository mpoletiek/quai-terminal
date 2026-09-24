//! The two markets between QUAI and Qi, quoted side by side.
//!
//! **Protocol conversion** is a single transaction at the controller's rate. It is subject to the
//! block's conversion-flow discount (which can reach the 90% floor) and the output is locked by
//! the protocol for a long period before it can be spent.
//!
//! **The market route** goes through Quainance: QUAI is wrapped to WQUAI, swapped for WQI and
//! unwrapped to Qi (and the reverse). It costs several transactions, LP fees, price impact and
//! slippage, but it settles in minutes and is not subject to the conversion discount.
//!
//! Neither rate is a promise: both are read at a block and move with the market.

use crate::amount;
use crate::data::DataCtx;
use crate::error::{CoreError, Result};
use crate::swap::{Router, SwapAsset, SwapQuote};
use quai_sdk::{BlockTag, U256};
use serde::{Deserialize, Serialize};

/// QUAI decimals.
const QUAI_DECIMALS: u8 = 18;
/// Qi decimals.
const QI_DECIMALS: u8 = 3;

/// Which way the trade goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// QUAI in, Qi out.
    QuaiToQi,
    /// Qi in, QUAI out.
    QiToQuai,
}

impl Direction {
    /// The conversion direction name used by the node and the CLI.
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::QuaiToQi => "quai_to_qi",
            Direction::QiToQuai => "qi_to_quai",
        }
    }

    /// Parse `quai_to_qi` / `qi_to_quai` (also with dashes).
    pub fn parse(text: &str) -> Result<Direction> {
        match text.replace('-', "_").as_str() {
            "quai_to_qi" => Ok(Direction::QuaiToQi),
            "qi_to_quai" => Ok(Direction::QiToQuai),
            _ => Err(CoreError::Invalid("direction must be quai_to_qi or qi_to_quai".into())),
        }
    }

    /// Symbols: (paid, received).
    pub fn assets(self) -> (&'static str, &'static str) {
        match self {
            Direction::QuaiToQi => ("QUAI", "Qi"),
            Direction::QiToQuai => ("Qi", "QUAI"),
        }
    }

    /// Decimals of the paid asset.
    pub fn pay_decimals(self) -> u8 {
        match self {
            Direction::QuaiToQi => QUAI_DECIMALS,
            Direction::QiToQuai => QI_DECIMALS,
        }
    }

    /// Decimals of the received asset.
    pub fn receive_decimals(self) -> u8 {
        match self {
            Direction::QuaiToQi => QI_DECIMALS,
            Direction::QiToQuai => QUAI_DECIMALS,
        }
    }

    /// Fraction digits worth showing for what a route pays. QUAI carries eighteen, and a quote
    /// rendered with all of them — `71.914530496790610041 QUAI` — is unreadable next to another
    /// one, and in the TUI puts a nineteen-character tail beside a block-digit heading.
    pub fn receive_display_digits(self) -> usize {
        match self {
            Direction::QuaiToQi => usize::from(QI_DECIMALS),
            Direction::QiToQuai => 4,
        }
    }
}

/// One transaction the route needs.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Leg {
    /// Short label ("wrap QUAI → WQUAI").
    pub label: String,
    /// What it does, for the detail line.
    pub detail: String,
}

/// A way to get from one asset to the other.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Route {
    /// Route name for display.
    pub name: String,
    /// Expected received amount (destination base units), when it could be quoted.
    pub receives: Option<String>,
    /// Human received amount with its unit.
    pub receives_display: Option<String>,
    /// Transactions the route takes.
    pub legs: Vec<Leg>,
    /// How long until the received amount can be spent.
    pub wait: String,
    /// Costs beyond gas, as text ("LP fee 0.6% · impact 3.30%").
    pub costs: Vec<String>,
    /// What stops the route being used right now.
    pub unavailable: Option<String>,
    /// Warnings worth showing beside the route.
    pub warnings: Vec<String>,
}

impl Route {
    /// Received amount as a number, for comparing routes.
    pub fn amount(&self) -> Option<U256> {
        self.receives.as_ref().and_then(|v| U256::from_str_radix(v, 10).ok())
    }

    /// Whether the route can be started now.
    pub fn usable(&self) -> bool {
        self.unavailable.is_none() && self.amount().is_some_and(|v| !v.is_zero())
    }
}

/// Both markets for one amount.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Comparison {
    /// Trade direction.
    pub direction: Direction,
    /// Input amount (source base units).
    pub amount: String,
    /// Human input amount.
    pub amount_display: String,
    /// One transaction at the controller's rate, output locked by the protocol.
    pub protocol: Route,
    /// Wrap, swap on Quainance, unwrap.
    pub market: Route,
    /// How much more the market route pays, in basis points (negative: the protocol pays more).
    pub market_advantage_bps: Option<i64>,
    /// When this was read (unix seconds).
    pub observed_at: u64,
}

impl Comparison {
    /// The route that pays more, when both could be quoted.
    pub fn better(&self) -> Option<&Route> {
        match (self.protocol.amount(), self.market.amount()) {
            (Some(p), Some(m)) if self.market.usable() && self.protocol.usable() => Some(if m > p { &self.market } else { &self.protocol }),
            _ => None,
        }
    }
}

/// Qits in one Qi (redemptions are whole Qi).
const QI_UNIT: u64 = 1_000;

/// Quote both markets. Reads the node (conversion estimate, pool reserves) and never signs.
/// `owner` lets the swap leg report allowance and balance; without it those are omitted.
pub async fn compare(ctx: &DataCtx, direction: Direction, amount: U256, owner: Option<&str>, slippage_bps: u16) -> Result<Comparison> {
    if amount.is_zero() {
        return Err(CoreError::Invalid("amount must be greater than zero".into()));
    }
    let (pay, _) = direction.assets();
    let amount_display = format!("{} {pay}", amount::format_amount(amount, direction.pay_decimals()));
    let (protocol, market) =
        futures_pair(protocol_route(ctx, direction, amount), market_route(ctx, direction, amount, owner, slippage_bps)).await;
    let protocol = protocol.unwrap_or_else(|e| unavailable_route("protocol conversion", "one transaction", &e.to_string()));
    let market = market.unwrap_or_else(|e| unavailable_route("market route (Quainance)", "several transactions", &e.to_string()));
    let market_advantage_bps = advantage_bps(&protocol, &market);
    Ok(Comparison {
        direction,
        amount: amount.to_string(),
        amount_display,
        protocol,
        market,
        market_advantage_bps,
        observed_at: crate::registry::now(),
    })
}

/// Await two futures concurrently (they read different endpoints).
async fn futures_pair<A: std::future::Future, B: std::future::Future>(a: A, b: B) -> (A::Output, B::Output) {
    let (mut a, mut b) = (Box::pin(a), Box::pin(b));
    let (mut ra, mut rb) = (None, None);
    std::future::poll_fn(|cx| {
        if ra.is_none()
            && let std::task::Poll::Ready(v) = a.as_mut().poll(cx)
        {
            ra = Some(v);
        }
        if rb.is_none()
            && let std::task::Poll::Ready(v) = b.as_mut().poll(cx)
        {
            rb = Some(v);
        }
        if ra.is_some() && rb.is_some() { std::task::Poll::Ready(()) } else { std::task::Poll::Pending }
    })
    .await;
    (ra.expect("polled to ready"), rb.expect("polled to ready"))
}

/// How much more the market route pays than the protocol conversion, in basis points (negative:
/// less). Only when both can run: a market route that buys less than one whole Qi redeems nothing,
/// and "the protocol conversion pays 100% more" than a route that cannot be taken says nothing.
fn advantage_bps(protocol: &Route, market: &Route) -> Option<i64> {
    if !protocol.usable() || !market.usable() {
        return None;
    }
    let scale = |v: U256| v.to_string().parse::<f64>().unwrap_or(0.0);
    let (p, m) = (scale(protocol.amount()?), scale(market.amount()?));
    (p > 0.0).then(|| ((m - p) / p * 10_000.0).clamp(-1_000_000.0, 1_000_000.0) as i64)
}

fn unavailable_route(name: &str, wait: &str, why: &str) -> Route {
    Route {
        name: name.into(),
        receives: None,
        receives_display: None,
        legs: Vec::new(),
        wait: wait.into(),
        costs: Vec::new(),
        unavailable: Some(why.to_string()),
        warnings: Vec::new(),
    }
}

/// The controller's estimate for a conversion of this size, with the discount applied.
async fn protocol_route(ctx: &DataCtx, direction: Direction, amount: U256) -> Result<Route> {
    ctx.online()?;
    // The estimate depends only on the ledgers and zone, so codeless Cyprus-1 placeholders stand
    // in for the wallet's own addresses.
    let quai: quai_sdk::QuaiAddress =
        "0x0000000000000000000000000000000000000001".parse().map_err(|_| CoreError::Invalid("placeholder address".into()))?;
    let qi: quai_sdk::QiAddress =
        "0x0080000000000000000000000000000000000001".parse().map_err(|_| CoreError::Invalid("placeholder address".into()))?;
    let (from, to) = match direction {
        Direction::QuaiToQi => (quai_sdk::primitives::Address::from(quai), quai_sdk::primitives::Address::from(qi)),
        Direction::QiToQuai => (quai_sdk::primitives::Address::from(qi), quai_sdk::primitives::Address::from(quai)),
    };
    let expected = ctx.node.provider.calculate_conversion_amount(from, to, amount).await?;
    let spot = match direction {
        Direction::QuaiToQi => ctx.node.provider.quai_to_qi(crate::network::ZONE, amount, BlockTag::Latest).await.ok().flatten(),
        Direction::QiToQuai => ctx.node.provider.qi_to_quai(crate::network::ZONE, amount, BlockTag::Latest).await.ok().flatten(),
    };
    let (_, receive) = direction.assets();
    let mut warnings = Vec::new();
    let mut costs = vec!["no LP fee or price impact".to_string()];
    if let Some(spot) = spot
        && !spot.is_zero()
        && expected < spot
    {
        let kept = discount_bps(expected, spot);
        costs.push(format!("flow discount {}.{:02}%", kept / 100, kept % 100));
        if kept >= 5_000 {
            warnings.push("the block's conversion flow is discounting heavily right now".into());
        }
    }
    if direction == Direction::QuaiToQi && amount < U256::from(quai_sdk::consensus::MIN_QUAI_CONVERSION_VALUE) {
        warnings.push(format!("conversions start at {} QUAI", amount::quai(U256::from(quai_sdk::consensus::MIN_QUAI_CONVERSION_VALUE))));
    }
    warnings.push("conversions in one prime block share a discount; one above your slippage is refunded".into());
    Ok(Route {
        name: "protocol conversion".into(),
        receives: Some(expected.to_string()),
        receives_display: Some(format!(
            "{} {receive}",
            amount::format_amount_short(expected, direction.receive_decimals(), direction.receive_display_digits())
        )),
        legs: vec![Leg {
            label: format!("convert {} → {receive}", direction.assets().0),
            detail: "one transaction at the controller's rate".into(),
        }],
        wait: "locked by the protocol (weeks)".into(),
        costs,
        unavailable: None,
        warnings,
    })
}

/// How much of the spot value the discount takes, in basis points.
fn discount_bps(expected: U256, spot: U256) -> u64 {
    let scale = |v: U256| v.to_string().parse::<f64>().unwrap_or(0.0);
    let (e, s) = (scale(expected), scale(spot));
    if s <= 0.0 { 0 } else { (((s - e) / s) * 10_000.0).clamp(0.0, 10_000.0) as u64 }
}

/// Wrap, swap on Quainance, unwrap.
async fn market_route(ctx: &DataCtx, direction: Direction, amount: U256, owner: Option<&str>, slippage_bps: u16) -> Result<Route> {
    ctx.online()?;
    let wqi = ctx.network.wqi.clone().ok_or_else(|| CoreError::NotFound("no WQI contract on this network".into()))?.to_lowercase();
    let wquai = ctx.network.wquai.clone().ok_or_else(|| CoreError::NotFound("no WQUAI contract on this network".into()))?.to_lowercase();
    // WQI carries whole Qi in 18 decimals; WQUAI is QUAI 1:1.
    let wqi_asset = SwapAsset::Token { address: wqi, symbol: "WQI".into(), decimals: 18 };
    let wquai_asset = SwapAsset::Token { address: wquai, symbol: "WQUAI".into(), decimals: 18 };
    let router = Router::open(&ctx.app, &ctx.node, &ctx.network, ctx.trust).await?;
    let (quote, legs, wait) = match direction {
        Direction::QuaiToQi => {
            let quote = router.quote(&wquai_asset, &wqi_asset, amount, slippage_bps, owner).await?;
            (
                quote,
                vec![
                    Leg { label: "wrap QUAI → WQUAI".into(), detail: "1:1, no rate risk".into() },
                    Leg { label: "swap WQUAI → WQI".into(), detail: "Quainance pools".into() },
                    Leg { label: "unwrap WQI → Qi".into(), detail: "whole Qi; the rest stays as WQI".into() },
                ],
                "minutes (redeemed Qi is briefly locked)".to_string(),
            )
        }
        Direction::QiToQuai => {
            // Qi is wrapped in whole Qi; WQI atoms are 1e18 per Qi.
            let atoms = quai_sdk::wrappers::qits_to_wqi_atoms(amount)?;
            let quote = router.quote(&wqi_asset, &wquai_asset, atoms, slippage_bps, owner).await?;
            (
                quote,
                vec![
                    Leg { label: "wrap Qi → WQI".into(), detail: "a Qi transaction that must settle".into() },
                    Leg { label: "claim WQI".into(), detail: "credits the wrapped balance".into() },
                    Leg { label: "swap WQI → WQUAI".into(), detail: "Quainance pools".into() },
                    Leg { label: "unwrap WQUAI → QUAI".into(), detail: "instant".into() },
                ],
                "minutes (the Qi wrap must settle first)".to_string(),
            )
        }
    };
    let out = U256::from_str_radix(&quote.amount_out, 10).unwrap_or(U256::ZERO);
    // The received side is native again: WQI atoms become whole Qi, WQUAI atoms are QUAI 1:1.
    let (receives, dust) = match direction {
        Direction::QuaiToQi => {
            // WQI atoms rarely land on a whole Qi: keep what redeems, leave the rest wrapped.
            let qits = out / U256::from(quai_sdk::wrappers::WQI_ATOMS_PER_QIT);
            let whole = qits / U256::from(QI_UNIT) * U256::from(QI_UNIT);
            (whole, qits.saturating_sub(whole))
        }
        Direction::QiToQuai => (out, U256::ZERO),
    };
    let (_, receive) = direction.assets();
    let mut costs = vec![format!("LP fee {}.{}%", quote.fee_bps / 100, (quote.fee_bps % 100) / 10)];
    costs.push(format!("impact {}.{:02}%", quote.impact_bps / 100, quote.impact_bps % 100));
    costs.push(format!("slippage {}.{:02}%", slippage_bps / 100, slippage_bps % 100));
    let mut warnings: Vec<String> = quote.warnings.iter().filter(|w| !w.starts_with("you have")).cloned().collect();
    if !dust.is_zero() && !receives.is_zero() {
        warnings.push(format!("{} Qi of the output stays as WQI (redemptions are whole Qi)", amount::qi(dust)));
    }
    // Below one whole Qi the swap still works, but nothing can be redeemed.
    let too_small = (direction == Direction::QuaiToQi && receives.is_zero())
        .then(|| format!("this buys {} Qi; redemptions are whole Qi, so trade enough for at least 1 Qi", amount::qi(dust)));
    Ok(Route {
        name: "market route (Quainance)".into(),
        receives: Some(receives.to_string()),
        receives_display: Some(format!(
            "{} {receive}",
            amount::format_amount_short(receives, direction.receive_decimals(), direction.receive_display_digits())
        )),
        legs,
        wait,
        costs,
        unavailable: too_small.or_else(|| receives.is_zero().then(|| "the pools cannot fill this amount".to_string())),
        warnings,
    })
}

/// The swap leg on its own, for a screen that needs the pool detail.
pub async fn market_quote(ctx: &DataCtx, direction: Direction, amount: U256, owner: Option<&str>, slippage_bps: u16) -> Result<SwapQuote> {
    let wqi = ctx.network.wqi.clone().ok_or_else(|| CoreError::NotFound("no WQI contract on this network".into()))?.to_lowercase();
    let wquai = ctx.network.wquai.clone().ok_or_else(|| CoreError::NotFound("no WQUAI contract on this network".into()))?.to_lowercase();
    let wqi_asset = SwapAsset::Token { address: wqi, symbol: "WQI".into(), decimals: 18 };
    let wquai_asset = SwapAsset::Token { address: wquai, symbol: "WQUAI".into(), decimals: 18 };
    let router = Router::open(&ctx.app, &ctx.node, &ctx.network, ctx.trust).await?;
    match direction {
        Direction::QuaiToQi => router.quote(&wquai_asset, &wqi_asset, amount, slippage_bps, owner).await,
        Direction::QiToQuai => {
            let atoms = quai_sdk::wrappers::qits_to_wqi_atoms(amount)?;
            router.quote(&wqi_asset, &wquai_asset, atoms, slippage_bps, owner).await
        }
    }
}

#[cfg(test)]
mod tests {
    /// Routes are compared only when both can be taken: a market route too small to redeem a whole
    /// Qi receives nothing, and is not "100% worse".
    #[test]
    fn routes_are_compared_only_when_both_can_run() {
        let route = |receives: Option<&str>| super::Route {
            name: "r".into(),
            receives: receives.map(str::to_string),
            receives_display: None,
            legs: Vec::new(),
            wait: String::new(),
            costs: Vec::new(),
            unavailable: None,
            warnings: Vec::new(),
        };
        assert_eq!(super::advantage_bps(&route(Some("1000")), &route(Some("1100"))), Some(1_000), "the market pays 10% more");
        assert_eq!(super::advantage_bps(&route(Some("1000")), &route(Some("0"))), None, "a market route that redeems nothing");
        assert_eq!(super::advantage_bps(&route(Some("1000")), &route(None)), None);
        let closed = super::unavailable_route("market", "", "no pool");
        assert_eq!(super::advantage_bps(&route(Some("1000")), &closed), None);
    }

    use super::*;

    #[test]
    fn directions_round_trip() {
        assert_eq!(Direction::parse("quai-to-qi").unwrap(), Direction::QuaiToQi);
        assert_eq!(Direction::parse("qi_to_quai").unwrap(), Direction::QiToQuai);
        assert!(Direction::parse("quai_to_usdt").is_err());
        assert_eq!(Direction::QuaiToQi.as_str(), "quai_to_qi");
        assert_eq!(Direction::QuaiToQi.assets(), ("QUAI", "Qi"));
        assert_eq!((Direction::QiToQuai.pay_decimals(), Direction::QiToQuai.receive_decimals()), (3, 18));
    }

    #[test]
    fn discount_and_comparison() {
        assert_eq!(discount_bps(U256::from(1u64), U256::from(10u64)), 9_000);
        assert_eq!(discount_bps(U256::from(10u64), U256::from(10u64)), 0);
        let route = |receives: Option<&str>, unavailable: Option<&str>| Route {
            name: "r".into(),
            receives: receives.map(str::to_string),
            receives_display: None,
            legs: vec![],
            wait: String::new(),
            costs: vec![],
            unavailable: unavailable.map(str::to_string),
            warnings: vec![],
        };
        assert!(route(Some("5"), None).usable());
        assert!(!route(Some("0"), None).usable());
        assert!(!route(Some("5"), Some("pools are empty")).usable());
        let c = Comparison {
            direction: Direction::QuaiToQi,
            amount: "1000".into(),
            amount_display: "1000 QUAI".into(),
            protocol: route(Some("910"), None),
            market: route(Some("8285"), None),
            market_advantage_bps: None,
            observed_at: 0,
        };
        assert_eq!(c.better().and_then(|r| r.amount()), Some(U256::from(8285u64)));
    }

    /// What a route pays is a display string, and QUAI carries eighteen decimals. Rendered in full
    /// it read `71.914530496790610041 QUAI`, which is unreadable beside the other route's figure
    /// and, in the TUI, puts a nineteen-character tail next to a block-digit heading.
    #[test]
    fn a_route_pays_a_readable_number_of_digits() {
        assert_eq!(Direction::QiToQuai.receive_display_digits(), 4, "QUAI is cut to four");
        assert_eq!(Direction::QuaiToQi.receive_display_digits(), 3, "Qi only has three to begin with");
        let quai = crate::amount::format_amount_short(
            U256::from(71_914_530_496_790_610_041u128),
            Direction::QiToQuai.receive_decimals(),
            Direction::QiToQuai.receive_display_digits(),
        );
        assert_eq!(quai, "71.9145");
        // A small amount keeps its precision rather than collapsing to zero.
        let small = crate::amount::format_amount_short(
            U256::from(1_234_500_000_000_000u128),
            Direction::QiToQuai.receive_decimals(),
            Direction::QiToQuai.receive_display_digits(),
        );
        assert_eq!(small, "0.0012");
        assert_eq!(crate::amount::format_amount_short(U256::from(428u64), QI_DECIMALS, 3), "0.428", "sub-1 Qi is not rounded away");
    }
}
