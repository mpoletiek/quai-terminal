//! Reconciliation of journaled operations, destination settlement, incoming activity and locks.
//!
//! Tracking never releases claims or rebroadcasts. Missing receipts leave operations open.

use crate::amount;
use crate::appdb::{Activity, OpStatus, Operation};
use crate::error::{CoreError, Result};
use crate::network::ZONE;
use crate::registry::now;
use crate::session::{Session, parse_op_id};
use quai_sdk::Provider;
use quai_sdk::accounts::{AccountCandidateStatus, AccountSession};
use quai_sdk::primitives::Hash32;
use quai_sdk::provider::{BlockReference, ConversionEffect, EtxScanRequest, ReceiptOutcome};
use quai_sdk::qi::{QiCandidateStatus, QiSession};
use quai_sdk::recovery::{OperationObservation, reconcile_operation};
use quai_sdk::rpc::Transport;
use quai_sdk::settlement::{SettlementKind, SettlementUpdate, revalidate_settlement_cursor, track_settlement};
use quai_sdk::signer::WatchOnlySigner;
use quai_sdk::{BlockTag, QuaiAddress, U256};
use serde::{Deserialize, Serialize};

/// A key resolver that holds no keys, for reading a Qi family's receipts.
///
/// Observing candidates never resolves one — it reads receipts and block anchors — so tracking a
/// replaced operation must not require an unlocked wallet, any more than the rest of tracking
/// does. Should a caller ever ask for a key, refusing is the only safe answer.
struct NoQiKeys;

impl quai_sdk::wallet::qi_keys::QiKeyResolver for NoQiKeys {
    fn resolve(
        &self,
        _address: &quai_sdk::wallet::storage::PublicAddress,
    ) -> std::result::Result<quai_sdk::crypto::SecretKey, quai_sdk::wallet::storage::StorageError> {
        Err(quai_sdk::wallet::storage::StorageError::Invalid)
    }
}

/// One status change produced by tracking.
#[derive(Clone, Debug, Serialize)]
pub struct StatusChange {
    /// Operation id.
    pub op_id: String,
    /// Kind.
    pub kind: String,
    /// Previous status.
    pub from: OpStatus,
    /// New status.
    pub to: OpStatus,
    /// Human summary.
    pub message: String,
}

/// Results of one tracking pass.
#[derive(Clone, Debug, Default, Serialize)]
pub struct TrackReport {
    /// Operation status changes.
    pub changes: Vec<StatusChange>,
    /// Newly observed incoming activity.
    pub incoming: Vec<Activity>,
    /// Non-fatal errors per operation.
    pub errors: Vec<String>,
    /// Listings that sold since the last check (notification text).
    pub sales: Vec<String>,
}

/// A time-locked balance.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LockItem {
    /// Source description.
    pub source: String,
    /// Asset.
    pub asset: String,
    /// Human amount.
    pub amount: String,
    /// Unlock block height when known.
    pub unlock_height: Option<u64>,
    /// Blocks remaining.
    pub blocks_remaining: Option<u64>,
    /// Estimated seconds remaining (5s blocks) when known.
    pub eta_secs: Option<u64>,
    /// Already spendable.
    pub unlocked: bool,
}

/// Approximate Cyprus-1 block time used only for ETA display.
pub const BLOCK_SECS: u64 = 5;

/// Accounts one token-transfer sync reads. Each is an explorer request against a shared per-IP
/// budget, so the pass is bounded — but it rotates, so every account is reached in turn.
pub const TRANSFER_SYNC_ACCOUNTS: usize = 8;

const SCAN_PAGE: u64 = 48;

/// Which open operations a tracking pass reconciles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpScope {
    /// Every one (the CLI, the daemon).
    All,
    /// Quai-ledger operations sent and not yet seen in a block: what a fast tracker watches.
    Inclusion,
    /// Everything [`OpScope::Inclusion`] leaves to that tracker.
    Rest,
}

impl OpScope {
    pub fn covers(self, op: &Operation) -> bool {
        let inclusion = op.store == "quai" && matches!(op.status, OpStatus::Signed | OpStatus::Submitted | OpStatus::Unknown);
        match self {
            OpScope::All => true,
            OpScope::Inclusion => inclusion,
            OpScope::Rest => !inclusion,
        }
    }
}

impl Session {
    /// Reconcile every open operation once, reading the head itself.
    pub async fn track(&mut self) -> Result<TrackReport> {
        let head = match self.provider().latest_header(ZONE).await? {
            Some(h) => h.number,
            None => return Err(CoreError::Network("node returned no header".into())),
        };
        self.track_at(head).await
    }

    /// Reconcile every open operation against a head the caller already read. The refresh checks
    /// the node one line earlier, so asking for the same header again bought nothing.
    pub async fn track_at(&mut self, head: u64) -> Result<TrackReport> {
        self.track_scoped(head, OpScope::All).await
    }

    /// [`Session::track_at`] for the operations `scope` covers, then incoming activity and
    /// listings (which [`Session::track_inclusion`] leaves alone).
    pub async fn track_scoped(&mut self, head: u64, scope: OpScope) -> Result<TrackReport> {
        self.reconcile_custody()?;
        let ops: Vec<Operation> = self.app.open_operations(&self.network.id)?.into_iter().filter(|o| scope.covers(o)).collect();
        let mut report = TrackReport::default();
        if scope != OpScope::Inclusion {
            self.audit_canonical_operations(&mut report).await?;
        }
        self.reconcile_ops(ops, head, &mut report).await?;
        self.observe_all(&mut report).await;
        Ok(report)
    }

    /// Only the Quai-ledger operations sent and not yet in a block — whether each has been mined.
    /// Cheap enough to run every few seconds while one is waiting, and nothing else: no activity,
    /// no listings. A sent operation's check reads the tip itself, so no head is needed.
    pub async fn track_inclusion(&mut self) -> Result<TrackReport> {
        self.reconcile_custody()?;
        let ops: Vec<Operation> =
            self.app.awaiting_inclusion(&self.network.id)?.into_iter().filter(|o| OpScope::Inclusion.covers(o)).collect();
        let mut report = TrackReport::default();
        self.reconcile_ops(ops, 0, &mut report).await?;
        Ok(report)
    }

    async fn reconcile_ops(&mut self, ops: Vec<Operation>, head: u64, report: &mut TrackReport) -> Result<()> {
        for op in ops {
            if matches!(op.status, OpStatus::Prepared) {
                continue;
            }
            let id = match parse_op_id(&op.id) {
                Ok(id) => id,
                Err(error) => {
                    report.errors.push(format!("{}: {error}", op.id));
                    continue;
                }
            };
            let Some(_guard) = self.try_operation_lock(id)? else { continue };
            match self.recheck_inclusion(&op, report).await {
                Ok(true) => continue,
                Err(error) => {
                    report.errors.push(format!("{}: {error}", op.id));
                    continue;
                }
                Ok(false) => {}
            }
            let result = if op.store == "quai" { self.track_account_op(&op, head).await } else { self.track_qi_op(&op, head).await };
            match result {
                Ok(Some((status, message, patch))) if status != op.status || patch.is_some() => {
                    // Only if nobody moved it meanwhile: another tracker that got there first
                    // has announced it already.
                    let canonical = patch.as_ref().and_then(|p| p["canonical_tx"].as_str());
                    let moved = self.app.transition_operation(&op.id, op.status, status, canonical, None, patch.as_ref())?;
                    if moved && status != op.status {
                        let level = match status {
                            OpStatus::Failed => "error",
                            OpStatus::Refunded => "warn",
                            _ => "success",
                        };
                        self.app.notify(level, &title_for(&op.kind, status), &message)?;
                        report.changes.push(StatusChange {
                            op_id: op.id.clone(),
                            kind: op.kind.clone(),
                            from: op.status,
                            to: status,
                            message,
                        });
                    }
                }
                Ok(_) => {}
                Err(e) => report.errors.push(format!("{}: {e}", &op.id[..8])),
            }
        }
        Ok(())
    }

    async fn audit_canonical_operations(&mut self, report: &mut TrackReport) -> Result<()> {
        let key = format!("canonical_audit:{}", self.network.id);
        let after = self.app.kv(&key)?.unwrap_or_default();
        let mut ops = self.app.canonical_audit_page(&self.network.id, &after, 8)?;
        if ops.is_empty() && !after.is_empty() {
            ops = self.app.canonical_audit_page(&self.network.id, "", 8)?;
        }
        for op in &ops {
            let Some(_guard) = self.try_operation_lock(parse_op_id(&op.id)?)? else { continue };
            if let Err(error) = self.recheck_inclusion(op, report).await {
                report.errors.push(format!("canonical audit {}: {error}", op.id));
            }
        }
        self.app.set_kv(&key, ops.last().map_or("", |op| op.id.as_str()))?;
        Ok(())
    }

    /// Revalidate source and destination anchors before advancing dependent state.
    async fn recheck_inclusion(&self, op: &Operation, report: &mut TrackReport) -> Result<bool> {
        recheck_operation_anchors(self.provider(), &self.app, op, report).await
    }

    async fn observe_all(&mut self, report: &mut TrackReport) {
        match self.observe_incoming().await {
            Ok(incoming) => report.incoming = incoming,
            Err(e) => report.errors.push(format!("activity: {e}")),
        }
        match self.observe_token_transfers().await {
            Ok(incoming) => report.incoming.extend(incoming),
            Err(e) => report.errors.push(format!("token activity: {e}")),
        }
        match self.watch_listings().await {
            Ok(sales) => report.sales = sales,
            Err(e) => report.errors.push(format!("listings: {e}")),
        }
    }

