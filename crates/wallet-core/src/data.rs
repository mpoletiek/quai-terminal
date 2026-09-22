//! Read-only ecosystem data context shared by the CLI, the TUI data worker and the daemon:
//! the network's explorer, a node for on-chain checks, the wallet's cache database and the
//! user's data-source policy. It never holds keys.

use crate::appdb::AppDb;
use crate::config::{AppConfig, DataPolicy};
use crate::error::{CoreError, Result};
use crate::explorer::Explorer;
use crate::network::{NetworkProfile, Node, PinnedContract};
use crate::paths::Paths;
use crate::registry::now;
use quai_sdk::contracts::{Contract, Erc20};
use quai_sdk::{BlockTag, QuaiAddress, U256};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// Oldest cached answer ever shown, as a first pass or when the source fails. Older entries
/// wait for the source (and are deleted after 30 days by `AppDb::prune`).
pub const MAX_SERVE_AGE: u64 = 7 * 86_400;

/// How long a caller with nothing to show waits for whoever is already fetching a key.
///
/// Long enough to cover the explorer's slower answers (`/api/stats/assets` is 0.25 s warm and has
/// taken 3-15 s cold), short enough that a leader that died costs one pause and not a screen.
pub const LEADER_WAIT: std::time::Duration = std::time::Duration::from_secs(4);

fn not_cached() -> CoreError {
    CoreError::NotFound("not cached yet".into())
}

/// Sender for read-only calls when no wallet account is involved. go-quai refuses `quai_call`
/// from contract addresses ("sender not an eoa"); this Cyprus-1 Quai-ledger address has no code.
pub const READ_CALLER: &str = "0x0000000000000000000000000000000000000001";

/// Whether a value may be answered from a memo or a cache, or must be read from the chain now.
///
/// A review is the wallet's one honest moment: everything it asserts is read first-hand, every
/// time (`docs/REVIEW_TRUST.md`). That used to be a convention every preparation path happened to
/// keep. [`Trust::FirstHand`] is how a path says so out loud, and it is checked in the two places
/// a remembered answer could otherwise reach a review: [`DataCtx::cached`] and [`verify_pinned`].
///
/// It is a parameter rather than a default so that adding one convenient cache to a preparation
/// path is a visible change at the call site, not a silent one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Trust {
    /// Display data. A cached answer within its freshness window is fine.
    #[default]
    Cached,
    /// Feeding a review. Read it now; never answer from a cache, and never write one.
    FirstHand,
}

impl Trust {
    /// Whether a cache or a memo may answer.
    pub fn may_cache(self) -> bool {
        matches!(self, Trust::Cached)
    }

    /// [`Trust::FirstHand`] when `yes`.
    pub fn first_hand(yes: bool) -> Trust {
        if yes { Trust::FirstHand } else { Trust::Cached }
    }
}

/// A value from a cache-backed lookup.
#[derive(Clone, Debug, Serialize)]
pub struct Cached<T> {
    /// The value.
    pub value: T,
    /// When it was fetched (unix seconds).
    pub fetched_at: u64,
    /// Served from cache after a failed refresh.
    pub stale: bool,
}

/// Releases a fetch lease when dropped.
struct LeaseGuard<'a> {
    store: &'a AppDb,
    key: String,
}

impl Drop for LeaseGuard<'_> {
    fn drop(&mut self) {
        let _ = self.store.release_fetch(&self.key);
    }
}

/// Read-only data context.
pub struct DataCtx {
    /// Network.
    pub network: NetworkProfile,
    /// Node for on-chain reads.
    pub node: Node,
    /// Wallet cache database.
    pub app: AppDb,
    /// The shared display cache, when this context was opened with one.
    ///
    /// Prices, the pool directory, listings, candles and the DEX tape are the same answer for
    /// every wallet, so they live once per data directory rather than once per wallet — which
    /// dedupes them across every wallet *and* every process, since the file is on disk. Only the
    /// feeds in [`crate::appdb::SHARED_FEEDS`] go here, and a review never reads it at all.
    pub shared: Option<AppDb>,
    /// What may be fetched.
    pub policy: DataPolicy,
    /// Explorer backend.
    pub explorer: Explorer,
    /// Answer only from the cache, whatever its age, and make no requests (a first pass that
    /// shows the last known data at once while the real lookup runs).
    pub cache_only: bool,
    /// Reads go through the verified monitoring endpoint.
    pub monitored: bool,
    /// Whether cached answers are allowed at all. A context built for review preparation is
    /// [`Trust::FirstHand`], and then every cache-backed lookup on it reads the chain.
    pub trust: Trust,
}

