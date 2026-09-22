//! Qualify runtime pins only for contracts deployed by the owned synthetic trading fixture.
//! This never signs or broadcasts. It refuses user homes and every non-fixture network.
use quai_sdk::{BlockTag, U256, Zone};
use serde_json::{Value, json};
use wallet_core::{config::AppConfig, paths::Paths};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::path::PathBuf::from(std::env::args().nth(1).ok_or("owned fixture root required")?).canonicalize()?;
    if !root.starts_with("/tmp") || std::fs::read_to_string(root.join(".owned"))?.trim() != "quai-terminal-disposable-trading-v1" {
        return Err("unowned fixture root".into());
    }
    let deployment: Value = serde_json::from_str(&std::fs::read_to_string(root.join("evidence/deployment.json"))?)?;
    if deployment["scope"] != "trading-only" {
        return Err("not a trading-only deployment".into());
    }
    let paths = Paths::resolve(Some(root.join("wallet")))?;
    let mut config = AppConfig::load(&paths)?;
    let network = config.networks.iter_mut().find(|n| n.id == "trading-fixture").ok_or("no fixture network")?;
    if network.chain_id != 1337
        || network.genesis != "0xff38a93744ee5aae738addc88da4f6b171528244e81d34aa4b25579fa3f44ed2"
        || network.rpc_url != "http://127.0.0.1:19200"
    {
        return Err("unqualified fixture identity".into());
    }
    let node = network.node()?;
    if node.provider.chain_id(Zone::Cyprus1.into()).await? != U256::from(1337)
        || node.provider.genesis_hash(Zone::Cyprus1).await? != network.genesis_hash()?
    {
        return Err("fixture identity changed".into());
    }
    let mut pins = serde_json::Map::new();
    for key in ["router", "factory", "tusd", "wquai", "wqi"] {
        let address = deployment[key].as_str().ok_or("missing deployed contract")?;
        let code = node.provider.code(address.parse()?, BlockTag::Latest).await?;
        if code.bytes().is_empty() {
            return Err(format!("{key} has no deployed code").into());
        }
        let hash = format!("0x{}", hex::encode(quai_sdk::crypto::keccak256(code.bytes())));
        pins.insert(key.into(), json!({"address":address,"code_hash":hash}));
        match key {
            "router" => {
                let pin = network.ecosystem.quainance_router.as_mut().ok_or("missing router")?;
                if !pin.address.eq_ignore_ascii_case(address) {
                    return Err("router changed".into());
                }
                pin.code_hash = Some(hash);
            }
            "factory" => {
                let pin = network.ecosystem.quainance_factory.as_mut().ok_or("missing factory")?;
                if !pin.address.eq_ignore_ascii_case(address) {
                    return Err("factory changed".into());
                }
                pin.code_hash = Some(hash);
            }
            "tusd" => {
                let pin = network.ecosystem.usdt.as_mut().ok_or("missing USDT")?;
                if !pin.address.eq_ignore_ascii_case(address) {
                    return Err("USDT changed".into());
                }
                pin.code_hash = Some(hash);
            }
            "wquai" => {
                if !network.wquai.as_ref().is_some_and(|a| a.eq_ignore_ascii_case(address)) {
                    return Err("wrapper changed".into());
                }
                network.ecosystem.wquai_code_hash = Some(hash);
            }
            "wqi" => {
                if !network.wqi.as_ref().is_some_and(|a| a.eq_ignore_ascii_case(address)) {
                    return Err("wrapper changed".into());
                }
                network.ecosystem.wqi_code_hash = Some(hash);
            }
            _ => unreachable!(),
        }
    }
    let gauge_fixture = root.join("evidence/gauge-deployment.json");
    if gauge_fixture.is_file() {
        let gauge: Value = serde_json::from_str(&std::fs::read_to_string(gauge_fixture)?)?;
        let address = gauge["gauge"].as_str().ok_or("gauge fixture address")?;
        let code = node.provider.code(address.parse()?, BlockTag::Latest).await?;
        let hash = format!("0x{}", hex::encode(quai_sdk::crypto::keccak256(code.bytes())));
        if gauge["code_hash"] != hash {
            return Err("synthetic gauge runtime changed".into());
        }
        network.ecosystem.quainance_gauge =
            Some(wallet_core::network::PinnedContract { address: address.into(), code_hash: Some(hash.clone()) });
        pins.insert("core_gauge".into(), json!({"address":address,"code_hash":hash}));
    }
    let hartii_fixture = root.join("evidence/hartii-deployment.json");
    if hartii_fixture.is_file() {
        let hartii: Value = serde_json::from_str(&std::fs::read_to_string(hartii_fixture)?)?;
        for key in ["router", "factory"] {
            let address = hartii[key].as_str().ok_or("Hartii fixture address")?;
            let code = node.provider.code(address.parse()?, BlockTag::Latest).await?;
            let hash = format!("0x{}", hex::encode(quai_sdk::crypto::keccak256(code.bytes())));
            if hartii[format!("{key}_code_hash")] != hash {
                return Err("synthetic Hartii runtime changed".into());
            }
            let pin = Some(wallet_core::network::PinnedContract { address: address.into(), code_hash: Some(hash.clone()) });
            if key == "router" {
                network.ecosystem.hartii_amm_router = pin;
            } else {
                network.ecosystem.hartii_amm_factory = pin;
            }
            pins.insert(format!("hartii_{key}"), json!({"address":address,"code_hash":hash}));
        }
    }
    config.save(&paths)?;
    println!("{}", json!({"scope":"synthetic local deployments only; not production token qualification","pins":pins}));
    Ok(())
}
