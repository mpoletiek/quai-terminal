//! Faults exercise the production tracker interpretation and durable anchor transitions.
use super::*;
use crate::journal::OpKind;
use quai_sdk::provider::{
    AddressOutpoint, ConversionObservation, ConversionOriginObservation, ConversionSpendability, EtxExecutionObservation, EtxScanResult,
    Extensions, OutPoint, QiCreditObservation, Receipt, ScanCoverage, Transaction,
};
use quai_sdk::rpc::{Endpoint, RpcError};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

fn hash(n: u8) -> Hash32 {
    Hash32::from_bytes([n; 32])
}
fn block(number: u64, h: u8) -> BlockReference {
    BlockReference { number, hash: hash(h) }
}
fn header(number: u64, h: u8) -> Value {
    json!({"gasLimit":"0x10000", "stateLimit":"0x10000", "woHeader": {
        "hash":hash(h).to_string(), "parentHash":hash(0).to_string(), "number":format!("0x{number:x}"),
        "primeTerminusNumber":"0x4", "location":"0x0000" }})
}
#[derive(Clone)]
struct Mock(Arc<Mutex<VecDeque<(&'static str, Value)>>>);
impl Transport for Mock {
    async fn request(&self, _: &Endpoint, method: &str, _: Value) -> std::result::Result<Value, RpcError> {
        if method == "quai_chainId" {
            return Ok(json!("0x3a98"));
        }
        let (expected, value) = self.0.lock().unwrap().pop_front().expect("unexpected extra RPC");
        assert_eq!(method, expected);
        Ok(value)
    }
}
fn provider(replies: Vec<(&'static str, Value)>) -> Provider<Mock> {
    Provider::new(
        Mock(Arc::new(Mutex::new(replies.into()))),
        quai_sdk::Routing::direct("http://127.0.0.1:9200", ZONE.into()).unwrap(),
        U256::from(15000),
    )
}
fn op(status: OpStatus) -> Operation {
    Operation {
        id: "11000000000000000000000000000000".into(),
        network: "fixture".into(),
        kind: OpKind::ConvertQuaiToQi,
        store: "quai".into(),
        account: "fixture".into(),
        status,
        tx_hash: Some(hash(3).to_string()),
        asset: "QUAI".into(),
        amount: "1".into(),
        counterparty: String::new(),
        fee: "21".into(),
        created: 1,
        updated: 1,
        detail: json!({"included_block":10, "included_hash":hash(1).to_string(), "canonical_tx":hash(3).to_string(),
            "execution_block":20, "execution_hash":hash(2).to_string(), "execution_tx":hash(4).to_string(),
            "credited_qits":"1000", "unlock_height":30, "actual_out":"1000", "scan_next":21,
            "scan_last_number":20, "scan_last_hash":hash(2).to_string(), "finality":"unverified"})
        .into(),
    }
}
fn db() -> (tempfile::TempDir, crate::appdb::AppDb) {
    let dir = tempfile::tempdir().unwrap();
    let db = crate::appdb::AppDb::open(&dir.path().join("app.sqlite")).unwrap();
    (dir, db)
}

#[tokio::test]
async fn successful_failed_and_refunded_source_reorgs_reset_dependent_cursor_and_credit() {
    for status in [OpStatus::Confirmed, OpStatus::Failed, OpStatus::Settled, OpStatus::Refunded, OpStatus::Locked] {
        let (dir, db) = db();
        let original = op(status);
        db.insert_operation(&original).unwrap();
        let source = provider(vec![("quai_getHeaderByNumber", header(10, 9))]);
        let mut report = TrackReport::default();
        assert!(recheck_operation_anchors(&source, &db, &original, &mut report).await.unwrap());
        let reopened = crate::appdb::AppDb::open(&dir.path().join("app.sqlite")).unwrap();
        let changed = reopened.operation(&original.id).unwrap().unwrap();
        assert_eq!(changed.status, OpStatus::Submitted);
        for field in [
            "included_hash",
            "canonical_tx",
            "actual_out",
            "execution_hash",
            "credited_qits",
            "scan_next",
            "scan_last_hash",
            "unlock_height",
        ] {
            assert!(changed.detail.test_get(field).is_null(), "{status:?} kept stale {field}");
        }
        assert_eq!(*changed.detail.finality(), "unverified");
        assert_eq!(report.changes.len(), 1);
        // An unanchored operation does not rediscover the same old reorg on a second pass.
        assert!(!recheck_operation_anchors(&provider(vec![]), &reopened, &changed, &mut report).await.unwrap());
        assert_eq!(report.changes.len(), 1);
    }
}

#[tokio::test]
async fn destination_reorg_keeps_source_and_missing_header_is_only_uncertainty() {
    let (_dir, db) = db();
    let original = op(OpStatus::Settled);
    db.insert_operation(&original).unwrap();
    let mut report = TrackReport::default();
    let unavailable = provider(vec![("quai_getHeaderByNumber", Value::Null)]);
    assert!(recheck_operation_anchors(&unavailable, &db, &original, &mut report).await.unwrap());
    let historical = db.operation(&original.id).unwrap().unwrap();
    assert_eq!(historical.status, OpStatus::Settled);
    assert_eq!(*historical.detail.credited_qits(), "1000");
    assert_eq!(*historical.detail.source_canonicality(), "unverified");
    assert!(report.changes.is_empty());
    let fork = provider(vec![("quai_getHeaderByNumber", header(10, 1)), ("quai_getHeaderByNumber", header(20, 9))]);
    assert!(recheck_operation_anchors(&fork, &db, &historical, &mut report).await.unwrap());
    let changed = db.operation(&original.id).unwrap().unwrap();
    assert_eq!(changed.status, OpStatus::Settling);
    assert_eq!(*changed.detail.included_hash(), hash(1).to_string());
    assert!(changed.detail.execution_hash().is_null());
    assert!(changed.detail.credited_qits().is_null());
}

#[tokio::test]
async fn origin_is_rechecked_after_destination_and_reincluded_anchor_remains_provisional() {
    let (_dir, db) = db();
    let original = op(OpStatus::Settled);
    db.insert_operation(&original).unwrap();
    let mut report = TrackReport::default();
    let raced = provider(vec![
        ("quai_getHeaderByNumber", header(10, 1)),
        ("quai_getHeaderByNumber", header(20, 2)),
        ("quai_getHeaderByNumber", header(10, 9)),
    ]);
    assert!(recheck_operation_anchors(&raced, &db, &original, &mut report).await.unwrap());
    let reincluded =
        crate::journal::Detail::from(json!({"included_block":12,"included_hash":hash(8).to_string(),"canonical_tx":hash(3).to_string()}));
    db.transition_operation(&original.id, OpStatus::Submitted, OpStatus::Confirmed, None, None, Some(&reincluded)).unwrap();
    let current = db.operation(&original.id).unwrap().unwrap();
    let same = provider(vec![("quai_getHeaderByNumber", header(12, 8))]);
    assert!(!recheck_operation_anchors(&same, &db, &current, &mut report).await.unwrap());
    assert_eq!(*current.detail.finality(), "unverified");
}

fn receipt(h: u8, status: u8) -> Value {
    json!({"transactionHash":hash(3).to_string(), "blockHash":hash(h).to_string(), "blockNumber":"0xa", "transactionIndex":"0x0",
        "type":"0x0", "from":"0x0000000000000000000000000000000000000001", "gasUsed":"0x5208", "cumulativeGasUsed":"0x5208",
        "effectiveGasPrice":"0x1", "status":format!("0x{status:x}"), "logsBloom":format!("0x{}","00".repeat(10240)), "logs":[]})
}
/// A log as a node returns it, for the receipt `onto`: the SDK's `Log` is `#[non_exhaustive]`, and
/// `Log::try_from` builds one from its JSON.
fn log_in(onto: &quai_sdk::provider::Receipt, address: &str, topics: Vec<Hash32>, data: Vec<u8>) -> quai_sdk::provider::Log {
    let mut log = quai_sdk::provider::Log::try_from(json!({"address": address, "topics": [], "data": "0x", "blockNumber": "0xa",
        "blockHash": hash(1).to_string(), "transactionHash": onto.transaction_hash.to_string(), "transactionIndex": "0x0",
        "logIndex": "0x0", "removed": false}))
    .unwrap();
    log.topics = topics;
    log.data = quai_sdk::provider::RpcData::new(data).unwrap();
    log.transaction_hash = onto.transaction_hash;
    log.inclusion = onto.inclusion;
    log
}

#[tokio::test]
async fn fee_and_output_receipt_cannot_change_after_candidate_observation() {
    for (r, headers, expected) in [
        (receipt(1, 1), vec![header(10, 1)], true),
        (receipt(9, 1), vec![], false),
        (receipt(1, 0), vec![], false),
        (Value::Null, vec![], false),
        (receipt(1, 1), vec![header(10, 9)], false),
    ] {
        let mut replies = vec![("quai_getTransactionReceipt", r)];
        replies.extend(headers.into_iter().map(|h| ("quai_getHeaderByNumber", h)));
        assert_eq!(canonical_receipt(&provider(replies), hash(3), block(10, 1), ReceiptOutcome::Succeeded).await.is_ok(), expected);
    }
}

fn credit() -> QiCreditObservation {
    QiCreditObservation::new(
        "0x0080000000000000000000000000000000000001".parse().unwrap(),
        hash(4),
        hash(4),
        block(20, 2),
        block(25, 5),
        vec![AddressOutpoint::new(OutPoint { tx_hash: hash(4), index: 0 }, 6, U256::from(30), Extensions::default())],
        U256::from(1000),
        U256::ZERO,
        U256::ZERO,
    )
}
#[test]
fn partial_delayed_and_previously_spent_outputs_never_become_complete_credit() {
    let mut c = credit();
    assert_eq!(credit_status(&c).0, OpStatus::Locked);
    c.unobserved_qits = U256::from(2000);
    let (status, message, patch) = credit_status(&c);
    assert_eq!(status, OpStatus::Settling);
    assert!(message.contains("spent"));
    assert_eq!(patch.observed_credit_qits(), "1000");
    assert!(patch.credited_qits().is_null(), "partial current data must not replace earlier historical total");
    c.outputs.clear();
    c.locked_qits = U256::ZERO;
    assert_eq!(credit_status(&c).0, OpStatus::Settling);
    c = credit();
    c.head = block(31, 6);
    c.locked_qits = U256::ZERO;
    c.unlocked_qits = U256::from(1000);
    let (status, _, patch) = credit_status(&c);
    assert_eq!(status, OpStatus::Settled);
    assert_eq!(patch.spendability(), "indexed_unlocked_at_observed_head");
}

#[test]
fn account_conversion_effects_do_not_invent_operation_specific_maturity() {
    let mut op = op(OpStatus::Settling);
    op.kind = crate::journal::OpKind::ConvertQiToQuai;
    for effect in [
        ConversionEffect::ConversionReported,
        ConversionEffect::ReceiptUnavailable { etx_type: 2 },
        ConversionEffect::UnknownSubtype(99),
        ConversionEffect::LegacyOutcome { etx_type: 2 },
        ConversionEffect::Locked { etx_type: 2 },
    ] {
        let conversion =
            ConversionObservation::new(ConversionOriginObservation::Unavailable, None, Some(effect), ConversionSpendability::Unverified);
        let (status, message, patch) =
            settle_result(&op, SettlementEvidence { conversion: Some(&conversion), external: None, qi_credit: None }).unwrap().unwrap();
        assert_ne!(status, OpStatus::Settled);
        assert!(!message.contains("two weeks"));
        assert_eq!(patch.unwrap().spendability(), "unverified");
    }
}

/// A scanned destination execution with inclusion and a succeeded receipt.
fn executed_destination() -> EtxScanResult {
    let transaction = json!({"type":"0x1", "hash":hash(4).to_string(), "blockHash":hash(2).to_string(),
        "blockNumber":"0x14", "transactionIndex":"0x0", "input":"0x", "gas":"0x0", "nonce":"0x0",
        "from":"0x0000000000000000000000000000000000000001", "to":"0x0000000000000000000000000000000000000002",
        "value":"0x1", "originatingTxHash":hash(3).to_string(), "etxIndex":"0x0", "etxType":"0x2"});
    EtxScanResult::new(
        ScanCoverage::Complete,
        Some(block(25, 5)),
        1,
        Some(EtxExecutionObservation::new(Transaction::try_from(transaction).unwrap(), Some(Receipt::try_from(receipt(2, 1)).unwrap()))),
    )
}

/// Qi-to-Quai has no attributed credit observation to wait for, so a reported conversion with an
/// observed destination stops waiting — but its proceeds sit in the protocol's conversion lockup,
/// so it rests at `Locked`, never at `Settled`, which would promise a spendable credit.
#[test]
fn a_reported_account_conversion_rests_locked_without_claiming_a_spendable_credit() {
    let mut op = op(OpStatus::Settling);
    op.kind = crate::journal::OpKind::ConvertQiToQuai;
    let conversion = ConversionObservation::new(
        ConversionOriginObservation::Unavailable,
        Some(executed_destination()),
        Some(ConversionEffect::ConversionReported),
        ConversionSpendability::Unverified,
    );
    let (status, message, patch) =
        settle_result(&op, SettlementEvidence { conversion: Some(&conversion), external: None, qi_credit: None }).unwrap().unwrap();
    let patch = patch.unwrap();
    assert_eq!(status, OpStatus::Locked);
    assert_ne!(status, OpStatus::Settled, "the proceeds are not spendable for the length of the lockup");
    assert_eq!(patch.account_credit(), "reported_by_destination_receipt");
    assert_eq!(patch.spendability(), "held_by_conversion_lockup");
    assert_eq!(patch.quai_lock(), true);
    assert_eq!(patch.finality(), "unverified");
    assert_eq!(patch.destination_receipt(), "succeeded");
    assert_eq!(patch.destination_canonicality(), "observed");
    assert_eq!(patch.execution_block(), 20);
    assert!(patch.unlock_height().is_null(), "no maturity is attributable to this operation");
    assert!(message.contains("conversion lockup"));

    // A locked destination receipt reaches the same resting state by the older path.
    let mut locked = conversion.clone();
    locked.effect = Some(ConversionEffect::Locked { etx_type: 2 });
    let (status, _, patch) =
        settle_result(&op, SettlementEvidence { conversion: Some(&locked), external: None, qi_credit: None }).unwrap().unwrap();
    assert_eq!(status, OpStatus::Locked);
    assert_eq!(patch.unwrap().account_credit(), "unverified");

    // The forward direction does have an attributed observation, so it still waits for one.
    op.kind = crate::journal::OpKind::ConvertQuaiToQi;
    let (status, _, _) =
        settle_result(&op, SettlementEvidence { conversion: Some(&conversion), external: None, qi_credit: None }).unwrap().unwrap();
    assert_eq!(status, OpStatus::Settling);
}

#[tokio::test]
async fn legacy_destination_height_without_hash_rescans_without_inventing_finality() {
    let (_dir, db) = db();
    let mut old = op(OpStatus::Settled);
    old.detail.set_execution_hash(Value::Null);
    db.insert_operation(&old).unwrap();
    let mut report = TrackReport::default();
    let source = provider(vec![("quai_getHeaderByNumber", header(10, 1))]);
    assert!(recheck_operation_anchors(&source, &db, &old, &mut report).await.unwrap());
    let current = db.operation(&old.id).unwrap().unwrap();
    assert_eq!(current.status, OpStatus::Settling);
    assert_eq!(current.detail.legacy_destination_observation()["credited_qits"], "1000");
    assert!(current.detail.credited_qits().is_null());
    assert!(current.detail.scan_next().is_null());
    assert_eq!(*current.detail.spendability(), "unverified");
    assert_eq!(*current.detail.finality(), "unverified");
}

#[test]
fn swap_receipt_output_requires_exact_token_recipient_and_router_withdrawal() {
    let mut r = quai_sdk::provider::Receipt::try_from(receipt(1, 1)).unwrap();
    let recipient = "0x0000000000000000000000000000000000000001";
    let token = "0x0000000000000000000000000000000000000002";
    let router = "0x0000000000000000000000000000000000000003";
    let topic = |address: &str| format!("0x{:0>64}", address.trim_start_matches("0x")).parse::<Hash32>().unwrap();
    let mut log = log_in(
        &r,
        token,
        vec![TRANSFER_TOPIC.parse().unwrap(), topic(router), topic(recipient)],
        U256::from(1000).to_be_bytes::<32>().to_vec(),
    );
    r.logs = vec![log.clone()];
    let detail = crate::journal::Detail::from(json!({"recipient":recipient,"to_token":token,"router":router}));
    assert_eq!(swap_output(&r, &detail, None), Some(U256::from(1000)));
    let mut fee = log.clone();
    fee.topics = vec![TRANSFER_TOPIC.parse().unwrap(), topic(recipient), topic(router)];
    fee.data = quai_sdk::provider::RpcData::new(U256::from(25).to_be_bytes::<32>().to_vec()).unwrap();
    r.logs.push(fee.clone());
    assert_eq!(swap_output(&r, &detail, None), Some(U256::from(975)), "recipient-side transfer fees must reduce onward amount");
    fee.data = quai_sdk::provider::RpcData::new(U256::from(1001).to_be_bytes::<32>().to_vec()).unwrap();
    r.logs[1] = fee;
    assert_eq!(swap_output(&r, &detail, None), None, "negative net credit is not spendable output");
    r.logs = vec![log.clone()];

    r.logs[0].topics[2] = topic(router);
    assert_eq!(swap_output(&r, &detail, None), None);
    r.logs[0] = log.clone();
    r.logs[0].data = quai_sdk::provider::RpcData::new(vec![0; 33]).unwrap();
    assert_eq!(swap_output(&r, &detail, None), None);
    r.logs[0] = log.clone();
    r.outcome = ReceiptOutcome::Failed;
    assert_eq!(swap_output(&r, &detail, None), None);
    r.outcome = ReceiptOutcome::Succeeded;
    r.to = Some(router.parse().unwrap());
    log.topics = vec![WITHDRAWAL_TOPIC.parse().unwrap(), topic(router)];
    r.logs = vec![log.clone()];
    let native = crate::journal::Detail::from(json!({"recipient":recipient,"to_token":"quai","router":router}));
    assert_eq!(swap_output(&r, &native, Some(token)), Some(U256::from(1000)));
    r.logs[0].topics[1] = topic(recipient);
    assert_eq!(swap_output(&r, &native, Some(token)), None);
    r.logs[0] = log;
    r.to = Some(recipient.parse().unwrap());
    assert_eq!(swap_output(&r, &native, Some(token)), None);
}

#[test]
fn failed_redemption_records_partial_credit_without_success_or_loss_claims() {
    let mut op = op(OpStatus::Settling);
    op.kind = crate::journal::OpKind::UnwrapWqi;
    let mut c = credit();
    c.unobserved_qits = U256::from(500);
    let external =
        quai_sdk::provider::ExternalObservation::new(ConversionOriginObservation::Unavailable, None, Some(ReceiptOutcome::Failed));
    let (status, message, patch) =
        settle_result(&op, SettlementEvidence { conversion: None, external: Some(&external), qi_credit: Some(&c) }).unwrap().unwrap();
    assert_eq!(status, OpStatus::Failed);
    assert!(message.contains("partial"));
    let patch = patch.unwrap();
    assert_eq!(patch.observed_credit_qits(), "1000");
    assert_eq!(patch.unobserved_qits(), "500");
    assert_eq!(patch.credit_partial(), true);
    assert_eq!(patch.spendability(), "unverified");
}

#[test]
fn hartii_native_fill_requires_the_verified_curve_and_exact_event_recipient() {
    let mut receipt = quai_sdk::provider::Receipt::try_from(receipt(1, 1)).unwrap();
    let owner = "0x0000000000000000000000000000000000000001";
    let curve = "0x0000000000000000000000000000000000000002";
    let topic = |address: &str| format!("0x{:0>64}", address.trim_start_matches("0x")).parse::<Hash32>().unwrap();
    receipt.to = Some(curve.parse().unwrap());
    let data: Vec<u8> = [100u64, 500, 1].into_iter().flat_map(|n| U256::from(n).to_be_bytes::<32>()).collect();
    let log = log_in(
        &receipt,
        curve,
        vec!["0x846c37eef631e0943682d87352ec117c20008eb7f425c9b85ac011a6d4774cc0".parse().unwrap(), topic(owner)],
        data,
    );
    receipt.logs = vec![log.clone()];
    let mut operation = op(OpStatus::Confirmed);
    operation.kind = crate::journal::OpKind::HartiiSell;
    operation.account = owner.into();
    operation.detail = json!({"curve":curve,"recipient":owner}).into();
    assert_eq!(hartii_fill(&receipt, &operation), Some((U256::from(100), U256::from(500), U256::from(1))));
    receipt.logs.push(log.clone());
    assert!(hartii_fill(&receipt, &operation).is_none(), "duplicate fills are ambiguous");
    receipt.logs = vec![log.clone()];
    receipt.logs[0].topics[1] = topic(curve);
    assert!(hartii_fill(&receipt, &operation).is_none());
    receipt.logs = vec![log];
    receipt.to = Some(owner.parse().unwrap());
    assert!(hartii_fill(&receipt, &operation).is_none());
}