impl DataCtx {
    /// Open for a wallet on a network, with the data directory's shared display cache.
    pub fn open(paths: &Paths, wallet_id: &str, network: NetworkProfile, config: &AppConfig) -> Result<Self> {
        let app = AppDb::open(&paths.wallet_dir(wallet_id).join("app.sqlite"))?;
        let shared = AppDb::open_shared(&paths.shared_cache()).ok();
        Ok(Self { shared, ..Self::with_app(app, network, config.data_policy())? })
    }

    /// Build from an open database, with no shared cache: every feed answers from this one.
    pub fn with_app(app: AppDb, network: NetworkProfile, policy: DataPolicy) -> Result<Self> {
        let node = network.node()?;
        let explorer = Explorer::for_network(&network);
        Ok(DataCtx { network, node, app, shared: None, policy, explorer, cache_only: false, monitored: false, trust: Trust::Cached })
    }

    /// Build from an open wallet database and an open shared cache.
    pub fn with_stores(app: AppDb, shared: Option<AppDb>, network: NetworkProfile, policy: DataPolicy) -> Result<Self> {
        Ok(Self { shared, ..Self::with_app(app, network, policy)? })
    }

    /// Which store answers a cache key: the shared one for wallet-independent feeds, this
    /// wallet's own for everything else.
    ///
    /// The key is the one the caller passed, before the network prefix — the list in
    /// [`crate::appdb::SHARED_FEEDS`] names feeds, not rows.
    pub fn store(&self, key: &str) -> &AppDb {
        match &self.shared {
            Some(shared) if crate::appdb::is_shared_feed(key) => shared,
            _ => &self.app,
        }
    }

    /// Where the observation feeds live: pool events and the DEX tape.
    ///
    /// They are global trade history — the same trades for everybody — so one copy per data
    /// directory rather than one per wallet, which is also what stops them growing 50× on a
    /// machine with five wallets open.
    pub fn feeds(&self) -> &AppDb {
        self.shared.as_ref().unwrap_or(&self.app)
    }

    /// Switch reads to the network's monitoring endpoint after checking that it reports the
    /// trusted chain id and genesis. On failure reads stay on the main RPC and the reason is
    /// returned for display. Without a monitoring endpoint this does nothing.
    pub async fn use_monitor(&mut self) -> Option<String> {
        let endpoint = self.network.monitor.clone()?;
        // Bounded: an unreachable node must not hold up the first loads.
        let checked = async {
            let node = self.network.monitor_node()?;
            crate::network::require_identity(&self.network, &node.provider).await?;
            Ok::<_, CoreError>(node)
        };
        let checked = tokio::time::timeout(std::time::Duration::from_secs(2), checked)
            .await
            .unwrap_or_else(|_| Err(CoreError::Network("did not answer within 2 s".into())));
        match checked {
            Ok(node) => {
                self.node = node;
                self.monitored = true;
                None
            }
            Err(e) => Some(format!("monitoring endpoint {} not used: {e}", endpoint.rpc_url)),
        }
    }

    /// Refuse network work in a cache-only pass.
    pub fn online(&self) -> Result<()> {
        if self.cache_only { Err(not_cached()) } else { Ok(()) }
    }

    /// Whether reads go through a verified monitoring endpoint.
    pub fn monitoring(&self) -> bool {
        self.monitored
    }

    /// Whether the node answers a cheap read within `timeout`.
    pub async fn node_answers(&self, timeout: std::time::Duration) -> bool {
        matches!(tokio::time::timeout(timeout, self.node.raw("quai_blockNumber", json!([]))).await, Ok(Ok(_)))
    }

    /// A context that reads first-hand: no cache answers it, and none is written from it.
    ///
    /// This is what a review preparation path builds. It is incompatible with `cache_only`, and
    /// clears it rather than silently preferring one of the two.
    pub fn for_review(mut self) -> Self {
        self.trust = Trust::FirstHand;
        self.cache_only = false;
        self
    }

