//! The operation journal's vocabulary, typed: what kind an operation is, the facts its detail
//! holds, and the receipts a multi-step trade may advance on.
//!
//! The journal is stored as text (a kind name and a JSON detail per row), and it stays that way:
//! older and newer builds must keep reading each other's databases, and a backup is a copy of
//! them. What changes is how the code reaches it:
//!
//! - [`OpKind`] names every kind the wallet writes. An unknown name (from a newer build) is kept
//!   as [`OpKind::Other`] and written back unchanged, never guessed at.
//! - [`Detail`] reaches a key only through its accessor, so a misspelled key does not compile.
//!   The stored JSON round-trips exactly, keys this build does not know included.
//! - Receipts ([`SwapReceipt`], [`WrapDeposit`], [`WqiClaim`]) are the typed, checked facts a
//!   saved trade plan advances on. They refuse anything missing or malformed rather than reading
//!   an absent field as "nothing to check".

use quai_sdk::U256;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

macro_rules! kinds {
    ($($variant:ident => $name:literal,)*) => {
        /// What an operation is. Stored as its name ([`OpKind::as_str`]).
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum OpKind {
            $($variant,)*
            /// A kind this build does not know, kept exactly as stored.
            Other(String),
        }

        impl OpKind {
            /// Every kind this build writes.
            pub const KNOWN: &'static [OpKind] = &[$(OpKind::$variant,)*];

            /// The stored name.
            pub fn as_str(&self) -> &str {
                match self {
                    $(OpKind::$variant => $name,)*
                    OpKind::Other(name) => name,
                }
            }

            /// The kind a stored name means. Unknown names are kept, not refused.
            pub fn parse(name: &str) -> OpKind {
                match name {
                    $($name => OpKind::$variant,)*
                    other => OpKind::Other(other.to_string()),
                }
            }
        }
    };
}

kinds! {
    SendQuai => "send_quai",
    SendQi => "send_qi",
    SendToken => "send_token",
    Approve => "approve",
    Revoke => "revoke",
    Swap => "swap",
    SwapExactOutput => "swap_exact_output",
    ConvertQuaiToQi => "convert_quai_to_qi",
    ConvertQiToQuai => "convert_qi_to_quai",
    WrapQi => "wrap_qi",
    ClaimWqi => "claim_wqi",
    UnwrapWqi => "unwrap_wqi",
    WrapQuai => "wrap_quai",
    UnwrapQuai => "unwrap_quai",
    AggregateQi => "aggregate_qi",
    SweepQi => "sweep_qi",
    AddLiquidity => "add_liquidity",
    RemoveLiquidity => "remove_liquidity",
    Stake => "stake",
    Unstake => "unstake",
    Harvest => "harvest",
    Exit => "exit",
    Incentivize => "incentivize",
    CurveBuy => "curve_buy",
    CurveSell => "curve_sell",
    CurveClaim => "curve_claim",
    HartiiBuy => "hartii_buy",
    HartiiSell => "hartii_sell",
    NftTransfer => "nft_transfer",
    NftBuy => "nft_buy",
    NftList => "nft_list",
    NftReprice => "nft_reprice",
    NftUnlist => "nft_unlist",
    ContractCall => "contract_call",
    BoardPost => "board_post",
    Notify => "notify",
    FillGap => "fill_gap",
    SpeedUp => "speed_up",
    Recovered => "recovered",
}

impl OpKind {
    /// A step a sequence takes before its action: an approval, or the QUAI wrapped to fund it.
    pub fn is_sequence_step(&self) -> bool {
        matches!(self, OpKind::Approve | OpKind::WrapQuai)
    }

    /// A trade through a DEX router (exact input or exact output).
    pub fn is_router_swap(&self) -> bool {
        matches!(self, OpKind::Swap | OpKind::SwapExactOutput)
    }

    /// Moves an NFT or its listing.
    pub fn is_nft(&self) -> bool {
        matches!(self, OpKind::NftTransfer | OpKind::NftBuy | OpKind::NftList | OpKind::NftReprice | OpKind::NftUnlist)
            || matches!(self, OpKind::Other(name) if name.starts_with("nft"))
    }

