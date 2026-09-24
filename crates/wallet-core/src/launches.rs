//! Quainance's launch zone: tokens launched on a bonding curve, the ones that graduated into their
//! own AMM, and the ones pooled on the main exchange.
//!
//! None of these live in the main factory's pair list until they are pooled there, so the pool
//! directory never saw them. Quainance's trade-zone indexer lists every launch with its phase,
//! price and progress; this reads it as market data (no address is sent) and keeps only
//! Quainance's own venues — the indexer also follows other launchpads on the same chain.

use crate::data::DataCtx;
use crate::error::{CoreError, Result};
use crate::explorer::clean;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The launchpads the Launches view carries: Quainance's own bonding curve, the AMM a curve
/// graduates into, the main AMM that launched tokens are pooled on, and the HartiiLabs curve.
///
/// The index knows two more — `POOP_CURVE` at two deployments, 62 launches between them — which are
/// deliberately left out until someone has looked at what is on them. Adding one is a line here.
pub const LAUNCH_VENUES: [&str; 4] = ["QUAINANCE_CURVE", "QUAINANCE_CURVE_AMM", "QUAINANCE_AMM", "HARTII_CURVE"];

fn eighteen() -> u8 {
    18
}

/// Where a launch stands.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum Phase {
    /// Trading on its bonding curve, raising toward graduation.
    Bonding,
    /// Graduated from the curve into its own AMM pair.
    Graduated,
    /// Pooled on the main Quainance AMM.
    Pooled,
    #[default]
    Other,
}

impl Phase {
    fn parse(text: &str) -> Phase {
        match text {
            "BONDING" => Phase::Bonding,
            "GRADUATED" => Phase::Graduated,
            "POOLED" => Phase::Pooled,
            _ => Phase::Other,
        }
    }

    pub fn text(self) -> &'static str {
        match self {
            Phase::Bonding => "bonding",
            Phase::Graduated => "graduated",
            Phase::Pooled => "pooled",
            Phase::Other => "—",
        }
    }
}

/// One launched token.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Launch {
    /// Token contract (lowercase).
    pub token: String,
    pub symbol: String,
    pub name: String,
    /// Token decimals (18, the launcher's own, when the index does not say).
    #[serde(default = "eighteen")]
    pub decimals: u8,
    pub phase: Phase,
    /// Which venue it trades on, as Quainance labels it.
    pub venue: String,
    /// Typed adapter identity from sourceVenue.kind or an authenticated on-chain adapter.
    #[serde(default)]
    pub venue_kind: Option<crate::capabilities::Family>,
    /// The bonding curve contract, while it has one.
    pub curve: Option<String>,
    /// The AMM pair it trades in once graduated or pooled (lowercase).
    pub pair: Option<String>,
    /// Progress toward graduation, in basis points (bonding only).
    pub progress_bps: Option<u64>,
    /// Latest price in QUAI per whole token.
    pub price_quai: Option<f64>,
    #[serde(default)]
    pub price_basis: crate::markets::PriceBasis,
    /// QUAI raised on the curve, and what graduation needs.
    pub raised_quai: f64,
    pub target_quai: Option<f64>,
    pub buys: u64,
    pub sells: u64,
    /// Unix seconds it launched.
    pub created_at: u64,
    /// The launch's metadata document, `ipfs://<cid>`, when it has one (see [`logos`]).
    #[serde(default)]
    pub metadata_uri: Option<String>,
}

impl Launch {
    /// Trades in total.
    pub fn trades(&self) -> u64 {
        self.buys + self.sells
    }
}