    /// Cache-backed fetch: fresh values within `ttl` seconds are returned without a request;
    /// failures fall back to the last cached value (marked stale).
    ///
    /// On a [`Trust::FirstHand`] context the cache is skipped entirely, in both directions: the
    /// value comes from `fetch`, a failure is an error rather than a stale answer, and nothing is
    /// written that a later review could read instead of the chain.
    pub async fn cached<T, F, Fut>(&self, key: &str, ttl: u64, fetch: F) -> Result<Cached<T>>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        if !self.trust.may_cache() {
            self.online()?;
            return Ok(Cached { value: fetch().await?, fetched_at: now(), stale: false });
        }
        let store = self.store(key);
        let key = format!("{}:{key}", self.network.id);
        let previous = store
            .cache_get(&key)?
            .filter(|(_, at)| now().saturating_sub(*at) < MAX_SERVE_AGE.max(ttl))
            .and_then(|(text, at)| serde_json::from_str::<T>(&text).ok().map(|v| (v, at)));
        if self.cache_only {
            return previous
                .map(|(value, at)| Cached { value, fetched_at: at, stale: now().saturating_sub(at) >= ttl })
                .ok_or_else(not_cached);
        }
        if let Some((_, at)) = &previous
            && now().saturating_sub(*at) < ttl
        {
            let (value, at) = previous.unwrap_or_else(|| unreachable!());
            return Ok(Cached { value, fetched_at: at, stale: false });
        }
        // One refresh per key across every wallet and every process. Two windows opening together
        // used to miss the cache at the same instant and both ask; against a per-IP budget that is
        // how a handful of windows parks background work everywhere.
        let leading = store.claim_fetch(&key).unwrap_or(true);
        // Released however this ends — including the future being dropped mid-fetch, which is what
        // a quit or a cancelled view does — so no lease outlives the lookup that took it.
        let mut lease = leading.then(|| LeaseGuard { store, key: key.clone() });
        if !leading {
            // Somebody else is already refreshing this. Anything servable is better than a second
            // request for the same answer; with nothing to show, wait for theirs before asking.
            if let Some((value, at)) = previous {
                return Ok(Cached { value, fetched_at: at, stale: true });
            }
            let waited = std::time::Instant::now();
            let answer = self.await_leader(store, &key, ttl).await;
            // Which feed waited, never which address: the key's first segment after the network.
            let feed = key.split(':').nth(1).unwrap_or("?");
            crate::diag::timing(&format!("cache.leader_wait.{feed}"), waited);
            if let Some((value, at)) = answer {
                return Ok(Cached { value, fetched_at: at, stale: now().saturating_sub(at) >= ttl });
            }
            // Timing out is not ownership. A slow live leader must not turn every waiting
            // window into another fetcher; if it released the slot, claim it before retrying.
            if !store.claim_fetch(&key)? {
                return Err(CoreError::NotFound("shared feed refresh is still in progress; retry shortly".into()));
            }
            lease = Some(LeaseGuard { store, key: key.clone() });
        }
        let _lease = lease;
        let outcome = fetch().await;
        match outcome {
            Ok(value) => {
                if let Ok(text) = serde_json::to_string(&value) {
                    let _ = store.cache_put(&key, &text);
                }
                Ok(Cached { value, fetched_at: now(), stale: false })
            }
            Err(e) => match previous {
                Some((value, at)) => Ok(Cached { value, fetched_at: at, stale: true }),
                None => Err(e),
            },
        }
    }

    /// Wait for whoever holds a key's refresh to write it, up to [`LEADER_WAIT`].
    ///
    /// Only reached with nothing to show, so waiting costs a screen nothing it was not already
    /// waiting for, and the bound means a leader that dies is a short delay rather than a hang.
    async fn await_leader<T: DeserializeOwned>(&self, store: &AppDb, key: &str, ttl: u64) -> Option<(T, u64)> {
        let deadline = std::time::Instant::now() + LEADER_WAIT;
        while std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
            if let Ok(Some((text, at))) = store.cache_get(key)
                && now().saturating_sub(at) < MAX_SERVE_AGE.max(ttl)
                && let Ok(value) = serde_json::from_str::<T>(&text)
            {
                return Some((value, at));
            }
            // The holder finished or gave up without writing: ask for it ourselves.
            if store.claim_fetch(key).unwrap_or(true) {
                let _ = store.release_fetch(key);
                return None;
            }
        }
        None
    }

    /// Verify a pinned contract's runtime bytecode hash, at this context's trust: a display
    /// context accepts a match remembered within the day, a review context re-reads the code.
    pub async fn verify_pinned(&self, contract: &PinnedContract, what: &str) -> Result<QuaiAddress> {
        self.online()?;
        verify_pinned(&self.app, &self.node, &self.network, contract, what, self.trust).await
    }

    /// Exact ERC-20 balance from the node.
    pub async fn erc20_balance(&self, token: &str, owner: &str) -> Result<U256> {
        self.online()?;
        let token: QuaiAddress = token.parse().map_err(|_| CoreError::Invalid("bad token address".into()))?;
        let owner: QuaiAddress = owner.parse().map_err(|_| CoreError::Invalid("bad owner address".into()))?;
        let caller: QuaiAddress = READ_CALLER.parse().map_err(|_| CoreError::Invalid("read caller".into()))?;
        Ok(Erc20::new(token, &self.node.provider)?.balance_of(caller, owner, BlockTag::Latest).await?)
    }

    /// Current ERC-721 owner from the node.
    pub async fn erc721_owner(&self, contract: &str, token_id: &str, caller: &str) -> Result<String> {
        self.online()?;
        let (c, caller) = nft_contract(self, contract, caller)?;
        let out = c.call(caller, "ownerOf", &[json!(token_id)], BlockTag::Latest).await?;
        out.first().and_then(Value::as_str).map(str::to_lowercase).ok_or_else(|| CoreError::Network("ownerOf returned nothing".into()))
    }

    /// ERC-1155 balance from the node.
    pub async fn erc1155_balance(&self, contract: &str, token_id: &str, owner: &str) -> Result<U256> {
        self.online()?;
        let (c, caller) = nft_contract(self, contract, READ_CALLER)?;
        let out = c.call(caller, "balanceOf", &[json!(owner), json!(token_id)], BlockTag::Latest).await?;
        out.first()
            .and_then(Value::as_str)
            .and_then(|t| U256::from_str_radix(t, 10).ok())
            .ok_or_else(|| CoreError::Network("balanceOf returned nothing".into()))
    }

    /// ERC-20 metadata read on-chain (symbol, name, decimals).
    pub async fn erc20_metadata(&self, token: &str, caller: &str) -> Result<(String, String, u8)> {
        self.erc20_metadata_at(token, caller, BlockTag::Latest).await
    }

    pub(crate) async fn erc20_metadata_at(&self, token: &str, caller: &str, block: BlockTag) -> Result<(String, String, u8)> {
        self.online()?;
        let token: QuaiAddress = token.parse().map_err(|_| CoreError::Invalid("bad token address".into()))?;
        let caller: QuaiAddress = caller.parse().map_err(|_| CoreError::Invalid("bad caller address".into()))?;
        let erc = Erc20::new(token, &self.node.provider)?;
        let text = |v: Vec<Value>| v.first().and_then(Value::as_str).map(crate::ops::sanitize_display);
        let symbol = erc.contract().call(caller, "symbol", &[], block).await.ok().and_then(text).unwrap_or_else(|| "TOKEN".into());
        let name = erc.contract().call(caller, "name", &[], block).await.ok().and_then(text).unwrap_or_else(|| symbol.clone());
        let decimals = erc
            .contract()
            .call(caller, "decimals", &[], block)
            .await?
            .first()
            .and_then(Value::as_str)
            .and_then(|d| d.parse::<u8>().ok())
            .filter(|d| *d <= 77)
            .ok_or_else(|| CoreError::Invalid("token decimals invalid".into()))?;
        Ok((symbol, name, decimals))
    }
}

