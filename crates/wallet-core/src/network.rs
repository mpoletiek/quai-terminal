//! Network profiles, provider construction and node identity checks.

use crate::error::{CoreError, Result};
use quai_sdk::accounts::{AccountObservationPolicy, FeePolicy};
use quai_sdk::primitives::Hash32;
use quai_sdk::wallet::storage::NetworkScope;
use quai_sdk::{HttpConfig, HttpProxy, HttpTransport, Provider, Routing, U256, Zone};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// The single zone supported by this release.
pub const ZONE: Zone = Zone::Cyprus1;

/// Provider type used throughout the wallet.
pub type WalletProvider = Provider<WalletTransport>;

/// How long a node's identity answers (chain ID, genesis header) are reused.
const IDENTITY_TTL: Duration = Duration::from_secs(300);

/// The SDK's HTTP transport plus two things it lacks: identity answers the SDK re-reads before
/// every preparation (chain ID, genesis header) are reused for [`IDENTITY_TTL`] per endpoint,
/// and `QW_RPC_LOG=<file>` traces each call (unix ms, thread, host, method, ms taken, ok/err).
/// Nothing else is cached: balances, nonces, gas and simulation always go to the node.
#[derive(Clone)]
pub struct WalletTransport {
    inner: HttpTransport,
    identity: std::sync::Arc<std::sync::Mutex<IdentityCache>>,
}

/// (endpoint, answer kind) → (when read, answer).
type IdentityCache = std::collections::HashMap<(String, &'static str), (std::time::Instant, serde_json::Value)>;

impl WalletTransport {
    fn new(inner: HttpTransport) -> Self {
        WalletTransport { inner, identity: Default::default() }
    }

    /// Cache slot for identity reads: `quai_chainId` and the genesis header.
    fn identity_key(method: &str, params: &serde_json::Value) -> Option<&'static str> {
        match method {
            "quai_chainId" => Some("chain"),
            "quai_getHeaderByNumber" if params.get(0).and_then(|v| v.as_str()) == Some("0x0") => Some("genesis"),
            _ => None,
        }
    }
}

fn rpc_log() -> Option<&'static std::sync::Mutex<std::fs::File>> {
    use std::sync::{Mutex, OnceLock};
    static LOG: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();
    LOG.get_or_init(|| {
        std::env::var_os("QW_RPC_LOG").and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok()).map(Mutex::new)
    })
    .as_ref()
}

fn rpc_trace(endpoint: &quai_sdk::Endpoint, method: &str, started: std::time::Instant, ok: bool) {
    let Some(log) = rpc_log() else { return };
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let host = reqwest::Url::parse(endpoint.as_str()).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
    // Which thread asked: the TUI runs a wallet worker and a data worker with their own runtimes,
    // and a per-method histogram is unattributable without knowing which of them made the call.
    let thread = std::thread::current().name().unwrap_or("?").to_string();
    if let Ok(mut f) = log.lock() {
        use std::io::Write;
        let _ = writeln!(f, "{ms} {thread} {host} {method} {} {}", started.elapsed().as_millis(), if ok { "ok" } else { "err" });
    }
}

impl quai_sdk::rpc::Transport for WalletTransport {
    async fn request_batch(
        &self,
        endpoint: &quai_sdk::Endpoint,
        requests: Vec<(&str, serde_json::Value)>,
    ) -> Option<quai_sdk::rpc::BatchResult> {
        // A batch is logged by the methods in it (`a+b+c`), and only when the log is on: which
        // calls went to which endpoint is the question the log exists to answer.
        let names = rpc_log().map(|_| requests.iter().map(|(m, _)| *m).collect::<Vec<_>>().join("+"));
        let started = std::time::Instant::now();
        let result = self.inner.request_batch(endpoint, requests).await;
        rpc_trace(endpoint, names.as_deref().unwrap_or("batch"), started, matches!(result, Some(Ok(_))));
        result
    }

    async fn request(
        &self,
        endpoint: &quai_sdk::Endpoint,
        method: &str,
        params: serde_json::Value,
    ) -> std::result::Result<serde_json::Value, quai_sdk::rpc::RpcError> {
        let slot = Self::identity_key(method, &params).map(|k| (endpoint.as_str().to_string(), k));
        if let Some(key) = &slot
            && let Ok(cache) = self.identity.lock()
            && let Some((at, value)) = cache.get(key)
            && at.elapsed() < IDENTITY_TTL
        {
            return Ok(value.clone());
        }
        let started = std::time::Instant::now();
        let result = self.inner.request(endpoint, method, params).await;
        rpc_trace(endpoint, method, started, result.is_ok());
        if let (Some(key), Ok(value)) = (slot, &result)
            && let Ok(mut cache) = self.identity.lock()
        {
            cache.insert(key, (std::time::Instant::now(), value.clone()));
        }
        result
    }
}

