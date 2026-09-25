//! An opened wallet on one network: SDK stores, app database, provider and optional unlocked keys.

use crate::appdb::{AppDb, OpStatus, Operation};
use crate::config::AppConfig;
use crate::error::{CoreError, Result};
use crate::identity::Unlocked;
use crate::network::{self, NetworkProfile, Node, WalletProvider, ZONE};
use crate::registry::{QuaiAccount, Registry, WalletMeta, now};
use quai_sdk::crypto::PublicKey;
use quai_sdk::primitives::Address;
use quai_sdk::qi::QiError;
use quai_sdk::qi_discovery::{QiBalance, QiScanOptions, qi_balance, refresh_qi, scan_and_refresh_qi};
use quai_sdk::wallet::storage::{KeyOrigin, PublicAddress, ReservationId, SqliteStore};
use quai_sdk::{BlockTag, QiAddress, QuaiAddress, U256};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Balance of one Quai account.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountBalance {
    /// Address.
    pub address: String,
    /// Label.
    pub label: String,
    /// HD index, when derived.
    pub hd_index: Option<u32>,
    /// Spendable balance at latest block (base units).
    #[serde(serialize_with = "crate::ser::u256", deserialize_with = "crate::ser::de_u256")]
    pub balance: U256,
    /// Locked (conversion) balance reported by the node (base units).
    #[serde(serialize_with = "crate::ser::u256", deserialize_with = "crate::ser::de_u256")]
    pub locked: U256,
    /// Account nonce at latest.
    pub nonce: u64,
}

/// What a launch paints before the network answers: the last known dashboard.
///
/// Display only, and re-fetchable — see [`Session::remember_dashboard`]. Written by whichever
/// process last read these values: a wallet at the end of a refresh, or the daemon on every poll.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DashboardCache {
    /// Quai account balances.
    pub accounts: Vec<AccountBalance>,
    /// Token balances across those accounts.
    pub tokens: Vec<crate::ops::TokenBalance>,
    /// Time-locked balances.
    pub locks: Vec<crate::track::LockItem>,
    /// Wrapped asset balances for the wrap card's account.
    pub wrap: Option<crate::ops::WrapStatus>,
}

/// A Qi coin for display.
#[derive(Clone, Debug, Serialize)]
pub struct CoinView {
    /// `txhash:index`.
    pub outpoint: String,
    /// Owner address.
    pub address: String,
    /// Qits.
    pub qits: u64,
    /// Denomination index.
    pub denomination: u8,
    /// Unlock height.
    #[serde(serialize_with = "crate::ser::u256")]
    pub unlock_height: U256,
    /// Held by a pending wallet operation.
    pub reserved: bool,
    /// Origin description (`receive #3`, `change #7`, `payment <code>`, `imported`).
    pub origin: String,
    /// Full payment code of the sender, for coins received on a payment channel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer: Option<String>,
    /// Label (mining, contact) when set.
    pub label: Option<String>,
}

/// Qi summary.
#[derive(Clone, Debug, Serialize)]
pub struct QiSummary {
    /// Exact buckets.
    pub balance: QiBalanceView,
    /// Checkpoint height of the current snapshot.
    pub checkpoint_height: Option<u64>,
    /// Coins in the snapshot.
    pub coins: Vec<CoinView>,
}

/// Serializable Qi balance buckets (Qits).
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct QiBalanceView {
    /// Total.
    #[serde(serialize_with = "crate::ser::u256")]
    pub total: U256,
    /// Spendable.
    #[serde(serialize_with = "crate::ser::u256")]
    pub spendable: U256,
    /// Reserved by pending operations.
    #[serde(serialize_with = "crate::ser::u256")]
    pub reserved: U256,
    /// Locked.
    #[serde(serialize_with = "crate::ser::u256")]
    pub locked: U256,
    /// Expired (not evaluated without a trim profile).
    #[serde(serialize_with = "crate::ser::u256")]
    pub expired: U256,
}

impl From<QiBalance> for QiBalanceView {
    fn from(b: QiBalance) -> Self {
        Self { total: b.total, spendable: b.spendable, reserved: b.reserved, locked: b.locked, expired: b.expired }
    }
}

/// A new random operation/reservation id.
pub fn new_operation_id() -> Result<ReservationId> {
    let mut bytes = [0u8; 16];
    quai_sdk::crypto::fill_random(&mut bytes).map_err(|_| CoreError::Storage("OS randomness unavailable".into()))?;
    Ok(ReservationId(bytes))
}

/// Hex form of a reservation id.
pub fn op_hex(id: ReservationId) -> String {
    hex::encode(id.0)
}

/// Parse a reservation id.
pub fn parse_op_id(text: &str) -> Result<ReservationId> {
    let mut bytes = [0u8; 16];
    hex::decode_to_slice(text, &mut bytes).map_err(|_| CoreError::Invalid(format!("invalid operation id `{text}`")))?;
    Ok(ReservationId(bytes))
}

/// An opened wallet session on one network.
pub struct Session {
    /// Registry.
    pub registry: Registry,
    /// Preferences.
    pub config: AppConfig,
    /// Wallet metadata.
    pub meta: WalletMeta,
    /// Network profile.
    pub network: NetworkProfile,
    /// The node every read goes to: the network's monitoring endpoint once it has shown the
    /// trusted chain and genesis, otherwise `rpc`. Balances, quotes, tracking and the reads that
    /// prepare a transaction all come from here.
    pub node: Node,
    /// The network's RPC endpoint. Transactions are broadcast here and nowhere else: a monitoring
    /// node is a full node with no hashrate behind it, so it is where to read, not where to send.
    pub rpc: Node,
    /// `node` is the verified monitoring endpoint.
    monitored: bool,
    /// The block display reads are made at, when the caller knows the head the screens were told
    /// about; `None` reads the latest. Reviews never use it: they read first-hand.
    read_at: Option<u64>,
    /// SDK store for Quai account custody.
    pub quai_store: SqliteStore,
    /// SDK store for Qi custody (HD, imported, payment channels).
    pub qi_store: SqliteStore,
    /// App database.
    pub app: AppDb,
    pub(crate) unlocked: Option<Unlocked>,
    pub(crate) pending: HashMap<String, crate::tx::Pending>,
    pub(crate) preparing_plan: Option<String>,
    /// Reviews that need a typed confirmation, by operation id: the words to type.
    pub(crate) confirmations: HashMap<String, String>,
    unlocked_at: u64,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("wallet", &self.meta.name)
            .field("network", &self.network.id)
            .field("unlocked", &self.unlocked.is_some())
            .finish()
    }
}

