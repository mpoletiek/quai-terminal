//! Network profiles, provider construction and node identity checks.

use crate::error::{CoreError, Result};
use quai_sdk::accounts::{AccountObservationPolicy, FeePolicy};
use quai_sdk::primitives::Hash32;
use quai_sdk::wallet::storage::NetworkScope;
use quai_sdk::{HttpConfig, HttpTransport, Provider, Routing, U256, Zone};
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

/// Explorer API flavor.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExplorerKind {
    /// explorer.qu.ai (`/api/...`).
    QuaiExplorer,
    /// Blockscout v2 (`/api/v2/...`), e.g. orchard.quaiscan.io.
    Blockscout,
}

/// Explorer API endpoint for a network.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExplorerApiConfig {
    /// API flavor.
    pub kind: ExplorerKind,
    /// Base URL (no trailing `/api`).
    pub base_url: String,
}

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

/// A contract the wallet calls directly, pinned to its runtime bytecode hash.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PinnedContract {
    /// Address.
    pub address: String,
    /// Keccak-256 of the deployed runtime bytecode; checked on first use. None skips the check
    /// (custom networks and dev chains).
    #[serde(default)]
    pub code_hash: Option<String>,
}

impl PinnedContract {
    /// Review label: whether the bytecode is pinned or only configured.
    pub fn trust_label(&self) -> &'static str {
        if self.code_hash.is_some() { "✓ pinned bytecode" } else { "configured, no bytecode pin" }
    }

    fn new(address: &str, code_hash: &str) -> Self {
        PinnedContract { address: address.into(), code_hash: Some(code_hash.into()) }
    }
}

/// Ecosystem contracts and services for a network. Every entry is optional.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Ecosystem {
    /// Quainance UniswapV2 Router02 (the only swap router the wallet uses).
    pub quainance_router: Option<PinnedContract>,
    /// Quainance UniswapV2 factory.
    pub quainance_factory: Option<PinnedContract>,
    /// The older UniswapV2 deployment still carrying trade, which Quainance's frontend calls
    /// `legacyRouter` / `legacyFactory`.
    #[serde(default)]
    pub legacy_router: Option<PinnedContract>,
    #[serde(default)]
    pub legacy_factory: Option<PinnedContract>,
    /// The HartiiLabs launchpad: a second bonding-curve launcher with its own token and curve
    /// implementations. Its curves are EIP-1167 clones of `hartii_curve_impl`, which is what makes
    /// reading them by one fixed ABI safe.
    #[serde(default)]
    pub hartii_launcher: Option<PinnedContract>,
    #[serde(default)]
    pub hartii_curve_impl: Option<PinnedContract>,
    /// Token implementation authenticated by the Hartii launcher and each token proxy.
    #[serde(default)]
    pub hartii_token_impl: Option<PinnedContract>,
    /// Independently pinned Hartii UniswapV2 AMM; separate from its bonding curves.
    pub hartii_amm_router: Option<PinnedContract>,
    pub hartii_amm_factory: Option<PinnedContract>,
    /// Which of that factory's pairs the wallet reads, lowercase.
    ///
    /// An allowlist rather than the whole factory. It holds eighteen pairs, and `QSWAP/WQUAI` and
    /// `POOP/WQUAI` each appear twice at different addresses with different reserves — so a symbol
    /// is not an identity there, and routing a swap by symbol could pick the wrong one. Naming the
    /// pairs makes that impossible.
    #[serde(default)]
    pub legacy_pairs: Vec<String>,
    /// The exchange a bonding curve graduates into: a second UniswapV2 Router02 and its factory,
    /// holding the pairs of launched tokens that left their curve. Its pairs are not in the main
    /// factory, and a router only trades its own factory's pairs.
    #[serde(default)]
    pub launch_amm_router: Option<PinnedContract>,
    #[serde(default)]
    pub launch_amm_factory: Option<PinnedContract>,
    /// Quainance PoolGauge: LP staking with per-pool reward streams.
    pub quainance_gauge: Option<PinnedContract>,
    /// Launch-zone gauges, which carry the genesis campaigns on launched tokens. Several
    /// deployments run at once (each launch is enrolled in whichever was current), so this is a
    /// list; the wallet reads them all and skips any that fails its pin.
    #[serde(default)]
    pub zone_gauges: Vec<PinnedContract>,
    /// Quainance's bonding-curve launcher: it names each launched token's curve, which is how a
    /// curve (unpinnable one by one) is trusted.
    #[serde(default)]
    pub curve_launcher: Option<PinnedContract>,
    /// Multicall3, for batching the many small reads a pool or position sweep needs.
    pub multicall3: Option<PinnedContract>,
    /// Quainance's own indexer, for candles and pool history. Market data only: no address is
    /// ever sent to it.
    pub quainance_subgraph: Option<String>,
    /// Quainance's trade-zone indexer: launches on its bonding curve and where they trade now.
    /// Market data only: no address is ever sent to it.
    #[serde(default)]
    pub launch_subgraph: Option<String>,
    /// Bridged USDT.
    pub usdt: Option<PinnedContract>,
    /// Zora V3 Asks v1.1 module (Bazarr listings).
    pub zora_asks: Option<PinnedContract>,
    /// Zora V3 module manager.
    pub zora_module_manager: Option<PinnedContract>,
    /// Zora V3 ERC-721 transfer helper.
    pub zora_erc721_helper: Option<PinnedContract>,
    /// Zora V3 ERC-20 transfer helper.
    pub zora_erc20_helper: Option<PinnedContract>,
    /// Keccak-256 runtime hash of the network's WQI contract (`wqi`), checked before wrapping.
    pub wqi_code_hash: Option<String>,
    /// Keccak-256 runtime hash of the network's WQUAI contract (`wquai`).
    pub wquai_code_hash: Option<String>,
    /// The on-chain message board (`contracts/Messages.sol`), when one is deployed.
    pub messages: Option<PinnedContract>,
    /// Bazarr listings indexer base URL.
    pub bazarr_indexer: Option<String>,
    /// Bazarr marketplace web base URL (for "view on Bazarr").
    pub bazarr_web: Option<String>,
}

