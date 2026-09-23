//! Prepare → review → commit (sign, persist, broadcast) for every value-changing operation.

use crate::amount;
use crate::appdb::{OpStatus, Operation};
use crate::error::{CoreError, Result};
use crate::registry::QuaiAccount;
use crate::session::{Session, op_hex, parse_op_id, qi_stale, stale_pause};
use quai_sdk::U256;
use quai_sdk::accounts::{
    AccountError, AccountIntent, AccountSession, PreparedAccountReplacement, PreparedAccountTransaction, ReplacementPolicy,
};
use quai_sdk::consensus::{QiOutput, QiTransaction, QuaiTransaction};
use quai_sdk::primitives::{Address, Ledger};
use quai_sdk::provider::{BroadcastError, QiFeeProfile};
use quai_sdk::qi::QiPolicy;
use quai_sdk::qi::{PreparedQiOperation, PreparedQiReplacement, PreparedQiTransaction, QiError, QiReplacementIntent, QiSession};
use serde::{Deserialize, Serialize};

/// One row of a review.
#[derive(Clone, Debug, Serialize)]
pub struct Field {
    /// Label.
    pub label: String,
    /// Full value (never truncated).
    pub value: String,
}

/// A Qi input or output line.
#[derive(Clone, Debug, Serialize)]
pub struct CoinLine {
    /// Address or outpoint.
    pub address: String,
    /// Qits.
    pub qits: u64,
    /// `input`, `recipient`, `change`.
    pub role: String,
}

/// Everything shown before authorization, generated from the frozen payload.
#[derive(Clone, Debug, Serialize)]
pub struct Review {
    /// Operation id (used to commit or discard).
    pub op_id: String,
    /// Operation kind.
    pub kind: String,
    /// Title for display.
    pub title: String,
    /// Network id and chain.
    pub network: String,
    /// Source (account address or `Qi wallet`).
    pub from: String,
    /// Destination as signed.
    pub to: String,
    /// Asset label.
    pub asset: String,
    /// Human amount.
    pub amount: String,
    /// Base-unit amount.
    pub amount_base: String,
    /// Maximum fee, human with asset label.
    pub max_fee: String,
    /// Fee as basis points of amount when meaningful.
    pub fee_bps: Option<u64>,
    /// Additional exact fields (nonce, gas, data, slippage, digest...).
    pub fields: Vec<Field>,
    /// Qi inputs/outputs.
    pub coins: Vec<CoinLine>,
    /// Warnings to display prominently.
    pub warnings: Vec<String>,
    /// Assets pictured next to the amounts (token icons, NFT thumbnail). Presentation only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub visuals: Vec<ReviewVisual>,
    /// The fee is above the network fee policy (highlighted; approving still sends).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fee_over_policy: bool,
    /// What this does to the wallet's balances, asset by asset: what leaves, what arrives, and
    /// the fee. The one place a review says it in a single glance.
    #[serde(default)]
    pub changes: Vec<BalanceChange>,
}

/// One asset's movement in a review.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct BalanceChange {
    /// `out`, `in`, `fee`, or `none` (nothing moves, e.g. an approval).
    pub direction: String,
    /// Asset symbol or NFT name.
    pub asset: String,
    /// Unsigned human amount; `≈` in front when it is an estimate.
    pub amount: String,
    /// Bound or explanation: `at most`, `at least 51.68 USDT`, …
    pub note: String,
}

/// Versioned, base-unit effects supplied by the same builder as the signed intent.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FinancialEffect {
    pub direction: String,
    pub asset: String,
    pub token: String,
    pub decimals: u8,
    pub amount: String,
    #[serde(default)]
    pub minimum: Option<String>,
    #[serde(default)]
    pub estimated: bool,
    #[serde(default)]
    pub note: String,
}

fn financial_effects(detail: &serde_json::Value) -> Result<Option<Vec<FinancialEffect>>> {
    let Some(value) = detail.get("financial_effects") else { return Ok(None) };
    let effects: Vec<FinancialEffect> = serde_json::from_value(value.clone())?;
    for effect in &effects {
        if !matches!(effect.direction.as_str(), "in" | "out")
            || effect.asset.is_empty()
            || U256::from_str_radix(&effect.amount, 10).is_err()
            || effect.minimum.as_ref().is_some_and(|v| U256::from_str_radix(v, 10).is_err())
        {
            return Err(CoreError::Invalid("invalid financial effects in transaction review".into()));
        }
    }
    if effects.is_empty() {
        return Err(CoreError::Invalid("value-moving review has no financial effects".into()));
    }
    Ok(Some(effects))
}

/// Kinds whose `amount` is a token leaving the wallet (native QUAI is read from the value).
const TOKEN_OUT: &[&str] = &["send_token", "swap", "curve_sell", "unwrap_wqi", "unwrap_quai", "stake", "incentivize"];

/// The balance changes of an account or Qi operation, from what the review already knows:
/// the native value (exact), the amount and its kind, the op detail's expected output, and the fee.
pub fn balance_changes(
    kind: &str,
    asset: &str,
    amount_base: U256,
    decimals: u8,
    native_value: U256,
    fee: (U256, &str, u8),
    detail: &serde_json::Value,
) -> Vec<BalanceChange> {
    let show = |v: U256, d: u8| amount::group_thousands(&amount::format_amount_short(v, d, 6));
    let change = |direction: &str, asset: &str, amount: String, note: &str| BalanceChange {
        direction: direction.into(),
        asset: asset.into(),
        amount,
        note: note.into(),
    };
    let base = |k: &str| -> Option<U256> {
        match &detail[k] {
            serde_json::Value::String(s) => U256::from_str_radix(s, 10).ok(),
            serde_json::Value::Number(n) => n.as_u64().map(U256::from),
            _ => None,
        }
    };
    let text = |k: &str| detail[k].as_str().unwrap_or_default().to_string();
    let mut out = Vec::new();
    match financial_effects(detail) {
        Ok(Some(effects)) => {
            for effect in effects {
                let atoms = U256::from_str_radix(&effect.amount, 10).expect("validated effects");
                let shown = show(atoms, effect.decimals);
                let mut note = effect.note;
                if let Some(minimum) = effect.minimum {
                    let minimum = U256::from_str_radix(&minimum, 10).expect("validated minimum");
                    if !note.is_empty() {
                        note.push_str("; ");
                    }
                    note.push_str(&format!("at least {} {}", show(minimum, effect.decimals), effect.asset));
                }
                out.push(change(&effect.direction, &effect.asset, if effect.estimated { format!("≈ {shown}") } else { shown }, &note));
            }
            out.push(change("fee", fee.1, show(fee.0, fee.2), "network fee, at most"));
            return out;
        }
        Err(_) => {
            out.push(change("none", "", String::new(), "financial changes unavailable; review cannot be executed"));
            out.push(change("fee", fee.1, show(fee.0, fee.2), "network fee, at most"));
            return out;
        }
        Ok(None) => {}
    }
    if !native_value.is_zero() {
        out.push(change("out", "QUAI", show(native_value, amount::QUAI_DECIMALS), ""));
    }
    let token = !asset.eq_ignore_ascii_case("QUAI") && !asset.eq_ignore_ascii_case("QI");
    if token && !amount_base.is_zero() && TOKEN_OUT.contains(&kind) {
        out.push(change("out", asset, show(amount_base, decimals), ""));
    }
    if matches!(kind, "send_qi" | "wrap_qi" | "convert_qi_to_quai") && !amount_base.is_zero() {
        out.push(change("out", "Qi", show(amount_base, amount::QI_DECIMALS), ""));
    }
    let nft = || detail["name"].as_str().filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| format!("#{}", text("token_id")));
    match kind {
        "nft_transfer" => out.push(change("out", &nft(), "1".into(), "NFT")),
        "nft_buy" => out.push(change("in", &nft(), "1".into(), "NFT")),
        "swap" | "curve_buy" => {
            let d = detail["to_decimals"].as_u64().unwrap_or(18) as u8;
            if let Some(v) = base("expected_out") {
                let floor = base("minimum_out").map(|m| format!("at least {} {}", show(m, d), text("to_symbol"))).unwrap_or_default();
                out.push(change("in", &text("to_symbol"), format!("≈ {}", show(v, d)), &floor));
            }
        }
        "wrap_quai" => out.push(change("in", "WQUAI", show(amount_base, decimals), "1:1")),
        "unwrap_quai" => out.push(change("in", "QUAI", show(amount_base, decimals), "1:1")),
        "unwrap_wqi" => out.push(change("in", "Qi", show(amount_base, decimals), "whole Qi, 1:1")),
        // What the node expects to arrive after the protocol's discount, not the spot rate: at size
        // the two differ by an order of magnitude. The floor is where the slippage refunds instead.
        "convert_quai_to_qi" | "convert_qi_to_quai" => {
            let (unit, d, expected, spot) = if kind == "convert_quai_to_qi" {
                ("Qi", amount::QI_DECIMALS, base("expected_qits"), base("quoted_qits"))
            } else {
                ("QUAI", 18, base("expected_its"), base("quoted_its"))
            };
            let floor = spot.zip(detail["slippage_bps"].as_u64()).map(|(spot, bps)| {
                let kept = U256::from(10_000u64.saturating_sub(bps.min(10_000)));
                format!("; refunded if under {}", show(spot * kept / U256::from(10_000u64), d))
            });
            match (expected, spot) {
                (Some(v), _) => out.push(change(
                    "in",
                    unit,
                    format!("≈ {}", show(v, d)),
                    &format!("after the protocol discount, locked ~2 weeks{}", floor.unwrap_or_default()),
                )),
                (None, Some(v)) => out.push(change(
                    "in",
                    unit,
                    format!("≤ {}", show(v, d)),
                    "at the spot rate; the protocol discount comes off this, locked ~2 weeks",
                )),
                (None, None) => {}
            }
        }
        _ => {}
    }
    if out.is_empty() {
        let why = if kind == "approve" {
            "an allowance only: nothing leaves until it is used"
        } else {
            "asset changes unknown; inspect operation details"
        };
        out.push(change("none", "", String::new(), why));
    }
    out.push(change("fee", fee.1, show(fee.0, fee.2), "network fee, at most"));
    out
}

/// An asset a review can picture. Amounts stay text; pictures never carry meaning alone.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ReviewVisual {
    /// `pay`, `receive`, `token` or `nft`.
    pub role: String,
    /// Symbol or item name.
    pub symbol: String,
    /// Contract address, or `quai` / `qi` for the native coins.
    pub contract: String,
    /// NFT token id.
    pub token_id: Option<String>,
}