    /// Record token and NFT transfers from the explorer as activity (address lookups switch).
    /// Transfers belonging to the wallet's own operations are skipped; returns new incoming rows.
    pub async fn observe_token_transfers(&mut self) -> Result<Vec<Activity>> {
        let policy = self.config.data_policy();
        let explorer = crate::explorer::Explorer::for_network(&self.network);
        if !policy.explorer || explorer.backend == crate::explorer::Backend::ChainOnly {
            return Ok(Vec::new());
        }
        let network = self.network.id.clone();
        let sync_key = format!("token_transfers_synced:{network}");
        if self.app.kv(&sync_key)?.and_then(|v| v.parse::<u64>().ok()).is_some_and(|at| now().saturating_sub(at) < 120) {
            return Ok(Vec::new());
        }
        self.app.set_kv(&sync_key, &now().to_string())?;
        let seeded_key = format!("token_transfers_seeded:{network}");
        let seeded = self.app.kv(&seeded_key)?.is_some();
        let own: std::collections::HashSet<String> =
            self.app.operations(&network, 10_000)?.into_iter().filter_map(|o| o.tx_hash.map(|h| h.to_lowercase())).collect();
        let mut new = Vec::new();
        // One explorer request per account, so a sync covers at most [`TRANSFER_SYNC_ACCOUNTS`] of
        // them — but it starts where the last sync stopped and wraps, so an account past the eighth
        // is late rather than invisible. Taking the first eight every time meant a wallet with nine
        // accounts never saw incoming tokens on the ninth at all.
        let owners = self.quai_owner_addresses();
        let cursor_key = format!("token_transfers_cursor:{network}");
        let start = self.app.kv(&cursor_key)?.and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
        let start = if owners.is_empty() { 0 } else { start % owners.len() };
        let covered = owners.len().min(TRANSFER_SYNC_ACCOUNTS);
        if !owners.is_empty() {
            self.app.set_kv(&cursor_key, &((start + covered) % owners.len()).to_string())?;
        }
        for owner in owners.iter().cycle().skip(start).take(covered).map(String::clone) {
            let me = owner.to_lowercase();
            let transfers = explorer.token_transfers(&me, 50).await?;
            for t in transfers {
                if own.contains(&t.tx_hash) || (t.from != me && t.to != me) {
                    continue;
                }
                let incoming = t.to == me;
                let activity = Activity {
                    network: network.clone(),
                    key: format!("tt:{}:{}:{}", t.tx_hash, t.log_index, if incoming { "in" } else { "out" }),
                    direction: if incoming { "in".into() } else { "out".into() },
                    asset: if t.symbol.is_empty() { "TOKEN".into() } else { t.symbol.clone() },
                    amount: t.value.to_string(),
                    address: owner.clone(),
                    tx_hash: Some(t.tx_hash.clone()),
                    block: Some(t.block),
                    detail: serde_json::json!({
                        "source": "explorer",
                        "token": t.token,
                        "decimals": t.decimals,
                        "standard": t.kind.label(),
                        "token_id": t.token_id,
                        "name": t.name,
                        "counterparty": if incoming { t.from.clone() } else { t.to.clone() },
                    }),
                    observed: if t.timestamp > 0 { t.timestamp } else { now() },
                };
                if self.app.record_activity(&activity)? && seeded && incoming {
                    new.push(activity);
                }
            }
        }
        if !seeded {
            self.app.set_kv(&seeded_key, "1")?;
        }
        Ok(new)
    }

    async fn track_account_op(&mut self, op: &Operation, head: u64) -> Result<Option<(OpStatus, String, Option<serde_json::Value>)>> {
        let id = parse_op_id(&op.id)?;
        match op.status {
            OpStatus::Signed | OpStatus::Submitted | OpStatus::Unknown => {
                let address: QuaiAddress = op.account.parse().map_err(|_| CoreError::Storage("bad account".into()))?;
                let signer = WatchOnlySigner::new(address.address(), U256::from(self.network.chain_id))?;
                let mut session = AccountSession::new(&self.node.provider, &signer, &mut self.quai_store)?;
                let family = session.observe_candidates(id).await?;
                let Some(canonical) = family.canonical else {
                    let pending = family.candidates.iter().any(|(_, s)| matches!(s, AccountCandidateStatus::Pending));
                    if op.status == OpStatus::Unknown && pending {
                        return Ok(Some((OpStatus::Submitted, "the node has the transaction; waiting for inclusion".into(), None)));
                    }
                    return Ok(None);
                };
                let (block, outcome) = family
                    .candidates
                    .iter()
                    .find_map(|(h, s)| match s {
                        AccountCandidateStatus::Included { block, outcome, .. } if *h == canonical => Some((*block, *outcome)),
                        _ => None,
                    })
                    .ok_or_else(|| CoreError::Network("canonical candidate lost its inclusion".into()))?;
                let original = op.detail["original_tx"].as_str().or(op.tx_hash.as_deref());
                let replaced = original.is_some_and(|o| !o.eq_ignore_ascii_case(&canonical.to_string()));
                let patch = serde_json::json!({
                    "included_block": block.number,
                    "included_hash": block.hash.to_string(),
                    "canonical_tx": canonical.to_string(),
                    "replacement_won": replaced,
                    "finality": "unverified", "source_canonicality": "observed",
                });
                let receipt = canonical_receipt(self.provider(), canonical, block, outcome).await?;
                let mut patch = patch;
                let mut receipt_detail = op.detail.clone();
                if matches!(op.kind.as_str(), "swap" | "swap_exact_output") && receipt_detail["router"].is_null() {
                    receipt_detail["router"] = serde_json::json!(op.counterparty);
                }
                if matches!(op.kind.as_str(), "swap" | "swap_exact_output" | "claim_wqi" | "curve_buy")
                    && let Some(r) = &receipt
                    && let Some(out) = swap_output(r, &receipt_detail, self.network.wquai.as_deref())
                {
                    patch["actual_out"] = serde_json::json!(out.to_string());
                }
                if let Some(receipt) = &receipt
                    && let Some((input, output, fee)) = hartii_fill(receipt, op)
                {
                    patch["actual_in"] = serde_json::json!(input.to_string());
                    patch["actual_out"] = serde_json::json!(output.to_string());
                    patch["curve_fee"] = serde_json::json!(fee.to_string());
                    if op.kind == "hartii_buy"
                        && let Ok(offered) = U256::from_str_radix(&op.amount, 10)
                        && offered >= input
                    {
                        patch["native_refund"] = serde_json::json!((offered - input).to_string());
                    }
                }
                let fee = receipt.as_ref().and_then(|r| r.fee().ok()).map(|f| f.to_string());
                if let Some(fee) = fee {
                    // Not over a status another tracker has moved on since this one read it.
                    self.app.transition_operation(&op.id, op.status, op.status, Some(&canonical.to_string()), Some(&fee), None)?;
                }
                Ok(Some(match outcome {
                    ReceiptOutcome::PostState(_) => {
                        (OpStatus::Unknown, "source included with a legacy receipt; execution outcome is unverified".into(), Some(patch))
                    }
                    ReceiptOutcome::Succeeded => {
                        if matches!(op.kind.as_str(), "convert_quai_to_qi" | "unwrap_wqi") {
                            (
                                OpStatus::Settling,
                                format!("included at block {}; waiting for destination settlement", block.number),
                                Some(patch),
                            )
                        } else {
                            (OpStatus::Confirmed, format!("{} confirmed at block {}", describe(op), block.number), Some(patch))
                        }
                    }
                    ReceiptOutcome::Locked => {
                        (OpStatus::Locked, format!("included at block {} with locked value", block.number), Some(patch))
                    }
                    ReceiptOutcome::Failed => {
                        (OpStatus::Failed, format!("{} reverted at block {}", describe(op), block.number), Some(patch))
                    }
                }))
            }
            OpStatus::Settling | OpStatus::Locked if matches!(op.kind.as_str(), "convert_quai_to_qi" | "unwrap_wqi") => {
                let kind = match op.kind.as_str() {
                    "convert_quai_to_qi" => SettlementKind::Conversion,
                    "unwrap_wqi" => SettlementKind::WqiRedemption {
                        contract: op.detail["contract"]
                            .as_str()
                            .and_then(|s| s.parse().ok())
                            .ok_or_else(|| CoreError::Storage("missing WQI contract".into()))?,
                        etx_index: 0,
                    },
                    _ => return Ok(None),
                };
                let hash = op.detail["canonical_tx"]
                    .as_str()
                    .or(op.tx_hash.as_deref())
                    .and_then(|h| h.parse().ok())
                    .ok_or_else(|| CoreError::Storage("missing transaction hash".into()))?;
                let Some(update) = observe_settlement(&self.node.provider, &mut self.quai_store, op, id, hash, kind, head).await? else {
                    return Ok(None);
                };
                settle_result(op, (&update).into())
            }
            OpStatus::Locked => lock_progress(op, head),
            _ => Ok(None),
        }
    }