impl Session {
    /// Attach public operation records to a durable plan before preparation starts. This grants
    /// no signing authority. The caller must hold the plan coordinator lease.
    pub fn begin_plan_preparation(&mut self, id: &str) -> Result<()> {
        let plan = self.app.trade_plan(id)?.ok_or_else(|| CoreError::NotFound("trade plan".into()))?;
        if plan.network != self.network.id || matches!(plan.state, crate::plans::PlanState::Cancelled | crate::plans::PlanState::Complete) {
            return Err(CoreError::Rejected("plan is stopped or belongs to another network".into()));
        }
        if self.preparing_plan.is_some() {
            return Err(CoreError::Rejected("nested plan preparation".into()));
        }
        self.preparing_plan = Some(id.into());
        Ok(())
    }

    pub fn end_plan_preparation(&mut self) {
        self.preparing_plan = None;
    }

    /// Open a wallet on a network. No network access occurs.
    pub fn open(registry: Registry, config: AppConfig, meta: WalletMeta, network: NetworkProfile) -> Result<Self> {
        let meta = registry.load(&meta.id)?;
        let dir = registry.paths().network_dir(&meta.id, &network.id);
        crate::paths::ensure_private_dir(&dir)?;
        let scope = network.scope()?;
        let quai_store = SqliteStore::open(dir.join("quai.sqlite"), scope)?;
        let qi_store = SqliteStore::open(dir.join("qi.sqlite"), scope)?;
        let app = AppDb::open(&registry.paths().wallet_dir(&meta.id).join("app.sqlite"))?;
        let rpc = network.node()?;
        let mut session = Self {
            registry,
            config,
            meta,
            network,
            node: rpc.clone(),
            rpc,
            monitored: false,
            read_at: None,
            quai_store,
            qi_store,
            app,
            unlocked: None,
            pending: HashMap::new(),
            preparing_plan: None,
            confirmations: HashMap::new(),
            unlocked_at: 0,
        };
        session.sync_metadata()?;
        session.reconcile_custody()?;
        Ok(session)
    }

    /// The provider reads go to (see [`Session::node`]).
    pub fn provider(&self) -> &WalletProvider {
        &self.node.provider
    }

    /// Whether reads go to the verified monitoring endpoint.
    pub fn monitoring(&self) -> bool {
        self.monitored
    }

    /// Read from the RPC endpoint again, after the monitoring node stopped answering. The next
    /// [`Session::use_monitor`] can bring it back.
    pub fn drop_monitor(&mut self) {
        self.node = self.rpc.clone();
        self.monitored = false;
    }

    /// Cache key for the remembered dashboard on this network. One row, not four: the four values
    /// are read and written together, and a partial set would paint a dashboard whose halves came
    /// from different moments.
    fn dashboard_slot(&self) -> String {
        format!("dashboard:{}", self.network.id)
    }

    /// Keep what a launch paints, so the next one shows numbers immediately.
    ///
    /// **Display only.** This lands in the same re-fetchable cache table as prices and explorer
    /// answers: excluded from backups, safe to delete, and never read by a review — every review
    /// reads its balances, allowances and code from the node itself. What it buys is a screen with
    /// numbers on it at once, replaced stage by stage as the real ones arrive, instead of a wallet
    /// that shows nothing for the first second.
    pub fn remember_dashboard(&self, cache: &DashboardCache) {
        if let Ok(text) = serde_json::to_string(cache) {
            let _ = self.app.cache_put(&self.dashboard_slot(), &text);
        }
    }

    /// The remembered dashboard, with how long ago it was stored, when that is within `max_age`.
    pub fn remembered_dashboard(&self, max_age: u64) -> Option<(DashboardCache, u64)> {
        let (text, at) = self.app.cache_get(&self.dashboard_slot()).ok()??;
        let age = crate::registry::now().saturating_sub(at);
        (age <= max_age).then(|| serde_json::from_str(&text).ok().map(|c| (c, age)))?
    }

    /// Read everything a launch paints and remember it.
    ///
    /// The daemon calls this on every poll, so a wallet opened while it is running paints numbers
    /// that are current rather than whatever the last session left behind. It needs no keys — a
    /// balance, a token balance and a lock are all public reads — so a `--locked` daemon does it
    /// too, which is the only way it is worth having.
    pub async fn warm_dashboard_cache(&mut self) -> Result<()> {
        let head = self.head().await?;
        let accounts = self.quai_balances().await?;
        let locked: Vec<(String, U256)> = accounts.iter().map(|a| (a.label.clone(), a.locked)).collect();
        let locks = self.locks_from(head, &locked).unwrap_or_default();
        let owners: Vec<String> = self.meta.quai_accounts.iter().filter(|a| !a.archived).map(|a| a.address.clone()).collect();
        let read = self.dashboard_balances(&owners).await?;
        self.remember_dashboard(&DashboardCache { accounts, tokens: read.tokens, locks, wrap: read.wrap });
        Ok(())
    }

    pub(crate) fn require_execution_source(&self) -> Result<()> {
        self.config.require_execution_transport(&self.network)
    }

