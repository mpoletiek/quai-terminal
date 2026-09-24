//! Multicall3: many small reads in one round trip.
//!
//! A pool sweep or a position sweep is dozens of one-word `view` calls, and doing them one at a
//! time is what makes those screens feel slow. `aggregate3` sends them together and tolerates
//! individual failures, so one unreadable contract does not sink the batch.
//!
//! Calls here are encoded by hand rather than through `AbiInterface`, because every read the wallet
//! batches takes static arguments and returns static words. That keeps the batch a pure function of
//! its inputs, which the tests pin.

use crate::data::DataCtx;
use crate::error::{CoreError, Result};
use crate::network::Node;
use quai_sdk::abi::AbiInterface;
use quai_sdk::contracts::Contract;
use quai_sdk::{BlockTag, QuaiAddress, U256};
use serde_json::{Value, json};

/// Calls per `aggregate3`. Big enough that a 21-pool sweep is one round trip, small enough that the
/// node's `eth_call` gas and response limits are never near.
pub const CHUNK: usize = 120;

/// Chunks of one batch in flight at once: a sweep of a few hundred calls is one round trip, and a
/// directory of thousands still asks the node for no more than this many at a time.
pub const CHUNKS_IN_FLIGHT: usize = 4;

/// `aggregate3` is `payable` on the real contract, but the wallet only ever reads through it and
/// never sends value. It is declared `view` here so the SDK routes it to `eth_call` instead of
/// refusing it as state-changing; nothing in this module can produce a transaction.
const MULTICALL_ABI: &[&str] = &[
    "function aggregate3((address target, bool allowFailure, bytes callData)[] calls) view returns ((bool success, bytes returnData)[] returnData)",
];

/// An argument to a batched read. Only the shapes the wallet actually batches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Arg {
    /// A 20-byte address, right-aligned in its word.
    Addr(String),
    /// A `uint256`.
    Uint(U256),
}

fn selector(signature: &str) -> [u8; 4] {
    let hash = quai_sdk::crypto::keccak256(signature.as_bytes());
    [hash[0], hash[1], hash[2], hash[3]]
}

/// Calldata for a `view` call with static arguments: `selector ++ one word per argument`.
pub fn call_data(signature: &str, args: &[Arg]) -> String {
    let mut out = Vec::with_capacity(4 + args.len() * 32);
    out.extend_from_slice(&selector(signature));
    for arg in args {
        let mut word = [0u8; 32];
        match arg {
            Arg::Addr(a) => {
                let bytes = hex::decode(a.trim_start_matches("0x")).unwrap_or_default();
                // Right-align, and ignore anything that is not a 20-byte address.
                if bytes.len() == 20 {
                    word[12..].copy_from_slice(&bytes);
                }
            }
            Arg::Uint(v) => word.copy_from_slice(&v.to_be_bytes::<32>()),
        }
        out.extend_from_slice(&word);
    }
    format!("0x{}", hex::encode(out))
}

/// One batched read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    /// Contract to call (lowercase hex).
    pub target: String,
    /// Selector-prefixed calldata.
    pub data: String,
}

impl Call {
    /// A `view` call with static arguments.
    pub fn view(target: &str, signature: &str, args: &[Arg]) -> Call {
        Call { target: target.to_lowercase(), data: call_data(signature, args) }
    }
}

/// The `i`-th 32-byte word of a return, as a `U256`. Zero when the return is too short, which is
/// what a failed or empty call decodes to.
pub fn word(data: &[u8], i: usize) -> U256 {
    data.get(i * 32..(i + 1) * 32).map(U256::from_be_slice).unwrap_or(U256::ZERO)
}

/// The `i`-th word read as an address, lowercase and `0x`-prefixed. Empty when absent.
pub fn address_word(data: &[u8], i: usize) -> String {
    match data.get(i * 32 + 12..(i + 1) * 32) {
        Some(bytes) => format!("0x{}", hex::encode(bytes)),
        None => String::new(),
    }
}

