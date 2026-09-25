//! Owner-scoped pending financial commitments shared by all sessions under one data root.
use crate::appdb::{AppDb, Operation};
use crate::error::{CoreError, Result};
use crate::journal::{Detail, OpKind};
use crate::session::Session;
use crate::tx::FinancialEffect;
use quai_sdk::{BlockTag, QuaiAddress, U256};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions, TryLockError};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct Commitments {
    pub native_value: String,
    pub fee: String,
    pub tokens: BTreeMap<String, String>,
    #[serde(default)]
    pub unknown_assets: bool,
}
fn atoms(value: &str) -> Result<U256> {
    U256::from_str_radix(value, 10).map_err(|_| CoreError::Storage("invalid pending commitment amount; reconcile operations first".into()))
}
fn add(a: U256, b: U256) -> Result<U256> {
    a.checked_add(b).ok_or_else(|| CoreError::Insufficient("pending commitments exceed supported amount range".into()))
}
impl Commitments {
    pub(crate) fn from_intent(kind: &OpKind, amount: &str, native: U256, fee: U256, detail: &Detail) -> Result<Self> {
        let mut out = Self { native_value: native.to_string(), fee: fee.to_string(), ..Self::default() };
        if *kind == OpKind::Approve || matches!(kind, OpKind::Other(name) if name.starts_with("approve_")) {
            return Ok(out);
        }
        let effects = detail.financial_effects();
        if !effects.is_null() {
            for effect in serde_json::from_value::<Vec<FinancialEffect>>(effects.clone())? {
                if effect.direction != "out" || effect.token.eq_ignore_ascii_case("quai") || effect.token.is_empty() {
                    continue;
                }
                let token: QuaiAddress =
                    effect.token.parse().map_err(|_| CoreError::Invalid("invalid token in financial effect".into()))?;
                let key = token.to_string().to_lowercase();
                let current = out.tokens.get(&key).map(|v| atoms(v)).transpose()?.unwrap_or(U256::ZERO);
                out.tokens.insert(key, add(current, atoms(&effect.amount)?)?.to_string());
            }
            return Ok(out);
        }
        let field = match kind {
            OpKind::SendToken | OpKind::CurveSell => Some(detail.token()),
            OpKind::Swap | OpKind::SwapExactOutput => Some(detail.from_token()),
            OpKind::Stake | OpKind::RemoveLiquidity => Some(detail.pair()),
            OpKind::Incentivize => Some(detail.reward()),
            OpKind::UnwrapWqi | OpKind::UnwrapQuai => Some(detail.contract()),
            _ => None,
        };
        if let Some(field) = field {
            match field.as_str() {
                Some(token) if token.eq_ignore_ascii_case("quai") => (),
                Some(token) => {
                    let token: QuaiAddress = token.parse().map_err(|_| CoreError::Storage("invalid pending debit token".into()))?;
                    let debit =
                        if *kind == OpKind::UnwrapWqi { quai_sdk::wrappers::qits_to_wqi_atoms(atoms(amount)?)? } else { atoms(amount)? };
                    out.tokens.insert(token.to_string().to_lowercase(), debit.to_string());
                }
                None => out.unknown_assets = true,
            }
        }
        if matches!(kind, OpKind::ContractCall | OpKind::Recovered) {
            out.unknown_assets = true;
        }
        Ok(out)
    }
    pub(crate) fn from_operation(op: &Operation) -> Result<Self> {
        let saved = op.detail.commitments();
        let mut value = if !saved.is_null() {
            serde_json::from_value(saved.clone())?
        } else {
            Self::from_intent(&op.kind, &op.amount, atoms(op.detail.native_value().as_str().unwrap_or("0"))?, atoms(&op.fee)?, &op.detail)?
        };
        // Fee replacement candidates are mutually exclusive. One family reserves the largest fee.
        value.fee = atoms(&value.fee)?.max(atoms(&op.fee)?).to_string();
        Ok(value)
    }
    pub(crate) fn native(&self) -> Result<U256> {
        add(atoms(&self.native_value)?, atoms(&self.fee)?)
    }
}

