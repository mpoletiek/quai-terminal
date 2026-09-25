//! Local reconciliation of SDK custody records with the application journal.
//! Signed bytes remain authoritative; recovery never submits or signs.

use crate::appdb::{OpStatus, Operation};
use crate::error::{CoreError, Result};
use crate::journal::OpKind;
use crate::registry::now;
use crate::session::{Session, op_hex};
use quai_sdk::consensus::{SignedQiOperation, SignedQuaiTransaction};
use quai_sdk::wallet::storage::{ReservationId, ReservationState};
use std::fs::{File, OpenOptions, TryLockError};

impl Session {
    /// Try to own an operation mutation. The stable lock file is never unlinked.
    pub(crate) fn try_operation_lock(&self, id: ReservationId) -> Result<Option<File>> {
        let dir = self.registry.paths().network_dir(&self.meta.id, &self.network.id).join("operation-locks");
        crate::paths::ensure_private_dir(&dir)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(dir.join(op_hex(id)))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(file)),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(e)) => Err(e.into()),
        }
    }

    pub(crate) fn operation_lock(&self, id: ReservationId) -> Result<File> {
        self.try_operation_lock(id)?
            .ok_or_else(|| CoreError::Invalid("operation is active in another session; retry after it finishes".into()))
    }

    /// Repair durable signed candidates and expose orphan unsigned claims without
    /// reclaiming them. A live preparation holds its operation lock until journaled.
    /// This method performs no network requests and is safe while the wallet is locked.
    pub fn reconcile_custody(&mut self) -> Result<usize> {
        let mut repaired = 0;
        for qi in [false, true] {
            let mut after = None;
            loop {
                let records = if qi { &self.qi_store } else { &self.quai_store }.reservations(after, 250)?;
                if records.is_empty() {
                    break;
                }
                after = records.last().map(|r| r.id);
                for record in records {
                    if record.state == ReservationState::Released {
                        continue;
                    }
                    let Some(_guard) = self.try_operation_lock(record.id)? else { continue };
                    let id = op_hex(record.id);
                    let existing = self.app.operation(&id)?;
                    if existing.is_none() && record.state == ReservationState::Confirmed {
                        continue;
                    }
                    let store = if qi { &mut self.qi_store } else { &mut self.quai_store };
                    // Re-read under the operation lock: the page may predate a commit.
                    let Some(record) = store.reservation(record.id)? else { continue };
                    if record.state == ReservationState::Released {
                        continue;
                    }
                    let signed = store.signed_payload(record.id)?;
                    let mut hashes = Vec::new();
                    if let Some(bytes) = &signed {
                        let hash =
                            if qi { SignedQiOperation::decode(bytes)?.hash()? } else { SignedQuaiTransaction::decode(bytes)?.hash()? };
                        hashes.push(hash.to_string());
                        for candidate in store.replacement_candidates(record.id)? {
                            let hash = if qi {
                                SignedQiOperation::decode(&candidate.payload)?.hash()?
                            } else {
                                SignedQuaiTransaction::decode(&candidate.payload)?.hash()?
                            };
                            hashes.push(hash.to_string());
                        }
                    }
                    let owner = store.reserved_nonce(record.id)?.map(|(owner, _)| owner.to_string()).unwrap_or_else(|| "qi".into());
                    let latest = hashes.last().cloned();
                    let mut op = existing.clone().unwrap_or_else(|| Operation {
                        id: id.clone(),
                        network: self.network.id.clone(),
                        kind: OpKind::Recovered,
                        store: if qi { "qi" } else { "quai" }.into(),
                        account: owner,
                        status: OpStatus::Prepared,
                        tx_hash: None,
                        asset: if qi { "QI" } else { "QUAI" }.into(),
                        amount: "0".into(),
                        counterparty: String::new(),
                        fee: "0".into(),
                        detail: serde_json::json!({"recovered": true, "amount_unknown": true}).into(),
                        created: now(),
                        updated: now(),
                    });
                    if existing.is_none() {
                        // Preserve the signed account call identity even if its UI review was lost.
                        if !qi && let Some(bytes) = &signed {
                            let tx = SignedQuaiTransaction::decode(bytes)?;
                            op.account = tx.from().to_string();
                            op.amount = tx.transaction().value.to_string();
                            op.counterparty = tx.transaction().to.map(|a| a.to_string()).unwrap_or_default();
                            op.detail.set_native_value(serde_json::json!(op.amount));
                        }
                        self.app.insert_operation(&op)?;
                        repaired += 1;
                    }
                    if let Some(hash) = latest {
                        let patch = crate::journal::Detail::from(serde_json::json!({
                            "original_tx": hashes.first(), "candidates": hashes,
                            "recovered_signed": true
                        }));
                        let status = if op.status == OpStatus::Prepared { OpStatus::Signed } else { op.status };
                        let preserve_inclusion = status.is_terminal() || matches!(status, OpStatus::Settling | OpStatus::Locked);
                        let hash_update = (!preserve_inclusion).then_some(hash.as_str());
                        if (!preserve_inclusion && op.tx_hash.as_ref() != Some(&hash))
                            || op.status != status
                            || op.detail.candidates() != patch.candidates()
                        {
                            self.app.transition_operation(&id, op.status, status, hash_update, None, Some(&patch))?;
                            repaired += 1;
                        }
                    }
                }
            }
        }
        Ok(repaired)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::AppConfig, network::NetworkProfile, paths::Paths, registry::Registry};
    use quai_sdk::{U256, consensus::QuaiTransaction, signer::Signer};

    fn fixture() -> (tempfile::TempDir, Session) {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::fast(Paths::resolve(Some(dir.path().into())).unwrap());
        let meta = reg
            .create_hd(
                "recovery",
                "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
                "english",
                "",
                "password123",
                true,
            )
            .unwrap();
        let mut session = Session::open(reg, AppConfig::default(), meta, NetworkProfile::builtins().remove(0)).unwrap();
        session.unlock("password123").unwrap();
        (dir, session)
    }

    fn reopen(s: &Session) -> Session {
        Session::open(s.registry.clone(), s.config.clone(), s.meta.clone(), s.network.clone()).unwrap()
    }

    #[test]
    fn signing_before_journal_update_recovers_after_restart() {
        let (_dir, mut s) = fixture();
        let id = ReservationId([42; 16]);
        let account = s.meta.quai_accounts[0].clone();
        let address = account.address.parse().unwrap();
        s.quai_store.reserve_nonce(id, address, 0).unwrap();
        let op = s.new_op(id, OpKind::SendQuai, "quai", &account.address, "QUAI", U256::from(1), &account.address, serde_json::json!({}));
        s.app.insert_operation(&op).unwrap();
        let signer = s.keys().unwrap().quai_signer(address.into(), account.hd_index, s.network.chain_id).unwrap();
        let signed = signer
            .sign_quai(&QuaiTransaction {
                chain_id: U256::from(s.network.chain_id),
                nonce: 0,
                to: Some(address.into()),
                value: U256::from(1),
                gas_limit: 21_000,
                gas_price: U256::from(1),
                data: vec![],
                access_list: vec![],
            })
            .unwrap();
        s.quai_store.commit_signed_quai(id, &signed).unwrap();
        let mut r = reopen(&s);
        let restored = r.app.operation(&op.id).unwrap().unwrap();
        assert_eq!(restored.status, OpStatus::Signed);
        assert_eq!(restored.tx_hash, Some(signed.hash().unwrap().to_string()));
        assert_eq!(r.reconcile_custody().unwrap(), 0);
        assert_eq!(r.quai_store.signed_payload(id).unwrap(), Some(signed.signed_bytes().unwrap()));
    }

    #[test]
    fn replacement_signed_before_app_update_recovers_even_after_original_is_confirmed() {
        let (_dir, mut s) = fixture();
        let id = ReservationId([46; 16]);
        let account = s.meta.quai_accounts[0].clone();
        let address = account.address.parse().unwrap();
        s.quai_store.reserve_nonce(id, address, 0).unwrap();
        let signer = s.keys().unwrap().quai_signer(address.into(), account.hd_index, s.network.chain_id).unwrap();
        let tx = QuaiTransaction {
            chain_id: U256::from(s.network.chain_id),
            nonce: 0,
            to: Some(address.into()),
            value: U256::from(1),
            gas_limit: 21000,
            gas_price: U256::from(1),
            data: vec![],
            access_list: vec![],
        };
        let signed = signer.sign_quai(&tx).unwrap();
        s.quai_store.commit_signed_quai(id, &signed).unwrap();
        let mut replacement_tx = tx.clone();
        replacement_tx.gas_price = U256::from(2);
        let replacement = signer.sign_quai(&replacement_tx).unwrap();
        s.quai_store.commit_quai_replacement(id, signed.hash().unwrap(), &replacement).unwrap();
        let mut op=s.new_op(id,OpKind::SendQuai,"quai",&account.address,"QUAI",U256::from(1),&account.address,serde_json::json!({
            "canonical_tx":signed.hash().unwrap().to_string(),"included_block":10,"included_hash":"fixture-anchor","actual_out":"receipt-credit"}));
        op.status = OpStatus::Confirmed;
        op.tx_hash = Some(signed.hash().unwrap().to_string());
        op.fee = "actual-fee".into();
        s.app.insert_operation(&op).unwrap();
        let mut restarted = reopen(&s);
        let recovered = restarted.app.operation(&op.id).unwrap().unwrap();
        assert_eq!(recovered.status, OpStatus::Confirmed);
        assert_eq!(recovered.tx_hash, op.tx_hash);
        assert_eq!(recovered.fee, "actual-fee");
        assert_eq!(*recovered.detail.actual_out(), "receipt-credit");
        assert_eq!(
            *recovered.detail.candidates(),
            serde_json::json!([signed.hash().unwrap().to_string(), replacement.hash().unwrap().to_string()])
        );
        assert_eq!(restarted.reconcile_custody().unwrap(), 0, "second restart/recovery is idempotent");
        assert_eq!(restarted.quai_store.replacement_candidates(id).unwrap()[0].payload, replacement.signed_bytes().unwrap());
    }

    #[test]
    fn orphan_claim_is_visible_but_not_automatically_released() {
        let (_dir, mut s) = fixture();
        let id = ReservationId([43; 16]);
        let address = s.meta.quai_accounts[0].address.parse().unwrap();
        s.quai_store.reserve_nonce(id, address, 0).unwrap();
        let mut r = reopen(&s);
        assert_eq!(r.app.operation(&op_hex(id)).unwrap().unwrap().status, OpStatus::Prepared);
        assert_eq!(r.quai_store.reservation(id).unwrap().unwrap().state, ReservationState::Reserved);
        r.abandon(&op_hex(id)).unwrap();
        assert_eq!(r.quai_store.reservation(id).unwrap().unwrap().state, ReservationState::Released);
    }

    #[test]
    fn recovery_does_not_publish_another_live_preparation() {
        let (_dir, mut s) = fixture();
        let id = ReservationId([44; 16]);
        let guard = s.operation_lock(id).unwrap();
        s.quai_store.reserve_nonce(id, s.meta.quai_accounts[0].address.parse().unwrap(), 0).unwrap();
        let mut r = reopen(&s);
        assert!(r.app.operation(&op_hex(id)).unwrap().is_none());
        drop(guard);
        assert_eq!(r.reconcile_custody().unwrap(), 1);
    }

    #[test]
    fn old_pending_operation_is_not_hidden_by_history_limit() {
        let (_dir, s) = fixture();
        let mut op = s.new_op(
            ReservationId([45; 16]),
            OpKind::SendQuai,
            "quai",
            &s.meta.quai_accounts[0].address,
            "QUAI",
            U256::from(1),
            "",
            serde_json::json!({}),
        );
        op.created = 1;
        s.app.insert_operation(&op).unwrap();
        for n in 0u32..10_001 {
            let mut history = op.clone();
            history.id = format!("{n:032x}");
            history.created = 2;
            history.status = OpStatus::Confirmed;
            s.app.insert_operation(&history).unwrap();
        }
        let pending = s.app.open_operations(&s.network.id).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, op.id);
        assert_eq!(pending[0].status, OpStatus::Prepared);
    }
}