const QUAI: u128 = 1_000_000_000_000_000_000;

/// A network the wallet can connect to. Identity is chain ID plus trusted genesis.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct NetworkProfile {
    /// Short identifier used on the command line (`mainnet`, `orchard`, ...).
    pub id: String,
    /// Display name.
    pub name: String,
    /// Expected chain ID.
    pub chain_id: u64,
    /// Trusted genesis hash (0x-prefixed hex).
    pub genesis: String,
    /// JSON-RPC URL. With `use_pathing` this is the gateway base; otherwise the exact Cyprus-1 endpoint.
    pub rpc_url: String,
    /// Derive `/cyprus1` from a gateway base.
    #[serde(default)]
    pub use_pathing: bool,
    /// Optional WebSocket endpoint for head following.
    #[serde(default)]
    pub ws_url: Option<String>,
    /// Use pinned-latest observations for account preparation (nodes without pending reads).
    #[serde(default = "yes")]
    pub pinned_latest: bool,
    /// Node supports the go-quai v0.56 SHA-anchored specialized Qi fee estimator.
    #[serde(default)]
    pub specialized_fee_estimation: bool,
    /// WQI contract address, when deployed.
    #[serde(default)]
    pub wqi: Option<String>,
    /// WQUAI contract address, when deployed.
    #[serde(default)]
    pub wquai: Option<String>,
    /// Read-only monitoring endpoint (set with `network monitor`; stored in the app config).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor: Option<MonitorEndpoint>,
    /// Pelagus payment-code mailbox contract, when deployed.
    #[serde(default)]
    pub mailbox: Option<String>,
    /// Block explorer base URL.
    #[serde(default)]
    pub explorer: Option<String>,
    /// Explorer API used for portfolio, NFT and history lookups (none: chain-only).
    #[serde(default)]
    pub explorer_api: Option<ExplorerApiConfig>,
    /// Pinned ecosystem contracts (swaps, NFT market) and indexers.
    #[serde(default)]
    pub ecosystem: Ecosystem,
    /// Maximum gas price accepted by default fee policies (wei per gas).
    #[serde(default = "default_gas_price_cap")]
    pub max_gas_price: String,
    /// Maximum total account fee by default (base units).
    #[serde(default = "default_total_fee_cap")]
    pub max_total_fee: String,
    /// Built-in profile (not persisted to config).
    #[serde(skip)]
    pub builtin: bool,
}

pub use quai_feeds::explorer::{ExplorerApiConfig, ExplorerKind};

/// A separate JSON-RPC endpoint every read goes to once it reports the network's chain id and
/// genesis. Broadcasts never use it: a monitoring node has no hashrate behind it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MonitorEndpoint {
    /// JSON-RPC URL (exact Cyprus-1 endpoint, or a gateway base with `use_pathing`).
    pub rpc_url: String,
    /// Derive `/cyprus1` from a gateway base.
    #[serde(default)]
    pub use_pathing: bool,
}

pub use quai_venues::pins::{Ecosystem, PinnedContract};

/// A pin's trust label as a particular node sees it.
pub trait PinTrust {
    /// Review label, stronger when the node's view was confirmed by a second node.
    fn trust_label_on(&self, node: &Node) -> &'static str;
}

impl PinTrust for PinnedContract {
    /// The review label for a pin just checked through `node`: it says so when the code was
    /// proven at a block the network's RPC confirmed, which a lying monitor cannot fake.
    fn trust_label_on(&self, node: &Node) -> &'static str {
        match (self.code_hash.is_some(), node.anchor_confirmation()) {
            (true, Some(crate::anchor::Confirmation::Witnessed)) => "✓ pinned bytecode, confirmed by two nodes",
            _ => self.trust_label(),
        }
    }
}

fn yes() -> bool {
    true
}
fn default_gas_price_cap() -> String {
    "100000000000000".into()
}
fn default_total_fee_cap() -> String {
    (25 * QUAI).to_string()
}