impl Session {
    /// Nonblocking kernel lock: never hold a synchronous waiting lock across an async runtime.
    pub(crate) fn commitment_lock(&self, owner: &str) -> Result<File> {
        let owner: QuaiAddress = owner.parse().map_err(|_| CoreError::Invalid("invalid commitment owner".into()))?;
        let scope = self.network.scope()?;
        let directory = self.registry.paths().root().join("commitment-locks");
        crate::paths::ensure_private_dir(&directory)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(directory.join(format!("{}-{}-{}", scope.chain_id, scope.genesis, owner.to_string().to_lowercase())))?;
        match file.try_lock() {
            Ok(()) => Ok(file),
            Err(TryLockError::WouldBlock) => {
                Err(CoreError::Invalid("another session is preparing this owner's funds; retry after it finishes".into()))
            }
            Err(TryLockError::Error(error)) => Err(error.into()),
        }
    }

    /// Called while holding commitment_lock; excludes this operation when replacing or committing.
    #[cfg(test)]
    pub(crate) fn pending_commitments(&self, owner: &str, own_id: &str, wanted: &Commitments) -> Result<(U256, BTreeMap<String, U256>)> {
        self.pending_commitments_at_nonce(owner, own_id, wanted, None)
    }

    fn pending_commitments_at_nonce(
        &self,
        owner: &str,
        own_id: &str,
        wanted: &Commitments,
        confirmed_nonce: Option<u64>,
    ) -> Result<(U256, BTreeMap<String, U256>)> {
        let address: QuaiAddress = owner.parse().map_err(|_| CoreError::Invalid("invalid commitment owner".into()))?;
        let mut native = wanted.native()?;
        let mut tokens: BTreeMap<String, U256> = wanted.tokens.iter().map(|(k, v)| Ok((k.clone(), atoms(v)?))).collect::<Result<_>>()?;
        let mut aliases: Vec<_> = self
            .config
            .networks()
            .into_iter()
            .filter(|n| n.chain_id == self.network.chain_id && n.genesis.eq_ignore_ascii_case(&self.network.genesis))
            .map(|n| n.id)
            .collect();
        if !aliases.contains(&self.network.id) {
            aliases.push(self.network.id.clone());
        }
        aliases.sort();
        aliases.dedup();
        for wallet in self.registry.list()? {
            let path = self.registry.paths().wallet_dir(&wallet.id).join("app.sqlite");
            if !path.is_file() {
                continue;
            }
            let db = AppDb::open(&path)?;
            for network in &aliases {
                let mut nonces = BTreeMap::new();
                let custody = self.registry.paths().network_dir(&wallet.id, network).join("quai.sqlite");
                if custody.is_file() {
                    let store = quai_sdk::wallet::storage::SqliteStore::open(&custody, self.network.scope()?)?;
                    let mut after = None;
                    loop {
                        let page = store.reservations(after, 250)?;
                        if page.is_empty() {
                            break;
                        }
                        after = page.last().map(|record| record.id);
                        for record in page {
                            use quai_sdk::wallet::storage::ReservationState;
                            if let Some((account, nonce)) = store.reserved_nonce(record.id)?
                                && account == address
                            {
                                nonces.insert(crate::session::op_hex(record.id), nonce);
                            }
                            if matches!(record.state, ReservationState::Released | ReservationState::Confirmed) {
                                continue;
                            }
                            let id = crate::session::op_hex(record.id);
                            if wallet.id == self.meta.id && id == own_id {
                                continue;
                            }
                            if store.reserved_nonce(record.id)?.is_some_and(|(account, _)| account == address)
                                && db.operation(&id)?.is_none()
                            {
                                return Err(CoreError::Rejected("an active SDK nonce claim has no financial journal; reconcile that wallet before preparing another spend".into()));
                            }
                        }
                    }
                }
                for op in db.open_operations(network)? {
                    if op.store != "quai" || !op.account.eq_ignore_ascii_case(owner) || (wallet.id == self.meta.id && op.id == own_id) {
                        continue;
                    }
                    // Only a same-block observed consumed nonce proves this debit is already
                    // reflected by the observed balance; cached receipt status is insufficient.
                    let nonce = op.detail.nonce().as_u64().or_else(|| nonces.get(&op.id).copied());
                    if nonce.zip(confirmed_nonce).is_some_and(|(nonce, confirmed)| nonce < confirmed) {
                        continue;
                    }
                    if wallet.id != self.meta.id || network != &self.network.id {
                        return Err(CoreError::Rejected("this owner has an open operation in another wallet or network alias; resolve it before using a separate nonce store".into()));
                    }
                    let other = Commitments::from_operation(&op)?;
                    if wanted.unknown_assets || other.unknown_assets {
                        return Err(CoreError::Rejected(
                            "an open operation has unknown asset debits; reconcile it before preparing another spend".into(),
                        ));
                    }
                    native = add(native, other.native()?)?;
                    for (token, value) in other.tokens {
                        let total = add(tokens.get(&token).copied().unwrap_or(U256::ZERO), atoms(&value)?)?;
                        tokens.insert(token, total);
                    }
                }
            }
        }
        Ok((native, tokens))
    }

