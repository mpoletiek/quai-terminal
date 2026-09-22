//! Seed a tiny WQI/native pool only in the owned disposable trading fixture.
//! Initial-price setting is fixture setup, not a supported user liquidity workflow.
use quai_sdk::{BlockTag, U256, contracts::Contract};
use serde_json::{Value, json};
use wallet_core::{appdb::OpStatus, config::AppConfig, paths::Paths, registry::Registry, session::Session, tx::AccountRequest};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::path::PathBuf::from(std::env::args().nth(1).ok_or("fixture root required")?).canonicalize()?;
    if !root.starts_with("/tmp") || std::fs::read_to_string(root.join(".owned"))?.trim() != "quai-terminal-disposable-trading-v1" {
        return Err("unowned fixture".into());
    }
    let paths = Paths::resolve(Some(root.join("wallet")))?;
    let config = AppConfig::load(&paths)?;
    let network = config.network("trading-fixture")?.clone();
    if network.chain_id != 1337
        || network.rpc_url != "http://127.0.0.1:19200"
        || network.genesis != "0xff38a93744ee5aae738addc88da4f6b171528244e81d34aa4b25579fa3f44ed2"
    {
        return Err("not the isolated fixture chain".into());
    }
    let registry = Registry::new(paths);
    let meta = registry.resolve(Some("trader"), None)?;
    let mut session = Session::open(registry, config, meta, network)?;
    session.verify_node().await?;
    session.unlock(std::fs::read_to_string(root.join("password"))?.trim())?;
    let owner = session.account(None)?;
    let wqi = session.network.wqi.clone().ok_or("WQI missing")?;
    let router = session.network.ecosystem.quainance_router.as_ref().ok_or("router missing")?.address.clone();
    let deposit = U256::from(400_000_000_000_000_000u64);
    let deadline = wallet_core::registry::now() + 1800;
    let abi = quai_sdk::abi::AbiInterface::from_human_readable(&[
        "function approve(address,uint256) returns (bool)",
        "function addLiquidityETH(address,uint256,uint256,uint256,address,uint256) payable returns (uint256,uint256,uint256)",
    ])?;
    let factory = session.network.ecosystem.quainance_factory.as_ref().ok_or("factory missing")?.address.parse()?;
    let factory =
        Contract::new(factory, quai_sdk::abi::AbiInterface::from_human_readable(wallet_core::swap::FACTORY_ABI)?, &session.node.provider);
    let pair = factory.call(owner.address.parse()?, "getPair", &[json!(wqi), json!(session.network.wquai)], BlockTag::Latest).await?;
    if pair.first().and_then(Value::as_str).is_some_and(|a| a != "0x0000000000000000000000000000000000000000") {
        println!("{}", json!({"existing_pair":pair}));
        return Ok(());
    }
    for (target, method, args, value, detail) in [
        (wqi.clone(), "approve", vec![json!(router), json!(deposit.to_string())], U256::ZERO, json!({"token":wqi,"spender":router})),
        (
            router.clone(),
            "addLiquidityETH",
            vec![json!(wqi), json!(deposit.to_string()), json!("0"), json!("0"), json!(owner.address), json!(deadline.to_string())],
            U256::from(10).pow(U256::from(18)),
            json!({"financial_effects":[{"direction":"out","asset":"WQI","token":wqi,"decimals":18,"amount":deposit.to_string()}]}),
        ),
    ] {
        let contract = Contract::new(target.parse()?, abi.clone(), &session.node.provider);
        let call = contract.prepare(method, &args, value)?;
        let call = wallet_core::data::with_access_list(&session.node.provider, owner.address.parse()?, call).await?;
        let review = session
            .prepare_account(AccountRequest {
                from: owner.clone(),
                intent: call.into_account_intent(),
                kind: if method == "approve" { "approve" } else { "fixture_seed" }.into(),
                title: format!("Isolated fixture {method}"),
                asset: "WQI".into(),
                amount: deposit,
                decimals: 18,
                counterparty: target,
                fields: vec![],
                warnings: vec!["Synthetic local test liquidity only".into()],
                detail,
                max_gas: 4_000_000,
                max_fee: None,
            })
            .await?;
        let submitted = session.commit(&review.op_id).await?;
        let start = std::time::Instant::now();
        loop {
            session.track().await?;
            let operation = session.app.operation(&submitted.op_id)?.ok_or("operation missing")?;
            if operation.status == OpStatus::Confirmed {
                break;
            }
            if operation.status == OpStatus::Failed || start.elapsed().as_secs() > 180 {
                return Err(format!("{method}: {}", operation.status.as_str()).into());
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }
    println!(
        "{}",
        json!({"scope":"synthetic WQI/native pool seed","wqi":wqi,"router":router,"wqi_atoms":deposit.to_string(),"native_atoms":"1000000000000000000"})
    );
    Ok(())
}
