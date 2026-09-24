//! Quainance's own indexer, for data the chain cannot answer cheaply.
//!
//! The wallet builds candles by replaying a pool's `Swap`/`Sync` logs, which costs a page walk or a
//! 10,000-block scan per pool. The subgraph has them already bucketed, so a chart is one query.
//!
//! Two rules keep this from becoming a third opinion on the truth:
//!
//! - **Chain beats indexers.** Nothing from here reaches a review. Candles are display data, the
//!   same as the explorer's prices, and carry their source.
//! - **It is a third host.** Queries go to `graph.quai.network`, so they sit behind the market-data
//!   switch. A query naming a user's address would belong behind the address-revealing switch
//!   instead; this module deliberately only asks about pools.

use crate::data::DataCtx;
use crate::error::{CoreError, Result};
use crate::markets::Candle;
use serde_json::{Value, json};

/// Candle intervals the deployed subgraph buckets, in seconds. A timeframe outside this set has to
/// be built from logs, so [`interval_for`] says so rather than silently returning the wrong shape.
pub const INTERVALS: [u64; 6] = [60, 300, 900, 1800, 3600, 14_400];

/// The indexed interval that exactly matches a timeframe, when one does.
pub fn interval_for(bucket: u64) -> Option<u64> {
    INTERVALS.contains(&bucket).then_some(bucket)
}

