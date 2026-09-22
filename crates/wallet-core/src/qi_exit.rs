//! Imported-key Qi exits using the SDK's portable, exact-denomination planner.
//!
//! No HD/change origin is manufactured. The caller supplies one distinct destination per output;
//! fees are estimated on the actual no-change shape, then exact inputs are claimed before review.
use crate::error::{CoreError, Result};
use quai_sdk::consensus::{QiTransaction, SignedQiTransaction};
use quai_sdk::primitives::Hash32;
use quai_sdk::qi_preflight::{QiFeeMode, QiOperationIntent, QiPolicy, QiQuote, QiQuoteRequest, QiSource, quote_qi};
use quai_sdk::rpc::Transport;
use quai_sdk::wallet::SweepMode;
use quai_sdk::wallet::qi_keys::QiKeyResolver;
use quai_sdk::wallet::storage::{NetworkScope, PublicAddress, ReservationId, ReservationState, SqliteStore, StoreInstance};
use quai_sdk::{Provider, QiAddress, U256};

/// Frozen, claimed no-change transaction. Fields are private and it cannot be reconstructed from
/// untrusted JSON or cloned into another store session.
pub struct PreparedQiSweep {
    instance: StoreInstance,
    scope: NetworkScope,
    id: ReservationId,
    quote: QiQuote,
}

impl PreparedQiSweep {
    /// Exact reviewed transaction.
    pub fn transaction(&self) -> &QiTransaction {
        self.quote.transaction()
    }
    /// Exact planned fee, in qits (source values remain observations).
    pub fn fee(&self) -> U256 {
        self.quote.fee()
    }
    /// Immutable signing digest shown to the user.
    pub fn signing_digest(&self) -> Hash32 {
        self.quote.signing_digest()
    }
    /// Durable input claim belonging to this review.
    pub fn reservation_id(&self) -> ReservationId {
        self.id
    }
    /// Every output is a recipient output; there is no generated change.
    pub fn recipient_outputs(&self) -> usize {
        self.quote.recipient_outputs()
    }

    /// Verify the exact claim and every ordered local key, sign frozen bytes and commit them before
    /// exposing the signature. This deliberately mirrors native QiSession custody using public APIs.
    pub fn sign(&self, store: &mut SqliteStore, resolver: &impl QiKeyResolver) -> Result<SignedQiTransaction> {
        if self.instance != store.instance()
            || self.scope != store.scope()
            || store.reservation(self.id)?.is_none_or(|r| r.state != ReservationState::Reserved)
            || self.transaction().signing_digest()? != self.signing_digest()
        {
            return Err(CoreError::Rejected("Qi sweep no longer matches its prepared store or reservation".into()));
        }
        let mut expected: Vec<_> = self.transaction().inputs.iter().map(|input| input.previous_output).collect();
        let mut actual = store.reserved_outpoints(self.id)?;
        expected.sort_unstable();
        actual.sort_unstable();
        if expected != actual {
            return Err(CoreError::Rejected("Qi sweep input claim changed after review".into()));
        }
        let current = store.addresses()?;
        let mut keys = Vec::with_capacity(self.transaction().inputs.len());
        for input in &self.transaction().inputs {
            let owner = self
                .quote
                .selected_owners()
                .iter()
                .find(|owner| owner.address() == input.public_key.address())
                .ok_or_else(|| CoreError::Rejected("Qi sweep input has no verified owner".into()))?;
            if !current.contains(owner) {
                return Err(CoreError::Rejected("Qi sweep ownership metadata changed".into()));
            }
            let key = resolver.resolve(owner)?;
            if key.public_key() != input.public_key || key.public_key().to_compressed() != *owner.public_key() {
                return Err(CoreError::Rejected("Qi sweep signing key does not match the reviewed input".into()));
            }
            keys.push(key);
        }
        let references: Vec<_> = keys.iter().collect();
        let signed = self.transaction().sign_local(&references)?;
        store.commit_signed_qi(self.id, &signed)?;
        Ok(signed)
    }
}

