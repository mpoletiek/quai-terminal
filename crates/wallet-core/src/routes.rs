//! Which swaps the router can actually fill, decided before the user commits to a pair.
//!
//! The router quotes a route only once both sides are chosen, so a pair with no pool used to fail
//! late, with "no Quainance pool route from X to Y". This module answers the same question from the
//! pool list alone — no RPC — so the token picker can refuse an impossible pair instead of letting
//! the user build one.
//!
//! The rule here and the router's candidate paths are the same code — [`candidate_paths`],
//! [`VENUES`] and [`two_swap_hubs`] — so the picker cannot promise a route the router will not
//! try, or hide one it would have found.
//!
//! A router only reaches its own factory's pairs, so every swap stays on one venue. A pair no
//! single venue connects is filled by two swaps through a hub, one on each venue, and only then:
//! a second transaction costs a second fee and a second approval.

use crate::markets::{Pool, Venue};
use crate::swap::LP_FEE_BPS;
use std::collections::{HashMap, HashSet};

/// Pools at or below this TVL are real but too thin to trade through without severe impact. A
/// route is only as good as its thinnest pool, so this is judged per route, not per pool.
pub const THIN_ROUTE_USD: f64 = 100.0;

/// The venues a swap can use, in preference order when two routes are otherwise equal.
pub const VENUES: [Venue; 4] = [Venue::Main, Venue::LaunchAmm, Venue::Legacy, Venue::HartiiAmm];

/// Every path the router will consider for a pair on one venue, in preference order: direct, then
/// one hub, then two. Addresses are lowercased; `hubs` keeps the caller's order.
///
/// Both the router's quote and [`RouteGraph`] build their candidates here, so the token picker and
/// the router can never disagree about what is fillable.
pub fn candidate_paths(from: &str, to: &str, hubs: &[String]) -> Vec<Vec<String>> {
    let (a, b) = (from.to_lowercase(), to.to_lowercase());
    let hubs: Vec<String> = hubs.iter().map(|h| h.to_lowercase()).collect();
    let usable = |h: &String| *h != a && *h != b;
    let mut out = vec![vec![a.clone(), b.clone()]];
    for h in hubs.iter().filter(|h| usable(h)) {
        out.push(vec![a.clone(), h.clone(), b.clone()]);
    }
    for h1 in hubs.iter().filter(|h| usable(h)) {
        for h2 in hubs.iter().filter(|h| usable(h)) {
            if h1 != h2 {
                out.push(vec![a.clone(), h1.clone(), h2.clone(), b.clone()]);
            }
        }
    }
    out
}

/// The tokens a two-swap route may pass between venues, in the caller's hub order. Tried only when
/// no venue fills the pair on its own; the first swap ends on the hub, the second starts there.
pub fn two_swap_hubs(from: &str, to: &str, hubs: &[String]) -> Vec<String> {
    let (a, b) = (from.to_lowercase(), to.to_lowercase());
    hubs.iter().map(|h| h.to_lowercase()).filter(|h| *h != a && *h != b).collect()
}

/// The ordered venue pairs a two-swap route may use: first swap, second swap.
pub fn venue_pairs() -> Vec<(Venue, Venue)> {
    VENUES.iter().flat_map(|a| VENUES.iter().filter(move |b| *b != a).map(move |b| (*a, *b))).collect()
}

/// How a pair can be filled, and how thin the route's worst pool is.
#[derive(Clone, Debug, PartialEq)]
pub struct RouteInfo {
    /// Token addresses, pay side first, receive side last (lowercase). A hub between two swaps
    /// appears once.
    pub path: Vec<String>,
    /// Symbols along the same path, for display.
    pub symbols: Vec<String>,
    /// The thinnest pool on the route in USD, when every pool on it is priced.
    pub min_tvl_usd: Option<f64>,
    /// One entry per transaction: its venue and the index in `path` where that swap ends.
    pub swaps: Vec<(Venue, usize)>,
}

impl RouteInfo {
    /// Pools crossed.
    pub fn hops(&self) -> usize {
        self.path.len().saturating_sub(1)
    }