fn f(v: &Value) -> Option<f64> {
    match v {
        Value::String(s) => s.trim().parse().ok(),
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
    .filter(|x: &f64| x.is_finite())
}

/// Parse a `pairCandles` response into the wallet's own candle shape, oldest first.
///
/// The subgraph returns newest-first, which is the opposite of what a chart draws, so the order is
/// reversed here rather than at every call site.
pub fn parse_candles(v: &Value) -> Vec<Candle> {
    let Some(items) = v["data"]["pairCandles"].as_array() else { return Vec::new() };
    let mut out: Vec<Candle> = items
        .iter()
        .filter_map(|c| {
            let start = c["timestamp"].as_u64().or_else(|| c["timestamp"].as_str()?.parse().ok())?;
            Some(Candle {
                start,
                open: f(&c["open"])?,
                high: f(&c["high"])?,
                low: f(&c["low"])?,
                close: f(&c["close"])?,
                // The subgraph reports volume in USD; the log-built candles report it in quote
                // units. They are different measures, so the caller labels which it is showing.
                volume: f(&c["volumeUSD"]).unwrap_or(0.0),
                trades: c["txCount"].as_str().and_then(|t| t.parse().ok()).or_else(|| c["txCount"].as_u64().map(|n| n as u32)).unwrap_or(0),
            })
        })
        .collect();
    out.sort_by_key(|c| c.start);
    out.dedup_by_key(|c| c.start);
    out
}

/// The query includes an open candle, whose OHLC/volume changes during its bucket.
/// Refresh it at the market cadence rather than treating the whole answer as closed history.
/// Under the Markets tick (5 s, counted from when it asks), so every tick reads the current
/// candle. At 5 s, counted from when the answer landed, every other tick was served the old one.
fn candle_ttl(_bucket: u64, _now: u64) -> u64 {
    4
}

/// Candles for one pool, newest `count` buckets of `bucket` seconds, oldest first.
///
/// Returns `Unsupported` when this network has no subgraph, the timeframe is not indexed, or market
/// data is switched off — every one of which the caller answers by building candles from logs.
pub async fn candles(ctx: &DataCtx, pair: &str, bucket: u64, count: usize) -> Result<Vec<Candle>> {
    candles_at(ctx, pair, bucket, count, crate::http::Priority::Interactive).await
}

/// [`candles`] with an explicit priority: a chart the user is looking at is interactive, and
/// anything fetched ahead of them is background so it yields when the host is close to its limit.
pub async fn candles_at(ctx: &DataCtx, pair: &str, bucket: u64, count: usize, priority: crate::http::Priority) -> Result<Vec<Candle>> {
    if !ctx.policy.market {
        return Err(CoreError::NotFound("market data is off".into()));
    }
    let base = ctx
        .network
        .ecosystem
        .quainance_subgraph
        .clone()
        .ok_or_else(|| CoreError::NotFound(format!("no subgraph on {}", ctx.network.name)))?;
    let interval = interval_for(bucket).ok_or_else(|| CoreError::NotFound(format!("{bucket}s candles are not indexed")))?;
    let key = format!("subgraph_candles:{}:{interval}:{count}:{}", pair.to_lowercase(), source_set_digest(&base, &[]));
    let query = json!({
        "query": "query($pair: String!, $interval: Int!, $first: Int!) { pairCandles(where: {pair: $pair, interval: $interval}, orderBy: timestamp, orderDirection: desc, first: $first) { timestamp open high low close volumeUSD txCount } }",
        "variables": { "pair": pair.to_lowercase(), "interval": interval, "first": count.clamp(1, 1000) },
    });
    // The current candle changes before its bucket closes.
    let board = ctx
        .cached(&key, candle_ttl(interval, crate::registry::now()), || async move {
            let body = crate::http::post_json_with(&base, &query, priority).await?;
            if let Some(errors) = body["errors"].as_array().filter(|e| !e.is_empty()) {
                return Err(CoreError::Network(format!(
                    "subgraph: {}",
                    crate::explorer::clean_text(errors[0]["message"].as_str().unwrap_or("query failed"))
                )));
            }
            Ok(parse_candles(&body))
        })
        .await?;
    Ok(board.value)
}

/// Each pair's token1-per-token0 price as of 24 hours ago: the reserves in its last hourly bucket
/// at or before then. One request, one aliased subquery per pair; a pair with no bucket that old
/// (younger than a day, or never traded) is left out.
pub async fn spot_24h_ago(ctx: &DataCtx, pairs: &[String]) -> Result<std::collections::HashMap<String, f64>> {
    if !ctx.policy.market || pairs.is_empty() {
        return Err(CoreError::NotFound("market data is off".into()));
    }
    let base = ctx
        .network
        .ecosystem
        .quainance_subgraph
        .clone()
        .ok_or_else(|| CoreError::NotFound(format!("no subgraph on {}", ctx.network.name)))?;
    // The reference hour moves once an hour; the key follows it so a cached answer is never stale
    // by more than that.
    let hour = (crate::registry::now().saturating_sub(86_400)) / 3_600 * 3_600;
    let mut pairs: Vec<String> = pairs.iter().map(|p| p.to_lowercase()).collect();
    pairs.sort();
    pairs.dedup();
    let key = format!("subgraph_spot_24h:{hour}:{}", source_set_digest(&base, &pairs));
    let board = ctx
        .cached(&key, 600, || async move {
            use futures::StreamExt;
            let reads = pairs.chunks(100).map(|chunk| {
                let base = &base;
                async move {
                    let query = day_ago_query(chunk, hour);
                    let body = crate::http::post_json_with(base, &json!({ "query": query }), crate::http::Priority::Background).await?;
                    if let Some(errors) = body["errors"].as_array().filter(|e| !e.is_empty()) {
                        return Err(CoreError::Network(format!(
                            "subgraph: {}",
                            crate::explorer::clean_text(errors[0]["message"].as_str().unwrap_or("query failed"))
                        )));
                    }
                    Ok(parse_day_ago(&body, chunk))
                }
            });
            let chunks: Vec<Result<std::collections::HashMap<String, f64>>> =
                futures::stream::iter(reads).buffer_unordered(3).collect().await;
            let mut prices = std::collections::HashMap::new();
            for chunk in chunks {
                prices.extend(chunk?);
            }
            Ok(prices)
        })
        .await?;
    Ok(board.value)
}

fn source_set_digest(source: &str, pairs: &[String]) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    for item in std::iter::once(source).chain(pairs.iter().map(String::as_str)) {
        hash.update((item.len() as u64).to_be_bytes());
        hash.update(item.as_bytes());
    }
    hex::encode(hash.finalize())
}

fn day_ago_query(pairs: &[String], at: u64) -> String {
    let parts: String = pairs
        .iter()
        .enumerate()
        .map(|(i, p)| {
            format!(
                "p{i}: pairHourDatas(first: 1, where: {{pair: \"{p}\", hourStartUnix_lte: {at}}}, orderBy: hourStartUnix, orderDirection: desc) {{ reserve0 reserve1 }} "
            )
        })
        .collect();
    format!("{{ {parts}}}")
}