    /// Follow a Qi operation that has been replaced, through its whole family rather than its root.
    ///
    /// A reservation records the hash of the first transaction it signed and keeps it: that is its
    /// identity. `reconcile_operation` observes exactly that one hash, which is right until a
    /// replacement wins, and wrong forever after — the original can never be mined once its inputs
    /// are spent, but the node's Qi pool has no time expiry, so it sits there being reported as
    /// pending and the operation never leaves `submitted`.
    ///
    /// The family knows which member won. No key is ever resolved here, only receipts read, so
    /// this works on a locked wallet exactly as the rest of tracking does.
    async fn track_qi_family(
        &mut self,
        op: &Operation,
        id: quai_sdk::wallet::storage::ReservationId,
    ) -> Result<Option<(OpStatus, String, Option<serde_json::Value>)>> {
        let keys = NoQiKeys;
        let mut session = QiSession::with_keys(&self.node.provider, &keys, &mut self.qi_store);
        let family = session.observe_candidates(id).await;
        drop(session);
        let family = family?;
        // Nothing canonical yet: both candidates are still live and either may win.
        let Some(canonical) = family.canonical else { return Ok(None) };
        let (block, outcome) = family
            .candidates
            .iter()
            .find_map(|(hash, status)| match status {
                QiCandidateStatus::Included { block, outcome, .. } if *hash == canonical => Some((*block, *outcome)),
                _ => None,
            })
            .ok_or_else(|| CoreError::Network("the canonical Qi candidate lost its inclusion".into()))?;
        let height = block.number;
        let original = op.detail["original_tx"].as_str();
        let patch = serde_json::json!({
            "included_block": height,
            "included_hash": block.hash.to_string(),
            "canonical_tx": canonical.to_string(),
            "replacement_won": original.is_some_and(|o| !o.eq_ignore_ascii_case(&canonical.to_string())),
            "finality": "unverified", "source_canonicality": "observed",
        });
        Ok(Some(match outcome {
            ReceiptOutcome::Failed => (OpStatus::Failed, format!("{} failed at block {height}", describe(op)), Some(patch)),
            ReceiptOutcome::PostState(_) => {
                (OpStatus::Unknown, "source included with a legacy receipt; execution outcome is unverified".into(), Some(patch))
            }
            ReceiptOutcome::Locked => {
                (OpStatus::Locked, "source receipt reports locked value; current maturity is unverified".into(), Some(patch))
            }
            ReceiptOutcome::Succeeded if matches!(op.kind.as_str(), "convert_qi_to_quai" | "wrap_qi") => {
                (OpStatus::Settling, format!("included at block {height}; waiting for destination settlement"), Some(patch))
            }
            ReceiptOutcome::Succeeded => (OpStatus::Confirmed, format!("{} included at block {height}", describe(op)), Some(patch)),
        }))
    }

    async fn track_qi_op(&mut self, op: &Operation, head: u64) -> Result<Option<(OpStatus, String, Option<serde_json::Value>)>> {
        let id = parse_op_id(&op.id)?;
        match op.status {
            OpStatus::Signed | OpStatus::Submitted | OpStatus::Unknown => {
                // Once a replacement has been broadcast the root hash no longer says what became
                // of this operation, so the family is the only thing worth asking.
                if op.detail["candidates"].as_array().is_some_and(|c| !c.is_empty()) {
                    return self.track_qi_family(op, id).await;
                }
                match reconcile_operation(&self.node.provider, &mut self.qi_store, id).await? {
                    OperationObservation::Included { block, outcome, .. } => {
                        let height = u64::try_from(block.height).unwrap_or(0);
                        let patch = serde_json::json!({"included_block": height, "included_hash": block.hash.to_string(),
                            "canonical_tx": op.tx_hash, "finality": "unverified", "source_canonicality": "observed"});
                        Ok(Some(match outcome {
                            ReceiptOutcome::Failed => (OpStatus::Failed, format!("{} failed at block {height}", describe(op)), Some(patch)),
                            ReceiptOutcome::PostState(_) => (
                                OpStatus::Unknown,
                                "source included with a legacy receipt; execution outcome is unverified".into(),
                                Some(patch),
                            ),
                            ReceiptOutcome::Locked => (
                                OpStatus::Locked,
                                "source receipt reports locked value; current maturity is unverified".into(),
                                Some(patch),
                            ),
                            ReceiptOutcome::Succeeded if matches!(op.kind.as_str(), "convert_qi_to_quai" | "wrap_qi") => {
                                (OpStatus::Settling, format!("included at block {height}; waiting for destination settlement"), Some(patch))
                            }
                            ReceiptOutcome::Succeeded => {
                                (OpStatus::Confirmed, format!("{} included at block {height}", describe(op)), Some(patch))
                            }
                        }))
                    }
                    OperationObservation::Pending if op.status == OpStatus::Unknown => {
                        Ok(Some((OpStatus::Submitted, "the node has the transaction; waiting for inclusion".into(), None)))
                    }
                    OperationObservation::Reorganized => {
                        Ok(Some((OpStatus::Submitted, "a reorganization removed the earlier inclusion; tracking again".into(), None)))
                    }
                    _ => Ok(None),
                }
            }
            OpStatus::Settling | OpStatus::Locked if matches!(op.kind.as_str(), "convert_qi_to_quai" | "wrap_qi") => {
                let kind = if op.kind == "wrap_qi" { SettlementKind::QiWrapping } else { SettlementKind::Conversion };
                let hash = op.detail["canonical_tx"]
                    .as_str()
                    .or(op.tx_hash.as_deref())
                    .and_then(|hash| hash.parse().ok())
                    .ok_or_else(|| CoreError::Storage("missing canonical transaction hash".into()))?;
                let Some(update) = observe_settlement(&self.node.provider, &mut self.qi_store, op, id, hash, kind, head).await? else {
                    return Ok(None);
                };
                settle_result(op, (&update).into())
            }
            OpStatus::Locked => lock_progress(op, head),
            _ => Ok(None),
        }
    }

    /// Detect new Qi outputs and Quai balance increases not caused by our own operations.
    pub async fn observe_incoming(&mut self) -> Result<Vec<Activity>> {
        let network = self.network.id.clone();
        let seeded_key = format!("activity_seeded:{network}");
        let seeded = self.app.kv(&seeded_key)?.is_some();
        let own_hashes: std::collections::HashSet<String> =
            self.app.operations(&network, 10_000)?.into_iter().filter_map(|o| o.tx_hash.map(|h| h.to_lowercase())).collect();
        // Outputs credited to addresses our own conversions/redemptions named are not "received".
        let own_destinations: std::collections::HashSet<String> = self
            .app
            .operations(&network, 10_000)?
            .into_iter()
            .flat_map(|o| {
                ["destination", "refund", "beneficiary"]
                    .iter()
                    .filter_map(|k| o.detail[*k].as_str().map(str::to_lowercase))
                    .collect::<Vec<_>>()
            })
            .collect();
        let mut new = Vec::new();
        let summary = self.qi_summary()?;
        if summary.checkpoint_height.is_some() {
            for coin in &summary.coins {
                let hash = coin.outpoint.split(':').next().unwrap_or_default().to_lowercase();
                if own_hashes.contains(&hash) || own_destinations.contains(&coin.address.to_lowercase()) {
                    continue;
                }
                let activity = Activity {
                    network: network.clone(),
                    key: format!("qi:{}", coin.outpoint),
                    direction: "in".into(),
                    asset: "QI".into(),
                    amount: coin.qits.to_string(),
                    address: coin.address.clone(),
                    tx_hash: Some(hash),
                    block: None,
                    detail: serde_json::json!({"origin": coin.origin, "peer": coin.peer, "unlock_height": coin.unlock_height.to_string()}),
                    observed: now(),
                };
                if self.app.record_activity(&activity)? && seeded {
                    new.push(activity);
                }
            }
        }
        for account in self.meta.quai_accounts.clone() {
            if account.archived {
                continue;
            }
            let Ok(address) = account.address.parse::<QuaiAddress>() else { continue };
            let balance = self.provider().balance(address, BlockTag::Latest).await?;
            let key = format!("last_balance:{network}:{}", account.address.to_lowercase());
            let previous = self.app.kv(&key)?.and_then(|v| v.parse::<U256>().ok());
            if let Some(prev) = previous
                && balance > prev
            {
                let delta = balance - prev;
                let recent_own_credit = self
                    .app
                    .operations(&network, 50)?
                    .iter()
                    .any(|o| o.counterparty.eq_ignore_ascii_case(&account.address) && now().saturating_sub(o.updated) < 3600);
                if !recent_own_credit {
                    let activity = Activity {
                        network: network.clone(),
                        key: format!("quai:{}:{}", account.address.to_lowercase(), now()),
                        direction: "in".into(),
                        asset: "QUAI".into(),
                        amount: delta.to_string(),
                        address: account.address.clone(),
                        tx_hash: None,
                        block: None,
                        detail: serde_json::json!({"note": "balance increase"}),
                        observed: now(),
                    };
                    if self.app.record_activity(&activity)? && seeded {
                        new.push(activity);
                    }
                }
            }
            self.app.set_kv(&key, &balance.to_string())?;
        }
        if !seeded {
            self.app.set_kv(&seeded_key, "1")?;
        }
        for (title, text) in incoming_notices(&new) {
            self.app.notify("success", title, &text)?;
        }
        Ok(new)
    }

    /// Time-locked balances: locked Qi coins, account locked balances and settling operations.
    ///
    /// Reads the head and every account's locked balance itself. A caller that already has both —
    /// the dashboard refresh does — should use [`Session::locks_from`] instead and spend nothing.
    pub async fn locks(&mut self) -> Result<Vec<LockItem>> {
        let head = self.provider().latest_header(ZONE).await?.ok_or_else(|| CoreError::Network("node returned no header".into()))?.number;
        let mut locked = Vec::new();
        for account in self.meta.quai_accounts.clone() {
            if let Ok(address) = account.address.parse::<QuaiAddress>()
                && let Ok(balance) = self.provider().locked_quai_balance(address).await
            {
                locked.push((account.label, balance.balance));
            }
        }
        self.locks_from(head, &locked)
    }