/// What a review pictures, from the operation kind and its journal detail.
pub fn review_visuals(kind: &str, asset: &str, detail: &serde_json::Value) -> Vec<ReviewVisual> {
    let text = |k: &str| detail[k].as_str().map(str::to_string);
    let visual = |role: &str, symbol: String, contract: String, token_id: Option<String>| ReviewVisual {
        role: role.into(),
        symbol,
        contract,
        token_id,
    };
    match kind {
        "swap" => {
            let mut out = vec![visual("pay", asset.to_string(), text("from_token").unwrap_or_else(|| "quai".into()), None)];
            if let (Some(symbol), Some(to)) = (text("to_symbol"), text("to_token")) {
                out.push(visual("receive", symbol, to, None));
            }
            out
        }
        "nft_buy" | "nft_transfer" | "nft_list" | "nft_reprice" | "nft_unlist" => match (text("contract"), text("token_id")) {
            (Some(c), Some(id)) => {
                vec![visual("nft", text("name").filter(|n| !n.is_empty()).unwrap_or_else(|| format!("#{id}")), c, Some(id))]
            }
            _ => vec![],
        },
        "send_quai" | "wrap_quai" => vec![visual("token", "QUAI".into(), "quai".into(), None)],
        "send_qi" | "wrap_qi" | "sweep_qi" | "aggregate_qi" => vec![visual("token", "Qi".into(), "qi".into(), None)],
        "convert_quai_to_qi" => vec![visual("pay", "QUAI".into(), "quai".into(), None), visual("receive", "Qi".into(), "qi".into(), None)],
        "convert_qi_to_quai" => vec![visual("pay", "Qi".into(), "qi".into(), None), visual("receive", "QUAI".into(), "quai".into(), None)],
        _ => match text("token").filter(|t| t.starts_with("0x")) {
            Some(token) => vec![visual("token", asset.to_string(), token, None)],
            None => vec![],
        },
    }
}

/// Result of committing a reviewed operation.
#[derive(Clone, Debug, Serialize)]
pub struct Submitted {
    /// Operation id.
    pub op_id: String,
    /// Transaction hash.
    pub tx_hash: String,
    /// Status after submission.
    pub status: OpStatus,
    /// Explorer link.
    pub explorer: Option<String>,
    /// Human message.
    pub message: String,
}

/// A frozen, reserved but unsigned payload bound to this session's store handles.
pub(crate) enum Pending {
    Account { prepared: PreparedAccountTransaction, from: QuaiAccount, op: Operation },
    Replacement { prepared: PreparedAccountReplacement, from: QuaiAccount, op_id: String },
    Qi { prepared: PreparedQiTransaction, op: Operation },
    QiPortable { prepared: crate::qi_exit::PreparedQiSweep, op: Operation },
    QiSpecial { prepared: PreparedQiOperation, op: Operation },
    QiReplacement { prepared: PreparedQiReplacement, op_id: String },
}

/// Parameters for an account-ledger transaction.
pub struct AccountRequest {
    /// Source account.
    pub from: QuaiAccount,
    /// Intent (destination, value, calldata, access list).
    pub intent: AccountIntent,
    /// Journal kind.
    pub kind: String,
    /// Display title.
    pub title: String,
    /// Asset label for the journal.
    pub asset: String,
    /// Amount (base units of `asset`) for the journal.
    pub amount: U256,
    /// Asset decimals for display.
    pub decimals: u8,
    /// Counterparty for the journal.
    pub counterparty: String,
    /// Extra review fields.
    pub fields: Vec<Field>,
    /// Extra warnings.
    pub warnings: Vec<String>,
    /// JSON detail stored in the journal.
    pub detail: serde_json::Value,
    /// Gas ceiling.
    pub max_gas: u64,
    /// Optional total fee cap (base units).
    pub max_fee: Option<U256>,
}

pub(crate) fn field(label: &str, value: impl Into<String>) -> Field {
    Field { label: label.into(), value: value.into() }
}

fn map_account_error(e: AccountError) -> CoreError {
    CoreError::from(e)
}

/// Apply the same contract-identity warning to every account review family.
fn canonical_identity_warnings(network: &crate::network::NetworkProfile, asset: &str, detail: &serde_json::Value) -> Vec<String> {
    let known = crate::swap::canonical_tokens(network);
    let mut identities = Vec::new();
    for key in ["token", "from_token"] {
        if let Some(address) = detail[key].as_str() {
            identities.push((asset, address));
        }
    }
    if let (Some(symbol), Some(address)) = (detail["to_symbol"].as_str(), detail["to_token"].as_str()) {
        identities.push((symbol, address));
    }
    if let Some(effects) = detail["financial_effects"].as_array() {
        for effect in effects {
            if let (Some(symbol), Some(address)) = (effect["asset"].as_str(), effect["token"].as_str()) {
                identities.push((symbol, address));
            }
        }
    }
    let mut warnings = Vec::new();
    for (symbol, address) in identities {
        if address.starts_with("0x")
            && let Some(warning) = crate::swap::lookalike_warning(symbol, address, &known)
            && !warnings.contains(&warning)
        {
            warnings.push(warning);
        }
    }
    warnings
}

fn validate_review_expiry(detail: &serde_json::Value, now: u64) -> Result<()> {
    if let Some(deadline) = detail.get("expires_at") {
        let deadline = deadline.as_u64().ok_or_else(|| CoreError::Invalid("invalid frozen review expiry".into()))?;
        if now >= deadline {
            return Err(CoreError::Rejected("review expired; discard it and request a fresh review".into()));
        }
    }
    Ok(())
}

impl Session {
    fn signer_for(&self, from: &QuaiAccount) -> Result<quai_sdk::signer::LocalSigner> {
        let address: Address = from.address.parse().map_err(|_| CoreError::Storage("invalid account address".into()))?;
        self.keys()?.quai_signer(address, from.hd_index, self.network.chain_id)
    }

    /// Released, never-signed nonces for `from` at or above the node's confirmed nonce.
    ///
    /// The SDK never rewinds its nonce cursor, so a rejected review leaves its nonce unused.
    /// Every later transaction from the account would queue behind that gap, so the next
    /// account transaction reuses it (see `prepare_account`). Returns `(nonce, id)` sorted.
    pub async fn nonce_gaps(&self, from: &QuaiAccount) -> Result<Vec<(u64, quai_sdk::wallet::storage::ReservationId)>> {
        use quai_sdk::wallet::storage::ReservationState;
        let address: quai_sdk::QuaiAddress = from.address.parse().map_err(|_| CoreError::Storage("invalid account address".into()))?;
        let mut released = Vec::new();
        let mut after = None;
        loop {
            let page = self.quai_store.reservations(after, 1000)?;
            let Some(last) = page.last() else { break };
            after = Some(last.id);
            for r in &page {
                if r.state == ReservationState::Released
                    && r.transaction.is_none()
                    && let Some((owner, nonce)) = self.quai_store.reserved_nonce(r.id)?
                    && owner == address
                {
                    released.push((nonce, r.id));
                }
            }
            if page.len() < 1000 {
                break;
            }
        }
        if released.is_empty() {
            return Ok(released);
        }
        let confirmed = self.node.provider.transaction_count(address, quai_sdk::BlockTag::Latest).await?;
        released.retain(|(nonce, _)| *nonce >= confirmed);
        released.sort_by_key(|(nonce, _)| *nonce);
        Ok(released)
    }

