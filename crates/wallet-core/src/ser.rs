//! Serde helpers: chain quantities serialize as decimal strings.

use quai_sdk::U256;
use serde::{Deserialize, Deserializer, Serializer};

/// Serialize a U256 as a decimal string.
pub fn u256<S: Serializer>(value: &U256, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&value.to_string())
}

/// Read back a U256 written by [`u256`]. A number is accepted too, so hand-edited or
/// older cache entries do not fail the whole row.
pub fn de_u256<'de, D: Deserializer<'de>>(d: D) -> Result<U256, D::Error> {
    match serde_json::Value::deserialize(d)? {
        serde_json::Value::String(s) => U256::from_str_radix(s.trim(), 10).map_err(serde::de::Error::custom),
        serde_json::Value::Number(n) => Ok(U256::from(n.as_u64().unwrap_or(0))),
        other => Err(serde::de::Error::custom(format!("expected a decimal amount, got {other}"))),
    }
}