impl NetworkProfile {
    /// Built-in Mainnet and Orchard profiles.
    pub fn builtins() -> Vec<NetworkProfile> {
        vec![
            NetworkProfile {
                id: "mainnet".into(),
                name: "Quai Mainnet".into(),
                chain_id: 9,
                genesis: "0xac81c28f1a72591b87b5f16c9793cdc0e87c45c6d426d1a364c3b8f6386b5b8b".into(),
                rpc_url: "https://rpc.quai.network".into(),
                use_pathing: true,
                ws_url: None,
                pinned_latest: true,
                specialized_fee_estimation: true,
                wqi: Some(quai_sdk::wrappers::WQI_ADDRESS.into()),
                wquai: Some(quai_sdk::wrappers::WQUAI_MAINNET_ADDRESS.into()),
                monitor: None,
                mailbox: Some(quai_sdk::payment_mailbox::PELAGUS_MAILBOX_ADDRESS.into()),
                explorer: Some("https://explorer.qu.ai".into()),
                explorer_api: Some(ExplorerApiConfig { kind: ExplorerKind::QuaiExplorer, base_url: "https://explorer.qu.ai".into() }),
                ecosystem: Ecosystem::mainnet(),
                max_gas_price: "100000000000000".into(),
                max_total_fee: (25 * QUAI).to_string(),
                builtin: true,
            },
            NetworkProfile {
                id: "orchard".into(),
                name: "Orchard Testnet".into(),
                chain_id: 15000,
                genesis: "0x663a73416275109a01aad3a4c29ea9e310aded63c5eea491243b7312ad8cd16b".into(),
                rpc_url: "https://orchard.rpc.quai.network".into(),
                use_pathing: true,
                ws_url: None,
                pinned_latest: true,
                specialized_fee_estimation: false,
                wqi: Some(quai_sdk::wrappers::WQI_ADDRESS.into()),
                wquai: Some(quai_sdk::wrappers::WQUAI_ORCHARD_ADDRESS.into()),
                monitor: None,
                mailbox: Some(quai_sdk::payment_mailbox::PELAGUS_MAILBOX_ADDRESS.into()),
                explorer: Some("https://orchard.quaiscan.io".into()),
                explorer_api: Some(ExplorerApiConfig { kind: ExplorerKind::Blockscout, base_url: "https://orchard.quaiscan.io".into() }),
                ecosystem: Ecosystem::default(),
                max_gas_price: "1000000000000".into(),
                max_total_fee: (QUAI).to_string(),
                builtin: true,
            },
        ]
    }

    /// Parsed genesis hash.
    pub fn genesis_hash(&self) -> Result<Hash32> {
        self.genesis.parse().map_err(|_| CoreError::Invalid(format!("network `{}` has an invalid genesis hash", self.id)))
    }

    /// Whether reads here are proven against block headers. Only on networks this wallet ships
    /// contract pins for: those are go-quai chains whose headers quai-sdk can hash. A custom or
    /// development chain keeps plain reads.
    pub fn proves_state(&self) -> bool {
        self.ecosystem.wquai_code_hash.is_some()
    }

    /// Storage scope for SDK stores.
    pub fn scope(&self) -> Result<NetworkScope> {
        Ok(NetworkScope { chain_id: U256::from(self.chain_id), genesis: self.genesis_hash()?, zone: ZONE })
    }

    /// Build an HTTP provider. No network access happens here.
    pub fn provider(&self) -> Result<WalletProvider> {
        Ok(self.node()?.provider)
    }

    /// Build a provider plus raw transport access for diagnostics.
    pub fn node(&self) -> Result<Node> {
        let mut config = HttpConfig::default().with_timeout(Duration::from_secs(20));
        if let Some(proxy) = rpc_proxy(&self.rpc_url, crate::http::proxy()) {
            config = config.with_proxy(Some(HttpProxy::parse(proxy).map_err(|e| CoreError::Invalid(format!("proxy for node RPC: {e}")))?));
        }
        let transport = WalletTransport::new(HttpTransport::new(config).map_err(|e| CoreError::Network(format!("http transport: {e}")))?);
        let routing = Routing::with_pathing(&self.rpc_url, ZONE.into(), self.use_pathing)?;
        let endpoint = routing.endpoint(ZONE.into())?.clone();
        Ok(Node {
            provider: Provider::new(transport.clone(), routing, U256::from(self.chain_id)),
            transport,
            endpoint,
            witness: None,
            anchor: std::sync::Arc::default(),
        })
    }

    /// Node for read-only monitoring: the monitoring endpoint when one is set, else the main RPC.
    /// Callers verify its identity with [`require_identity`] before trusting its answers.
    pub fn monitor_node(&self) -> Result<Node> {
        match &self.monitor {
            Some(m) => NetworkProfile { rpc_url: m.rpc_url.clone(), use_pathing: m.use_pathing, monitor: None, ..self.clone() }.node(),
            None => self.node(),
        }
    }