    /// Prepare an account transaction and return its review. Keys must be unlocked.
    /// Reuses the lowest released nonce when a rejected review left a gap.
    pub async fn prepare_account(&mut self, req: AccountRequest) -> Result<Review> {
        self.require_execution_source()?;
        let _commitment_guard = self.commitment_lock(&req.from.address)?;
        let mut commitments =
            crate::commitments::Commitments::from_intent(&req.kind, &req.amount.to_string(), req.intent.value, U256::ZERO, &req.detail)?;
        financial_effects(&req.detail)?;
        // Anything that sends value somewhere says what the wallet knows about that somewhere —
        // lookalikes and dust-only senders first, above every other warning (`recipient.rs`).
        let mut req = req;
        if req.kind == "approve" && req.amount.is_zero() {
            req.title = format!("Clear {} allowance", req.asset);
            req.warnings.push("This transaction sets allowance to zero. Any new allowance requires a separate review.".into());
        }
        if matches!(req.kind.as_str(), "send_quai" | "send_token" | "nft_transfer") {
            let mut first = self.recipient_warnings(&req.counterparty);
            first.append(&mut req.warnings);
            req.warnings = first;
        }
        let gap = self.nonce_gaps(&req.from).await?.first().copied();
        let id = match gap {
            Some((_, gap_id)) => gap_id,
            None => crate::session::new_operation_id()?,
        };
        let _operation_guard = self.operation_lock(id)?;
        if gap.is_some() {
            self.quai_store.reopen_unsigned_nonce(id)?;
        }
        let signer = match self.signer_for(&req.from) {
            Ok(s) => s,
            Err(e) => {
                if gap.is_some() {
                    let _ = self.quai_store.release_unsigned(id);
                }
                return Err(e);
            }
        };
        let policy = self.network.preparation_limits(req.max_gas, req.max_fee)?;
        let observation = self.network.observation_policy();
        let mut attempt = 0;
        let prepared = loop {
            attempt += 1;
            let reserved =
                self.quai_store.reservation(id)?.is_some_and(|r| r.state == quai_sdk::wallet::storage::ReservationState::Reserved);
            let result = {
                let mut session = AccountSession::new(&self.node.provider, &signer, &mut self.quai_store)
                    .map_err(map_account_error)?
                    .with_observation_policy(observation);
                let prepare = async {
                    if reserved {
                        session.prepare_reserved(id, req.intent.clone(), policy).await
                    } else {
                        session.prepare(id, req.intent.clone(), policy).await
                    }
                };
                let started = std::time::Instant::now();
                let result = if attempt == 1 {
                    let (result, _) = tokio::join!(prepare, self.rpc.warm_transport());
                    result
                } else {
                    prepare.await
                };
                crate::diag::timing("transaction.prepare", started);
                result
            };
            match result {
                Ok(p) => break p,
                Err(AccountError::ObservationChanged) if attempt < 4 => {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
                Err(e) => {
                    // The SDK may have allocated before a later estimate/observation failed.
                    if self.quai_store.reservation(id)?.is_some_and(|r| r.state == quai_sdk::wallet::storage::ReservationState::Reserved) {
                        self.quai_store.release_unsigned(id)?;
                    }
                    return Err(map_account_error(e));
                }
            }
        };
        if let Some((nonce, _)) = gap {
            req.warnings.push(format!("reuses nonce {nonce}, left unused by an earlier rejected review"));
        }
        commitments.native_value = prepared.transaction().value.to_string();
        commitments.fee = prepared.maximum_fee().to_string();
        if let Err(error) = self.check_commitments(&req.from.address, &op_hex(id), &commitments).await {
            self.quai_store.release_unsigned(id)?;
            return Err(error);
        }
        if !req.detail.is_object() {
            req.detail = serde_json::json!({});
        }
        req.detail["commitments"] = serde_json::to_value(commitments)?;
        let result = self.finish_account_review(id, prepared, req);
        if result.is_err() {
            self.quai_store.release_unsigned(id)?;
        }
        result
    }

    /// Prepare a Quai→Qi conversion from an account.
    pub async fn prepare_quai_conversion(
        &mut self,
        mut req: AccountRequest,
        destination: quai_sdk::QiAddress,
        slippage_bps: u16,
    ) -> Result<Review> {
        self.require_execution_source()?;
        let _commitment_guard = self.commitment_lock(&req.from.address)?;
        let mut commitments =
            crate::commitments::Commitments::from_intent(&req.kind, &req.amount.to_string(), req.intent.value, U256::ZERO, &req.detail)?;
        if let Some((nonce, _)) = self.nonce_gaps(&req.from).await?.first() {
            return Err(CoreError::Invalid(format!(
                "nonce {nonce} of this account was left unused by a rejected review and would block the conversion; \
                 fill it first with any QUAI send, or `quai-terminal tx fill-gap` (a 0 QUAI self-transfer)"
            )));
        }
        let id = crate::session::new_operation_id()?;
        let signer = self.signer_for(&req.from)?;
        let policy = self.network.preparation_limits(req.max_gas, req.max_fee)?;
        let slippage = quai_sdk::consensus::ConversionSlippage::new(slippage_bps)?;
        let observation = self.network.observation_policy();
        let _operation_guard = self.operation_lock(id)?;
        let mut attempt = 0;
        let result = loop {
            attempt += 1;
            let result = {
                let mut session = AccountSession::new(&self.node.provider, &signer, &mut self.quai_store)
                    .map_err(map_account_error)?
                    .with_observation_policy(observation);
                session.prepare_conversion(id, destination, req.amount, slippage, policy).await
            };
            // A head race before nonce allocation is safe to retry under the same intent.
            // Once allocated the SDK has no reserved-conversion API; never allocate again.
            if matches!(&result, Err(AccountError::ObservationChanged)) && attempt < 4 && self.quai_store.reservation(id)?.is_none() {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                continue;
            }
            break result;
        };
        let prepared = match result {
            Ok(p) => p,
            Err(e) => {
                // The SDK has no reserved-conversion API: release a failed unsigned
                // allocation and require a fresh explicit preparation, never reuse its ID.
                if self.quai_store.reservation(id)?.is_some_and(|r| r.state == quai_sdk::wallet::storage::ReservationState::Reserved) {
                    self.quai_store.release_unsigned(id)?;
                }
                return Err(map_account_error(e));
            }
        };
        req.fields.push(field("Slippage", format!("{} bps ({}%)", slippage_bps, slippage_bps as f64 / 100.0)));
        req.fields.push(field("Qi destination", destination.to_string()));
        commitments.native_value = prepared.transaction().value.to_string();
        commitments.fee = prepared.maximum_fee().to_string();
        if let Err(error) = self.check_commitments(&req.from.address, &op_hex(id), &commitments).await {
            self.quai_store.release_unsigned(id)?;
            return Err(error);
        }
        if !req.detail.is_object() {
            req.detail = serde_json::json!({});
        }
        req.detail["commitments"] = serde_json::to_value(commitments)?;
        let result = self.finish_account_review(id, prepared, req);
        if result.is_err() {
            self.quai_store.release_unsigned(id)?;
        }
        result
    }

    fn finish_account_review(
        &mut self,
        id: quai_sdk::wallet::storage::ReservationId,
        prepared: PreparedAccountTransaction,
        req: AccountRequest,
    ) -> Result<Review> {
        let tx: &QuaiTransaction = prepared.transaction();
        let max_fee = prepared.maximum_fee();
        let mut fields = req.fields;
        fields.extend([
            field("Nonce", tx.nonce.to_string()),
            field("Gas limit", tx.gas_limit.to_string()),
            field("Gas price", format!("{} gwei", amount::format_amount(tx.gas_price, 9))),
            field("Native value", format!("{} QUAI", amount::quai(tx.value))),
        ]);
        if !tx.data.is_empty() {
            fields.push(field("Calldata", format!("0x{}", hex::encode(&tx.data))));
        }
        if !tx.access_list.is_empty() {
            fields.push(field("Access list", tx.access_list.iter().map(|a| a.address.to_string()).collect::<Vec<_>>().join(", ")));
        }
        fields.push(field("Signing digest", prepared.signing_digest().to_string()));
        let fee_bps = if req.asset == "QUAI" { amount::bps(max_fee, req.amount) } else { None };
        let fee_note = self.network.fee_policy_note(tx.gas_price, max_fee);
        let mut warnings = req.warnings;
        if let Some(note) = &fee_note {
            // After the recipient checks: a fee note must never push a poisoning warning down.
            let after = warnings.iter().take_while(|w| crate::recipient::is_recipient_warning(w)).count();
            warnings.insert(after, note.clone());
        }
        if let Some(bps) = fee_bps
            && bps >= 500
        {
            warnings.push(format!("maximum fee is {}.{:02}% of the amount", bps / 100, bps % 100));
        }
        let to = tx.to.map(|a| a.to_string()).unwrap_or_else(|| "(contract creation)".into());
        let visuals = review_visuals(&req.kind, &req.asset, &req.detail);
        let changes = balance_changes(
            &req.kind,
            &req.asset,
            req.amount,
            req.decimals,
            tx.value,
            (max_fee, "QUAI", amount::QUAI_DECIMALS),
            &req.detail,
        );
        let mut op = self.new_op(id, &req.kind, "quai", &req.from.address, &req.asset, req.amount, &req.counterparty, req.detail);
        op.fee = max_fee.to_string();
        // What the transaction itself carries, so activity can state it for any kind of call.
        if !op.detail.is_object() {
            op.detail = serde_json::json!({});
        }
        op.detail["native_value"] = serde_json::json!(tx.value.to_string());
        op.detail["nonce"] = serde_json::json!(tx.nonce);
        let mut review = Review {
            op_id: op_hex(id),
            kind: req.kind.clone(),
            title: req.title,
            network: self.network_label(),
            from: format!("{} ({})", req.from.address, req.from.label),
            to,
            asset: req.asset.clone(),
            amount: format!("{} {}", amount::format_amount(req.amount, req.decimals), req.asset),
            amount_base: req.amount.to_string(),
            max_fee: format!("{} QUAI", amount::quai(max_fee)),
            fee_bps,
            fields,
            coins: vec![],
            warnings,
            visuals,
            fee_over_policy: fee_note.is_some(),
            changes,
        };
        for warning in canonical_identity_warnings(&self.network, &op.asset, &op.detail) {
            if !review.warnings.contains(&warning) {
                review.warnings.push(warning);
            }
        }
        op.detail["review_version"] = serde_json::json!(1);
        op.detail["review"] = serde_json::to_value(&review)?;
        self.journal(op.clone())?;
        self.pending.insert(review.op_id.clone(), Pending::Account { prepared, from: req.from, op });
        Ok(review)
    }

    pub(crate) fn qi_review(
        &mut self,
        op: Operation,
        title: &str,
        to: String,
        tx: &QiTransaction,
        recipient_outputs: usize,
        fee: U256,
        mut fields: Vec<Field>,
        mut warnings: Vec<String>,
        digest: String,
    ) -> Result<Review> {
        let snapshot = self.qi_store.snapshot()?;
        let mut coins = Vec::new();
        for input in &tx.inputs {
            let value = snapshot.coins.iter().find(|c| c.outpoint == input.previous_output).map_or(0, |c| c.denomination.value());
            coins.push(CoinLine {
                address: format!(
                    "{}:{} ({})",
                    input.previous_output.transaction_hash,
                    input.previous_output.index,
                    input.public_key.address()
                ),
                qits: value,
                role: "input".into(),
            });
        }
        for (i, output) in tx.outputs.iter().enumerate() {
            coins.push(CoinLine {
                address: output.address.to_string(),
                qits: output.denomination.value(),
                role: if i < recipient_outputs { "recipient" } else { "change" }.into(),
            });
        }
        let amount_q: U256 = op.amount.parse().unwrap_or(U256::ZERO);
        let fee_bps = amount::bps(fee, amount_q);
        if let Some(bps) = fee_bps
            && bps >= 500
        {
            warnings.push(format!("fee is {}.{:02}% of the amount", bps / 100, bps % 100));
        }
        fields.push(field("Inputs", tx.inputs.len().to_string()));
        fields.push(field("Outputs", tx.outputs.len().to_string()));
        fields.push(field("Signing digest", digest));
        Ok(Review {
            op_id: op.id.clone(),
            kind: op.kind.clone(),
            title: title.into(),
            network: self.network_label(),
            from: "Qi wallet".into(),
            to,
            asset: "QI".into(),
            amount: format!("{} Qi", amount::qi(amount_q)),
            amount_base: op.amount.clone(),
            max_fee: format!("{} Qi", amount::qi(fee)),
            fee_bps,
            fields,
            coins,
            warnings,
            visuals: vec![],
            fee_over_policy: false,
            changes: balance_changes(
                &op.kind,
                "QI",
                amount_q,
                amount::QI_DECIMALS,
                U256::ZERO,
                (fee, "Qi", amount::QI_DECIMALS),
                &op.detail,
            ),
        })
    }

    /// Reviews waiting for a decision.
    pub fn pending_ids(&self) -> Vec<String> {
        self.pending.keys().cloned().collect()
    }

    /// Authorize: sign, persist and broadcast a reviewed operation.
    pub async fn commit(&mut self, op_id: &str) -> Result<Submitted> {
        self.keys()?;
        self.require_execution_source()?;
        let pending_ref = self.pending.get(op_id).ok_or_else(|| CoreError::NotFound(format!("no pending review {op_id}")))?;
        if let Pending::Account { op, .. } = pending_ref {
            validate_review_expiry(&op.detail, crate::registry::now())?;
        }
        let _order_lease = match pending_ref {
            Pending::Account { op, .. } => crate::orders::submission_guard(self, op)?,
            _ => None,
        };
        let commitment = match pending_ref {
            Pending::Account { op, .. } => Some((op.account.clone(), op.id.clone(), crate::commitments::Commitments::from_operation(op)?)),
            Pending::Replacement { prepared, from, op_id } => {
                let op = self.app.operation(op_id)?.ok_or_else(|| CoreError::NotFound("replacement operation".into()))?;
                if let Some(plan) = op.detail["plan_id"].as_str().map(|id| self.app.trade_plan(id)).transpose()?.flatten()
                    && plan.intent["client"] == "order"
                {
                    return Err(CoreError::Rejected(
                        "an order fee bump needs a separately budgeted authorization; exact-candidate rebroadcast remains available".into(),
                    ));
                }
                let mut wanted = crate::commitments::Commitments::from_operation(&op)?;
                let tx = prepared.transaction();
                wanted.fee = tx
                    .gas_price
                    .checked_mul(U256::from(tx.gas_limit))
                    .ok_or_else(|| CoreError::Invalid("replacement fee overflow".into()))?
                    .max(wanted.fee.parse().map_err(|_| CoreError::Storage("invalid commitment fee".into()))?)
                    .to_string();
                Some((from.address.clone(), op.id, wanted))
            }
            _ => None,
        };
        let _commitment_guard = if let Some((owner, id, wanted)) = commitment {
            let guard = self.commitment_lock(&owner)?;
            self.check_commitments(&owner, &id, &wanted).await?;
            Some(guard)
        } else {
            None
        };
        let reservation = match pending_ref {
            Pending::Account { prepared, .. } => prepared.reservation_id(),
            Pending::Replacement { prepared, .. } => prepared.reservation_id(),
            Pending::Qi { prepared, .. } => prepared.reservation_id(),
            Pending::QiSpecial { prepared, .. } => prepared.reservation_id(),
            Pending::QiPortable { prepared, .. } => prepared.reservation_id(),
            Pending::QiReplacement { prepared, .. } => prepared.reservation_id(),
        };
        let _operation_guard = self.operation_lock(reservation)?;
        // Signed and sent through the RPC endpoint, never the monitoring node: it has no hashrate.
        let pending = self.pending.remove(op_id).ok_or_else(|| CoreError::NotFound(format!("no pending review {op_id}")))?;
        match pending {
            Pending::Account { prepared, from, op } => {
                let signer = self.signer_for(&from)?;
                let id = prepared.reservation_id();
                let mut session = AccountSession::new(&self.rpc.provider, &signer, &mut self.quai_store).map_err(map_account_error)?;
                let signing_started = std::time::Instant::now();
                let signed = session.sign(&prepared).map_err(map_account_error)?;
                crate::diag::timing("transaction.sign_and_sdk_persist", signing_started);
                let hash = signed.hash()?.to_string();
                let persist_started = std::time::Instant::now();
                mark_signed(&self.app, &op.id, &hash, None)?;
                crate::diag::timing("transaction.application_persist", persist_started);
                let mut session = AccountSession::new(&self.rpc.provider, &signer, &mut self.quai_store).map_err(map_account_error)?;
                let submission_started = std::time::Instant::now();
                let result = session.broadcast(id).await;
                crate::diag::timing("transaction.submit", submission_started);
                self.finish_broadcast(
                    &op.id,
                    OpStatus::Signed,
                    hash,
                    result.map_err(|e| match e {
                        AccountError::Broadcast(b) => BroadcastOutcome::Broadcast(b),
                        other => BroadcastOutcome::Other(CoreError::from(other)),
                    }),
                )
            }
            Pending::Replacement { prepared, from, op_id } => {
                let signer = self.signer_for(&from)?;
                let id = prepared.reservation_id();
                let mut session = AccountSession::new(&self.rpc.provider, &signer, &mut self.quai_store).map_err(map_account_error)?;
                let signed = session.sign_replacement(&prepared).map_err(map_account_error)?;
                let hash = signed.hash()?;
                drop(session);
                self.record_candidate(&op_id, hash.to_string(), None)?;
                let mut session = AccountSession::new(&self.rpc.provider, &signer, &mut self.quai_store).map_err(map_account_error)?;
                let result = session.broadcast_candidate(id, hash).await;
                self.finish_broadcast(
                    &op_id,
                    OpStatus::Submitted,
                    hash.to_string(),
                    result.map_err(|e| match e {
                        AccountError::Broadcast(b) => BroadcastOutcome::Broadcast(b),
                        other => BroadcastOutcome::Other(CoreError::from(other)),
                    }),
                )
            }
            Pending::Qi { prepared, op } => {
                let id = prepared.reservation_id();
                let keys = self
                    .unlocked
                    .as_ref()
                    .ok_or_else(|| CoreError::Locked("wallet is locked".into()))?
                    .qi_keyring_with_channels(&self.qi_store)?;
                let mut session = QiSession::with_keys(&self.rpc.provider, &keys, &mut self.qi_store);
                let signing_started = std::time::Instant::now();
                let signed = session.sign(&prepared)?;
                crate::diag::timing("transaction.sign_and_sdk_persist", signing_started);
                let hash = signed.hash()?.to_string();
                mark_signed(&self.app, &op.id, &hash, Some(&prepared.fee().to_string()))?;
                let submission_started = std::time::Instant::now();
                let result = session.broadcast(id).await;
                crate::diag::timing("transaction.submit", submission_started);
                drop(session);
                drop(keys);
                let outcome = self.finish_broadcast(
                    &op.id,
                    OpStatus::Signed,
                    hash,
                    result.map_err(|e| match e {
                        QiError::Broadcast(b) => BroadcastOutcome::Broadcast(b),
                        other => BroadcastOutcome::Other(CoreError::from(other)),
                    }),
                )?;
                Ok(outcome)
            }
            Pending::QiPortable { prepared, op } => {
                let id = prepared.reservation_id();
                let keys = self
                    .unlocked
                    .as_ref()
                    .ok_or_else(|| CoreError::Locked("wallet is locked".into()))?
                    .qi_keyring_with_channels(&self.qi_store)?;
                let signing_started = std::time::Instant::now();
                let signed = prepared.sign(&mut self.qi_store, &keys)?;
                crate::diag::timing("transaction.sign_and_sdk_persist", signing_started);
                let hash = signed.hash()?.to_string();
                mark_signed(&self.app, &op.id, &hash, Some(&prepared.fee().to_string()))?;
                let mut session = QiSession::with_keys(&self.rpc.provider, &keys, &mut self.qi_store);
                let submission_started = std::time::Instant::now();
                let result = session.broadcast(id).await;
                crate::diag::timing("transaction.submit", submission_started);
                drop(session);
                drop(keys);
                self.finish_broadcast(
                    &op.id,
                    OpStatus::Signed,
                    hash,
                    result.map_err(|e| match e {
                        QiError::Broadcast(b) => BroadcastOutcome::Broadcast(b),
                        other => BroadcastOutcome::Other(CoreError::from(other)),
                    }),
                )
            }
            Pending::QiSpecial { prepared, op } => {
                let id = prepared.reservation_id();
                let keys = self
                    .unlocked
                    .as_ref()
                    .ok_or_else(|| CoreError::Locked("wallet is locked".into()))?
                    .qi_keyring_with_channels(&self.qi_store)?;
                let mut session = QiSession::with_keys(&self.rpc.provider, &keys, &mut self.qi_store);
                let signing_started = std::time::Instant::now();
                let signed = session.sign_special(&prepared)?;
                crate::diag::timing("transaction.sign_and_sdk_persist", signing_started);
                let hash = signed.hash()?.to_string();
                mark_signed(&self.app, &op.id, &hash, Some(&prepared.fee().to_string()))?;
                let submission_started = std::time::Instant::now();
                let result = session.broadcast(id).await;
                crate::diag::timing("transaction.submit", submission_started);
                drop(session);
                drop(keys);
                self.finish_broadcast(
                    &op.id,
                    OpStatus::Signed,
                    hash,
                    result.map_err(|e| match e {
                        QiError::Broadcast(b) => BroadcastOutcome::Broadcast(b),
                        other => BroadcastOutcome::Other(CoreError::from(other)),
                    }),
                )
            }
            Pending::QiReplacement { prepared, op_id } => {
                let id = prepared.reservation_id();
                let fee = prepared.fee().to_string();
                let keys = self
                    .unlocked
                    .as_ref()
                    .ok_or_else(|| CoreError::Locked("wallet is locked".into()))?
                    .qi_keyring_with_channels(&self.qi_store)?;
                let mut session = QiSession::with_keys(&self.rpc.provider, &keys, &mut self.qi_store);
                let signed = session.sign_replacement(&prepared)?;
                let hash = signed.hash()?;
                drop(session);
                drop(keys);
                self.record_candidate(&op_id, hash.to_string(), Some(&fee))?;
                let keys = self
                    .unlocked
                    .as_ref()
                    .ok_or_else(|| CoreError::Locked("wallet is locked".into()))?
                    .qi_keyring_with_channels(&self.qi_store)?;
                let mut session = QiSession::with_keys(&self.rpc.provider, &keys, &mut self.qi_store);
                let result = session.broadcast_candidate(id, hash).await;
                drop(session);
                drop(keys);
                self.finish_broadcast(
                    &op_id,
                    OpStatus::Submitted,
                    hash.to_string(),
                    result.map_err(|e| match e {
                        QiError::Broadcast(b) => BroadcastOutcome::Broadcast(b),
                        other => BroadcastOutcome::Other(CoreError::from(other)),
                    }),
                )
            }
        }
    }

    /// Record a freshly signed replacement candidate against the operation it replaces: remember
    /// the original hash once, append this candidate to the family, and point the journal row at
    /// the new hash.
    ///
    /// Moved only from the status the row actually had, so a tracker that confirmed the original
    /// in the meantime keeps its word and `finish_broadcast` reports that instead.
    fn record_candidate(&mut self, op_id: &str, hash: String, fee: Option<&str>) -> Result<()> {
        self.app.record_signed_candidate(op_id, &hash, fee)
    }

    /// Record how a broadcast went, moving the operation only from `from`, the status it had when
    /// the broadcast started. The pending lane checks sent transactions every few seconds, so by
    /// the time a slow broadcast returns the transaction may already be confirmed: then its real
    /// status is reported, not overwritten with "unknown" or "not submitted".
    fn finish_broadcast<T>(
        &mut self,
        op_id: &str,
        from: OpStatus,
        hash: String,
        result: std::result::Result<T, BroadcastOutcome>,
    ) -> Result<Submitted> {
        let explorer = self.network.tx_url(&hash);
        let submitted = |status: OpStatus, message: String| Submitted {
            op_id: op_id.into(),
            tx_hash: hash.clone(),
            status,
            explorer: explorer.clone(),
            message,
        };
        // Someone else moved it: say where it stands instead.
        let moved_on = |app: &crate::appdb::AppDb| -> Option<Submitted> {
            let op = app.operation(op_id).ok().flatten()?;
            (op.status != from).then(|| {
                let canonical = op.tx_hash.unwrap_or_else(|| hash.clone());
                Submitted {
                    op_id: op_id.into(),
                    explorer: self.network.tx_url(&canonical),
                    tx_hash: canonical,
                    status: op.status,
                    message: format!("already {}", op.status.as_str()),
                }
            })
        };
        match result {
            Ok(_) => match self.app.transition_operation(op_id, from, OpStatus::Submitted, Some(&hash), None, None) {
                Ok(true) => Ok(submitted(OpStatus::Submitted, "submitted; waiting for inclusion".into())),
                Ok(false) => Ok(moved_on(&self.app).unwrap_or_else(|| submitted(OpStatus::Submitted, "submitted".into()))),
                // The transaction is out; only the journal lagged. Saying "failed" here would
                // invite a second send, and the tracker catches the row up from its hash.
                Err(e) => Ok(submitted(OpStatus::Submitted, format!("submitted; the journal will catch up ({e})"))),
            },
            Err(BroadcastOutcome::Broadcast(b)) if b.acceptance_is_ambiguous() => {
                let patch = serde_json::json!({"submission_error": b.to_string()});
                if !self.app.transition_operation(op_id, from, OpStatus::Unknown, Some(&hash), None, Some(&patch))?
                    && let Some(now) = moved_on(&self.app)
                {
                    return Ok(now);
                }
                Ok(submitted(
                    OpStatus::Unknown,
                    format!("submission outcome unknown ({b}); the signed transaction is saved — run `tx reconcile` before retrying"),
                ))
            }
            Err(BroadcastOutcome::Broadcast(b)) => {
                let patch = serde_json::json!({"submission_error": b.to_string()});
                if !self.app.transition_operation(op_id, from, OpStatus::Signed, Some(&hash), None, Some(&patch))?
                    && let Some(now) = moved_on(&self.app)
                {
                    return Ok(now);
                }
                Err(CoreError::Network(format!(
                    "not submitted: {b}; the signed transaction is saved — `tx rebroadcast {}` retries the same bytes",
                    &op_id[..8]
                )))
            }
            Err(BroadcastOutcome::Other(e)) => {
                let patch = serde_json::json!({"submission_error": e.to_string()});
                if !self.app.transition_operation(op_id, from, OpStatus::Signed, Some(&hash), None, Some(&patch))?
                    && let Some(now) = moved_on(&self.app)
                {
                    return Ok(now);
                }
                Err(e)
            }
        }
    }

    /// Reject a reviewed operation and release its unsigned reservation. A rejected Qi review
    /// hands its change addresses back as well: they never reached a signed payload, so keeping
    /// them burned would push later change past the gap a seed-only restore scans.
    pub fn discard(&mut self, op_id: &str) -> Result<()> {
        let pending = self.pending.remove(op_id).ok_or_else(|| CoreError::NotFound(format!("no pending review {op_id}")))?;
        let (store_is_qi, id, journal_id) = match &pending {
            Pending::Account { prepared, op, .. } => (false, Some(prepared.reservation_id()), op.id.clone()),
            Pending::Replacement { op_id, .. } => (false, None, op_id.clone()),
            Pending::Qi { prepared, op } => (true, Some(prepared.reservation_id()), op.id.clone()),
            Pending::QiPortable { prepared, op } => (true, Some(prepared.reservation_id()), op.id.clone()),
            Pending::QiSpecial { prepared, op } => (true, Some(prepared.reservation_id()), op.id.clone()),
            // A replacement holds no reservation of its own and allocated no change address: it
            // reuses the parent's, whose claim stays held either way. Rejecting one leaves the
            // parent exactly as it was, still submitted and still the candidate to beat.
            Pending::QiReplacement { op_id, .. } => (true, None, op_id.clone()),
        };
        let _operation_guard = id.map(|id| self.operation_lock(id)).transpose()?;
        // Reclaiming releases the reservation itself, so it replaces `release_unsigned`.
        let reclaimed = match pending {
            Pending::Qi { prepared, .. } => self.reclaim_change(|pool, store| pool.reclaim(store, prepared)),
            Pending::QiSpecial { prepared, .. } => self.reclaim_change(|pool, store| pool.reclaim_operation(store, prepared)),
            _ => false,
        };
        if let Some(id) = id {
            if !reclaimed {
                let store = if store_is_qi { &mut self.qi_store } else { &mut self.quai_store };
                store.release_unsigned(id)?;
            }
            self.app.update_operation(&journal_id, OpStatus::Cancelled, None, None, None)?;
        }
        Ok(())
    }

    /// Return a thrown-away Qi transaction's change to the released set. False when the wallet
    /// cannot do it (no Qi account, or the store refused), so the caller releases the
    /// reservation the plain way instead.
    fn reclaim_change(
        &mut self,
        reclaim: impl FnOnce(
            &mut quai_sdk::qi::QiChangePool,
            &mut quai_sdk::wallet::storage::SqliteStore,
        ) -> std::result::Result<(), quai_sdk::qi::QiError>,
    ) -> bool {
        let Ok(mut pool) = self.empty_change_pool() else { return false };
        if let Err(e) = reclaim(&mut pool, &mut self.qi_store) {
            crate::ops::trace(format!("change not reclaimed: {e}"));
            return false;
        }
        if let Err(e) = pool.release(&mut self.qi_store) {
            crate::ops::trace(format!("reclaimed change not released: {e}"));
        }
        true
    }

    /// Release the nonce a prepared operation reserved after its review is gone — the wallet was
    /// closed or killed while it stood open. The reservation outlives the process that made it,
    /// and every later transaction from that account queues behind the nonce it holds, so this is
    /// the way out. A signed or submitted operation is never touched: those own their nonce.
    pub fn abandon(&mut self, selector: &str) -> Result<crate::appdb::Operation> {
        let op = self.app.find_operation(&self.network.id, selector)?;
        if op.status != OpStatus::Prepared {
            return Err(CoreError::Invalid(format!(
                "operation {} is {}, not a review waiting to be signed",
                &op.id[..8.min(op.id.len())],
                op.status.as_str()
            )));
        }
        // A review still open in this process is rejected the ordinary way.
        if self.pending.contains_key(&op.id) {
            self.discard(&op.id)?;
            return Ok(op);
        }
        let id = crate::session::parse_op_id(&op.id)?;
        let _operation_guard = self.operation_lock(id)?;
        if op.store == "qi" {
            self.qi_store.release_unsigned(id)?;
        } else {
            self.quai_store.release_unsigned(id)?;
        }
        self.app.update_operation(&op.id, OpStatus::Cancelled, None, None, None)?;
        Ok(op)
    }

    /// Rebroadcast the exact persisted bytes of a signed operation.
    pub async fn rebroadcast(&mut self, op_id: &str) -> Result<Submitted> {
        let op = self.app.find_operation(&self.network.id, op_id)?;
        if op.status.is_terminal() {
            return Err(CoreError::Invalid(format!("operation is already {}; there is nothing to rebroadcast", op.status.as_str())));
        }
        let id = parse_op_id(&op.id)?;
        let hash = op.tx_hash.clone().ok_or_else(|| CoreError::Invalid("operation was never signed".into()))?;
        if op.store == "quai" {
            let from = self.account(Some(&op.account))?;
            let signer = quai_sdk::signer::WatchOnlySigner::new(
                from.address.parse().map_err(|_| CoreError::Storage("bad address".into()))?,
                U256::from(self.network.chain_id),
            )?;
            let mut session = AccountSession::new(&self.rpc.provider, &signer, &mut self.quai_store).map_err(map_account_error)?;
            let target: quai_sdk::primitives::Hash32 = hash.parse().map_err(|_| CoreError::Storage("bad hash".into()))?;
            let result = session.broadcast_candidate(id, target).await;
            self.finish_broadcast(
                &op.id,
                op.status,
                hash,
                result.map_err(|e| match e {
                    AccountError::Broadcast(b) => BroadcastOutcome::Broadcast(b),
                    other => BroadcastOutcome::Other(CoreError::from(other)),
                }),
            )
        } else {
            let keys = self
                .unlocked
                .as_ref()
                .ok_or_else(|| CoreError::Locked("wallet is locked".into()))?
                .qi_keyring_with_channels(&self.qi_store)?;
            let mut session = QiSession::with_keys(&self.rpc.provider, &keys, &mut self.qi_store);
            let target = hash.parse().map_err(|_| CoreError::Storage("bad hash".into()))?;
            let result = session.broadcast_candidate(id, target).await;
            drop(session);
            drop(keys);
            self.finish_broadcast(
                &op.id,
                op.status,
                hash,
                result.map_err(|e| match e {
                    QiError::Broadcast(b) => BroadcastOutcome::Broadcast(b),
                    other => BroadcastOutcome::Other(CoreError::from(other)),
                }),
            )
        }
    }

    /// Prepare a fee-only speed-up of a pending operation, on either ledger.
    pub async fn prepare_speed_up(&mut self, op_selector: &str, bump_percent: u16) -> Result<Review> {
        self.require_execution_source()?;
        let op = self.app.find_operation(&self.network.id, op_selector)?;
        if let Some(plan_id) = op.detail.get("plan_id").and_then(|v| v.as_str())
            && self.app.trade_plan(plan_id)?.is_some_and(|plan| plan.intent["client"] == "order")
        {
            return Err(CoreError::Rejected("an automated order's saved fee budget does not authorize replacement fees; rebroadcast its existing signed candidate or create a separately authorized order".into()));
        }
        if op.status.is_terminal() {
            return Err(CoreError::Invalid(format!("operation is already {}", op.status.as_str())));
        }
        if op.store == "qi" {
            return self.prepare_qi_speed_up(op, bump_percent).await;
        }
        let _commitment_guard = self.commitment_lock(&op.account)?;
        let _operation_guard = self.operation_lock(parse_op_id(&op.id)?)?;
        let op = self.app.find_operation(&self.network.id, &op.id)?;
        if op.status.is_terminal() {
            return Err(CoreError::Invalid("operation completed while preparing its replacement".into()));
        }
        let from = self.account(Some(&op.account))?;
        let signer = self.signer_for(&from)?;
        let id = parse_op_id(&op.id)?;
        let fees = self.network.preparation_limits(1_000_000, None)?;
        let mut session = AccountSession::new(&self.node.provider, &signer, &mut self.quai_store)
            .map_err(map_account_error)?
            .with_observation_policy(self.network.observation_policy());
        let candidates = session.signed_candidates(id).map_err(map_account_error)?;
        let parent = candidates.last().ok_or_else(|| CoreError::Invalid("operation has no signed candidate".into()))?.hash()?;
        let prepared =
            session.prepare_replacement(id, parent, ReplacementPolicy::new(bump_percent, fees)).await.map_err(map_account_error)?;
        let tx = prepared.transaction();
        let mut commitments = crate::commitments::Commitments::from_operation(&op)?;
        let prior_fee =
            U256::from_str_radix(&commitments.fee, 10).map_err(|_| CoreError::Storage("invalid family fee commitment".into()))?;
        commitments.fee = prior_fee.max(tx.gas_price.saturating_mul(U256::from(tx.gas_limit))).to_string();
        self.check_commitments(&from.address, &op.id, &commitments).await?;
        if !self.app.transition_operation(
            &op.id,
            op.status,
            op.status,
            None,
            None,
            Some(&serde_json::json!({"commitments":commitments})),
        )? {
            return Err(CoreError::Invalid("operation changed while reserving replacement fee".into()));
        }
        let review = Review {
            op_id: format!("{}-r{}", op.id, candidates.len()),
            kind: "speed_up".into(),
            title: "Speed up transaction".into(),
            network: self.network_label(),
            from: format!("{} ({})", from.address, from.label),
            to: tx.to.map(|a| a.to_string()).unwrap_or_default(),
            asset: "QUAI".into(),
            amount: format!("{} QUAI", amount::quai(tx.value)),
            amount_base: tx.value.to_string(),
            max_fee: format!("{} QUAI", amount::quai(tx.gas_price.saturating_mul(U256::from(tx.gas_limit)))),
            fee_bps: None,
            fields: vec![
                field("Replaces", parent.to_string()),
                field("Nonce", tx.nonce.to_string()),
                field("New gas price", format!("{} wei", tx.gas_price)),
                field("Gas limit", tx.gas_limit.to_string()),
            ],
            coins: vec![],
            warnings: {
                let mut w = vec!["the original may still be mined instead; only one can succeed".into()];
                if let Some(note) = self.network.fee_policy_note(tx.gas_price, tx.gas_price.saturating_mul(U256::from(tx.gas_limit))) {
                    w.insert(0, note);
                }
                w
            },
            visuals: vec![],
            fee_over_policy: self.network.fee_policy_note(tx.gas_price, tx.gas_price.saturating_mul(U256::from(tx.gas_limit))).is_some(),
            changes: balance_changes(
                "speed_up",
                "QUAI",
                U256::ZERO,
                amount::QUAI_DECIMALS,
                tx.value,
                (tx.gas_price.saturating_mul(U256::from(tx.gas_limit)), "QUAI", amount::QUAI_DECIMALS),
                &serde_json::Value::Null,
            ),
        };
        self.pending.insert(review.op_id.clone(), Pending::Replacement { prepared, from, op_id: op.id });
        Ok(review)
    }

    /// Prepare a fee-only speed-up of a pending Qi operation, as a conflicting candidate over the
    /// same inputs.
    ///
    /// A Qi replacement cannot simply offer more. The inputs are fixed and so is everything the
    /// transaction pays out, so the only place a larger fee can come from is the wallet's own
    /// change — and the SDK requires the new change to be strictly lower than the old. The fee
    /// therefore always rises: by at least one qit, and by at most the whole of the change. A
    /// transaction that returned no change cannot be sped up at all.
    ///
    /// For a conversion or a wrap the candidate also re-decomposes the Quai-ledger destination
    /// largest-first. The node credits that destination with one aggregated value, so reshaping it
    /// costs nothing, but it charges `ETXGas` for every one of those outputs when it decides
    /// whether to include the transaction. Collapsing twelve outputs into two more than halves the
    /// gas the fee has to cover — and for a conversion that is stuck *because* it sits under that
    /// floor, the shape rather than the fee is what rescues it. See
    /// `docs/SDK_UPDATE_REVIEW_2026-09-20.md`.
    async fn prepare_qi_speed_up(&mut self, op: Operation, bump_percent: u16) -> Result<Review> {
        let id = parse_op_id(&op.id)?;
        // The SDK refuses to build on a snapshot older than a handful of blocks, and a replacement
        // is usually reached from a cold command rather than a running terminal, so the store is
        // brought up to the tip before anything is read from it.
        self.refresh_qi_for_spend().await?;
        let owned: std::collections::HashSet<Address> =
            self.qi_store.addresses()?.iter().map(quai_sdk::wallet::storage::PublicAddress::address).collect();
        let snapshot = self.qi_store.snapshot()?;

        // The newest candidate is the one to beat: every replacement returns less change than the
        // candidate it replaces, so building on an older one would not be strictly lower than the
        // newest and the SDK would refuse it.
        let keys =
            self.unlocked.as_ref().ok_or_else(|| CoreError::Locked("wallet is locked".into()))?.qi_keyring_with_channels(&self.qi_store)?;
        let mut session = QiSession::with_keys(&self.node.provider, &keys, &mut self.qi_store);
        let candidates = session.signed_candidates(id);
        drop(session);
        drop(keys);
        let candidates = candidates?;
        let parent = candidates.last().ok_or_else(|| CoreError::Invalid("this operation has no signed transaction to replace".into()))?;
        let parent_hash = parent.hash()?;
        let parent_tx = parent.transaction().clone();

        // What the parent paid, read from the coins it spends rather than the journal, which only
        // records what was quoted. A coin the snapshot no longer carries means the wallet cannot
        // price it; the journal is then the best remaining answer.
        let mut inputs_total = U256::ZERO;
        let mut priced = true;
        for input in &parent_tx.inputs {
            match snapshot.coins.iter().find(|c| c.outpoint == input.previous_output) {
                Some(coin) => inputs_total += U256::from(coin.denomination.value()),
                None => priced = false,
            }
        }
        let outputs_total = qits(&parent_tx.outputs);
        let parent_fee = match priced {
            true => inputs_total.checked_sub(outputs_total).unwrap_or(U256::ZERO),
            false => op.fee.parse::<U256>().unwrap_or(U256::ZERO),
        };

        // Change is what this wallet pays back to itself: a Qi-ledger output at an address the
        // store knows. A conversion's destination is on the Quai ledger, and an ordinary send's
        // recipient belongs to someone else, so neither is ours to reduce.
        let change: Vec<(u16, QiOutput)> = parent_tx
            .outputs
            .iter()
            .enumerate()
            .filter(|(_, o)| o.address.ledger() != Ledger::Quai && owned.contains(&o.address))
            .map(|(i, o)| (i as u16, o.clone()))
            .collect();
        if change.is_empty() {
            return Err(CoreError::Insufficient(
                "this transaction kept no change, and a Qi replacement can only raise its fee out of its own change; \
                 nothing here can fund a higher one"
                    .into(),
            ));
        }
        let change_total = qits(&change.iter().map(|(_, o)| o.clone()).collect::<Vec<_>>());

        let most = parent_fee + change_total;
        let mut increase = fee_increase(parent_fee, change_total, bump_percent);

        let special = !parent_tx.data.is_empty();
        let profile = (special && self.network.specialized_fee_estimation).then_some(QiFeeProfile::V056ShaAnchored);
        let policy = QiPolicy::new(most, 1024, 256, 10).with_max_fee_rounds(12);
        let indexes: Vec<u16> = change.iter().map(|(i, _)| *i).collect();

        let mut stale = 0;
        let prepared = loop {
            let mut intent = QiReplacementIntent::new(
                parent_hash,
                indexes.clone(),
                kept_change(&change, change_total.checked_sub(increase).unwrap_or(U256::ZERO)),
            );
            if special {
                intent = intent.aggregating_destination();
            }
            let keys = self
                .unlocked
                .as_ref()
                .ok_or_else(|| CoreError::Locked("wallet is locked".into()))?
                .qi_keyring_with_channels(&self.qi_store)?;
            let mut session = QiSession::with_keys(&self.node.provider, &keys, &mut self.qi_store);
            let result = session.prepare_replacement(id, intent, policy, profile).await;
            drop(session);
            drop(keys);
            match result {
                Ok(p) => break Ok(p),
                // The tip moved, or another handle committed first. Pause, take the new state and
                // build against that; retrying on the same snapshot would only fail the same way.
                Err(e) if qi_stale(&e) && stale < 4 => {
                    stale += 1;
                    stale_pause().await;
                    self.refresh_qi_for_spend().await?;
                }
                // The node wants more than this bump offered. There is exactly one more offer to
                // make: every qit of change, which is the most a replacement can ever pay.
                Err(QiError::Selection(quai_sdk::wallet::SelectionError::FeeBudgetExceeded)) if increase < change_total => {
                    increase = change_total;
                }
                Err(QiError::Selection(quai_sdk::wallet::SelectionError::FeeBudgetExceeded)) => {
                    break Err(CoreError::Insufficient(format!(
                        "the node prices this transaction above {} Qi, and its {} Qi of change cannot pay that much; \
                         the funds are safe, but this one cannot be made mineable by fee alone",
                        amount::qi(most),
                        amount::qi(change_total)
                    )));
                }
                Err(e) => break Err(CoreError::from(e)),
            }
        };
        let prepared = prepared?;

        let tx = prepared.transaction().clone();
        let new_fee = prepared.fee();
        let new_change = qits(
            &tx.outputs.iter().filter(|o| o.address.ledger() != Ledger::Quai && owned.contains(&o.address)).cloned().collect::<Vec<_>>(),
        );
        let mut coins = Vec::new();
        for input in &tx.inputs {
            let value = snapshot.coins.iter().find(|c| c.outpoint == input.previous_output).map_or(0, |c| c.denomination.value());
            coins.push(CoinLine {
                address: format!(
                    "{}:{} ({})",
                    input.previous_output.transaction_hash,
                    input.previous_output.index,
                    input.public_key.address()
                ),
                qits: value,
                role: "input".into(),
            });
        }
        for output in &tx.outputs {
            coins.push(CoinLine {
                address: output.address.to_string(),
                qits: output.denomination.value(),
                role: if output.address.ledger() == Ledger::Quai { "recipient" } else { "change" }.into(),
            });
        }
        let mut fields = vec![
            field("Replaces", parent_hash.to_string()),
            field("Fee", format!("{} Qi → {} Qi", amount::qi(parent_fee), amount::qi(new_fee))),
            field("Change kept", format!("{} Qi → {} Qi", amount::qi(change_total), amount::qi(new_change))),
        ];
        if special {
            let was = parent_tx.outputs.iter().filter(|o| o.address.ledger() == Ledger::Quai).count();
            let now = tx.outputs.iter().filter(|o| o.address.ledger() == Ledger::Quai).count();
            fields.push(field("Destination outputs", format!("{was} → {now}")));
        }
        fields.push(field("Inputs", tx.inputs.len().to_string()));
        fields.push(field("Outputs", format!("{} (was {})", tx.outputs.len(), parent_tx.outputs.len())));
        let mut warnings = vec!["the original may still be mined instead; only one of the two can succeed".into()];
        if special {
            warnings
                .push("the destination and the amount are unchanged; only the fee and the shape of the destination outputs differ".into());
        }
        let amount_q: U256 = op.amount.parse().unwrap_or(U256::ZERO);
        let review = Review {
            op_id: format!("{}-r{}", op.id, candidates.len()),
            kind: "speed_up".into(),
            title: "Speed up transaction".into(),
            network: self.network_label(),
            from: "Qi wallet".into(),
            to: op.counterparty.clone(),
            asset: "QI".into(),
            amount: format!("{} Qi", amount::qi(amount_q)),
            amount_base: op.amount.clone(),
            max_fee: format!("{} Qi", amount::qi(new_fee)),
            fee_bps: amount::bps(new_fee, amount_q),
            fields,
            coins,
            warnings,
            visuals: vec![],
            fee_over_policy: false,
            // Only the added fee is new money: the amount left the wallet when the parent was
            // signed, and whichever candidate wins, it leaves once.
            changes: balance_changes(
                "speed_up",
                "QI",
                U256::ZERO,
                amount::QI_DECIMALS,
                U256::ZERO,
                (new_fee.checked_sub(parent_fee).unwrap_or(U256::ZERO), "Qi", amount::QI_DECIMALS),
                &op.detail,
            ),
        };
        self.pending.insert(review.op_id.clone(), Pending::QiReplacement { prepared, op_id: op.id });
        Ok(review)
    }

    /// Network label shown on reviews: name, chain ID and id.
    pub fn network_label(&self) -> String {
        format!("{} · chain {} ({})", self.network.name, self.network.chain_id, self.network.id)
    }
}

/// How many more qits a Qi replacement should offer than the candidate it replaces.
///
/// A bump means on this ledger what it means on the account ledger — raise the fee by this
/// percentage — with two limits that are peculiar to Qi. The smallest step is one qit, because the
/// new change has to be *strictly* lower than the old, so a replacement always costs something
/// even at a bump of zero. The largest is the change itself, because that is the only place the
/// extra fee can come from.
///
/// The node's own quote is the real floor, and the SDK refuses anything under it; this only
/// chooses what to offer above it.
fn fee_increase(parent_fee: U256, change_total: U256, bump_percent: u16) -> U256 {
    let bumped = parent_fee.saturating_mul(U256::from(100u64 + u64::from(bump_percent))) / U256::from(100u64);
    bumped.max(parent_fee + U256::from(1u64)).min(parent_fee + change_total) - parent_fee
}

/// Total value of a set of Qi outputs.
fn qits(outputs: &[QiOutput]) -> U256 {
    outputs.iter().fold(U256::ZERO, |sum, o| sum + U256::from(o.denomination.value()))
}

/// Which of the parent's change outputs a replacement keeps, given how many qits it may still
/// return. Largest first, so the wallet keeps its value in as few coins as it can.
///
/// The kept outputs are the parent's own, untouched — the same addresses and the same
/// denominations. That matters twice. The node's `CheckDenominations` already accepted this exact
/// set against these exact inputs, so any subset of it is still acceptable, and no fresh change
/// address is derived, so replacing a transaction costs nothing against the gap limit that a
/// seed-only restore scans.
///
/// Returning nothing is a valid answer: it pays the whole of the change as fee, which is the most
/// a replacement can ever offer.
fn kept_change(change: &[(u16, QiOutput)], budget: U256) -> Vec<QiOutput> {
    let mut ordered: Vec<&QiOutput> = change.iter().map(|(_, o)| o).collect();
    ordered.sort_by_key(|o| std::cmp::Reverse(o.denomination.value()));
    let mut left = budget;
    let mut kept = Vec::new();
    for output in ordered {
        let value = U256::from(output.denomination.value());
        if value <= left {
            left -= value;
            kept.push(output.clone());
        }
    }
    kept
}

/// Record a signature before broadcasting, only over the review it came from. If that review was
/// cancelled meanwhile (another process abandoned it), nothing is broadcast.
fn mark_signed(app: &crate::appdb::AppDb, op_id: &str, hash: &str, fee: Option<&str>) -> Result<()> {
    if app.transition_operation(op_id, OpStatus::Prepared, OpStatus::Signed, Some(hash), fee, None)? {
        return Ok(());
    }
    let now = app.operation(op_id)?.map_or("gone", |o| o.status.as_str());
    Err(CoreError::Rejected(format!("this review is {now} now; nothing was broadcast")))
}

pub(crate) enum BroadcastOutcome {
    Broadcast(BroadcastError),
    Other(CoreError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_identity_applies_to_approvals_lp_and_curve_effects() {
        let network = crate::network::NetworkProfile::builtins().remove(0);
        let fake = "0x00000000000000000000000000000000000000ab";
        let approval = canonical_identity_warnings(&network, "USDT", &json!({"token":fake}));
        assert_eq!(approval.len(), 1);
        for (symbol, key) in [("USDT", "lp"), ("UЅDT", "curve")] {
            let effects = json!({"kind":key,"financial_effects":[{"asset":symbol,"token":fake},{"asset":symbol,"token":fake}]});
            assert_eq!(canonical_identity_warnings(&network, "LP", &effects).len(), 1);
        }
        let canonical = network.ecosystem.usdt.unwrap().address;
        let network = crate::network::NetworkProfile::builtins().remove(0);
        assert!(canonical_identity_warnings(&network, "USDT", &json!({"token":canonical})).is_empty());
    }

    #[test]
    fn frozen_review_expiry_is_checked_without_mutating_the_payload() {
        let detail = json!({"expires_at":100});
        assert!(validate_review_expiry(&detail, 99).is_ok());
        assert!(validate_review_expiry(&detail, 100).is_err());
        assert!(validate_review_expiry(&detail, 101).is_err());
        assert!(validate_review_expiry(&json!({"expires_at":"100"}), 99).is_err());
        assert_eq!(detail, json!({"expires_at":100}));
    }

    #[test]
    fn visuals_follow_the_operation() {
        let swap = review_visuals("swap", "WQI", &json!({"from_token": "0x002b", "to_symbol": "USDT", "to_token": "0x0049"}));
        assert_eq!(swap.len(), 2);
        assert_eq!((swap[0].role.as_str(), swap[0].contract.as_str()), ("pay", "0x002b"));
        assert_eq!((swap[1].role.as_str(), swap[1].symbol.as_str()), ("receive", "USDT"));
        // Older journal rows without from_token picture native QUAI.
        assert_eq!(review_visuals("swap", "QUAI", &json!({"to_symbol": "USDT", "to_token": "0x0049"}))[0].contract, "quai");
        let nft = review_visuals("nft_buy", "QUAI", &json!({"contract": "0x004d", "token_id": "7", "name": ""}));
        assert_eq!((nft[0].role.as_str(), nft[0].symbol.as_str(), nft[0].token_id.as_deref()), ("nft", "#7", Some("7")));
        assert_eq!(review_visuals("approve", "USDT", &json!({"token": "0x0049"}))[0].role, "token");
        assert_eq!(review_visuals("send_quai", "QUAI", &json!({}))[0].contract, "quai");
        let convert = review_visuals("convert_qi_to_quai", "QI", &json!({}));
        assert_eq!((convert[0].contract.as_str(), convert[1].contract.as_str()), ("qi", "quai"));
        assert!(review_visuals("fill_gap", "QUAI", &json!({})).is_empty());
    }
}

#[cfg(test)]
mod balance_change_tests {
    use super::*;
    use serde_json::json;