    /// Refuse a spend the owner's balances cannot cover together with every open operation.
    ///
    /// The nonce and the balances are read at one block, since only a nonce consumed there says
    /// an operation's debit is already out of the balance read there. On a network whose headers
    /// this wallet can hash, that block is the node's state anchor (shared, and usually kept warm)
    /// and the account and WQUAI balance are proven in one round trip; any other token is read with
    /// a plain call at the same block, alongside the proof.
    pub(crate) async fn check_commitments(&self, owner: &str, own_id: &str, wanted: &Commitments) -> Result<()> {
        let address: QuaiAddress = owner.parse().map_err(|_| CoreError::Invalid("invalid commitment owner".into()))?;
        let started = std::time::Instant::now();
        let seen = self.observe_holdings(address, &self.likely_tokens(owner, wanted)?).await?;
        let (native, tokens) = self.pending_commitments_at_nonce(owner, own_id, wanted, Some(seen.nonce))?;
        if seen.native < native {
            return Err(CoreError::Insufficient(format!(
                "QUAI balance cannot cover this spend plus open commitments (requires {native} base units including fees)"
            )));
        }
        let mut recheck = seen.recheck;
        for (token, needed) in tokens {
            let balance = match seen.tokens.get(&token) {
                Some(balance) => *balance,
                // An operation journaled between the two looks: rare enough to read on its own.
                None => {
                    recheck = true;
                    let block = BlockTag::Number(U256::from(seen.block));
                    self.held_tokens_at(address, &[&token], block).await?.remove(&token).unwrap_or_default()
                }
            };
            if balance < needed {
                return Err(CoreError::Insufficient(format!(
                    "token {token} balance cannot cover this spend plus open commitments (requires {needed} base units)"
                )));
            }
        }
        // A proof is bound to its block; a plain call by number is not, until the block is shown
        // to still be the chain's.
        if recheck
            && self.node.provider.header_at(crate::network::ZONE, seen.block).await?.is_none_or(|now| now.hash.to_string() != seen.hash)
        {
            return Err(CoreError::Rejected("commitment observation changed; refresh and review again".into()));
        }
        crate::diag::timing(if seen.proven { "review.commitments.proven" } else { "review.commitments" }, started);
        Ok(())
    }

    /// The tokens a check will probably need: this spend's, and those of the owner's open
    /// operations here. They are read beside the nonce, so the balances do not wait on it.
    fn likely_tokens(&self, owner: &str, wanted: &Commitments) -> Result<BTreeSet<String>> {
        let mut tokens: BTreeSet<String> = wanted.tokens.keys().cloned().collect();
        for op in self.app.open_operations(&self.network.id)? {
            if op.store == "quai"
                && op.account.eq_ignore_ascii_case(owner)
                && let Ok(other) = Commitments::from_operation(&op)
            {
                tokens.extend(other.tokens.into_keys());
            }
        }
        Ok(tokens)
    }

