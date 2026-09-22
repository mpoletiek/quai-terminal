//! Portfolio: every token holding with a USD price, a USD total, 7-day value history, trust
//! markers and an NFT summary. USD figures are display estimates from third-party data and never
//! feed amount arithmetic. NFTs are counted separately and never added to the total.

use crate::amount::{self, QI_DECIMALS, QUAI_DECIMALS};
use crate::data::{DataCtx, READ_CALLER};
use crate::error::Result;
use crate::explorer::{Holding, PriceBoard, TokenKind, TokenMarket};
use crate::registry::now;
use quai_sdk::U256;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// Where a price came from.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PriceKind {
    /// Market quote (exchange or pool).
    Market,
    /// Protocol-derived (Qi).
    Protocol,
    /// No price.
    None,
}

/// Contract trust marker.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Trust {
    /// Native asset or curated contract (network profile), or explorer-verified source.
    Verified,
    /// Unverified contract: never hidden, never auto-trusted.
    Unverified,
    /// Not checked (explorer lookups off).
    Unknown,
}

/// Identity of a holding.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case", tag = "type", content = "address")]
pub enum AssetKey {
    /// Native QUAI.
    Quai,
    /// Native Qi.
    Qi,
    /// ERC-20 contract (lowercase).
    Token(String),
}

impl AssetKey {
    /// Stable id string (`quai`, `qi`, `0x…`).
    pub fn id(&self) -> String {
        match self {
            AssetKey::Quai => "quai".into(),
            AssetKey::Qi => "qi".into(),
            AssetKey::Token(a) => a.clone(),
        }
    }
}

/// One row of the portfolio table.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AssetRow {
    /// Identity.
    pub key: AssetKey,
    /// Symbol (untrusted for tokens).
    pub symbol: String,
    /// Name (untrusted for tokens).
    pub name: String,
    /// Balance in base units.
    pub balance: String,
    /// Decimals.
    pub decimals: u8,
    /// Balance was read on-chain (false: indexer value, possibly rounded).
    pub exact: bool,
    /// USD per whole unit.
    pub price_usd: Option<f64>,
    /// Price provenance kind.
    pub price_kind: PriceKind,
    /// Price source text.
    pub price_source: String,
    /// When the price was observed (unix seconds).
    pub price_at: u64,
    /// USD value of the balance.
    pub value_usd: Option<f64>,
    /// Share of the priced total, 0..=1.
    pub allocation: f64,
    /// 24h change in percent (market-cap proxy), when known.
    pub change_24h: Option<f64>,
    /// Icon URL, when known.
    pub icon_url: Option<String>,
    /// Trust marker.
    pub trust: Trust,
    /// Holder count.
    pub holders: Option<u64>,
}

impl AssetRow {
    /// Balance as U256.
    pub fn amount(&self) -> U256 {
        U256::from_str_radix(&self.balance, 10).unwrap_or_default()
    }
}

/// NFT counts (never valued in the total).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct NftSummary {
    /// Items held (indexer count).
    pub items: u64,
    /// Distinct collections.
    pub collections: u64,
}

/// A point on the value history.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ValuePoint {
    /// Unix seconds.
    pub at: u64,
    /// USD value.
    pub usd: f64,
}

/// The whole portfolio.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Portfolio {
    /// Network id.
    pub network: String,
    /// Rows sorted by value, then unpriced rows by symbol.
    pub rows: Vec<AssetRow>,
    /// Sum of priced token values (USD).
    pub total_usd: f64,
    /// Rows without a price.
    pub unpriced: usize,
    /// 7-day value series (QUAI balance changes at today's prices, other holdings constant).
    pub history: Vec<ValuePoint>,
    /// 7-day change in percent.
    pub change_7d: Option<f64>,
    /// NFTs held (not in the total).
    pub nfts: NftSummary,
    /// QUAI/Qi prices used.
    pub prices: Option<PriceBoard>,
    /// Data sources consulted.
    pub sources: Vec<String>,
    /// Non-fatal problems (explorer down, stale data).
    pub notices: Vec<String>,
    /// Some inputs were served stale from cache.
    pub stale: bool,
    /// Build time (unix seconds).
    pub observed_at: u64,
}

/// Inputs the wallet already knows (exact, from the node and local stores).
#[derive(Clone, Debug, Default)]
pub struct Known {
    /// Quai account addresses.
    pub owners: Vec<String>,
    /// Total QUAI across accounts (base units).
    pub quai: U256,
    /// Total Qi (Qits), when the wallet has Qi.
    pub qi: Option<U256>,
    /// Tokens imported locally (address, symbol, name, decimals) with exact balances summed.
    pub tokens: Vec<(String, String, String, u8, U256)>,
}

