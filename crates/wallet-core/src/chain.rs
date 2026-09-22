//! Small, shared helpers for talking to contracts: parsing an address, building an interface
//! from a human-readable ABI, and reading values out of a decoded call result.

use crate::error::{CoreError, Result};
use quai_sdk::abi::AbiInterface;
use quai_sdk::{QuaiAddress, U256};
use serde_json::Value;

/// An interface from human-readable signatures.
pub(crate) fn interface(abi: &[&str]) -> Result<AbiInterface> {
    AbiInterface::from_human_readable(abi).map_err(|e| CoreError::Invalid(format!("abi: {e}")))
}

/// A Cyprus-1 Quai address.
pub(crate) fn addr(text: &str) -> Result<QuaiAddress> {
    text.parse().map_err(|_| CoreError::Invalid(format!("`{text}` is not a Cyprus-1 Quai address")))
}

/// A `uint` from a decoded result (decimal text), zero when absent or malformed.
pub(crate) fn uint(values: &[Value], index: usize) -> U256 {
    values.get(index).and_then(Value::as_str).and_then(|t| U256::from_str_radix(t, 10).ok()).unwrap_or_default()
}

/// An address from a decoded result, lowercase; empty when absent.
pub(crate) fn address_at(values: &[Value], index: usize) -> String {
    values.get(index).and_then(Value::as_str).unwrap_or_default().to_lowercase()
}

/// Whether an address is the zero address (with or without `0x`).
pub(crate) fn is_zero_address(address: &str) -> bool {
    address.trim_start_matches("0x").chars().all(|c| c == '0')
}