    /// The owner's nonce, QUAI balance and `tokens` balances at one block.
    async fn observe_holdings(&self, owner: QuaiAddress, tokens: &BTreeSet<String>) -> Result<Holdings> {
        let what = "your balance";
        if self.network.proves_state()
            && let Some(anchored) = crate::anchor::review_anchor(&self.node, &self.network, what).await?
        {
            let block = anchored.anchor.block;
            let wquai = self.network.wquai.as_deref().map(str::to_lowercase).filter(|w| tokens.contains(w));
            let wquai_slot = [crate::anchor::mapping_field_slot(owner, WQUAI_BALANCE_SLOT, 0)];
            let mut targets: Vec<(QuaiAddress, &[quai_sdk::primitives::Hash32])> = vec![(owner, &[])];
            if let Some(wquai) = &wquai {
                targets.push((wquai.parse().map_err(|_| CoreError::Invalid("WQUAI address is malformed".into()))?, &wquai_slot));
            }
            let others: Vec<&String> = tokens.iter().filter(|t| Some(*t) != wquai.as_ref()).collect();
            let (proven, called) = tokio::join!(
                crate::anchor::prove_at(&self.node, &self.network, &anchored, &targets, what),
                self.held_tokens_at(owner, &others, BlockTag::Number(U256::from(block.number)))
            );
            let (proven, mut held) = (proven?, called?);
            if let Some(wquai) = wquai {
                held.insert(wquai, proven[1].storage_value(wquai_slot[0]).unwrap_or_default());
            }
            return Ok(Holdings {
                block: block.number,
                hash: block.hash.to_string(),
                nonce: proven[0].nonce(),
                native: proven[0].balance(),
                tokens: held,
                recheck: !others.is_empty(),
                proven: true,
            });
        }
        let head = self
            .node
            .provider
            .latest_header(crate::network::ZONE)
            .await?
            .ok_or_else(|| CoreError::Network("no commitment observation header".into()))?;
        let block = BlockTag::Number(U256::from(head.number));
        let all: Vec<&String> = tokens.iter().collect();
        let (nonce, native, held) = tokio::join!(
            self.node.provider.transaction_count(owner, block),
            self.node.provider.balance(owner, block),
            self.held_tokens_at(owner, &all, block)
        );
        Ok(Holdings {
            block: head.number,
            hash: head.hash.to_string(),
            nonce: nonce?,
            native: native?,
            tokens: held?,
            recheck: true,
            proven: false,
        })
    }

    /// `owner`'s balance of each token at `block`, read together.
    async fn held_tokens_at(&self, owner: QuaiAddress, tokens: &[&String], block: BlockTag) -> Result<BTreeMap<String, U256>> {
        let reads = tokens.iter().map(|token| async move {
            let address = token.parse().map_err(|_| CoreError::Storage("invalid commitment token".into()))?;
            let contract = quai_sdk::contracts::Erc20::new(address, &self.node.provider)?;
            Ok::<_, CoreError>(((*token).clone(), contract.balance_of(owner, owner, block).await?))
        });
        Ok(futures::future::try_join_all(reads).await?.into_iter().collect())
    }
}

/// WQUAI (WETH9) keeps `balanceOf` in the mapping at slot 3.
const WQUAI_BALANCE_SLOT: u64 = 3;

/// What the owner held at one block.
struct Holdings {
    block: u64,
    hash: String,
    /// Transactions the chain has counted from this owner at `block`.
    nonce: u64,
    native: U256,
    tokens: BTreeMap<String, U256>,
    /// Some balance came from a plain call by block number, which a reorganization could have
    /// answered from another block.
    recheck: bool,
    /// The nonce and QUAI balance were proven at a state anchor.
    proven: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appdb::OpStatus;
    use quai_sdk::wallet::storage::ReservationId;
    use serde_json::json;
    const TOKEN: &str = "0x0000000000000000000000000000000000000001";
    fn fixture() -> (tempfile::TempDir, Session) {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::paths::Paths::resolve(Some(dir.path().to_path_buf())).unwrap();
        let registry = crate::registry::Registry::new(paths);
        let meta = registry
            .create_hd(
                "commitments",
                "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
                "english",
                "",
                "password123",
                true,
            )
            .unwrap();
        let config = crate::config::AppConfig::default();
        let network = config.network("orchard").unwrap();
        (dir, Session::open(registry, config, meta, network).unwrap())
    }
    #[test]
    fn legacy_wqi_redemption_commitment_uses_token_atoms_not_qits() {
        let token = "0x00000000000000000000000000000000000000ab";
        let legacy = Commitments::from_intent(
            &crate::journal::OpKind::UnwrapWqi,
            "2000",
            U256::ZERO,
            U256::ZERO,
            &crate::journal::Detail::from(json!({"contract":token})),
        )
        .unwrap();
        assert_eq!(legacy.tokens[token], "2000000000000000000");
        let effects = crate::journal::Detail::from(
            json!({"financial_effects":[{"direction":"out","asset":"WQI","token":token,"decimals":18,"amount":"2000000000000000000"}]}),
        );
        assert_eq!(
            Commitments::from_intent(&crate::journal::OpKind::UnwrapWqi, "2000", U256::ZERO, U256::ZERO, &effects).unwrap().tokens,
            legacy.tokens
        );
    }