    /// Sends value to someone: QUAI, Qi or a token.
    pub fn is_send(&self) -> bool {
        matches!(self, OpKind::SendQuai | OpKind::SendQi | OpKind::SendToken)
            || matches!(self, OpKind::Other(name) if name.starts_with("send"))
    }

    /// Exchanges one asset for another: a swap, a curve trade, a conversion, a wrap or a claim.
    pub fn is_trade(&self) -> bool {
        match self {
            OpKind::Swap
            | OpKind::SwapExactOutput
            | OpKind::CurveBuy
            | OpKind::CurveSell
            | OpKind::CurveClaim
            | OpKind::HartiiBuy
            | OpKind::HartiiSell
            | OpKind::ConvertQuaiToQi
            | OpKind::ConvertQiToQuai
            | OpKind::WrapQi
            | OpKind::ClaimWqi
            | OpKind::UnwrapWqi
            | OpKind::WrapQuai
            | OpKind::UnwrapQuai => true,
            OpKind::Other(k) => k.starts_with("curve") || k.starts_with("convert") || k.contains("wrap") || k.contains("claim"),
            _ => false,
        }
    }
}

impl std::fmt::Display for OpKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl rusqlite::types::ToSql for OpKind {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(rusqlite::types::ToSqlOutput::from(self.as_str()))
    }
}

impl rusqlite::types::FromSql for OpKind {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        value.as_str().map(OpKind::parse)
    }
}

impl Serialize for OpKind {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for OpKind {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Ok(OpKind::parse(&String::deserialize(d)?))
    }
}

/// An operation's (or an observed event's) detail: the JSON object the journal stores, reached
/// through one accessor per key.
///
/// Reading a key that is absent gives `null`, exactly as indexing JSON did, so a reader's own
/// checks (`as_str`, `as_u64`) decide what a missing or mistyped value means. Money paths do not
/// read raw values at all: they take a receipt, which refuses them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Detail(serde_json::Value);

impl Default for Detail {
    fn default() -> Self {
        Detail(serde_json::Value::Object(Default::default()))
    }
}

/// The stored JSON text.
impl std::fmt::Display for Detail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl From<serde_json::Value> for Detail {
    fn from(value: serde_json::Value) -> Self {
        Detail(value)
    }
}

impl Detail {
    /// An empty detail (`{}`).
    pub fn new() -> Self {
        Self::default()
    }

    /// The stored JSON, for writing it out (the database, JSON output, a backup). Never index it:
    /// use the accessors.
    pub fn as_json(&self) -> &serde_json::Value {
        &self.0
    }

    /// The stored JSON, owned.
    pub fn into_json(self) -> serde_json::Value {
        self.0
    }

    fn read(&self, key: &str) -> &serde_json::Value {
        static NULL: serde_json::Value = serde_json::Value::Null;
        self.0.get(key).unwrap_or(&NULL)
    }

    fn write(&mut self, key: &str, value: serde_json::Value) {
        if !self.0.is_object() {
            self.0 = serde_json::Value::Object(Default::default());
        }
        if let Some(map) = self.0.as_object_mut() {
            map.insert(key.to_string(), value);
        }
    }

    fn remove(&mut self, key: &str) -> Option<serde_json::Value> {
        self.0.as_object_mut().and_then(|m| m.remove(key))
    }

    /// Merge another detail's keys into this one (theirs win).
    pub fn merge(&mut self, other: Detail) {
        if let serde_json::Value::Object(theirs) = other.0 {
            for (k, v) in theirs {
                self.write(&k, v);
            }
        }
    }

    /// The stored JSON for the journal's own storage code (merging a patch, the timeline).
    pub(crate) fn json_mut(&mut self) -> &mut serde_json::Value {
        if !self.0.is_object() {
            self.0 = serde_json::Value::Object(Default::default());
        }
        &mut self.0
    }

    /// Any key, read as indexing reads it, for tests that sweep many keys.
    #[cfg(test)]
    pub(crate) fn test_get(&self, key: &str) -> &serde_json::Value {
        self.read(key)
    }

