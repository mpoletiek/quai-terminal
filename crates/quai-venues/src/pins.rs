//! Every contract the wallet trusts on a network, pinned to its runtime bytecode: the
//! exchanges, launchers, gauges and NFT market, plus the few contracts that are not venues
//! (WQI/WQUAI, the messages board) because a network profile lists them in one place.

use serde::{Deserialize, Serialize};

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

    pub fn new(address: &str, code_hash: &str) -> Self {
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
    /// Quainance's second launcher (its frontend's `revenueCurveSystem.launcher`), whose curves
    /// graduate onto the revenue AMM. Same interface and storage layout as `curve_launcher`
    /// (`live_mainnet::revenue_curves_speak_the_launch_zone_interface`); its curves' runtime differs.
    #[serde(default)]
    pub revenue_curve_launcher: Option<PinnedContract>,
    /// Multicall3, for batching the many small reads a pool or position sweep needs.
    pub multicall3: Option<PinnedContract>,
    /// Quainance's own indexer, for candles and pool history. Market data only: no address is
    /// ever sent to it.
    pub quainance_subgraph: Option<String>,
    /// Quainance's trade-zone indexer: launches on its bonding curve and where they trade now.
    /// Market data only: no address is ever sent to it.
    #[serde(default)]
    pub launch_subgraph: Option<String>,
    /// HartiiLabs' public read API: its launchpad's 24h changes. Market data only: no address is
    /// ever sent to it, only the one directory request.
    #[serde(default)]
    pub hartii_api: Option<String>,
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
    /// No private-message key announcement (v3) exists on this network before this block, so a
    /// first look-up of someone's key never reads further back.
    pub messages_v3_from: Option<u64>,
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
            // From Quainance's app config (revenueCurveSystem.launcher, with this runtime hash),
            // proven against chain 9 on 2026-09-24, confirmed by two nodes.
            revenue_curve_launcher: Some(PinnedContract::new(
                "0x002879c58c8430626d99bfd45504ffc484e6e811",
                "0x352f4d2c9b37278b36eb2da662bdb9bc6a6d02a5e8b3e8cba70e0f3071a544f7",
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
            // v3 private messages did not exist before 2026-09-24; this block is hours earlier.
            messages_v3_from: Some(10_270_000),
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
                // Q0/WQUAI and QPEPE/WQUAI: QuaiSwap pools Quainance's trade zone lists (its catalog,
                // 2026-09-24), for tokens that began on poop.fun. Only the pools are traded, never a
                // POOP curve. Each is still checked to name the pinned factory before it is listed.
                "0x003b4b96bf0793eb1d53b79f8c38746a298eeef8".into(),
                "0x00240aaca3e2c74e09522025b6b948ecc8a0fb27".into(),
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
            // The index Quainance's own trade zone reads (its frontend names it eight times): the
            // launch zone plus the revenue launcher's curves, which the older
            // `trade-zone-staging` does not carry.
            launch_subgraph: Some("https://graph.quai.network/subgraphs/name/quainance/trade-zone-revenue-staging-20260918-r2".into()),
            hartii_api: Some("https://hartiilabs.com".into()),
            bazarr_indexer: Some("https://watcher.basedhash.cc".into()),
            bazarr_web: Some("https://bazarr.xyz".into()),
        }
    }
}