    /// A conversion's headline is what arrives after the protocol discount, not the spot rate.
    /// Measured on mainnet for 10,000 QUAI: spot 75,964 qits, expected 7,582, so a headline from
    /// the spot quote promised ten times what arrived.
    #[test]
    fn a_conversion_headline_is_the_discounted_amount() {
        let fee = (U256::ZERO, "QUAI", 18);
        let detail = json!({"quoted_qits": "75964", "expected_qits": "7582", "slippage_bps": 9000});
        let c = balance_changes("convert_quai_to_qi", "QUAI", e18(10_000), 18, U256::ZERO, fee, &detail);
        let arrives = c.iter().find(|r| r.direction == "in").unwrap();
        assert_eq!((arrives.asset.as_str(), arrives.amount.as_str()), ("Qi", "≈ 7.582"));
        assert!(arrives.note.contains("refunded if under 7.596"), "{}", arrives.note);
        // Without the node's estimate the spot figure is shown as a ceiling, and said to be one.
        let spot_only = balance_changes("convert_quai_to_qi", "QUAI", e18(100), 18, U256::ZERO, fee, &json!({"quoted_qits": "759"}));
        let arrives = spot_only.iter().find(|r| r.direction == "in").unwrap();
        assert_eq!(arrives.amount, "≤ 0.759");
        assert!(arrives.note.contains("spot rate"));
        // Qi → QUAI says what arrives too; it used to show nothing.
        let back = json!({"quoted_its": e18(12).to_string(), "expected_its": e18(11).to_string(), "slippage_bps": 300});
        let c = balance_changes("convert_qi_to_quai", "QI", U256::from(1_000_000u64), 3, U256::ZERO, (U256::from(36u64), "QI", 3), &back);
        let arrives = c.iter().find(|r| r.direction == "in").unwrap();
        assert_eq!((arrives.asset.as_str(), arrives.amount.as_str()), ("QUAI", "≈ 11"));
    }