impl Ecosystem {
    /// Mainnet contracts, verified against chain 9 on 2026-09-15 (docs/ECOSYSTEM_PLAN.md).
    pub fn mainnet() -> Self {
        Ecosystem {
            quainance_router: Some(PinnedContract::new(
                "0x000d6795e06eA4F460CA9572a51741342156305A",
                "0xa44514555cf9e45726ceb5a1434691100c862bdd04f4039cb4af4cf0e66c2ef9",
            )),
            quainance_factory: Some(PinnedContract::new(
                "0x0018A110b6cA369DCf5Ab062C72F049E93B9eDe2",
                "0x0c4393d4bd2486ee81581fdef0cd88df239eebabb3fab7a302715f4c98bab903",
            )),
            // The launcher's own `router()` and `factory()` on chain 9, 2026-09-17: a standard Router02
            // whose `WETH()` is the network's WQUAI and whose `factory()` is the one below.
            launch_amm_router: Some(PinnedContract::new(
                "0x002b6c6bd946d78307824a7a73df2207d6b9928e",
                "0x7cd5576ca6e81fab8a5a926be4c8ba18c2f22369e72e3fc71e0be98f3d4f96ea",
            )),
            launch_amm_factory: Some(PinnedContract::new(
                "0x004658a54bfb73db3cfdc40b0e087dbd9ab1a9a3",
                "0xfe85a9faa97c8eb2335e93ea4ef517886c9cc7c4c51f1631a2964e3e2b317ea6",
            )),
            // Read from chain 9 on 2026-09-16 and cross-checked against Quainance's own published
            // config (`llms.txt` and the app bundle). The gauge's `factory()` returns the factory
            // pinned above, which `Gauge::open` re-checks before every stake.
            quainance_gauge: Some(PinnedContract::new(
                "0x0051f335c238dC8C69E2974000955d715292fCcB",
                "0x9cf6484d84505041c40aca86ae20145508cd00248aeb732146c9702e3b190700",
            )),
            // Read from chain 9 on 2026-09-17. Both answer the same interface; the first holds
            // the launches enrolled to date (SMOL/WQI among them), the second is the deployment
            // the app's own config now points new launches at.
            zone_gauges: vec![
                PinnedContract::new(
                    "0x004A1FE8b99AeBF9077f68884de6EDAa036c6df6",
                    "0x380bce1b3a954543419ba0593fef43ee09cc378407fdce08b0f32b99bbb1faa4",
                ),
                PinnedContract::new(
                    "0x0022a8192a58dD1145D5869071dbcC47a561286f",
                    "0x27a79b37a4c409a22f8c0a387c04d944d20e93de44351809635ae9fc8347cba1",
                ),
            ],
            // From Quainance's app config (curveSystem.launcher), bytecode hash checked against chain
            // 9 on 2026-09-17 along with the market factory, curve AMM factory and router it names.
            curve_launcher: Some(PinnedContract::new(
                "0x002658af3d4a4d0366c5ab997630211a818ab923",
                "0x80abe47423b1dcea2a1ccb2e03f6ac237d56674755c2048a76ab8e5234b1d3ea",
            )),
            multicall3: Some(PinnedContract::new(
                "0x00567637197E6554e2CF47a3988Cb7B819f4E92C",
                "0xf04e9845b5acf40ea55c268a50df01b9c2de08078740d2ee80504033cece3488",
            )),
            usdt: Some(PinnedContract::new(
                "0x0049F7cbCa3556C2DfaE62Aafa7015F99de1b8f5",
                "0xca9a26f2129509bafda9e9832cd68d01be3228e26dbee315bada4a33aaa6f3f9",
            )),
            zora_asks: Some(PinnedContract::new(
                "0x00124aA47f2FA701a1B8E4519fC8ad9608EB692F",
                "0xbda34eecaabfe65c75153b4cf084b4892154d73346567378fc22fa26e8b9ed1b",
            )),
            zora_module_manager: Some(PinnedContract::new(
                "0x0035112f262f4Da0064C5863f1A5845F071d6459",
                "0xd1764ada6e1e3b98b17be04024851d45464a9245a86d617830384b69733e4d43",
            )),
            zora_erc721_helper: Some(PinnedContract::new(
                "0x0045f14924f1029Ba7390d7FC2Cc57B847B269e8",
                "0x2eadd974662664c648fe8ba1a1b0e48fd04deb21b317b467152435fffac76235",
            )),
            zora_erc20_helper: Some(PinnedContract::new(
                "0x002A4fA2ddBF359Cc4E373E9285b2eEa0213a11A",
                "0x23ab86806aa401551e4df7c39b61e2197047979dbd1007d02d47c1d895af6182",
            )),
            // SHA-256 of the same runtimes matches quai-sdk's fetch_wrapper_bytecode.py pins.
            wqi_code_hash: Some("0xab2ed076d1d3fa4c5a99a450ff7dcfd7e24710348fef0af8cad3b0665e482c4d".into()),
            wquai_code_hash: Some("0x72ebc2d882f6c49aa65b9be06e23d07516f7b34d144e132a154a3105eb2a88a9".into()),
            // The message board (contracts/Messages.sol in the quai-messages project), deployed
            // 2026-09-16. It has no owner: the key that deployed it holds no power over it.
            messages: Some(PinnedContract::new(
                "0x0077AD436f63F35D0DeD89055402659750a28D0a",
                "0x44f85316cf5be37e598a0d08e3ca01572d0e762b57505f07d65acb0ee290d9c1",
            )),
            // Read from chain 9 on 2026-09-21; the runtime hashes are unchanged from the
            // 2026-09-16 survey in docs/DEX_EXPANSION_PLAN.md. The router's `factory()` is the
            // factory below and its `WETH()` is the same WQUAI the wallet already pins, so
            // `Router::open`'s invariants hold for it as written.
            legacy_router: Some(PinnedContract::new(
                "0x006432Ea8c46cBF981f6e710d2439C941CeBe2d0",
                "0x0c3791a59ffbe3e43a6f6be9f298e593fcbff7ce0ad256031da3ad55f23beab1",
            )),
            legacy_factory: Some(PinnedContract::new(
                "0x0006112e89ee10615273ED72FE035cC068BC57A9",
                "0x96767bbe38752f02bc30ef9491eb5feb8ac0ff060a9ac29cfd3afe840052b239",
            )),
            // The three still carrying real depth, read 2026-09-21: BOSS/WQUAI 176,288 WQUAI,
            // QIQI/WQUAI 47,560 and BARRY/WQUAI 18,499. Each pair's own `factory()` is checked
            // against the pin before it is used, so naming an address here cannot smuggle in a
            // pair from somewhere else.
            legacy_pairs: vec![
                "0x0002d1373b8bf88a03809eda30cb23815c2192b7".into(),
                "0x0012f9ce8e7e0918bc22be5d6d0878ea523932cc".into(),
                "0x002cf345a9ae76400662e87adf57ad278afd5752".into(),
            ],
            // Read from chain 9 on 2026-09-21 and cross-checked against the app bundle's
            // LAUNCH_FACTORY_ADDRESS and BONDING_CURVE_IMPL_ADDRESS. 27 tokens, of which HRT and
            // QAXE have bonded; neither has a pool on any factory the wallet knows.
            hartii_launcher: Some(PinnedContract::new(
                "0x001AF1BbB40807fcb99C9Eeaa49dF5E91e7Efd42",
                "0x9e4c6a28daec6adac45f4f390b994230f28c7fea28a5f9115fb4d430f5414f14",
            )),
            hartii_curve_impl: Some(PinnedContract::new(
                "0x0062D75A096E67FEF48A8C5a9fD9094d2BEa9D14",
                "0x60231d399aa6709360fc6c3b281b6be05b600f7ed04d50bdb69b6c9377a60073",
            )),
            hartii_token_impl: Some(PinnedContract::new(
                "0x0059af4b7441e15E06e4686bd2f4CC5dfF5AAA0A",
                "0xe34ceb71dab47ee57d432e88801ad7733c8b9382697380c732bfb776e02e8eb0",
            )),
            hartii_amm_router: Some(PinnedContract::new(
                "0x000284FD8Df039CFF2949b62D854dc43c2Eae6E1",
                "0x6150c2a53eeccb818146583059319fa94b77c2399705374359eaeeee86ea38c1",
            )),
            hartii_amm_factory: Some(PinnedContract::new(
                "0x000993E799424EEB1f1d6AdAa0e68006f4C696Fe",
                "0x5921f7e274335dc1d0a6076079acf41461927b341702efa8271f0e58bc8c78f1",
            )),
            quainance_subgraph: Some("https://graph.quai.network/subgraphs/name/quainance/v2".into()),
            launch_subgraph: Some("https://graph.quai.network/subgraphs/name/quainance/trade-zone-staging".into()),
            bazarr_indexer: Some("https://watcher.basedhash.cc".into()),
            bazarr_web: Some("https://bazarr.xyz".into()),
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
        let transport = WalletTransport::new(
            HttpTransport::new(HttpConfig::default().with_timeout(Duration::from_secs(20)))
                .map_err(|e| CoreError::Network(format!("http transport: {e}")))?,
        );
        let routing = Routing::with_pathing(&self.rpc_url, ZONE.into(), self.use_pathing)?;
        let endpoint = routing.endpoint(ZONE.into())?.clone();
        Ok(Node { provider: Provider::new(transport.clone(), routing, U256::from(self.chain_id)), transport, endpoint })
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
        Ok(FeePolicy {
            max_gas: gas_hint.saturating_mul(4).clamp(1_000_000, 20_000_000),
            max_gas_price: policy.max_gas_price.saturating_mul(U256::from(20)),
            max_total_fee: explicit_cap.unwrap_or_else(|| policy.max_total_fee.saturating_mul(U256::from(40))),
            gas_margin_bps: policy.gas_margin_bps,
        })
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
        Ok(FeePolicy {
            max_gas,
            max_gas_price: parse_u256(&self.max_gas_price, "max_gas_price")?,
            max_total_fee: parse_u256(&self.max_total_fee, "max_total_fee")?,
            gas_margin_bps: 1000,
        })
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
}

impl Node {
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

/// Result of checking a node against a profile.
#[derive(Clone, Debug, Serialize)]
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