/// Curve buys and sells for the DEX tape, newest first.
///
/// A bonding curve is not an AMM pair. It emits nothing the pool tape decodes — the wallet only
/// ever quotes one, through `quoteBuy`/`quoteSell` view calls — so a curve trade could not appear
/// beside pool swaps however wide the log window was opened. The trade-zone index records every
/// execution on both kinds of venue, and that is where these come from.
///
/// Only `CURVE` executions are asked for. The same index carries the AMM side, but the tape
/// already reads those from the pools' own logs, and taking them from both would show every swap
/// twice — the chain is the better source for anything the chain can answer.
///
/// Display data, like the rest of the tape: nothing here reaches a review.
pub async fn curve_trades(ctx: &DataCtx, first: usize) -> Result<Vec<crate::markets::DexSwap>> {
    if !ctx.policy.market {
        return Err(CoreError::NotFound("market data is off".into()));
    }
    let url = ctx
        .network
        .ecosystem
        .launch_subgraph
        .clone()
        .ok_or_else(|| CoreError::NotFound(format!("no launch index on {}", ctx.network.name)))?;
    let first = first.clamp(1, 200);
    let query = json!({ "query": TRADES_QUERY, "variables": { "venues": LAUNCH_VENUES, "first": first } });
    let rows = ctx
        .cached(&format!("curve_trades:{first}"), TRADES_TTL, || async move {
            let body = crate::http::post_json(&url, &query).await?;
            if let Some(errors) = body["errors"].as_array().filter(|e| !e.is_empty()) {
                return Err(CoreError::Network(format!(
                    "launch index: {}",
                    clean(errors[0]["message"].as_str().unwrap_or("query failed"), 160)
                )));
            }
            Ok(parse_curve_trades(&body))
        })
        .await?;
    Ok(rows.value)
}

const TRADES_QUERY: &str = "query($venues: [String!]!, $first: Int!) { tradeExecutions(first: $first, orderBy: timestamp, orderDirection: desc, where: {kind: CURVE, venue_: {kind_in: $venues}}) { side token quoteToken quoteKind tokenAmount quoteAmount transactionHash blockNumber timestamp logIndex account sourceContract launch { tokenSymbol tokenDecimals } } }";

/// Turn the index's executions into the tape's own shape.
///
/// A row missing anything the tape needs to describe a trade — either amount, the contract it
/// happened on — is dropped rather than shown as a zero: a tape is read for what moved, and a
/// trade of nothing is noise that looks like data.
fn parse_curve_trades(body: &Value) -> Vec<crate::markets::DexSwap> {
    use crate::markets::{DexSwap, PoolToken};
    let Some(items) = body["data"]["tradeExecutions"].as_array() else { return Vec::new() };
    let int = |v: &Value| v.as_str().and_then(|s| s.parse::<u64>().ok()).or_else(|| v.as_u64());
    // The index reports amounts as atom strings, which outrun f64's exact range long before they
    // outrun its magnitude. The tape shows them to a few significant figures, so the precision
    // lost here is well below the last digit anyone reads.
    let atoms = |v: &Value, decimals: u8| -> Option<f64> {
        let raw: f64 = v.as_str()?.trim().parse().ok()?;
        let scaled = raw / 10f64.powi(i32::from(decimals));
        (scaled.is_finite() && scaled > 0.0).then_some(scaled)
    };
    items
        .iter()
        .filter_map(|e| {
            let decimals = e["launch"]["tokenDecimals"].as_str().and_then(|d| d.parse().ok()).unwrap_or(18);
            let token = PoolToken {
                address: e["token"].as_str()?.to_lowercase(),
                symbol: crate::explorer::clean(e["launch"]["tokenSymbol"].as_str().unwrap_or("?"), 32),
                decimals,
            };
            // A curve priced in native QUAI reports the wrapper's address; the tape names it QUAI,
            // which is what was actually paid and what every other row calls it.
            let native = e["quoteKind"].as_str() == Some("NATIVE_QUAI");
            let quote = PoolToken {
                address: e["quoteToken"].as_str().unwrap_or_default().to_lowercase(),
                symbol: if native { "QUAI".into() } else { "?".into() },
                decimals: 18,
            };
            let (token_amount, quote_amount) = (atoms(&e["tokenAmount"], decimals)?, atoms(&e["quoteAmount"], 18)?);
            // A buy pays the quote and receives the token; a sell is the same trade read backwards.
            let buying = e["side"].as_str()? == "BUY";
            let (token_in, amount_in, token_out, amount_out) =
                if buying { (quote, quote_amount, token, token_amount) } else { (token, token_amount, quote, quote_amount) };
            Some(DexSwap {
                // The index carries the block's real time, so unlike a log read this never needs
                // a header fetched to stop being an estimate.
                at: int(&e["timestamp"])?,
                timed: true,
                block: int(&e["blockNumber"])?,
                tx: e["transactionHash"].as_str()?.to_lowercase(),
                index: int(&e["logIndex"]).unwrap_or(0),
                pool: e["sourceContract"].as_str()?.to_lowercase(),
                token_in,
                token_out,
                amount_in,
                amount_out,
                trader: e["account"].as_str().unwrap_or_default().to_lowercase(),
            })
        })
        .collect()
}