    /// Any key, for tests that build or break a detail on purpose.
    #[cfg(test)]
    pub(crate) fn test_set(&mut self, key: &str, value: serde_json::Value) {
        self.write(key, value);
    }

    /// Debug builds refuse a key no accessor names: a typo in a writer fails every test that
    /// reaches it instead of writing a fact no reader will find.
    pub(crate) fn check_keys(&self) {
        if cfg!(debug_assertions)
            && let Some(map) = self.0.as_object()
        {
            for key in map.keys() {
                debug_assert!(
                    DETAIL_KEYS.contains(&key.as_str()) || EXTRA_KEYS.contains(&key.as_str()),
                    "journal detail key `{key}` has no accessor"
                );
            }
        }
    }

    /// Every key and value, for a detail view that lists what the journal holds.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &serde_json::Value)> {
        self.0.as_object().into_iter().flat_map(|m| m.iter().map(|(k, v)| (k.as_str(), v)))
    }

    /// Whether the detail has no keys at all.
    pub fn is_empty(&self) -> bool {
        self.0.as_object().is_none_or(|m| m.is_empty())
    }
}

macro_rules! keys {
    ($($key:ident, $set:ident, $take:ident;)*) => {
        impl Detail {
            $(
                #[doc = concat!("`", stringify!($key), "`, or `null` when absent.")]
                pub fn $key(&self) -> &serde_json::Value {
                    self.read(stringify!($key))
                }
                #[doc = concat!("Set `", stringify!($key), "`.")]
                pub fn $set(&mut self, value: impl Into<serde_json::Value>) {
                    self.write(stringify!($key), value.into());
                }
                #[doc = concat!("Remove `", stringify!($key), "` and return it.")]
                pub fn $take(&mut self) -> Option<serde_json::Value> {
                    self.remove(stringify!($key))
                }
            )*
        }

        /// Every key this build reads or writes.
        pub const DETAIL_KEYS: &[&str] = &[$(stringify!($key),)*];
    };
}