    fn e18(n: u64) -> U256 {
        U256::from(n) * U256::from(10u64).pow(U256::from(18u8))
    }

    #[test]
    fn a_swap_shows_what_leaves_what_arrives_and_the_fee() {
        let detail = json!({"to_symbol": "USDT", "to_decimals": 6, "expected_out": "51940000", "minimum_out": "51680000"});
        let c = balance_changes("swap", "WQI", e18(50), 18, U256::ZERO, (U256::from(2_100_000_000_000_000u64), "QUAI", 18), &detail);
        fn row(c: &BalanceChange) -> (&str, &str, &str) {
            (c.direction.as_str(), c.asset.as_str(), c.amount.as_str())
        }
        assert_eq!(row(&c[0]), ("out", "WQI", "50"));
        assert_eq!(row(&c[1]), ("in", "USDT", "≈ 51.94"));
        assert_eq!(c[1].note, "at least 51.68 USDT");
        assert_eq!(row(&c[2]), ("fee", "QUAI", "0.0021"));
    }

    #[test]
    fn native_value_is_the_quai_that_leaves() {
        // Swapping from QUAI: the amount is the value, and it is counted once.
        let c = balance_changes("swap", "QUAI", e18(3), 18, e18(3), (U256::ZERO, "QUAI", 18), &json!({}));
        assert_eq!(c.iter().filter(|c| c.direction == "out").count(), 1);
        assert_eq!(c[0].amount, "3");
    }

