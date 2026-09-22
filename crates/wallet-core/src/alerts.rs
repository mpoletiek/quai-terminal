//! Watched pairs and alerts: a price crossing a line, a pair moving a lot in a day, gas getting
//! cheap.
//!
//! Both live in the wallet's own database (`kv`), per network. An alert fires on the edge: when
//! its condition becomes true, not on every check while it stays true, and it re-arms once the
//! condition is false again. The daemon checks them every poll; the TUI checks them itself only
//! when no daemon is running, so an alert never fires twice.

use crate::appdb::AppDb;
use crate::data::DataCtx;
use crate::error::Result;
use crate::markets::Pool;
use serde::{Deserialize, Serialize};

/// What an alert waits for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Rule {
    /// The pair's price at or above this.
    Above { price: f64 },
    /// At or below this.
    Below { price: f64 },
    /// Up or down at least this many percent over 24 hours.
    Moves { pct: f64 },
    /// The network's gas price at or below this many gwei.
    GasBelow { gwei: f64 },
}

/// One alert.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Alert {
    pub id: u64,
    /// Pool address (lowercase); empty for a gas alert.
    pub pool: String,
    /// The pair as it was named when the alert was set, base first (`WQI/QUAI`).
    pub name: String,
    /// The base is the pool's token1, so its price is token0 per token1.
    pub inverted: bool,
    pub rule: Rule,
    /// The condition held at the last check.
    #[serde(default)]
    pub active: bool,
    /// When it last fired (unix seconds; 0 never).
    #[serde(default)]
    pub fired: u64,
}

impl Alert {
    /// `WQI/QUAI above 125`.
    pub fn describe(&self) -> String {
        match &self.rule {
            Rule::Above { price } => format!("{} above {}", self.name, trim(*price)),
            Rule::Below { price } => format!("{} below {}", self.name, trim(*price)),
            Rule::Moves { pct } => format!("{} moves {}% in 24h", self.name, trim(*pct)),
            Rule::GasBelow { gwei } => format!("gas below {} gwei", trim(*gwei)),
        }
    }
}