    #[test]
    fn exact_token_debits_native_value_and_replacement_fee_are_not_approvals_or_double_counted() {
        let detail = crate::journal::Detail::from(json!({"financial_effects":[
            {"direction":"out","asset":"QUAI","token":"quai","decimals":18,"amount":"100"},
            {"direction":"out","asset":"T","token":TOKEN,"decimals":6,"amount":"50"},
            {"direction":"out","asset":"T","token":TOKEN,"decimals":6,"amount":"20"},
            {"direction":"in","asset":"T","token":TOKEN,"decimals":6,"amount":"999"}]}));
        let effects =
            Commitments::from_intent(&crate::journal::OpKind::SwapExactOutput, "70", U256::from(100), U256::from(5), &detail).unwrap();
        assert_eq!(effects.native().unwrap(), U256::from(105));
        assert_eq!(effects.tokens[TOKEN], "70");
        let approval =
            Commitments::from_intent(&crate::journal::OpKind::Approve, &U256::MAX.to_string(), U256::ZERO, U256::from(3), &detail).unwrap();
        assert!(approval.tokens.is_empty());
        assert_eq!(approval.native().unwrap(), U256::from(3));
        let (_dir, session) = fixture();
        let owner = &session.meta.quai_accounts[0].address;
        let mut op = session.new_op(
            ReservationId([1; 16]),
            OpKind::SwapExactOutput,
            "quai",
            owner,
            "T",
            U256::from(70),
            TOKEN,
            json!({"commitments":effects}),
        );
        op.fee = "9".into();
        let family = Commitments::from_operation(&op).unwrap();
        assert_eq!(family.native().unwrap(), U256::from(109));
        assert_eq!(family.tokens[TOKEN], "70");
    }
    #[test]
    fn reopened_journal_sums_unknown_and_prepared_claims_and_releases_only_terminal_rows() {
        let (_dir, session) = fixture();
        let owner = session.meta.quai_accounts[0].address.clone();
        for (n, status) in [(1, OpStatus::Prepared), (2, OpStatus::Unknown), (3, OpStatus::Cancelled), (4, OpStatus::Confirmed)] {
            let mut op = session.new_op(
                ReservationId([n; 16]),
                OpKind::SendToken,
                "quai",
                &owner,
                "T",
                U256::from(60),
                TOKEN,
                json!({"token":TOKEN,"native_value":"0"}),
            );
            op.fee = "7".into();
            op.status = status;
            session.journal(op).unwrap();
        }
        let wanted = Commitments::from_intent(
            &crate::journal::OpKind::SendToken,
            "50",
            U256::ZERO,
            U256::from(3),
            &crate::journal::Detail::from(json!({"token":TOKEN})),
        )
        .unwrap();
        let second =
            Session::open(session.registry.clone(), session.config.clone(), session.meta.clone(), session.network.clone()).unwrap();
        let (native, tokens) = second.pending_commitments(&owner, "new", &wanted).unwrap();
        assert_eq!(native, U256::from(17));
        assert_eq!(tokens[TOKEN], U256::from(170));
        let (native, tokens) = second.pending_commitments(&owner, &crate::session::op_hex(ReservationId([1; 16])), &wanted).unwrap();
        assert_eq!(native, U256::from(10));
        assert_eq!(tokens[TOKEN], U256::from(110));
        let lock = session.commitment_lock(&owner).unwrap();
        assert!(second.commitment_lock(&owner).is_err(), "separate handles contend on the kernel lock");
        drop(lock);
        assert!(second.commitment_lock(&owner).is_ok());
    }
    #[test]
    fn unknown_contract_debits_block_other_prepared_spends() {
        let (_dir, session) = fixture();
        let owner = &session.meta.quai_accounts[0].address;
        let mut op = session.new_op(
            ReservationId([9; 16]),
            OpKind::ContractCall,
            "quai",
            owner,
            "QUAI",
            U256::ZERO,
            TOKEN,
            json!({"native_value":"0"}),
        );
        op.fee = "5".into();
        session.journal(op).unwrap();
        let wanted = Commitments::from_intent(
            &crate::journal::OpKind::SendQuai,
            "1",
            U256::from(1),
            U256::from(1),
            &crate::journal::Detail::from(json!({})),
        )
        .unwrap();
        assert!(session.pending_commitments(owner, "new", &wanted).unwrap_err().to_string().contains("unknown asset debits"));
    }