/// Attach the node-discovered access list to a contract call. go-quai charges cold-access gas
/// for accounts missing from the signed list (router → pairs → tokens, marketplace → helpers →
/// collections), so multi-contract calls without one run out of gas on-chain even though they
/// simulate fine. The list is part of the reviewed payload and shown in the review.
pub async fn with_access_list<T: quai_sdk::rpc::Transport>(
    provider: &quai_sdk::Provider<T>,
    from: QuaiAddress,
    call: quai_sdk::contracts::ContractCall,
) -> Result<quai_sdk::contracts::ContractCall> {
    let mut request = quai_sdk::provider::CallRequest::new(from, call.destination());
    request.value = Some(call.value());
    request.input = call.data().clone();
    let estimate = provider.create_access_list(&request, BlockTag::Latest).await?;
    let list = estimate
        .access_list
        .into_iter()
        .map(|item| quai_sdk::consensus::AccessTuple { address: item.address, storage_keys: item.storage_keys })
        .collect();
    Ok(call.with_access_list(list)?)
}

/// How long a verified bytecode pin is trusted before it is read from the chain again, on a
/// display path. A review path ([`Trust::FirstHand`]) never uses the memo at all.
pub const PIN_RECHECK_SECS: u64 = 86_400;

/// Whether a remembered pin match (its unix time, as stored) is recent enough to trust.
fn pin_fresh(checked_at: Option<&str>, now: u64) -> bool {
    checked_at.and_then(|at| at.parse::<u64>().ok()).is_some_and(|at| now.saturating_sub(at) < PIN_RECHECK_SECS)
}