const QUERY: &str = "query($venues: [String!]!, $first: Int!) { tradeLaunches(first: $first, orderBy: createdAtTimestamp, orderDirection: desc, where: {sourceVenue_: {kind_in: $venues}}) { token tokenSymbol tokenName tokenDecimals phase curveAddress graduationProgressBps latestPriceQuoteE12 metadataURI netQuoteRaised graduationQuoteAmount buyCount sellCount createdAtTimestamp pair { address } sourceVenue { kind label } } }";

/// How long a page of curve trades is served from cache.
///
/// The tape mixes these with pool swaps read from the chain, so they have to keep the tape's pace,
/// not the launch directory's: caching them for the minute the directory uses left five-second-old
/// pool rows sitting beside minute-old curve rows, which reads as the curves having gone quiet.
/// Unlike the explorer's pool page, this index publishes no cache window of its own to respect.
pub const TRADES_TTL: u64 = 5;

/// How long a launch directory read is served from cache.
///
/// A curve's stage moves with every trade on it, and the Launches list is ordered by stage, so a
/// minute-old page put the rows in a minute-old order — and a token that launched in between did
/// not exist yet as far as the screen was concerned. This is one GraphQL request, so refreshing it
/// at the rate the rest of the market moves is affordable; what it must not do is outrun the
/// launchpad reads that fill in the stages the index leaves null.
pub const LAUNCH_TTL: u64 = 15;

/// Quainance's launches, newest first, at most `first`. New launches and curve progress arrive
/// within [`LAUNCH_TTL`]; the list is a directory, not a ticker.
pub async fn launches(ctx: &DataCtx, first: usize) -> Result<Vec<Launch>> {
    if !ctx.policy.market {
        return Err(CoreError::NotFound("market data is off".into()));
    }
    let url = ctx
        .network
        .ecosystem
        .launch_subgraph
        .clone()
        .ok_or_else(|| CoreError::NotFound(format!("no launch index on {}", ctx.network.name)))?;
    let query = json!({ "query": QUERY, "variables": { "venues": LAUNCH_VENUES, "first": first.clamp(1, 500) } });
    let listed = ctx
        .cached(&format!("launches:{first}"), LAUNCH_TTL, || async move {
            let body = crate::http::post_json(&url, &query).await?;
            if let Some(errors) = body["errors"].as_array().filter(|e| !e.is_empty()) {
                return Err(CoreError::Network(format!(
                    "launch index: {}",
                    clean(errors[0]["message"].as_str().unwrap_or("query failed"), 160)
                )));
            }
            Ok(parse(&body))
        })
        .await?;
    let mut rows = listed.value;
    fill_hartii_progress(ctx, &mut rows).await;
    Ok(rows)
}

/// Reconcile Hartii state by launcher-verified token and curve identity. Its finite quote is
/// shown on both screens with its explicit basis; it must not masquerade as an indexed spot.
async fn fill_hartii_progress(ctx: &DataCtx, rows: &mut [Launch]) {
    if rows.is_empty() {
        return;
    }
    let Ok(onchain) = crate::hartii::launches(ctx).await else { return };
    merge_hartii(rows, &onchain);
}

