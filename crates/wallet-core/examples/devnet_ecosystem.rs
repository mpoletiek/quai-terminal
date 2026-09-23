//! Development-only: deploy a local swap and NFT marketplace ecosystem on the loopback dev chain
//! with the PUBLIC fixture key 0x325, using the exact mainnet creation code of WQUAI, the
//! Quainance factory and router, and the Zora V3 fee settings, module manager, transfer helpers
//! and Asks v1.1 (constructor arguments rewired to the local deployments), plus WQI from its
//! pinned mainnet creation code. Seeds two pools, mints test NFTs and lists tokens 1 and 2 as Zora
//! asks. Refuses anything but chain 1337 on loopback.
//!
//! Usage: devnet_ecosystem <scripts/devnet/ecosystem-artifacts.json> [rpc] [seller-address]

use quai_sdk::abi::AbiInterface;
use quai_sdk::accounts::{AccountIntent, AccountObservationPolicy, AccountSession, FeePolicy};
use quai_sdk::contracts::{Contract, DeploymentSearch, prepare_deployment};
use quai_sdk::provider::{ReceiptOutcome, WaitConfig};
use quai_sdk::signer::{LocalSigner, Signer};
use quai_sdk::wallet::storage::{NetworkScope, PublicAddress, ReservationId, SqliteStore};
use quai_sdk::{HttpConfig, HttpTransport, Provider, QuaiAddress, Routing, U256, Zone};
use serde_json::{Value, json};

type Err = Box<dyn std::error::Error>;

struct Chain {
    provider: Provider<HttpTransport>,
    signer: LocalSigner,
    store: SqliteStore,
    sender: QuaiAddress,
}

fn policy(gas: u64, margin_bps: u16) -> FeePolicy {
    FeePolicy::new(gas, U256::from(100_000_000_000_000_000u64), U256::from(1_000_000_000_000_000_000_000_000u128))
        .with_gas_margin_bps(margin_bps)
}

fn new_id() -> Result<ReservationId, Err> {
    let mut id = [0u8; 16];
    quai_sdk::crypto::fill_random(&mut id)?;
    Ok(ReservationId(id))
}

fn word_address(a: QuaiAddress) -> Vec<u8> {
    let mut w = vec![0u8; 12];
    w.extend_from_slice(a.address().bytes());
    w
}

/// Transparent synthetic constructor: initialize documented slots and return exact runtime.
fn runtime_init(runtime: &[u8], storage: &[(Vec<u8>, Vec<u8>)]) -> Result<Vec<u8>, Err> {
    let mut init = Vec::new();
    for (slot, value) in storage {
        if slot.len() != 32 || value.len() != 32 {
            return Err("invalid synthetic constructor word".into());
        }
        init.push(0x7f);
        init.extend(value);
        init.push(0x7f);
        init.extend(slot);
        init.push(0x55);
    }
    let length = u16::try_from(runtime.len())?;
    let offset = u16::try_from(init.len() + 15)?;
    init.extend([
        0x61,
        (length >> 8) as u8,
        length as u8,
        0x61,
        (offset >> 8) as u8,
        offset as u8,
        0x60,
        0,
        0x39,
        0x61,
        (length >> 8) as u8,
        length as u8,
        0x60,
        0,
        0xf3,
    ]);
    init.extend(runtime);
    Ok(init)
}

impl Chain {
    async fn wait(&self, hash: quai_sdk::primitives::Hash32, what: &str) -> Result<(), Err> {
        let observed = self
            .provider
            .wait_for_receipt(
                Zone::Cyprus1,
                hash,
                WaitConfig::new(1, std::time::Duration::from_secs(180), std::time::Duration::from_secs(1)),
            )
            .await?;
        match observed.receipt.outcome {
            ReceiptOutcome::Failed => Err(format!("{what} reverted ({hash})").into()),
            _ => Ok(()),
        }
    }

    async fn deploy(&mut self, what: &str, code: &[u8], interface: &AbiInterface, args: &[Value]) -> Result<QuaiAddress, Err> {
        self.deploy_with_margin(what, code, interface, args, 3000).await
    }