    /// Observation policy for account preparation.
    pub fn observation_policy(&self) -> AccountObservationPolicy {
        if self.pinned_latest { AccountObservationPolicy::PinnedLatest } else { AccountObservationPolicy::Pending }
    }

    /// Limits the SDK enforces while preparing a transaction. They are technical ceilings, wide
    /// enough that a real transaction is never refused on a guess: the actual gas limit is the
    /// estimate plus a margin, and the fee policy below is compared afterwards and shown on the
    /// review for the user to decide. A fee cap the user typed (`explicit_cap`) stays hard.
    pub fn preparation_limits(&self, gas_hint: u64, explicit_cap: Option<U256>) -> Result<FeePolicy> {
        let policy = self.fee_policy(gas_hint)?;
        Ok(FeePolicy::new(
            gas_hint.saturating_mul(4).clamp(1_000_000, 20_000_000),
            policy.max_gas_price.saturating_mul(U256::from(20)),
            explicit_cap.unwrap_or_else(|| policy.max_total_fee.saturating_mul(U256::from(40))),
        )
        .with_gas_margin_bps(policy.gas_margin_bps))
    }

    /// Why a prepared fee is above the network fee policy, if it is (shown on the review).
    pub fn fee_policy_note(&self, gas_price: U256, max_fee: U256) -> Option<String> {
        let policy = self.fee_policy(0).ok()?;
        let quai = |v: U256| crate::amount::quai(v);
        if max_fee > policy.max_total_fee {
            Some(format!(
                "maximum fee {} QUAI is above your fee policy ({} QUAI); approving sends it anyway",
                quai(max_fee),
                quai(policy.max_total_fee)
            ))
        } else if gas_price > policy.max_gas_price {
            Some(format!(
                "gas price {} gwei is above your fee policy ({} gwei); the network is busy",
                crate::amount::format_amount(gas_price, 9),
                crate::amount::format_amount(policy.max_gas_price, 9)
            ))
        } else {
            None
        }
    }

    /// The network fee policy (compared on reviews; see `preparation_limits`).
    pub fn fee_policy(&self, max_gas: u64) -> Result<FeePolicy> {
        Ok(FeePolicy::new(max_gas, parse_u256(&self.max_gas_price, "max_gas_price")?, parse_u256(&self.max_total_fee, "max_total_fee")?)
            .with_gas_margin_bps(1000))
    }

    /// Explorer URL for a transaction hash.
    pub fn tx_url(&self, hash: &str) -> Option<String> {
        self.explorer.as_ref().map(|e| format!("{}/tx/{hash}", e.trim_end_matches('/')))
    }

    /// Explorer URL for a token or NFT collection.
    pub fn token_url(&self, address: &str) -> Option<String> {
        self.explorer.as_ref().map(|e| format!("{}/token/{address}", e.trim_end_matches('/')))
    }

    /// Explorer URL for an address.
    pub fn address_url(&self, address: &str) -> Option<String> {
        self.explorer.as_ref().map(|e| format!("{}/address/{address}", e.trim_end_matches('/')))
    }

    /// Validate profile fields for a user network.
    pub fn validate(&self) -> Result<()> {
        crate::paths::validate_id(&self.id)?;
        self.genesis_hash()?;
        if !(self.rpc_url.starts_with("http://") || self.rpc_url.starts_with("https://")) {
            return Err(CoreError::Invalid("rpc url must start with http:// or https://".into()));
        }
        parse_u256(&self.max_gas_price, "max_gas_price")?;
        parse_u256(&self.max_total_fee, "max_total_fee")?;
        Routing::with_pathing(&self.rpc_url, ZONE.into(), self.use_pathing)?;
        Ok(())
    }
}

/// The proxy node RPC to `rpc_url` goes through: the wallet's proxy (`config set proxy`), except for
/// a node on this machine or the LAN, which is reached directly as the IPFS gateway is — a Tor
/// circuit cannot reach a private address, and a node there reveals nothing to hide.
///
/// With a proxy set and a public node, nothing reaches the node directly: a proxy that is down
/// fails the request rather than revealing the address it was hiding.
pub fn rpc_proxy<'a>(rpc_url: &str, proxy: Option<&'a str>) -> Option<&'a str> {
    let proxy = proxy?;
    let host = reqwest::Url::parse(rpc_url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
    (!crate::ipfs::is_local(&host)).then_some(proxy)
}

/// A credential-free origin for review provenance. Paths, query strings and user info may
/// contain provider API keys, so they are never included in review output.
pub fn rpc_origin(raw: &str) -> String {
    reqwest::Url::parse(raw).map(|url| url.origin().ascii_serialization()).unwrap_or_else(|_| "invalid endpoint".into())
}