    #[test]
    fn an_approval_moves_nothing_but_the_fee() {
        let c = balance_changes("approve", "WQI", e18(50), 18, U256::ZERO, (U256::from(1u8), "QUAI", 18), &json!({}));
        assert_eq!(c[0].direction, "none");
        assert!(c[0].note.contains("allowance only"));
        assert_eq!(c[1].direction, "fee");
    }

    #[test]
    fn an_nft_transfer_is_one_item_out() {
        let c = balance_changes(
            "nft_transfer",
            "Quai Pepe",
            U256::from(1u8),
            0,
            U256::ZERO,
            (U256::ZERO, "QUAI", 18),
            &json!({"name": "Quai Pepe #212", "token_id": "212"}),
        );
        assert_eq!((c[0].direction.as_str(), c[0].asset.as_str(), c[0].amount.as_str()), ("out", "Quai Pepe #212", "1"));
    }

    #[test]
    fn qi_sends_count_in_qi() {
        let c = balance_changes("send_qi", "QI", U256::from(2_500u64), 3, U256::ZERO, (U256::from(5u8), "Qi", 3), &json!({}));
        assert_eq!((c[0].asset.as_str(), c[0].amount.as_str()), ("Qi", "2.5"));
        assert_eq!((c[1].asset.as_str(), c[1].amount.as_str()), ("Qi", "0.005"));
    }
}

#[cfg(test)]
mod broadcast_race_tests {
    use super::*;
    use crate::appdb::Operation;

