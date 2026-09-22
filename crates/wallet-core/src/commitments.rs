//! Owner-scoped pending financial commitments shared by all sessions under one data root.
use crate::appdb::{AppDb, Operation};
use crate::error::{CoreError, Result};
use crate::session::Session;
use crate::tx::FinancialEffect;
use quai_sdk::{BlockTag, QuaiAddress, U256};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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
    pub(crate) fn from_intent(kind: &str, amount: &str, native: U256, fee: U256, detail: &serde_json::Value) -> Result<Self> {
        let mut out = Self { native_value: native.to_string(), fee: fee.to_string(), ..Self::default() };
        if kind == "approve" || kind.starts_with("approve_") {
            return Ok(out);
        }
        if let Some(effects) = detail.get("financial_effects") {
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
            "send_token" | "curve_sell" => Some("token"),
            "swap" | "swap_exact_output" => Some("from_token"),
            "stake" | "remove_liquidity" => Some("pair"),
            "incentivize" => Some("reward"),
            "unwrap_wqi" | "unwrap_quai" => Some("contract"),
            _ => None,
        };
        if let Some(field) = field {
            match detail.get(field).and_then(|v| v.as_str()) {
                Some(token) if token.eq_ignore_ascii_case("quai") => (),
                Some(token) => {
                    let token: QuaiAddress = token.parse().map_err(|_| CoreError::Storage("invalid pending debit token".into()))?;
                    let debit = if kind == "unwrap_wqi" { quai_sdk::wrappers::qits_to_wqi_atoms(atoms(amount)?)? } else { atoms(amount)? };
                    out.tokens.insert(token.to_string().to_lowercase(), debit.to_string());
                }
                None => out.unknown_assets = true,
            }
        }
        if matches!(kind, "contract_call" | "recovered") {
            out.unknown_assets = true;
        }
        Ok(out)
    }
    pub(crate) fn from_operation(op: &Operation) -> Result<Self> {
        let mut value = if let Some(saved) = op.detail.get("commitments") {
            serde_json::from_value(saved.clone())?
        } else {
            Self::from_intent(
                &op.kind,
                &op.amount,
                atoms(op.detail.get("native_value").and_then(|v| v.as_str()).unwrap_or("0"))?,
                atoms(&op.fee)?,
                &op.detail,
            )?
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
                    let nonce = op.detail.get("nonce").and_then(|n| n.as_u64()).or_else(|| nonces.get(&op.id).copied());
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

    pub(crate) async fn check_commitments(&self, owner: &str, own_id: &str, wanted: &Commitments) -> Result<()> {
        let address: QuaiAddress = owner.parse().map_err(|_| CoreError::Invalid("invalid commitment owner".into()))?;
        let head = self
            .node
            .provider
            .latest_header(crate::network::ZONE)
            .await?
            .ok_or_else(|| CoreError::Network("no commitment observation header".into()))?;
        let block = BlockTag::Number(U256::from(head.number));
        let nonce = self.node.provider.transaction_count(address, block).await?;
        let (native, tokens) = self.pending_commitments_at_nonce(owner, own_id, wanted, Some(nonce))?;
        let balance = self.node.provider.balance(address, block).await?;
        if balance < native {
            return Err(CoreError::Insufficient(format!(
                "QUAI balance cannot cover this spend plus open commitments (requires {native} base units including fees)"
            )));
        }
        for (token, needed) in tokens {
            let contract = quai_sdk::contracts::Erc20::new(
                token.parse().map_err(|_| CoreError::Storage("invalid commitment token".into()))?,
                &self.node.provider,
            )?;
            let balance = contract.balance_of(address, address, block).await?;
            if balance < needed {
                return Err(CoreError::Insufficient(format!(
                    "token {token} balance cannot cover this spend plus open commitments (requires {needed} base units)"
                )));
            }
        }
        if self.node.provider.header_at(crate::network::ZONE, head.number).await?.is_none_or(|now| now.hash != head.hash) {
            return Err(CoreError::Rejected("commitment observation changed; refresh and review again".into()));
        }
        Ok(())
    }
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
        let legacy = Commitments::from_intent("unwrap_wqi", "2000", U256::ZERO, U256::ZERO, &json!({"contract":token})).unwrap();
        assert_eq!(legacy.tokens[token], "2000000000000000000");
        let effects =
            json!({"financial_effects":[{"direction":"out","asset":"WQI","token":token,"decimals":18,"amount":"2000000000000000000"}]});
        assert_eq!(Commitments::from_intent("unwrap_wqi", "2000", U256::ZERO, U256::ZERO, &effects).unwrap().tokens, legacy.tokens);
    }

    #[test]
    fn exact_token_debits_native_value_and_replacement_fee_are_not_approvals_or_double_counted() {
        let detail = json!({"financial_effects":[
            {"direction":"out","asset":"QUAI","token":"quai","decimals":18,"amount":"100"},
            {"direction":"out","asset":"T","token":TOKEN,"decimals":6,"amount":"50"},
            {"direction":"out","asset":"T","token":TOKEN,"decimals":6,"amount":"20"},
            {"direction":"in","asset":"T","token":TOKEN,"decimals":6,"amount":"999"}]});
        let effects = Commitments::from_intent("swap_exact_output", "70", U256::from(100), U256::from(5), &detail).unwrap();
        assert_eq!(effects.native().unwrap(), U256::from(105));
        assert_eq!(effects.tokens[TOKEN], "70");
        let approval = Commitments::from_intent("approve", &U256::MAX.to_string(), U256::ZERO, U256::from(3), &detail).unwrap();
        assert!(approval.tokens.is_empty());
        assert_eq!(approval.native().unwrap(), U256::from(3));
        let (_dir, session) = fixture();
        let owner = &session.meta.quai_accounts[0].address;
        let mut op = session.new_op(
            ReservationId([1; 16]),
            "swap_exact_output",
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
                "send_token",
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
        let wanted = Commitments::from_intent("send_token", "50", U256::ZERO, U256::from(3), &json!({"token":TOKEN})).unwrap();
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
        let mut op =
            session.new_op(ReservationId([9; 16]), "contract_call", "quai", owner, "QUAI", U256::ZERO, TOKEN, json!({"native_value":"0"}));
        op.fee = "5".into();
        session.journal(op).unwrap();
        let wanted = Commitments::from_intent("send_quai", "1", U256::from(1), U256::from(1), &json!({})).unwrap();
        assert!(session.pending_commitments(owner, "new", &wanted).unwrap_err().to_string().contains("unknown asset debits"));
    }

    #[test]
    fn orphan_nonce_blocks_and_explicit_unsigned_release_restores_availability() {
        let (_dir, mut session) = fixture();
        let owner = session.meta.quai_accounts[0].address.clone();
        let id = ReservationId([50; 16]);
        session.quai_store.reserve_nonce(id, owner.parse().unwrap(), 0).unwrap();
        let wanted = Commitments::from_intent("send_quai", "1", U256::from(1), U256::from(1), &json!({})).unwrap();
        assert!(session.pending_commitments(&owner, "new", &wanted).unwrap_err().to_string().contains("no financial journal"));
        session.quai_store.release_unsigned(id).unwrap();
        assert_eq!(session.pending_commitments(&owner, "new", &wanted).unwrap().0, U256::from(2));
    }

    #[tokio::test]
    async fn order_bound_speedup_is_refused_before_keys_or_rpc() {
        let (_dir, mut session) = fixture();
        let owner = session.meta.quai_accounts[0].address.clone();
        let mut plan =
            crate::plans::TradePlan::new(session.network.id.clone(), owner.clone(), "order".into(), json!({"client":"order"})).unwrap();
        session.app.save_trade_plan(&mut plan).unwrap();
        let mut op = session.new_op(ReservationId([60; 16]), "swap", "quai", &owner, "T", U256::from(5), TOKEN, json!({"plan_id":plan.id}));
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
