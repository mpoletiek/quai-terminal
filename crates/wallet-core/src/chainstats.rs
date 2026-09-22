//! Network statistics for System › Network: hashrate per algorithm, transactions and gas over
//! time, and the chain's running totals, from explorer.qu.ai.
//!
//! Display data only, and the same for every wallet — nothing here names an address — so it is a
//! shared feed (`chain_stats`), fetched once per data directory. The node's own gas price stays
//! the live figure; these are the history behind it.

use crate::data::DataCtx;
use crate::error::{CoreError, Result};
use crate::explorer::parse_timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Hashrate (hashes per second) for each of Quai's three proof-of-work algorithms.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Hashrates {
    pub sha: f64,
    pub scrypt: f64,
    pub kawpow: f64,
}

/// One closed UTC hour.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Hour {
    /// Unix seconds at the start of the hour.
    pub at: u64,
    pub blocks: u64,
    /// Transactions executed in the hour: QUAI, Qi and inbound cross-zone (ETX), as the explorer
    /// counts `totalTransactions`.
    pub transactions: u64,
    pub quai_transactions: u64,
    pub qi_transactions: u64,
    /// The chain's running transaction total at the end of the hour.
    pub total_transactions: Option<u64>,
    pub gas_used: f64,
    /// Fees paid in the hour, in QUAI-equivalent wei.
    pub fees_wei: f64,
}

impl Hour {
    /// What a unit of gas actually cost on average this hour, in gwei: fees paid over gas used.
    /// This is the price people paid, which is what "gas price" means when deciding a fee; the
    /// node's `quai_gasPrice` is its suggestion for the next block.
    pub fn avg_gas_price_gwei(&self) -> Option<f64> {
        (self.gas_used > 0.0).then(|| self.fees_wei / self.gas_used / 1e9)
    }
}

/// Everything the Network screen draws beyond the node's own health.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ChainStats {
    /// When the explorer last observed it (unix seconds).
    pub observed_at: u64,
    pub avg_block_secs: Option<f64>,
    pub total_transactions: Option<u64>,
    pub quai_addresses: Option<u64>,
    pub qi_addresses: Option<u64>,
    /// Current hashrate, over the explorer's recent window.
    pub hashrate: Hashrates,
    /// Hourly hashrate, oldest first (the last 24 hours).
    pub hashrate_history: Vec<(u64, Hashrates)>,
    /// Hourly activity, oldest first (the last 48 hours).
    pub hours: Vec<Hour>,
    /// Estimated QUAI reward for a block, in QUAI.
    pub block_reward_quai: Option<f64>,
}

/// Hours of transaction and gas history the screen shows.
pub const HISTORY_HOURS: u32 = 48;

/// Network statistics, cached five minutes: the hourly buckets close once an hour and the current
/// hashrate is a trailing average, so nothing here moves faster than that is worth paying for.
pub async fn chain_stats(ctx: &DataCtx) -> Result<crate::data::Cached<ChainStats>> {
    if !ctx.policy.market || ctx.explorer.source() != "explorer.qu.ai" {
        return Err(CoreError::NotFound("network statistics come from explorer.qu.ai (market data, mainnet)".into()));
    }
    let url = |path: &str| ctx.explorer.absolute(path);
    let (network, hashrate, mining, hourly) = (
        url("/api/stats/network"),
        url("/api/stats/hashrate"),
        url("/api/mining/summary?minutes=1440"),
        url(&format!("/api/stats/hourly?hours={HISTORY_HOURS}")),
    );
    ctx.cached("chain_stats", 300, || async move {
        use crate::http::get_json;
        // Four independent reads; the screen waits on the slowest, not on all of them in series.
        let (network, hashrate, mining, hourly) =
            futures::future::join4(get_json(&network), get_json(&hashrate), get_json(&mining), get_json(&hourly)).await;
        // A partial answer is still worth showing; nothing at all is an error.
        if network.is_err() && hashrate.is_err() && hourly.is_err() {
            return Err(network.err().unwrap_or_else(|| CoreError::Network("explorer statistics unavailable".into())));
        }
        let empty = Value::Null;
        Ok(parse(
            network.as_ref().unwrap_or(&empty),
            hashrate.as_ref().unwrap_or(&empty),
            mining.as_ref().unwrap_or(&empty),
            hourly.as_ref().unwrap_or(&empty),
        ))
    })
    .await
}