/// Multicall3 bound to a network, when one is configured and its bytecode matches the pin.
pub struct Multicall<'a> {
    node: &'a Node,
    address: QuaiAddress,
}

impl<'a> Multicall<'a> {
    /// Open the pinned Multicall3, or `None` when this network has none. Absent is not an error:
    /// every caller falls back to sequential reads, which need no third contract at all.
    pub async fn open(ctx: &'a DataCtx) -> Option<Multicall<'a>> {
        let pin = ctx.network.ecosystem.multicall3.as_ref()?;
        let address = ctx.verify_pinned(pin, "Multicall3").await.ok()?;
        Some(Multicall { node: &ctx.node, address })
    }

    /// Open the pinned Multicall3 on a specific node (a session's reader, say).
    pub async fn on(
        app: &crate::appdb::AppDb,
        node: &'a Node,
        network: &crate::network::NetworkProfile,
        trust: crate::data::Trust,
    ) -> Option<Multicall<'a>> {
        let pin = network.ecosystem.multicall3.as_ref()?;
        let address = crate::data::verify_pinned(app, node, network, pin, "Multicall3", trust).await.ok()?;
        Some(Multicall { node, address })
    }

    /// Run every call, in order, tolerating individual failures.
    ///
    /// The result is positional: `out[i]` is the return data of `calls[i]`, or `None` when that one
    /// reverted. Batches larger than [`CHUNK`] are split, still in order.
    pub async fn try_all(&self, calls: &[Call]) -> Result<Vec<Option<Vec<u8>>>> {
        self.try_all_at(calls, BlockTag::Latest).await
    }

    /// [`Self::try_all`] at one block, so that everything read together describes the same state.
    pub async fn try_all_at(&self, calls: &[Call], block: BlockTag) -> Result<Vec<Option<Vec<u8>>>> {
        use futures::{StreamExt, TryStreamExt};
        // The chunks are independent, so up to [`CHUNKS_IN_FLIGHT`] go out at once; `buffered`
        // keeps them in order. One after another, the launchpad's 385-call sweep was four round
        // trips of half a second each.
        let chunks: Vec<Vec<Option<Vec<u8>>>> = futures::stream::iter(calls.chunks(CHUNK).map(|chunk| self.aggregate3(chunk, block)))
            .buffered(CHUNKS_IN_FLIGHT)
            .try_collect()
            .await?;
        Ok(chunks.into_iter().flatten().collect())
    }

    async fn aggregate3(&self, calls: &[Call], block: BlockTag) -> Result<Vec<Option<Vec<u8>>>> {
        if calls.is_empty() {
            return Ok(Vec::new());
        }
        let interface = AbiInterface::from_human_readable(MULTICALL_ABI).map_err(|e| CoreError::Invalid(format!("multicall abi: {e}")))?;
        let contract = Contract::new(self.address, interface, &self.node.provider);
        // Tuples are positional arrays in this ABI layer, not objects. `allowFailure` is always
        // true: one bad contract must not sink the batch.
        let list: Vec<Value> = calls.iter().map(|c| json!([c.target, true, c.data])).collect();
        let caller: QuaiAddress = crate::data::READ_CALLER.parse().map_err(|_| CoreError::Invalid("read caller".into()))?;
        let out = contract.call(caller, "aggregate3", &[Value::Array(list)], block).await?;
        let rows = out.first().and_then(Value::as_array).ok_or_else(|| CoreError::Network("multicall returned no results".into()))?;
        if rows.len() != calls.len() {
            return Err(CoreError::Network("multicall returned the wrong number of results".into()));
        }
        Ok(rows.iter().map(decode_row).collect())
    }
}