    /// The same list built from a head and locked balances the caller already read.
    ///
    /// The refresh reads both while fetching account balances, so asking the node again — once for
    /// the header and once per account — was the whole cost of this stage for no new information.
    /// Everything left here is local: the Qi coin set and the operation journal.
    pub fn locks_from(&mut self, head: u64, locked: &[(String, U256)]) -> Result<Vec<LockItem>> {
        let mut items = Vec::new();
        let summary = self.qi_summary()?;
        let mut by_height: std::collections::BTreeMap<u64, (u64, usize)> = std::collections::BTreeMap::new();
        for coin in &summary.coins {
            let unlock = u64::try_from(coin.unlock_height).unwrap_or(u64::MAX);
            if unlock > head {
                let entry = by_height.entry(unlock).or_default();
                entry.0 += coin.qits;
                entry.1 += 1;
            }
        }
        for (height, (qits, count)) in by_height {
            let remaining = height - head;
            items.push(LockItem {
                source: format!("{count} Qi output(s)"),
                asset: "QI".into(),
                amount: amount::qi(U256::from(qits)),
                unlock_height: Some(height),
                blocks_remaining: Some(remaining),
                eta_secs: Some(remaining * BLOCK_SECS),
                unlocked: false,
            });
        }
        for (label, balance) in locked.iter().filter(|(_, b)| !b.is_zero()) {
            items.push(LockItem {
                source: format!("{label} locked conversion balance"),
                asset: "QUAI".into(),
                amount: amount::quai(*balance),
                unlock_height: None,
                blocks_remaining: None,
                eta_secs: None,
                unlocked: false,
            });
        }
        for op in self.app.open_operations(&self.network.id)? {
            if op.status == OpStatus::Settling {
                items.push(LockItem {
                    source: "awaiting settlement".into(),
                    asset: String::new(),
                    amount: describe(&op),
                    unlock_height: None,
                    blocks_remaining: None,
                    eta_secs: None,
                    unlocked: false,
                });
            }
        }
        Ok(items)
    }
}

/// A second receipt fetch must still match the exact candidate and anchor the SDK observed.
async fn canonical_receipt<T: Transport>(
    provider: &Provider<T>,
    hash: Hash32,
    block: BlockReference,
    outcome: ReceiptOutcome,
) -> Result<Option<quai_sdk::provider::Receipt>> {
    let receipt = provider
        .receipt(ZONE, hash)
        .await?
        .ok_or_else(|| CoreError::Network("canonical candidate receipt became unavailable; retry observation".into()))?;
    if receipt.transaction_hash != hash
        || receipt.inclusion.block_hash != block.hash
        || receipt.inclusion.block_number != block.number
        || receipt.outcome != outcome
        || provider.header_at(ZONE, block.number).await?.is_none_or(|header| header.hash != block.hash)
    {
        return Err(CoreError::Network("candidate receipt or block changed during observation; retry".into()));
    }
    Ok(Some(receipt))
}

async fn recheck_operation_anchors<T: Transport>(
    provider: &Provider<T>,
    app: &crate::appdb::AppDb,
    op: &Operation,
    report: &mut TrackReport,
) -> Result<bool> {
    let source = (op.detail["included_block"].as_u64(), op.detail["included_hash"].as_str());
    let destination = (op.detail["execution_block"].as_u64(), op.detail["execution_hash"].as_str());
    let final_source = if destination.0.is_some() && destination.1.is_some() { source } else { (None, None) };
    for (stage, (height, expected)) in [("source", source), ("destination", destination), ("source", final_source)] {
        let (Some(height), Some(expected)) = (height, expected) else { continue };
        let Some(header) = provider.header_at(ZONE, height).await? else {
            app.transition_operation(&op.id, op.status, op.status, None, None,
                Some(&serde_json::json!({format!("{stage}_canonicality"):"unverified", "spendability":"unverified", "finality":"unverified"})))?;
            report
                .errors
                .push(format!("{}: {stage} anchor is currently unavailable; retaining historical observation without advancing", op.id));
            return Ok(true);
        };
        if header.hash.to_string().eq_ignore_ascii_case(expected) {
            continue;
        }
        let mut patch = serde_json::json!({
            "reorged_anchor": {"stage":stage, "block":height, "hash":expected},
            "actual_out":null, "actual_in":null, "native_refund":null, "curve_fee":null, "credited_qits":null, "observed_credit_qits":null,
            "unobserved_qits":null, "credit_partial":null, "credit_head":null, "credit_head_hash":null,
            "credit_visibility":null, "unlock_height":null,
            "execution_block":null, "execution_hash":null, "execution_tx":null,
            "destination_receipt":null, "destination_canonicality":"unverified", "conversion_effect":null,
            "quai_lock":null, "account_credit":null, "refund_credit":null,
            "scan_next":null, "scan_last_number":null, "scan_last_hash":null,
            "spendability":"unverified", "finality":"unverified"
        });
        let next = if stage == "source" {
            patch["included_block"] = serde_json::Value::Null;
            patch["included_hash"] = serde_json::Value::Null;
            patch["canonical_tx"] = serde_json::Value::Null;
            patch["source_canonicality"] = serde_json::json!("unverified");
            OpStatus::Submitted
        } else {
            OpStatus::Settling
        };
        if app.transition_operation(&op.id, op.status, next, None, None, Some(&patch))? {
            report.changes.push(StatusChange {
                op_id: op.id.clone(),
                kind: op.kind.clone(),
                from: op.status,
                to: next,
                message: format!("{stage} inclusion was reorganized; dependent credit and scan observations were invalidated"),
            });
        }
        return Ok(true);
    }
    if destination.0.is_some()
        && destination.1.is_none()
        && matches!(op.kind.as_str(), "convert_quai_to_qi" | "convert_qi_to_quai" | "wrap_qi" | "unwrap_wqi")
    {
        // Old rows recorded a height without its hash. Keep their historical observation separate
        // and restart a bounded, intent-bound scan; a height alone cannot be audited for reorgs.
        let patch = serde_json::json!({
            "legacy_destination_observation": {"block":destination.0,"credited_qits":op.detail["credited_qits"],"actual_out":op.detail["actual_out"]},
            "execution_block":null,"execution_hash":null,"execution_tx":null,"credited_qits":null,"actual_out":null,
            "unlock_height":null,"quai_lock":null,"scan_next":null,"scan_last_number":null,"scan_last_hash":null,
            "destination_canonicality":"unverified","spendability":"unverified","finality":"unverified"
        });
        app.transition_operation(&op.id, op.status, OpStatus::Settling, None, None, Some(&patch))?;
        return Ok(true);
    }
    let mut patch = serde_json::json!({"finality":"unverified"});
    if source.0.is_some() && source.1.is_some() {
        patch["source_canonicality"] = serde_json::json!("observed");
    }
    if destination.0.is_some() && destination.1.is_some() {
        patch["destination_canonicality"] = serde_json::json!("observed");
    }
    if patch.as_object().unwrap().iter().any(|(key, value)| op.detail.get(key) != Some(value)) {
        app.transition_operation(&op.id, op.status, op.status, None, None, Some(&patch))?;
    }
    Ok(false)
}

/// Continue only SDK-revalidated cursors. A known execution is re-read at its exact block;
/// advancing past it would turn partial/temporarily missing output observations into permanent gaps.
async fn observe_settlement<T: Transport>(
    provider: &Provider<T>,
    store: &mut quai_sdk::wallet::storage::SqliteStore,
    op: &Operation,
    id: quai_sdk::wallet::storage::ReservationId,
    hash: Hash32,
    kind: SettlementKind,
    head: u64,
) -> Result<Option<SettlementUpdate>> {
    if let Some(cursor) = revalidate_settlement_cursor(provider, store, id, hash, kind, ZONE).await? {
        let from = cursor.execution().map(|b| b.number).unwrap_or(cursor.scanned_through().number.saturating_add(1));
        if from > head {
            return Ok(None);
        }
        return Ok(Some(cursor.track(provider, store, head.min(from.saturating_add(SCAN_PAGE)), 4096, 65_536, 512).await?));
    }
    let from = op.detail["included_block"].as_u64().unwrap_or(head).max(1);
    if from > head {
        return Ok(None);
    }
    let request = EtxScanRequest::new(ZONE, from, head.min(from.saturating_add(SCAN_PAGE)), 4096, 65_536);
    Ok(Some(track_settlement(provider, store, id, hash, kind, request, 512).await?))
}

#[derive(Clone, Copy)]
struct SettlementEvidence<'a> {
    conversion: Option<&'a quai_sdk::provider::ConversionObservation>,
    external: Option<&'a quai_sdk::provider::ExternalObservation>,
    qi_credit: Option<&'a quai_sdk::provider::QiCreditObservation>,
}
impl<'a> From<&'a SettlementUpdate> for SettlementEvidence<'a> {
    fn from(update: &'a SettlementUpdate) -> Self {
        Self { conversion: update.conversion.as_ref(), external: update.external.as_ref(), qi_credit: update.qi_credit.as_ref() }
    }
}