    /// Total LP fee along the route, in basis points.
    pub fn fee_bps(&self) -> u64 {
        LP_FEE_BPS * self.hops() as u64
    }

    /// The route is fillable but its thinnest pool is dust; any useful size will move it hard.
    pub fn thin(&self) -> bool {
        self.min_tvl_usd.is_some_and(|v| v <= THIN_ROUTE_USD)
    }

    /// Transactions the route takes.
    pub fn swap_count(&self) -> usize {
        self.swaps.len()
    }

    /// `WQI → WQUAI → LAPTOP`, or `QOGE → WQUAI, then WQUAI → USDT` for two swaps.
    pub fn text(&self) -> String {
        let mut parts = Vec::new();
        let mut start = 0;
        for (_, end) in &self.swaps {
            parts.push(self.symbols[start..=(*end).min(self.symbols.len().saturating_sub(1))].join(" → "));
            start = *end;
        }
        if parts.is_empty() { self.symbols.join(" → ") } else { parts.join(", then ") }
    }
}

/// Estimated execution costs for comparison. These gas limits are planning hints; preparation
/// still simulates each transaction and reserves its actual reviewed maximum fee.
#[derive(Clone, Debug, serde::Serialize)]
pub struct RouteCost {
    pub swap_transactions: usize,
    pub approval_transactions_estimate: usize,
    pub approval_state_complete: bool,
    pub gas_units_estimate: u64,
    pub gas_price_base: String,
    pub fee_native_estimate: String,
    pub fee_output_estimate: Option<String>,
    pub net_output_estimate: Option<String>,
    pub native_funds_sufficient: Option<bool>,
    pub sequential: bool,
    /// Sequential routes cannot enforce the final minimum in the first transaction.
    pub atomic_minimum_out: Option<String>,
    pub warnings: Vec<String>,
}

/// `output_per_native` is an optional, externally qualified ratio of output token atoms to
/// native QUAI atoms. Missing prices stay unknown rather than being treated as zero execution cost.
pub fn estimate_cost(
    quote: &crate::swap::SwapQuote,
    gas_price: quai_sdk::U256,
    output_per_native: Option<(quai_sdk::U256, quai_sdk::U256)>,
    native_balance: Option<quai_sdk::U256>,
) -> crate::Result<RouteCost> {
    use quai_sdk::U256;
    let parse = |raw: &str| U256::from_str_radix(raw, 10).map_err(|_| crate::CoreError::Invalid("invalid quote amount".into()));
    let swaps = quote.legs.len().max(1);
    let sequential = swaps > 1;
    let mut approvals = usize::from(quote.approval_needed);
    if quote.approval_needed && quote.allowance.as_deref().is_some_and(|a| parse(a).is_ok_and(|a| !a.is_zero())) {
        approvals += 1; // exact approval may first require clearing the old nonzero allowance
    }
    // Later-spender allowances are not yet read; conservatively budget reset plus approval.
    approvals += swaps.saturating_sub(1) * 2;
    let gas = 400_000u64
        .checked_mul(swaps as u64)
        .and_then(|v| v.checked_add(150_000u64.checked_mul(quote.pools.len() as u64)?))
        .and_then(|v| v.checked_add(80_000u64.checked_mul(approvals as u64)?))
        .ok_or_else(|| crate::CoreError::Invalid("route cost overflow".into()))?;
    let fee = gas_price.checked_mul(U256::from(gas)).ok_or_else(|| crate::CoreError::Invalid("route fee overflow".into()))?;
    let converted = output_per_native.and_then(|(n, d)| {
        if n.is_zero() || d.is_zero() {
            return None;
        }
        let floor = crate::amount::mul_div(fee, n, d)?;
        let remainder = fee.widening_mul::<256, 4, 512, 8>(n) % d.widening_mul::<256, 4, 512, 8>(U256::from(1));
        floor.checked_add(U256::from(!remainder.is_zero() as u8))
    });
    let native_payment = if matches!(quote.from, crate::swap::SwapAsset::Quai) { parse(&quote.amount_in)? } else { U256::ZERO };
    let needed = native_payment.checked_add(fee).ok_or_else(|| crate::CoreError::Invalid("route native funding overflow".into()))?;
    let mut warnings = vec!["Gas is a planning estimate; each transaction is simulated and reviewed before signing.".into()];
    if sequential {
        warnings.push("Sequential route: intermediate tokens remain in the wallet; final output is re-quoted after settlement and is not atomically guaranteed.".into());
    }
    if converted.is_none() {
        warnings.push("Output-token fee conversion unavailable; net output and net route ranking are unknown.".into());
    }
    Ok(RouteCost {
        swap_transactions: swaps,
        approval_transactions_estimate: approvals,
        approval_state_complete: !sequential && (!matches!(quote.from, crate::swap::SwapAsset::Token { .. }) || quote.allowance.is_some()),
        gas_units_estimate: gas,
        gas_price_base: gas_price.to_string(),
        fee_native_estimate: fee.to_string(),
        fee_output_estimate: converted.map(|v| v.to_string()),
        net_output_estimate: converted.map(|fee| parse(&quote.amount_out).map(|out| out.saturating_sub(fee).to_string())).transpose()?,
        native_funds_sufficient: native_balance.map(|balance| balance >= needed),
        sequential,
        atomic_minimum_out: (!sequential).then(|| quote.minimum_out.clone()),
        warnings,
    })
}