fn parse_day_ago(body: &serde_json::Value, pairs: &[String]) -> std::collections::HashMap<String, f64> {
    let num = |v: &serde_json::Value| v.as_str().and_then(|s| s.parse::<f64>().ok());
    pairs
        .iter()
        .enumerate()
        .filter_map(|(i, p)| {
            let row = body["data"][format!("p{i}")].get(0)?;
            let (r0, r1) = (num(&row["reserve0"])?, num(&row["reserve1"])?);
            let price = r1 / r0;
            (r0 > 0.0 && price.is_finite() && price > 0.0).then(|| (p.clone(), price))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_day_ago_price_comes_from_the_last_bucket_before_it() {
        let pairs = vec!["0xaa".to_string(), "0xbb".to_string(), "0xcc".to_string()];
        let q = day_ago_query(&pairs, 1_000);
        assert!(q.contains("p1: pairHourDatas(first: 1, where: {pair: \"0xbb\", hourStartUnix_lte: 1000}"), "{q}");
        let body = json!({ "data": {
            "p0": [{ "reserve0": "100", "reserve1": "250" }],
            "p1": [],
            "p2": [{ "reserve0": "0", "reserve1": "5" }],
        }});
        let got = parse_day_ago(&body, &pairs);
        assert_eq!(got.get("0xaa"), Some(&2.5));
        assert!(!got.contains_key("0xbb"), "no bucket that old: no change shown");
        assert!(!got.contains_key("0xcc"), "an empty pool has no price");
    }

    #[test]
    fn only_indexed_intervals_are_offered() {
        // The wallet's own timeframes: 15m, 1h and 4h are indexed; a day is not.
        assert_eq!(interval_for(900), Some(900));
        assert_eq!(interval_for(3_600), Some(3_600));
        assert_eq!(interval_for(14_400), Some(14_400));
        assert_eq!(interval_for(86_400), None, "daily candles must fall back to logs");
        assert_eq!(interval_for(0), None);
    }

    #[test]
    fn open_candles_expire_before_the_bucket_closes() {
        // Under the Markets tick, which is counted from the ask while this is counted from the
        // answer: equal to it, every other tick got the old candle.
        for bucket in [0, 60, 300, 3600, 14400] {
            assert!(candle_ttl(bucket, 1_800_000_001) < crate::markets::MARKET_TICK_SECS);
        }
    }

    #[test]
    fn reference_cache_identity_includes_source_and_addresses() {
        let a = vec!["0xaa".into(), "0xbb".into()];
        let b = vec!["0xaa".into(), "0xcc".into()];
        assert_ne!(source_set_digest("index-a", &a), source_set_digest("index-a", &b));
        assert_ne!(source_set_digest("index-a", &a), source_set_digest("index-b", &a));
    }

    #[test]
    fn candles_parse_oldest_first_and_survive_junk() {
        let body = json!({"data": {"pairCandles": [
            {"timestamp": 200, "open": "2", "high": "3", "low": "1", "close": "2.5", "volumeUSD": "10", "txCount": "4"},
            {"timestamp": 100, "open": "1", "high": "2", "low": "0.5", "close": "2", "volumeUSD": "5", "txCount": "2"},
        ]}});
        let cs = parse_candles(&body);
        assert_eq!(cs.len(), 2);
        assert_eq!(cs[0].start, 100, "the subgraph answers newest first; charts draw oldest first");
        assert_eq!((cs[1].open, cs[1].high, cs[1].low, cs[1].close), (2.0, 3.0, 1.0, 2.5));
        assert_eq!((cs[1].volume, cs[1].trades), (10.0, 4));
        // A row missing a price is dropped rather than drawn as a zero candle.
        let partial = json!({"data": {"pairCandles": [
            {"timestamp": 300, "open": "1", "high": "2", "low": "1"},
            {"timestamp": 400, "open": "1", "high": "2", "low": "1", "close": "1.5", "volumeUSD": "0", "txCount": "0"},
        ]}});
        assert_eq!(parse_candles(&partial).len(), 1);
        // Nothing at all, and a shape that is not a candle list.
        assert!(parse_candles(&json!({"data": {"pairCandles": []}})).is_empty());
        assert!(parse_candles(&json!({"errors": [{"message": "boom"}]})).is_empty());
        assert!(parse_candles(&Value::Null).is_empty());
    }

    #[test]
    fn duplicate_buckets_collapse() {
        let body = json!({"data": {"pairCandles": [
            {"timestamp": 100, "open": "1", "high": "2", "low": "1", "close": "2", "volumeUSD": "1", "txCount": "1"},
            {"timestamp": 100, "open": "1", "high": "2", "low": "1", "close": "2", "volumeUSD": "1", "txCount": "1"},
        ]}});
        assert_eq!(parse_candles(&body).len(), 1, "one bucket, one candle");
    }
}
