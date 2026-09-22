//! Deterministic JSON-RPC identity regressions. Contracts are explicitly configured without code
//! pins here: these tests isolate pair/factory membership and token units, not deployment hashing.
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wallet_core::appdb::AppDb;
use wallet_core::config::DataPolicy;
use wallet_core::data::DataCtx;
use wallet_core::network::{NetworkProfile, PinnedContract};

const PAIR: &str = "0x0000000000000000000000000000000000000010";
const TOKEN0: &str = "0x0000000000000000000000000000000000000011";
const TOKEN1: &str = "0x0000000000000000000000000000000000000012";
const FACTORY: &str = "0x0000000000000000000000000000000000000020";
const ROUTER: &str = "0x0000000000000000000000000000000000000030";

fn address_word(address: &str) -> String {
    format!("0x{:0>64}", &address[2..])
}
fn words(values: &[u128]) -> String {
    format!("0x{}", values.iter().map(|v| format!("{v:064x}")).collect::<String>())
}
fn string_word(value: &str) -> String {
    let mut bytes = value.as_bytes().to_vec();
    bytes.resize(bytes.len().div_ceil(32) * 32, 0);
    format!("0x{:064x}{:064x}{}", 32, value.len(), hex::encode(bytes))
}

type Requests = Arc<Mutex<Vec<Value>>>;
async fn fixture(wrong_pair: bool, missing_decimals: bool) -> (DataCtx, tokio::task::JoinHandle<()>) {
    let (ctx, server, _) = traced_fixture(wrong_pair, missing_decimals).await;
    (ctx, server)
}
async fn traced_fixture(wrong_pair: bool, missing_decimals: bool) -> (DataCtx, tokio::task::JoinHandle<()>, Requests) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let observed = requests.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let observed = observed.clone();
            tokio::spawn(async move {
                let mut input = Vec::new();
                let body = loop {
                    let mut buffer = [0u8; 4096];
                    let n = socket.read(&mut buffer).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    input.extend_from_slice(&buffer[..n]);
                    if let Some(end) = input.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&input[..end]);
                        let len: usize = headers
                            .lines()
                            .find_map(|line| {
                                let (key, value) = line.split_once(':')?;
                                key.eq_ignore_ascii_case("content-length").then(|| value.trim().parse().unwrap())
                            })
                            .unwrap();
                        if input.len() >= end + 4 + len {
                            break input[end + 4..end + 4 + len].to_vec();
                        }
                    }
                };
                let request: Value = serde_json::from_slice(&body).unwrap();
                let respond = |request: &Value| {
                    observed.lock().unwrap().push(request.clone());
                    if request["method"] == "quai_getHeaderByNumber" {
                        return json!({"jsonrpc":"2.0","id":request["id"],"result":{
                            "gasLimit":"0x10000","stateLimit":"0x10000","woHeader":{
                                "hash":format!("0x{}","11".repeat(32)),"parentHash":format!("0x{}","00".repeat(32)),
                                "number":"0x64","primeTerminusNumber":"0x4","location":"0x0000"
                            }
                        }});
                    }
                    if request["method"] == "quai_chainId" {
                        return json!({"jsonrpc":"2.0", "id":request["id"], "result":"0x9"});
                    }
                    let call = &request["params"][0];
                    let data = call["data"].as_str().or_else(|| call["input"].as_str()).unwrap_or("");
                    let selector = data.get(..10).unwrap_or(data);
                    let token0 = call["to"].as_str().is_some_and(|a| a.eq_ignore_ascii_case(TOKEN0));
                    let result = match selector {
                        "0x0dfe1681" => Some(address_word(TOKEN0)),
                        "0xd21220a7" | "0xad5c4648" => Some(address_word(TOKEN1)),
                        "0xc45a0155" => Some(address_word(FACTORY)),
                        "0xe6a43905" => Some(address_word(if wrong_pair { ROUTER } else { PAIR })),
                        "0x95d89b41" | "0x06fdde03" => Some(string_word(if token0 { "USDT" } else { "WQUAI" })),
                        "0x313ce567" if !missing_decimals || !token0 => Some(words(&[if token0 { 6 } else { 18 }])),
                        "0x0902f1ac" => Some(words(&[1_000_000_000, 1_000_000_000_000_000_000_000, 1])),
                        "0x18160ddd" => Some(words(&[1_000_000_000_000_000_000_000])),
                        "0xdd62ed3e" => Some(words(&[0])),
                        _ => None,
                    };
                    match result {
                        Some(result) => json!({"jsonrpc":"2.0", "id":request["id"], "result":result}),
                        None => {
                            json!({"jsonrpc":"2.0", "id":request["id"], "error":{"code":-32000,"message":format!("fixture refuses read {selector} method {}", request["method"])}})
                        }
                    }
                };
                let response = if let Some(requests) = request.as_array() {
                    Value::Array(requests.iter().map(respond).collect())
                } else {
                    respond(&request)
                }
                .to_string();
                let head = format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", response.len());
                socket.write_all(head.as_bytes()).await.unwrap();
                socket.write_all(response.as_bytes()).await.unwrap();
            });
        }
    });
    let mut network = NetworkProfile::builtins().remove(0);
    network.rpc_url = endpoint;
    network.use_pathing = false;
    network.wquai = Some(TOKEN1.into());
    network.ecosystem.quainance_factory = Some(PinnedContract { address: FACTORY.into(), code_hash: None });
    network.ecosystem.quainance_router = Some(PinnedContract { address: ROUTER.into(), code_hash: None });
    network.ecosystem.launch_amm_factory = None;
    network.ecosystem.launch_amm_router = None;
    network.ecosystem.legacy_factory = None;
    network.ecosystem.legacy_router = None;
    network.ecosystem.multicall3 = None;
    network.ecosystem.hartii_amm_factory = None;
    network.ecosystem.hartii_amm_router = None;
    let ctx = DataCtx::with_app(AppDb::memory().unwrap(), network, DataPolicy::OFFLINE).unwrap().for_review();
    (ctx, server, requests)
}