/// Verify a pinned contract's runtime bytecode hash.
///
/// The address this returns is a call destination: everything the wallet is willing to transact
/// with — routers, factories, gauges, the curve launcher, the marketplace modules, the message
/// board — comes through here. This chain still has SELFDESTRUCT, so code at an address can be
/// replaced after it was checked, which makes a remembered match a time-of-check/time-of-use
/// window as wide as the memo's lifetime.
///
/// So the memo is a display convenience only: under [`Trust::Cached`] a match is remembered for
/// [`PIN_RECHECK_SECS`], and under [`Trust::FirstHand`] — anything feeding a review — the code is
/// read from the chain every time. A first-hand match still refreshes the memo, since it is a
/// stronger observation than the one the memo holds.
pub async fn verify_pinned(
    app: &AppDb,
    node: &Node,
    network: &NetworkProfile,
    contract: &PinnedContract,
    what: &str,
    trust: Trust,
) -> Result<QuaiAddress> {
    let address: QuaiAddress =
        contract.address.parse().map_err(|_| CoreError::Invalid(format!("{what} address is not a Cyprus-1 Quai address")))?;
    let Some(hash) = &contract.code_hash else { return Ok(address) };
    let key = format!("pinned:{}:{}:{}", network.id, contract.address.to_lowercase(), hash.to_lowercase());
    // A match is trusted for a day on a display path, not forever, and not at all when the address
    // is about to appear in a review.
    if trust.may_cache() && pin_fresh(app.kv(&key)?.as_deref(), now()) {
        return Ok(address);
    }
    let expected: quai_sdk::primitives::Hash32 =
        hash.parse().map_err(|_| CoreError::Invalid(format!("{what} pinned hash is malformed")))?;
    let c = Contract::new(address, quai_sdk::abi::AbiInterface::default(), &node.provider);
    let mut attempt = 0;
    loop {
        attempt += 1;
        match c.verify_deployment(network.genesis_hash()?, Some(expected), BlockTag::Latest).await {
            Ok(_) => {
                app.set_kv(&key, &now().to_string())?;
                return Ok(address);
            }
            Err(quai_sdk::contracts::ContractError::RuntimeMismatch) => {
                return Err(CoreError::Rejected(format!(
                    "{what} at {} does not match its pinned bytecode; refusing to use it",
                    contract.address
                )));
            }
            Err(quai_sdk::contracts::ContractError::MissingCode) => {
                return Err(CoreError::Rejected(format!("{what} has no code at {}", contract.address)));
            }
            // A new block during the observation is transient.
            Err(_) if attempt < 4 => tokio::time::sleep(std::time::Duration::from_millis(200)).await,
            Err(e) => return Err(CoreError::Network(format!("could not verify {what}: {e}"))),
        }
    }
}

/// Verify several pinned contracts at once, in the order given.
///
/// One verification is a genesis read, a head read, the code read and a re-read of the head to
/// prove nothing moved underneath it — three or four round trips, which is ~140 ms against the
/// public RPC and ~4 ms against a node on the LAN. A review path repeats all of them rather than
/// trusting a memo, so doing them one after another is most of what a swap review spends before
/// it can draw. They are independent reads of the same node, so they run together.
pub async fn verify_pinned_all(
    app: &AppDb,
    node: &Node,
    network: &NetworkProfile,
    pins: &[(&PinnedContract, &str)],
    trust: Trust,
) -> Result<Vec<QuaiAddress>> {
    futures::future::try_join_all(pins.iter().map(|(pin, what)| verify_pinned(app, node, network, pin, what, trust))).await
}

/// Minimal NFT ABI (ERC-721 + ERC-1155 reads and transfers).
pub const NFT_ABI: &[&str] = &[
    "function ownerOf(uint256 tokenId) view returns (address)",
    "function balanceOf(address owner, uint256 id) view returns (uint256)",
    "function isApprovedForAll(address owner, address operator) view returns (bool)",
    "function getApproved(uint256 tokenId) view returns (address)",
    "function setApprovalForAll(address operator, bool approved)",
    "function safeTransferFrom(address from, address to, uint256 tokenId)",
    "function tokenURI(uint256 tokenId) view returns (string)",
];