    fn session() -> (tempfile::TempDir, Session) {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::paths::Paths::resolve(Some(dir.path().to_path_buf())).unwrap();
        let registry = crate::registry::Registry::fast(paths);
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let meta = registry.create_hd("main", phrase, "english", "", "password123", true).unwrap();
        let network = crate::network::NetworkProfile::builtins().into_iter().next().unwrap();
        let s = Session::open(registry, crate::config::AppConfig::default(), meta, network).unwrap();
        (dir, s)
    }

    fn op(s: &Session, id: &str, status: OpStatus) -> Operation {
        let op = Operation {
            id: id.into(),
            network: s.network.id.clone(),
            kind: "send_quai".into(),
            store: "quai".into(),
            account: "0x00aa".into(),
            status,
            tx_hash: Some("0x01".into()),
            asset: "QUAI".into(),
            amount: "1".into(),
            counterparty: "0x00bb".into(),
            fee: String::new(),
            detail: serde_json::json!({}),
            created: 1,
            updated: 1,
        };
        s.app.insert_operation(&op).unwrap();
        op
    }

    /// A slow broadcast returning after the pending lane has already seen the transaction mined
    /// reports "confirmed" and leaves it confirmed: before, it was written back to "not
    /// submitted" with advice to rebroadcast, and then confirmed and announced a second time.
    #[test]
    fn a_broadcast_that_returns_late_does_not_undo_a_confirmation() {
        let (_dir, mut s) = session();
        let id = "aa000000000000000000000000000001";
        op(&s, id, OpStatus::Signed);
        assert!(s.app.transition_operation(id, OpStatus::Signed, OpStatus::Confirmed, None, None, None).unwrap(), "the tracker");
        let late: std::result::Result<(), BroadcastOutcome> = Err(BroadcastOutcome::Other(CoreError::Network("timed out".into())));
        let r = s.finish_broadcast(id, OpStatus::Signed, "0x01".into(), late).unwrap();
        assert_eq!(r.status, OpStatus::Confirmed, "{}", r.message);
        assert_eq!(s.app.operation(id).unwrap().unwrap().status, OpStatus::Confirmed);
        // The same when the broadcast itself succeeded, just later than the block.
        let ok: std::result::Result<(), BroadcastOutcome> = Ok(());
        assert_eq!(s.finish_broadcast(id, OpStatus::Signed, "0x01".into(), ok).unwrap().status, OpStatus::Confirmed);
        // The ordinary case still records the submission.
        let fresh = "aa000000000000000000000000000002";
        op(&s, fresh, OpStatus::Signed);
        let ok: std::result::Result<(), BroadcastOutcome> = Ok(());
        assert_eq!(s.finish_broadcast(fresh, OpStatus::Signed, "0x02".into(), ok).unwrap().status, OpStatus::Submitted);
        assert_eq!(s.app.operation(fresh).unwrap().unwrap().status, OpStatus::Submitted);
    }