type Adjacency = HashMap<String, HashMap<String, Option<f64>>>;

/// The pool graph: which tokens are connected on each venue, and how deep each connection is.
#[derive(Clone, Debug, Default)]
pub struct RouteGraph {
    /// venue → token → (neighbour → pool TVL in USD, when known).
    venues: HashMap<Venue, Adjacency>,
    symbols: HashMap<String, String>,
    hubs: Vec<String>,
}

impl RouteGraph {
    /// Build from the pool list. `hubs` are the intermediate tokens the router is willing to route
    /// through, in the same order [`crate::swap::Router`] uses them (WQUAI, WQI, USDT). Markets no
    /// router trades (bonding curves) are left out.
    pub fn new(pools: &[Pool], hubs: &[String]) -> RouteGraph {
        let mut venues: HashMap<Venue, Adjacency> = HashMap::new();
        let mut symbols = HashMap::new();
        for p in pools.iter().filter(|p| p.venue.routable()) {
            // A pool with an empty side cannot fill anything; the router skips it too.
            if p.reserve0 <= 0.0 || p.reserve1 <= 0.0 {
                continue;
            }
            let (a, b) = (p.token0.address.to_lowercase(), p.token1.address.to_lowercase());
            if a.is_empty() || b.is_empty() || a == b {
                continue;
            }
            symbols.entry(a.clone()).or_insert_with(|| p.token0.symbol.clone());
            symbols.entry(b.clone()).or_insert_with(|| p.token1.symbol.clone());
            // Keep the deepest pool when a pair is listed more than once.
            let keep = |slot: &mut Option<f64>, tvl: Option<f64>| {
                if slot.is_none() || tvl.is_some_and(|t| slot.is_some_and(|s| t > s)) {
                    *slot = tvl;
                }
            };
            let adj = venues.entry(p.venue).or_default();
            keep(adj.entry(a.clone()).or_default().entry(b.clone()).or_default(), p.tvl_usd);
            keep(adj.entry(b).or_default().entry(a).or_default(), p.tvl_usd);
        }
        RouteGraph { venues, symbols, hubs: hubs.iter().map(|h| h.to_lowercase()).collect() }
    }

    /// No pools known yet — the caller should not filter anything.
    pub fn is_empty(&self) -> bool {
        self.venues.values().all(HashMap::is_empty)
    }

    /// The token trades in at least one non-empty pool, on any venue.
    pub fn has_pool(&self, token: &str) -> bool {
        let t = token.to_lowercase();
        self.venues.values().any(|adj| adj.contains_key(&t))
    }

    /// Every token with a pool.
    pub fn tokens(&self) -> impl Iterator<Item = &str> {
        let mut seen = HashSet::new();
        self.venues.values().flat_map(|adj| adj.keys()).filter(move |t| seen.insert(t.as_str())).map(String::as_str)
    }