keys! {
    abi_source, set_abi_source, take_abi_source;
    account_credit, set_account_credit, take_account_credit;
    actual_in, set_actual_in, take_actual_in;
    actual_out, set_actual_out, take_actual_out;
    allocation_count, set_allocation_count, take_allocation_count;
    allocation_index, set_allocation_index, take_allocation_index;
    allocation_input, set_allocation_input, take_allocation_input;
    allocation_minimum, set_allocation_minimum, take_allocation_minimum;
    amount, set_amount, take_amount;
    amount0, set_amount0, take_amount0;
    amount0_min, set_amount0_min, take_amount0_min;
    amount1, set_amount1, take_amount1;
    amount1_min, set_amount1_min, take_amount1_min;
    amount_unknown, set_amount_unknown, take_amount_unknown;
    asset, set_asset, take_asset;
    at, set_at, take_at;
    beneficiary, set_beneficiary, take_beneficiary;
    block, set_block, take_block;
    buyer, set_buyer, take_buyer;
    bytes, set_bytes, take_bytes;
    candidates, set_candidates, take_candidates;
    canonical_tx, set_canonical_tx, take_canonical_tx;
    channel, set_channel, take_channel;
    client, set_client, take_client;
    closed, set_closed, take_closed;
    closed_at, set_closed_at, take_closed_at;
    commitments, set_commitments, take_commitments;
    contract, set_contract, take_contract;
    conversion_effect, set_conversion_effect, take_conversion_effect;
    counterparty, set_counterparty, take_counterparty;
    credit_head, set_credit_head, take_credit_head;
    credit_head_hash, set_credit_head_hash, take_credit_head_hash;
    credit_partial, set_credit_partial, take_credit_partial;
    credit_visibility, set_credit_visibility, take_credit_visibility;
    credited_qits, set_credited_qits, take_credited_qits;
    currency, set_currency, take_currency;
    curve, set_curve, take_curve;
    curve_fee, set_curve_fee, take_curve_fee;
    data, set_data, take_data;
    decimals, set_decimals, take_decimals;
    destination, set_destination, take_destination;
    destination_canonicality, set_destination_canonicality, take_destination_canonicality;
    destination_receipt, set_destination_receipt, take_destination_receipt;
    direction, set_direction, take_direction;
    dirty, set_dirty, take_dirty;
    duration, set_duration, take_duration;
    estimated, set_estimated, take_estimated;
    execution_block, set_execution_block, take_execution_block;
    execution_hash, set_execution_hash, take_execution_hash;
    execution_tx, set_execution_tx, take_execution_tx;
    exit, set_exit, take_exit;
    expected_its, set_expected_its, take_expected_its;
    expected_out, set_expected_out, take_expected_out;
    expected_qits, set_expected_qits, take_expected_qits;
    expires_at, set_expires_at, take_expires_at;
    fields, set_fields, take_fields;
    final_operation, set_final_operation, take_final_operation;
    finality, set_finality, take_finality;
    financial_effects, set_financial_effects, take_financial_effects;
    from_block, set_from_block, take_from_block;
    from_token, set_from_token, take_from_token;
    function, set_function, take_function;
    gauge, set_gauge, take_gauge;
    hash, set_hash, take_hash;
    included_block, set_included_block, take_included_block;
    included_hash, set_included_hash, take_included_hash;
    intent, set_intent, take_intent;
    kind, set_kind, take_kind;
    label, set_label, take_label;
    legacy_destination_observation, set_legacy_destination_observation, take_legacy_destination_observation;
    liquidity, set_liquidity, take_liquidity;
    listings, set_listings, take_listings;
    maximum_input, set_maximum_input, take_maximum_input;
    messaging, set_messaging, take_messaging;
    minimum, set_minimum, take_minimum;
    minimum_out, set_minimum_out, take_minimum_out;
    mode, set_mode, take_mode;
    module, set_module, take_module;
    name, set_name, take_name;
    native_refund, set_native_refund, take_native_refund;
    native_value, set_native_value, take_native_value;
    needs_notify, set_needs_notify, take_needs_notify;
    no_change, set_no_change, take_no_change;
    nonce, set_nonce, take_nonce;
    note, set_note, take_note;
    observed_credit_qits, set_observed_credit_qits, take_observed_credit_qits;
    operator, set_operator, take_operator;
    order, set_order, take_order;
    origin, set_origin, take_origin;
    original_tx, set_original_tx, take_original_tx;
    pair, set_pair, take_pair;
    path, set_path, take_path;
    peer, set_peer, take_peer;
    percent, set_percent, take_percent;
    pid, set_pid, take_pid;
    plan_id, set_plan_id, take_plan_id;
    pool, set_pool, take_pool;
    pool_contracts, set_pool_contracts, take_pool_contracts;
    price, set_price, take_price;
    private_fields, set_private_fields, take_private_fields;
    purpose, set_purpose, take_purpose;
    quai_lock, set_quai_lock, take_quai_lock;
    quantity, set_quantity, take_quantity;
    quote_mode, set_quote_mode, take_quote_mode;
    raw, set_raw, take_raw;
    quoted_its, set_quoted_its, take_quoted_its;
    quoted_qits, set_quoted_qits, take_quoted_qits;
    recipient, set_recipient, take_recipient;
    recipients, set_recipients, take_recipients;
    recovered, set_recovered, take_recovered;
    recovered_signed, set_recovered_signed, take_recovered_signed;
    refund, set_refund, take_refund;
    refund_credit, set_refund_credit, take_refund_credit;
    refunds_excess, set_refunds_excess, take_refunds_excess;
    reorged_anchor, set_reorged_anchor, take_reorged_anchor;
    replacement_won, set_replacement_won, take_replacement_won;
    required_input, set_required_input, take_required_input;
    review, set_review, take_review;
    review_version, set_review_version, take_review_version;
    revision, set_revision, take_revision;
    reward, set_reward, take_reward;
    route, set_route, take_route;
    router, set_router, take_router;
    s, set_s, take_s;
    sale, set_sale, take_sale;
    scan_last_hash, set_scan_last_hash, take_scan_last_hash;
    scan_last_number, set_scan_last_number, take_scan_last_number;
    scan_next, set_scan_next, take_scan_next;
    sealed, set_sealed, take_sealed;
    seller, set_seller, take_seller;
    sequence, set_sequence, take_sequence;
    slippage_bps, set_slippage_bps, take_slippage_bps;
    source, set_source, take_source;
    source_canonicality, set_source_canonicality, take_source_canonicality;
    spendability, set_spendability, take_spendability;
    spender, set_spender, take_spender;
    split, set_split, take_split;
    stage, set_stage, take_stage;
    standard, set_standard, take_standard;
    submission_error, set_submission_error, take_submission_error;
    symbol, set_symbol, take_symbol;
    through_block, set_through_block, take_through_block;
    timed, set_timed, take_timed;
    timeline, set_timeline, take_timeline;
    to_decimals, set_to_decimals, take_to_decimals;
    to_symbol, set_to_symbol, take_to_symbol;
    to_token, set_to_token, take_to_token;
    token, set_token, take_token;
    token0, set_token0, take_token0;
    token1, set_token1, take_token1;
    token_id, set_token_id, take_token_id;
    total_input, set_total_input, take_total_input;
    tx, set_tx, take_tx;
    undeclared, set_undeclared, take_undeclared;
    unlimited, set_unlimited, take_unlimited;
    unlock_height, set_unlock_height, take_unlock_height;
    unobserved_qits, set_unobserved_qits, take_unobserved_qits;
    unrelated_balance_change, set_unrelated_balance_change, take_unrelated_balance_change;
    value, set_value, take_value;
    venue, set_venue, take_venue;
    verified, set_verified, take_verified;
    weekly, set_weekly, take_weekly;
}