fn settlement_patch(update: SettlementEvidence<'_>) -> serde_json::Value {
    let scan = update.conversion.as_ref().and_then(|c| c.scan.as_ref()).or_else(|| update.external.as_ref().and_then(|e| e.scan.as_ref()));
    let mut patch = serde_json::json!({"finality":"unverified", "spendability":"unverified"});
    if let Some(last) = scan.and_then(|scan| scan.last_block) {
        patch["scan_next"] = serde_json::json!(last.number.saturating_add(1));
        patch["scan_last_number"] = serde_json::json!(last.number);
        patch["scan_last_hash"] = serde_json::json!(last.hash.to_string());
    }
    if let Some(execution) = scan.and_then(|scan| scan.execution.as_ref()) {
        if let Some(inclusion) = execution.transaction.inclusion {
            patch["execution_block"] = serde_json::json!(inclusion.block_number);
            patch["execution_hash"] = serde_json::json!(inclusion.block_hash.to_string());
            patch["execution_tx"] = serde_json::json!(execution.transaction.hash.to_string());
            patch["destination_canonicality"] = serde_json::json!("observed");
        }
        patch["destination_receipt"] = serde_json::json!(execution.receipt.as_ref().map(|r| match r.outcome {
            ReceiptOutcome::Succeeded => "succeeded",
            ReceiptOutcome::Failed => "failed",
            ReceiptOutcome::Locked => "locked",
            ReceiptOutcome::PostState(_) => "legacy",
        }));
    }
    patch
}

fn settle_result(op: &Operation, update: SettlementEvidence<'_>) -> Result<Option<(OpStatus, String, Option<serde_json::Value>)>> {
    let mut patch = settlement_patch(update);
    let credit = update.qi_credit.as_ref();
    if op.kind == "wrap_qi" {
        let outcome = update.external.as_ref().and_then(|e| e.outcome);
        let (status, message) = match outcome {
            Some(ReceiptOutcome::Succeeded) => (OpStatus::Settled, "wrapped Qi deposit executed; claim WQI separately"),
            Some(ReceiptOutcome::Failed) => (OpStatus::Failed, "wrapping deposit failed at the destination"),
            Some(ReceiptOutcome::Locked) => (OpStatus::Locked, "wrapping deposit reports locked value; current maturity is unverified"),
            _ => (OpStatus::Settling, "destination execution or receipt remains unverified"),
        };
        if status == OpStatus::Settled {
            patch["spendability"] = serde_json::json!("requires_wqi_claim");
        }
        return Ok(Some((status, message.into(), Some(patch))));
    }
    if op.kind == "unwrap_wqi" {
        match update.external.and_then(|external| external.outcome) {
            Some(ReceiptOutcome::Failed) => {
                if let Some(c) = credit {
                    merge_patch(&mut patch, credit_status(c).2);
                }
                return Ok(Some((
                    OpStatus::Failed,
                    "redemption execution failed; any partial outputs are recorded separately".into(),
                    Some(patch),
                )));
            }
            None | Some(ReceiptOutcome::PostState(_)) => {
                return Ok(Some((OpStatus::Settling, "redemption receipt outcome remains unverified".into(), Some(patch))));
            }
            _ => {}
        }
    }
    let effect = update.conversion.as_ref().and_then(|c| c.effect);
    if let Some(effect) = effect {
        patch["conversion_effect"] = serde_json::json!(format!("{effect:?}"));
        match effect {
            ConversionEffect::ExecutionFailed { .. } => {
                if let Some(c) = credit {
                    let (_, _, observed) = credit_status(c);
                    merge_patch(&mut patch, observed);
                }
                return Ok(Some((
                    OpStatus::Failed,
                    "destination execution failed; any observed partial outputs are recorded separately".into(),
                    Some(patch),
                )));
            }
            ConversionEffect::ReceiptUnavailable { .. } | ConversionEffect::UnknownSubtype(_) | ConversionEffect::LegacyOutcome { .. } => {
                return Ok(Some((
                    OpStatus::Settling,
                    "destination effect is unverified; no credit or maturity is inferred".into(),
                    Some(patch),
                )));
            }
            ConversionEffect::RefundReported { .. } if credit.is_none() => {
                patch["refund_credit"] = serde_json::json!("unverified");
                return Ok(Some((
                    OpStatus::Refunded,
                    "refund processing reported; account credit and spendability remain unverified".into(),
                    Some(patch),
                )));
            }
            _ => {}
        }
    }
    if let Some(c) = credit {
        let (mut status, message, observed) = credit_status(c);
        if matches!(effect, Some(ConversionEffect::RefundReported { .. })) && status == OpStatus::Settled {
            status = OpStatus::Refunded;
        }
        merge_patch(&mut patch, observed);
        return Ok(Some((status, message, Some(patch))));
    }
    if op.kind == "convert_qi_to_quai" && effect.is_some() {
        let locked = matches!(effect, Some(ConversionEffect::Locked { .. }));
        patch["quai_lock"] = serde_json::json!(locked);
        // The destination of this direction is a Quai account, so there is no attributed credit
        // observation to wait for: the SDK resolves a beneficiary only through `QiAddress`
        // (quai-provider `qi_credit.rs`), which a Quai destination can never satisfy, and the one
        // account-side reading the node offers — `quai_getLockedBalance` — is an aggregate its own
        // documentation says carries no per-conversion attribution, maturity or spendability.
        //
        // So `account_credit` stays unverified whatever happens, and tracking must not pretend a
        // later observation will settle it. A reported conversion is therefore closed on its
        // destination receipt with both qualifiers kept distinct (plan rule 4): the execution is
        // observed, the credit is reported by that receipt rather than verified, and spendability
        // is unattributed. A locked receipt keeps the operation open, since its maturity is a real
        // future event even though only the aggregate lock balance reports it.
        if matches!(effect, Some(ConversionEffect::ConversionReported))
            && patch["destination_canonicality"] == serde_json::json!("observed")
        {
            // The proceeds of this direction are held by the protocol's conversion lockup — the
            // wallet says so in its own review ("locked ~2 weeks") — so a succeeded receipt reports
            // execution, not maturity, and the account's spendable balance does not move yet.
            // `Locked` is what that is; `Settled` would promise a spendable credit that is not
            // there. No unlock height is claimed, because none is attributable to this operation.
            patch["quai_lock"] = serde_json::json!(true);
            patch["account_credit"] = serde_json::json!("reported_by_destination_receipt");
            patch["spendability"] = serde_json::json!("held_by_conversion_lockup");
            return Ok(Some((
                OpStatus::Locked,
                "account conversion executed at the destination; its credit is reported by that receipt and held by the protocol conversion lockup, whose maturity this operation cannot observe".into(),
                Some(patch),
            )));
        }
        patch["account_credit"] = serde_json::json!("unverified");
        return Ok(Some((
            if locked { OpStatus::Locked } else { OpStatus::Settling },
            "account conversion processing observed; operation-specific credit and spendability are unverified".into(),
            Some(patch),
        )));
    }
    Ok(Some((OpStatus::Settling, "destination execution or current credit remains unverified".into(), Some(patch))))
}

fn merge_patch(target: &mut serde_json::Value, source: serde_json::Value) {
    if let (Some(target), Some(source)) = (target.as_object_mut(), source.as_object()) {
        target.extend(source.iter().map(|(key, value)| (key.clone(), value.clone())));
    }
}

fn credit_status(c: &quai_sdk::provider::QiCreditObservation) -> (OpStatus, String, serde_json::Value) {
    let observed = c.locked_qits + c.unlocked_qits;
    let unlock = c.outputs.iter().map(|o| u64::try_from(o.lock).unwrap_or(u64::MAX)).max().unwrap_or(0);
    let complete = !c.outputs.is_empty() && c.unobserved_qits.is_zero();
    let mut patch = serde_json::json!({
        "observed_credit_qits": observed.to_string(), "unobserved_qits": c.unobserved_qits.to_string(),
        "credit_partial": !complete, "credit_head": c.head.number, "credit_head_hash": c.head.hash.to_string(),
        "execution_block": c.execution.number, "execution_hash": c.execution.hash.to_string(),
        "execution_tx": c.transaction_hash.to_string(), "unlock_height": unlock,
        "credit_visibility": if complete { "currently_indexed" } else { "partial_or_spent_trimmed_unindexed" },
        "spendability": "unverified"
    });
    if !complete {
        return (
            OpStatus::Settling,
            "current credit observation is incomplete; outputs may be spent, trimmed or not indexed".into(),
            patch,
        );
    }
    patch["credited_qits"] = serde_json::json!(observed.to_string());
    if c.locked_qits.is_zero() && unlock <= c.head.number {
        patch["spendability"] = serde_json::json!("indexed_unlocked_at_observed_head");
        (
            OpStatus::Settled,
            format!("{} Qi currently indexed as unlocked; wallet claims and later spends still apply", amount::qi(observed)),
            patch,
        )
    } else {
        (OpStatus::Locked, format!("{} Qi currently indexed; reported lock through block {unlock}", amount::qi(observed)), patch)
    }
}

fn lock_progress(op: &Operation, head: u64) -> Result<Option<(OpStatus, String, Option<serde_json::Value>)>> {
    match op.detail["unlock_height"].as_u64() {
        Some(unlock) if unlock <= head => Ok(Some((OpStatus::Settled, format!("{} is now spendable", describe(op)), None))),
        _ => Ok(None),
    }
}