    async fn deploy_with_margin(
        &mut self,
        what: &str,
        code: &[u8],
        interface: &AbiInterface,
        args: &[Value],
        margin: u16,
    ) -> Result<QuaiAddress, Err> {
        let id = new_id()?;
        let (hash, address) = {
            let mut session = AccountSession::new(&self.provider, &self.signer, &mut self.store)?
                .with_observation_policy(AccountObservationPolicy::PinnedLatest);
            let nonce = session.reserve_deployment_nonce(id).await?;
            let deployment = prepare_deployment(
                interface,
                code,
                args,
                self.sender,
                U256::from(1337),
                nonce,
                U256::ZERO,
                DeploymentSearch::new(0, 10_000),
                || false,
            )?;
            let address = deployment.address();
            // Constructors that call other contracts pay cold-access gas that the estimate misses,
            // and deployments carry only the predicted-address access tuple.
            let prepared = session.prepare_deployment(id, deployment, policy(11_000_000, margin)).await?;
            let signed = session.sign(&prepared)?;
            session.broadcast(id).await?;
            (signed.hash()?, address)
        };
        self.wait(hash, what).await?;
        let code = self.provider.code(address, quai_sdk::BlockTag::Latest).await?;
        if code.bytes().is_empty() {
            return Err(format!("{what}: no code at {address}").into());
        }
        eprintln!("deployed {what} at {address}");
        Ok(address)
    }

    async fn call(&mut self, what: &str, to: QuaiAddress, abi: &[&str], function: &str, args: &[Value], value: U256) -> Result<(), Err> {
        let interface = AbiInterface::from_human_readable(abi)?;
        let contract = Contract::new(to, interface, &self.provider);
        let call = contract.prepare(function, args, value)?;
        // Multi-contract calls need the discovered access list (see wallet_core::data::with_access_list).
        let call = wallet_core::data::with_access_list(&self.provider, self.sender, call).await?;
        let intent: AccountIntent = call.into_account_intent();
        let id = new_id()?;
        let hash = {
            let mut session = AccountSession::new(&self.provider, &self.signer, &mut self.store)?
                .with_observation_policy(AccountObservationPolicy::PinnedLatest);
            let prepared = session.prepare(id, intent, policy(11_000_000, 3000)).await?;
            let signed = session.sign(&prepared)?;
            session.broadcast(id).await?;
            signed.hash()?
        };
        self.wait(hash, what).await?;
        eprintln!("ok {what}");
        Ok(())
    }
}

impl Chain {
    /// Deploy with an explicit access list (constructors that call other contracts need the
    /// callees listed, as the mainnet deployment did). Signed locally and broadcast directly.
    async fn deploy_listed(&mut self, what: &str, init: &[u8], callees: &[QuaiAddress], gas: u64) -> Result<QuaiAddress, Err> {
        let nonce = self.provider.transaction_count(self.sender, quai_sdk::BlockTag::Pending).await?;
        let mut data = init.to_vec();
        data.extend_from_slice(&[0; 4]);
        let n = data.len();
        let mut address = None;
        for salt in 0u32..100_000 {
            data[n - 4..].copy_from_slice(&salt.to_be_bytes());
            let raw = quai_sdk::primitives::contract_address(self.sender.address(), nonce, &data);
            if let Ok(a) = QuaiAddress::try_from(raw)
                && a.zone() == self.sender.zone()
            {
                address = Some(a);
                break;
            }
        }
        let address = address.ok_or("no in-zone address")?;
        let mut access_list = vec![quai_sdk::consensus::AccessTuple { address: address.address(), storage_keys: vec![] }];
        for c in callees {
            access_list.push(quai_sdk::consensus::AccessTuple { address: c.address(), storage_keys: vec![] });
        }
        let gas_price = self.provider.gas_price(Zone::Cyprus1).await?;
        let tx = quai_sdk::consensus::QuaiTransaction {
            chain_id: U256::from(1337),
            nonce,
            to: None,
            value: U256::ZERO,
            gas_limit: gas,
            gas_price,
            data,
            access_list,
        };
        let signed = self.signer.sign_quai(&tx)?;
        let hash = signed.hash()?;
        self.provider
            .broadcast(&signed)
            .await
            .map_err(|e| format!("{what}: broadcast {e} {:?}", std::error::Error::source(&e).map(|s| s.to_string())))?;
        self.wait(hash, what).await?;
        if self.provider.code(address, quai_sdk::BlockTag::Latest).await?.bytes().is_empty() {
            return Err(format!("{what}: no code at {address}").into());
        }
        eprintln!("deployed {what} at {address}");
        Ok(address)
    }

