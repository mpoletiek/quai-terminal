//! Block headers for the Chain screen: Quai's hierarchy as this wallet's zone sees it.
//!
//! A zone block carries the prime and region chains' numbers and its own order (whether it was
//! also a region or prime block), so the zone's headers alone draw the whole lattice. Headers are
//! read from the raw block (`quai_getBlockByNumber`, without transactions), in one JSON-RPC batch
//! where the node takes batches.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::network::Node;
use crate::{CoreError, Result};

/// One zone block, as much of it as the Chain screen draws.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockHead {
    /// Zone (Cyprus-1) height.
    pub height: u64,
    /// The prime and region chains' numbers at this block.
    pub prime: u64,
    pub region: u64,
    /// 0 prime, 1 region, 2 zone: the highest chain this block is also a block of.
    pub order: u8,
    pub hash: String,
    pub parent: String,
    /// Unix seconds.
    pub timestamp: u64,
    pub txs: u32,
    /// Transactions this block sends to other zones.
    pub etxs: u32,
    /// Workshares (sub-threshold work the block includes).
    pub workshares: u32,
    pub gas_used: u64,
    pub gas_limit: u64,
    pub base_fee_wei: u128,
    pub miner: String,
    pub difficulty: u128,
    /// Entropy this block added to the zone's total (PoEM), in thousandths of a bit:
    /// `totalEntropy − parentEntropy[zone]`, which the node keeps in units of 2⁻⁶⁴ bits.
    pub entropy_mbits: u64,
}

/// The most headers one request asks for.
pub const MAX_BATCH: usize = 64;

fn hex_u128(v: &Value) -> Option<u128> {
    u128::from_str_radix(v.as_str()?.trim_start_matches("0x"), 16).ok()
}

fn hex_u64(v: &Value) -> Option<u64> {
    u64::try_from(hex_u128(v)?).ok()
}

fn count(v: &Value) -> u32 {
    v.as_array().map_or(0, |a| a.len() as u32)
}

/// A header from a raw block (`quai_getBlockByNumber` with `false`). None when a field the screen
/// needs is missing or malformed.
pub fn parse(block: &Value) -> Option<BlockHead> {
    let header = &block["header"];
    let wo = &block["woHeader"];
    let numbers = header["number"].as_array()?;
    Some(BlockHead {
        height: hex_u64(&wo["number"])?,
        prime: hex_u64(numbers.first()?)?,
        region: hex_u64(numbers.get(1)?)?,
        order: u8::try_from(block["order"].as_u64()?).ok().filter(|o| *o <= 2)?,
        hash: block["hash"].as_str()?.to_string(),
        parent: wo["parentHash"].as_str().unwrap_or_default().to_string(),
        timestamp: hex_u64(&wo["timestamp"])?,
        txs: count(&block["transactions"]),
        etxs: count(&block["outboundEtxs"]),
        workshares: count(&block["workshares"]),
        gas_used: hex_u64(&header["gasUsed"]).unwrap_or(0),
        gas_limit: hex_u64(&header["gasLimit"]).unwrap_or(0),
        base_fee_wei: hex_u128(&header["baseFeePerGas"]).unwrap_or(0),
        miner: wo["primaryCoinbase"].as_str().unwrap_or_default().to_string(),
        difficulty: hex_u128(&wo["difficulty"]).unwrap_or(0),
        entropy_mbits: header["parentEntropy"]
            .get(2)
            .and_then(hex_u128)
            .zip(hex_u128(&block["totalEntropy"]))
            .map_or(0, |(parent, total)| u64::try_from(total.saturating_sub(parent).saturating_mul(1000) >> 64).unwrap_or(u64::MAX)),
    })
}

/// The node's head height.
pub async fn head_height(node: &Node) -> Result<u64> {
    let v = node.raw("quai_blockNumber", json!([])).await?;
    hex_u64(&v).ok_or_else(|| CoreError::Network(format!("quai_blockNumber answered {v}")))
}