/// Keys writers store for display or export only, read by no accessor (a detail view lists them).
pub const EXTRA_KEYS: &[&str] = &[];

/// A base-unit amount stored as a decimal string.
fn atoms(value: &serde_json::Value) -> Option<U256> {
    value.as_str().filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).and_then(|s| U256::from_str_radix(s, 10).ok())
}

impl Detail {
    /// `actual_out` as base units: what a trade's receipt showed arriving.
    pub fn actual_out_atoms(&self) -> Option<U256> {
        atoms(self.actual_out())
    }

    /// `expected_out` as base units.
    pub fn expected_out_atoms(&self) -> Option<U256> {
        atoms(self.expected_out())
    }

    /// `native_value` as base units.
    pub fn native_value_atoms(&self) -> Option<U256> {
        atoms(self.native_value())
    }

    /// `split.allocation_index`: which allocation of a split plan a swap filled.
    pub fn split_allocation_index(&self) -> Option<u64> {
        self.split()["allocation_index"].as_u64()
    }

    /// `to_decimals`, when it is a real ERC-20 decimal count (at most 77: 10^77 still fits U256).
    pub fn to_decimals_u8(&self) -> Option<u8> {
        self.to_decimals().as_u64().filter(|v| *v <= 77).map(|v| v as u8)
    }
}

fn included(op: &crate::appdb::Operation) -> bool {
    matches!(op.status, crate::appdb::OpStatus::Confirmed | crate::appdb::OpStatus::Settled)
}

/// Why a receipt was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiptError {
    /// Not this kind, not included, signed by another account, or naming no output.
    NotMatching,
    /// Its output was never attributed from the chain's receipt.
    OutputUnknown,
    /// Its output token's decimals are not recorded (or not a real decimal count).
    UnitsUnknown,
}

/// What a router swap delivered, from its receipt: the only facts a plan advances a swap on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwapReceipt {
    /// The token that arrived.
    pub to_token: String,
    /// Base units that arrived, attributed from the receipt. Zero is a real (if unlikely) answer;
    /// an absent figure is refused instead.
    pub actual_out: U256,
    /// Its decimals, when recorded.
    pub to_decimals: Option<u8>,
}