fn source(store: &mut SqliteStore) -> Result<(QiSource, u64)> {
    let snapshot = store.snapshot()?;
    let checkpoint = snapshot.checkpoint.ok_or_else(|| CoreError::Invalid("refresh Qi outputs before planning an exit".into()))?;
    let owners: Vec<PublicAddress> = store.addresses()?;
    Ok((QiSource { scope: snapshot.scope, checkpoint, coins: snapshot.coins, owners }, snapshot.generation))
}

async fn quote_at<T: Transport>(
    provider: &Provider<T>,
    source: &QiSource,
    destinations: Vec<QiAddress>,
    policy: QiPolicy,
) -> Result<QiQuote> {
    quote_qi(
        provider,
        QiQuoteRequest {
            source,
            intent: QiOperationIntent::Sweep { destinations, mode: SweepMode::PreserveDenominations },
            policy,
            fees: QiFeeMode::Node,
            change: &[],
        },
    )
    .await
    .map_err(|e| CoreError::Invalid(format!("Qi sweep cannot be prepared: {e}")))
}

/// Read-only exact sweep/MAX quote. Never allocates addresses, claims inputs, signs or broadcasts.
/// Output capacity/input limits fail explicitly rather than silently omitting funds.
pub async fn quote_sweep<T: Transport>(
    provider: &Provider<T>,
    store: &mut SqliteStore,
    destinations: Vec<QiAddress>,
    policy: QiPolicy,
) -> Result<QiQuote> {
    let (source, _) = source(store)?;
    quote_at(provider, &source, destinations, policy).await
}

/// Maximum exact-qit specialized amount within the same input policy used by preparation.
/// Public derivations are advisory output capacity only: nothing is allocated or reserved here.
/// Descending candidates avoid assuming shape-dependent fees are monotonic. Resource/time
/// exhaustion returns an error rather than labelling a lower unproven candidate as MAX.
pub async fn quote_special_max<T: Transport>(
    provider: &Provider<T>,
    store: &mut SqliteStore,
    intent: quai_sdk::qi::QiSpecialIntent,
    policy: QiPolicy,
    change: &[PublicAddress],
) -> Result<(U256, QiQuote, usize, U256)> {
    use quai_sdk::qi_preflight::QiPreflightError;
    use quai_sdk::wallet::SelectionError;
    let (source, _) = source(store)?;
    let height = source.checkpoint.height.saturating_add(U256::from(1));
    let mut eligible: Vec<_> =
        source.coins.iter().filter(|c| !c.reserved && c.unlock_height <= height && c.expires_at.is_none_or(|end| height < end)).collect();
    eligible.sort_by_key(|coin| std::cmp::Reverse(coin.denomination.value()));
    let omitted = eligible.len().saturating_sub(policy.max_inputs);
    let omitted_qits: U256 = eligible.iter().skip(policy.max_inputs).map(|c| U256::from(c.denomination.value())).sum();
    let gross: U256 = eligible.iter().take(policy.max_inputs).map(|c| U256::from(c.denomination.value())).sum();
    let unit = U256::from(1);
    let mut amount = gross;
    let minimum = gross.saturating_sub(policy.max_fee);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    let before = tokio::time::timeout_at(deadline, provider.latest_header(source.scope.zone))
        .await
        .map_err(|_| CoreError::Network("Qi MAX head request exceeded its quote budget".into()))??
        .ok_or_else(|| CoreError::Network("missing Qi MAX head".into()))?;
    // A huge user cap must not turn a read-only fill into unbounded RPC work.
    for _ in 0..512 {
        if amount.is_zero() || amount < minimum {
            break;
        }
        let operation = match intent {
            quai_sdk::qi::QiSpecialIntent::Conversion(intent) => QiOperationIntent::Conversion { amount, intent },
            quai_sdk::qi::QiSpecialIntent::Wrapping(intent) => QiOperationIntent::Wrapping { amount, intent },
        };
        match tokio::time::timeout_at(
            deadline,
            quote_qi(
                provider,
                QiQuoteRequest {
                    source: &source,
                    intent: operation,
                    policy,
                    fees: QiFeeMode::Profile(quai_sdk::provider::QiFeeProfile::V056ShaAnchored),
                    change,
                },
            ),
        )
        .await
        .map_err(|_| CoreError::Network("Qi MAX exceeded its 20-second quote budget; use an explicit amount".into()))?
        {
            Ok(quote) => {
                let after = tokio::time::timeout_at(deadline, provider.latest_header(source.scope.zone))
                    .await
                    .map_err(|_| CoreError::Network("Qi MAX head verification exceeded its quote budget".into()))??
                    .ok_or_else(|| CoreError::Network("missing Qi MAX head".into()))?;
                if before.hash != after.hash || before.number != after.number {
                    return Err(CoreError::Network("Qi state changed during MAX search; retry the quote".into()));
                }
                return Ok((amount, quote, omitted, omitted_qits));
            }
            Err(QiPreflightError::Selection(
                SelectionError::InsufficientFunds | SelectionError::FeeBudgetExceeded | SelectionError::LimitExceeded,
            )) => amount = amount.saturating_sub(unit),
            Err(error) => return Err(CoreError::Invalid(format!("specialized Qi MAX cannot be quoted: {error}"))),
        }
    }
    Err(CoreError::Insufficient(
        "no exact-qit MAX candidate verified within the fee/resource/search bounds; use an explicit smaller amount".into(),
    ))
}