    /// Contract call signed locally with the discovered access list (after a direct broadcast the
    /// session store's nonce cursor is behind the chain, so later calls use this path too).
    async fn call_listed(
        &mut self,
        what: &str,
        to: QuaiAddress,
        abi: &[&str],
        function: &str,
        args: &[Value],
        value: U256,
    ) -> Result<(), Err> {
        let interface = AbiInterface::from_human_readable(abi)?;
        let contract = Contract::new(to, interface, &self.provider);
        let call = contract.prepare(function, args, value)?;
        let call = wallet_core::data::with_access_list(&self.provider, self.sender, call).await?;
        let mut request = quai_sdk::provider::CallRequest::new(self.sender, to);
        request.value = Some(value);
        request.input = call.data().clone();
        let estimate = self.provider.estimate_gas(&request, quai_sdk::BlockTag::Latest).await?;
        let nonce = self.provider.transaction_count(self.sender, quai_sdk::BlockTag::Pending).await?;
        let gas_price = self.provider.gas_price(Zone::Cyprus1).await?;
        let tx = quai_sdk::consensus::QuaiTransaction {
            chain_id: U256::from(1337),
            nonce,
            to: Some(to.address()),
            value,
            gas_limit: estimate * 3 / 2 + 50_000,
            gas_price,
            data: call.data().bytes().to_vec(),
            access_list: call.access_list().to_vec(),
        };
        let signed = self.signer.sign_quai(&tx)?;
        let hash = signed.hash()?;
        self.provider
            .broadcast(&signed)
            .await
            .map_err(|e| format!("{what}: broadcast {e} {:?}", std::error::Error::source(&e).map(|s| s.to_string())))?;
        self.wait(hash, what).await?;
        eprintln!("ok {what}");
        Ok(())
    }
}

fn hex_bytes(v: &Value) -> Result<Vec<u8>, Err> {
    Ok(hex::decode(v.as_str().ok_or("artifact")?.trim_start_matches("0x"))?)
}