    /// The two nodes to compare before a review is shown: the monitoring node every read went to,
    /// and the RPC endpoint the transaction will be broadcast through. None when reads are not on
    /// a monitor. Cloned, so the comparison can run while the review is being prepared.
    pub fn lag_probe(&self) -> Option<network::LagProbe> {
        self.monitored.then(|| network::LagProbe {
            monitor: self.node.clone(),
            rpc: self.rpc.clone(),
            rpc_url: self.network.rpc_url.clone(),
        })
    }

    pub async fn use_monitor(&mut self) -> Option<String> {
        let endpoint = self.network.monitor.clone()?;
        // Bounded: an unreachable node must not hold up the first loads.
        let checked = async {
            let node = self.network.monitor_node()?;
            network::require_identity(&self.network, &node.provider).await?;
            Ok::<_, CoreError>(node)
        };
        let checked = tokio::time::timeout(std::time::Duration::from_secs(2), checked)
            .await
            .unwrap_or_else(|_| Err(CoreError::Network("did not answer within 2 s".into())));
        match checked {
            Ok(node) => {
                // The RPC transactions go to confirms the blocks the monitor proves state at.
                self.node = node.with_witness(self.rpc.clone());
                self.monitored = true;
                None
            }
            Err(e) => {
                self.drop_monitor();
                Some(format!("monitoring endpoint {} not used: {e}", endpoint.rpc_url))
            }
        }
    }

    /// Register every wallet account/key with the SDK stores for this network.
    pub fn sync_metadata(&mut self) -> Result<()> {
        let registered: std::collections::HashSet<Address> = self.quai_store.addresses()?.iter().map(PublicAddress::address).collect();
        let quai_account = self.meta.quai_account()?;
        let mut missing = Vec::new();
        for account in &self.meta.quai_accounts {
            let address: Address = account.address.parse().map_err(|_| CoreError::Storage("bad account address".into()))?;
            if registered.contains(&address) {
                continue;
            }
            let record = match (account.hd_index, &quai_account, &account.public_key) {
                (Some(index), Some(xpub), _) => PublicAddress::derive(xpub, false, index)?,
                (None, _, Some(pk)) => {
                    let bytes = hex::decode(pk).map_err(|_| CoreError::Storage("bad public key".into()))?;
                    PublicAddress::imported(&PublicKey::from_sec1_bytes(&bytes)?)?
                }
                _ => return Err(CoreError::Storage(format!("account {} lacks derivation data", account.address))),
            };
            if record.address() != address {
                return Err(CoreError::Storage(format!("account {} does not match its derivation", account.address)));
            }
            missing.push(record);
        }
        if !missing.is_empty() {
            let generation = self.quai_store.snapshot()?.generation;
            self.quai_store.import_metadata(generation, &missing)?;
        }
        let registered_qi: std::collections::HashSet<Address> = self.qi_store.addresses()?.iter().map(PublicAddress::address).collect();
        let mut missing_qi = Vec::new();
        for imported in &self.meta.qi_imported {
            let address: Address = imported.address.parse().map_err(|_| CoreError::Storage("bad Qi address".into()))?;
            if registered_qi.contains(&address) {
                continue;
            }
            let bytes = hex::decode(&imported.public_key).map_err(|_| CoreError::Storage("bad public key".into()))?;
            missing_qi.push(PublicAddress::imported(&PublicKey::from_sec1_bytes(&bytes)?)?);
        }
        if !missing_qi.is_empty() {
            let generation = self.qi_store.snapshot()?.generation;
            self.qi_store.import_metadata(generation, &missing_qi)?;
        }
        Ok(())
    }

    // ---------------- lock state ----------------

    /// Unlock with the vault password. The password itself is not kept: whatever needs it again
    /// (importing a key re-seals the vault) asks for it again.
    pub fn unlock(&mut self, password: &str) -> Result<()> {
        let (meta, unlocked) = self.registry.unlock_current(&self.meta.id, password)?;
        self.meta = meta;
        self.sync_metadata()?;
        self.unlocked = Some(unlocked);
        self.unlocked_at = now();
        Ok(())
    }

    /// Take keys whose password was checked elsewhere, so an unlock need not wait for whatever
    /// this session is doing. They must come from this session's own vault: the caller matches
    /// the wallet id before handing them over.
    pub fn use_keys(&mut self, unlocked: Unlocked) {
        self.unlocked = Some(unlocked);
        self.unlocked_at = now();
    }

    /// A copy of this session's keys for another session of the same wallet (see
    /// [`Unlocked::duplicate`]); `None` while locked.
    pub fn duplicate_keys(&self) -> Option<Unlocked> {
        self.keys().ok().and_then(|k| k.duplicate().ok())
    }

    /// Lock: drop keys and any unsigned pending reviews (their reservations are released).
    pub fn lock(&mut self) {
        let ids: Vec<String> = self.pending.keys().cloned().collect();
        for id in ids {
            let _ = self.discard(&id);
        }
        self.unlocked = None;
    }

    /// Loaded signing material, when unlocked.
    pub fn unlocked_keys(&self) -> Option<&Unlocked> {
        self.unlocked.as_ref()
    }

    /// Whether keys are loaded.
    pub fn is_unlocked(&self) -> bool {
        self.unlocked.is_some()
    }

    /// Unix time of the last unlock.
    pub fn unlocked_at(&self) -> u64 {
        self.unlocked_at
    }

    pub(crate) fn keys(&self) -> Result<&Unlocked> {
        let keys = self.unlocked.as_ref().ok_or_else(|| CoreError::Locked("wallet is locked; unlock with your password".into()))?;
        let current = self.registry.load(&self.meta.id)?;
        // Keys from before the vault last changed (a new password, an imported key) are refused;
        // a public change since (a new account, a label) leaves them this wallet's keys.
        if keys.vault_generation.is_none_or(|g| g < current.custody_generation) {
            return Err(CoreError::Locked("wallet changed in another session; unlock again before signing".into()));
        }
        Ok(keys)
    }

