//! Development-only: post an arbitrary body to the message board from a wallet's first account,
//! the way an attacker would re-post a body lifted off the chain. The wallet never offers this;
//! the messaging end-to-end test uses it to show a copied message does not open. Refuses anything
//! but chain 1337 over loopback.
//!
//! Usage: devnet_board_raw <home> <wallet> <network> <password-file> <tag hex> <kind> <body hex>

use quai_sdk::{U256, contracts::Contract};
use wallet_core::{appdb::OpStatus, config::AppConfig, paths::Paths, registry::Registry, session::Session, tx::AccountRequest};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [home, wallet, network, password, tag, kind, body] = args.as_slice() else {
        return Err("usage: devnet_board_raw <home> <wallet> <network> <password-file> <tag hex> <kind> <body hex>".into());
    };
    let paths = Paths::resolve(Some(home.into()))?;
    let config = AppConfig::load(&paths)?;
    let network = config.network(network)?.clone();
    if network.chain_id != 1337 || !network.rpc_url.starts_with("http://127.0.0.1:") {
        return Err("devnet_board_raw only posts on a loopback chain 1337".into());
    }
    let registry = Registry::new(paths);
    let meta = registry.resolve(Some(wallet), None)?;
    let mut session = Session::open(registry, config, meta, network)?;
    session.verify_node().await?;
    session.unlock(std::fs::read_to_string(password)?.trim())?;
    let from = session.account(None)?;
    let board = session.network.ecosystem.messages.as_ref().ok_or("no board")?.address.parse()?;
    let tag: [u8; 32] = hex::decode(tag.trim_start_matches("0x"))?.try_into().map_err(|_| "tag must be 32 bytes")?;
    let body = hex::decode(body.trim_start_matches("0x"))?;
    let args = wallet_core::messages::post_args(&tag, kind.parse()?, &body)?;
    let contract = Contract::new(board, wallet_core::messages::interface()?, &session.node.provider);
    let call = contract.prepare("post", &args, U256::ZERO)?;
    let review = session
        .prepare_account(AccountRequest {
            from: from.clone(),
            intent: call.into_account_intent(),
            kind: wallet_core::journal::OpKind::BoardPost,
            title: "Raw board post (dev chain)".into(),
            asset: "QUAI".into(),
            amount: U256::ZERO,
            decimals: 18,
            counterparty: "raw".into(),
            fields: vec![],
            warnings: vec![],
            detail: serde_json::json!({"raw": true}).into(),
            max_gas: 400_000,
            max_fee: None,
        })
        .await?;
    let submitted = session.commit(&review.op_id).await?;
    let start = std::time::Instant::now();
    loop {
        session.track().await?;
        let op = session.app.operation(&submitted.op_id)?.ok_or("operation missing")?;
        match op.status {
            OpStatus::Confirmed => break,
            OpStatus::Failed => return Err("raw post failed".into()),
            _ if start.elapsed().as_secs() > 180 => return Err("raw post not mined".into()),
            _ => tokio::time::sleep(std::time::Duration::from_secs(1)).await,
        }
    }
    println!("{}", serde_json::json!({"from": from.address, "op": submitted.op_id}));
    Ok(())
}
