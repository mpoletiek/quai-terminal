//! Development-only: deploy hash-pinned WQUAI/WQI creation bytecode to a local dev chain
//! using the PUBLIC fixture key 0x325. Refuses any chain other than 1337 on loopback.
//!
//! Usage: devnet_deploy <wquai.json|wqi.json> [rpc]

use quai_sdk::abi::AbiInterface;
use quai_sdk::accounts::{AccountObservationPolicy, AccountSession, FeePolicy};
use quai_sdk::contracts::{DeploymentSearch, prepare_deployment};
use quai_sdk::provider::WaitConfig;
use quai_sdk::signer::{LocalSigner, Signer};
use quai_sdk::wallet::storage::{NetworkScope, PublicAddress, ReservationId, SqliteStore};
use quai_sdk::{HttpConfig, HttpTransport, Provider, QuaiAddress, Routing, U256, Zone};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let artifact = std::env::args().nth(1).ok_or("artifact path required")?;
    let rpc = std::env::args().nth(2).unwrap_or_else(|| "http://127.0.0.1:19200".into());
    if !rpc.starts_with("http://127.0.0.1") {
        return Err("devnet_deploy only targets loopback dev chains".into());
    }
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&artifact)?)?;
    let creation = hex::decode(json["creation_bytecode"].as_str().ok_or("creation")?.trim_start_matches("0x"))?;
    let provider =
        Provider::new(HttpTransport::new(HttpConfig::default())?, Routing::direct(&rpc, Zone::Cyprus1.into())?, U256::from(1337));
    let genesis = provider.genesis_hash(Zone::Cyprus1).await?;
    let scope = NetworkScope { chain_id: U256::from(1337), genesis, zone: Zone::Cyprus1 };
    let mut bytes = [0u8; 32];
    bytes[31] = 0x25;
    bytes[30] = 0x03;
    let key = quai_sdk::crypto::SecretKey::from_bytes(&bytes)?;
    let public = PublicAddress::imported(&key.public_key())?;
    let signer = LocalSigner::new(key, scope.chain_id)?;
    let sender: QuaiAddress = signer.address().try_into()?;
    let dir = std::env::temp_dir().join("quai-terminal-devnet-deploy");
    std::fs::create_dir_all(&dir)?;
    let mut store = SqliteStore::open(dir.join(format!("deploy-{}.sqlite", genesis)), scope)?;
    if store.addresses()?.is_empty() {
        store.import_metadata(0, &[public])?;
    }
    let mut id = [0u8; 16];
    quai_sdk::crypto::fill_random(&mut id)?;
    let id = ReservationId(id);
    let mut session = AccountSession::new(&provider, &signer, &mut store)?.with_observation_policy(AccountObservationPolicy::PinnedLatest);
    let nonce = session.reserve_deployment_nonce(id).await?;
    let deployment = prepare_deployment(
        &AbiInterface::from_json(b"[]")?,
        &creation,
        &[],
        sender,
        scope.chain_id,
        nonce,
        U256::ZERO,
        DeploymentSearch { start_salt: 0, max_attempts: 10_000 },
        || false,
    )?;
    let address = deployment.address();
    let policy = FeePolicy {
        max_gas: 8_000_000,
        max_gas_price: U256::from(10_000_000_000_000_000u64),
        max_total_fee: U256::from(100_000_000_000_000_000_000_000u128),
        gas_margin_bps: 2000,
    };
    let prepared = session.prepare_deployment(id, deployment, policy).await?;
    let signed = session.sign(&prepared)?;
    session.broadcast(id).await?;
    let hash = signed.hash()?;
    let observed = provider
        .wait_for_receipt(Zone::Cyprus1, hash, WaitConfig::new(1, std::time::Duration::from_secs(120), std::time::Duration::from_secs(2)))
        .await?;
    let code = provider.code(address, quai_sdk::BlockTag::Latest).await?;
    println!(
        "{}",
        serde_json::json!({"contract": address.to_string(), "tx": hash.to_string(), "outcome": format!("{:?}", observed.receipt.outcome), "codeBytes": code.bytes().len()})
    );
    Ok(())
}