/// Headers for `heights` (at most [`MAX_BATCH`]), oldest first, in one batch where the node takes
/// batches and one call each where it doesn't. Heights the node has no block for are left out.
pub async fn heads(node: &Node, heights: &[u64]) -> Result<Vec<BlockHead>> {
    let heights = &heights[heights.len().saturating_sub(MAX_BATCH)..];
    let params: Vec<Value> = heights.iter().map(|h| json!([format!("0x{h:x}"), false])).collect();
    let requests: Vec<(&str, Value)> = params.iter().map(|p| ("quai_getBlockByNumber", p.clone())).collect();
    let answers: Vec<Value> = match node.raw_batch(requests).await {
        Some(Ok(items)) => items.into_iter().filter_map(|r| r.ok()).collect(),
        Some(Err(e)) => return Err(e),
        None => {
            let mut out = Vec::new();
            for p in params {
                out.push(node.raw("quai_getBlockByNumber", p).await?);
            }
            out
        }
    };
    let mut blocks: Vec<BlockHead> = answers.iter().filter_map(parse).collect();
    blocks.sort_by_key(|b| b.height);
    blocks.dedup_by_key(|b| b.height);
    Ok(blocks)
}

/// What a poll asks for: every block after `after` up to the head, the newest `max` of them; or,
/// with nothing known yet, the newest `max`.
pub fn wanted(after: Option<u64>, head: u64, max: usize) -> Vec<u64> {
    let max = max.clamp(1, MAX_BATCH) as u64;
    let from = match after {
        Some(a) if a >= head => return Vec::new(),
        Some(a) => (a + 1).max(head.saturating_sub(max - 1)),
        None => head.saturating_sub(max - 1),
    };
    (from..=head).collect()
}

/// New headers since `after`: the head's height, then the blocks missing up to it.
pub async fn since(node: &Node, after: Option<u64>, max: usize) -> Result<Vec<BlockHead>> {
    let head = head_height(node).await?;
    let want = wanted(after, head, max);
    if want.is_empty() {
        return Ok(Vec::new());
    }
    heads(node, &want).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Value {
        serde_json::from_str(include_str!("fixtures/block_10515394.json")).unwrap()
    }

    #[test]
    fn a_mainnet_region_block_parses() {
        let b = parse(&fixture()).expect("parses");
        assert_eq!(b.height, 10_515_394);
        assert_eq!(b.order, 1, "a region block");
        assert_eq!((b.prime, b.region), (2_325_683, 5_647_492));
        assert_eq!(b.hash, "0x667bbb1670b5f2f10fd61a64467e3008e1dc9b60840a146c26280a48ff59e2ff");
        assert_eq!(b.txs, 3, "the fixture keeps three");
        assert_eq!(b.gas_limit, 0x2faf080);
        assert_eq!(b.miner, "0x0011d16c5f4801D8d7B2eD4A84fC98D114Cb85b8");
        assert!(b.timestamp > 1_700_000_000);
        assert_eq!(b.entropy_mbits, 38_676, "38.676 bits added to Cyprus-1's total");
    }

    #[test]
    fn a_block_missing_its_order_or_numbers_is_refused() {
        let mut v = fixture();
        v["order"] = json!(7);
        assert!(parse(&v).is_none());
        let mut v = fixture();
        v["header"]["number"] = json!([]);
        assert!(parse(&v).is_none());
    }

    #[test]
    fn a_poll_asks_for_the_gap_and_never_more_than_a_batch() {
        assert_eq!(wanted(None, 100, 3), vec![98, 99, 100]);
        assert_eq!(wanted(Some(97), 100, 48), vec![98, 99, 100]);
        assert_eq!(wanted(Some(100), 100, 48), Vec::<u64>::new());
        assert_eq!(wanted(Some(101), 100, 48), Vec::<u64>::new(), "a node behind what we saw");
        assert_eq!(wanted(Some(0), 1_000, 1_000).len(), MAX_BATCH);
        assert_eq!(*wanted(Some(0), 1_000, 1_000).last().unwrap(), 1_000);
    }
}