/// Human description of an operation.
/// Whether an activity row is a real incoming payment or only a balance that rose.
///
/// Without an explorer the wallet cannot read transfers, so it compares balances between
/// refreshes. That difference is net: the account's own spending, gas included, is already taken
/// out of it, so calling the number "received" would claim an amount nobody sent.
/// One notification per incoming payment, not per output it arrived in.
///
/// Qi is UTXO-based: a single payment turns up as several coins, one per denomination, and each is
/// its own activity row because each has its own outpoint and its own unlock height. Notifying per
/// row would tell someone they had been paid three times when they were paid once — so rows are
/// grouped by the transaction that created them and the amounts summed. A payment code derives a
/// fresh address per output, so one payment can also land on several addresses; the total is what
/// matters, and the count is said rather than one address being picked to stand for the rest.
///
/// A QUAI balance increase has no transaction behind it (it is a difference between two reads), so
/// those group by account instead.
pub(crate) fn incoming_notices(new: &[crate::appdb::Activity]) -> Vec<(&'static str, String)> {
    let mut groups: Vec<(String, Vec<&crate::appdb::Activity>)> = Vec::new();
    for a in new {
        // Never across assets, and never across separate payments.
        let key = format!("{}:{}", a.asset, a.tx_hash.clone().unwrap_or_else(|| a.address.to_lowercase()));
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, items)) => items.push(a),
            None => groups.push((key, vec![a])),
        }
    }
    groups.iter().filter_map(|(_, items)| notice_for(items)).collect()
}

/// The one line for a group of outputs that arrived together.
fn notice_for(items: &[&crate::appdb::Activity]) -> Option<(&'static str, String)> {
    let first = items.first()?;
    let total: U256 = items.iter().filter_map(|a| a.amount.parse::<U256>().ok()).fold(U256::ZERO, |sum, v| sum + v);
    let shown = if first.asset == "QI" { format!("{} Qi", amount::qi(total)) } else { format!("{} QUAI", amount::quai(total)) };
    let mut addresses: Vec<String> = items.iter().map(|a| a.address.to_lowercase()).collect();
    addresses.sort();
    addresses.dedup();
    let where_to = match addresses.as_slice() {
        [one] => crate::session::short_address(one),
        many => format!("{} of your addresses", many.len()),
    };
    if is_balance_increase(first) {
        return Some(("Balance rose", format!("by {shown} at {where_to}")));
    }
    // The coin count matters for Qi: it is what a later send has to work with.
    let coins = if items.len() > 1 { format!(" · {} coins", items.len()) } else { String::new() };
    Some(("Payment received", format!("{shown} to {where_to}{coins}")))
}

pub fn is_balance_increase(a: &crate::appdb::Activity) -> bool {
    a.detail["note"].as_str() == Some("balance increase")
}

/// How to describe an incoming activity: what was received, or what the balance did.
pub fn incoming_verb(a: &crate::appdb::Activity) -> &'static str {
    if is_balance_increase(a) { "balance rose by" } else { "received" }
}

/// What a transaction cost, in the coin of the ledger it ran on: QUAI for account transactions,
/// Qi for UTXO ones. A token transfer moves no native value, but still pays gas in QUAI.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TxCost {
    /// Native value the transaction carried. `None` when it cannot be told from what is stored.
    pub value: Option<U256>,
    /// The fee: paid when `fee_final`, otherwise the most it can cost.
    pub fee: Option<U256>,
    pub fee_final: bool,
    /// Counted in Qi (qits) rather than QUAI (wei).
    pub qi: bool,
}

impl TxCost {
    /// `1.5 QUAI`, `12.345 Qi`.
    pub fn text(&self, v: U256) -> String {
        native_text(v, self.qi)
    }
}

/// A native amount with its unit: QUAI to six decimals (fees are small), Qi to its three.
pub fn native_text(v: U256, qi: bool) -> String {
    if qi {
        format!("{} Qi", amount::group_thousands(&amount::qi(v)))
    } else {
        format!("{} QUAI", amount::group_thousands(&amount::format_amount_short(v, amount::QUAI_DECIMALS, 6)))
    }
}

/// Operations that move only tokens or NFTs, so carry no native value. Everything with a
/// `native_value` recorded at preparation is answered from that instead.
const TOKEN_ONLY_KINDS: &[&str] = &[
    "send_token",
    "approve",
    "revoke",
    "nft_list",
    "nft_reprice",
    "nft_unlist",
    "nft_transfer",
    "claim_wqi",
    "unwrap_wqi",
    "unwrap_quai",
    "stake",
    "unstake",
    "harvest",
    "exit",
    "incentivize",
    "curve_sell",
    "curve_claim",
];

/// The value and fee of one of this wallet's own operations.
pub fn op_cost(op: &Operation) -> TxCost {
    let qi = op.store == "qi";
    let parse = |t: &str| U256::from_str_radix(t, 10).ok();
    let value = match op.detail["native_value"].as_str() {
        Some(v) => parse(v),
        None if qi && op.asset.eq_ignore_ascii_case("QI") => parse(&op.amount),
        None if !qi && op.asset.eq_ignore_ascii_case("QUAI") => parse(&op.amount),
        None if !qi && TOKEN_ONLY_KINDS.contains(&op.kind.as_str()) => Some(U256::ZERO),
        None => None,
    };
    // An account transaction's stored fee is its maximum until the receipt replaces it; a Qi
    // transaction's fee is fixed by its inputs and outputs the moment it is signed.
    let fee = parse(&op.fee).filter(|_| !op.fee.is_empty());
    let fee_final = if qi { !matches!(op.status, OpStatus::Prepared) } else { op.detail["included_block"].is_number() };
    TxCost { value, fee, fee_final, qi }
}

/// The value and fee of any account transaction, read from the node: for rows the wallet only
/// observed (incoming, or sent from elsewhere). A Qi transaction answers with neither: its outputs
/// include the sender's change, and its fee is the inputs less the outputs, which the node does not
/// state — the activity row's own amount is what it moved to this wallet.
pub async fn chain_cost(provider: &crate::network::WalletProvider, hash: &str) -> Result<TxCost> {
    use quai_sdk::provider::TransactionDetails;
    let hash: quai_sdk::primitives::Hash32 = hash.parse().map_err(|_| CoreError::Invalid(format!("`{hash}` is not a transaction hash")))?;
    let tx =
        provider.transaction(ZONE, hash).await?.ok_or_else(|| CoreError::NotFound("the node does not have this transaction".into()))?;
    let (value, qi) = match &tx.details {
        TransactionDetails::Quai(t) => (Some(t.value), false),
        TransactionDetails::External(t) => (Some(t.value), false),
        TransactionDetails::Qi(_) => (None, true),
    };
    let fee = if qi { None } else { provider.receipt(ZONE, hash).await?.and_then(|r| r.fee().ok()) };
    Ok(TxCost { value, fee_final: fee.is_some(), fee, qi })
}

pub fn describe(op: &Operation) -> String {
    let amount_text = match op.asset.as_str() {
        "QUAI" => format!("{} QUAI", amount::quai(op.amount.parse().unwrap_or_default())),
        "QI" => format!("{} Qi", amount::qi(op.amount.parse().unwrap_or_default())),
        other => {
            let decimals = op.detail["decimals"].as_u64().unwrap_or(if other == "WQUAI" || other == "WQI" { 18 } else { 0 });
            format!("{} {other}", amount::format_amount(op.amount.parse().unwrap_or_default(), decimals as u8))
        }
    };
    let verb = match op.kind.as_str() {
        "send_quai" | "send_token" | "send_qi" => "send",
        "convert_quai_to_qi" => "QUAI→Qi conversion",
        "convert_qi_to_quai" => "Qi→QUAI conversion",
        "wrap_qi" => "Qi wrap",
        "claim_wqi" => "WQI claim",
        "unwrap_wqi" => "WQI redemption",
        "wrap_quai" => "QUAI wrap",
        "unwrap_quai" => "WQUAI unwrap",
        "approve" => "approval",
        "revoke" => "revocation",
        "notify" => "mailbox notify",
        "sweep_qi" => "Qi consolidation",
        "aggregate_qi" => "Qi aggregation",
        "fill_gap" => "nonce gap fill",
        "swap" | "swap_exact_output" => "swap",
        "curve_buy" => "curve buy",
        "curve_sell" => "curve sale",
        "curve_claim" => "curve claim",
        "nft_buy" => "bought",
        "nft_list" => "listed",
        "nft_reprice" => "re-priced",
        "nft_unlist" => "listing cancelled",
        "nft_transfer" => "sent",
        other => other,
    };
    if op.kind == "approve" && op.detail["module"].is_string() {
        return "marketplace module approval".into();
    }
    if op.kind == "approve" && op.detail["operator"].is_string() {
        return "marketplace collection approval".into();
    }
    match op.kind.as_str() {
        "notify" => format!("{verb} to {}", crate::session::short_code(&op.counterparty)),
        "fill_gap" => verb.to_string(),
        "curve_buy" => {
            let to_symbol = op.detail["to_symbol"].as_str().unwrap_or("?");
            let decimals = op.detail["to_decimals"].as_u64().and_then(|value| u8::try_from(value).ok()).unwrap_or(18);
            let out =
                amount::format_amount_short(op.detail["expected_out"].as_str().unwrap_or("0").parse().unwrap_or_default(), decimals, 2);
            format!("bought ≈{} {to_symbol} with {amount_text} on its curve", amount::group_thousands(&out))
        }
        "curve_sell" => format!("sold {amount_text} to its curve"),
        "curve_claim" => format!("claimed {amount_text} from a curve"),
        "swap" | "swap_exact_output" => {
            let to_symbol = op.detail["to_symbol"].as_str().unwrap_or("?");
            let to_decimals = op.detail["to_decimals"].as_u64().unwrap_or(18) as u8;
            let (out, approx) = match op.detail["actual_out"].as_str() {
                Some(a) => (a, ""),
                None => (op.detail["expected_out"].as_str().unwrap_or("0"), "≈"),
            };
            let out = amount::format_amount_short(out.parse().unwrap_or_default(), to_decimals, 4);
            let paid = if op.kind == "swap_exact_output" { format!("up to {amount_text}") } else { amount_text };
            format!("swap {paid} → {approx}{} {to_symbol}", amount::group_thousands(&out))
        }
        "nft_list" | "nft_reprice" | "nft_unlist" => {
            let name = op.detail["name"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| format!("NFT #{}", op.detail["token_id"].as_str().unwrap_or("?")));
            match (op.kind.as_str(), op.detail["closed"].as_str()) {
                ("nft_unlist", _) => format!("cancelled the listing of {name}"),
                (_, Some("sold")) => format!("sold {name} for {amount_text}"),
                (_, Some(_)) => format!("listing of {name} ended"),
                ("nft_reprice", _) => format!("re-priced {name} to {amount_text}"),
                _ => format!("listed {name} for {amount_text}"),
            }
        }
        "nft_buy" | "nft_transfer" => {
            let name = op.detail["name"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| format!("NFT #{}", op.detail["token_id"].as_str().unwrap_or("?")));
            if op.kind == "nft_buy" { format!("bought {name} for {amount_text}") } else { format!("sent {name}") }
        }
        _ => format!("{verb} of {amount_text}"),
    }
}