    /// Save metadata changes.
    pub fn save_meta(&mut self) -> Result<()> {
        self.registry.save(&mut self.meta)?;
        self.sync_metadata()
    }

    // ---------------- node ----------------

    /// Verify node identity.
    pub async fn verify_node(&self) -> Result<()> {
        network::require_identity(&self.network, self.provider()).await
    }

    /// Latest block height.
    pub async fn head(&self) -> Result<u64> {
        let header = self.provider().latest_header(ZONE).await?.ok_or_else(|| CoreError::Network("node returned no header".into()))?;
        Ok(header.number)
    }

    // ---------------- Quai accounts ----------------

    /// Resolve an account selector (label, address, index) or the default.
    pub fn account(&self, selector: Option<&str>) -> Result<QuaiAccount> {
        match selector {
            Some(s) => Ok(self.meta.find_quai_account(s)?.clone()),
            None => Ok(self.meta.default_quai_account()?.clone()),
        }
    }

    /// Addresses of all active Quai accounts and Quai watch-only addresses (no network access).
    pub fn quai_owner_addresses(&self) -> Vec<String> {
        self.meta.quai_owner_addresses()
    }

    /// Balances of all active Quai accounts (and Quai watch-only addresses).
    pub async fn quai_balances(&self) -> Result<Vec<AccountBalance>> {
        self.quai_balances_with(&[]).await
    }

    /// Read the dashboard's balances at this block: the head the screens were told about, so the
    /// balances beside a price describe the same block as the price. A node that cannot answer for
    /// it yet is asked for its latest instead.
    pub fn read_at_block(&mut self, block: Option<u64>) {
        self.read_at = block.filter(|b| *b > 0);
    }

    /// The block tag display reads use: [`Self::read_at_block`]'s, else the latest.
    pub fn read_tag(&self) -> BlockTag {
        self.read_at.map_or(BlockTag::Latest, |b| BlockTag::Number(U256::from(b)))
    }

    /// The same, reusing locked balances read earlier instead of asking again.
    ///
    /// A locked balance costs three calls, not one: the node method is wrapped in a header read
    /// before and after so the answer can be pinned to a block. It also only changes when a
    /// conversion settles, so reading it on every block was the single most expensive thing the
    /// refresh did. Pass what the last read found and it is carried; pass nothing and it is read.
    pub async fn quai_balances_with(&self, known_locked: &[(String, U256)]) -> Result<Vec<AccountBalance>> {
        let mut out = Vec::new();
        let mut entries: Vec<(String, String, Option<u32>)> =
            self.meta.quai_accounts.iter().filter(|a| !a.archived).map(|a| (a.address.clone(), a.label.clone(), a.hd_index)).collect();
        for w in &self.meta.watch {
            if let Ok(addr) = w.address.parse::<Address>()
                && addr.ledger() == quai_sdk::Ledger::Quai
            {
                entries.push((w.address.clone(), format!("{} (watch)", w.label), None));
            }
        }
        let carried: HashMap<&str, U256> = known_locked.iter().map(|(a, v)| (a.as_str(), *v)).collect();
        let at = self.read_tag();
        match self.balances_at(&entries, &carried, at).await {
            Ok(read) => out.extend(read),
            // The node has not reached the announced block yet: its latest, rather than nothing.
            Err(_) if at != BlockTag::Latest => out.extend(self.balances_at(&entries, &carried, BlockTag::Latest).await?),
            Err(e) => return Err(e),
        }
        Ok(out)
    }

    async fn balances_at(
        &self,
        entries: &[(String, String, Option<u32>)],
        carried: &HashMap<&str, U256>,
        at: BlockTag,
    ) -> Result<Vec<AccountBalance>> {
        // Every account's reads at once: they are independent, and a wallet with five accounts
        // otherwise waits on that many round trips in a row.
        let provider = self.provider();
        let reads = entries.iter().cloned().map(|(address, label, hd_index)| {
            let carried = carried.get(address.as_str()).copied();
            async move {
                let parsed: QuaiAddress = address.parse().map_err(|_| CoreError::Storage(format!("invalid account {address}")))?;
                let locked = async {
                    match carried {
                        Some(v) => v,
                        None => provider.locked_quai_balance(parsed).await.map(|l| l.balance).unwrap_or(U256::ZERO),
                    }
                };
                let (balance, nonce, locked) = tokio::join!(provider.balance(parsed, at), provider.transaction_count(parsed, at), locked,);
                Ok::<_, CoreError>(AccountBalance { address, label, hd_index, balance: balance?, nonce: nonce?, locked })
            }
        });
        futures::future::try_join_all(reads).await
    }

    /// Make an account the one that acts when none is named (label, address or 1-based index).
    /// Public metadata only: keys, reviews and other sessions' unlocks are untouched.
    pub fn set_active_account(&mut self, selector: &str) -> Result<QuaiAccount> {
        // A watch-only wallet's addresses act too (for what they can do: quotes, views).
        let watched = || {
            self.meta.watch.iter().find(|w| w.address.eq_ignore_ascii_case(selector) || w.label.eq_ignore_ascii_case(selector)).map(|w| QuaiAccount {
                address: w.address.clone(),
                label: w.label.clone(),
                hd_index: None,
                archived: false,
                public_key: None,
            })
        };
        let account = match self.meta.find_quai_account(selector) {
            Ok(a) => a.clone(),
            Err(e) => watched().ok_or(e)?,
        };
        if account.archived {
            return Err(CoreError::Invalid(format!("{} is archived; unarchive it first", account.label)));
        }
        let address = account.address.clone();
        let mut meta = self.meta.clone();
        self.registry.update_meta(&mut meta, |m| {
            m.active_account = Some(address);
            Ok(())
        })?;
        self.meta = meta;
        self.sync_metadata()?;
        Ok(account)
    }