/// A number the explorer may send as a JSON number or a decimal string. Absent, not zero, when it
/// is neither: a chart with a false zero in it is worse than one with a gap.
fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    }
    .filter(|f| f.is_finite() && *f >= 0.0)
}

fn int(v: &Value) -> Option<u64> {
    num(v).map(|f| f as u64)
}

fn rates(v: &Value) -> Hashrates {
    Hashrates { sha: num(&v["sha"]).unwrap_or(0.0), scrypt: num(&v["scrypt"]).unwrap_or(0.0), kawpow: num(&v["kawpow"]).unwrap_or(0.0) }
}

fn time(v: &Value) -> Option<u64> {
    v.as_str().and_then(parse_timestamp)
}

/// Pure part of [`chain_stats`].
pub fn parse(network: &Value, hashrate: &Value, mining: &Value, hourly: &Value) -> ChainStats {
    let exact = &hashrate["hashratesExact"];
    let current = if exact.is_object() { rates(exact) } else { rates(&hashrate["hashrates"]) };
    let mut hashrate_history: Vec<(u64, Hashrates)> = mining["hashrateHistory"]["series"]
        .as_array()
        .map(|series| series.iter().filter_map(|b| Some((time(&b["bucket"])?, rates(&b["hashrates"])))).collect())
        .unwrap_or_default();
    hashrate_history.sort_by_key(|(at, _)| *at);
    // The explorer sends blocks and transactions as parallel arrays keyed by the hour.
    let blocks: std::collections::HashMap<u64, &Value> =
        hourly["blocks"].as_array().map(|b| b.iter().filter_map(|x| Some((time(&x["bucket"])?, x))).collect()).unwrap_or_default();
    let mut hours: Vec<Hour> = hourly["transactions"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|t| {
                    let at = time(&t["bucket"])?;
                    let b = blocks.get(&at).copied().unwrap_or(&Value::Null);
                    let (quai, qi, etx) = (int(&t["tx_count_quai"]), int(&t["tx_count_qi"]), int(&t["tx_count_etx"]));
                    Some(Hour {
                        at,
                        blocks: int(&b["block_count"]).unwrap_or(0),
                        transactions: int(&t["tx_count_total"]).unwrap_or_else(|| quai.unwrap_or(0) + qi.unwrap_or(0) + etx.unwrap_or(0)),
                        quai_transactions: quai.unwrap_or(0),
                        qi_transactions: qi.unwrap_or(0),
                        total_transactions: int(&t["total_transactions"]),
                        gas_used: num(&b["gas_used"]).unwrap_or(0.0),
                        fees_wei: num(&b["total_fees"]).unwrap_or(0.0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    hours.sort_by_key(|h| h.at);
    let reward = &hashrate["protocolRewards"]["estimatedBlockReward"];
    ChainStats {
        observed_at: time(&hashrate["asOf"]).or_else(|| time(&hourly["asOf"])).unwrap_or_else(crate::registry::now),
        avg_block_secs: num(&network["avgBlockTime"]).or_else(|| num(&hashrate["avgBlockTime"])),
        total_transactions: int(&network["totalTransactions"]).or_else(|| hours.last().and_then(|h| h.total_transactions)),
        quai_addresses: int(&network["totalQuaiAddresses"]),
        qi_addresses: int(&network["totalQiAddresses"]),
        hashrate: current,
        hashrate_history,
        hours,
        block_reward_quai: num(reward).map(|wei| wei / 1e18),
    }
}

/// `211.2 PH/s`: a hashrate in the largest unit that keeps it above one.
pub fn hashrate_text(hashes_per_second: f64) -> String {
    const UNITS: [&str; 7] = ["H/s", "kH/s", "MH/s", "GH/s", "TH/s", "PH/s", "EH/s"];
    let mut value = hashes_per_second.max(0.0);
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Shapes captured from explorer.qu.ai on 2026-09-18, trimmed.
    #[test]
    fn statistics_decode_and_gas_is_what_was_paid() {
        let network =
            json!({"avgBlockTime": 5.25, "totalTransactions": 100701162, "totalQuaiAddresses": 90529, "totalQiAddresses": 260207});
        let hashrate = json!({"asOf": "2026-09-18T02:28:48.972Z", "avgBlockTime": 5.132,
            "hashrates": {"kawpow": 1, "sha": 1, "scrypt": 1},
            "hashratesExact": {"kawpow": "206042187140", "sha": "211219565928244369", "scrypt": "10744925004232"},
            "protocolRewards": {"estimatedBlockReward": "94556223523571989938"}});
        let mining = json!({"hashrateHistory": {"series": [
            {"bucket": "2026-09-18T01:00:00.000Z", "hashrates": {"kawpow": "227404192143", "sha": "189230772643432436", "scrypt": "10102555274633"}},
            {"bucket": "2026-09-17T02:00:00.000Z", "hashrates": {"kawpow": "1", "sha": "2", "scrypt": "3"}}
        ]}});
        let hourly = json!({"asOf": "2026-09-18T02:28:44.000Z",
            "blocks": [{"bucket": "2026-09-16T02:00:00.000Z", "block_count": "717", "gas_used": "758814166", "total_fees": "18012397371016453651129"}],
            "transactions": [{"bucket": "2026-09-16T02:00:00.000Z", "tx_count_total": "6652", "tx_count_quai": "501", "tx_count_qi": "0",
                              "tx_count_etx": "6151", "total_transactions": "100359518"}]});
        let s = parse(&network, &hashrate, &mining, &hourly);
        assert_eq!(s.total_transactions, Some(100_701_162));
        assert_eq!((s.quai_addresses, s.qi_addresses), (Some(90_529), Some(260_207)));
        assert_eq!(s.avg_block_secs, Some(5.25));
        assert_eq!(s.hashrate.sha, 211_219_565_928_244_369.0, "the exact figures win over the rounded ones");
        assert_eq!(hashrate_text(s.hashrate.sha), "211.2 PH/s");
        assert_eq!(hashrate_text(s.hashrate.scrypt), "10.7 TH/s");
        assert_eq!(hashrate_text(s.hashrate.kawpow), "206.0 GH/s");
        assert_eq!(s.hashrate_history.len(), 2);
        assert!(s.hashrate_history[0].0 < s.hashrate_history[1].0, "history is oldest first");
        let hour = &s.hours[0];
        assert_eq!((hour.blocks, hour.transactions, hour.quai_transactions, hour.total_transactions), (717, 6652, 501, Some(100_359_518)));
        // 18,012 QUAI of fees over 758.8M gas: ~23,737 gwei a unit of gas.
        let gwei = hour.avg_gas_price_gwei().unwrap();
        assert!((gwei - 23_737.5).abs() < 1.0, "{gwei}");
        assert!((s.block_reward_quai.unwrap() - 94.556).abs() < 0.001);
    }

    /// Missing pieces leave gaps, never zeros, and an empty answer is an empty (not a broken) chart.
    #[test]
    fn a_partial_answer_leaves_gaps() {
        let s = parse(&Value::Null, &Value::Null, &Value::Null, &json!({"transactions": [{"bucket": "nonsense"}]}));
        assert_eq!(s.total_transactions, None);
        assert!(s.hours.is_empty() && s.hashrate_history.is_empty());
        assert_eq!(Hour::default().avg_gas_price_gwei(), None, "no gas used is no price, not a price of zero");
        assert_eq!(num(&json!("-5")), None);
        assert_eq!(num(&json!("NaN")), None);
    }
}