fn merge_hartii(rows: &mut [Launch], onchain: &[crate::hartii::HartiiLaunch]) {
    let by_token: std::collections::HashMap<&str, &crate::hartii::HartiiLaunch> = onchain.iter().map(|h| (h.token.as_str(), h)).collect();
    for row in rows.iter_mut() {
        let Some(found) = by_token.get(row.token.to_lowercase().as_str()) else { continue };
        if !row.curve.as_ref().is_some_and(|curve| curve.eq_ignore_ascii_case(&found.curve)) {
            continue;
        }
        row.venue_kind = Some(crate::capabilities::Family::HartiiCurve);
        row.progress_bps = found.progress_bps.or(row.progress_bps);
        row.raised_quai = found.raised_quai;
        row.price_quai = found.price_quai;
        row.price_basis = found.price_basis;
    }
}

/// Quainance's media proxy. It resolves a launch's IPFS metadata, checks the image against its
/// own limits, and serves it immutable — the public gateways have stopped serving content
/// (`ipfs.io` answers every CID with a notice page), and this is what Quainance's own launch pages
/// read. The wallet builds every URL here itself from a CID; nothing a token says is fetched as
/// a URL.
pub const MEDIA_PROXY: &str = "https://www.quainance.com/api/media";

/// Image types the wallet accepts from the proxy, and the most it will show.
const LOGO_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/webp", "image/gif"];
const LOGO_MAX_BYTES: u64 = 3 * 1024 * 1024;

/// The CID of an `ipfs://<cid>` reference, when that is all it is: one immutable document, no
/// path, nothing that could be anything but a CID.
fn ipfs_cid(uri: &str) -> Option<&str> {
    let cid = uri.strip_prefix("ipfs://")?;
    (!cid.is_empty() && cid.len() <= 100 && cid.chars().all(|c| c.is_ascii_alphanumeric())).then_some(cid)
}

/// A launch's logo, as a URL on [`MEDIA_PROXY`]. `Ok(None)` when it has none or its metadata does
/// not hold up.
///
/// The checks are the ones Quainance's own page makes before it shows the picture: the document is
/// the one the launch names, it is a Quainance token-metadata document, it names this token's own
/// name and symbol (so one launch cannot borrow another's picture), and the image is one IPFS CID
/// of an allowed type within the size limit. A CID never changes what it points at, so an answer is
/// kept for a month and shared by every wallet (`launch_logo:` is a shared feed).
pub async fn logo(ctx: &DataCtx, launch: &Launch) -> Result<Option<String>> {
    let Some(cid) = launch.metadata_uri.as_deref().and_then(ipfs_cid).map(str::to_string) else { return Ok(None) };
    if !ctx.policy.market || !ctx.policy.icons {
        return Ok(None);
    }
    let (name, symbol) = (launch.name.clone(), launch.symbol.clone());
    let url = format!("{MEDIA_PROXY}/metadata/{cid}");
    let found = ctx
        .cached(&format!("launch_logo:{cid}"), 30 * 86_400, || async move {
            Ok(logo_from(&crate::http::get_json(&url).await?, &cid, &name, &symbol))
        })
        .await?;
    Ok(found.value)
}

/// Pure part of [`logo`].
pub fn logo_from(doc: &Value, cid: &str, name: &str, symbol: &str) -> Option<String> {
    let (meta, media) = (&doc["metadata"], &doc["media"]);
    fn text(v: &Value) -> &str {
        v.as_str().unwrap_or_default()
    }
    let named = clean(text(&meta["name"]), 48) == name && clean(text(&meta["symbol"]), 16) == symbol;
    let document = doc["ok"].as_bool() == Some(true)
        && text(&meta["cid"]) == cid
        && text(&meta["uri"]) == format!("ipfs://{cid}")
        && text(&meta["schema"]) == "quainance.token-metadata"
        && meta["version"].as_u64() == Some(1);
    let image = ipfs_cid(text(&media["uri"]))?;
    let sound = text(&media["cid"]) == image
        && text(&media["url"]) == format!("/api/media/{image}")
        && LOGO_TYPES.contains(&text(&media["mime"]))
        && media["byteLength"].as_u64().is_some_and(|b| b > 0 && b <= LOGO_MAX_BYTES)
        && media["width"].as_u64().is_some_and(|w| w > 0)
        && media["height"].as_u64().is_some_and(|h| h > 0);
    (document && named && sound).then(|| format!("{MEDIA_PROXY}/{image}"))
}