    fn tvl(&self, venue: Venue, a: &str, b: &str) -> Option<Option<f64>> {
        self.venues.get(&venue).and_then(|adj| adj.get(a)).and_then(|n| n.get(b)).copied()
    }

    fn symbol(&self, a: &str) -> String {
        self.symbols.get(a).cloned().unwrap_or_else(|| crate::session::short_address(a))
    }

    /// The shortest path one venue offers between two tokens.
    fn venue_path(&self, venue: Venue, a: &str, b: &str) -> Option<Vec<String>> {
        let adj = self.venues.get(&venue)?;
        if a == b || !adj.contains_key(a) || !adj.contains_key(b) {
            return None;
        }
        candidate_paths(a, b, &self.hubs).into_iter().find(|path| path.windows(2).all(|w| self.tvl(venue, &w[0], &w[1]).is_some()))
    }

    fn build(&self, legs: Vec<(Venue, Vec<String>)>) -> RouteInfo {
        // The route is only as deep as its thinnest pool; an unpriced pool makes the whole route
        // unpriced rather than optimistically deep.
        let mut min: Option<f64> = None;
        let mut known = true;
        let mut path: Vec<String> = Vec::new();
        let mut swaps = Vec::new();
        for (venue, leg) in legs {
            for w in leg.windows(2) {
                match self.tvl(venue, &w[0], &w[1]).flatten() {
                    Some(v) => min = Some(min.map_or(v, |m: f64| m.min(v))),
                    None => known = false,
                }
            }
            let skip = usize::from(!path.is_empty());
            path.extend(leg.into_iter().skip(skip));
            swaps.push((venue, path.len() - 1));
        }
        let symbols = path.iter().map(|t| self.symbol(t)).collect();
        RouteInfo { path, symbols, min_tvl_usd: known.then_some(min).flatten(), swaps }
    }

    /// The shortest route the router would consider, or None when it would find nothing.
    ///
    /// Shortest wins rather than deepest: fewer hops means less LP fee and less compounding impact,
    /// and the router still picks by output among the candidates it prices. One swap always beats
    /// two.
    pub fn route(&self, from: &str, to: &str) -> Option<RouteInfo> {
        let (a, b) = (from.to_lowercase(), to.to_lowercase());
        if a == b || !self.has_pool(&a) || !self.has_pool(&b) {
            return None;
        }
        let single = VENUES.iter().filter_map(|v| self.venue_path(*v, &a, &b).map(|p| (*v, p))).min_by_key(|(_, p)| p.len());
        if let Some((venue, path)) = single {
            return Some(self.build(vec![(venue, path)]));
        }
        let mut best: Option<Vec<(Venue, Vec<String>)>> = None;
        for hub in two_swap_hubs(&a, &b, &self.hubs) {
            for (first, second) in venue_pairs() {
                if let (Some(p1), Some(p2)) = (self.venue_path(first, &a, &hub), self.venue_path(second, &hub, &b)) {
                    let hops = p1.len() + p2.len();
                    if best.as_ref().is_none_or(|legs| hops < legs.iter().map(|(_, p)| p.len()).sum()) {
                        best = Some(vec![(first, p1), (second, p2)]);
                    }
                }
            }
        }
        best.map(|legs| self.build(legs))
    }

    /// Bounded candidate descriptions for explicit route choice. A direct pool does not hide
    /// cross-venue alternatives. This graph does not rank value; authoritative quotes do that.
    pub fn alternatives(&self, from: &str, to: &str, limit: usize) -> Vec<RouteInfo> {
        let (a, b) = (from.to_lowercase(), to.to_lowercase());
        if a == b {
            return Vec::new();
        }
        let mut results = Vec::new();
        for venue in VENUES {
            if let Some(path) = self.venue_path(venue, &a, &b) {
                results.push(self.build(vec![(venue, path)]));
            }
        }
        for hub in two_swap_hubs(&a, &b, &self.hubs) {
            for (first, second) in venue_pairs() {
                if let (Some(p1), Some(p2)) = (self.venue_path(first, &a, &hub), self.venue_path(second, &hub, &b)) {
                    let info = self.build(vec![(first, p1), (second, p2)]);
                    if info.path.iter().collect::<HashSet<_>>().len() == info.path.len() && !results.contains(&info) {
                        results.push(info);
                    }
                }
            }
        }
        results.truncate(limit.min(32));
        results
    }