impl SwapReceipt {
    /// The receipt of `op`, which must be an included router swap by `owner`, with its output
    /// token named and its output attributed.
    pub fn of(op: &crate::appdb::Operation, owner: &str) -> std::result::Result<SwapReceipt, ReceiptError> {
        if op.kind != OpKind::Swap || !included(op) || !op.account.eq_ignore_ascii_case(owner) {
            return Err(ReceiptError::NotMatching);
        }
        let to_token = op.detail.to_token().as_str().filter(|t| !t.is_empty()).ok_or(ReceiptError::NotMatching)?;
        let actual_out = op.detail.actual_out_atoms().ok_or(ReceiptError::OutputUnknown)?;
        Ok(SwapReceipt { to_token: to_token.to_string(), actual_out, to_decimals: op.detail.to_decimals_u8() })
    }

    /// Whether the output is `token`.
    pub fn delivered(&self, token: &str) -> bool {
        self.to_token.eq_ignore_ascii_case(token)
    }

    /// The output's decimals, required.
    pub fn decimals(&self) -> std::result::Result<u8, ReceiptError> {
        self.to_decimals.ok_or(ReceiptError::UnitsUnknown)
    }
}

/// A settled Qi → WQI deposit (the first step of wrapping Qi).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrapDeposit {
    /// The account the wrapped Qi was credited to.
    pub beneficiary: String,
    /// Qits deposited.
    pub qits: U256,
}

impl WrapDeposit {
    /// The deposit `op` made, which must be a settled `wrap_qi` naming its beneficiary.
    pub fn of(op: &crate::appdb::Operation) -> std::result::Result<WrapDeposit, ReceiptError> {
        if op.kind != OpKind::WrapQi || op.status != crate::appdb::OpStatus::Settled {
            return Err(ReceiptError::NotMatching);
        }
        let beneficiary = op.detail.beneficiary().as_str().filter(|b| !b.is_empty()).ok_or(ReceiptError::NotMatching)?;
        let qits = atoms(&serde_json::Value::String(op.amount.clone())).ok_or(ReceiptError::NotMatching)?;
        Ok(WrapDeposit { beneficiary: beneficiary.to_string(), qits })
    }
}

/// An included WQI claim and what it delivered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WqiClaim {
    /// The wrapped token that arrived.
    pub to_token: String,
    /// Base units that arrived.
    pub actual_out: U256,
}