    /// Add the next HD Quai account.
    pub fn add_account(&mut self, label: Option<&str>) -> Result<QuaiAccount> {
        let mut meta = self.meta.clone();
        let record = self.registry.add_quai_account(&mut meta, label)?;
        self.meta = meta;
        self.sync_metadata()?;
        Ok(record)
    }

    /// Watch another address in this watch-only wallet. Returns it, checksummed.
    ///
    /// Only a watch-only wallet watches. In a wallet with keys a watched address would sit among
    /// its accounts, and one of them is a receive address someone could hand out for money that
    /// no key here can move.
    pub fn add_watch_address(&mut self, address: &str, label: Option<&str>) -> Result<String> {
        if self.meta.kind != crate::registry::WalletKind::Watch {
            return Err(CoreError::Invalid(
                "only a watch-only wallet watches addresses; create one (wallet watch) to follow an address you hold no key for".into(),
            ));
        }
        let parsed = crate::registry::parse_any_address(address)?.to_string();
        let label = label.map(str::trim).filter(|l| !l.is_empty()).map(str::to_string);
        if label.as_ref().is_some_and(|l| l.chars().count() > 64 || l.chars().any(char::is_control)) {
            return Err(CoreError::Invalid("a label is up to 64 printable characters".into()));
        }
        self.registry.update_meta(&mut self.meta, |meta| {
            if meta.watch.iter().any(|w| w.address.eq_ignore_ascii_case(&parsed)) {
                return Err(CoreError::Invalid(format!("{parsed} is already watched here")));
            }
            let label = label.unwrap_or_else(|| format!("Watch {}", meta.watch.len() + 1));
            meta.watch.push(crate::registry::WatchAddress { address: parsed.clone(), label });
            Ok(())
        })?;
        Ok(parsed)
    }

    /// Rename a Quai account.
    pub fn rename_account(&mut self, selector: &str, label: &str) -> Result<()> {
        let address = self.meta.find_quai_account(selector)?.address.clone();
        self.registry.update_meta(&mut self.meta, |meta| {
            let account = meta
                .quai_accounts
                .iter_mut()
                .find(|a| a.address == address)
                .ok_or_else(|| CoreError::NotFound("account no longer exists".into()))?;
            account.label = label.to_string();
            Ok(())
        })?;
        self.sync_metadata()
    }

    /// Archive or restore an account (custody records are kept).
    pub fn set_archived(&mut self, selector: &str, archived: bool) -> Result<()> {
        let address = self.meta.find_quai_account(selector)?.address.clone();
        self.registry.update_meta(&mut self.meta, |meta| {
            let account = meta
                .quai_accounts
                .iter_mut()
                .find(|a| a.address == address)
                .ok_or_else(|| CoreError::NotFound("account no longer exists".into()))?;
            account.archived = archived;
            Ok(())
        })?;
        self.sync_metadata()
    }

    /// Discover used HD Quai accounts (nonce or balance) with a gap limit, adding them.
    pub async fn discover_quai_accounts(&mut self, gap: u32) -> Result<Vec<QuaiAccount>> {
        let account =
            self.meta.quai_account()?.ok_or_else(|| CoreError::Invalid("only recovery-phrase wallets can discover accounts".into()))?;
        let mut added = Vec::new();
        let mut empty_run = 0;
        let mut index = 0u32;
        let known: std::collections::HashSet<u32> = self.meta.quai_accounts.iter().filter_map(|a| a.hd_index).collect();
        while empty_run < gap {
            let found =
                account.search(false, quai_sdk::wallet::Search { zone: ZONE, start_index: index, max_attempts: 1_000_000 }, || false)?;
            index = found.address.index + 1;
            let address: QuaiAddress =
                found.address.address.try_into().map_err(|_| CoreError::Storage("derived address is not Quai".into()))?;
            let nonce = self.provider().transaction_count(address, BlockTag::Latest).await?;
            let balance = self.provider().balance(address, BlockTag::Latest).await?;
            if nonce > 0 || !balance.is_zero() {
                empty_run = 0;
                if !known.contains(&found.address.index) {
                    let n = self.meta.quai_accounts.len() + 1;
                    let mut meta = self.meta.clone();
                    let record = self.registry.add_quai_account_at(&mut meta, found.address.index, &format!("Account {n}"))?;
                    self.meta = meta;
                    added.push(record);
                }
            } else {
                empty_run += 1;
            }
        }
        self.sync_metadata()?;
        Ok(added)
    }

    // ---------------- Qi ----------------

    /// Refresh Qi state for every known address. Retries head changes a few times.
    pub async fn refresh_qi(&mut self) -> Result<u64> {
        if self.qi_store.addresses()?.is_empty() {
            if self.meta.qi_xpub.is_some() {
                return Box::pin(self.scan_qi(None)).await;
            }
            return Ok(0);
        }
        for attempt in 0..5 {
            match refresh_qi(&self.node.provider, &mut self.qi_store, 100_000, || false).await {
                Ok(cp) => {
                    // How many times the chain moved under us before this stuck. A whole refresh
                    // is redone per retry, not a cheap re-read, so this is the difference between
                    // a 1.4 s Qi stage and a 4.6 s one — and the only way to see that from a trace.
                    crate::diag::count("refresh.qi.retries", attempt);
                    return Ok(u64::try_from(cp.height).unwrap_or(0));
                }
                Err(e) if qi_stale(&e) && attempt < 4 => stale_pause().await,
                Err(e) => {
                    crate::diag::count("refresh.qi.retries", attempt);
                    return Err(e.into());
                }
            }
        }
        crate::diag::count("refresh.qi.retries", 5);
        Err(CoreError::Network("Qi refresh kept crossing block boundaries; try again".into()))
    }