    #[test]
    fn orphan_nonce_blocks_and_explicit_unsigned_release_restores_availability() {
        let (_dir, mut session) = fixture();
        let owner = session.meta.quai_accounts[0].address.clone();
        let id = ReservationId([50; 16]);
        session.quai_store.reserve_nonce(id, owner.parse().unwrap(), 0).unwrap();
        let wanted = Commitments::from_intent(
            &crate::journal::OpKind::SendQuai,
            "1",
            U256::from(1),
            U256::from(1),
            &crate::journal::Detail::from(json!({})),
        )
        .unwrap();
        assert!(session.pending_commitments(&owner, "new", &wanted).unwrap_err().to_string().contains("no financial journal"));
        session.quai_store.release_unsigned(id).unwrap();
        assert_eq!(session.pending_commitments(&owner, "new", &wanted).unwrap().0, U256::from(2));
    }

    /// A session on mainnet whose node is the anchor tests' mock, serving a captured block.
    async fn proving() -> (tempfile::TempDir, Session, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use crate::anchor::tests::{Kind, network, serve};
        let (dir, orchard) = fixture();
        let (url, log) = serve(Kind::Honest).await;
        let session = Session::open(orchard.registry.clone(), orchard.config.clone(), orchard.meta.clone(), network(&url)).unwrap();
        (dir, session, log)
    }
    fn journal_spend(session: &Session, n: u8, native: &str, nonce: u64) {
        use crate::anchor::tests::PROVEN_OWNER;
        let mut op = session.new_op(
            ReservationId([n; 16]),
            OpKind::SendQuai,
            "quai",
            PROVEN_OWNER,
            "QUAI",
            U256::from_str_radix(native, 10).unwrap(),
            TOKEN,
            json!({"native_value": native, "nonce": nonce}),
        );
        op.fee = "0".into();
        session.journal(op).unwrap();
    }
    fn methods(log: &std::sync::Mutex<Vec<String>>) -> Vec<String> {
        let calls = |r: &String| match serde_json::from_str::<serde_json::Value>(r).unwrap() {
            serde_json::Value::Array(batch) => batch,
            call => vec![call],
        };
        log.lock().unwrap().iter().flat_map(calls).map(|c| c["method"].as_str().unwrap_or_default().to_string()).collect()
    }

    /// The owner's nonce and balance come from one proof at the anchor, and the proven nonce says
    /// which journaled spends the proven balance already paid for.
    #[tokio::test]
    async fn a_review_proves_the_owner_nonce_and_balance_at_the_anchor() {
        use crate::anchor::tests::PROVEN_OWNER;
        let (_dir, session, log) = proving().await;
        let quai = U256::from(10u64).pow(U256::from(18));
        let wanted =
            Commitments::from_intent(&crate::journal::OpKind::SendQuai, "1", quai, U256::ZERO, &crate::journal::Detail::from(json!({})))
                .unwrap();
        session.check_commitments(PROVEN_OWNER, "new", &wanted).await.unwrap();
        let asked = methods(&log);
        assert!(asked.contains(&"quai_getProof".to_string()), "{asked:?}");
        for plain in ["quai_getBalance", "quai_getTransactionCount", "quai_call"] {
            assert!(!asked.contains(&plain.to_string()), "{plain} was read without a proof: {asked:?}");
        }
        // The block proves 406 transactions sent: nonce 405 is spent and its debit is already out
        // of the proven balance, so it commits nothing more.
        journal_spend(&session, 1, "1000000000000000000000000", 405);
        session.check_commitments(PROVEN_OWNER, "new", &wanted).await.unwrap();
        // Nonce 406 is still open, and the ~30,567 QUAI proven cannot cover it.
        journal_spend(&session, 2, "1000000000000000000000000", 406);
        let refused = session.check_commitments(PROVEN_OWNER, "new", &wanted).await.unwrap_err();
        assert!(matches!(refused, CoreError::Insufficient(_)), "{refused:?}");
    }

