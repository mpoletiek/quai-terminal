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

pub mod u256_string {
    use quai_sdk::U256;
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &U256, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<U256, D::Error> {
        let text = String::deserialize(d)?;
        U256::from_str_radix(&text, 10).map_err(serde::de::Error::custom)
    }
}

/// An optional U256 as a decimal string (`#[serde(with = "ser::u256_opt")]`).
pub mod u256_opt {
    use quai_sdk::U256;
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &Option<U256>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(v) => s.serialize_some(&v.to_string()),
            None => s.serialize_none(),
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<U256>, D::Error> {
        Option::<String>::deserialize(d)?.map(|text| U256::from_str_radix(&text, 10).map_err(serde::de::Error::custom)).transpose()
    }
}

/// Named U256 amounts, each as a decimal string (`#[serde(with = "ser::u256_pairs")]`).
pub mod u256_pairs {
    use quai_sdk::U256;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    pub fn serialize<S: Serializer>(v: &[(String, U256)], s: S) -> Result<S::Ok, S::Error> {
        v.iter().map(|(name, n)| (name, n.to_string())).collect::<Vec<_>>().serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<(String, U256)>, D::Error> {
        Vec::<(String, String)>::deserialize(d)?
            .into_iter()
            .map(|(name, text)| U256::from_str_radix(&text, 10).map(|n| (name, n)).map_err(serde::de::Error::custom))
            .collect()
    }
}