    /// Gap scan (default 50) or deep scan to `deep_end` raw index, then refresh.
    pub async fn scan_qi(&mut self, deep_end: Option<u32>) -> Result<u64> {
        let Some(account) = self.meta.qi_account()? else {
            return self.refresh_qi().await;
        };
        let mut options = QiScanOptions::default();
        if let Some(end) = deep_end {
            options.gap_limit = None;
            options.receive.end = end;
            options.change.end = end;
            options.max_addresses = 100_000;
        }
        for attempt in 0..5 {
            match scan_and_refresh_qi(&self.node.provider, &mut self.qi_store, &account, &options, || false).await {
                Ok(_) => {
                    let snapshot = self.qi_store.snapshot()?;
                    return Ok(snapshot.checkpoint.and_then(|c| u64::try_from(c.height).ok()).unwrap_or(0));
                }
                Err(e) if qi_stale(&e) && attempt < 4 => stale_pause().await,
                Err(e) => return Err(e.into()),
            }
        }
        Err(CoreError::Network("Qi scan kept crossing block boundaries; try again".into()))
    }

    /// Qi balances and coins from the current snapshot (no network).
    pub fn qi_summary(&mut self) -> Result<QiSummary> {
        let snapshot = self.qi_store.snapshot()?;
        let Some(checkpoint) = snapshot.checkpoint else {
            return Ok(QiSummary { balance: QiBalanceView::default(), checkpoint_height: None, coins: vec![] });
        };
        let height = checkpoint.height.saturating_add(U256::from(1));
        let balance = qi_balance(&mut self.qi_store, height)?;
        let owners: HashMap<Address, PublicAddress> = self.qi_store.addresses()?.into_iter().map(|a| (a.address(), a)).collect();
        let channels = self.payment_exposures()?;
        let mut coins = Vec::new();
        for coin in snapshot.coins {
            let address = coin.address.address();
            let origin = match owners.get(&address).map(PublicAddress::origin) {
                Some(KeyOrigin::Bip44 { change, index, .. }) => {
                    format!("{} #{index}", if change { "change" } else { "receive" })
                }
                Some(KeyOrigin::ImportedPublic) => match channels.get(&address) {
                    Some(peer) => format!("payment from {}", short_code(peer)),
                    None => "imported".into(),
                },
                None => "unknown".into(),
            };
            coins.push(CoinView {
                outpoint: format!("{}:{}", coin.outpoint.transaction_hash, coin.outpoint.index),
                address: coin.address.to_string(),
                qits: coin.denomination.value(),
                denomination: coin.denomination.index(),
                unlock_height: coin.unlock_height,
                reserved: coin.reserved,
                origin,
                peer: channels.get(&address).cloned(),
                label: self.app.label(&coin.address.to_string())?,
            });
        }
        coins.sort_by(|a, b| b.qits.cmp(&a.qits).then(a.outpoint.cmp(&b.outpoint)));
        Ok(QiSummary { balance: balance.into(), checkpoint_height: u64::try_from(checkpoint.height).ok(), coins })
    }

    /// Map of payment-channel receive addresses to peer codes.
    pub fn payment_exposures(&self) -> Result<HashMap<Address, String>> {
        let mut map = HashMap::new();
        let cache_key = format!("payment_exposures:{}", self.network.id);
        let Some(keys) = self.unlocked.as_ref().and_then(|u| u.payment.as_ref()) else {
            // Locked: use the public mapping cached the last time the wallet was unlocked.
            if let Some(text) = self.app.kv(&cache_key)?
                && let Ok(cached) = serde_json::from_str::<HashMap<String, String>>(&text)
            {
                for (address, peer) in cached {
                    if let Ok(parsed) = address.parse::<Address>() {
                        map.insert(parsed, peer);
                    }
                }
            }
            return Ok(map);
        };
        for channel in self.qi_store.payment_channels(keys)? {
            let peer = channel.channel.counterparty_code().clone();
            for record in self.qi_store.payment_addresses(keys, &peer, quai_sdk::payments::PaymentDirection::Receive)? {
                map.insert(record.address.address(), peer.to_base58());
            }
        }
        let cached: HashMap<String, String> = map.iter().map(|(a, p)| (a.to_string(), p.clone())).collect();
        self.app.set_kv(&cache_key, &serde_json::to_string(&cached)?)?;
        Ok(map)
    }

    /// Current Qi held by watch-only Qi addresses (latest node view, Qits).
    pub async fn watch_qi_balances(&self) -> Result<Vec<(String, String, U256)>> {
        let mut out = Vec::new();
        for w in &self.meta.watch {
            if let Ok(address) = w.address.parse::<QiAddress>() {
                let outpoints = self.provider().outpoints(address).await?;
                let total = outpoints
                    .iter()
                    .filter_map(|o| quai_sdk::consensus::Denomination::new(o.denomination).ok())
                    .fold(U256::ZERO, |acc, d| acc.saturating_add(U256::from(d.value())));
                out.push((w.address.clone(), w.label.clone(), total));
            }
        }
        Ok(out)
    }

    /// Allocate a fresh Qi receive address (Pelagus "Qi address" / coinbase address).
    pub fn new_qi_address(&mut self, label: Option<&str>) -> Result<QiAddress> {
        let account = self.meta.qi_account()?.ok_or_else(|| CoreError::Invalid("this wallet has no Qi HD account".into()))?;
        let allocation = self.qi_store.allocate_address_compact(&account, false, 100_000, || false)?;
        let address: QiAddress =
            allocation.address.address().try_into().map_err(|_| CoreError::Storage("allocated address is not Qi".into()))?;
        if let Some(label) = label {
            self.app.set_label(&address.to_string(), label)?;
        }
        Ok(address)
    }

    /// Qi receive addresses allocated so far (receive branch), newest first.
    pub fn qi_receive_addresses(&self) -> Result<Vec<(u32, String, Option<String>)>> {
        let mut out = Vec::new();
        for a in self.qi_store.addresses()? {
            if let KeyOrigin::Bip44 { change: false, index, coin, .. } = a.origin()
                && coin == quai_sdk::wallet::CoinType::Qi
            {
                let text = a.address().to_string();
                let label = self.app.label(&text)?;
                out.push((index, text, label));
            }
        }
        out.sort_by(|a, b| b.0.cmp(&a.0));
        Ok(out)
    }