/// Contracts considered verified without asking the explorer (native wrappers and pinned USDT).
pub fn curated(ctx: &DataCtx) -> Vec<String> {
    curated_addresses(&ctx.network)
}

/// Curated contract addresses (lowercase) for a network profile.
pub fn curated_addresses(network: &crate::network::NetworkProfile) -> Vec<String> {
    [network.wqi.clone(), network.wquai.clone(), network.ecosystem.usdt.as_ref().map(|c| c.address.clone())]
        .into_iter()
        .flatten()
        .map(|a| a.to_lowercase())
        .collect()
}

const MAX_TOKENS: usize = 60;

/// Build the portfolio. Explorer failures degrade to chain-only data with a notice.
pub async fn build(ctx: &DataCtx, known: &Known) -> Result<Portfolio> {
    let mut p = Portfolio { network: ctx.network.id.clone(), observed_at: now(), ..Portfolio::default() };
    let explorer = &ctx.explorer;
    let source = explorer.source();

    // Prices, token markets, holdings and value history are independent lookups (often on slow
    // explorer endpoints): start them together.
    let market_on = ctx.policy.market;
    let lookups_on = ctx.policy.explorer && explorer.backend != crate::explorer::Backend::ChainOnly;
    let (prices, token_markets, holdings, histories) = tokio::join!(
        async { if market_on { Some(ctx.cached("prices", 60, || explorer.prices()).await) } else { None } },
        async { if market_on { Some(ctx.cached("token_markets", 300, || explorer.token_markets()).await) } else { None } },
        futures_join(known.owners.iter().map(|owner| async move {
            if lookups_on {
                Some(ctx.cached(&format!("holdings:{}", owner.to_lowercase()), 120, || explorer.holdings(owner)).await)
            } else {
                None
            }
        })),
        futures_join(known.owners.iter().map(|owner| async move {
            if ctx.policy.explorer {
                Some(ctx.cached(&format!("history:{}", owner.to_lowercase()), 600, || explorer.balance_history(owner)).await)
            } else {
                None
            }
        })),
    );

    // Prices (market data switch).
    let mut markets: HashMap<String, TokenMarket> = HashMap::new();
    if let Some(prices) = prices {
        match prices {
            Ok(c) => {
                p.stale |= c.stale;
                p.prices = Some(c.value);
                push_source(&mut p.sources, &source);
            }
            Err(e) if is_unsupported(&e) => {}
            Err(e) => p.notices.push(format!("prices unavailable: {e}")),
        }
    }
    if let Some(token_markets) = token_markets {
        match token_markets {
            Ok(c) => {
                p.stale |= c.stale;
                markets = c.value.into_iter().map(|m| (m.address.clone(), m)).collect();
            }
            Err(e) if is_unsupported(&e) => {}
            Err(e) => p.notices.push(format!("token prices unavailable: {e}")),
        }
    }

    // Holdings: explorer discovery (address lookups switch) merged with local tokens.
    let mut discovered: BTreeMap<String, Holding> = BTreeMap::new();
    let mut nft_items = 0u64;
    let mut nft_collections = std::collections::BTreeSet::new();
    {
        for held in holdings.into_iter().flatten() {
            match held {
                Ok(c) => {
                    p.stale |= c.stale;
                    push_source(&mut p.sources, &source);
                    for h in c.value {
                        match h.kind {
                            TokenKind::Erc20 => {
                                let entry =
                                    discovered.entry(h.token.clone()).or_insert_with(|| Holding { balance: U256::ZERO, ..h.clone() });
                                entry.balance = entry.balance.saturating_add(h.balance);
                            }
                            _ => {
                                nft_items += u64::try_from(h.balance).unwrap_or(u64::MAX).min(1_000_000);
                                nft_collections.insert(h.token.clone());
                            }
                        }
                    }
                }
                Err(e) => p.notices.push(format!("holdings lookup failed: {e}")),
            }
        }
    }
    p.nfts = NftSummary { items: nft_items, collections: nft_collections.len() as u64 };

    let curated = curated(ctx);
    let price_board = p.prices.clone();

    // Native rows.
    let quai_price = price_board.as_ref().and_then(|b| b.quai_usd);
    p.rows.push(row(
        AssetKey::Quai,
        "QUAI",
        "Quai",
        known.quai,
        QUAI_DECIMALS,
        true,
        quai_price,
        if quai_price.is_some() { PriceKind::Market } else { PriceKind::None },
        price_board.as_ref().map(|b| format!("{} via {source}", b.quai_source)).unwrap_or_default(),
        price_board.as_ref().map_or(0, |b| b.taken_at),
        None,
        None,
        Trust::Verified,
        None,
    ));
    if let Some(qits) = known.qi.filter(|q| !q.is_zero()) {
        let qi_price = price_board.as_ref().and_then(|b| b.qi_usd);
        p.rows.push(row(
            AssetKey::Qi,
            "Qi",
            "Qi",
            qits,
            QI_DECIMALS,
            true,
            qi_price,
            if qi_price.is_some() { PriceKind::Protocol } else { PriceKind::None },
            price_board.as_ref().map(|b| format!("{} via {source}", b.qi_source)).unwrap_or_default(),
            price_board.as_ref().map_or(0, |b| b.taken_at),
            None,
            None,
            Trust::Verified,
            None,
        ));
    }

    // Token rows: local tokens (exact balances) plus discovered ones (re-read on-chain).
    // address → (symbol, name, decimals, balance, exact, icon)
    type TokenEntry = (String, String, u8, U256, bool, Option<String>);
    let mut tokens: BTreeMap<String, TokenEntry> = BTreeMap::new();
    for (address, symbol, name, decimals, balance) in &known.tokens {
        tokens.insert(address.to_lowercase(), (symbol.clone(), name.clone(), *decimals, *balance, true, None));
    }
    for (address, h) in &discovered {
        if tokens.contains_key(address) {
            if let Some(t) = tokens.get_mut(address) {
                t.5 = h.icon_url.clone();
            }
            continue;
        }
        let Some(decimals) = h.decimals else { continue };
        tokens.insert(address.clone(), (h.symbol.clone(), h.name.clone(), decimals, h.balance, false, h.icon_url.clone()));
    }
    for (index, (address, (symbol, name, decimals, balance, exact, icon))) in tokens.into_iter().enumerate() {
        if index >= MAX_TOKENS {
            p.notices.push(format!("showing the first {MAX_TOKENS} tokens"));
            break;
        }
        // Chain beats indexers: re-read discovered balances on-chain.
        let (balance, exact) = if exact {
            (balance, true)
        } else {
            let mut sum = U256::ZERO;
            let mut ok = true;
            for owner in &known.owners {
                match ctx.erc20_balance(&address, owner).await {
                    Ok(b) => sum = sum.saturating_add(b),
                    Err(_) => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok { (sum, true) } else { (balance, false) }
        };
        if balance.is_zero() {
            continue;
        }
        let market = markets.get(&address);
        let price = market.and_then(|m| m.price_usd);
        let trust = if curated.contains(&address) {
            Trust::Verified
        } else if ctx.policy.explorer && explorer.backend != crate::explorer::Backend::ChainOnly {
            match ctx.cached(&format!("verified:{address}"), 86_400, || explorer.contract_verified(&address)).await {
                Ok(c) if c.value => Trust::Verified,
                Ok(_) => Trust::Unverified,
                Err(_) => Trust::Unknown,
            }
        } else {
            Trust::Unknown
        };
        let icon = if ctx.policy.icons { market.and_then(|m| m.icon_url.clone()).or(icon) } else { None };
        p.rows.push(row(
            AssetKey::Token(address.clone()),
            &market.map(|m| m.symbol.clone()).filter(|s| !s.is_empty()).unwrap_or(symbol),
            &market.map(|m| m.name.clone()).filter(|s| !s.is_empty()).unwrap_or(name),
            balance,
            decimals,
            exact,
            price,
            if price.is_some() { PriceKind::Market } else { PriceKind::None },
            market.map(|m| format!("{} via {source}", m.price_source)).unwrap_or_default(),
            market.map_or(0, |m| m.price_at),
            market.and_then(|m| m.cap_growth_24h),
            icon,
            trust,
            market.and_then(|m| m.holders),
        ));
    }

    // Totals and allocation.
    p.total_usd = p.rows.iter().filter_map(|r| r.value_usd).sum();
    p.unpriced = p.rows.iter().filter(|r| r.value_usd.is_none() && !r.amount().is_zero()).count();
    for r in &mut p.rows {
        r.allocation = match (r.value_usd, p.total_usd > 0.0) {
            (Some(v), true) => (v / p.total_usd).clamp(0.0, 1.0),
            _ => 0.0,
        };
    }
    p.rows.sort_by(|a, b| {
        b.value_usd
            .unwrap_or(-1.0)
            .partial_cmp(&a.value_usd.unwrap_or(-1.0))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.symbol.to_lowercase().cmp(&b.symbol.to_lowercase()))
    });

    // Value history (explorer): reconstruct QUAI balances backwards from the exact current total.
    if ctx.policy.explorer
        && let Some(price) = quai_price
    {
        let mut changes = Vec::new();
        let mut ok = true;
        for history in histories.into_iter().flatten() {
            match history {
                Ok(c) => changes.extend(c.value.into_iter().filter(|h| h.coin.eq_ignore_ascii_case("QUAI"))),
                Err(e) if is_unsupported(&e) => {
                    ok = false;
                    break;
                }
                Err(_) => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            let other: f64 = p.rows.iter().filter(|r| r.key != AssetKey::Quai).filter_map(|r| r.value_usd).sum();
            p.history = value_series(known.quai, &changes, price, other, now(), 7 * 86_400, 28);
            if let (Some(first), Some(last)) = (p.history.first(), p.history.last())
                && first.usd > 0.0
            {
                p.change_7d = Some((last.usd - first.usd) / first.usd * 100.0);
            }
        }
    }
    let _ = READ_CALLER;
    Ok(p)
}

fn is_unsupported(e: &crate::CoreError) -> bool {
    matches!(e, crate::CoreError::NotFound(m) if m.contains("not available from"))
}

fn push_source(sources: &mut Vec<String>, s: &str) {
    if !sources.iter().any(|x| x == s) {
        sources.push(s.to_string());
    }
}

#[allow(clippy::too_many_arguments)]
fn row(
    key: AssetKey,
    symbol: &str,
    name: &str,
    balance: U256,
    decimals: u8,
    exact: bool,
    price: Option<f64>,
    price_kind: PriceKind,
    price_source: String,
    price_at: u64,
    change_24h: Option<f64>,
    icon_url: Option<String>,
    trust: Trust,
    holders: Option<u64>,
) -> AssetRow {
    let value = price.map(|p| amount::to_f64(balance, decimals) * p);
    AssetRow {
        key,
        symbol: symbol.to_string(),
        name: name.to_string(),
        balance: balance.to_string(),
        decimals,
        exact,
        price_usd: price,
        price_kind,
        price_source,
        price_at,
        value_usd: value,
        allocation: 0.0,
        change_24h,
        icon_url,
        trust,
        holders,
    }
}

/// Sample a QUAI balance backwards through signed changes into `points` evenly spaced USD values
/// over `window` seconds ending at `now`, adding a constant `other` USD.
pub fn value_series(
    current: U256,
    changes: &[crate::explorer::BalanceChange],
    price: f64,
    other: f64,
    now: u64,
    window: u64,
    points: usize,
) -> Vec<ValuePoint> {
    let points = points.max(2);
    let start = now.saturating_sub(window);
    let mut sorted: Vec<&crate::explorer::BalanceChange> = changes.iter().filter(|c| c.timestamp > start).collect();
    sorted.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    let current_f = amount::to_f64(current, QUAI_DECIMALS);
    let mut series = Vec::with_capacity(points);
    for i in 0..points {
        let at = start + window * i as u64 / (points as u64 - 1);
        // Balance at `at` = current − Σ deltas after `at`.
        let after: f64 = sorted
            .iter()
            .filter(|c| c.timestamp > at)
            .map(|c| {
                let (neg, digits) = c.delta.strip_prefix('-').map_or((false, c.delta.as_str()), |d| (true, d));
                let v = U256::from_str_radix(digits, 10).map(|u| amount::to_f64(u, QUAI_DECIMALS)).unwrap_or(0.0);
                if neg { -v } else { v }
            })
            .sum();
        let balance = (current_f - after).max(0.0);
        series.push(ValuePoint { at, usd: balance * price + other });
    }
    series
}

impl crate::session::Session {
    /// A read-only data context for this wallet and network, for display.
    pub fn data_ctx(&self) -> Result<DataCtx> {
        self.data_ctx_at(crate::data::Trust::Cached)
    }

    /// The same, at a stated trust. [`crate::data::Trust::FirstHand`] is what a review preparation
    /// path asks for: no cached answer reaches it and none is written from it.
    pub fn data_ctx_at(&self, trust: crate::data::Trust) -> Result<DataCtx> {
        let mut ctx = DataCtx::open(self.registry.paths(), &self.meta.id, self.network.clone(), &self.config)?;
        // The same node the session reads from: quotes, pools, listings and the board read from the
        // monitoring node too once the session has verified it.
        if self.monitoring() {
            ctx.node = self.node.clone();
            ctx.monitored = true;
        }
        Ok(if trust.may_cache() { ctx } else { ctx.for_review() })
    }

    /// Exact balances the portfolio starts from: QUAI and token balances from the node, Qi from
    /// the local snapshot (`refresh_qi` re-reads known addresses first).
    pub async fn portfolio_known(&mut self, refresh_qi: bool) -> Result<Known> {
        let accounts = self.quai_balances().await?;
        let quai = accounts.iter().fold(U256::ZERO, |sum, a| sum.saturating_add(a.balance));
        let has_qi = self.meta.qi_xpub.is_some() || !self.meta.qi_imported.is_empty();
        if has_qi && refresh_qi {
            let _ = self.refresh_qi().await;
        }
        let qi = if has_qi { self.qi_summary().ok().map(|s| s.balance.total) } else { None };
        let mut tokens: BTreeMap<String, (String, String, u8, U256)> = BTreeMap::new();
        for account in &accounts {
            for b in self.token_balances(Some(&account.address)).await.unwrap_or_default() {
                let e = tokens.entry(b.token.address.to_lowercase()).or_insert((
                    b.token.symbol.clone(),
                    b.token.name.clone(),
                    b.token.decimals,
                    U256::ZERO,
                ));
                e.3 = e.3.saturating_add(b.balance);
            }
        }
        Ok(Known {
            owners: accounts.iter().map(|a| a.address.clone()).collect(),
            quai,
            qi,
            tokens: tokens.into_iter().map(|(a, (s, n, d, b))| (a, s, n, d, b)).collect(),
        })
    }
}

/// Await every future (in order of the input) concurrently.
async fn futures_join<F: std::future::Future>(futures: impl IntoIterator<Item = F>) -> Vec<F::Output> {
    let mut pending: Vec<std::pin::Pin<Box<F>>> = futures.into_iter().map(Box::pin).collect();
    let mut out: Vec<Option<F::Output>> = pending.iter().map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut done = true;
        for (slot, fut) in out.iter_mut().zip(pending.iter_mut()) {
            if slot.is_none() {
                match fut.as_mut().poll(cx) {
                    std::task::Poll::Ready(v) => *slot = Some(v),
                    std::task::Poll::Pending => done = false,
                }
            }
        }
        if done { std::task::Poll::Ready(()) } else { std::task::Poll::Pending }
    })
    .await;
    out.into_iter().flatten().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer::BalanceChange;

    #[test]
    fn value_series_walks_back_from_the_current_balance() {
        let quai = |n: u128| U256::from(n * 10u128.pow(18));
        let now = 10_000_000;
        let changes = vec![
            BalanceChange { timestamp: now - 3600, block: 3, delta: (10u128.pow(18) * 40).to_string(), coin: "QUAI".into() },
            BalanceChange { timestamp: now - 5 * 86_400, block: 2, delta: format!("-{}", 10u128.pow(18) * 10), coin: "QUAI".into() },
            BalanceChange { timestamp: now - 30 * 86_400, block: 1, delta: (10u128.pow(18) * 999).to_string(), coin: "QUAI".into() },
        ];
        let s = value_series(quai(100), &changes, 2.0, 5.0, now, 7 * 86_400, 8);
        assert_eq!(s.len(), 8);
        // Seven days ago: 100 − 40 + 10 = 70 QUAI → $140 + $5.
        assert!((s[0].usd - 145.0).abs() < 1e-6, "{}", s[0].usd);
        // Now: exactly the current balance.
        assert!((s[7].usd - 205.0).abs() < 1e-6, "{}", s[7].usd);
        assert!(s.windows(2).all(|w| w[0].at < w[1].at));
    }

    #[tokio::test]
    async fn offline_portfolio_uses_only_known_balances() {
        let network = crate::network::NetworkProfile::builtins()[0].clone();
        let ctx = DataCtx::with_app(crate::appdb::AppDb::memory().unwrap(), network, crate::config::DataPolicy::OFFLINE).unwrap();
        let known = Known {
            owners: vec!["0x002360Bc8E2A359bE7335B06De43F1c7F040f15a".into()],
            quai: U256::from(5u128 * 10u128.pow(18)),
            qi: Some(U256::from(1500)),
            tokens: vec![("0x002b2596ecf05c93a31ff916e8b456df6c77c750".into(), "WQI".into(), "Wrapped Qi".into(), 18, U256::from(7))],
        };
        let p = build(&ctx, &known).await.unwrap();
        assert_eq!(p.rows.len(), 3);
        assert_eq!(p.total_usd, 0.0);
        assert_eq!(p.unpriced, 3);
        assert!(p.history.is_empty() && p.prices.is_none());
        let wqi = p.rows.iter().find(|r| r.symbol == "WQI").unwrap();
        assert_eq!(wqi.trust, Trust::Verified, "curated wrapper");
        assert!(wqi.exact);
    }
}