/// Quote against a captured snapshot and claim its exact inputs only after all network checks.
pub async fn prepare_sweep<T: Transport>(
    provider: &Provider<T>,
    store: &mut SqliteStore,
    id: ReservationId,
    destinations: Vec<QiAddress>,
    policy: QiPolicy,
) -> Result<PreparedQiSweep> {
    let (source, generation) = source(store)?;
    let quote = quote_at(provider, &source, destinations, policy).await?;
    let outpoints: Vec<_> = quote.selected_inputs().iter().map(|coin| coin.outpoint).collect();
    store.reserve_qi(id, generation, quote.candidate_height(), &outpoints)?;
    Ok(PreparedQiSweep { instance: store.instance(), scope: store.scope(), id, quote })
}

#[cfg(test)]
mod tests {
    use super::*;
    use quai_sdk::consensus::{Denomination, OutPoint};
    use quai_sdk::crypto::SecretKey;
    use quai_sdk::rpc::RpcError;
    use quai_sdk::wallet::CandidateCoin;
    use quai_sdk::wallet::discovery::Checkpoint;
    use quai_sdk::wallet::qi_keys::QiKeyring;
    use quai_sdk::{Endpoint, Routing, Zone};
    use serde_json::{Value, json};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    #[derive(Clone, Default)]
    struct Mock {
        fee_calls: Arc<AtomicUsize>,
        invalid_head: Arc<AtomicBool>,
    }
    fn hash(value: u8) -> Hash32 {
        Hash32::from_bytes([value; 32])
    }
    fn scope() -> NetworkScope {
        NetworkScope { chain_id: U256::from(15000), genesis: hash(1), zone: Zone::Cyprus1 }
    }
    impl Transport for Mock {
        async fn request(&self, _: &Endpoint, method: &str, params: Value) -> std::result::Result<Value, RpcError> {
            Ok(match method {
                "quai_chainId" => json!("0x3a98"),
                "quai_getHeaderByNumber" if params[0] == "0x0" => json!({"woHeader": {
                    "hash": hash(1).to_string(), "number":"0x0", "location":"0x", "parentHash":Hash32::ZERO.to_string()}}),
                "quai_getHeaderByNumber" => json!({"baseFeePerGas":"0x1", "gasLimit":"0x10000", "stateLimit":"0x10000", "woHeader": {
                    "hash":hash(if self.invalid_head.load(Ordering::SeqCst) { 9 } else { 2 }).to_string(),
                    "number":"0x10", "location":"0x0000", "primeTerminusNumber":"0x1b1598", "parentHash":hash(1).to_string()}}),
                "quai_getLatestUTXOSetSize" => json!("0x1"),
                "quai_quaiToQi" => {
                    self.fee_calls.fetch_add(1, Ordering::SeqCst);
                    let value = U256::from_str_radix(params[0].as_str().unwrap().trim_start_matches("0x"), 16).unwrap();
                    json!(format!("{:#x}", value / U256::from(1000)))
                }
                "quai_qiToQuai" => {
                    let value = U256::from_str_radix(params[0].as_str().unwrap().trim_start_matches("0x"), 16).unwrap();
                    json!(format!("{:#x}", value * U256::from(1000)))
                }
                "quai_estimateFeeForQi" => {
                    self.fee_calls.fetch_add(1, Ordering::SeqCst);
                    // A fee that depends on the actual output shape forces more than one round.
                    json!(if params[0]["txOut"].as_array().unwrap().len() > 1 { "0x7" } else { "0x5" })
                }
                _ => panic!("unexpected RPC in sweep fixture: {method}"),
            })
        }
    }
    fn key() -> SecretKey {
        let mut bytes = [0; 32];
        bytes[31] = 130;
        SecretKey::from_bytes(&bytes).unwrap()
    }
    fn destinations() -> Vec<QiAddress> {
        (1..=32).map(|n| format!("0x0080{n:036x}").parse().unwrap()).collect()
    }
    fn policy() -> QiPolicy {
        QiPolicy {
            initial_fee: U256::ZERO,
            max_fee: U256::from(20),
            max_inputs: 64,
            max_outputs: 256,
            max_fee_rounds: 12,
            max_snapshot_age: 10,
        }
    }
    fn setup() -> (tempfile::TempDir, SqliteStore, Provider<Mock>, Mock) {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SqliteStore::open(dir.path().join("qi.sqlite"), scope()).unwrap();
        let owner = PublicAddress::imported(&key().public_key()).unwrap();
        assert_eq!(owner.address().zone().unwrap(), Zone::Cyprus1);
        store.import_metadata(0, std::slice::from_ref(&owner)).unwrap();
        let mut snapshot = store.snapshot().unwrap();
        snapshot.checkpoint = Some(Checkpoint { hash: hash(2), height: U256::from(16) });
        let mut tx_hash = [0; 32];
        tx_hash[3] = 0x80;
        tx_hash[31] = 1;
        snapshot.coins = vec![CandidateCoin {
            outpoint: OutPoint { transaction_hash: Hash32::from_bytes(tx_hash), index: 0 },
            address: owner.address().try_into().unwrap(),
            denomination: Denomination::new(6).unwrap(),
            unlock_height: U256::ZERO,
            expires_at: None,
            reserved: false,
        }];
        store.replace_snapshot(&snapshot).unwrap();
        let mock = Mock::default();
        let provider =
            Provider::new(mock.clone(), Routing::direct("http://127.0.0.1:9200", Zone::Cyprus1.into()).unwrap(), scope().chain_id);
        (dir, store, provider, mock)
    }
    fn keyring() -> QiKeyring<'static> {
        let mut keys = QiKeyring::new(None).unwrap();
        keys.import(key()).unwrap();
        keys
    }

    #[tokio::test]
    async fn specialized_fractional_max_uses_actual_shape_fee_without_claims_or_allocations() {
        for wrapping in [false, true] {
            let (_dir, mut store, provider, mock) = setup();
            let mut snapshot = store.snapshot().unwrap();
            snapshot.coins[0].denomination = Denomination::new(7).unwrap(); // 5 Qi
            let mut another = snapshot.coins[0].clone();
            another.outpoint.index = 1;
            snapshot.coins.push(another);
            store.replace_snapshot(&snapshot).unwrap();
            let generation = store.snapshot().unwrap().generation;
            let owners = store.addresses().unwrap();
            let hd = quai_sdk::wallet::HdWallet::from_seed(&[7; 32], quai_sdk::wallet::CoinType::Qi).unwrap().account_public(0).unwrap();
            let mut change = Vec::new();
            let mut index = 0;
            for _ in 0..16 {
                let found = hd
                    .search(true, quai_sdk::wallet::Search { zone: Zone::Cyprus1, start_index: index, max_attempts: 100_000 }, || false)
                    .unwrap();
                index = found.address.index + 1;
                change.push(PublicAddress::derive(&hd, true, found.address.index).unwrap());
            }
            let destination = "0x0000000000000000000000000000000000000001".parse().unwrap();
            let intent = if wrapping {
                quai_sdk::qi::QiSpecialIntent::Wrapping(quai_sdk::consensus::QiWrappingIntent {
                    destination,
                    owner_contract: "0x002b2596EcF05C93a31ff916E8b456DF6C77c750".parse().unwrap(),
                })
            } else {
                quai_sdk::qi::QiSpecialIntent::Conversion(quai_sdk::consensus::QiConversionIntent {
                    destination,
                    refund: destinations()[0],
                    slippage: quai_sdk::consensus::ConversionSlippage::new(100).unwrap(),
                })
            };
            let mut bounds = policy();
            bounds.max_fee = U256::from(500);
            bounds.max_inputs = 1;
            let (amount, quote, omitted, omitted_qits) = quote_special_max(&provider, &mut store, intent, bounds, &change).await.unwrap();
            assert!(amount > U256::from(4000));
            assert_ne!(amount % U256::from(1000), U256::ZERO);
            assert!(amount + quote.fee() <= U256::from(5000));
            let larger = match intent {
                quai_sdk::qi::QiSpecialIntent::Conversion(intent) => {
                    QiOperationIntent::Conversion { amount: amount + U256::from(1), intent }
                }
                quai_sdk::qi::QiSpecialIntent::Wrapping(intent) => QiOperationIntent::Wrapping { amount: amount + U256::from(1), intent },
            };
            assert!(
                matches!(
                    quote_qi(
                        &provider,
                        QiQuoteRequest {
                            source: &source(&mut store).unwrap().0,
                            intent: larger,
                            policy: bounds,
                            fees: QiFeeMode::Profile(quai_sdk::provider::QiFeeProfile::V056ShaAnchored),
                            change: &change,
                        }
                    )
                    .await,
                    Err(quai_sdk::qi_preflight::QiPreflightError::Selection(_))
                ),
                "one extra qit cannot prepare at the sampled shape fee"
            );
            assert!(quote.fee() > U256::from(5), "uses specialized shape gas, not heuristic per-input fee");
            assert!(mock.fee_calls.load(Ordering::SeqCst) > 0);
            assert_eq!(omitted, 1);
            assert_eq!(omitted_qits, U256::from(5000));
            assert_eq!(quote.transaction().data.len(), if wrapping { 20 } else { 22 });
            assert_eq!(store.snapshot().unwrap().generation, generation);
            assert_eq!(store.addresses().unwrap(), owners);
            assert!(store.reservations(None, 10).unwrap().is_empty());
            mock.invalid_head.store(true, Ordering::SeqCst);
            assert!(quote_special_max(&provider, &mut store, intent, bounds, &change).await.is_err());
            assert_eq!(store.snapshot().unwrap().generation, generation);
        }
    }

    #[tokio::test]
    async fn imported_key_exit_quotes_claims_signs_and_recovers_exact_bytes_without_hd_change() {
        let (dir, mut store, provider, mock) = setup();
        assert!(store.addresses().unwrap().iter().all(|a| matches!(a.origin(), quai_sdk::wallet::storage::KeyOrigin::ImportedPublic)));
        let addresses = store.addresses().unwrap();
        let generation = store.snapshot().unwrap().generation;
        let quoted = quote_sweep(&provider, &mut store, destinations(), policy()).await.unwrap();
        assert_eq!(quoted.fee(), U256::from(7));
        assert!(mock.fee_calls.load(Ordering::SeqCst) >= 3, "fee converged using actual changed output shape");
        assert!(store.reservations(None, 10).unwrap().is_empty());
        assert_eq!(store.snapshot().unwrap().generation, generation);
        assert_eq!(store.addresses().unwrap(), addresses);
        let id = ReservationId([31; 16]);
        let prepared = prepare_sweep(&provider, &mut store, id, destinations(), policy()).await.unwrap();
        let received: U256 = prepared.transaction().outputs.iter().map(|o| U256::from(o.denomination.value())).sum();
        assert_eq!(received + prepared.fee(), U256::from(1000));
        assert_eq!(prepared.recipient_outputs(), prepared.transaction().outputs.len());
        assert_eq!(store.reservation(id).unwrap().unwrap().state, ReservationState::Reserved);
        let signed = prepared.sign(&mut store, &keyring()).unwrap();
        assert_eq!(signed.transaction().signing_digest().unwrap(), prepared.signing_digest());
        let bytes = signed.signed_bytes().unwrap();
        assert_eq!(store.reservation(id).unwrap().unwrap().state, ReservationState::Signed);
        assert!(store.release_unsigned(id).is_err());
        drop(store);
        let mut reopened = SqliteStore::open(dir.path().join("qi.sqlite"), scope()).unwrap();
        assert_eq!(reopened.signed_payload(id).unwrap().unwrap(), bytes);
        assert_eq!(SignedQiTransaction::decode(&bytes).unwrap().hash().unwrap(), signed.hash().unwrap());
        assert_eq!(reopened.addresses().unwrap(), addresses, "no manufactured HD/change origin");
    }

    #[tokio::test]
    async fn bad_capacity_fee_or_canonicality_never_claim_inputs() {
        for case in 0..3 {
            let (_dir, mut store, provider, mock) = setup();
            let mut policy = policy();
            let mut outputs = destinations();
            match case {
                0 => outputs.truncate(1),
                1 => policy.max_fee = U256::from(1),
                _ => mock.invalid_head.store(true, Ordering::SeqCst),
            }
            assert!(prepare_sweep(&provider, &mut store, ReservationId([32; 16]), outputs, policy).await.is_err());
            assert!(store.reservations(None, 10).unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn changed_claim_wrong_key_and_other_store_cannot_sign() {
        let (dir, mut store, provider, _) = setup();
        let id = ReservationId([33; 16]);
        let prepared = prepare_sweep(&provider, &mut store, id, destinations(), policy()).await.unwrap();
        let missing = QiKeyring::new(None).unwrap();
        assert!(prepared.sign(&mut store, &missing).is_err());
        struct WrongKey;
        impl QiKeyResolver for WrongKey {
            fn resolve(&self, _: &PublicAddress) -> std::result::Result<SecretKey, quai_sdk::wallet::storage::StorageError> {
                let mut bytes = [0; 32];
                bytes[31] = 1;
                Ok(SecretKey::from_bytes(&bytes).unwrap())
            }
        }
        assert!(prepared.sign(&mut store, &WrongKey).is_err(), "caller independently verifies resolver output");
        assert_eq!(store.reservation(id).unwrap().unwrap().state, ReservationState::Reserved);
        let mut other = SqliteStore::open(dir.path().join("qi.sqlite"), scope()).unwrap();
        assert!(prepared.sign(&mut other, &keyring()).is_err());
        store.release_unsigned(id).unwrap();
        assert!(prepared.sign(&mut store, &keyring()).is_err());
        assert!(store.signed_payload(id).unwrap().is_none());
    }
}