    // ---------------- journal helpers ----------------

    pub(crate) fn journal(&self, op: Operation) -> Result<()> {
        self.app.remove_cancelled_operation(&op.id)?;
        self.app.insert_operation(&op)
    }

    pub(crate) fn new_op(
        &self,
        id: ReservationId,
        kind: crate::journal::OpKind,
        store: &str,
        account: &str,
        asset: &str,
        amount: U256,
        counterparty: &str,
        detail: impl Into<crate::journal::Detail>,
    ) -> Operation {
        let mut detail = detail.into();
        if let Some(plan) = &self.preparing_plan {
            detail.set_plan_id(plan.clone());
        }
        Operation {
            id: op_hex(id),
            network: self.network.id.clone(),
            kind,
            store: store.into(),
            account: account.into(),
            status: OpStatus::Prepared,
            tx_hash: None,
            asset: asset.into(),
            amount: amount.to_string(),
            counterparty: counterparty.into(),
            fee: String::new(),
            detail,
            created: now(),
            updated: now(),
        }
    }
}

/// Keep the first `head` and last `tail` characters of text longer than `over` characters.
/// Counted in characters, not bytes: the text may come from an explorer, and slicing a
/// multi-byte character in half would panic.
fn abbreviate(text: &str, over: usize, head: usize, tail: usize) -> String {
    let count = text.chars().count();
    if count <= over {
        return text.to_string();
    }
    let start: String = text.chars().take(head).collect();
    let end: String = text.chars().skip(count - tail).collect();
    format!("{start}…{end}")
}

/// Whether a Qi failure only means the state moved under the read, so doing it again can work.
///
/// The SDK reports several different things as stale: the tip moving across a refresh
/// (`QiError::StaleSnapshot`), another writer committing to the same store first
/// (`QiError::Storage(StaleSnapshot)`, what the daemon and the terminal do to each other), and,
/// since alpha.6, an observation that lost a race (`StorageError::ObservationRaced`). Matching
/// single variants left the others to fail outright; the SDK's own class covers them all.
pub(crate) fn qi_stale(e: &QiError) -> bool {
    e.class() == quai_sdk::ErrorClass::Stale
}

/// A pause of 100–400 ms before retrying a stale read. Random, so two processes that collided
/// do not retry in step and collide again.
pub(crate) async fn stale_pause() {
    let mut b = [0u8; 1];
    let _ = quai_sdk::crypto::fill_random(&mut b);
    tokio::time::sleep(std::time::Duration::from_millis(100 + u64::from(b[0]) * 300 / 255)).await;
}

/// Abbreviate a payment code for display.
pub fn short_code(code: &str) -> String {
    abbreviate(code, 16, 8, 6)
}