#[tokio::test]
async fn lp_quote_uses_authenticated_pair_tokens_and_six_decimal_units_without_an_indexer() {
    let (ctx, server) = fixture(false, false).await;
    let quote = wallet_core::liquidity::quote(&ctx, PAIR, "1", Some("USDT"), 50, None).await.unwrap();
    assert_eq!(quote.token0.address, TOKEN0);
    assert_eq!(quote.token0.decimals, 6);
    assert_eq!(quote.amount0.to_string(), "1000000");
    assert_eq!(quote.amount1.to_string(), "1000000000000000000");
    server.abort();
}

#[tokio::test]
async fn lp_quote_refuses_a_pair_not_authenticated_by_the_pinned_factory() {
    let (ctx, server) = fixture(true, false).await;
    let error = wallet_core::liquidity::quote(&ctx, PAIR, "1", None, 50, None).await.unwrap_err();
    assert!(error.to_string().contains("does not authenticate"), "{error}");
    server.abort();
}

#[tokio::test]
async fn lp_quote_refuses_unreadable_decimals_instead_of_sizing_six_decimal_input_as_eighteen() {
    let (ctx, server) = fixture(false, true).await;
    let error = wallet_core::liquidity::quote(&ctx, PAIR, "1", None, 50, None).await.unwrap_err();
    assert!(!error.to_string().is_empty());
    server.abort();
}

#[tokio::test]
async fn selected_lp_quote_rpc_count_and_payload_do_not_scale_with_unrelated_directory_rows() {
    let mut baseline = None;
    for count in [10, 100, 1000] {
        let (ctx, server, requests) = traced_fixture(false, false).await;
        let unrelated:Vec<Value>=(0..count).map(|i|json!({"address":format!("0x{:040x}",0x1000+i),"token0":{"address":TOKEN1,"symbol":"UNRELATED","decimals":18},"token1":{"address":TOKEN0,"symbol":"OTHER","decimals":6},"venue":"Main"})).collect();
        for key in ["pools", "launch_amm_pools_v2:unrelated", "legacy_pools:unrelated"] {
            ctx.app.cache_put(&format!("{}:{key}", ctx.network.id), &serde_json::to_string(&unrelated).unwrap()).unwrap();
        }
        let quote = wallet_core::liquidity::quote(&ctx, PAIR, "1", Some("USDT"), 50, Some("0x0000000000000000000000000000000000000040"))
            .await
            .unwrap();
        let calls = requests.lock().unwrap().clone();
        let mut normalized: Vec<Value> = calls.iter().map(|r| json!({"method":r["method"],"params":r["params"]})).collect();
        normalized.sort_by_key(Value::to_string);
        let payload = serde_json::to_value(&quote).unwrap();
        if let Some((expected_calls, expected_payload)) = &baseline {
            assert_eq!(&normalized, expected_calls, "directory size {count} must not add requests or change execution reads");
            assert_eq!(&payload, expected_payload, "directory size {count} must not alter selected LP amounts, token units or minima");
        } else {
            baseline = Some((normalized, payload));
        }
        for call in calls.iter().filter(|r| r["method"] == "quai_call") {
            let to = call["params"][0]["to"].as_str().unwrap();
            assert!(
                [PAIR, TOKEN0, TOKEN1, FACTORY, ROUTER].iter().any(|known| known.eq_ignore_ascii_case(to)),
                "unrelated target read: {to}"
            );
            let data = call["params"][0]["data"].as_str().or_else(|| call["params"][0]["input"].as_str()).unwrap();
            if ["0x0902f1ac", "0x18160ddd", "0x313ce567", "0xdd62ed3e"].iter().any(|s| data.starts_with(s)) {
                assert_eq!(call["params"][1], "0x64", "reserves/supply/required units/allowances share the captured block");
            }
        }
        server.abort();
    }
}