impl WqiClaim {
    /// The claim `op` made, which must be an included `claim_wqi` by `owner`.
    pub fn of(op: &crate::appdb::Operation, owner: &str) -> std::result::Result<WqiClaim, ReceiptError> {
        if op.kind != OpKind::ClaimWqi || !included(op) || !op.account.eq_ignore_ascii_case(owner) {
            return Err(ReceiptError::NotMatching);
        }
        let to_token = op.detail.to_token().as_str().filter(|t| !t.is_empty()).ok_or(ReceiptError::NotMatching)?;
        let actual_out = op.detail.actual_out_atoms().ok_or(ReceiptError::OutputUnknown)?;
        Ok(WqiClaim { to_token: to_token.to_string(), actual_out })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Rng;

    #[test]
    fn every_known_kind_round_trips_through_its_name() {
        let mut names = std::collections::HashSet::new();
        for kind in OpKind::KNOWN {
            assert!(names.insert(kind.as_str()), "two kinds share {}", kind.as_str());
            assert_eq!(&OpKind::parse(kind.as_str()), kind);
            let json = serde_json::to_string(kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
            assert_eq!(&serde_json::from_str::<OpKind>(&json).unwrap(), kind);
        }
    }

    /// Fuzz: any stored name comes back exactly as it was written, known or not.
    #[test]
    fn unknown_kinds_are_kept_exactly() {
        for seed in 0..5_000u64 {
            let mut rng = Rng::new(seed);
            let len = rng.below(24);
            let name: String = (0..len).map(|_| char::from_u32(0x20 + rng.below(0x5f) as u32).unwrap()).collect();
            let kind = OpKind::parse(&name);
            assert_eq!(kind.as_str(), name);
            let back: OpKind = serde_json::from_str(&serde_json::to_string(&kind).unwrap()).unwrap();
            assert_eq!(back, kind);
        }
        assert_eq!(OpKind::parse("message_dm_v9"), OpKind::Other("message_dm_v9".into()));
    }

    fn random_json(rng: &mut Rng, depth: u32) -> serde_json::Value {
        use serde_json::Value;
        match if depth > 2 { rng.below(5) } else { rng.below(7) } {
            0 => Value::Null,
            1 => Value::Bool(rng.below(2) == 0),
            2 => serde_json::json!(rng.next() >> rng.below(64)),
            3 => serde_json::json!(-(rng.below(1000) as i64)),
            4 => Value::String((0..rng.below(12)).map(|_| char::from_u32(0x20 + rng.below(0x3000) as u32).unwrap_or('?')).collect()),
            5 => Value::Array((0..rng.below(4)).map(|_| random_json(rng, depth + 1)).collect()),
            _ => {
                let mut map = serde_json::Map::new();
                for _ in 0..rng.below(6) {
                    let key = if rng.below(2) == 0 {
                        DETAIL_KEYS[rng.below(DETAIL_KEYS.len())].to_string()
                    } else {
                        format!("k{}", rng.below(50))
                    };
                    map.insert(key, random_json(rng, depth + 1));
                }
                Value::Object(map)
            }
        }
    }

    /// Fuzz: whatever JSON a journal row holds, `Detail` stores it back byte for byte, and every
    /// accessor reads exactly what indexing the JSON read. This is the "migration": there is none
    /// to run, and nothing is lost.
    #[test]
    fn details_round_trip_and_read_as_indexing_did() {
        for seed in 0..5_000u64 {
            let mut rng = Rng::new(seed);
            let value = random_json(&mut rng, 0);
            let text = serde_json::to_string(&value).unwrap();
            let detail: Detail = serde_json::from_str(&text).unwrap();
            assert_eq!(serde_json::to_string(&detail).unwrap(), text, "seed {seed}");
            assert_eq!(detail.as_json(), &value);
            for key in DETAIL_KEYS {
                assert_eq!(detail.read(key), &value[*key], "seed {seed}, {key}");
            }
        }
    }

    #[test]
    fn setters_make_an_object_and_keep_the_other_keys() {
        let mut d = Detail::from(serde_json::json!({"z_unknown": [1, 2]}));
        d.set_actual_out("15");
        d.set_to_decimals(6u64);
        assert_eq!(d.actual_out_atoms(), Some(U256::from(15)));
        assert_eq!(d.to_decimals_u8(), Some(6));
        assert_eq!(d.as_json()["z_unknown"], serde_json::json!([1, 2]));
        let mut not_object = Detail::from(serde_json::json!("text"));
        not_object.set_name("x");
        assert_eq!(not_object.name(), "x");
        assert_eq!(d.take_actual_out(), Some(serde_json::json!("15")));
        assert!(d.actual_out().is_null());
    }

    #[test]
    fn amounts_accept_only_decimal_digits() {
        for bad in ["", "0x10", "-1", "1.5", " 1", "1e3", "１"] {
            let d = Detail::from(serde_json::json!({ "actual_out": bad }));
            assert_eq!(d.actual_out_atoms(), None, "{bad:?}");
        }
        let d = Detail::from(serde_json::json!({ "actual_out": 15 }));
        assert_eq!(d.actual_out_atoms(), None, "a number is not the stored form");
        let d = Detail::from(serde_json::json!({ "to_decimals": 78 }));
        assert_eq!(d.to_decimals_u8(), None);
    }

    fn op(kind: OpKind, status: crate::appdb::OpStatus, detail: serde_json::Value) -> crate::appdb::Operation {
        crate::appdb::Operation {
            id: "01".into(),
            network: "local".into(),
            kind,
            store: "quai".into(),
            account: "0x00AA".into(),
            status,
            tx_hash: None,
            asset: "T".into(),
            amount: "1000".into(),
            counterparty: String::new(),
            fee: String::new(),
            detail: detail.into(),
            created: 0,
            updated: 0,
        }
    }

    #[test]
    fn swap_receipts_refuse_what_they_cannot_prove() {
        use crate::appdb::OpStatus::*;
        let good = serde_json::json!({"to_token": "0xT", "actual_out": "42", "to_decimals": 18});
        let r = SwapReceipt::of(&op(OpKind::Swap, Confirmed, good.clone()), "0x00aa").unwrap();
        assert_eq!((r.to_token.as_str(), r.actual_out, r.to_decimals), ("0xT", U256::from(42), Some(18)));
        assert!(r.delivered("0xt") && !r.delivered("0xU"));
        assert!(SwapReceipt::of(&op(OpKind::Swap, Settled, good.clone()), "0x00aa").is_ok());
        // Not included, another signer, another kind.
        assert!(SwapReceipt::of(&op(OpKind::Swap, Submitted, good.clone()), "0x00aa").is_err());
        assert!(SwapReceipt::of(&op(OpKind::Swap, Confirmed, good.clone()), "0x00bb").is_err());
        assert!(SwapReceipt::of(&op(OpKind::SwapExactOutput, Confirmed, good.clone()), "0x00aa").is_err());
        assert!(SwapReceipt::of(&op(OpKind::Other("swap ".into()), Confirmed, good), "0x00aa").is_err());
        // Each fact missing or malformed, and what the refusal says.
        for (broken, why) in [
            (serde_json::json!({"actual_out": "42", "to_decimals": 18}), ReceiptError::NotMatching),
            (serde_json::json!({"to_token": "", "actual_out": "42", "to_decimals": 18}), ReceiptError::NotMatching),
            (serde_json::json!({"to_token": "0xT", "to_decimals": 18}), ReceiptError::OutputUnknown),
            (serde_json::json!({"to_token": "0xT", "actual_out": 42, "to_decimals": 18}), ReceiptError::OutputUnknown),
            (serde_json::json!({"to_token": "0xT", "actual_out": "4 2", "to_decimals": 18}), ReceiptError::OutputUnknown),
        ] {
            assert_eq!(SwapReceipt::of(&op(OpKind::Swap, Confirmed, broken.clone()), "0x00aa"), Err(why), "{broken}");
        }
        // Zero is an answer; missing or impossible decimals are unknown units.
        let zero =
            SwapReceipt::of(&op(OpKind::Swap, Confirmed, serde_json::json!({"to_token": "0xT", "actual_out": "0"})), "0x00aa").unwrap();
        assert_eq!((zero.actual_out, zero.decimals()), (U256::ZERO, Err(ReceiptError::UnitsUnknown)));
        let huge = SwapReceipt::of(
            &op(OpKind::Swap, Confirmed, serde_json::json!({"to_token": "0xT", "actual_out": "1", "to_decimals": 99})),
            "0x00aa",
        )
        .unwrap();
        assert_eq!(huge.decimals(), Err(ReceiptError::UnitsUnknown));
    }

    #[test]
    fn deposits_and_claims_are_checked_the_same_way() {
        use crate::appdb::OpStatus::*;
        let d = WrapDeposit::of(&op(OpKind::WrapQi, Settled, serde_json::json!({"beneficiary": "0xB"}))).unwrap();
        assert_eq!((d.beneficiary.as_str(), d.qits), ("0xB", U256::from(1000)));
        assert!(WrapDeposit::of(&op(OpKind::WrapQi, Confirmed, serde_json::json!({"beneficiary": "0xB"}))).is_err());
        assert!(WrapDeposit::of(&op(OpKind::WrapQi, Settled, serde_json::json!({}))).is_err());
        let c =
            WqiClaim::of(&op(OpKind::ClaimWqi, Confirmed, serde_json::json!({"to_token": "0xW", "actual_out": "7"})), "0x00AA").unwrap();
        assert_eq!(c.actual_out, U256::from(7));
        assert_eq!(
            WqiClaim::of(&op(OpKind::ClaimWqi, Confirmed, serde_json::json!({"to_token": "0xW"})), "0x00AA"),
            Err(ReceiptError::OutputUnknown)
        );
        assert_eq!(
            WqiClaim::of(&op(OpKind::ClaimWqi, Submitted, serde_json::json!({"to_token": "0xW", "actual_out": "7"})), "0x00AA"),
            Err(ReceiptError::NotMatching)
        );
    }
}