/// Abbreviate an address for lists (never for review screens).
pub fn short_address(address: &str) -> String {
    abbreviate(address, 14, 6, 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The TUI holds two sessions of one wallet: one adds an account, the other signs. Adding an
    /// account used to make the signer's keys look stale ("wallet changed in another session"),
    /// so funding a new account failed until the wallet was unlocked again. A new password still
    /// does make them stale.
    #[test]
    fn a_new_account_leaves_another_session_s_keys_usable() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::paths::Paths::resolve(Some(dir.path().to_path_buf())).unwrap();
        let registry = Registry::fast(paths);
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let meta = registry.create_hd("bob", phrase, "english", "", "password123", true).unwrap();
        let network = crate::network::NetworkProfile::builtins().into_iter().next().unwrap();
        let mut worker = Session::open(registry.clone(), crate::config::AppConfig::default(), meta.clone(), network.clone()).unwrap();
        worker.unlock("password123").unwrap();
        let mut signer = Session::open(registry.clone(), crate::config::AppConfig::default(), meta, network).unwrap();
        signer.use_keys(worker.duplicate_keys().unwrap());
        worker.add_account(Some("messaging")).unwrap();
        assert!(worker.keys().is_ok(), "the session that added it");
        assert!(signer.keys().is_ok(), "and the one that signs");
        registry.change_password(&worker.meta, "password123", "rotatedpassword").unwrap();
        assert!(signer.keys().is_err(), "a new password still asks for the unlock again");
    }

    /// Every way a Qi read goes stale is retried: the tip moving across it, another writer
    /// committing to the same store first (what the daemon and the terminal do to each other),
    /// and an observation that lost a race. Anything else is a real failure and is not.
    #[test]
    fn both_kinds_of_stale_qi_state_are_retried() {
        use quai_sdk::wallet::storage::StorageError;
        assert!(qi_stale(&QiError::StaleSnapshot), "the tip moved");
        let other_writer = QiError::Storage(StorageError::StaleSnapshot);
        assert_eq!(other_writer.to_string(), "stale or mismatched wallet snapshot", "the daemon log's line");
        assert!(qi_stale(&other_writer), "another writer committed first");
        // alpha.6 moved a lost observation race out of Conflict (Invalid) into its own Stale error.
        assert!(qi_stale(&QiError::Storage(StorageError::ObservationRaced)), "an observation lost a race");
        assert!(!qi_stale(&QiError::InvalidPolicy));
        assert!(!qi_stale(&QiError::InsufficientChange));
    }

    /// The dashboard a launch paints survives the round trip through the cache, exact amounts and
    /// all, and is refused once it is older than the caller is willing to show.
    ///
    /// The amounts matter: they are `U256` written as decimal strings, so a reader that quietly
    /// truncated or defaulted them would put a wrong balance on screen — briefly, but on screen.
    #[test]
    fn the_remembered_dashboard_round_trips_exactly_and_expires() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::paths::Paths::resolve(Some(dir.path().to_path_buf())).unwrap();
        let registry = Registry::fast(paths);
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let meta = registry.create_hd("main", phrase, "english", "", "password123", true).unwrap();
        let network = NetworkProfile::builtins().into_iter().next().unwrap();
        let session = Session::open(registry, AppConfig::default(), meta, network).unwrap();
        assert!(session.remembered_dashboard(86_400).is_none(), "nothing remembered yet");
        // A balance too large for a u64, which is the case a lazy deserializer gets wrong.
        let big = U256::from_str_radix("123456789012345678901234567890", 10).unwrap();
        let cache = DashboardCache {
            accounts: vec![AccountBalance {
                address: "0x00a1".into(),
                label: "main".into(),
                hd_index: Some(0),
                balance: big,
                locked: U256::from(7u64),
                nonce: 3,
            }],
            tokens: Vec::new(),
            locks: Vec::new(),
            wrap: None,
        };
        session.remember_dashboard(&cache);
        let (back, age) = session.remembered_dashboard(86_400).expect("just stored");
        assert_eq!(back.accounts.len(), 1);
        assert_eq!(back.accounts[0].balance, big, "the exact amount, not a truncated one");
        assert_eq!(back.accounts[0].locked, U256::from(7u64));
        assert_eq!(back.accounts[0].nonce, 3);
        assert!(age <= 1, "stored a moment ago, not {age}s");
        // Too old to show is the same as not having it. (A `max_age` of one second rather than
        // zero: the store and this read can straddle a second boundary under a loaded test run,
        // and a flaky test is worse than a slightly looser one.)
        assert!(session.remembered_dashboard(1).is_some(), "stored a moment ago");
        let key = format!("dashboard:{}", session.network.id);
        session.app.conn_for_tests().execute("UPDATE cache SET fetched = 1 WHERE key = ?1", [&key]).unwrap();
        assert!(session.remembered_dashboard(86_400).is_none(), "an ancient dashboard is not painted");
    }

    /// Importing a key re-seals the vault with the password it is given, so that password is
    /// checked first: a typo must neither become the vault's password nor import anything. The
    /// session itself holds no copy of the password to fall back on.
    #[test]
    fn importing_a_key_needs_the_right_password_again() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::paths::Paths::resolve(Some(dir.path().to_path_buf())).unwrap();
        let registry = Registry::fast(paths);
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let meta = registry.create_hd("main", phrase, "english", "", "password123", true).unwrap();
        let network = NetworkProfile::builtins().into_iter().next().unwrap();
        let mut s = Session::open(registry.clone(), AppConfig::default(), meta, network).unwrap();
        s.unlock("password123").unwrap();
        // A key controlling a Cyprus-1 address, found the way the registry tests find one.
        let key = (1u32..20_000)
            .map(|i| format!("{i:064x}"))
            .find(|k| crate::identity::parse_secret_hex(k).unwrap().public_key().address().zone().ok() == Some(ZONE))
            .unwrap();
        assert!(s.import_key("wrongpass9", &key, "miner").is_err(), "a mistyped password is refused");
        assert!(registry.unlock(&s.meta, "password123").unwrap().secrets().imported.is_empty(), "and nothing was sealed");
        s.import_key("password123", &key, "miner").unwrap();
        assert_eq!(registry.unlock(&s.meta, "password123").unwrap().secrets().imported.len(), 1);
    }

    /// A watch-only wallet takes more addresses, once each, and survives a reopen with them. A
    /// wallet with keys refuses: a watched address among its accounts could be handed out to be
    /// paid into, with no key here to move what arrives.
    #[test]
    fn a_watch_wallet_watches_more_addresses_and_a_key_wallet_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::paths::Paths::resolve(Some(dir.path().to_path_buf())).unwrap();
        let registry = Registry::fast(paths);
        let first = "0x0019f740d9e1602ce0e5a5da864e50b8d7d5ab61";
        let second = "0x0035187a7660f595d93cd53a4d16c635d6cffc8f";
        let meta = registry.create_watch("watched", &[(first.into(), "Watch 1".into())]).unwrap();
        let network = NetworkProfile::builtins().into_iter().next().unwrap();
        let mut s = Session::open(registry.clone(), AppConfig::default(), meta, network.clone()).unwrap();
        let added = s.add_watch_address(second, None).unwrap();
        assert!(added.eq_ignore_ascii_case(second));
        assert_eq!(s.meta.watch.len(), 2);
        assert_eq!(s.meta.watch[1].label, "Watch 2", "a default label that counts");
        assert!(s.add_watch_address(&second.to_uppercase().replace("0X", "0x"), Some("again")).is_err(), "once each, whatever the case");
        assert!(s.add_watch_address("0x1234", None).is_err(), "an address, not anything");
        assert!(s.add_watch_address(first.trim_start_matches("0x"), Some("bad\u{7}label")).is_err());
        assert_eq!(registry.load(&s.meta.id).unwrap().watch.len(), 2, "it is on disk, not only in this session");
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let hd = registry.create_hd("main", phrase, "english", "", "password123", true).unwrap();
        let mut keyed = Session::open(registry, AppConfig::default(), hd, network).unwrap();
        let refused = keyed.add_watch_address(second, None).unwrap_err().to_string();
        assert!(refused.contains("watch-only wallet"), "{refused}");
        assert!(keyed.meta.watch.is_empty());
    }

    /// Abbreviation counts characters: text from an explorer can hold multi-byte characters, and
    /// cutting one in half would crash the wallet.
    #[test]
    fn abbreviating_never_splits_a_character() {
        assert_eq!(short_address("0x0048ccd296f2484ec4e61d8375d15bfc2991b780"), "0x0048…b780");
        assert_eq!(short_address("0x00ab"), "0x00ab");
        assert_eq!(short_address("ééééééééééééééééé"), "éééééé…éééé");
        assert_eq!(short_code("🙂".repeat(20).as_str()).chars().count(), 15);
    }
}