#[tokio::main]
async fn main() -> Result<(), Err> {
    let artifacts_path = std::env::args().nth(1).ok_or("artifacts path required")?;
    let rpc = std::env::args().nth(2).unwrap_or_else(|| "http://127.0.0.1:19200".into());
    let endpoint = reqwest::Url::parse(&rpc)?;
    if endpoint.scheme() != "http"
        || endpoint.host_str().and_then(|s| s.parse::<std::net::IpAddr>().ok()).is_none_or(|ip| !ip.is_loopback())
        || endpoint.port().is_none()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
        || endpoint.path() != "/"
    {
        return Err("devnet_ecosystem only targets loopback dev chains".into());
    }
    let art: Value = serde_json::from_str(&std::fs::read_to_string(&artifacts_path)?)?;
    let provider =
        Provider::new(HttpTransport::new(HttpConfig::default())?, Routing::direct(&rpc, Zone::Cyprus1.into())?, U256::from(1337));
    let chain_id = provider.chain_id(Zone::Cyprus1.into()).await?;
    if chain_id != U256::from(1337) {
        return Err(format!("refusing chain {chain_id}").into());
    }
    let genesis = provider.genesis_hash(Zone::Cyprus1).await?;
    let trading_only = std::env::var("TRADING_ONLY").as_deref() == Ok("1");
    if trading_only && genesis.to_string() != "0xff38a93744ee5aae738addc88da4f6b171528244e81d34aa4b25579fa3f44ed2" {
        return Err("trading fixture requires the pinned conversion-development genesis".into());
    }
    if trading_only && std::env::var_os("DEVNET_RESUME").is_some() {
        return Err("trading-only fixture requires a fresh isolated deployment".into());
    }
    let scope = NetworkScope { chain_id: U256::from(1337), genesis, zone: Zone::Cyprus1 };
    let mut bytes = [0u8; 32];
    bytes[31] = 0x25;
    bytes[30] = 0x03;
    let key = quai_sdk::crypto::SecretKey::from_bytes(&bytes)?;
    let public = PublicAddress::imported(&key.public_key())?;
    let signer = LocalSigner::new(key, scope.chain_id)?;
    let sender: QuaiAddress = signer.address().try_into()?;
    let dir = match std::env::var_os("DEVNET_DEPLOY_HOME") {
        Some(path) => {
            let path = std::path::PathBuf::from(path);
            if !path.is_absolute() || !path.starts_with("/tmp") || path.components().any(|p| p == std::path::Component::ParentDir) {
                return Err("deployment storage must be an isolated absolute /tmp path".into());
            }
            path
        }
        None if trading_only => return Err("trading-only fixture requires DEVNET_DEPLOY_HOME".into()),
        None => std::env::temp_dir().join("quai-terminal-devnet-ecosystem"),
    };
    std::fs::create_dir_all(&dir)?;
    let mut store = SqliteStore::open(dir.join(format!("deploy-{genesis}.sqlite")), scope)?;
    if store.addresses()?.is_empty() {
        store.import_metadata(0, &[public])?;
    }
    let mut c = Chain { provider, signer, store, sender };
    let empty = AbiInterface::from_json(b"[]")?;
    let code = &art["code"];
    if let Ok(root) = std::env::var("DEVNET_HARTII_RUN") {
        if !trading_only {
            return Err("synthetic Hartii requires TRADING_ONLY=1".into());
        }
        let root = std::path::PathBuf::from(root).canonicalize()?;
        if !root.starts_with("/tmp") || std::fs::read_to_string(root.join(".owned"))?.trim() != "quai-terminal-disposable-trading-v1" {
            return Err("synthetic Hartii requires an owned trading fixture".into());
        }
        let deployed: Value = serde_json::from_str(&std::fs::read_to_string(root.join("evidence/deployment.json"))?)?;
        let wquai: QuaiAddress = deployed["wquai"].as_str().ok_or("fixture WQUAI")?.parse()?;
        let fixture: Value = serde_json::from_str(&std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/hartii_amm_runtime_evidence.json"
        ))?)?;
        let progress_path = root.join("evidence/hartii-deployment-progress.json");
        let mut state: Value = if progress_path.is_file() {
            serde_json::from_str(&std::fs::read_to_string(&progress_path)?)?
        } else {
            json!({"scope":"exact Hartii factory/router runtimes with synthetic constructor storage; real factory-created pair and real deposited liquidity; not production deployment equivalence","wquai":wquai.to_string()})
        };
        for (key, expected) in [
            ("factory", "0x5921f7e274335dc1d0a6076079acf41461927b341702efa8271f0e58bc8c78f1"),
            ("router", "0x6150c2a53eeccb818146583059319fa94b77c2399705374359eaeeee86ea38c1"),
        ] {
            let runtime = hex_bytes(&fixture[key]["runtime"])?;
            let hash = format!("0x{}", hex::encode(quai_sdk::crypto::keccak256(&runtime)));
            if hash != expected {
                return Err("Hartii runtime fixture digest mismatch".into());
            }
            let address: QuaiAddress = if let Some(address) = state[key].as_str() {
                address.parse()?
            } else {
                let storage = if key == "router" {
                    vec![
                        (
                            U256::ZERO.to_be_bytes::<32>().to_vec(),
                            word_address(state["factory"].as_str().ok_or("Hartii factory")?.parse()?),
                        ),
                        (U256::from(1).to_be_bytes::<32>().to_vec(), word_address(wquai)),
                    ]
                } else {
                    vec![]
                };
                let address =
                    c.deploy_listed(&format!("Synthetic Hartii {key}"), &runtime_init(&runtime, &storage)?, &[], 11_000_000).await?;
                state[key] = json!(address.to_string());
                state[format!("{key}_code_hash")] = json!(hash);
                state[format!("{key}_constructor_storage")] =
                    json!(storage.iter().map(|(slot, value)| (hex::encode(slot), hex::encode(value))).collect::<Vec<_>>());
                std::fs::write(&progress_path, serde_json::to_vec_pretty(&state)?)?;
                address
            };
            if format!(
                "0x{}",
                hex::encode(quai_sdk::crypto::keccak256(c.provider.code(address, quai_sdk::BlockTag::Latest).await?.bytes()))
            ) != expected
            {
                return Err("Hartii resumed runtime mismatch".into());
            }
        }
        let router: QuaiAddress = state["router"].as_str().ok_or("Hartii router")?.parse()?;
        let factory: QuaiAddress = state["factory"].as_str().ok_or("Hartii factory")?.parse()?;
        let token: QuaiAddress = if let Some(token) = state["token"].as_str() {
            token.parse()?
        } else {
            let abi = AbiInterface::from_human_readable(&["constructor(string name,string symbol,uint8 decimals,uint256 supply)"])?;
            let token = c
                .deploy(
                    "Hartii fixture 8-decimal token",
                    &hex_bytes(&art["test_token"])?,
                    &abi,
                    &[json!("Hartii Fixture"), json!("HTK"), json!("8"), json!("100000000000000")],
                )
                .await?;
            state["token"] = json!(token.to_string());
            std::fs::write(&progress_path, serde_json::to_vec_pretty(&state)?)?;
            token
        };
        if state["seeded"] != true {
            let erc20 = ["function approve(address,uint256) returns(bool)"];
            c.call_listed(
                "approve HTK fixture seed",
                token,
                &erc20,
                "approve",
                &[json!(router.to_string()), json!("10000000000000")],
                U256::ZERO,
            )
            .await?;
            c.call_listed(
                "deposit real WQUAI fixture seed",
                wquai,
                &["function deposit() payable"],
                "deposit",
                &[],
                U256::from(1000u128 * 10u128.pow(18)),
            )
            .await?;
            c.call_listed(
                "approve WQUAI fixture seed",
                wquai,
                &erc20,
                "approve",
                &[json!(router.to_string()), json!("1000000000000000000000")],
                U256::ZERO,
            )
            .await?;
            c.call_listed("seed real Hartii HTK/WQUAI liquidity",router,
                &["function addLiquidity(address,address,uint256,uint256,uint256,uint256,address,uint256) returns(uint256,uint256,uint256)"],"addLiquidity",
                &[json!(token.to_string()),json!(wquai.to_string()),json!("10000000000000"),json!("1000000000000000000000"),json!("1"),json!("1"),json!(sender.to_string()),json!((std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs()+3600).to_string())],U256::ZERO).await?;
            state["seeded"] = json!(true);
            std::fs::write(&progress_path, serde_json::to_vec_pretty(&state)?)?;
        }
        let contract = Contract::new(
            factory,
            AbiInterface::from_human_readable(&["function getPair(address,address) view returns(address)"])?,
            &c.provider,
        );
        state["pair"] = contract
            .call(sender, "getPair", &[json!(token.to_string()), json!(wquai.to_string())], quai_sdk::BlockTag::Latest)
            .await?
            .first()
            .cloned()
            .ok_or("Hartii pair missing")?;
        println!("{}", state);
        return Ok(());
    }
    if let Ok(root) = std::env::var("DEVNET_GAUGE_RUN") {
        if !trading_only {
            return Err("synthetic gauge requires TRADING_ONLY=1".into());
        }
        let root = std::path::PathBuf::from(root).canonicalize()?;
        if !root.starts_with("/tmp") || std::fs::read_to_string(root.join(".owned"))?.trim() != "quai-terminal-disposable-trading-v1" {
            return Err("synthetic gauge requires an owned trading fixture".into());
        }
        let deployed: Value = serde_json::from_str(&std::fs::read_to_string(root.join("evidence/deployment.json"))?)?;
        let factory: QuaiAddress = deployed["factory"].as_str().ok_or("fixture factory")?.parse()?;
        let reward: QuaiAddress = deployed["tusd"].as_str().ok_or("fixture reward")?.parse()?;
        let factory_view = Contract::new(
            factory,
            AbiInterface::from_human_readable(&["function getPair(address,address) view returns(address)"])?,
            &c.provider,
        );
        let pair =
            factory_view.call(sender, "getPair", &[deployed["tka"].clone(), deployed["tusd"].clone()], quai_sdk::BlockTag::Latest).await?;
        let pair: QuaiAddress = pair.first().and_then(Value::as_str).ok_or("fixture LP")?.parse()?;
        let fixture: Value = serde_json::from_str(&std::fs::read_to_string("crates/wallet-core/tests/fixtures/gauge_rate_evidence.json")?)?;
        let runtime = hex_bytes(&fixture["runtime"])?;
        let runtime_hash = format!("0x{}", hex::encode(quai_sdk::crypto::keccak256(&runtime)));
        if fixture["runtime_keccak256"] != runtime_hash {
            return Err("gauge fixture runtime digest mismatch".into());
        }
        let word = |value: u64| U256::from(value).to_be_bytes::<32>().to_vec();
        let mapping = |address: QuaiAddress, slot: u64| {
            let mut input = word_address(address);
            input.extend(word(slot));
            quai_sdk::crypto::keccak256(&input).to_vec()
        };
        let pool_base = U256::from_be_bytes(quai_sdk::crypto::keccak256(&word(2)));
        // Exact public core runtime, with transparent synthetic constructor storage. No live
        // state is copied and no chain storage is edited after deployment.
        let storage = vec![
            (word(0), word(1)),
            (word(1), word_address(factory)),
            (word(2), word(1)),
            (pool_base.to_be_bytes::<32>().to_vec(), word_address(pair)),
            ((pool_base + U256::from(1)).to_be_bytes::<32>().to_vec(), word(0)),
            (mapping(pair, 3), word(1)),
            (mapping(reward, 5), word(1)),
        ];
        let mut init = Vec::new();
        for (slot, value) in &storage {
            init.push(0x7f);
            init.extend(value);
            init.push(0x7f);
            init.extend(slot);
            init.push(0x55);
        }
        let length = u16::try_from(runtime.len())?;
        let offset = u16::try_from(init.len() + 15)?;
        init.extend([
            0x61,
            (length >> 8) as u8,
            length as u8,
            0x61,
            (offset >> 8) as u8,
            offset as u8,
            0x60,
            0,
            0x39,
            0x61,
            (length >> 8) as u8,
            length as u8,
            0x60,
            0,
            0xf3,
        ]);
        init.extend(runtime);
        let gauge = c.deploy_listed("Synthetic core gauge", &init, &[], 8_000_000).await?;
        println!(
            "{}",
            json!({"gauge":gauge.to_string(),"code_hash":runtime_hash,"pair":pair.to_string(),"reward":reward.to_string(),
            "scope":"exact core runtime with synthetic constructor storage; no production/source-equivalence claim",
            "storage":storage.iter().map(|(slot,value)|(format!("0x{}",hex::encode(slot)),format!("0x{}",hex::encode(value)))).collect::<Vec<_>>()})
        );
        return Ok(());
    }
    let with_args = |code: Vec<u8>, words: &[Vec<u8>]| -> Vec<u8> {
        let mut out = code;
        for w in words {
            out.extend_from_slice(w);
        }
        out
    };

    // DEVNET_RESUME=<json from a partial run> skips the swap and Zora core deployments.
    let resume: Option<Value> = std::env::var("DEVNET_RESUME").ok().and_then(|t| serde_json::from_str(&t).ok());
    let erc20 = [
        "function approve(address spender, uint256 amount) returns (bool)",
        "function transfer(address to, uint256 amount) returns (bool)",
    ];
    let (wquai, factory, router, tusd, tka, fee_settings, manager, erc20_helper, erc721_helper) = if let Some(r) = &resume {
        let a = |k: &str| -> Result<QuaiAddress, Err> { Ok(r[k].as_str().ok_or("resume key")?.parse()?) };
        (
            a("wquai")?,
            a("factory")?,
            a("router")?,
            a("tusd")?,
            a("tka")?,
            a("zora_fee_settings")?,
            a("zora_module_manager")?,
            a("zora_erc20_helper")?,
            a("zora_erc721_helper")?,
        )
    } else {
        let wquai = c.deploy("WQUAI", &hex_bytes(&code["wquai"])?, &empty, &[]).await?;
        let factory = c.deploy("Quainance factory", &hex_bytes(&code["quainance_factory"])?, &empty, &[]).await?;
        let router = c
            .deploy(
                "Quainance router",
                &with_args(hex_bytes(&code["quainance_router"])?, &[word_address(factory), word_address(wquai)]),
                &empty,
                &[],
            )
            .await?;
        let token_abi = AbiInterface::from_human_readable(&["constructor(string name, string symbol, uint8 decimals, uint256 supply)"])?;
        let tusd = c
            .deploy(
                "TUSD",
                &hex_bytes(&art["test_token"])?,
                &token_abi,
                &[json!("Test USD"), json!("TUSD"), json!("6"), json!("1000000000000000")],
            )
            .await?;
        let tka = c
            .deploy(
                "TKA",
                &hex_bytes(&art["test_token"])?,
                &token_abi,
                &[json!("Test Alpha"), json!("TKA"), json!("18"), json!("1000000000000000000000000000")],
            )
            .await?;
        let max = U256::MAX.to_string();
        c.call("approve TUSD", tusd, &erc20, "approve", &[json!(router.to_string()), json!(max)], U256::ZERO).await?;
        c.call("approve TKA", tka, &erc20, "approve", &[json!(router.to_string()), json!(max)], U256::ZERO).await?;
        let router_abi = [
            "function addLiquidityETH(address token, uint256 amountTokenDesired, uint256 amountTokenMin, uint256 amountETHMin, address to, uint256 deadline) payable returns (uint256, uint256, uint256)",
            "function addLiquidity(address tokenA, address tokenB, uint256 amountADesired, uint256 amountBDesired, uint256 amountAMin, uint256 amountBMin, address to, uint256 deadline) returns (uint256, uint256, uint256)",
        ];
        let deadline = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs() + 3600).to_string();
        // QUAI/TUSD at 100 TUSD per QUAI; TKA/TUSD at 1:1.
        c.call(
            "pool QUAI/TUSD",
            router,
            &router_abi,
            "addLiquidityETH",
            &[json!(tusd.to_string()), json!("1000000000000"), json!("0"), json!("0"), json!(sender.to_string()), json!(deadline)],
            U256::from(10_000u128 * 10u128.pow(18)),
        )
        .await?;
        c.call(
            "pool TKA/TUSD",
            router,
            &router_abi,
            "addLiquidity",
            &[
                json!(tka.to_string()),
                json!(tusd.to_string()),
                json!("100000000000000000000000"),
                json!("100000000000"),
                json!("0"),
                json!("0"),
                json!(sender.to_string()),
                json!(deadline),
            ],
            U256::ZERO,
        )
        .await?;

        if trading_only {
            let wqi = c.deploy_listed("WQI", &hex_bytes(&code["wqi"])?, &[], 4_000_000).await?;
            println!(
                "{}",
                json!({"scope":"trading-only", "deployer":sender.to_string(), "wquai":wquai.to_string(),
                "wqi":wqi.to_string(), "factory":factory.to_string(), "router":router.to_string(),
                "tusd":tusd.to_string(), "tka":tka.to_string()})
            );
            return Ok(());
        }
        let fee_settings = c.deploy("Zora fee settings", &hex_bytes(&code["zora_fee_settings"])?, &empty, &[]).await?;
        let manager = c
            .deploy(
                "Zora module manager",
                &with_args(hex_bytes(&code["zora_module_manager"])?, &[word_address(sender), word_address(fee_settings)]),
                &empty,
                &[],
            )
            .await?;
        let erc20_helper = c
            .deploy("Zora ERC-20 helper", &with_args(hex_bytes(&code["zora_erc20_helper"])?, &[word_address(manager)]), &empty, &[])
            .await?;
        let erc721_helper = c
            .deploy("Zora ERC-721 helper", &with_args(hex_bytes(&code["zora_erc721_helper"])?, &[word_address(manager)]), &empty, &[])
            .await?;
        eprintln!(
            "resume with DEVNET_RESUME='{}'",
            json!({"wquai": wquai.to_string(), "factory": factory.to_string(), "router": router.to_string(), "tusd": tusd.to_string(), "tka": tka.to_string(), "zora_fee_settings": fee_settings.to_string(), "zora_module_manager": manager.to_string(), "zora_erc20_helper": erc20_helper.to_string(), "zora_erc721_helper": erc721_helper.to_string()})
        );
        (wquai, factory, router, tusd, tka, fee_settings, manager, erc20_helper, erc721_helper)
    };
    let asks = match resume.as_ref().and_then(|r| r["zora_asks"].as_str()) {
        Some(existing) => existing.parse::<QuaiAddress>()?,
        None => {
            c.deploy_listed(
                "Zora Asks v1.1",
                &with_args(
                    hex_bytes(&code["zora_asks"])?,
                    &[
                        word_address(erc20_helper),
                        word_address(erc721_helper),
                        vec![0u8; 32],
                        word_address(fee_settings),
                        word_address(wquai),
                    ],
                ),
                &[manager, erc721_helper, erc20_helper, fee_settings],
                4_500_000,
            )
            .await?
        }
    };
    // As on mainnet: the fee settings token is minted by the module manager.
    let initialized = c
        .call_listed(
            "init fee settings",
            fee_settings,
            &["function init(address minter, address metadataRenderer)"],
            "init",
            &[json!(manager.to_string()), json!("0x0000000000000000000000000000000000000000")],
            U256::ZERO,
        )
        .await;
    if let Err(e) = initialized {
        eprintln!("fee settings init skipped: {e}");
    }
    c.call_listed(
        "register Asks module",
        manager,
        &["function registerModule(address module)"],
        "registerModule",
        &[json!(asks.to_string())],
        U256::ZERO,
    )
    .await?;

    // WQI: the mainnet creation code as served (its constructor arguments are included). Only its
    // EIP-712 permit cache differs from mainnet (chain id and address), which wrapping never uses.
    let wqi = match resume.as_ref().and_then(|r| r["wqi"].as_str()) {
        Some(existing) => existing.parse::<QuaiAddress>()?,
        None => c.deploy_listed("WQI", &hex_bytes(&code["wqi"])?, &[], 4_000_000).await?,
    };

    let nft = c.deploy_listed("TestNFT", &hex_bytes(&art["test_nft"])?, &[], 3_000_000).await?;
    let nft_abi = ["function mint(address to, uint256 tokenId)", "function setApprovalForAll(address operator, bool approved)"];
    for id in ["1", "2", "3"] {
        c.call_listed(&format!("mint NFT {id}"), nft, &nft_abi, "mint", &[json!(sender.to_string()), json!(id)], U256::ZERO).await?;
    }
    c.call_listed(
        "approve ERC-721 helper",
        nft,
        &nft_abi,
        "setApprovalForAll",
        &[json!(erc721_helper.to_string()), json!(true)],
        U256::ZERO,
    )
    .await?;
    c.call_listed(
        "approve Asks module",
        manager,
        &["function setApprovalForModule(address module, bool approved)"],
        "setApprovalForModule",
        &[json!(asks.to_string()), json!(true)],
        U256::ZERO,
    )
    .await?;
    let asks_abi = [
        "function createAsk(address tokenContract, uint256 tokenId, uint256 askPrice, address askCurrency, address sellerFundsRecipient, uint16 findersFeeBps)",
    ];
    c.call_listed(
        "list NFT 1 for 5 QUAI",
        asks,
        &asks_abi,
        "createAsk",
        &[
            json!(nft.to_string()),
            json!("1"),
            json!("5000000000000000000"),
            json!("0x0000000000000000000000000000000000000000"),
            json!(sender.to_string()),
            json!("0"),
        ],
        U256::ZERO,
    )
    .await?;
    // A second listing priced in TUSD exercises the token-approval steps.
    c.call_listed(
        "list NFT 2 for 50 TUSD",
        asks,
        &asks_abi,
        "createAsk",
        &[json!(nft.to_string()), json!("2"), json!("50000000"), json!(tusd.to_string()), json!(sender.to_string()), json!("0")],
        U256::ZERO,
    )
    .await?;
    // Optional: send test tokens to a wallet under test.
    if let Some(recipient) = std::env::args().nth(3) {
        let to: QuaiAddress = recipient.parse()?;
        c.call_listed("fund TUSD", tusd, &erc20, "transfer", &[json!(to.to_string()), json!("500000000")], U256::ZERO).await?;
        c.call_listed("fund TKA", tka, &erc20, "transfer", &[json!(to.to_string()), json!("1000000000000000000000")], U256::ZERO).await?;
    }
    println!(
        "{}",
        json!({
            "deployer": sender.to_string(),
            "wquai": wquai.to_string(),
            "wqi": wqi.to_string(),
            "factory": factory.to_string(),
            "router": router.to_string(),
            "tusd": tusd.to_string(),
            "tka": tka.to_string(),
            "zora_fee_settings": fee_settings.to_string(),
            "zora_module_manager": manager.to_string(),
            "zora_erc20_helper": erc20_helper.to_string(),
            "zora_erc721_helper": erc721_helper.to_string(),
            "zora_asks": asks.to_string(),
            "nft": nft.to_string(),
        })
    );
    Ok(())
}