fn title_for(kind: &str, status: OpStatus) -> String {
    let what = match kind {
        "convert_quai_to_qi" | "convert_qi_to_quai" => "Conversion",
        "wrap_qi" | "claim_wqi" | "unwrap_wqi" | "wrap_quai" | "unwrap_quai" => "Wrap",
        "send_qi" | "send_quai" | "send_token" | "nft_transfer" => "Transfer",
        "swap" | "swap_exact_output" => "Swap",
        "nft_buy" => "Purchase",
        "nft_list" | "nft_reprice" | "nft_unlist" => "Listing",
        _ => "Transaction",
    };
    format!("{what} {}", status.as_str())
}

/// `Transfer(address,address,uint256)` topic.
const TRANSFER_TOPIC: &str = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
/// WETH-style `Withdrawal(address,uint256)` topic.
const WITHDRAWAL_TOPIC: &str = "0x7fcf532c15f0a6db0bd6d0e038bea71d30d808c7d98cb3bf7268a95bf5081b65";

/// Actual swap output from a receipt: the output token's `Transfer` to the recipient, or the
/// wrapped QUAI `Withdrawal` for swaps that pay out native QUAI.
fn hartii_fill(receipt: &quai_sdk::provider::Receipt, op: &Operation) -> Option<(U256, U256, U256)> {
    let topic = match op.kind.as_str() {
        "hartii_buy" => "0xbeae048c6d270d9469f86cf6e8fedda3c60ad770f16c24c9fc131c8e9a09101d",
        "hartii_sell" => "0x846c37eef631e0943682d87352ec117c20008eb7f425c9b85ac011a6d4774cc0",
        _ => return None,
    };
    if receipt.outcome != ReceiptOutcome::Succeeded {
        return None;
    }
    let curve: quai_sdk::Address = op.detail["curve"].as_str()?.parse().ok()?;
    let owner: quai_sdk::Address = op.detail["recipient"].as_str()?.parse().ok()?;
    if receipt.to != Some(curve) || !op.account.eq_ignore_ascii_case(&owner.to_string()) {
        return None;
    }
    let mut found = None;
    for log in &receipt.logs {
        if log.removed || log.address != curve || log.topics.first()?.to_string() != topic {
            continue;
        }
        if log.topics.len() != 2 || log.data.bytes().len() != 96 {
            return None;
        }
        let recipient = log.topics[1].bytes();
        if recipient[..12].iter().any(|b| *b != 0) || &recipient[12..] != owner.bytes() {
            continue;
        }
        let data = log.data.bytes();
        let fill = (U256::from_be_slice(&data[..32]), U256::from_be_slice(&data[32..64]), U256::from_be_slice(&data[64..]));
        if found.replace(fill).is_some() {
            return None;
        }
    }
    let fill = found?;
    if op.kind == "hartii_buy" && swap_output(receipt, &op.detail, None) != Some(fill.1) {
        return None;
    }
    Some(fill)
}

pub fn swap_output(receipt: &quai_sdk::provider::Receipt, detail: &serde_json::Value, wquai: Option<&str>) -> Option<U256> {
    if receipt.outcome != ReceiptOutcome::Succeeded {
        return None;
    }
    let recipient: QuaiAddress = detail["recipient"].as_str()?.parse().ok()?;
    let to_token = detail["to_token"].as_str()?;
    let topic_address = |topic: &Hash32| -> Option<quai_sdk::Address> {
        let bytes = topic.bytes();
        if bytes[..12].iter().any(|b| *b != 0) {
            return None;
        }
        Some(quai_sdk::Address::from_bytes(bytes[12..].try_into().ok()?))
    };
    let native = to_token.eq_ignore_ascii_case("quai");
    let token: quai_sdk::Address = if native { wquai?.parse().ok()? } else { to_token.parse().ok()? };
    let router = if native {
        Some(match detail["router"].as_str() {
            Some(router) => router.parse::<quai_sdk::Address>().ok()?,
            None => receipt.to?,
        })
    } else {
        None
    };
    if native && receipt.to != router {
        return None;
    }
    let mut total = U256::ZERO;
    let mut debited = U256::ZERO;
    let mut found = false;
    for log in &receipt.logs {
        if log.address != token || log.removed {
            continue;
        }
        let Some(topic0) = log.topics.first().map(ToString::to_string) else { continue };
        if !native
            && topic0.eq_ignore_ascii_case(TRANSFER_TOPIC)
            && log.topics.len() == 3
            && topic_address(&log.topics[1]) == Some(recipient.address())
        {
            if log.data.bytes().len() != 32 {
                return None;
            }
            debited = debited.checked_add(U256::from_be_slice(log.data.bytes()))?;
        }
        let matches = if native {
            topic0.eq_ignore_ascii_case(WITHDRAWAL_TOPIC) && log.topics.len() == 2 && topic_address(&log.topics[1]) == router
        } else {
            topic0.eq_ignore_ascii_case(TRANSFER_TOPIC)
                && log.topics.len() == 3
                && topic_address(&log.topics[2]) == Some(recipient.address())
        };
        if !matches {
            continue;
        }
        if log.data.bytes().len() != 32 {
            return None;
        }
        total = total.checked_add(U256::from_be_slice(log.data.bytes()))?;
        found = true;
    }
    if found { total.checked_sub(debited) } else { None }
}