fn trim(v: f64) -> String {
    let s = format!("{v:.8}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn alerts_key(network: &str) -> String {
    format!("alerts:{network}")
}

fn watch_key(network: &str) -> String {
    format!("watchlist:{network}")
}

pub fn load(app: &AppDb, network: &str) -> Vec<Alert> {
    app.kv(&alerts_key(network)).ok().flatten().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn save(app: &AppDb, network: &str, alerts: &[Alert]) -> Result<()> {
    app.set_kv(&alerts_key(network), &serde_json::to_string(alerts).unwrap_or_else(|_| "[]".into()))
}

/// Add an alert; returns its id.
pub fn add(app: &AppDb, network: &str, mut alert: Alert) -> Result<u64> {
    let mut all = load(app, network);
    alert.id = all.iter().map(|a| a.id).max().unwrap_or(0) + 1;
    alert.active = false;
    alert.fired = 0;
    let id = alert.id;
    all.push(alert);
    save(app, network, &all)?;
    Ok(id)
}

/// Remove an alert by id; false when there was none.
pub fn remove(app: &AppDb, network: &str, id: u64) -> Result<bool> {
    let mut all = load(app, network);
    let before = all.len();
    all.retain(|a| a.id != id);
    save(app, network, &all)?;
    Ok(all.len() < before)
}

/// Watched pools (lowercase addresses), in the order they were added.
pub fn watchlist(app: &AppDb, network: &str) -> Vec<String> {
    app.kv(&watch_key(network)).ok().flatten().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

/// Watch a pool, or stop watching it; returns whether it is watched now.
pub fn toggle_watch(app: &AppDb, network: &str, pool: &str) -> Result<bool> {
    let pool = pool.to_lowercase();
    let mut list = watchlist(app, network);
    let watched = if let Some(i) = list.iter().position(|p| *p == pool) {
        list.remove(i);
        false
    } else {
        list.push(pool);
        true
    };
    app.set_kv(&watch_key(network), &serde_json::to_string(&list).unwrap_or_else(|_| "[]".into()))?;
    Ok(watched)
}

/// A pair by name (`WQI/QUAI`, either way round, WQUAI read as QUAI) or pool address: the pool,
/// whether the named base is its token1, and the name as written back.
pub fn find_pair(pools: &[Pool], text: &str, wquai: Option<&str>) -> Option<(Pool, bool, String)> {
    let symbol = |t: &crate::markets::PoolToken| {
        if wquai.is_some_and(|w| w.eq_ignore_ascii_case(&t.address)) { "QUAI".to_string() } else { t.symbol.clone() }
    };
    let text = text.trim();
    if text.starts_with("0x") {
        let p = pools.iter().find(|p| p.address.eq_ignore_ascii_case(text))?;
        return Some((p.clone(), false, format!("{}/{}", symbol(&p.token0), symbol(&p.token1))));
    }
    let (base, quote) = text.split_once('/')?;
    let same = |t: &crate::markets::PoolToken, s: &str| symbol(t).eq_ignore_ascii_case(s) || t.symbol.eq_ignore_ascii_case(s);
    // Deepest first, so a symbol two pools share picks the one people trade.
    let mut ranked: Vec<&Pool> = pools.iter().filter(|p| p.venue != crate::markets::Venue::Curve).collect();
    ranked.sort_by(|a, b| b.tvl_usd.unwrap_or(0.0).total_cmp(&a.tvl_usd.unwrap_or(0.0)));
    ranked.into_iter().find_map(|p| {
        if same(&p.token0, base) && same(&p.token1, quote) {
            Some((p.clone(), false, format!("{}/{}", symbol(&p.token0), symbol(&p.token1))))
        } else if same(&p.token1, base) && same(&p.token0, quote) {
            Some((p.clone(), true, format!("{}/{}", symbol(&p.token1), symbol(&p.token0))))
        } else {
            None
        }
    })
}

/// Check alerts against the market and gas price; returns what fired as (title, body), and
/// updates each alert's state. An alert whose pool is not in `pools`, or a gas alert without a
/// gas price, is left as it was.
pub fn check(alerts: &mut [Alert], pools: &[Pool], gas_gwei: Option<f64>, now: u64) -> Vec<(String, String)> {
    let mut fired = Vec::new();
    for a in alerts.iter_mut() {
        let pool = pools.iter().find(|p| p.address.eq_ignore_ascii_case(&a.pool));
        let price = pool.and_then(Pool::spot_price).map(|p| if a.inverted { 1.0 / p } else { p });
        let change = pool.and_then(Pool::change_24h).map(|c| if a.inverted { (100.0 / (100.0 + c) - 1.0) * 100.0 } else { c });
        let quote = a.name.split('/').nth(1).unwrap_or_default().to_string();
        let (holds, now_text) = match a.rule {
            Rule::Above { price: line } => match price {
                Some(p) => (p >= line, format!("now {} {quote}", trim_price(p))),
                None => continue,
            },
            Rule::Below { price: line } => match price {
                Some(p) => (p <= line, format!("now {} {quote}", trim_price(p))),
                None => continue,
            },
            Rule::Moves { pct } => match change {
                Some(c) => (
                    c.abs() >= pct,
                    format!(
                        "{}{:.1}% in 24h, now {} {quote}",
                        if c >= 0.0 { "+" } else { "" },
                        c,
                        price.map(trim_price).unwrap_or_default()
                    ),
                ),
                None => continue,
            },
            Rule::GasBelow { gwei } => match gas_gwei {
                Some(g) => {
                    (g <= gwei, format!("now {} gwei", if g >= 100.0 { format!("{g:.0}") } else { trim((g * 100.0).round() / 100.0) }))
                }
                None => continue,
            },
        };
        if holds && !a.active {
            a.fired = now;
            fired.push((a.describe(), now_text));
        }
        a.active = holds;
    }
    fired
}

fn trim_price(p: f64) -> String {
    crate::amount::subscript_zeros(p, 4).unwrap_or_else(|| {
        if p >= 1000.0 { format!("{p:.2}") } else { format!("{p:.6}").trim_end_matches('0').trim_end_matches('.').to_string() }
    })
}

/// Load this wallet's alerts, check them against the (cached) market and the gas price, save
/// their state and add a notification for each that fired. Returns what fired. With `pairs` off
/// (trading turned off) only gas alerts are checked: the markets are not read, and pair alerts
/// stay as they were.
fn fresh_pools(pools: Vec<Pool>, overview: &crate::markets::DexOverview, at: u64) -> Vec<Pool> {
    pools
        .into_iter()
        .filter(|pool| {
            overview.sources.iter().any(|source| {
                let same_source = if pool.venue == crate::markets::Venue::Curve {
                    pool.curve.as_ref().and_then(|curve| curve.launchpad.as_deref()) == Some(source.source.as_str())
                } else {
                    true
                };
                source.venue == pool.venue && same_source && source.fresh_at(at)
            })
        })
        .collect()
}

pub async fn run(ctx: &DataCtx, pairs: bool) -> Result<Vec<(String, String)>> {
    let network = ctx.network.id.clone();
    let mut alerts = load(&ctx.app, &network);
    if !alerts.iter().any(|a| pairs || a.pool.is_empty()) {
        return Ok(vec![]);
    }
    let pools = if pairs && alerts.iter().any(|a| !a.pool.is_empty()) {
        crate::markets::all_markets(ctx).await.map(|(p, overview)| fresh_pools(p, &overview, crate::registry::now())).unwrap_or_default()
    } else {
        vec![]
    };
    let gas = if alerts.iter().any(|a| matches!(a.rule, Rule::GasBelow { .. })) {
        ctx.node.provider.gas_price(crate::network::ZONE).await.ok().map(|g| crate::amount::to_f64(g, 9))
    } else {
        None
    };
    let fired = check(&mut alerts, &pools, gas, crate::registry::now());
    // Write the new state onto what is stored now, not onto what was read before the network
    // calls: an alert added or removed meanwhile (the TUI, the CLI) must survive.
    let mut stored = load(&ctx.app, &network);
    for a in &mut stored {
        if let Some(checked) = alerts.iter().find(|c| c.id == a.id && c.rule == a.rule) {
            a.active = checked.active;
            a.fired = checked.fired;
        }
    }
    save(&ctx.app, &network, &stored)?;
    for (title, body) in &fired {
        let _ = ctx.app.notify("alert", title, body);
    }
    Ok(fired)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markets::PoolToken;

    fn pool(reserve0: f64, reserve1: f64, day_ago: Option<f64>) -> Pool {
        Pool {
            address: "0x00aa".into(),
            token0: PoolToken { address: "0x00b0".into(), symbol: "WQI".into(), decimals: 18 },
            token1: PoolToken { address: "0x00b1".into(), symbol: "WQUAI".into(), decimals: 18 },
            reserve0,
            reserve1,
            spot_24h_ago: day_ago,
            ..Default::default()
        }
    }

    fn alert(rule: Rule) -> Alert {
        Alert { id: 1, pool: "0x00AA".into(), name: "WQI/QUAI".into(), inverted: false, rule, active: false, fired: 0 }
    }

    #[test]
    fn stale_observations_cannot_fire_or_rearm_price_alerts() {
        let mut alerts = vec![alert(Rule::Above { price: 120.0 })];
        let stale = crate::markets::DexOverview {
            sources: vec![crate::markets::MarketSource {
                observed_at: 100,
                fetched_at: 110,
                complete: true,
                stale: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        let filtered = fresh_pools(vec![pool(1.0, 130.0, None)], &stale, 111);
        assert!(check(&mut alerts, &filtered, None, 111).is_empty());
        alerts[0].active = true;
        let filtered = fresh_pools(vec![pool(1.0, 100.0, None)], &stale, 112);
        assert!(check(&mut alerts, &filtered, None, 112).is_empty());
        assert!(alerts[0].active, "stale below-threshold data must not rearm an active alert");
    }

    #[test]
    fn an_alert_fires_once_when_its_line_is_crossed_and_rearms() {
        let mut alerts = vec![alert(Rule::Above { price: 120.0 })];
        assert!(check(&mut alerts, &[pool(1.0, 110.0, None)], None, 1).is_empty(), "below the line");
        let fired = check(&mut alerts, &[pool(1.0, 125.0, None)], None, 2);
        assert_eq!(fired, [("WQI/QUAI above 120".to_string(), "now 125 QUAI".to_string())]);
        assert!(check(&mut alerts, &[pool(1.0, 130.0, None)], None, 3).is_empty(), "still above: no repeat");
        assert!(check(&mut alerts, &[pool(1.0, 100.0, None)], None, 4).is_empty(), "back below re-arms");
        assert_eq!(check(&mut alerts, &[pool(1.0, 121.0, None)], None, 5).len(), 1, "and it fires again");
        assert_eq!(alerts[0].fired, 5);
    }

    #[test]
    fn an_inverted_pair_is_priced_the_way_it_is_shown() {
        // Shown as WQUAI/WQI: 1 WQUAI = 0.008 WQI.
        let mut a = alert(Rule::Below { price: 0.01 });
        a.inverted = true;
        a.name = "QUAI/WQI".into();
        assert_eq!(check(&mut [a], &[pool(1.0, 125.0, None)], None, 1).len(), 1);
    }

    #[test]
    fn moves_and_gas() {
        let mut moves = vec![alert(Rule::Moves { pct: 10.0 })];
        assert!(check(&mut moves, &[pool(1.0, 105.0, Some(100.0))], None, 1).is_empty(), "5% is not 10%");
        let fired = check(&mut moves, &[pool(1.0, 85.0, Some(100.0))], None, 2);
        assert!(fired[0].1.starts_with("-15.0% in 24h"), "{fired:?}");
        let mut gas = vec![Alert { pool: String::new(), name: String::new(), ..alert(Rule::GasBelow { gwei: 2.0 }) }];
        assert!(check(&mut gas, &[], None, 1).is_empty(), "no gas price read: nothing decided");
        assert_eq!(check(&mut gas, &[], Some(1.5), 2), [("gas below 2 gwei".to_string(), "now 1.5 gwei".to_string())]);
    }

    #[test]
    fn a_pair_is_found_by_name_either_way_round() {
        let pools = [pool(1.0, 120.0, None)];
        let wquai = Some("0x00b1");
        let (p, inverted, name) = find_pair(&pools, "wqi/quai", wquai).unwrap();
        assert_eq!((p.address.as_str(), inverted, name.as_str()), ("0x00aa", false, "WQI/QUAI"));
        let (_, inverted, name) = find_pair(&pools, "QUAI/WQI", wquai).unwrap();
        assert_eq!((inverted, name.as_str()), (true, "QUAI/WQI"));
        assert!(find_pair(&pools, "0x00AA", wquai).is_some());
        assert!(find_pair(&pools, "USDT/QUAI", wquai).is_none());
    }

    #[test]
    fn alerts_and_the_watchlist_persist_per_network() {
        let app = AppDb::memory().unwrap();
        let id = add(&app, "mainnet", alert(Rule::Above { price: 1.0 })).unwrap();
        let second = add(&app, "mainnet", alert(Rule::Below { price: 1.0 })).unwrap();
        assert_eq!((id, second), (1, 2));
        assert!(load(&app, "orchard").is_empty());
        assert!(remove(&app, "mainnet", 1).unwrap());
        assert_eq!(load(&app, "mainnet").iter().map(|a| a.id).collect::<Vec<_>>(), [2]);
        assert!(toggle_watch(&app, "mainnet", "0x00AA").unwrap());
        assert_eq!(watchlist(&app, "mainnet"), ["0x00aa"]);
        assert!(!toggle_watch(&app, "mainnet", "0x00aa").unwrap());
        assert!(watchlist(&app, "mainnet").is_empty());
    }
}