    /// A token with no known layout is read with a plain call at the proven block, beside the
    /// proof, and the block is then shown to still be the chain's.
    #[tokio::test]
    async fn a_token_balance_is_read_at_the_proven_block_and_that_block_rechecked() {
        use crate::anchor::tests::{PROVEN_OWNER, TOKEN_BALANCE};
        let (_dir, session, log) = proving().await;
        let spend = |atoms: u64| {
            Commitments::from_intent(
                &crate::journal::OpKind::SendToken,
                &atoms.to_string(),
                U256::ZERO,
                U256::ZERO,
                &crate::journal::Detail::from(json!({"token": TOKEN})),
            )
            .unwrap()
        };
        session.check_commitments(PROVEN_OWNER, "new", &spend(TOKEN_BALANCE)).await.unwrap();
        let block = crate::anchor::tests::captured()["header"]["woHeader"]["number"].as_str().unwrap().to_string();
        let calls: Vec<String> = log.lock().unwrap().iter().filter(|r| r.contains("quai_call")).cloned().collect();
        assert!(!calls.is_empty() && calls.iter().all(|c| c.contains(&block)), "balanceOf read at {block}: {calls:?}");
        let last = log.lock().unwrap().last().cloned().unwrap();
        assert!(last.contains("quai_getHeaderByNumber") && last.contains(&block), "then the block rechecked: {last}");
        let refused = session.check_commitments(PROVEN_OWNER, "new", &spend(TOKEN_BALANCE + 1)).await.unwrap_err();
        assert!(matches!(refused, CoreError::Insufficient(text) if text.contains("token")));
    }

    /// Against mainnet, from a watch-only wallet (the check reads; it never signs): a funded
    /// account's proven nonce, QUAI, WQUAI and USDT at the anchor equal the node's plain answers at
    /// the same block; a spend it covers passes and one it cannot is refused. `QW_FUNDS_ACCOUNT`
    /// picks the account (else a recent curve trader); `QW_MONITOR_RPC` proves through a monitor
    /// with the RPC as witness.
    #[tokio::test]
    #[ignore = "network"]
    async fn live_the_funds_check_is_proven_on_mainnet() {
        use crate::network::MonitorEndpoint;
        let config = crate::config::AppConfig::default();
        let mut network = config.network("mainnet").unwrap();
        if let Ok(url) = std::env::var("QW_MONITOR_RPC") {
            network.monitor = Some(MonitorEndpoint { rpc_url: url, use_pathing: false });
        }
        let probe = network.node().unwrap();
        let owner = match std::env::var("QW_FUNDS_ACCOUNT") {
            Ok(a) => a.to_lowercase(),
            Err(_) => {
                let policy = crate::config::DataPolicy { explorer: true, market: true, images: false, icons: false };
                let ctx = crate::data::DataCtx::with_app(AppDb::memory().unwrap(), network.clone(), policy).unwrap();
                let trades = crate::launches::curve_trades(&ctx, 200).await.unwrap();
                let mut found = None;
                for trader in trades.iter().map(|t| t.trader.to_lowercase()) {
                    let Ok(a) = trader.parse::<QuaiAddress>() else { continue };
                    let code = probe.provider.code(a, BlockTag::Latest).await.unwrap_or_default();
                    if code.bytes().is_empty() && probe.provider.balance(a, BlockTag::Latest).await.unwrap_or_default() > U256::ZERO {
                        found = Some(trader);
                        break;
                    }
                }
                found.expect("a funded trader")
            }
        };
        let dir = tempfile::tempdir().unwrap();
        let registry = crate::registry::Registry::new(crate::paths::Paths::resolve(Some(dir.path().to_path_buf())).unwrap());
        let meta = registry.create_watch("funds", &[(owner.clone(), "Main".into())]).unwrap();
        let mut session = Session::open(registry, config, meta, network.clone()).unwrap();
        if network.monitor.is_some() {
            assert!(session.use_monitor().await.is_none(), "the monitor is used");
        }
        let address: QuaiAddress = owner.parse().unwrap();
        let wquai = network.wquai.clone().unwrap().to_lowercase();
        let usdt = network.ecosystem.usdt.clone().unwrap().address.to_lowercase();
        let tokens: BTreeSet<String> = [wquai.clone(), usdt.clone()].into();

        let started = std::time::Instant::now();
        let seen = session.observe_holdings(address, &tokens).await.unwrap();
        let took = started.elapsed();
        assert!(seen.proven, "mainnet proves the funds check");
        let at = BlockTag::Number(U256::from(seen.block));
        let token = |t: &str| quai_sdk::contracts::Erc20::new(t.parse().unwrap(), &session.node.provider).unwrap();
        let (nonce, native, wq, us) = tokio::join!(
            session.node.provider.transaction_count(address, at),
            session.node.provider.balance(address, at),
            async { token(&wquai).balance_of(address, address, at).await },
            async { token(&usdt).balance_of(address, address, at).await },
        );
        eprintln!(
            "FUNDS {owner} at #{}: nonce {} QUAI {} WQUAI {} USDT {} ({} ms, {})",
            seen.block,
            seen.nonce,
            seen.native,
            seen.tokens[&wquai],
            seen.tokens[&usdt],
            took.as_millis(),
            session.node.anchor_confirmation().map_or("no anchor", |c| c.text())
        );
        assert_eq!(seen.nonce, nonce.unwrap(), "proven nonce = the node's");
        assert_eq!(seen.native, native.unwrap(), "proven QUAI = the node's");
        assert_eq!(seen.tokens[&wquai], wq.unwrap(), "proven WQUAI (slot 3) = balanceOf");
        assert_eq!(seen.tokens[&usdt], us.unwrap(), "USDT by call at the anchor's block");

        let spend = |native: U256, token: Option<(&str, U256)>| Commitments {
            native_value: native.to_string(),
            fee: "0".into(),
            tokens: token.map(|(t, v)| [(t.to_string(), v.to_string())].into()).unwrap_or_default(),
            unknown_assets: false,
        };
        session.check_commitments(&owner, "live", &spend(U256::from(1), None)).await.unwrap();
        let over = seen.native.saturating_mul(U256::from(10)).max(U256::from(10u128.pow(30)));
        assert!(matches!(session.check_commitments(&owner, "live", &spend(over, None)).await, Err(CoreError::Insufficient(_))));
        let wq_over = seen.tokens[&wquai].saturating_mul(U256::from(2)) + U256::from(1);
        let refused = session.check_commitments(&owner, "live", &spend(U256::ZERO, Some((&wquai, wq_over)))).await;
        assert!(matches!(refused, Err(CoreError::Insufficient(text)) if text.contains("token")));
    }