    #[test]
    fn production_candidate_recording_preserves_each_canonical_winner_and_receipt_fee() {
        for status in [OpStatus::Confirmed, OpStatus::Failed, OpStatus::Settling, OpStatus::Locked, OpStatus::Settled, OpStatus::Refunded] {
            for replacement_won in [false, true] {
                let (_dir, mut s) = session();
                let id = "aa000000000000000000000000000099";
                op(&s, id, OpStatus::Submitted);
                let winner = if replacement_won { "0x02" } else { "0x01" };
                s.app
                    .transition_operation(
                        id,
                        OpStatus::Submitted,
                        status,
                        Some(winner),
                        Some("receipt-fee"),
                        Some(&serde_json::json!({"original_tx":"0x01", "canonical_tx":winner, "actual_out":"1000"})),
                    )
                    .unwrap();
                s.record_candidate(id, "0x02".into(), Some("replacement-fee")).unwrap();
                s.record_candidate(id, "0x02".into(), Some("replacement-fee")).unwrap();
                let current = s.app.operation(id).unwrap().unwrap();
                assert_eq!(current.status, status);
                assert_eq!(current.tx_hash.as_deref(), Some(winner));
                assert_eq!(current.fee, "receipt-fee");
                assert_eq!(current.detail["actual_out"], "1000");
                assert_eq!(current.detail["original_tx"], "0x01");
                assert_eq!(current.detail["candidates"], serde_json::json!(["0x02"]));
                let late: std::result::Result<(), BroadcastOutcome> =
                    Err(BroadcastOutcome::Other(CoreError::Network("lost response".into())));
                let returned = s.finish_broadcast(id, OpStatus::Submitted, "0x02".into(), late).unwrap();
                assert_eq!(returned.status, status);
                assert_eq!(returned.tx_hash, winner);
                assert_eq!(returned.explorer, s.network.tx_url(winner));
                assert_eq!(s.app.operation(id).unwrap().unwrap().tx_hash.as_deref(), Some(winner));
            }
        }
    }

    /// A review cancelled elsewhere between the prompt and the signature is never broadcast.
    #[test]
    fn a_review_cancelled_elsewhere_is_not_signed() {
        let (_dir, s) = session();
        let id = "aa000000000000000000000000000003";
        op(&s, id, OpStatus::Prepared);
        s.app.update_operation(id, OpStatus::Cancelled, None, None, None).unwrap();
        assert!(matches!(mark_signed(&s.app, id, "0x03", None), Err(CoreError::Rejected(_))));
        assert_eq!(s.app.operation(id).unwrap().unwrap().status, OpStatus::Cancelled);
        let ok = "aa000000000000000000000000000004";
        op(&s, ok, OpStatus::Prepared);
        mark_signed(&s.app, ok, "0x04", None).unwrap();
        assert_eq!(s.app.operation(ok).unwrap().unwrap().status, OpStatus::Signed);
    }
}

/// How a Qi replacement decides what to pay and what to keep.
///
/// The worked example throughout is the wrap that made these releases necessary: 15 Qi to a
/// Quai-ledger beneficiary across twelve destination outputs, 78 qits of fee, and four change
/// outputs of 10, 10, 1 and 1 qits — 22 qits, which is every qit a replacement has to work with.
#[cfg(test)]
mod replacement_tests {
    use super::*;
    use quai_sdk::consensus::Denomination;

    fn out(qits: u64) -> QiOutput {
        // Qi denominations are a fixed ladder; these are the four the stuck wrap left as change.
        let index = match qits {
            1 => 0,
            5 => 1,
            10 => 2,
            500 => 5,
            1000 => 6,
            other => panic!("not a denomination: {other}"),
        };
        QiOutput { address: Address::from_bytes([0u8; 20]), denomination: Denomination::new(index).expect("a denomination") }
    }

    fn change(values: &[u64]) -> Vec<(u16, QiOutput)> {
        values.iter().enumerate().map(|(i, v)| (i as u16, out(*v))).collect()
    }

    /// The fee always rises, because the new change must be strictly lower than the old. Even a
    /// bump of nothing costs one qit; nothing can cost more than the whole of the change.
    #[test]
    fn a_replacement_always_costs_at_least_one_qit_and_never_more_than_the_change() {
        let (fee, change_total) = (U256::from(78u64), U256::from(22u64));
        // 78 + 20% is 93.6, truncated to 93: fifteen qits more than the parent paid.
        assert_eq!(fee_increase(fee, change_total, 20), U256::from(15u64));
        // A bump too small to move a whole qit still moves one, because standing still is refused.
        assert_eq!(fee_increase(fee, change_total, 0), U256::from(1u64));
        assert_eq!(fee_increase(fee, change_total, 1), U256::from(1u64), "78.78 truncates back to 78");
        // And it stops at the change, however large the bump: 78 + 500% is far beyond 22.
        assert_eq!(fee_increase(fee, change_total, 500), change_total);
        // A parent that paid nothing still has to pay a qit more than nothing.
        assert_eq!(fee_increase(U256::ZERO, change_total, 20), U256::from(1u64));
    }

    /// Change is kept largest first, so the wallet holds its value in as few coins as it can.
    #[test]
    fn change_is_kept_largest_first_within_the_budget() {
        let change = change(&[10, 10, 1, 1]);
        // Paying 16 more leaves 6 qits of room: the tens do not fit, both ones do.
        let kept = kept_change(&change, U256::from(6u64));
        assert_eq!(qits(&kept), U256::from(2u64));
        assert_eq!(kept.len(), 2, "two coins of one qit: {kept:?}");
        // Room for everything keeps everything — the caller, not this, refuses to stand still.
        assert_eq!(qits(&kept_change(&change, U256::from(22u64))), U256::from(22u64));
        // Paying the whole change keeps nothing, which is a valid replacement.
        assert!(kept_change(&change, U256::ZERO).is_empty());
        // A budget between denominations takes the largest that fits, then fills down.
        assert_eq!(qits(&kept_change(&change, U256::from(11u64))), U256::from(11u64), "10 + 1");
        assert_eq!(qits(&kept_change(&change, U256::from(9u64))), U256::from(2u64), "no ten fits; both ones do");
    }

    /// The two together: what the stuck wrap would actually offer, at the TUI's default bump.
    ///
    /// The point is the *margin*. The parent paid 78 qits against an inclusion floor of 97 for its
    /// twelve-output shape and sat in the pool for a day. The replacement aggregates that
    /// destination down to two outputs, whose floor is about 34, and pays 98 — nearly three times
    /// over. Its real mistake was having no margin at all, not having too small a fee.
    #[test]
    fn the_stuck_wrap_offers_nearly_three_times_the_floor_it_missed() {
        let (parent_fee, change) = (U256::from(78u64), change(&[10, 10, 1, 1]));
        let total = qits(&change.iter().map(|(_, o)| o.clone()).collect::<Vec<_>>());
        let increase = fee_increase(parent_fee, total, 20);
        let kept = kept_change(&change, total - increase);
        let new_fee = parent_fee + (total - qits(&kept));
        assert_eq!(new_fee, U256::from(98u64), "kept 2 of 22 qits, so 20 more went to the fee");
        assert!(new_fee > U256::from(34u64) * U256::from(2u64), "comfortably over the aggregated shape's floor");
        assert!(new_fee <= parent_fee + total, "and never more than the change could pay");
    }
}