/// HTTPS is required for remote financial reads/broadcasts unless explicitly accepted.
/// Literal loopback/private/link-local addresses and localhost remain usable for local nodes.
pub fn require_secure_rpc(raw: &str, allow_insecure: bool) -> Result<()> {
    let url = reqwest::Url::parse(raw).map_err(|_| CoreError::Invalid("invalid RPC URL".into()))?;
    let host = url.host_str().unwrap_or_default().trim_matches(['[', ']']);
    let local = host.eq_ignore_ascii_case("localhost")
        || match host.parse::<std::net::IpAddr>() {
            Ok(std::net::IpAddr::V4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
            Ok(std::net::IpAddr::V6(ip)) => ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local(),
            Err(_) => false,
        };
    match url.scheme() {
        "https" => Ok(()),
        "http" if local || allow_insecure => Ok(()),
        "http" => Err(CoreError::Invalid("remote plaintext RPC requires explicit network transport consent (--allow-insecure-rpc)".into())),
        _ => Err(CoreError::Invalid("RPC URL must use http or https".into())),
    }
}

fn parse_u256(value: &str, name: &str) -> Result<U256> {
    value.parse::<U256>().map_err(|_| CoreError::Invalid(format!("{name} must be an integer")))
}

/// A provider with raw transport access for diagnostic methods the typed provider omits.
#[derive(Clone)]
pub struct Node {
    /// Typed provider.
    pub provider: WalletProvider,
    transport: WalletTransport,
    endpoint: quai_sdk::Endpoint,
    /// A second, independent endpoint that confirms this node's state anchors: the network's RPC
    /// while this node is the user's monitoring node. See [`crate::anchor`].
    witness: Option<std::sync::Arc<Node>>,
    /// The last anchor read through this node, shared by its clones.
    anchor: std::sync::Arc<crate::anchor::AnchorSlot>,
}

impl Node {
    /// This node, with `witness` confirming the blocks its state proofs are read at.
    pub fn with_witness(mut self, witness: Node) -> Node {
        self.anchor = crate::anchor::shared_slot(self.endpoint.as_str(), witness.endpoint.as_str());
        self.witness = Some(std::sync::Arc::new(Node { witness: None, ..witness }));
        self
    }

    /// The endpoint that confirms this node's anchors, when it has one.
    pub fn witness(&self) -> Option<&Node> {
        self.witness.as_deref()
    }

    /// How far the block this node's latest proofs were read at is vouched for, while that anchor
    /// is still in use.
    pub fn anchor_confirmation(&self) -> Option<crate::anchor::Confirmation> {
        self.recent_anchor(crate::anchor::ANCHOR_REUSE).map(|a| a.confirmation)
    }

    pub(crate) fn anchor_slot(&self) -> &crate::anchor::AnchorSlot {
        &self.anchor
    }

    pub(crate) fn recent_anchor(&self, within: Duration) -> Option<crate::anchor::Anchored> {
        self.anchor.get(within)
    }

    pub(crate) fn remember_anchor(&self, anchored: &crate::anchor::Anchored) {
        self.anchor.set(anchored);
    }

    /// Drop the remembered anchor, after a read at it found the block reorganized away.
    pub(crate) fn forget_anchor(&self) {
        self.anchor.clear();
    }

    /// Warm this exact provider's HTTP connection pool, bypassing the identity memo.
    /// This is transport preparation only; it never substitutes for execution identity checks.
    pub async fn warm_transport(&self) -> Result<()> {
        use quai_sdk::rpc::Transport;
        let start = std::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            self.transport.inner.request(&self.endpoint, "quai_chainId", serde_json::json!([])),
        )
        .await;
        crate::diag::timing("broadcast.transport_warm", start);
        match result {
            Ok(answer) => {
                answer?;
                Ok(())
            }
            Err(_) => Err(CoreError::Network("broadcast transport warmup timed out".into())),
        }
    }

    /// Raw JSON-RPC call for read-only diagnostics.
    pub async fn raw(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        use quai_sdk::rpc::Transport;
        Ok(self.transport.request(&self.endpoint, method, params).await?)
    }
}

/// How many blocks behind the network's RPC a monitoring node may be before a review says so.
/// A few seconds of lag is normal; three blocks is enough for a balance, a nonce or a pool's
/// reserves to have moved since the monitor last saw them.
pub const MONITOR_LAG_WARN: u64 = 3;

/// A monitoring node and the RPC a transaction will be broadcast through, to compare their heads.
#[derive(Clone)]
pub struct LagProbe {
    pub monitor: Node,
    pub rpc: Node,
    pub rpc_url: String,
}