/// ERC-1155 transfer ABI (separate interface: its `safeTransferFrom` overload differs).
pub const ERC1155_ABI: &[&str] = &[
    "function balanceOf(address owner, uint256 id) view returns (uint256)",
    "function safeTransferFrom(address from, address to, uint256 id, uint256 amount, bytes data)",
];

fn nft_contract<'a>(
    ctx: &'a DataCtx,
    contract: &str,
    caller: &str,
) -> Result<(Contract<'a, crate::network::WalletTransport>, QuaiAddress)> {
    let address: QuaiAddress = contract.parse().map_err(|_| CoreError::Invalid("bad collection address".into()))?;
    let caller: QuaiAddress = caller.parse().map_err(|_| CoreError::Invalid("bad owner address".into()))?;
    let abi = quai_sdk::abi::AbiInterface::from_human_readable(&NFT_ABI[..4]).map_err(|e| CoreError::Invalid(format!("abi: {e}")))?;
    Ok((Contract::new(address, abi, &ctx.node.provider), caller))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_slow_live_cache_leader_does_not_turn_followers_into_unleased_fetchers() {
        let ctx = DataCtx::with_app(AppDb::memory().unwrap(), crate::network::NetworkProfile::builtins()[1].clone(), DataPolicy::OFFLINE)
            .unwrap();
        let key = format!("{}:slow", ctx.network.id);
        assert!(ctx.app.claim_fetch(&key).unwrap());
        let answer = ctx.cached::<String, _, _>("slow", 60, || async { panic!("another live process still owns the fetch") }).await;
        assert!(answer.unwrap_err().to_string().contains("still in progress"));
        assert!(!ctx.app.claim_fetch(&key).unwrap(), "the follower did not release the leader's slot");
        ctx.app.release_fetch(&key).unwrap();
        assert_eq!(ctx.cached("slow", 60, || async { Ok("recovered".to_owned()) }).await.unwrap().value, "recovered");
    }

    /// A bytecode match is trusted for a day; an entry from an older version (or garbage) is not.
    #[test]
    fn a_pinned_match_expires() {
        let now = 1_800_000_000;
        assert!(pin_fresh(Some(&(now - 60).to_string()), now));
        assert!(!pin_fresh(Some(&(now - PIN_RECHECK_SECS).to_string()), now));
        assert!(!pin_fresh(Some("yes"), now) && !pin_fresh(None, now));
    }

    #[tokio::test]
    async fn cache_serves_fresh_then_stale() {
        let network = crate::network::NetworkProfile::builtins()[1].clone();
        let ctx = DataCtx::with_app(AppDb::memory().unwrap(), network, DataPolicy::OFFLINE).unwrap();
        let first = ctx.cached("k", 60, || async { Ok(41u32) }).await.unwrap();
        assert_eq!((first.value, first.stale), (41, false));
        // Within the TTL the fetcher is not called.
        let second = ctx.cached("k", 60, || async { Err::<u32, _>(CoreError::Network("down".into())) }).await.unwrap();
        assert_eq!(second.value, 41);
        // Past the TTL a failure serves the old value, marked stale.
        let third = ctx.cached("k", 0, || async { Err::<u32, _>(CoreError::Network("down".into())) }).await.unwrap();
        assert_eq!((third.value, third.stale), (41, true));
        assert!(ctx.cached("missing", 0, || async { Err::<u32, _>(CoreError::Network("down".into())) }).await.is_err());
        // A cache-only pass answers from the cache whatever its age and never fetches.
        let cache = DataCtx { cache_only: true, ..ctx };
        let held = cache.cached("k", 0, || async { panic!("no fetch in a cache-only pass") }).await.unwrap();
        assert_eq!((held.value, held.stale), (41u32, true));
        assert!(cache.cached("missing", 0, || async { Ok(1u32) }).await.is_err());
        assert!(cache.erc20_balance(READ_CALLER, READ_CALLER).await.is_err());
        // Nothing older than the serving limit is shown, even when the source fails.
        let key = format!("{}:ancient", cache.network.id);
        cache.app.cache_put(&key, "5").unwrap();
        cache.app.conn_for_tests().execute("UPDATE cache SET fetched = 1 WHERE key = ?1", [&key]).unwrap();
        assert!(cache.cached("ancient", 60, || async { Ok(6u32) }).await.is_err());
    }

    /// The guard rail itself: on a first-hand context no cache answers a lookup, a failure is an
    /// error rather than a quietly stale value, and nothing is written for a later review to read.
    #[tokio::test]
    async fn a_review_context_neither_reads_nor_writes_the_cache() {
        let network = crate::network::NetworkProfile::builtins()[1].clone();
        let ctx = DataCtx::with_app(AppDb::memory().unwrap(), network, DataPolicy::OFFLINE).unwrap().for_review();
        // Seed the cache with a value a display pass would happily serve.
        ctx.app.cache_put(&format!("{}:balance", ctx.network.id), "\"999\"").unwrap();
        // The seeded value is never returned: the fetcher answers instead.
        let fresh = ctx.cached("balance", 86_400, || async { Ok("7".to_string()) }).await.unwrap();
        assert_eq!(fresh.value, "7");
        // And the fresh answer is not written back, so a later review cannot read it either.
        let stored = ctx.app.cache_get(&format!("{}:balance", ctx.network.id)).unwrap().map(|(t, _)| t);
        assert_eq!(stored.as_deref(), Some("\"999\""), "a review must leave the display cache alone");
        // A source failure errors rather than falling back to the seeded value.
        let failed = ctx.cached::<String, _, _>("balance", 86_400, || async { Err(CoreError::Network("down".into())) }).await;
        assert!(failed.is_err(), "a review shows the error, never the cached value");
        // `for_review` and `cache_only` cannot both hold: a review is never answered from disk.
        let confused =
            DataCtx { cache_only: true, ..DataCtx::with_app(AppDb::memory().unwrap(), ctx.network.clone(), ctx.policy).unwrap() }
                .for_review();
        assert!(!confused.cache_only && confused.trust == Trust::FirstHand);
    }

    /// A remembered bytecode match is a display convenience. With the node unreachable a display
    /// path still answers from the memo; a review path fails instead of trusting it.
    #[tokio::test]
    async fn a_pin_memo_answers_a_display_path_and_never_a_review() {
        let mut network = crate::network::NetworkProfile::builtins()[0].clone();
        // Nothing listens here, so any read of the chain fails fast and visibly.
        network.rpc_url = "http://127.0.0.1:1".into();
        network.use_pathing = false;
        let app = AppDb::memory().unwrap();
        let node = network.node().unwrap();
        let contract = PinnedContract {
            address: "0x0018a110b6ca369dcf5ab062c72f049e93b9ede2".into(),
            code_hash: Some("0x00000000000000000000000000000000000000000000000000000000000000ff".into()),
        };
        let key = format!("pinned:{}:{}:{}", network.id, contract.address, contract.code_hash.clone().unwrap());
        app.set_kv(&key, &now().to_string()).unwrap();
        // Display: the memo answers, and the dead node is never asked.
        let shown = verify_pinned(&app, &node, &network, &contract, "factory", Trust::Cached).await;
        assert_eq!(shown.unwrap().to_string().to_lowercase(), contract.address);
        // A review: the code is read, the read fails, and the operation fails with it.
        let review = verify_pinned(&app, &node, &network, &contract, "factory", Trust::FirstHand).await;
        assert!(review.is_err(), "a review must not accept a remembered match");
    }

    /// A wallet-independent feed is fetched once for the whole data directory, and everything
    /// that is any wallet's own stays in that wallet's own file.
    #[tokio::test]
    async fn one_wallet_fetches_a_feed_and_the_next_one_finds_it() {
        let network = crate::network::NetworkProfile::builtins()[1].clone();
        let shared_path = tempfile::tempdir().unwrap();
        let shared_path = shared_path.path().join("shared.sqlite");
        let open = || {
            DataCtx::with_stores(
                AppDb::memory().unwrap(),
                Some(AppDb::open_shared(&shared_path).unwrap()),
                network.clone(),
                DataPolicy::OFFLINE,
            )
            .unwrap()
        };
        let first = open();
        let got = first.cached("prices", 60, || async { Ok("0.0086".to_string()) }).await.unwrap();
        assert_eq!(got.value, "0.0086");
        // A different wallet, its own `app.sqlite`, and the price is already there.
        let second = open();
        let reused =
            second.cached::<String, _, _>("prices", 60, || async { panic!("the second wallet must not fetch it again") }).await.unwrap();
        assert_eq!(reused.value, "0.0086");
        // It went to the shared file, not to either wallet's own.
        assert!(second.app.cache_get(&format!("{}:prices", second.network.id)).unwrap().is_none());
        assert!(second.shared.as_ref().unwrap().cache_get(&format!("{}:prices", second.network.id)).unwrap().is_some());
        // A holdings lookup is this wallet's own and is not shared, even though it is the same
        // kind of cached answer.
        let key = format!("holdings:{}", crate::data::READ_CALLER);
        first.cached(&key, 60, || async { Ok("7".to_string()) }).await.unwrap();
        assert!(first.app.cache_get(&format!("{}:{key}", first.network.id)).unwrap().is_some(), "it stayed with the wallet");
        assert!(first.shared.as_ref().unwrap().cache_get(&format!("{}:{key}", first.network.id)).unwrap().is_none());
        let fresh = open();
        assert!(fresh.cached::<String, _, _>(&key, 60, || async { Err(CoreError::Network("offline".into())) }).await.is_err());
        // And a review reads neither store: the shared cache is display data and nothing else.
        let review = open().for_review();
        let asserted = review.cached("prices", 86_400, || async { Ok("0.0091".to_string()) }).await.unwrap();
        assert_eq!(asserted.value, "0.0091", "a review reads the source, not the shared row");
        let stored = review.shared.as_ref().unwrap().cache_get(&format!("{}:prices", review.network.id)).unwrap();
        assert_eq!(stored.map(|(t, _)| t).as_deref(), Some("\"0.0086\""), "and writes nothing back to it");
    }

    /// Two wallets that miss the same feed at the same instant make one request, not two.
    ///
    /// This is the shape that matters: windows opened together all reach the source in the same
    /// tenth of a second, and against a budget shared per IP that is what parks background work
    /// everywhere. The one holding nothing waits for the answer rather than asking again.
    #[tokio::test]
    async fn concurrent_wallets_make_one_request_for_one_feed() {
        let network = crate::network::NetworkProfile::builtins()[1].clone();
        let dir = tempfile::tempdir().unwrap();
        let shared_path = dir.path().join("shared.sqlite");
        let open = || {
            DataCtx::with_stores(
                AppDb::memory().unwrap(),
                Some(AppDb::open_shared(&shared_path).unwrap()),
                network.clone(),
                DataPolicy::OFFLINE,
            )
            .unwrap()
        };
        let (first, second) = (open(), open());
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = |calls: std::sync::Arc<std::sync::atomic::AtomicUsize>| async move {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
            Ok("0.0086".to_string())
        };
        let (a, b) = futures::future::join(first.cached("prices", 60, || count(calls.clone())), async {
            // A tick later, the way a second window starts.
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            second.cached("prices", 60, || count(calls.clone())).await
        })
        .await;
        assert_eq!(a.unwrap().value, "0.0086");
        assert_eq!(b.unwrap().value, "0.0086", "the second wallet got the first one's answer");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1, "one request for one feed");

        // With something already cached, the follower serves that instead of waiting at all.
        let third = open();
        let before = std::time::Instant::now();
        third.app.set_kv("unused", "").unwrap();
        third.shared.as_ref().unwrap().claim_fetch(&format!("{}:prices", third.network.id)).unwrap();
        let held = third.cached::<String, _, _>("prices", 0, || async { panic!("no second request") }).await.unwrap();
        assert_eq!(held.value, "0.0086");
        assert!(held.stale, "a follower cannot label an expired snapshot fresh");
        assert!(before.elapsed() < LEADER_WAIT, "and does not wait for the leader it has an answer for");

        // A lease whose holder never writes is taken over rather than waited out forever.
        let fourth = open();
        let key = format!("{}:token_markets", fourth.network.id);
        fourth.shared.as_ref().unwrap().claim_fetch(&key).unwrap();
        fourth.shared.as_ref().unwrap().release_fetch(&key).unwrap();
        let own = fourth.cached("token_markets", 60, || async { Ok("mine".to_string()) }).await.unwrap();
        assert_eq!(own.value, "mine");
    }

    #[test]
    fn nft_abis_parse() {
        quai_sdk::abi::AbiInterface::from_human_readable(NFT_ABI).unwrap();
        quai_sdk::abi::AbiInterface::from_human_readable(ERC1155_ABI).unwrap();
    }
}