    #[tokio::test]
    async fn order_bound_speedup_is_refused_before_keys_or_rpc() {
        let (_dir, mut session) = fixture();
        let owner = session.meta.quai_accounts[0].address.clone();
        let mut plan =
            crate::plans::TradePlan::new(session.network.id.clone(), owner.clone(), "order".into(), json!({"client":"order"})).unwrap();
        session.app.save_trade_plan(&mut plan).unwrap();
        let mut op =
            session.new_op(ReservationId([60; 16]), OpKind::Swap, "quai", &owner, "T", U256::from(5), TOKEN, json!({"plan_id":plan.id}));
        op.fee = "3".into();
        let id = op.id.clone();
        session.journal(op).unwrap();
        assert!(session.prepare_speed_up(&id, 10).await.unwrap_err().to_string().contains("saved fee budget"));
    }

    #[test]
    fn kernel_lock_child() {
        let Some(root) = std::env::var_os("COMMITMENT_LOCK_FIXTURE_ROOT") else {
            return;
        };
        let registry = crate::registry::Registry::new(crate::paths::Paths::resolve(Some(root.into())).unwrap());
        let meta = registry.list().unwrap().remove(0);
        let config = crate::config::AppConfig::default();
        let network = config.network("orchard").unwrap();
        let session = Session::open(registry, config, meta, network).unwrap();
        assert!(session.commitment_lock(&session.meta.quai_accounts[0].address).is_err());
    }

    #[test]
    fn owner_lock_serializes_independent_processes() {
        let (dir, session) = fixture();
        let _guard = session.commitment_lock(&session.meta.quai_accounts[0].address).unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "commitments::tests::kernel_lock_child", "--nocapture"])
            .env("COMMITMENT_LOCK_FIXTURE_ROOT", dir.path())
            .status()
            .unwrap();
        assert!(status.success());
    }
}