/// One `(bool success, bytes returnData)` row. A row the decoder cannot read counts as a failure,
/// never as an empty success, so a caller can always tell "no answer" from "answered zero".
fn decode_row(row: &Value) -> Option<Vec<u8>> {
    let ok = row.get("success").or_else(|| row.get(0));
    let success = match ok {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "true" || s == "1",
        Some(Value::Number(n)) => n.as_u64() == Some(1),
        _ => return None,
    };
    if !success {
        return None;
    }
    let data = row.get("returnData").or_else(|| row.get(1)).and_then(Value::as_str)?;
    hex::decode(data.trim_start_matches("0x")).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calldata_is_the_selector_and_one_word_per_argument() {
        // The canonical ERC-20 selectors, so a typo in the encoder is visible.
        assert_eq!(call_data("totalSupply()", &[]), "0x18160ddd");
        let owner = "0x002360bc8e2a359be7335b06de43f1c7f040f15a";
        let balance = call_data("balanceOf(address)", &[Arg::Addr(owner.into())]);
        assert!(balance.starts_with("0x70a08231"), "{balance}");
        assert_eq!(balance.len(), 2 + 8 + 64, "selector plus exactly one word");
        assert!(balance.ends_with(&owner[2..]), "the address is right-aligned: {balance}");
        // getReserves and token0 match the values markets.rs already uses.
        assert!(call_data("getReserves()", &[]).starts_with("0x0902f1ac"));
        assert!(call_data("token0()", &[]).starts_with("0x0dfe1681"));
        // Two arguments, mixed kinds.
        let two = call_data("earned(uint256,address)", &[Arg::Uint(U256::from(3u64)), Arg::Addr(owner.into())]);
        assert_eq!(two.len(), 2 + 8 + 128);
        assert!(two[10..74].ends_with('3'), "the uint is left-padded: {}", &two[10..74]);
        // A malformed address encodes as zero rather than panicking or shifting the layout.
        let bad = call_data("balanceOf(address)", &[Arg::Addr("0xnope".into())]);
        assert_eq!(bad[10..], "0".repeat(64));
    }

    #[test]
    fn words_decode_and_a_short_return_is_zero_not_a_panic() {
        let mut data = vec![0u8; 64];
        data[31] = 7;
        data[63] = 9;
        assert_eq!(word(&data, 0), U256::from(7u64));
        assert_eq!(word(&data, 1), U256::from(9u64));
        assert_eq!(word(&data, 5), U256::ZERO, "past the end reads as zero");
        assert_eq!(word(&[], 0), U256::ZERO);
        let mut addr = vec![0u8; 32];
        addr[12..].copy_from_slice(&hex::decode("002360bc8e2a359be7335b06de43f1c7f040f15a").unwrap());
        assert_eq!(address_word(&addr, 0), "0x002360bc8e2a359be7335b06de43f1c7f040f15a");
        assert_eq!(address_word(&[], 0), "");
    }

    #[test]
    fn rows_distinguish_failure_from_an_empty_success() {
        assert_eq!(decode_row(&json!({"success": true, "returnData": "0x0a"})), Some(vec![10]));
        assert_eq!(decode_row(&json!({"success": true, "returnData": "0x"})), Some(vec![]), "answered nothing");
        assert_eq!(decode_row(&json!({"success": false, "returnData": "0x"})), None, "reverted");
        // Positional decoding, for a backend that returns tuples as arrays.
        assert_eq!(decode_row(&json!([true, "0x0b"])), Some(vec![11]));
        assert_eq!(decode_row(&json!([false, "0x"])), None);
        // Anything unreadable is a failure, never a silent empty success.
        assert_eq!(decode_row(&json!({})), None);
        assert_eq!(decode_row(&json!({"success": true})), None);
        assert_eq!(decode_row(&json!({"success": true, "returnData": "zz"})), None);
    }

    #[test]
    fn the_multicall_abi_parses() {
        AbiInterface::from_human_readable(MULTICALL_ABI).unwrap();
    }
}