impl LagProbe {
    /// Both heads at once, bounded: `(monitor, rpc)`. None when either does not answer in time,
    /// which says nothing about lag (a review cannot be built without the monitor, and a dead RPC
    /// fails the broadcast on its own).
    pub async fn heads(&self) -> Option<(u64, u64)> {
        let head = |node: &Node| {
            let provider = node.provider.clone();
            async move { provider.latest_header(ZONE).await.ok().flatten().map(|h| h.number) }
        };
        let both = async { tokio::join!(head(&self.monitor), head(&self.rpc)) };
        match tokio::time::timeout(Duration::from_secs(3), both).await {
            Ok((Some(monitor), Some(rpc))) => Some((monitor, rpc)),
            _ => None,
        }
    }

    /// The review's warning when the monitor is [`MONITOR_LAG_WARN`] or more blocks behind.
    pub async fn warning(&self) -> Option<String> {
        let (monitor, rpc) = self.heads().await?;
        lag_warning(monitor, rpc, &self.rpc_url)
    }
}

/// The warning for a monitor at `monitor` while the broadcast RPC is at `rpc`.
pub fn lag_warning(monitor: u64, rpc: u64, rpc_url: &str) -> Option<String> {
    let behind = rpc.saturating_sub(monitor);
    (behind >= MONITOR_LAG_WARN).then(|| {
        format!(
            "your monitoring node is {behind} blocks behind {} (block {monitor} against {rpc}): balances, nonces and prices in this review may be out of date",
            rpc_origin(rpc_url).trim_start_matches("https://").trim_start_matches("http://")
        )
    })
}

/// Result of checking a node against a profile.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeHealth {
    /// Network id.
    pub network: String,
    /// Reported chain ID.
    pub chain_id: String,
    /// Reported genesis.
    pub genesis: String,
    /// Both identity checks passed.
    pub identity_ok: bool,
    /// Latest Cyprus-1 block height.
    pub height: u64,
    /// Latest block hash.
    pub head_hash: String,
    /// Seconds since the latest block timestamp, when known.
    pub head_age_secs: Option<u64>,
    /// Current gas price (wei per gas).
    pub gas_price: String,
    /// Client version string, when exposed.
    pub client_version: Option<String>,
    /// Round-trip latency for the identity checks, milliseconds.
    pub latency_ms: u128,
    /// Order of the latest block: 0 prime, 1 region, 2 zone (when the node reports it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<u8>,
}

/// Verify chain ID and genesis match the profile, returning node health.
pub async fn check_node(profile: &NetworkProfile, node: &Node) -> Result<NodeHealth> {
    check_node_detail(profile, node, true).await
}

/// Node health, with the parts that do not change between blocks made optional.
///
/// Identity, height and latency are what a refresh needs, and that is two calls once the identity
/// answers are cached. The gas price, the client version and the block's order are for the System
/// screen: asking for them on every block spent three more calls a refresh restating facts about
/// the node, so they are read on their own slower schedule and carried in between.
pub async fn check_node_detail(profile: &NetworkProfile, node: &Node, detail: bool) -> Result<NodeHealth> {
    let provider = &node.provider;
    let started = std::time::Instant::now();
    let chain = provider.chain_id(ZONE.into()).await?;
    let genesis = provider.genesis_hash(ZONE).await?;
    let latency_ms = started.elapsed().as_millis();
    let head = provider.latest_header(ZONE).await?.ok_or_else(|| CoreError::Network("node returned no latest header".into()))?;
    let gas_price = if detail { provider.gas_price(ZONE).await?.to_string() } else { String::new() };
    let client_version = match detail {
        true => node.raw("web3_clientVersion", serde_json::json!([])).await.ok().and_then(|v| v.as_str().map(str::to_string)),
        false => None,
    };
    // The typed header omits the block's order (prime / region / zone); read it from the raw block.
    let order = match detail {
        true => node
            .raw("quai_getBlockByNumber", serde_json::json!(["latest", false]))
            .await
            .ok()
            .and_then(|b| b["order"].as_u64())
            .and_then(|o| u8::try_from(o).ok())
            .filter(|o| *o <= 2),
        false => None,
    };
    let identity_ok = chain == U256::from(profile.chain_id) && genesis == profile.genesis_hash()?;
    let head_age_secs = head_timestamp(&head).and_then(|ts| {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
        now.checked_sub(ts)
    });
    Ok(NodeHealth {
        network: profile.id.clone(),
        chain_id: chain.to_string(),
        genesis: genesis.to_string(),
        identity_ok,
        height: head.number,
        head_hash: head.hash.to_string(),
        head_age_secs,
        gas_price,
        client_version,
        latency_ms,
        order,
    })
}