    /// Whether a pair can be filled at all.
    pub fn routable(&self, from: &str, to: &str) -> bool {
        self.route(from, to).is_some()
    }

    /// Every token reachable from `from`, itself excluded.
    pub fn routable_from(&self, from: &str) -> HashSet<String> {
        let a = from.to_lowercase();
        self.tokens().filter(|t| **t != a && self.routable(&a, t)).map(str::to_string).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markets::PoolToken;

    fn tok(address: &str, symbol: &str) -> PoolToken {
        PoolToken { address: address.into(), symbol: symbol.into(), decimals: 18 }
    }

    fn pool(a: (&str, &str), b: (&str, &str), tvl: Option<f64>) -> Pool {
        Pool {
            address: format!("0xpair{}{}", a.1, b.1),
            token0: tok(a.0, a.1),
            token1: tok(b.0, b.1),
            reserve0: 1.0,
            reserve1: 1.0,
            tvl_usd: tvl,
            volume_24h_usd: None,

            ..Default::default()
        }
    }

    const WQUAI: &str = "0x006c3e2aaae5db1bcd11a1a097ce572312eaddbb";
    const WQI: &str = "0x002b2596ecf05c93a31ff916e8b456df6c77c750";
    const USDT: &str = "0x0049f7cbca3556c2dfae62aafa7015f99de1b8f5";

    fn hubs() -> Vec<String> {
        vec![WQUAI.into(), WQI.into(), USDT.into()]
    }

    /// The live Cyprus-1 shape: a WQI cluster and a WQUAI cluster, bridged by WQI/WQUAI.
    fn mainnet_shape() -> Vec<Pool> {
        vec![
            pool((WQI, "WQI"), (WQUAI, "WQUAI"), Some(67_150.0)),
            pool((USDT, "USDT"), (WQUAI, "WQUAI"), Some(4_355.0)),
            pool((WQI, "WQI"), (USDT, "USDT"), Some(535.0)),
            pool(("0xa1", "SMOL"), (WQI, "WQI"), Some(1_157.0)),
            pool(("0xa2", "QOWBOY"), (WQI, "WQI"), Some(2_651.0)),
            pool(("0xa3", "LONGHORN"), (WQI, "WQI"), Some(67.0)),
            pool(("0xb1", "LAPTOP"), (WQUAI, "WQUAI"), Some(510.0)),
            pool(("0xb2", "ROGUE"), (WQUAI, "WQUAI"), Some(136.0)),
            pool(("0xb3", "NVNT"), (WQUAI, "WQUAI"), Some(4.25)),
        ]
    }

    #[test]
    fn direct_route_keeps_cross_venue_alternatives_visible() {
        let mut pools = mainnet_shape();
        let mut launch = pool((WQI, "WQI"), (WQUAI, "WQUAI"), Some(100_000.0));
        launch.venue = Venue::LaunchAmm;
        pools.push(launch);
        let graph = RouteGraph::new(&pools, &hubs());
        let alternatives = graph.alternatives(WQI, USDT, 32);
        assert!(alternatives.iter().any(|r| r.swap_count() == 1));
        assert!(alternatives.iter().any(|r| r.swap_count() == 2));
        assert_eq!(graph.alternatives(WQI, USDT, 1).len(), 1);
        assert!(graph.alternatives(WQI, WQI, 32).is_empty());
    }

    #[test]
    fn unknown_fee_conversion_and_sequential_minimum_stay_explicit() {
        use crate::swap::{SwapAsset, SwapLeg, SwapQuote};
        use quai_sdk::U256;
        let leg = SwapLeg {
            venue: Venue::Main,
            router: "router".into(),
            path: vec!["a".into(), "b".into()],
            route: vec![],
            pools: vec![],
            amount_in: "100".into(),
            amount_out: "200".into(),
            minimum_out: "190".into(),
            output_decimals: 18,
        };
        let mut quote = SwapQuote {
            from: SwapAsset::Quai,
            to: SwapAsset::Quai,
            amount_in: "100".into(),
            amount_out: "100000000".into(),
            minimum_out: "190".into(),
            slippage_bps: 50,
            path: vec![],
            route: vec![],
            pools: vec![],
            impact_bps: 0,
            fee_bps: 30,
            router: "router".into(),
            allowance: None,
            approval_needed: false,
            balance: None,
            insufficient: false,
            warnings: vec![],
            observed_at: 1,
            liquidity_at: None,
            legs: vec![leg.clone()],
        };
        let unknown = estimate_cost(&quote, U256::from(1), None, Some(U256::from(100))).unwrap();
        assert_eq!(unknown.net_output_estimate, None);
        assert_eq!(unknown.native_funds_sufficient, Some(false), "input balance alone is not gas funding");
        assert_eq!(unknown.atomic_minimum_out.as_deref(), Some("190"));
        let priced = estimate_cost(&quote, U256::from(1), Some((U256::from(1), U256::from(3))), None).unwrap();
        assert_eq!(priced.fee_output_estimate.as_deref(), Some("133334"), "fee conversion rounds upward");
        quote.legs.push(leg);
        let sequential = estimate_cost(&quote, U256::from(1), None, None).unwrap();
        assert_eq!(sequential.atomic_minimum_out, None);
        assert_eq!(sequential.approval_transactions_estimate, 2);
        assert!(!sequential.approval_state_complete);
        quote.from = SwapAsset::Token { address: "a".into(), symbol: "A".into(), decimals: 18 };
        quote.approval_needed = true;
        quote.allowance = Some("1".into());
        assert_eq!(estimate_cost(&quote, U256::from(1), None, None).unwrap().approval_transactions_estimate, 4);
        assert!(estimate_cost(&quote, U256::MAX, None, None).is_err());
    }

    #[test]
    fn direct_then_one_hub_then_two() {
        let g = RouteGraph::new(&mainnet_shape(), &hubs());
        // Direct.
        let r = g.route(WQI, WQUAI).unwrap();
        assert_eq!(r.hops(), 1);
        assert_eq!(r.fee_bps(), 30);
        // One hub: SMOL only pairs with WQI, USDT reaches WQI directly.
        let r = g.route("0xa1", USDT).unwrap();
        assert_eq!(r.hops(), 2);
        assert_eq!(r.text(), "SMOL → WQI → USDT");
        // Two hubs: SMOL is WQI-side, LAPTOP is WQUAI-side. This is the case that fails today.
        let r = g.route("0xa1", "0xb1").unwrap();
        assert_eq!(r.hops(), 3);
        assert_eq!(r.text(), "SMOL → WQI → WQUAI → LAPTOP");
        assert_eq!(r.fee_bps(), 90, "three hops cost 0.9%");
    }

    /// The whole point: with two hubs allowed, every token pair on the live shape is fillable.
    #[test]
    fn every_pair_is_routable_on_the_live_shape() {
        let g = RouteGraph::new(&mainnet_shape(), &hubs());
        let tokens: Vec<String> = g.tokens().map(str::to_string).collect();
        assert_eq!(tokens.len(), 9);
        let mut unroutable = Vec::new();
        for a in &tokens {
            for b in &tokens {
                if a != b && !g.routable(a, b) {
                    unroutable.push((a.clone(), b.clone()));
                }
            }
        }
        assert!(unroutable.is_empty(), "unroutable pairs: {unroutable:?}");
        // And the one-hub-only rule really would have missed some.
        let one_hub_only = |a: &str, b: &str| g.route(a, b).is_some_and(|r| r.hops() <= 2);
        assert!(!one_hub_only("0xa1", "0xb1"), "SMOL→LAPTOP needs the second hub");
    }

    #[test]
    fn a_route_is_as_thin_as_its_worst_pool() {
        let g = RouteGraph::new(&mainnet_shape(), &hubs());
        // NVNT/WQUAI is $4.25, so anything through it is dust however deep the rest is.
        let r = g.route("0xb3", WQI).unwrap();
        assert_eq!(r.min_tvl_usd, Some(4.25));
        assert!(r.thin());
        // WQI → WQUAI is deep on both counts.
        assert!(!g.route(WQI, WQUAI).unwrap().thin());
        // An unpriced pool makes the route unpriced rather than optimistically deep.
        let mut pools = mainnet_shape();
        pools.push(pool(("0xc1", "MYST"), (WQUAI, "WQUAI"), None));
        let g = RouteGraph::new(&pools, &hubs());
        let r = g.route("0xc1", WQI).unwrap();
        assert_eq!(r.min_tvl_usd, None);
        assert!(!r.thin(), "unknown depth is not asserted to be thin");
    }

    #[test]
    fn empty_pools_and_unknown_tokens_are_not_routes() {
        let mut pools = mainnet_shape();
        // A pool that exists but holds nothing routes nothing.
        pools.push(Pool { reserve0: 0.0, ..pool(("0xd1", "EMPTY"), (WQUAI, "WQUAI"), Some(10.0)) });
        let g = RouteGraph::new(&pools, &hubs());
        assert!(!g.has_pool("0xd1"));
        assert!(g.route("0xd1", WQUAI).is_none());
        assert!(g.route("0xdead", WQUAI).is_none(), "a token with no pool at all");
        assert!(g.route(WQI, WQI).is_none(), "a token cannot route to itself");
        assert!(RouteGraph::default().is_empty());
        assert!(RouteGraph::default().route(WQI, WQUAI).is_none());
    }

    #[test]
    fn routable_from_lists_the_reachable_set() {
        let g = RouteGraph::new(&mainnet_shape(), &hubs());
        let from_quai = g.routable_from(WQUAI);
        assert_eq!(from_quai.len(), 8, "every other token is reachable from WQUAI");
        assert!(!from_quai.contains(WQUAI));
        // A token whose only pool is with an isolated partner reaches only that partner.
        let pools = vec![pool(("0xe1", "A"), ("0xe2", "B"), Some(5.0))];
        let g = RouteGraph::new(&pools, &hubs());
        assert_eq!(g.routable_from("0xe1"), HashSet::from(["0xe2".to_string()]));
    }

    /// The picker and the router share this, so its shape is the contract between them.
    #[test]
    fn candidate_paths_are_direct_then_one_hub_then_two() {
        let paths = candidate_paths("0xa1", "0xb1", &hubs());
        assert_eq!(paths[0], vec!["0xa1", "0xb1"], "direct first");
        assert_eq!(paths.len(), 1 + 3 + 6, "1 direct + 3 one-hub + 6 ordered hub pairs");
        assert_eq!(paths[1..4].iter().map(Vec::len).collect::<Vec<_>>(), vec![3, 3, 3]);
        assert!(paths[4..].iter().all(|p| p.len() == 4));
        // Hub order is the caller's, and a hub is never revisited within a path.
        assert_eq!(paths[1][1], WQUAI);
        assert!(paths.iter().all(|p| {
            let mut seen = p.clone();
            seen.sort();
            seen.dedup();
            seen.len() == p.len()
        }));
        // A hub that is itself one of the sides is never used as an intermediate.
        let from_hub = candidate_paths(WQI, "0xb1", &hubs());
        assert!(from_hub.iter().all(|p| p[1..p.len() - 1].iter().all(|h| h != WQI)));
        assert_eq!(from_hub.len(), 1 + 2 + 2, "WQI drops out of the hub set");
        // Addresses are lowercased so map lookups match.
        assert_eq!(candidate_paths("0xAA", "0xBB", &[])[0], vec!["0xaa", "0xbb"]);
    }

    /// A graduated launch pairs only with WQUAI on its own exchange. QUAI reaches it in one swap
    /// there; a token on the main exchange takes two, handing WQUAI across; and a bonding curve
    /// is never a route at all.
    #[test]
    fn launch_amm_tokens_route_on_their_venue_or_through_a_hub() {
        let mut pools = mainnet_shape();
        pools.push(Pool { venue: Venue::LaunchAmm, ..pool(("0xd1", "QOGE"), (WQUAI, "WQUAI"), Some(60_000.0)) });
        pools.push(Pool { venue: Venue::LaunchAmm, ..pool(("0xd2", "QCON"), (WQUAI, "WQUAI"), Some(50_000.0)) });
        pools.push(Pool { venue: Venue::Curve, ..pool(("0xe9", "CHEEZ"), (WQUAI, "WQUAI"), Some(1.0)) });
        let g = RouteGraph::new(&pools, &hubs());
        // One swap on the launch AMM.
        let r = g.route("0xd1", WQUAI).unwrap();
        assert_eq!((r.swap_count(), r.swaps[0].0), (1, Venue::LaunchAmm));
        let r = g.route("0xd1", "0xd2").unwrap();
        assert_eq!((r.swap_count(), r.text()), (1, "QOGE → WQUAI → QCON".to_string()), "both launches share the venue's WQUAI");
        // Two swaps: off the launch AMM on WQUAI, onto the main exchange from it.
        let r = g.route("0xd1", USDT).unwrap();
        assert_eq!(r.swaps, vec![(Venue::LaunchAmm, 1), (Venue::Main, 2)]);
        assert_eq!(r.text(), "QOGE → WQUAI, then WQUAI → USDT");
        assert_eq!((r.hops(), r.fee_bps()), (2, 60));
        let r = g.route("0xa1", "0xd1").unwrap();
        assert_eq!(r.text(), "SMOL → WQI → WQUAI, then WQUAI → QOGE");
        assert_eq!(r.swaps, vec![(Venue::Main, 2), (Venue::LaunchAmm, 3)]);
        assert_eq!(r.min_tvl_usd, Some(1_157.0), "thinnest pool across both swaps");
        // The bonding curve is not a pool any router trades.
        assert!(!g.has_pool("0xe9") && g.route("0xe9", WQUAI).is_none());
        // Everything routable before still is, and in one swap.
        for a in ["0xa1", "0xb1", WQI, USDT] {
            for b in ["0xa2", "0xb2", WQUAI] {
                assert_eq!(g.route(a, b).map(|r| r.swap_count()), Some(1), "{a} → {b}");
            }
        }
    }

    #[test]
    fn two_swap_hubs_and_venue_pairs_are_the_shared_rule() {
        assert_eq!(two_swap_hubs(WQUAI, "0xd1", &hubs()), vec![WQI.to_string(), USDT.to_string()], "a side is never its own hub");
        // Every ordered pair of distinct routable venues: a two-swap route may start on one
        // exchange and finish on another, with Hartii there are four independently pinned exchanges.
        assert_eq!(
            venue_pairs(),
            vec![
                (Venue::Main, Venue::LaunchAmm),
                (Venue::Main, Venue::Legacy),
                (Venue::Main, Venue::HartiiAmm),
                (Venue::LaunchAmm, Venue::Main),
                (Venue::LaunchAmm, Venue::Legacy),
                (Venue::LaunchAmm, Venue::HartiiAmm),
                (Venue::Legacy, Venue::Main),
                (Venue::Legacy, Venue::LaunchAmm),
                (Venue::Legacy, Venue::HartiiAmm),
                (Venue::HartiiAmm, Venue::Main),
                (Venue::HartiiAmm, Venue::LaunchAmm),
                (Venue::HartiiAmm, Venue::Legacy),
            ]
        );
        assert!(venue_pairs().iter().all(|(a, b)| a != b), "a leg never crosses into its own venue");
    }

    #[test]
    fn the_deepest_pool_wins_when_a_pair_is_listed_twice() {
        let pools = vec![pool(("0xf1", "DUP"), (WQUAI, "WQUAI"), Some(10.0)), pool(("0xf1", "DUP"), (WQUAI, "WQUAI"), Some(9_000.0))];
        let g = RouteGraph::new(&pools, &hubs());
        assert_eq!(g.route("0xf1", WQUAI).unwrap().min_tvl_usd, Some(9_000.0));
    }
}