/// Format seconds as `2d 4h`, `3h 10m` or `45s`.
pub fn human_duration(secs: u64) -> String {
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3600;
    let m = (secs % 3600) / 60;
    if d > 0 && h > 0 {
        format!("{d}d {h}h")
    } else if d > 0 {
        format!("{d}d")
    } else if h > 0 && m > 0 {
        format!("{h}h {m}m")
    } else if h > 0 {
        format!("{h}h")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
mod incoming_wording {
    use super::*;
    use crate::appdb::Activity;

    fn activity(detail: serde_json::Value) -> Activity {
        Activity {
            network: "local".into(),
            key: "k".into(),
            direction: "in".into(),
            asset: "QUAI".into(),
            amount: "471111783000000000000".into(),
            address: "0x000b".into(),
            tx_hash: None,
            block: None,
            detail,
            observed: 0,
        }
    }

    /// A balance read between refreshes is net of the account's own spending, so it is never
    /// reported as an amount somebody sent.
    #[test]
    fn a_balance_that_rose_is_not_called_a_payment() {
        let observed = activity(serde_json::json!({"note": "balance increase"}));
        assert!(is_balance_increase(&observed));
        assert_eq!(incoming_verb(&observed), "balance rose by");
        // A transfer the explorer really read keeps the plain word.
        let transfer = activity(serde_json::json!({"peer": "0x00ab"}));
        assert!(!is_balance_increase(&transfer));
        assert_eq!(incoming_verb(&transfer), "received");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pending lane watches exactly what the worker's pass leaves out, so between them every
    /// open operation is tracked once: sent Quai transactions waiting for a block go to the lane;
    /// Qi, settling and locked ones stay with the refresh.
    #[test]
    fn inclusion_and_rest_split_the_journal() {
        let op = |store: &str, status: OpStatus| Operation {
            id: "aa".into(),
            network: "mainnet".into(),
            kind: "send_quai".into(),
            store: store.into(),
            account: "0xabc".into(),
            status,
            tx_hash: None,
            asset: "QUAI".into(),
            amount: "1".into(),
            counterparty: String::new(),
            fee: String::new(),
            detail: serde_json::json!({}),
            created: 0,
            updated: 0,
        };
        for status in [OpStatus::Signed, OpStatus::Submitted, OpStatus::Unknown] {
            assert!(OpScope::Inclusion.covers(&op("quai", status)) && !OpScope::Rest.covers(&op("quai", status)));
            assert!(!OpScope::Inclusion.covers(&op("qi", status)) && OpScope::Rest.covers(&op("qi", status)), "Qi stays with the refresh");
        }
        for status in [OpStatus::Settling, OpStatus::Locked] {
            assert!(!OpScope::Inclusion.covers(&op("quai", status)) && OpScope::Rest.covers(&op("quai", status)));
        }
        assert!(OpScope::All.covers(&op("qi", OpStatus::Submitted)) && OpScope::All.covers(&op("quai", OpStatus::Submitted)));
    }

    /// The token-transfer sync reads a bounded number of accounts per pass, but the window moves,
    /// so a wallet with more accounts than the budget sees all of them over successive passes
    /// rather than never seeing the ones past the eighth.
    #[test]
    fn the_transfer_sync_window_rotates_over_every_account() {
        let owners: Vec<usize> = (0..11).collect();
        let pass = |start: usize| {
            let covered = owners.len().min(TRANSFER_SYNC_ACCOUNTS);
            let read: Vec<usize> = owners.iter().cycle().skip(start % owners.len()).take(covered).copied().collect();
            (read, (start % owners.len() + covered) % owners.len())
        };
        let (first, next) = pass(0);
        assert_eq!(first, vec![0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(next, 8, "the next pass starts where this one stopped");
        let (second, next) = pass(next);
        assert_eq!(second, vec![8, 9, 10, 0, 1, 2, 3, 4], "it wraps rather than stopping at the end");
        // Two passes have covered every account.
        let mut seen: Vec<usize> = first.into_iter().chain(second).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen, owners);
        assert_eq!(next, 5);
        // A wallet inside the budget reads everything every time and the cursor returns to zero.
        let owners: Vec<usize> = (0..3).collect();
        let covered = owners.len().min(TRANSFER_SYNC_ACCOUNTS);
        assert_eq!(covered, 3);
        assert_eq!(covered % owners.len(), 0, "the cursor returns to the start");
    }

    /// Value and fee are stated in the ledger's own coin, and a fee is only called paid once it is:
    /// an account transaction's stored fee is its maximum until the receipt lands.
    #[test]
    fn an_operation_states_its_value_and_gas_in_its_own_coin() {
        let e18 = |n: u128| (n * 10u128.pow(18)).to_string();
        let mut op = Operation {
            id: "x".into(),
            network: "mainnet".into(),
            kind: "send_quai".into(),
            store: "quai".into(),
            account: "0x00".into(),
            status: OpStatus::Submitted,
            tx_hash: None,
            asset: "QUAI".into(),
            amount: e18(5),
            counterparty: "0x00".into(),
            fee: "21000000000000".into(),
            detail: serde_json::json!({}),
            created: 0,
            updated: 0,
        };
        let cost = op_cost(&op);
        assert_eq!((cost.value, cost.qi, cost.fee_final), (Some(U256::from(5u128 * 10u128.pow(18))), false, false));
        assert_eq!(cost.text(cost.fee.unwrap()), "0.000021 QUAI");
        op.detail["included_block"] = serde_json::json!(100);
        assert!(op_cost(&op).fee_final, "mined: the fee is the receipt's");
        // A token send carries no QUAI, but still paid gas in it.
        op.kind = "send_token".into();
        op.asset = "USDT".into();
        assert_eq!(op_cost(&op).value, Some(U256::ZERO));
        // Whatever the kind, a recorded native value wins (a swap from QUAI, a marketplace buy).
        op.kind = "swap".into();
        op.detail["native_value"] = serde_json::json!(e18(2));
        assert_eq!(op_cost(&op).value, Some(U256::from(2u128 * 10u128.pow(18))));
        op.detail = serde_json::json!({});
        assert_eq!(op_cost(&op).value, None, "an unknown value is not guessed as zero");
        // The UTXO ledger counts both in Qi, and its fee is fixed once signed.
        let qi =
            Operation { store: "qi".into(), kind: "send_qi".into(), asset: "QI".into(), amount: "12345".into(), fee: "5".into(), ..op };
        let cost = op_cost(&qi);
        assert!(cost.qi && cost.fee_final);
        assert_eq!((cost.text(cost.value.unwrap()), cost.text(cost.fee.unwrap())), ("12.345 Qi".to_string(), "0.005 Qi".to_string()));
    }

    #[test]
    fn describes_swaps_and_nfts() {
        let mut op = Operation {
            id: "x".into(),
            network: "mainnet".into(),
            kind: "swap".into(),
            store: "quai".into(),
            account: "0x00".into(),
            status: OpStatus::Submitted,
            tx_hash: None,
            asset: "WQI".into(),
            amount: (50u128 * 10u128.pow(18)).to_string(),
            counterparty: "0x00".into(),
            fee: String::new(),
            detail: serde_json::json!({"decimals": 18, "to_symbol": "USDT", "to_decimals": 6, "expected_out": "51940000"}),
            created: 0,
            updated: 0,
        };
        assert_eq!(describe(&op), "swap 50 WQI → ≈51.94 USDT");
        op.detail["actual_out"] = serde_json::json!("51900000");
        assert_eq!(describe(&op), "swap 50 WQI → 51.9 USDT");
        op.kind = "nft_buy".into();
        op.asset = "QUAI".into();
        op.amount = (1000u128 * 10u128.pow(18)).to_string();
        op.detail = serde_json::json!({"name": "Quai Pepe #212", "token_id": "212"});
        assert_eq!(describe(&op), "bought Quai Pepe #212 for 1000 QUAI");
    }

    #[test]
    fn durations() {
        assert_eq!(human_duration(45), "45s");
        assert_eq!(human_duration(600), "10m");
        assert_eq!(human_duration(3 * 3600 + 600), "3h 10m");
        assert_eq!(human_duration(241_920 * 5), "14d");
        assert_eq!(human_duration(3600), "1h");
        assert_eq!(human_duration(90_000), "1d 1h");
    }
}

#[cfg(test)]
mod notice_tests {
    use super::*;
    use crate::appdb::Activity;

    fn coin(tx: &str, index: u32, qits: u64, address: &str) -> Activity {
        Activity {
            network: "mainnet".into(),
            key: format!("qi:{tx}:{index}"),
            direction: "in".into(),
            asset: "QI".into(),
            amount: qits.to_string(),
            address: address.into(),
            tx_hash: Some(tx.into()),
            block: None,
            detail: serde_json::json!({}),
            observed: 0,
        }
    }

    /// One payment that arrived as three coins is one notification for the total, not three.
    #[test]
    fn coins_from_one_payment_are_one_notification() {
        let one = "0xaa";
        let notices = incoming_notices(&[coin(one, 0, 1000, "0x0011"), coin(one, 1, 1000, "0x0011"), coin(one, 2, 1000, "0x0011")]);
        assert_eq!(notices.len(), 1, "one payment, one notification: {notices:?}");
        let (title, text) = &notices[0];
        assert_eq!(*title, "Payment received");
        assert!(text.starts_with("3 Qi"), "the total, not one coin: {text}");
        assert!(text.contains("3 coins"), "how many coins it landed in is worth saying: {text}");
    }

    /// Two separate payments stay two notifications, whatever order they are seen in.
    #[test]
    fn separate_payments_stay_separate() {
        let notices =
            incoming_notices(&[coin("0xaa", 0, 1000, "0x0011"), coin("0xbb", 0, 5000, "0x0011"), coin("0xaa", 1, 1000, "0x0011")]);
        assert_eq!(notices.len(), 2);
        assert!(notices[0].1.starts_with("2 Qi"), "{:?}", notices[0]);
        assert!(notices[1].1.starts_with("5 Qi"), "{:?}", notices[1]);
    }

    /// A payment code derives a fresh address per output, so one payment can land on several.
    /// The total is what matters; no single address is picked to stand for the rest.
    #[test]
    fn one_payment_across_several_addresses_names_none_of_them() {
        let notices = incoming_notices(&[coin("0xaa", 0, 1000, "0x0011"), coin("0xaa", 1, 2000, "0x0022")]);
        assert_eq!(notices.len(), 1);
        let text = &notices[0].1;
        assert!(text.starts_with("3 Qi") && text.contains("2 of your addresses"), "{text}");
        assert!(!text.contains("0x0011"), "one address was made to stand for both: {text}");
    }

    /// A single coin reads exactly as it did before: no coin count, and the address named.
    #[test]
    fn a_single_coin_is_unchanged() {
        let notices = incoming_notices(&[coin("0xaa", 0, 1000, "0x0011")]);
        assert_eq!(notices.len(), 1);
        let text = &notices[0].1;
        assert!(text.starts_with("1 Qi") && !text.contains("coins"), "{text}");
    }

    /// QUAI balance increases have no transaction behind them, so they group per account and keep
    /// their own wording. Two accounts rising is two notifications, not one merged total.
    #[test]
    fn quai_balance_increases_group_by_account() {
        let rise = |address: &str, wei: &str| Activity {
            network: "mainnet".into(),
            key: format!("quai:{address}"),
            direction: "in".into(),
            asset: "QUAI".into(),
            amount: wei.into(),
            address: address.into(),
            tx_hash: None,
            block: None,
            detail: serde_json::json!({"note": "balance increase"}),
            observed: 0,
        };
        let notices = incoming_notices(&[rise("0x0011", "1000000000000000000"), rise("0x0022", "2000000000000000000")]);
        assert_eq!(notices.len(), 2, "separate accounts are separate news");
        assert_eq!(notices[0].0, "Balance rose");
        assert!(notices[0].1.contains("by 1 QUAI") && notices[0].1.contains("0x0011"), "{:?}", notices[0]);
        // Qi and QUAI are never folded together even if a hash somehow matched.
        assert_eq!(incoming_notices(&[coin("0xaa", 0, 1000, "0x0011"), rise("0x0011", "1")]).len(), 2);
    }
}

#[cfg(test)]
#[path = "track_lifecycle_tests.rs"]
mod lifecycle_tests;