/// Logos for many launches at once, by token address: at most six requests in flight, and a
/// launch that has none (or fails its checks) is simply absent.
pub async fn logos(ctx: &DataCtx, launches: &[Launch]) -> std::collections::HashMap<String, String> {
    use futures::StreamExt;
    futures::stream::iter(launches.iter().filter(|l| l.metadata_uri.is_some()))
        .map(|l| async move { logo(ctx, l).await.ok().flatten().map(|url| (l.token.clone(), url)) })
        .buffer_unordered(6)
        .filter_map(|x| async move { x })
        .collect()
        .await
}

/// Decode the indexer's answer. Every field is untrusted text: numbers that do not parse are
/// absent rather than zero, and names are cleaned before they can reach a terminal.
pub fn parse(body: &Value) -> Vec<Launch> {
    let text = |v: &Value, max: usize| v.as_str().map(|s| clean(s, max)).unwrap_or_default();
    let int = |v: &Value| v.as_str().and_then(|s| s.parse::<u128>().ok());
    let address = |v: &Value| {
        v.as_str().filter(|a| a.len() == 42 && a.starts_with("0x") && a[2..].chars().all(|c| c.is_ascii_hexdigit())).map(str::to_lowercase)
    };
    let quai = |v: &Value| {
        v.as_str()
            .and_then(|s| quai_sdk::U256::from_str_radix(s, 10).ok())
            .map(|atoms| crate::amount::to_f64(atoms, crate::amount::QUAI_DECIMALS))
    };
    body["data"]["tradeLaunches"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|l| {
                    Some(Launch {
                        token: address(&l["token"])?,
                        symbol: text(&l["tokenSymbol"], 16),
                        name: text(&l["tokenName"], 48),
                        decimals: int(&l["tokenDecimals"]).filter(|d| *d <= 36).map_or(18, |d| d as u8),
                        phase: Phase::parse(l["phase"].as_str().unwrap_or_default()),
                        venue: text(&l["sourceVenue"]["label"], 48),
                        venue_kind: l["sourceVenue"]["kind"].as_str().and_then(crate::capabilities::Family::for_launch_venue),
                        curve: address(&l["curveAddress"]),
                        pair: address(&l["pair"]["address"]),
                        progress_bps: int(&l["graduationProgressBps"]).map(|b| b.min(10_000) as u64),
                        price_quai: int(&l["latestPriceQuoteE12"]).map(|p| p as f64 / 1e12),
                        price_basis: crate::markets::PriceBasis::IndexedLastTrade,
                        raised_quai: quai(&l["netQuoteRaised"]).unwrap_or(0.0),
                        target_quai: quai(&l["graduationQuoteAmount"]),
                        buys: int(&l["buyCount"]).unwrap_or(0) as u64,
                        sells: int(&l["sellCount"]).unwrap_or(0) as u64,
                        created_at: int(&l["createdAtTimestamp"]).unwrap_or(0) as u64,
                        metadata_uri: l["metadataURI"].as_str().and_then(ipfs_cid).map(|cid| format!("ipfs://{cid}")),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {

    #[test]
    fn source_kind_selects_adapter_independently_of_display_label() {
        let mut body = serde_json::json!({"data":{"tradeLaunches":[{
            "token":"0x0000000000000000000000000000000000000001",
            "sourceVenue":{"kind":"HARTII_CURVE","label":"Quainance bonding curve"}
        }]}});
        assert_eq!(super::parse(&body)[0].venue_kind, Some(crate::capabilities::Family::HartiiCurve));
        body["data"]["tradeLaunches"][0]["sourceVenue"]["kind"] = serde_json::json!("unknown");
        assert_eq!(super::parse(&body)[0].venue_kind, None);
    }

    /// A curve trade becomes a tape row in the direction the trader took it.
    ///
    /// The fixture is a real page of the trade-zone index: QAXE and KISHORE on HartiiLabs' curves,
    /// QMON on Quainance's — three launchpads the pool tape cannot see at all, because a bonding
    /// curve emits no pool log to decode.
    #[test]
    fn hartii_price_reconciliation_is_independent_of_progress_and_identity_bound() {
        let mut launch = Launch {
            token: "0x0011".into(),
            curve: Some("0x0022".into()),
            progress_bps: Some(20),
            price_quai: Some(100.0),
            price_basis: crate::markets::PriceBasis::IndexedLastTrade,
            ..Launch::default()
        };
        let source = crate::hartii::HartiiLaunch {
            token: launch.token.clone(),
            curve: "0x0022".into(),
            progress_bps: Some(30),
            price_quai: Some(2.0),
            price_basis: crate::markets::PriceBasis::OneQuaiBuyQuote,
            ..Default::default()
        };
        merge_hartii(std::slice::from_mut(&mut launch), std::slice::from_ref(&source));
        assert_eq!(launch.price_quai, Some(2.0));
        assert_eq!(launch.progress_bps, Some(30));
        assert_eq!(launch.price_basis, crate::markets::PriceBasis::OneQuaiBuyQuote);
        let mut unavailable = source.clone();
        unavailable.price_quai = None;
        merge_hartii(std::slice::from_mut(&mut launch), &[unavailable]);
        assert_eq!(launch.price_quai, None, "do not silently substitute old indexed price for unavailable quote");
        launch.curve = Some("0x0033".into());
        merge_hartii(std::slice::from_mut(&mut launch), &[source]);
        assert_eq!(launch.price_quai, None, "matching token symbol/address is insufficient if curve differs");
    }

    #[test]
    fn curve_tape_symbols_are_cleaned_and_bounded_at_ingestion() {
        let symbol = format!("US\u{2060}DT\u{1b}{}", "x".repeat(100));
        let body = serde_json::json!({"data": {"tradeExecutions": [{
            "token": "0xtoken", "sourceContract": "0xcurve", "transactionHash": "0xtx", "blockNumber": "1",
            "timestamp": "100", "logIndex": "0", "account": "0xowner", "side": "BUY", "quoteKind": "NATIVE_QUAI",
            "tokenAmount": "1000000000000000000", "quoteAmount": "1000000000000000000",
            "launch": {"tokenDecimals": "18", "tokenSymbol": symbol}
        }]}});
        let rows = parse_curve_trades(&body);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].token_out.symbol.chars().count(), 32);
        assert!(rows[0].token_out.symbol.starts_with("USDT"));
        assert!(!rows[0].token_out.symbol.contains('\u{1b}'));
    }

    #[test]
    fn curve_trades_become_tape_rows_in_the_traders_direction() {
        let body: Value = serde_json::from_str(include_str!("fixtures/quai_curve_trades.json")).unwrap();
        let rows = parse_curve_trades(&body);
        assert_eq!(rows.len(), 6, "every execution in the page becomes a row");

        // A buy pays QUAI and receives the token.
        let buy = rows.iter().find(|r| r.token_out.symbol == "QAXE").expect("a QAXE buy");
        assert_eq!(buy.token_in.symbol, "QUAI", "native quote is named QUAI, not the wrapper");
        assert!((buy.amount_in - 4_200.0).abs() < 1.0, "4200 QUAI paid, got {}", buy.amount_in);
        assert!((buy.amount_out - 1_015_403.5).abs() < 1.0, "about 1.0155M QAXE out, got {}", buy.amount_out);
        // The index carries the block's own time, so a curve row never needs a header fetched.
        assert!(buy.timed && buy.at > 1_700_000_000, "{buy:?}");
        assert_eq!(buy.block, 10_220_299);
        assert!(buy.pool.starts_with("0x") && buy.pool == buy.pool.to_lowercase());

        // A sell is the same trade read backwards: the token goes in, QUAI comes out.
        let sell = rows.iter().find(|r| r.token_in.symbol == "KISHORE").expect("a KISHORE sell");
        assert_eq!(sell.token_out.symbol, "QUAI");
        assert!(sell.amount_in > 1_000.0 && sell.amount_out > 0.0, "{sell:?}");

        // Which way each row went, as the tape asks it: the launch token is the base either way.
        let base = &buy.token_out.address;
        assert!(buy.buys(base), "the QAXE buy bought QAXE");
        assert!(!sell.buys(&sell.token_in.address), "the KISHORE sell sold KISHORE");

        // Two curves trading in the same block stay apart: the tape orders by log position.
        let same_block: Vec<&crate::markets::DexSwap> = rows.iter().filter(|r| r.block == 10_219_754).collect();
        assert_eq!(same_block.len(), 2, "two curves traded in that block");
        assert_ne!(same_block[0].index, same_block[1].index, "a shared block still gives distinct rows");

        // Nothing is shown as a trade of zero.
        assert!(rows.iter().all(|r| r.amount_in > 0.0 && r.amount_out > 0.0), "a zero-amount row is noise");
        // Only what the venue allowlist admits — POOP_CURVE is deliberately not asked for.
        assert!(TRADES_QUERY.contains("kind: CURVE"), "the AMM side would duplicate the pool tape");
        assert!(TRADES_QUERY.contains("venue_: {kind_in: $venues}"), "the same allowlist as the directory");
    }

    /// A malformed page yields no rows rather than rows full of zeros.
    #[test]
    fn a_curve_trade_page_that_makes_no_sense_yields_nothing() {
        assert!(parse_curve_trades(&json!({})).is_empty());
        assert!(parse_curve_trades(&json!({"data": {"tradeExecutions": []}})).is_empty());
        let junk = json!({"data": {"tradeExecutions": [
            {"side": "BUY", "token": "0x00aa", "tokenAmount": "0", "quoteAmount": "1000", "transactionHash": "0x1",
             "blockNumber": "5", "timestamp": "9", "sourceContract": "0x00cc", "launch": {"tokenSymbol": "Z", "tokenDecimals": "18"}},
            {"side": "BUY", "token": "0x00aa", "tokenAmount": "1000", "quoteAmount": "1000", "transactionHash": "0x1",
             "blockNumber": "5", "timestamp": "9", "launch": {"tokenSymbol": "Z", "tokenDecimals": "18"}},
        ]}});
        assert!(parse_curve_trades(&junk).is_empty(), "a zero amount and a missing contract are both dropped");
    }
    use super::*;

    /// A logo is shown only when its metadata is the launch's own and the image is within the
    /// limits Quainance's page enforces. The document is from the live proxy, 2026-09-18.
    #[test]
    fn a_logo_must_be_the_launches_own_and_within_limits() {
        let cid = "bafkreigynbdigag634tzagcofoclwl7yqaehe4gq4s4gnx6yxal764fzfa";
        let image = "bafybeicdowsg4ppek6zj5ymvivx6qsmp3xveztw6tsni4fciw7xszbx2wa";
        let doc = json!({"ok": true,
            "metadata": {"cid": cid, "uri": format!("ipfs://{cid}"), "schema": "quainance.token-metadata", "version": 1, "name": "CHEEZ", "symbol": "CHEEZ"},
            "media": {"cid": image, "uri": format!("ipfs://{image}"), "url": format!("/api/media/{image}"), "mime": "image/jpeg",
                      "width": 1152, "height": 1712, "byteLength": 442875, "frameCount": 1}});
        assert_eq!(logo_from(&doc, cid, "CHEEZ", "CHEEZ"), Some(format!("{MEDIA_PROXY}/{image}")));
        // Another launch pointing at CHEEZ's document does not get CHEEZ's picture.
        assert_eq!(logo_from(&doc, cid, "NOTCHEEZ", "NCZ"), None);
        // A document that is not the one the launch names.
        assert_eq!(logo_from(&doc, "bafkreiother", "CHEEZ", "CHEEZ"), None);
        // An image type or size outside the limits.
        let mut svg = doc.clone();
        svg["media"]["mime"] = json!("image/svg+xml");
        assert_eq!(logo_from(&svg, cid, "CHEEZ", "CHEEZ"), None);
        let mut huge = doc.clone();
        huge["media"]["byteLength"] = json!(LOGO_MAX_BYTES + 1);
        assert_eq!(logo_from(&huge, cid, "CHEEZ", "CHEEZ"), None);
        // An image reference that is a path rather than one CID.
        let mut path = doc.clone();
        path["media"]["uri"] = json!(format!("ipfs://{image}/../x"));
        assert_eq!(logo_from(&path, cid, "CHEEZ", "CHEEZ"), None);
        // Only a bare `ipfs://<cid>` is a metadata reference at all.
        assert_eq!(ipfs_cid("ipfs://abc123"), Some("abc123"));
        assert_eq!(ipfs_cid("https://evil.example/x"), None);
        assert_eq!(ipfs_cid("ipfs://abc/def"), None);
        assert_eq!(ipfs_cid("ipfs://"), None);
    }

    /// The indexer's shape as captured on 2026-09-17, including a hostile name and a malformed
    /// address: numbers decode at their scales, junk is dropped rather than guessed at.
    #[test]
    fn launches_decode_at_their_scales_and_untrusted_text_is_cleaned() {
        let body = json!({"data": {"tradeLaunches": [
            {"token": "0x0016c3221b6a1707427d660945cd284a9be58cec", "tokenSymbol": "CHEEZ", "tokenName": "CHEEZ\u{1b}[31m",
             "phase": "BONDING", "curveAddress": "0x004ce1cbb33cad511b79d52c6e1118ce4eb60db3", "graduationProgressBps": "5135",
             "latestPriceQuoteE12": "51048817", "netQuoteRaised": "12837703006217154265089", "graduationQuoteAmount": "25000000000000000000000",
             "buyCount": "49", "sellCount": "15", "createdAtTimestamp": "1789636189", "pair": null, "sourceVenue": {"label": "Quainance bonding curve"}},
            {"token": "0x0048848CA70EA1560577B4725A84B23B6BC589E2", "tokenSymbol": "QOGE", "tokenName": "Qoge", "phase": "GRADUATED",
             "curveAddress": null, "graduationProgressBps": "10000", "latestPriceQuoteE12": null, "netQuoteRaised": "25000000000000000000000",
             "graduationQuoteAmount": "25000000000000000000000", "buyCount": "900", "sellCount": "300", "createdAtTimestamp": "1789000000",
             "pair": {"address": "0x001dac18c8702f07d18b1bcdca45e85c6b8364a6"}, "sourceVenue": {"label": "Quainance bonding curve"}},
            {"token": "not an address", "tokenSymbol": "BAD"}
        ]}});
        let launches = parse(&body);
        assert_eq!(launches.len(), 2, "a row without a valid token is dropped");
        let cheez = &launches[0];
        assert_eq!((cheez.phase, cheez.progress_bps, cheez.trades()), (Phase::Bonding, Some(5135), 64));
        assert!((cheez.price_quai.unwrap() - 0.000051048817).abs() < 1e-12);
        assert!((cheez.raised_quai - 12_837.703).abs() < 0.01, "{}", cheez.raised_quai);
        assert!(cheez.target_quai.is_some_and(|t| (t - 25_000.0).abs() < 1e-6), "{:?}", cheez.target_quai);
        assert!(!cheez.name.contains('\u{1b}'), "{:?}", cheez.name);
        let qoge = &launches[1];
        assert_eq!(qoge.token, "0x0048848ca70ea1560577b4725a84b23b6bc589e2", "addresses are lowercased");
        assert_eq!(
            (qoge.phase, qoge.price_quai, qoge.pair.as_deref()),
            (Phase::Graduated, None, Some("0x001dac18c8702f07d18b1bcdca45e85c6b8364a6"))
        );
    }
}