/// Fail unless the node matches the trusted profile.
pub async fn require_identity(profile: &NetworkProfile, provider: &WalletProvider) -> Result<()> {
    let chain = provider.chain_id(ZONE.into()).await?;
    if chain != U256::from(profile.chain_id) {
        return Err(CoreError::Network(format!("node chain ID {chain} does not match network `{}` ({})", profile.id, profile.chain_id)));
    }
    let genesis = provider.genesis_hash(ZONE).await?;
    if genesis != profile.genesis_hash()? {
        return Err(CoreError::Network(format!("node genesis {genesis} does not match trusted genesis for `{}`", profile.id)));
    }
    Ok(())
}

/// Block timestamp (unix seconds) from a zone header.
pub(crate) fn header_time(header: &quai_sdk::provider::ZoneHeader) -> Option<u64> {
    head_timestamp(header)
}

fn head_timestamp(header: &quai_sdk::provider::ZoneHeader) -> Option<u64> {
    for ext in [&header.work_object_extensions, &header.extensions] {
        if let Some(value) = ext.fields().get("timestamp").and_then(|v| v.as_str()) {
            return u64::from_str_radix(value.trim_start_matches("0x"), 16).ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    /// Two blocks behind is a monitor keeping up; three is where a review says so, with both
    /// heights and the host it is behind (never a URL's credentials). Ahead is not behind.
    #[test]
    fn a_review_warns_from_three_blocks_behind() {
        use super::{MONITOR_LAG_WARN, lag_warning};
        let rpc = "https://user:key@rpc.quai.network/cyprus1";
        assert_eq!(lag_warning(100, 102, rpc), None);
        let warned = lag_warning(100, 103, rpc).expect("three blocks behind");
        assert!(warned.contains("3 blocks behind rpc.quai.network") && warned.contains("100 against 103"), "{warned}");
        assert!(!warned.contains("key"), "{warned}");
        assert_eq!(lag_warning(110, 103, rpc), None, "a monitor ahead of the RPC is not behind it");
        assert_eq!(MONITOR_LAG_WARN, 3);
    }

    /// Node RPC takes the wallet's proxy to a public node and goes direct to one on this machine
    /// or the LAN; with no proxy set it is always direct.
    #[test]
    fn node_rpc_goes_through_the_proxy_unless_the_node_is_local() {
        let tor = Some("socks5h://127.0.0.1:9050");
        assert_eq!(rpc_proxy("https://rpc.quai.network/cyprus1", tor), tor);
        assert_eq!(rpc_proxy("https://203.0.113.9:9200", tor), tor, "a public address is proxied too");
        for local in
            ["http://127.0.0.1:9200", "http://localhost:9200", "http://10.0.0.12:9200", "http://192.168.1.5:9200", "http://[::1]:9200"]
        {
            assert_eq!(rpc_proxy(local, tor), None, "{local} is reached directly");
        }
        assert_eq!(rpc_proxy("https://rpc.quai.network/cyprus1", None), None);
        // A proxy the SDK accepts for every scheme the wallet's setting allows.
        for scheme in ["socks5h", "socks5", "http", "https"] {
            assert!(HttpProxy::parse(&format!("{scheme}://127.0.0.1:9050")).is_ok(), "{scheme}");
        }
    }

    use super::*;

    #[tokio::test]
    async fn warm_transport_reuses_the_actual_client_across_idle_and_reconnects_after_close() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let accepted = Arc::new(AtomicUsize::new(0));
        let requested = Arc::new(AtomicUsize::new(0));
        let (connections, requests) = (accepted.clone(), requested.clone());
        let server = tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                connections.fetch_add(1, Ordering::SeqCst);
                let requests = requests.clone();
                tokio::spawn(async move {
                    let mut stream = BufReader::new(socket);
                    loop {
                        let mut line = String::new();
                        if tokio::time::timeout(Duration::from_millis(200), stream.read_line(&mut line)).await.unwrap_or(Ok(0)).unwrap_or(0)
                            == 0
                        {
                            break;
                        }
                        let mut length = 0;
                        loop {
                            line.clear();
                            if stream.read_line(&mut line).await.unwrap_or(0) == 0 {
                                return;
                            }
                            if line == "\r\n" {
                                break;
                            }
                            if let Some((name, value)) = line.split_once(':')
                                && name.eq_ignore_ascii_case("content-length")
                            {
                                length = value.trim().parse::<usize>().unwrap();
                            }
                        }
                        assert!(length < 4096);
                        let mut body = vec![0; length];
                        stream.read_exact(&mut body).await.unwrap();
                        let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        requests.fetch_add(1, Ordering::SeqCst);
                        let close = request["method"] == "test_close";
                        let body = serde_json::json!({"jsonrpc":"2.0","id":request["id"],"result":"0x9"}).to_string();
                        let connection = if close { "close" } else { "keep-alive" };
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: {connection}\r\n\r\n{body}",
                            body.len()
                        );
                        stream.get_mut().write_all(response.as_bytes()).await.unwrap();
                        if close {
                            break;
                        }
                    }
                });
            }
        });
        let mut network = NetworkProfile::builtins()[1].clone();
        network.rpc_url = format!("http://{address}");
        network.use_pathing = false;
        network.chain_id = 9;
        let node = network.node().unwrap();
        node.warm_transport().await.unwrap();
        assert_eq!(requested.load(Ordering::SeqCst), 1);
        node.provider.chain_id(ZONE.into()).await.unwrap();
        assert_eq!(requested.load(Ordering::SeqCst), 2, "warming did not replace identity verification with a memo");
        node.raw("quai_chainId", serde_json::json!([])).await.unwrap();
        assert_eq!(requested.load(Ordering::SeqCst), 2, "normal identity memo remains active");
        node.warm_transport().await.unwrap();
        assert_eq!(requested.load(Ordering::SeqCst), 3, "warmup bypasses even a populated identity memo");
        tokio::time::sleep(Duration::from_millis(100)).await;
        node.warm_transport().await.unwrap();
        assert_eq!(accepted.load(Ordering::SeqCst), 1, "cold, reused and briefly idle calls use the same client pool");
        tokio::time::sleep(Duration::from_millis(250)).await;
        node.warm_transport().await.unwrap();
        assert_eq!(accepted.load(Ordering::SeqCst), 2, "a server-expired idle connection reconnects");
        node.raw("test_close", serde_json::json!([])).await.unwrap();
        node.warm_transport().await.unwrap();
        assert_eq!(accepted.load(Ordering::SeqCst), 3, "an explicitly closed pooled connection reconnects");
        assert_eq!(requested.load(Ordering::SeqCst), 7);
        server.abort();
    }

    #[tokio::test]
    async fn warm_transport_times_out_a_silent_peer() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            futures::future::pending::<()>().await;
        });
        let mut network = NetworkProfile::builtins()[1].clone();
        network.rpc_url = format!("http://{address}");
        network.use_pathing = false;
        let error = network.node().unwrap().warm_transport().await.unwrap_err();
        assert!(error.to_string().contains("warmup timed out"));
        server.abort();
    }

    #[test]
    fn only_identity_reads_are_reused() {
        use serde_json::json;
        assert_eq!(WalletTransport::identity_key("quai_chainId", &json!([])), Some("chain"));
        assert_eq!(WalletTransport::identity_key("quai_getHeaderByNumber", &json!(["0x0"])), Some("genesis"));
        assert_eq!(WalletTransport::identity_key("quai_getHeaderByNumber", &json!(["latest"])), None);
        for method in ["quai_getBalance", "quai_getTransactionCount", "quai_gasPrice", "quai_estimateGas", "quai_sendRawTransaction"] {
            assert_eq!(WalletTransport::identity_key(method, &json!([])), None, "{method}");
        }
    }

    #[test]
    fn fee_policy_informs_reviews_instead_of_refusing() {
        let mainnet = NetworkProfile::builtins().remove(0);
        let quai = |n: u64| U256::from(n) * U256::from(10u64).pow(U256::from(18));
        let limits = mainnet.preparation_limits(120_000, None).unwrap();
        let policy = mainnet.fee_policy(120_000).unwrap();
        assert!(limits.max_gas >= 1_000_000, "a gas hint is not a refusal threshold");
        assert!(limits.max_total_fee > policy.max_total_fee && limits.max_gas_price > policy.max_gas_price);
        assert_eq!(mainnet.preparation_limits(120_000, Some(quai(2))).unwrap().max_total_fee, quai(2), "a typed cap stays hard");
        let price = U256::from(24_000_000_000_000u64);
        assert!(mainnet.fee_policy_note(price, quai(3)).is_none());
        assert!(mainnet.fee_policy_note(price, quai(30)).unwrap().contains("above your fee policy"));
        assert!(mainnet.fee_policy_note(policy.max_gas_price + U256::from(1), quai(1)).unwrap().contains("gas price"));
    }
}
