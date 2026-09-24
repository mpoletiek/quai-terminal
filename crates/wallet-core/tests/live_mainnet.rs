//! Read-only checks against Quai mainnet and explorer.qu.ai. No transactions are sent.
//! Trading-only bounded probes: `cargo test -p wallet-core --test live_mainnet trading_readonly_ -- --ignored --nocapture --test-threads=1`.

use wallet_core::appdb::AppDb;
use wallet_core::config::DataPolicy;
use wallet_core::data::{DataCtx, Trust};
use wallet_core::network::NetworkProfile;

fn mainnet() -> DataCtx {
    let network = NetworkProfile::builtins().into_iter().find(|n| n.id == "mainnet").unwrap();
    DataCtx::with_app(AppDb::memory().unwrap(), network, DataPolicy { explorer: true, market: true, images: true, icons: true }).unwrap()
}

#[tokio::test]
#[ignore = "network"]
async fn pinned_ecosystem_bytecode_matches() {
    let ctx = mainnet();
    let eco = ctx.network.ecosystem.clone();
    for (name, c) in [
        ("router", eco.quainance_router),
        ("factory", eco.quainance_factory),
        ("launch AMM router", eco.launch_amm_router),
        ("launch AMM factory", eco.launch_amm_factory),
        ("gauge", eco.quainance_gauge),
        ("multicall3", eco.multicall3),
        ("usdt", eco.usdt),
        ("asks", eco.zora_asks),
        ("module manager", eco.zora_module_manager),
        ("erc721 helper", eco.zora_erc721_helper),
        ("erc20 helper", eco.zora_erc20_helper),
    ] {
        let c = c.unwrap();
        ctx.verify_pinned(&c, name).await.unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    for (name, address, hash) in [("WQI", &ctx.network.wqi, &eco.wqi_code_hash), ("WQUAI", &ctx.network.wquai, &eco.wquai_code_hash)] {
        let c = wallet_core::network::PinnedContract { address: address.clone().unwrap(), code_hash: hash.clone() };
        assert!(c.code_hash.is_some(), "{name} pin");
        ctx.verify_pinned(&c, name).await.unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    // A wrong hash is refused.
    let mut bad = ctx.network.ecosystem.quainance_router.clone().unwrap();
    bad.code_hash = Some(format!("0x{}", "11".repeat(32)));
    assert!(ctx.verify_pinned(&bad, "router").await.is_err());
}

/// With a monitoring node, every pin is proven at a block the network's RPC confirms, and a
/// review's worth of pins shares one confirmation. `QW_MONITOR_RPC=http://host:9200`.
#[tokio::test]
#[ignore = "network"]
async fn pins_are_proven_at_a_block_the_rpc_confirms() {
    use wallet_core::anchor::Confirmation;
    use wallet_core::data::{Trust, verify_pinned_all};
    let Ok(url) = std::env::var("QW_MONITOR_RPC") else {
        eprintln!("QW_MONITOR_RPC not set; skipping");
        return;
    };
    let ctx = mainnet();
    let rpc = ctx.network.node().unwrap();
    let monitor =
        wallet_core::network::NetworkProfile { rpc_url: url, use_pathing: false, monitor: None, ..ctx.network.clone() }.node().unwrap();
    let node = monitor.with_witness(rpc);
    let eco = ctx.network.ecosystem.clone();
    let pins: Vec<(wallet_core::network::PinnedContract, &str)> = [
        ("router", eco.quainance_router),
        ("factory", eco.quainance_factory),
        ("launch AMM router", eco.launch_amm_router),
        ("launch AMM factory", eco.launch_amm_factory),
        ("legacy router", eco.legacy_router),
        ("legacy factory", eco.legacy_factory),
        ("Hartii AMM router", eco.hartii_amm_router),
        ("Hartii AMM factory", eco.hartii_amm_factory),
        ("multicall3", eco.multicall3),
    ]
    .into_iter()
    .map(|(name, c)| (c.unwrap(), name))
    .collect();
    let refs: Vec<(&wallet_core::network::PinnedContract, &str)> = pins.iter().map(|(c, n)| (c, *n)).collect();
    let started = std::time::Instant::now();
    verify_pinned_all(&ctx.app, &node, &ctx.network, &refs, Trust::FirstHand).await.unwrap();
    let first = started.elapsed();
    assert_eq!(node.anchor_confirmation(), Some(Confirmation::Witnessed), "the RPC confirmed the monitor's block");
    let started = std::time::Instant::now();
    verify_pinned_all(&ctx.app, &node, &ctx.network, &refs, Trust::FirstHand).await.unwrap();
    eprintln!("9 pins: {first:?} with the confirmation, {:?} sharing it", started.elapsed());
}

/// The storage slots a review proves a curve's address from still hold what the launchers' own
/// calls return, at the same block, for the launches listed now. A launcher upgrade that moved them
/// would refuse every curve review; this says so first.
#[tokio::test]
#[ignore = "network"]
async fn curve_destination_slots_match_the_launchers_calls() {
    use wallet_core::capabilities::Family;
    use wallet_core::sdk::BlockTag;
    let ctx = mainnet();
    let launches = wallet_core::launches::launches(&ctx, 200).await.unwrap();
    let eco = ctx.network.ecosystem.clone();
    for (family, launcher, base, field, selector) in [
        (
            Family::QuainanceCurve,
            eco.curve_launcher.unwrap(),
            wallet_core::curve::LAUNCHES_SLOT,
            wallet_core::curve::LAUNCH_MARKET_FIELD,
            "launches(address)",
        ),
        (Family::HartiiCurve, eco.hartii_launcher.unwrap(), wallet_core::hartii_tx::CURVE_OF_SLOT, 0, "curveOf(address)"),
    ] {
        let tokens: Vec<_> = launches.iter().filter(|l| l.venue_kind == Some(family) && l.curve.is_some()).take(3).collect();
        assert!(!tokens.is_empty(), "{family:?}: no launches listed to check against");
        let launcher: wallet_core::sdk::QuaiAddress = launcher.address.parse().unwrap();
        for l in tokens {
            let token: wallet_core::sdk::QuaiAddress = l.token.parse().unwrap();
            let slot = wallet_core::anchor::mapping_field_slot(token, base, field);
            let (proven, _) =
                wallet_core::anchor::prove_state(&ctx.node, &ctx.network, &[(launcher, &[slot])], "launcher").await.unwrap().unwrap();
            let stored = wallet_core::anchor::word_address(proven[0].storage_value(slot).unwrap());
            let block = BlockTag::Number(wallet_core::sdk::U256::from(proven[0].block.number));
            let mut data = wallet_core::sdk::crypto::keccak256(selector.as_bytes())[..4].to_vec();
            data.extend_from_slice(&[0u8; 12]);
            data.extend_from_slice(token.bytes());
            let mut request = wallet_core::sdk::provider::CallRequest::new(wallet_core::data::READ_CALLER.parse().unwrap(), launcher);
            request.input = wallet_core::sdk::provider::RpcData::new(data).unwrap();
            let answer = ctx.node.provider.call(&request, block).await.unwrap();
            let word = |i: usize| format!("0x{}", hex::encode(&answer.bytes()[i * 32 + 12..(i + 1) * 32]));
            let called = word(if family == Family::QuainanceCurve { 1 } else { 0 });
            assert_eq!(stored, called, "{family:?} {}: the slot and the call disagree", l.symbol);
            assert!(called.eq_ignore_ascii_case(l.curve.as_deref().unwrap()), "{family:?} {}: listed curve differs", l.symbol);
            // And a review of it takes the curve from that proof, clones and all, and passes.
            let review = mainnet().for_review();
            wallet_core::curve::market(&review, &l.token, l.curve.as_deref().unwrap(), &[])
                .await
                .unwrap_or_else(|e| panic!("{family:?} {}: a first-hand read failed: {e}", l.symbol));
        }
    }
}

/// On every exchange, a trade's output computed from pools proven at one block equals the
/// exchange's own router quote at that block: the factory's `getPair` slot, the pair layout and the
/// 0.3% fee are what a review's minimum is checked against.
#[tokio::test]
#[ignore = "network"]
async fn a_route_s_output_is_proven_from_its_pools() {
    use wallet_core::markets::Venue;
    use wallet_core::sdk::{BlockTag, U256};
    let ctx = mainnet();
    let wquai = ctx.network.wquai.clone().unwrap().to_lowercase();
    let mut all = wallet_core::markets::pools(&ctx).await.unwrap().0;
    for directory in [
        wallet_core::markets::launch_amm_pools(&ctx).await,
        wallet_core::markets::legacy_pools(&ctx).await,
        wallet_core::markets::hartii_amm_pools(&ctx).await,
    ] {
        all.extend(directory.unwrap().pools);
    }
    for venue in [Venue::Main, Venue::LaunchAmm, Venue::Legacy, Venue::HartiiAmm] {
        let pool = all
            .iter()
            .filter(|p| p.venue == venue && (p.token0.address == wquai || p.token1.address == wquai))
            .max_by(|a, b| {
                let side = |p: &wallet_core::markets::Pool| if p.token0.address == wquai { p.reserve0 } else { p.reserve1 };
                side(a).total_cmp(&side(b))
            })
            .unwrap_or_else(|| panic!("{venue:?}: no WQUAI pool"));
        let other = if pool.token0.address == wquai { &pool.token1 } else { &pool.token0 };
        let path = vec![wquai.clone(), other.address.clone()];
        let amount = U256::from(10u128.pow(18)); // one QUAI
        let proven = wallet_core::swap::prove_route(&ctx.node, &ctx.network, venue, &path, std::slice::from_ref(&pool.address), amount)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("{venue:?}: not provable"));
        let (router, _) = wallet_core::swap::venue_pins(&ctx.network, venue).unwrap();
        let block = proven.block;
        let contract = wallet_core::sdk::contracts::Contract::new(
            router.address.parse().unwrap(),
            wallet_core::sdk::abi::AbiInterface::from_human_readable(wallet_core::swap::ROUTER_ABI).unwrap(),
            &ctx.node.provider,
        );
        let quoted = contract
            .call(
                wallet_core::data::READ_CALLER.parse().unwrap(),
                "getAmountsOut",
                &[serde_json::json!(amount.to_string()), serde_json::json!(path)],
                BlockTag::Number(U256::from(block)),
            )
            .await
            .unwrap();
        let last = quoted[0].as_array().and_then(|a| a.last()).and_then(|v| v.as_str()).unwrap().to_string();
        eprintln!("{venue:?} {}: proven {} router {last}", other.symbol, proven.amount_out);
        assert_eq!(proven.amount_out.to_string(), last, "{venue:?}: the proven pools and the router disagree");

        // Exact output: what the proven pools need for the router's own output, against its
        // `getAmountsIn`, both at the anchor's block.
        let (reserves, anchored) = wallet_core::swap::prove_path_reserves(&ctx.node, &ctx.network, venue, &path).await.unwrap().unwrap();
        let out = U256::from_str_radix(&last, 10).unwrap() / U256::from(2u64);
        let needed = wallet_core::swap::input_for_output(out, &reserves).unwrap();
        let block = anchored.anchor.block.number;
        let quoted = contract
            .call(
                wallet_core::data::READ_CALLER.parse().unwrap(),
                "getAmountsIn",
                &[serde_json::json!(out.to_string()), serde_json::json!(path)],
                BlockTag::Number(U256::from(block)),
            )
            .await
            .unwrap();
        let first = quoted[0].as_array().and_then(|a| a.first()).and_then(|v| v.as_str()).unwrap().to_string();
        assert_eq!(needed.to_string(), first, "{venue:?}: exact-output input from the proven pools and the router disagree");

        // LP: a review's quote reads the pool at the anchor and proves supply and reserves there.
        let review = mainnet().for_review();
        wallet_core::liquidity::quote(&review, &pool.address, "1", None, 50, None)
            .await
            .unwrap_or_else(|e| panic!("{venue:?}: a first-hand LP quote failed: {e}"));
        // And the LP balance slot, read for address zero: every UniswapV2 pair mints its minimum
        // liquidity there (and the launch AMM locks graduated liquidity there too), so it is never
        // empty. The proven word must be the pair's own `balanceOf` at the same block.
        let zero: wallet_core::sdk::QuaiAddress = "0x0000000000000000000000000000000000000000".parse().unwrap();
        let pair: wallet_core::sdk::QuaiAddress = pool.address.parse().unwrap();
        let slot = wallet_core::anchor::mapping_field_slot(zero, 1, 0);
        let (proven, _) = wallet_core::anchor::prove_state(&ctx.node, &ctx.network, &[(pair, &[slot])], "pair").await.unwrap().unwrap();
        let at = BlockTag::Number(U256::from(proven[0].block.number));
        let called = wallet_core::sdk::contracts::Erc20::new(pair, &ctx.node.provider)
            .unwrap()
            .balance_of(wallet_core::data::READ_CALLER.parse().unwrap(), zero, at)
            .await
            .unwrap();
        assert!(!called.is_zero(), "{venue:?}: address zero holds no LP");
        assert_eq!(proven[0].storage_value(slot), Some(called), "{venue:?}: balanceOf is not the mapping at slot 1");
    }
}

/// Reads asked for at a block describe that block: the tape ends there, and every pool's reserves
/// are what the pair's own `getReserves` answers there. This is what lets the header, the prices
/// and the tape name one block.
#[tokio::test]
#[ignore = "network"]
async fn reads_at_the_announced_block_describe_that_block() {
    use wallet_core::sdk::{BlockTag, U256};
    let ctx = mainnet();
    let head = ctx.node.provider.latest_header(wallet_core::network::ZONE).await.unwrap().unwrap().number;
    let at = head - 2;
    let pools: Vec<_> = wallet_core::markets::pools(&ctx).await.unwrap().0.into_iter().take(8).collect();
    let tape = wallet_core::markets::dex_flow_at(&ctx, &pools, 300, Some(at)).await.unwrap();
    let pool_rows: Vec<_> = tape.iter().filter(|s| pools.iter().any(|p| p.address == s.pool)).collect();
    assert!(pool_rows.iter().all(|s| s.block <= at), "a pool swap after the asked block: {:?}", pool_rows.iter().map(|s| s.block).max());
    let reserves = wallet_core::markets::refresh_reserves_at(&ctx, &pools, Some(at)).await.unwrap();
    assert!(!reserves.is_empty());
    for (address, r0, r1) in reserves.iter().take(4) {
        let pool = pools.iter().find(|p| &p.address == address).unwrap();
        let contract = wallet_core::sdk::contracts::Contract::new(
            address.parse().unwrap(),
            wallet_core::sdk::abi::AbiInterface::from_human_readable(&[
                "function getReserves() view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast)",
            ])
            .unwrap(),
            &ctx.node.provider,
        );
        let answer = contract
            .call(wallet_core::data::READ_CALLER.parse().unwrap(), "getReserves", &[], BlockTag::Number(U256::from(at)))
            .await
            .unwrap();
        let units = |v: &serde_json::Value, d: u8| v.as_str().unwrap().parse::<f64>().unwrap() / 10f64.powi(i32::from(d));
        assert!((units(&answer[0], pool.token0.decimals) - r0).abs() <= r0.abs() * 1e-12, "{address}: reserve0 at {at}");
        assert!((units(&answer[1], pool.token1.decimals) - r1).abs() <= r1.abs() * 1e-12, "{address}: reserve1 at {at}");
    }
}

/// A quote that is going into a review reads the pins and every pair address from the chain
/// (`docs/REVIEW_TRUST.md`), so it must agree with the cached one and must still work when the
/// memo and the pair cache are seeded with nothing. The timing is printed because the cost of
/// that guarantee is the one number the decision to keep it turns on.
#[tokio::test]
#[ignore = "network"]
async fn a_first_hand_quote_agrees_with_a_cached_one() {
    use wallet_core::sdk::U256;
    use wallet_core::swap::{Router, SwapAsset};
    let ctx = mainnet();
    let usdt = SwapAsset::Token { address: "0x0049f7cbca3556c2dfae62aafa7015f99de1b8f5".into(), symbol: "USDT".into(), decimals: 6 };
    let amount = U256::from(100u128) * U256::from(10u128.pow(18));

    // Warm every memo and pair row, the way an open wallet would have.
    let warm = Router::open(&ctx.app, &ctx.node, &ctx.network, Trust::Cached).await.unwrap();
    warm.quote(&SwapAsset::Quai, &usdt, amount, 50, None).await.unwrap();

    let started = std::time::Instant::now();
    let cached = Router::open(&ctx.app, &ctx.node, &ctx.network, Trust::Cached).await.unwrap();
    let shown = cached.quote(&SwapAsset::Quai, &usdt, amount, 50, None).await.unwrap();
    let cached_ms = started.elapsed().as_millis();

    let started = std::time::Instant::now();
    let review = Router::open(&ctx.app, &ctx.node, &ctx.network, Trust::FirstHand).await.unwrap();
    let asserted = review.quote(&SwapAsset::Quai, &usdt, amount, 50, None).await.unwrap();
    let first_hand_ms = started.elapsed().as_millis();

    assert_eq!(shown.route, asserted.route, "the same route");
    assert_eq!(shown.pools, asserted.pools, "the same pools, read again");
    assert_eq!(shown.router, asserted.router, "the same router, verified again");
    println!("QUAI→USDT quote: cached {cached_ms} ms, first-hand {first_hand_ms} ms");

    // With nothing remembered at all, the first-hand quote is unchanged: it never used the cache.
    let cold = mainnet();
    let bare = Router::open(&cold.app, &cold.node, &cold.network, Trust::FirstHand).await.unwrap();
    let from_cold = bare.quote(&SwapAsset::Quai, &usdt, amount, 50, None).await.unwrap();
    assert_eq!(from_cold.route, asserted.route);
    assert_eq!(from_cold.pools, asserted.pools);
}

#[tokio::test]
#[ignore = "network"]
async fn nft_owner_and_token_reads() {
    let ctx = mainnet();
    let owner =
        ctx.erc721_owner("0x004d92fd198c21af21016f4b119b8b851b5aeaa4", "1", "0x00201a76447452e62c008b6b5d70d6ed5837b946").await.unwrap();
    assert!(owner.starts_with("0x00"), "{owner}");
    let (symbol, _, decimals) =
        ctx.erc20_metadata("0x002b2596ecf05c93a31ff916e8b456df6c77c750", wallet_core::data::READ_CALLER).await.unwrap();
    assert_eq!((symbol.as_str(), decimals), ("WQI", 18));
    let bal = ctx.erc20_balance("0x002b2596ecf05c93a31ff916e8b456df6c77c750", "0x001f91029df78af6d13cbffa8724f1b2718da3f1").await.unwrap();
    assert!(!bal.is_zero());
}

#[tokio::test]
#[ignore = "network"]
async fn quainance_quotes_match_reserves_math() {
    use wallet_core::sdk::U256;
    use wallet_core::swap::{Router, SwapAsset, amount_out};
    let ctx = mainnet();
    let router = Router::open(&ctx.app, &ctx.node, &ctx.network, Trust::Cached).await.unwrap();
    let wqi = SwapAsset::Token { address: "0x002b2596ecf05c93a31ff916e8b456df6c77c750".into(), symbol: "WQI".into(), decimals: 18 };
    let usdt = SwapAsset::Token { address: "0x0049f7cbca3556c2dfae62aafa7015f99de1b8f5".into(), symbol: "USDT".into(), decimals: 6 };
    let one = U256::from(10u128.pow(18));
    for (from, to) in [(&wqi, &usdt), (&SwapAsset::Quai, &usdt), (&usdt, &SwapAsset::Quai)] {
        let amount = if from.decimals() == 6 {
            U256::from(1_000_000u64)
        } else if matches!(from, SwapAsset::Quai) {
            one * U256::from(100u64)
        } else {
            one
        };
        let mut q = router.quote(from, to, amount, 50, Some("0x00201a76447452e62c008b6b5d70d6ed5837b946")).await.unwrap();
        // Every pool the router uses is indexed by the explorer's TVL stats.
        wallet_core::swap::attach_liquidity(&ctx, &mut q).await;
        assert!(q.pools.iter().all(|p| p.tvl_usd.is_some_and(|v| v > 0.0)), "unindexed pool in {:?}", q.pools);
        eprintln!("liquidity {}", q.liquidity_text().unwrap());
        eprintln!(
            "{} → {}: route {:?} out {} min {} impact {}bps approval {}",
            from.symbol(),
            to.symbol(),
            q.route,
            q.receive_text(),
            q.minimum_text(),
            q.impact_bps,
            q.approval_needed
        );
        // Recompute the router's answer from the reserves it reported.
        let mut expect = amount;
        for hop in &q.pools {
            expect =
                amount_out(expect, U256::from_str_radix(&hop.reserve_in, 10).unwrap(), U256::from_str_radix(&hop.reserve_out, 10).unwrap());
        }
        assert_eq!(expect.to_string(), q.amount_out, "router and reserves disagree");
        assert!(U256::from_str_radix(&q.minimum_out, 10).unwrap() < U256::from_str_radix(&q.amount_out, 10).unwrap());
    }
    assert!(
        router
            .quote(
                &SwapAsset::Quai,
                &SwapAsset::Token { address: router.wquai().into(), symbol: "WQUAI".into(), decimals: 18 },
                one,
                50,
                None
            )
            .await
            .is_err()
    );
}

#[tokio::test]
#[ignore = "network"]
async fn native_swap_calldata_simulates_on_mainnet() {
    use wallet_core::sdk::{BlockTag, QuaiAddress, U256};
    use wallet_core::swap::{ROUTER_ABI, Router, SwapAsset};
    let ctx = mainnet();
    let router = Router::open(&ctx.app, &ctx.node, &ctx.network, Trust::Cached).await.unwrap();
    // Discovered, not written down; nothing is signed or sent. See `funded_account`.
    let from = funded_account(&ctx).await;
    let holder = from.to_string();
    let holder = holder.as_str();
    let usdt = SwapAsset::Token { address: "0x0049f7cbca3556c2dfae62aafa7015f99de1b8f5".into(), symbol: "USDT".into(), decimals: 6 };
    let amount = U256::from(5u128 * 10u128.pow(17));
    let q = router.quote(&SwapAsset::Quai, &usdt, amount, 100, None).await.unwrap();
    let abi = wallet_core::sdk::abi::AbiInterface::from_human_readable(ROUTER_ABI).unwrap();
    let contract = wallet_core::sdk::contracts::Contract::new(router.address(), abi, &ctx.node.provider);
    let path: Vec<serde_json::Value> = q.path.iter().map(|p| serde_json::json!(p)).collect();
    let deadline = (wallet_core::registry::now() + 600).to_string();
    let call = contract
        .prepare(
            "swapExactETHForTokens",
            &[serde_json::json!(q.minimum_out), serde_json::Value::Array(path), serde_json::json!(holder), serde_json::json!(deadline)],
            amount,
        )
        .unwrap();
    let from: QuaiAddress = holder.parse().unwrap();
    let out = contract.simulate(from, &call, BlockTag::Latest, Some(600_000)).await.unwrap();
    let amounts = out[0].as_array().unwrap();
    let received = U256::from_str_radix(amounts.last().unwrap().as_str().unwrap(), 10).unwrap();
    eprintln!("simulated: 0.5 QUAI → {} USDT atoms (quoted {})", received, q.amount_out);
    assert!(received >= U256::from_str_radix(&q.minimum_out, 10).unwrap());
    // An impossible minimum reverts in simulation (the review would refuse it).
    let greedy = contract
        .prepare(
            "swapExactETHForTokens",
            &[
                serde_json::json!((U256::from_str_radix(&q.amount_out, 10).unwrap() * U256::from(2u64)).to_string()),
                serde_json::Value::Array(q.path.iter().map(|p| serde_json::json!(p)).collect()),
                serde_json::json!(holder),
                serde_json::json!((wallet_core::registry::now() + 600).to_string()),
            ],
            amount,
        )
        .unwrap();
    assert!(contract.simulate(from, &greedy, BlockTag::Latest, Some(600_000)).await.is_err());
}

#[tokio::test]
#[ignore = "network"]
async fn bazarr_listing_rechecks_and_fill_simulates() {
    use wallet_core::market::{ASKS_ABI, ZERO_ADDRESS, Zora, listings};
    use wallet_core::sdk::{BlockTag, QuaiAddress, U256};
    let ctx = mainnet();
    let zora = Zora::open(&ctx.app, &ctx.node, &ctx.network).await.unwrap();
    let all = listings(&ctx, None).await.unwrap();
    assert!(all.len() > 50, "indexer returned {}", all.len());
    // A buyer seen trading on mainnet, discovered rather than written down. Nothing is sent.
    let discovered = funded_account(&ctx).await.to_string();
    let buyers = [discovered.as_str()];
    let mut valid = 0;
    let mut simulated = false;
    for l in all.iter().filter(|l| l.buyable() && l.is_native()).take(12) {
        let check = zora.check(&ctx.node, &l.contract, &l.token_id, None).await.unwrap();
        eprintln!("{} #{} {} → valid={} problems={:?}", l.contract, l.token_id, l.price_text(), check.valid, check.problems);
        if !check.valid {
            continue;
        }
        valid += 1;
        let ask = check.ask.unwrap();
        assert_eq!(ask.price, l.price, "indexer price differs from chain");
        if simulated {
            continue;
        }
        for buyer in buyers {
            let from: QuaiAddress = buyer.parse().unwrap();
            let balance = ctx.node.provider.balance(from, BlockTag::Latest).await.unwrap();
            let price = U256::from_str_radix(&ask.price, 10).unwrap();
            if balance <= price + U256::from(10u128.pow(18)) || buyer.eq_ignore_ascii_case(&ask.seller) {
                continue;
            }
            let abi = wallet_core::sdk::abi::AbiInterface::from_human_readable(ASKS_ABI).unwrap();
            let asks = wallet_core::sdk::contracts::Contract::new(zora.asks, abi, &ctx.node.provider);
            let call = asks
                .prepare(
                    "fillAsk",
                    &[
                        serde_json::json!(l.contract),
                        serde_json::json!(l.token_id),
                        serde_json::json!(ZERO_ADDRESS),
                        serde_json::json!(ask.price),
                        serde_json::json!(ZERO_ADDRESS),
                    ],
                    price,
                )
                .unwrap();
            asks.simulate(from, &call, BlockTag::Latest, Some(800_000)).await.unwrap();
            // Underpaying reverts.
            let cheap = asks
                .prepare(
                    "fillAsk",
                    &[
                        serde_json::json!(l.contract),
                        serde_json::json!(l.token_id),
                        serde_json::json!(ZERO_ADDRESS),
                        serde_json::json!((price - U256::from(1u64)).to_string()),
                        serde_json::json!(ZERO_ADDRESS),
                    ],
                    price - U256::from(1u64),
                )
                .unwrap();
            assert!(asks.simulate(from, &cheap, BlockTag::Latest, Some(800_000)).await.is_err());
            eprintln!("simulated fillAsk for {} #{} from {buyer}", l.contract, l.token_id);
            simulated = true;
            break;
        }
    }
    assert!(valid > 0, "no valid asks among the cheapest listings");
    eprintln!("valid asks: {valid}, simulated: {simulated}");
}

#[tokio::test]
#[ignore = "network"]
async fn portfolio_and_images_from_the_explorer() {
    use wallet_core::portfolio::{Known, Trust, build};
    use wallet_core::sdk::{BlockTag, QuaiAddress};
    let ctx = mainnet();
    let address: QuaiAddress = funded_account(&ctx).await;
    let owner = address.to_string();
    let owner = owner.as_str();
    let quai = ctx.node.provider.balance(address, BlockTag::Latest).await.unwrap();
    let known = Known { owners: vec![owner.into()], quai, qi: None, tokens: vec![] };
    let p = build(&ctx, &known).await.unwrap();
    for r in &p.rows {
        eprintln!(
            "{:8} {:>28} exact={} price={:?} value={:?} trust={:?} icon={:?}",
            r.symbol, r.balance, r.exact, r.price_usd, r.value_usd, r.trust, r.icon_url
        );
    }
    eprintln!(
        "total ${:.2} unpriced {} nfts {:?} history {} pts change {:?} notices {:?}",
        p.total_usd,
        p.unpriced,
        p.nfts,
        p.history.len(),
        p.change_7d,
        p.notices
    );
    assert!(p.prices.as_ref().and_then(|b| b.quai_usd).is_some());
    // What any holder's portfolio must satisfy, rather than what one particular wallet happens to
    // contain: this account is discovered on each run, so naming its tokens would only assert that
    // the chain had not moved. `exact` is the claim that matters — a balance the explorer reported
    // was re-read on chain rather than trusted.
    assert!(!p.rows.is_empty(), "a trader holds something");
    assert!(p.rows.iter().any(|r| r.exact), "at least one balance is re-read on-chain");
    assert!(p.rows.iter().all(|r| !r.symbol.is_empty()), "every row names its token");
    assert_eq!(p.history.len(), 28);
    assert!(p.rows.iter().all(|r| r.trust != Trust::Unknown || r.key == wallet_core::portfolio::AssetKey::Quai));
    // Token icon (SVG) and NFT media (PNG) through the pipeline.
    let icon = wallet_core::media::load(&ctx.app, "https://explorer.qu.ai/token-icons/wrapped-qi.svg", wallet_core::media::ICON)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(icon.width, 32);
    let nft = wallet_core::media::load(
        &ctx.app,
        "https://explorer.qu.ai/api/nft-media/61c2ac6a34096a8767c28b2d3be69fb585eff089c9e188e107c07d90b5512bd7",
        wallet_core::media::THUMB,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!((nft.width, nft.height), (256, 256));
    // NFT holdings verified on-chain.
    let held = wallet_core::market::holdings(&ctx, &[owner.to_string()], 30, false).await.unwrap();
    eprintln!("verified NFTs: {}", held.iter().map(|n| format!("{} #{}", n.item.name, n.item.token_id)).collect::<Vec<_>>().join(", "));
    assert!(held.iter().all(|n| n.verified));
}

#[tokio::test]
#[ignore = "network"]
async fn swap_output_decodes_real_receipts() {
    let ctx = mainnet();
    // swapExactTokensForETH by 0x004a1e… (USDT → QUAI), 2026-09-15.
    let hash = "0x00550009b278724f0d95fd7c137b4a21391970692ac88659111b6e9619b13605".parse().unwrap();
    let receipt = ctx.node.provider.receipt(wallet_core::network::ZONE, hash).await.unwrap().unwrap();
    let detail = serde_json::json!({"recipient": "0x004a1ea50754d904883db3ca9138cd4bc321734b", "to_token": "quai"});
    let out = wallet_core::track::swap_output(&receipt, &detail, ctx.network.wquai.as_deref()).expect("withdrawal found");
    eprintln!("native out: {} QUAI", wallet_core::amount::quai(out));
    assert!(!out.is_zero());
    // Asking for a token the swap did not pay out finds nothing.
    let wrong = serde_json::json!({"recipient": "0x004a1ea50754d904883db3ca9138cd4bc321734b", "to_token": "0x002b2596ecf05c93a31ff916e8b456df6c77c750"});
    assert!(wallet_core::track::swap_output(&receipt, &wrong, ctx.network.wquai.as_deref()).is_none());
}

#[tokio::test]
#[ignore = "network"]
async fn discovered_access_lists_cover_router_and_marketplace_calls() {
    use wallet_core::sdk::{QuaiAddress, U256};
    use wallet_core::swap::{ROUTER_ABI, Router, SwapAsset};
    let ctx = mainnet();
    let router = Router::open(&ctx.app, &ctx.node, &ctx.network, Trust::Cached).await.unwrap();
    let holder: QuaiAddress = funded_account(&ctx).await;
    let usdt = SwapAsset::Token { address: "0x0049f7cbca3556c2dfae62aafa7015f99de1b8f5".into(), symbol: "USDT".into(), decimals: 6 };
    let amount = U256::from(5u128 * 10u128.pow(17));
    let q = router.quote(&SwapAsset::Quai, &usdt, amount, 100, None).await.unwrap();
    let abi = wallet_core::sdk::abi::AbiInterface::from_human_readable(ROUTER_ABI).unwrap();
    let contract = wallet_core::sdk::contracts::Contract::new(router.address(), abi, &ctx.node.provider);
    let path: Vec<serde_json::Value> = q.path.iter().map(|p| serde_json::json!(p)).collect();
    let call = contract
        .prepare(
            "swapExactETHForTokens",
            &[
                serde_json::json!(q.minimum_out),
                serde_json::Value::Array(path),
                serde_json::json!(holder.to_string()),
                serde_json::json!((wallet_core::registry::now() + 600).to_string()),
            ],
            amount,
        )
        .unwrap();
    let call = wallet_core::data::with_access_list(&ctx.node.provider, holder, call).await.unwrap();
    let listed: Vec<String> = call.access_list().iter().map(|a| a.address.to_string().to_lowercase()).collect();
    eprintln!("swap access list: {listed:?}");
    for pool in &q.pools {
        assert!(listed.contains(&pool.pair), "pool {} missing", pool.pair);
    }
    assert!(listed.contains(&"0x0049f7cbca3556c2dfae62aafa7015f99de1b8f5".to_string()), "output token missing");
}

#[tokio::test]
#[ignore = "network"]
async fn wqi_redemption_and_claim_simulate() {
    use wallet_core::sdk::wrappers::WrappedQi;
    use wallet_core::sdk::{BlockTag, U256};
    let ctx = mainnet();
    let wqi = WrappedQi::new(ctx.network.wqi.as_ref().unwrap().parse().unwrap(), &ctx.node.provider).unwrap();
    // Top WQI holders (explorer.qu.ai, 2026-09-15); simulate from the first plain account that
    // still holds at least 1 WQI (contract senders are rejected by quai_call).
    let reader: wallet_core::sdk::QuaiAddress = wallet_core::data::READ_CALLER.parse().unwrap();
    let mut found = None;
    for h in [
        "0x001f91029df78af6d13cbffa8724f1b2718da3f1",
        "0x000ed989feefb2b8f40bf3ee60131ccbd8f9a5d0",
        "0x002509f7eee56cb065010bd3cfd80176a75e0f68",
    ] {
        let holder: wallet_core::sdk::QuaiAddress = h.parse().unwrap();
        let atoms = wqi.token().unwrap().balance_of(reader, holder, BlockTag::Latest).await.unwrap();
        let code = ctx.node.provider.code(holder, BlockTag::Latest).await.unwrap();
        if code.bytes().is_empty() && atoms >= U256::from(10u128.pow(18)) {
            found = Some((holder, atoms));
            break;
        }
    }
    let (holder, atoms) = found.expect("no plain-account WQI holder with 1 WQI");
    // Unclaimed backing reads (absent deposits map to zero).
    let unclaimed = wqi.unclaimed(holder, BlockTag::Latest).await.unwrap();
    eprintln!("holder unclaimed backing: {unclaimed} qits");
    // Redeeming 1 Qi exactly as the wallet builds it succeeds in simulation.
    let beneficiary: wallet_core::sdk::QiAddress = "0x0080000000000000000000000000000000000001".parse().unwrap();
    let call = wqi.unwrap(beneficiary, U256::from(1000u64), 30_000).unwrap();
    let mut request = wallet_core::sdk::provider::CallRequest::new(holder, call.destination());
    request.input = call.data().clone();
    request.access_list = call
        .access_list()
        .iter()
        .map(|t| wallet_core::sdk::provider::AccessListItem { address: t.address, storage_keys: t.storage_keys.clone() })
        .collect();
    ctx.node.provider.call(&request, BlockTag::Latest).await.unwrap();
    let gas = ctx.node.provider.estimate_gas(&request, BlockTag::Latest).await.unwrap();
    eprintln!("unwrapQi(1 Qi) gas estimate {gas}");
    assert!(gas < 1_100_000, "wallet caps unwrap gas at 1.1M");
    let _ = atoms;
}

#[tokio::test]
#[ignore = "network"]
async fn market_pools_events_and_candles() {
    use wallet_core::markets::{base_is_token0, candles, pair_stats, pool_events, pools, trades};
    let ctx = mainnet();
    let (pools, overview) = pools(&ctx).await.unwrap();
    assert!(pools.len() >= 5 && overview.tvl_usd.unwrap() > 0.0 && !overview.history.is_empty());
    let usdt = "0x0049f7cbca3556c2dfae62aafa7015f99de1b8f5";
    let wquai = "0x006c3e2aaae5db1bcd11a1a097ce572312eaddbb";
    let pool = pools
        .iter()
        .find(|p| {
            [&p.token0.address, &p.token1.address].iter().any(|a| *a == usdt)
                && [&p.token0.address, &p.token1.address].iter().any(|a| *a == wquai)
        })
        .unwrap();
    assert_eq!(if pool.token0.address == usdt { pool.token0.decimals } else { pool.token1.decimals }, 6, "decimals read on-chain");
    let now = wallet_core::registry::now();
    let events = pool_events(&ctx, pool, now - 7 * 86_400, 6).await.unwrap();
    assert!(!events.is_empty(), "a week of USDT/WQUAI events");
    let base0 = base_is_token0(pool, Some(usdt), Some(wquai), None);
    let c = candles(&events, pool, base0, 3600, now, 48);
    assert!(!c.is_empty() && c.iter().all(|c| c.low <= c.open.min(c.close) && c.high >= c.open.max(c.close)));
    let stats = pair_stats(&events, pool, base0, now);
    let reserve_price = if base0 { pool.reserve1 / pool.reserve0 } else { pool.reserve0 / pool.reserve1 };
    let last = c.last().unwrap().close;
    eprintln!(
        "WQUAI/USDT last {last} · reserves {reserve_price} · 24h {:?} · trades {}",
        stats.change_24h,
        trades(&events, pool, base0).len()
    );
    assert!((last / reserve_price - 1.0).abs() < 0.05, "chart price follows the pool reserves");
    // A second call is incremental (served mostly from the cached history) and returns the same window.
    let again = pool_events(&ctx, pool, now - 7 * 86_400, 6).await.unwrap();
    assert!(again.len() >= events.len());
}

/// Market data without the explorer, from a monitoring node: `QW_MONITOR_RPC=http://host:9200`.
#[tokio::test]
#[ignore = "network"]
async fn market_data_from_a_monitoring_node() {
    use wallet_core::markets::{base_is_token0, candles, pool_events, pools};
    let Ok(url) = std::env::var("QW_MONITOR_RPC") else {
        eprintln!("QW_MONITOR_RPC not set; skipping");
        return;
    };
    let mut network = NetworkProfile::builtins().into_iter().find(|n| n.id == "mainnet").unwrap();
    network.monitor = Some(wallet_core::network::MonitorEndpoint { rpc_url: url, use_pathing: false });
    // Market data off: pools come from the factory and events from quai_getLogs on the node.
    let policy = DataPolicy { explorer: false, market: false, images: false, icons: false };
    let mut ctx = DataCtx::with_app(AppDb::memory().unwrap(), network, policy).unwrap();
    assert!(ctx.use_monitor().await.is_none(), "monitoring node identity verified");
    let (pools, overview) = pools(&ctx).await.unwrap();
    assert_eq!(overview.source, "chain");
    assert!(pools.len() >= 5, "{} pools from the factory", pools.len());
    let usdt = "0x0049f7cbca3556c2dfae62aafa7015f99de1b8f5";
    let wquai = "0x006c3e2aaae5db1bcd11a1a097ce572312eaddbb";
    let pool = pools.iter().find(|p| p.token0.address == usdt || p.token1.address == usdt).expect("a USDT pool");
    let now = wallet_core::registry::now();
    let events = pool_events(&ctx, pool, now - 86_400, 1).await.unwrap();
    let base0 = base_is_token0(pool, Some(usdt), Some(wquai), None);
    let c = candles(&events, pool, base0, 3600, now, 12);
    eprintln!("{} pools · {} events in 24h · {} candles", pools.len(), events.len(), c.len());
    // A wrong endpoint is refused and reads stay on the main RPC.
    let mut bad = ctx.network.clone();
    bad.monitor =
        Some(wallet_core::network::MonitorEndpoint { rpc_url: "https://orchard.rpc.quai.network/cyprus1".into(), use_pathing: false });
    let mut other = DataCtx::with_app(AppDb::memory().unwrap(), bad, policy).unwrap();
    assert!(other.use_monitor().await.is_some());
}

/// Both QUAI ⇄ Qi markets quote on mainnet: the protocol conversion and the Quainance route.
#[tokio::test]
#[ignore = "network"]
async fn both_qi_markets_quote() {
    use wallet_core::qi_market::{Direction, compare};
    let ctx = mainnet();
    let quai = |n: u64| wallet_core::sdk::U256::from(n) * wallet_core::sdk::U256::from(10u64).pow(wallet_core::sdk::U256::from(18));
    let c = compare(&ctx, Direction::QuaiToQi, quai(2_000), None, 50).await.unwrap();
    assert_eq!(c.amount_display, "2000 QUAI");
    assert!(c.protocol.usable(), "the controller always quotes: {:?}", c.protocol);
    assert_eq!(c.protocol.legs.len(), 1);
    assert!(c.protocol.wait.contains("locked"));
    assert_eq!(c.market.legs.len(), 3, "wrap, swap, unwrap");
    assert!(c.better().is_some(), "one of them pays more");
    // Too little to redeem a whole Qi: the route says so instead of looking empty.
    let small = compare(&ctx, Direction::QuaiToQi, quai(10), None, 50).await.unwrap();
    assert!(small.market.unavailable.as_ref().is_some_and(|w| w.contains("whole Qi")), "{:?}", small.market.unavailable);
    // Qi in: the route wraps, claims, swaps and unwraps.
    let qi = wallet_core::sdk::U256::from(8_000u64);
    let back = compare(&ctx, Direction::QiToQuai, qi, None, 50).await.unwrap();
    assert_eq!(back.amount_display, "8 Qi");
    assert_eq!(back.market.legs.len(), 4);
    for route in [&back.protocol, &back.market] {
        if let Some(text) = &route.receives_display {
            assert!(text.ends_with(" QUAI"), "{text}");
        }
    }
}

/// The DEX-wide tape reads every pool in one `quai_getLogs`: recent swaps, both sides priced
/// from the pool's own tokens, and a second call that adds only what the chain has mined since.
#[tokio::test]
#[ignore = "network"]
async fn dex_flow_reads_every_pool_at_once() {
    let ctx = mainnet();
    let (pools, _) = wallet_core::markets::pools(&ctx).await.unwrap();
    assert!(pools.len() > 3, "{} pools", pools.len());
    let flow = wallet_core::markets::dex_flow(&ctx, &pools, 600).await.unwrap();
    assert!(!flow.is_empty(), "Cyprus-1 trades: the last ~50 minutes had no swaps at all?");
    // The tape is pool logs plus bonding-curve trades, which carry no pool log and are named by
    // their source contract rather than a pair. `pools` lists pairs only, so a curve row is not an
    // unknown pool — but anything that is neither is.
    let curve_sources: std::collections::HashSet<String> =
        wallet_core::launches::curve_trades(&ctx, 500).await.map(|trades| trades.into_iter().map(|t| t.pool).collect()).unwrap_or_default();
    let now = wallet_core::registry::now();
    let mut pools_seen = std::collections::HashSet::new();
    for s in &flow {
        pools_seen.insert(s.pool.clone());
        assert!(s.amount_in > 0.0 && s.amount_out > 0.0, "{s:?}");
        assert_ne!(s.token_in.address, s.token_out.address, "{s:?}");
        assert!(
            pools.iter().any(|p| p.address == s.pool) || curve_sources.contains(&s.pool),
            "a row that is neither a known pair nor a known curve got in: {s:?}"
        );
        assert!(s.at <= now + 60 && s.at + 7 * 86_400 > now, "dated within the window: {} vs {now}", s.at);
        let base = &s.token_in.address;
        assert!(s.price(base).is_some_and(|p| p.is_finite() && p > 0.0), "{s:?}");
    }
    // Newest transaction first, each route's hops in the order the trader took them.
    assert!(flow.windows(2).all(|w| w[0].tx == w[1].tx || w[0].position() >= w[1].position()), "transactions newest first");
    assert!(flow.windows(2).all(|w| w[0].tx != w[1].tx || w[0].index < w[1].index), "a route reads first hop down");
    assert!(flow.iter().take(5).all(|s| s.timed), "the newest rows are timed from their headers");
    // A refresh keeps the tape and only reads the blocks since: nothing is lost or duplicated.
    let again = wallet_core::markets::dex_flow(&ctx, &pools, 600).await.unwrap();
    assert!(again.len() >= flow.len(), "{} then {}", flow.len(), again.len());
    let keys: std::collections::HashSet<(String, u64)> = again.iter().map(|s| (s.tx.clone(), s.index)).collect();
    assert_eq!(keys.len(), again.len(), "no duplicate rows after a merge");
    assert!(flow.iter().all(|s| keys.contains(&(s.tx.clone(), s.index))), "the tape keeps what it had");
    println!("{} swaps across {} pools; newest: {:?}", again.len(), pools_seen.len(), again.first());
}

/// The gauge reads as the plan describes it: three pools, each bound to a real pair, each paying
/// a reward stream whose rate reproduces a sane emission.
#[tokio::test]
#[ignore = "network"]
async fn gauge_pools_and_reward_rates() {
    let ctx = mainnet();
    let view = wallet_core::gauge::open(&ctx, &[]).await.unwrap();
    assert!(!view.pools.is_empty(), "the gauge has pools");
    for pool in &view.pools {
        assert!(pool.lp_token.starts_with("0x00"), "pid {} lp {}", pool.pid, pool.lp_token);
        assert_eq!(view.pid_for(&pool.lp_token), Some(pool.pid));
        for reward in &pool.rewards {
            // A live stream must emit something believable — not 1e-12 tokens a day, and not
            // hundreds of thousands. This is the assertion that would catch a scale regression.
            if reward.live(wallet_core::registry::now()) {
                let per_day = reward.per_day();
                assert!(per_day > 0.01 && per_day < 100_000.0, "pid {} pays {per_day}/day", pool.pid);
            }
        }
    }
    // Reading with no owners still works and reports nothing staked by us.
    assert!(view.pools.iter().all(|p| p.staked.is_zero()));

    // F12: the reward-rate unit, checked against something only the chain can answer. The main
    // gauge stores `amount * 1e18 / duration`, so what a live stream still owes is
    // `rate * seconds_left / 1e18` atoms — and a gauge cannot owe more of a token than it holds.
    // A missing or doubled 1e18 moves that by eighteen orders of magnitude, so the two bounds
    // below pin the scale from both sides without keys, funds or a fixture gauge.
    use wallet_core::sdk::{BlockTag, QuaiAddress, U256, contracts::Erc20};
    let now = wallet_core::registry::now();
    let gauge: QuaiAddress = view.address.parse().unwrap();
    let reader: QuaiAddress = "0x0000000000000000000000000000000000000001".parse().unwrap();
    let scale = U256::from(10u128.pow(18));
    let mut owed: std::collections::HashMap<String, (U256, u8)> = std::collections::HashMap::new();
    for pool in &view.pools {
        for reward in pool.rewards.iter().filter(|r| r.live(now)) {
            let seconds = U256::from(reward.period_finish.saturating_sub(now));
            let entry = owed.entry(reward.token.address.to_lowercase()).or_insert((U256::ZERO, reward.token.decimals));
            entry.0 += reward.rate * seconds / scale;
        }
    }
    assert!(!owed.is_empty(), "no live reward stream to qualify the rate unit against");
    for (token, (atoms, decimals)) in &owed {
        let erc = Erc20::new(token.parse().unwrap(), &ctx.node.provider).unwrap();
        let held = erc.balance_of(reader, gauge, BlockTag::Latest).await.unwrap();
        let whole = |v: U256| wallet_core::amount::to_f64(v, *decimals);
        eprintln!("GAUGE_RATE {token} owes {} holds {} ({decimals} decimals)", whole(*atoms), whole(held));
        assert!(*atoms <= held, "the gauge owes {} of {token} but holds {}", whole(*atoms), whole(held));
        assert!(
            atoms.saturating_mul(U256::from(1_000_000_000u64)) >= held,
            "a funded campaign owing {} against {} of {token} means the rate scale is far too small",
            whole(*atoms),
            whole(held)
        );
    }
}

/// Two-hub routing is not theoretical: a WQI-side token must be able to reach a WQUAI-side one.
#[tokio::test]
#[ignore = "network"]
async fn trading_readonly_directory_routes_are_capability_bounded() {
    live_trading_check("directory_routes", async {
        use wallet_core::capabilities::{Action, Family};
        use wallet_core::routes::{RouteGraph, VENUES};
        let ctx = mainnet();
        let (pools, overview) = wallet_core::markets::all_markets(&ctx).await?;
        if pools.is_empty() {
            return Err("fixture_drift: empty trading directory".into());
        }
        for source in &overview.sources {
            eprintln!("TRADING_SOURCE {}", serde_json::to_string(source)?);
            if source.error.is_some() || source.stale {
                return Err(format!("endpoint_failure: degraded {} source", source.source).into());
            }
        }
        let hubs: Vec<String> =
            [ctx.network.wquai.clone(), ctx.network.wqi.clone(), ctx.network.ecosystem.usdt.as_ref().map(|u| u.address.clone())]
                .into_iter()
                .flatten()
                .collect();
        let graph = RouteGraph::new(&pools, &hubs);
        let tokens: Vec<_> = graph.tokens().take(24).map(str::to_owned).collect();
        assert!(!tokens.is_empty());
        assert!(graph.route(&tokens[0], "0x0000000000000000000000000000000000000000").is_none());
        // Disconnected listings are valid. Only returned routes promise adapter support.
        for a in &tokens {
            for b in &tokens {
                if a == b {
                    continue;
                }
                if let Some(route) = graph.route(a, b) {
                    assert_eq!(route.path.first(), Some(a));
                    assert_eq!(route.path.last(), Some(b));
                    assert!(route.swaps.iter().all(|(venue, _)| VENUES.contains(venue)));
                }
            }
        }
        for pool in pools.iter().take(120) {
            assert_ne!(pool.token0.address, pool.token1.address);
            if let Some(family) = Family::for_pool(pool) {
                assert!(family.support(Action::Discover).supported);
            }
        }
        Ok(())
    })
    .await;
}

/// Multicall3 answers a real batch, positionally, and tolerates a call that reverts.
#[tokio::test]
#[ignore = "network"]
async fn multicall_batches_reads_and_survives_a_bad_call() {
    use wallet_core::multicall::{Arg, Call, Multicall, address_word, word};
    let ctx = mainnet();
    let mc = Multicall::open(&ctx).await.expect("mainnet pins a Multicall3");
    let wqi_wquai = "0x00602f12ea0491f02865aa6c418815319e2a645b";
    let wqi = "0x002b2596ecf05c93a31ff916e8b456df6c77c750";
    let calls = vec![
        Call::view(wqi_wquai, "token0()", &[]),
        Call::view(wqi_wquai, "totalSupply()", &[]),
        Call::view(wqi_wquai, "getReserves()", &[]),
        // A function this contract does not have: the batch must survive it.
        Call::view(wqi_wquai, "definitelyNotAFunction()", &[]),
        Call::view(wqi, "balanceOf(address)", &[Arg::Addr(wqi_wquai.into())]),
    ];
    let out = mc.try_all(&calls).await.unwrap();
    assert_eq!(out.len(), calls.len(), "results are positional");
    assert_eq!(address_word(out[0].as_ref().unwrap(), 0), wqi, "token0 of the WQI/WQUAI pair");
    assert!(!word(out[1].as_ref().unwrap(), 0).is_zero(), "the pair has LP outstanding");
    let reserves = out[2].as_ref().unwrap();
    assert!(!word(reserves, 0).is_zero() && !word(reserves, 1).is_zero(), "both reserves are non-zero");
    assert!(out[3].is_none(), "a missing function fails its own row, not the batch");
    // The pair's WQI balance is its reserve0.
    assert_eq!(word(out[4].as_ref().unwrap(), 0), word(reserves, 0));
}

/// The batched gauge read and the sequential one must agree exactly — otherwise Multicall3 is a
/// silent correctness risk rather than a speed win.
#[tokio::test]
#[ignore = "network"]
async fn batched_and_sequential_gauge_reads_agree() {
    let mut ctx = mainnet();
    let owners = vec!["0x007fb65c1c53183555db3dbaf82919c5f5ee0677".to_string()];
    let batched = wallet_core::gauge::open(&ctx, &owners).await.unwrap();
    // Drop the Multicall3 pin to force the fallback path.
    ctx.network.ecosystem.multicall3 = None;
    let sequential = wallet_core::gauge::open(&ctx, &owners).await.unwrap();
    assert_eq!(batched.pools.len(), sequential.pools.len(), "same pool count");
    assert_eq!(batched, sequential, "batched and sequential gauge reads disagree");
    assert!(!batched.pools.is_empty());
    // And the same for positions, which lean on the gauge for staked balances.
    let (pools, _) = wallet_core::markets::pools(&ctx).await.unwrap();
    let seq = wallet_core::liquidity::positions(&ctx, &owners, &pools, Some(&sequential), None).await;
    let mut ctx2 = mainnet();
    ctx2.network.ecosystem.multicall3 = wallet_core::network::Ecosystem::mainnet().multicall3;
    let bat = wallet_core::liquidity::positions(&ctx2, &owners, &pools, Some(&batched), None).await;
    assert_eq!(bat, seq, "batched and sequential position reads disagree");
    assert!(!bat.is_empty(), "this address provides liquidity, so something should be found");
}

/// The subgraph answers real candles, and they agree with the ones the wallet builds from logs.
/// Disagreement here would mean the chart shows a different market depending on the data source.
#[tokio::test]
#[ignore = "network"]
async fn subgraph_candles_match_the_chain_built_ones() {
    let ctx = mainnet();
    let pair = "0x00602f12ea0491f02865aa6c418815319e2a645b"; // WQI/WQUAI, the deepest pool.
    let indexed = wallet_core::subgraph::candles(&ctx, pair, 3_600, 24).await.unwrap();
    assert!(!indexed.is_empty(), "the subgraph has hourly candles for the deepest pool");
    assert!(indexed.windows(2).all(|w| w[0].start < w[1].start), "oldest first, strictly increasing");
    for c in &indexed {
        assert!(c.low <= c.open && c.open <= c.high, "{c:?}");
        assert!(c.low <= c.close && c.close <= c.high, "{c:?}");
        assert!(c.low > 0.0, "a price is positive: {c:?}");
    }
    // Build the same window from logs and compare the closing prices.
    let (pools, _) = wallet_core::markets::pools(&ctx).await.unwrap();
    let pool = pools.iter().find(|p| p.address == pair).unwrap();
    let now = wallet_core::registry::now();
    let events = wallet_core::markets::pool_events(&ctx, pool, now - 24 * 3_600, 4).await.unwrap();
    let base0 = wallet_core::markets::base_is_token0(pool, None, ctx.network.wquai.as_deref(), ctx.network.wqi.as_deref());
    let built = wallet_core::markets::candles(&events, pool, base0, 3_600, now, 24);
    assert!(!built.is_empty());
    // Compare buckets present in both. The two are independent derivations, so allow a little
    // slack, but a factor-of-two difference would mean one of them is wrong.
    let mut compared = 0;
    for a in &indexed {
        let Some(b) = built.iter().find(|b| b.start == a.start) else { continue };
        let ratio = a.close / b.close;
        assert!((0.9..1.1).contains(&ratio), "bucket {}: subgraph {} vs logs {}", a.start, a.close, b.close);
        compared += 1;
    }
    assert!(compared >= 3, "only {compared} buckets overlapped; the comparison is too weak");
    // A timeframe the subgraph does not index says so rather than returning the wrong shape.
    assert!(wallet_core::subgraph::candles(&ctx, pair, 86_400, 7).await.is_err());
}

/// `chain_pools` is the no-explorer path. Batched and unbatched must find the same pools, or a
/// network without an indexer would see a different DEX.
#[tokio::test]
#[ignore = "network"]
async fn chain_pools_batched_matches_unbatched() {
    // Force the chain path by removing the explorer API, and start from a cold cache on both
    // sides so the batched token-metadata read is actually exercised rather than remembered.
    let chain_only = || {
        let mut ctx = mainnet();
        ctx.network.explorer_api = None;
        ctx.explorer = wallet_core::explorer::Explorer::for_network(&ctx.network);
        ctx
    };
    let ctx = chain_only();
    let started = std::time::Instant::now();
    let (batched, overview) = wallet_core::markets::pools(&ctx).await.unwrap();
    let batched_ms = started.elapsed().as_millis();
    let mut ctx = chain_only();
    ctx.network.ecosystem.multicall3 = None;
    let started = std::time::Instant::now();
    let (sequential, _) = wallet_core::markets::pools(&ctx).await.unwrap();
    println!("factory directory: batched {batched_ms} ms · sequential {} ms · {} pairs", started.elapsed().as_millis(), batched.len());
    assert!(!batched.is_empty(), "the factory has pairs");
    assert_eq!(batched.len(), sequential.len());
    // Identity must match exactly; reserves must not be required to. The two reads are seconds
    // apart on a live chain — 883 ms against 5,686 ms here — and a trade landing in that window
    // moves the busiest pool, which is not the batching disagreeing with anything. So compare the
    // part batching could actually get wrong, and bound the part the chain is entitled to change:
    // a batch that paired reserves with the wrong pool would be out by far more than a trade.
    for (b, q) in batched.iter().zip(&sequential) {
        assert_eq!((&b.address, &b.token0, &b.token1, b.venue), (&q.address, &q.token0, &q.token1, q.venue), "batched read disagrees");
        for (a, c) in [(b.reserve0, q.reserve0), (b.reserve1, q.reserve1)] {
            assert!(a > 0.0 && c > 0.0, "{} has an empty side: {a} vs {c}", b.address);
            assert!(a / c > 0.5 && a / c < 2.0, "{} reserves are not the same pool's: {a} vs {c}", b.address);
        }
    }
    // Symbols and decimals came from the batch, not from a fallback to the short address.
    assert!(batched.iter().any(|p| p.token0.symbol == "WQI" || p.token1.symbol == "WQI"), "WQI is named");
    // Today's factory is far under the ceiling, so nothing is omitted and nothing is claimed.
    assert!(overview.omitted.is_empty(), "{:?}", overview.omitted);
}

/// The launch AMM's directory reports the whole factory, newest pair first, and says so honestly
/// when a ceiling applies. Graduated curves land here, so this is the factory that grows.
#[tokio::test]
#[ignore = "network"]
async fn the_launch_amm_directory_is_whole_and_newest_first() {
    use wallet_core::markets::{MAX_FACTORY_PAIRS, launch_amm_pools};
    let ctx = mainnet();
    let directory = launch_amm_pools(&ctx).await.unwrap();
    println!("launch AMM: {} of {} pairs read", directory.read, directory.total);
    assert!(directory.total >= 2, "the launch AMM has pairs: {}", directory.total);
    assert_eq!(directory.read, directory.total.min(MAX_FACTORY_PAIRS as usize));
    assert!(directory.omitted("launch AMM factory").is_none(), "nothing is omitted below the ceiling");
    // Every pair it read is a real pool with both sides named.
    for pool in &directory.pools {
        assert!(pool.address.starts_with("0x00"), "{}", pool.address);
        assert!(!pool.token0.symbol.is_empty() && !pool.token1.symbol.is_empty(), "{pool:?}");
    }
}

/// The launch-zone gauges answer, and a campaign reads the way the app shows it.
///
/// Read-only: `zone::open` verifies each pinned deployment, reads its pools, and prices nothing
/// it cannot see. SMOL/WQI was the first enrolled pool when this was written, which is why it is
/// the one asserted on — a campaign that ends will show `Ended` rather than disappearing.
#[tokio::test]
#[ignore = "network"]
async fn launch_zone_campaigns_read() {
    let ctx = mainnet();
    for pin in &ctx.network.ecosystem.zone_gauges {
        ctx.verify_pinned(pin, "zone gauge").await.unwrap_or_else(|e| panic!("{}: {e}", pin.address));
    }
    let view = wallet_core::zone::open(&ctx, &[]).await.unwrap();
    assert_eq!(view.gauges.len(), 2, "both pinned deployments answered");
    assert!(view.pools.len() >= 13, "pools: {}", view.pools.len());
    let smol = view.pool_for("0x0050287dad80029957a3c696ee3d876abadeaf92").expect("the SMOL/WQI campaign");
    assert_eq!(smol.pid, 0);
    assert!(smol.campaign.exists() && smol.campaign.activated, "{:?}", smol.campaign);
    assert_eq!(smol.rewards.len(), 1);
    let reward = &smol.rewards[0];
    assert_eq!(reward.token.symbol, "SMOL");
    // 25,000,000 SMOL over 84 days is 297,619 a day — the figure Quainance quotes for this pool.
    assert!((reward.per_day() - 297_619.0).abs() < 1.0, "{}", reward.per_day());
    assert!(smol.activation_bps() == 10_000, "long past its activation stake");
    assert!(!smol.lp_supply.is_zero() && smol.staked_share_bps() > 0, "the staked share is known");

    // F12, the other family: a zone gauge's `currentRate` is plain atoms per second, with none of
    // the main gauge's 1e18 above the token's own units. The campaign figure above pins that for
    // one pool and stops meaning anything when it ends, so this pins the unit itself: what a live
    // stream still owes is `rate * seconds_left`, and the pool reports that as `remaining`. The
    // identity is in atoms, so it holds whatever the reward token's decimals are — which is the
    // part a fixture with an 18-decimal mock could never establish.
    let now = wallet_core::registry::now();
    let mut checked = 0;
    for pool in &view.pools {
        for reward in pool.rewards.iter().filter(|r| r.live(now)) {
            let seconds = wallet_core::sdk::U256::from(reward.period_finish.saturating_sub(now));
            let implied = reward.rate * seconds;
            // `remaining` is settled when someone last touched the pool, not continuously, so it
            // lags `rate * seconds_left` by however long the pool has been idle — hours on a quiet
            // campaign. The claim under test is the unit, which is an order-of-magnitude claim:
            // per-day instead of per-second is 86,400x out and a 1e18 scale is astronomically
            // further. A factor of ten separates those from any settlement lag.
            let ten = wallet_core::sdk::U256::from(10u64);
            assert!(
                implied <= reward.remaining.saturating_mul(ten) && reward.remaining <= implied.saturating_mul(ten),
                "{} pid {}: rate*{seconds}s = {implied} atoms against {} remaining — the rate is not atoms/second",
                reward.token.symbol,
                pool.pid,
                reward.remaining
            );
            checked += 1;
        }
    }
    assert!(checked > 0, "no live zone stream to qualify the rate unit against");
    eprintln!("ZONE_RATE {checked} live stream(s) match rate x seconds remaining");
}

/// The launch-zone write path, against a real staker and without signing: a position counts LP
/// staked in a zone gauge, and the exact calls the reviews build — `withdraw`, `getReward` with the
/// pool's reward list, `exit` — all simulate from that staker's address. Over-withdrawing reverts,
/// so a simulated success is not the node ignoring the call.
#[tokio::test]
#[ignore = "network"]
async fn launch_zone_stake_calls_simulate_from_a_real_staker() {
    use serde_json::{Value, json};
    use wallet_core::sdk::{BlockTag, QuaiAddress, U256};
    let ctx = mainnet();
    // The largest SMOL/WQI staker on 2026-09-17 (trade-zone subgraph, `tradeGaugeStakePositions`).
    let staker = "0x0048ccd296f2484ec4e61d8375d15bfc2991b780";
    let smol_pair = "0x0050287dad80029957a3c696ee3d876abadeaf92";
    let owners = vec![staker.to_string()];
    let zone = wallet_core::zone::open(&ctx, &owners).await.unwrap();
    let pool = zone.pool_for(smol_pair).expect("SMOL/WQI campaign").clone();
    assert!(!pool.staked.is_zero(), "this address stakes in SMOL/WQI");

    // Positions put that stake on the pair and name the gauge it sits in.
    let gauge = wallet_core::gauge::open(&ctx, &owners).await.ok();
    let (pools, _) = wallet_core::markets::pools(&ctx).await.unwrap();
    let positions = wallet_core::liquidity::positions(&ctx, &owners, &pools, gauge.as_ref(), Some(&zone)).await;
    let position = positions.iter().find(|p| p.pair.eq_ignore_ascii_case(smol_pair)).expect("the staked position is listed");
    assert_eq!(position.gauge, Some(wallet_core::gauge::GaugeKind::Zone));
    assert_eq!((position.pid, position.lp_staked), (Some(pool.pid), pool.staked));

    let abi = wallet_core::sdk::abi::AbiInterface::from_human_readable(wallet_core::zone::ZONE_GAUGE_ABI).unwrap();
    let address: QuaiAddress = pool.gauge.parse().unwrap();
    let contract = wallet_core::sdk::contracts::Contract::new(address, abi, &ctx.node.provider);
    let from: QuaiAddress = staker.parse().unwrap();
    let pid = json!(pool.pid.to_string());
    let tokens = Value::Array(pool.reward_addresses().iter().map(|t| json!(t)).collect());
    let half = (pool.staked / U256::from(2u64)).to_string();
    for (method, args) in [
        ("withdraw", vec![pid.clone(), json!(half)]),
        ("getReward", vec![pid.clone(), tokens.clone()]),
        ("exit", vec![pid.clone(), tokens.clone()]),
    ] {
        let call = contract.prepare(method, &args, U256::ZERO).unwrap();
        contract.simulate(from, &call, BlockTag::Latest, Some(900_000)).await.unwrap_or_else(|e| panic!("{method} reverted: {e}"));
    }
    let too_much = contract.prepare("withdraw", &[pid, json!((pool.staked + U256::from(1u64)).to_string())], U256::ZERO).unwrap();
    assert!(contract.simulate(from, &too_much, BlockTag::Latest, Some(900_000)).await.is_err(), "withdrawing more than staked reverts");
}

/// An observed transaction's value and gas come from the node: a real reward claim carries no
/// QUAI and paid a non-zero fee.
#[tokio::test]
#[ignore = "network"]
async fn an_observed_transaction_states_its_value_and_gas() {
    let ctx = mainnet();
    // A launch-zone `getReward` (trade-zone subgraph, 2026-09-16).
    let hash = "0x0043000adcd4b844bb8ea9e75bbf22025125f6e8c900ffe3ae7cd84dbef965bf";
    let cost = wallet_core::track::chain_cost(&ctx.node.provider, hash).await.unwrap();
    assert!(!cost.qi);
    assert_eq!(cost.value, Some(wallet_core::sdk::U256::ZERO), "claiming sends no QUAI");
    let fee = cost.fee.expect("mined, so the receipt states the fee");
    assert!(!fee.is_zero() && cost.fee_final);
    eprintln!("gas {}", cost.text(fee));
}

/// The batched launch-zone read and the call-per-value read agree exactly, for a real staker —
/// the batched one only saves round trips.
#[tokio::test]
#[ignore = "network"]
async fn batched_and_sequential_zone_reads_agree() {
    let owners = vec!["0x0048ccd296f2484ec4e61d8375d15bfc2991b780".to_string()];
    let ctx = mainnet();
    let started = std::time::Instant::now();
    let batched = wallet_core::zone::open(&ctx, &owners).await.unwrap();
    let batched_ms = started.elapsed().as_millis();
    let mut plain = mainnet();
    plain.network.ecosystem.multicall3 = None;
    let started = std::time::Instant::now();
    let sequential = wallet_core::zone::open(&plain, &owners).await.unwrap();
    eprintln!("batched {batched_ms} ms · sequential {} ms · {} pools", started.elapsed().as_millis(), batched.pools.len());
    // Rewards stream every block and the two reads are seconds apart: compare everything else.
    let settled = |mut v: wallet_core::zone::ZoneView| {
        for p in &mut v.pools {
            p.campaign.emitted_reward = Default::default();
            for r in &mut p.rewards {
                (r.remaining, r.earned) = Default::default();
            }
        }
        v
    };
    assert_eq!(settled(batched.clone()), settled(sequential), "batched and sequential zone reads disagree");
    assert!(batched.pools.iter().any(|p| !p.staked.is_zero()), "the staker's stake is read");
}

/// A watch-only session's balances read the same whether token balances are batched or read one
/// by one, and the account reads (balance, nonce, locked) run together.
#[tokio::test]
#[ignore = "network"]
async fn batched_token_balances_match_sequential_ones() {
    let dir = tempfile::tempdir().unwrap();
    let paths = wallet_core::paths::Paths::resolve(Some(dir.path().to_path_buf())).unwrap();
    let registry = wallet_core::registry::Registry::new(paths);
    // Token balances are per signing account, so this needs one: the public test phrase's.
    let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    let meta = registry.create_hd("w", phrase, "english", "", "password123", true).unwrap();
    let open = |multicall: bool| {
        let mut network = NetworkProfile::builtins().into_iter().next().unwrap();
        if !multicall {
            network.ecosystem.multicall3 = None;
        }
        wallet_core::session::Session::open(registry.clone(), wallet_core::config::AppConfig::default(), meta.clone(), network).unwrap()
    };
    let mut batched = open(true);
    let started = std::time::Instant::now();
    let a = batched.token_balances(None).await;
    eprintln!("batched tokens {} ms", started.elapsed().as_millis());
    let mut plain = open(false);
    let started = std::time::Instant::now();
    let b = plain.token_balances(None).await;
    eprintln!("sequential tokens {} ms", started.elapsed().as_millis());
    let (a, b) = (a.unwrap(), b.unwrap());
    let key = |v: &[wallet_core::ops::TokenBalance]| v.iter().map(|t| (t.token.address.clone(), t.balance)).collect::<Vec<_>>();
    assert_eq!(key(&a), key(&b));
    assert!(a.len() >= 2, "the default tokens were read: {}", a.len());
    // Warm: the Multicall3 pin is verified once, then every refresh is one round trip.
    let started = std::time::Instant::now();
    batched.token_balances(None).await.unwrap();
    eprintln!("batched tokens, warm {} ms", started.elapsed().as_millis());
    let started = std::time::Instant::now();
    let accounts = batched.quai_balances().await.unwrap();
    eprintln!("accounts {} ms", started.elapsed().as_millis());
    assert_eq!(accounts.len(), 1);
}

/// A sealed conversation's read asks the node for several tags at once (each epoch the window
/// touches, plus the v1 tag). The node must accept that filter, and a public channel read the
/// same way still finds its posts.
#[tokio::test]
#[ignore = "network"]
async fn conversation_reads_ask_for_every_epoch_tag() {
    let ctx = mainnet();
    let seed = |b: u8| wallet_core::sdk::payments::PrivatePaymentCode::from_seed(&[b; 32], 0).unwrap();
    let (alice, bob) = (seed(41), seed(42));
    let c = wallet_core::messages::conversation(&alice, bob.public_code()).unwrap();
    // A made-up conversation has no posts, but the multi-topic query must still be answered.
    let posts = wallet_core::messages::conversation_posts(&ctx, &c, wallet_core::messages::BOARD_BLOCKS).await.unwrap();
    assert!(posts.is_empty());
    let general = wallet_core::messages::channel_tag("general").unwrap();
    wallet_core::messages::channel(&ctx, &general, wallet_core::messages::BOARD_BLOCKS).await.unwrap();
}

/// Read configured launch families with explicit adapter identity; optional families need not have rows.
#[tokio::test]
#[ignore = "network"]
async fn trading_readonly_launch_directory_has_typed_sources() {
    live_trading_check("launch_directory", async {
        use wallet_core::capabilities::{Action, Family};
        let ctx = mainnet();
        for venue in wallet_core::launches::LAUNCH_VENUES {
            assert!(Family::for_launch_venue(venue).is_some());
        }
        let launches = wallet_core::launches::launches(&ctx, 40).await?;
        if launches.is_empty() {
            return Err("fixture_drift: no launch rows returned".into());
        }
        for launch in launches {
            assert!(launch.token.parse::<wallet_core::sdk::QuaiAddress>().is_ok());
            assert!(launch.price_quai.is_none_or(|p| p.is_finite() && p > 0.0));
            assert!(launch.progress_bps.is_none_or(|p| p <= 10_000));
            assert!(!launch.symbol.chars().any(char::is_control));
            // Unknown identities stay discoverable without authorizing an execution adapter.
            if let Some(family) = launch.venue_kind {
                assert!(family.support(Action::Discover).supported);
            }
        }
        Ok(())
    })
    .await;
}

/// A token on its bonding curve reads from the curve itself once the pinned launcher vouches for
/// it: the position matches the curve's own arithmetic, the drawn curve rises, a curve the
/// launcher does not name for the token is refused, and the buy the review builds simulates.
#[tokio::test]
#[ignore = "network"]
async fn a_bonding_curve_reads_verifies_and_simulates() {
    use wallet_core::sdk::{BlockTag, QuaiAddress, U256};
    let ctx = mainnet();
    let launches = wallet_core::launches::launches(&ctx, 200).await.unwrap();
    let bonding: Vec<_> = launches.iter().filter(|l| l.phase == wallet_core::launches::Phase::Bonding && l.curve.is_some()).collect();
    assert!(bonding.len() >= 2, "need two tokens on their curves");
    // Which launchpads are actually represented, and whether a symbol alone identifies a token.
    // Two launchpads' curves sit in one list; they are different contracts with different
    // operators, and nothing stops both from minting the same ticker.
    let mut by_launchpad: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut by_symbol: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for l in &bonding {
        let pad = format!("{} ({:?})", l.venue, l.venue_kind);
        *by_launchpad.entry(pad).or_default() += 1;
        by_symbol.entry(l.symbol.to_uppercase()).or_default().push(l.token.to_lowercase());
    }
    eprintln!("LAUNCHPADS {by_launchpad:?}");
    let mut clashes: Vec<_> = by_symbol.iter().filter(|(_, t)| t.len() > 1).collect();
    clashes.sort();
    for (symbol, tokens) in &clashes {
        eprintln!("TICKER_CLASH {symbol} is {} different tokens: {tokens:?}", tokens.len());
    }
    // A clash is the launchpads' business, not a defect here — but every row must still be a
    // distinct contract, or the list is collapsing two tokens into one.
    for (symbol, tokens) in &clashes {
        let mut unique = tokens.to_vec();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), tokens.len(), "{symbol} appears twice as the same contract");
    }
    // Not `bonding[0]`: a live launch can sit on its curve reporting `graduationQuoteAmount() == 0`
    // (QHUB did on 2026-09-22, 2,425 QUAI raised and 16% sold), and such a curve has no target to
    // measure progress against and nothing to draw. This test is about a curve that does, so it
    // takes the first one that does rather than assuming the list order.
    // One candidate per launchpad, not the first few in list order: both families are on this
    // list (29 Hartii to 14 Quainance on 2026-09-22) but they are not interleaved, so taking the
    // head of the list checks whichever one happens to sort first and silently skips the other.
    let mut per_family: Vec<&&wallet_core::launches::Launch> = Vec::new();
    for candidate in &bonding {
        if !per_family.iter().any(|seen| seen.venue_kind == candidate.venue_kind) {
            per_family.push(candidate);
        }
    }
    let mut found = None;
    for candidate in per_family {
        let market = wallet_core::curve::market(&ctx, &candidate.token, candidate.curve.as_deref().unwrap(), &[]).await.unwrap();
        if market.target_quai() > 0.0 {
            // Which basis `progress_bps` reports on, settled by the chain rather than assumed, and
            // for every family present rather than whichever curve this test goes on to use. A
            // bonding curve is convex, so the share of the raise reached and the share of tokens
            // sold are different numbers — and the one shown beside `raised / target` has to be
            // the raise share, or one column means two things depending on the row.
            let quote_share = (market.raised_quai() / market.target_quai() * 10_000.0).round() as i64;
            let token_share = market.sold_bps() as i64;
            let family =
                if wallet_core::hartii_tx::matches_curve_runtime(&ctx, &market.curve).await.unwrap() { "Hartii" } else { "Quainance" };
            eprintln!(
                "CURVE_BASIS {family} {} progress {} bps, raise {quote_share}, tokens {token_share}",
                candidate.symbol, market.progress_bps
            );
            assert!(
                (market.progress_bps as i64 - quote_share).abs() <= 100,
                "{family} curve {} reports {} bps against a raise share of {quote_share} (tokens sold: {token_share})",
                candidate.symbol,
                market.progress_bps
            );
        }
        if found.is_none() && !market.graduated && market.target_quai() > 0.0 && market.raised_quai() < market.target_quai() {
            found = Some((*candidate, market));
            continue;
        }
        if found.is_some() {
            continue;
        }
        eprintln!("{}: skipped, target {:.0} QUAI", candidate.symbol, market.target_quai());
    }
    let Some((a, m)) = found else {
        eprintln!("no bonding token with a graduation target among the first {}; nothing to qualify", bonding.len().min(8));
        return;
    };
    let b = bonding.iter().find(|l| l.curve != a.curve).expect("a second, different curve");
    eprintln!(
        "{}: {:.0}/{:.0} QUAI raised, {}% sold, spot {:.10}",
        a.symbol,
        m.raised_quai(),
        m.target_quai(),
        m.sold_bps() / 100,
        m.spot_price
    );
    assert_eq!(m.points.len(), wallet_core::curve::CURVE_POINTS);
    assert!(m.points.windows(2).all(|w| w[1].1 >= w[0].1 && w[1].0 > w[0].0), "the curve rises left to right");
    // The spot price sits on the drawn curve between its neighbours.
    let raised = m.raised_quai();
    let below = m.points.iter().rev().find(|p| p.0 <= raised).map_or(m.points[0].1, |p| p.1);
    let above = m.points.iter().find(|p| p.0 >= raised).map_or(m.spot_price, |p| p.1);
    assert!(m.spot_price >= below * 0.95 && m.spot_price <= above * 1.05, "spot {} between {below} and {above}", m.spot_price);
    // Another token's curve is not this token's curve.
    let wrong = wallet_core::curve::market(&ctx, &a.token, b.curve.as_deref().unwrap(), &[]).await;
    assert!(matches!(wrong, Err(wallet_core::CoreError::Rejected(_))), "{wrong:?}");
    // The exact buy call, from an address with QUAI, simulates. The two launchers do not share a
    // buy: Quainance takes a minimum and an on-chain deadline, Hartii takes only a minimum and
    // bounds the trade by the output instead. Using one family's selector on the other reverts, so
    // this dispatches the way the wallet itself does rather than assuming which curve it drew.
    let hartii = wallet_core::hartii_tx::matches_curve_runtime(&ctx, &m.curve).await.unwrap();
    let (curve_abi, buy_args) = if hartii {
        (wallet_core::hartii::CURVE_ABI, vec![serde_json::json!("1")])
    } else {
        (wallet_core::curve::CURVE_ABI, vec![serde_json::json!("1"), serde_json::json!((wallet_core::registry::now() + 600).to_string())])
    };
    eprintln!("{} buys through the {} launcher", a.symbol, if hartii { "Hartii" } else { "Quainance" });
    let abi = wallet_core::sdk::abi::AbiInterface::from_human_readable(curve_abi).unwrap();
    let curve: QuaiAddress = m.curve.parse().unwrap();
    let contract = wallet_core::sdk::contracts::Contract::new(curve, abi, &ctx.node.provider);
    let holder: QuaiAddress = funded_account(&ctx).await;
    let call = contract.prepare("buy", &buy_args, U256::from(10u128.pow(17))).unwrap();
    let out = contract.simulate(holder, &call, BlockTag::Latest, Some(600_000)).await;
    eprintln!("0.1 QUAI buy simulates: {:?}", out.as_ref().map(|v| v.first().cloned()));
    assert!(out.is_ok(), "{out:?}");
}

/// A curve's trades from the launch index read as a pool's events, and its latest price is the
/// one the launch list shows.
#[tokio::test]
#[ignore = "network"]
async fn a_bonding_curve_charts_from_its_trades() {
    use wallet_core::markets::{PoolEvent, Venue};
    let ctx = mainnet();
    let (markets, _) = wallet_core::markets::all_markets(&ctx).await.unwrap();
    let curve = markets
        .iter()
        .filter(|m| m.venue == Venue::Curve)
        .max_by_key(|m| m.curve.as_ref().map_or(0, |c| c.progress_bps.unwrap_or(0)))
        .cloned()
        .unwrap();
    let now = wallet_core::registry::now();
    let events = wallet_core::markets::pool_events(&ctx, &curve, now.saturating_sub(30 * 86_400), 5).await.unwrap();
    let swaps = events.iter().filter(|e| matches!(e, PoolEvent::Swap { .. })).count();
    eprintln!("{}: {} swaps in 30 days", curve.token0.symbol, swaps);
    if swaps == 0 {
        return;
    }
    let stats = wallet_core::markets::pair_stats(&events, &curve, true, now);
    let (charted, listed) = (stats.price.unwrap(), curve.spot_price().unwrap());
    eprintln!("charted {charted:.12} QUAI, listed {listed:.12} QUAI");
    // These are different quantities and only approximately agree. The chart's last point is the
    // *average* price the last trade filled at; the list shows the curve's *marginal* price after
    // it. On a bonding curve a buy always fills below the price it leaves behind, so they diverge
    // by roughly the size of that trade — QAXE stood 6.9% apart on 2026-09-22 after 332 swaps in
    // thirty days. The band is wide because the quantity is, not because the check is weak: it
    // still catches a units error, an inverted pair, or a chart left days behind the curve.
    assert!((charted / listed - 1.0).abs() < 0.25, "the chart's last price and the curve's are unrelated");
}

/// Keyless first-hand quote checks use actual verified legs, independent of changing venue liquidity.
#[tokio::test]
#[ignore = "network"]
async fn trading_readonly_quote_uses_verified_adapter() {
    live_trading_check("verified_quote", async {
        use wallet_core::sdk::U256;
        use wallet_core::swap::{Router, SwapAsset};
        let ctx = mainnet();
        let usdt = SwapAsset::Token {
            address: ctx.network.ecosystem.usdt.as_ref().ok_or("fixture_drift: no USDT pin")?.address.clone(),
            symbol: "USDT".into(),
            decimals: 6,
        };
        let router = Router::open(&ctx.app, &ctx.node, &ctx.network, Trust::FirstHand).await?;
        let quote = router.quote(&SwapAsset::Quai, &usdt, U256::from(10u128.pow(17)), 100, None).await?;
        if quote.legs.is_empty() {
            return Err("fixture_drift: empty quote legs".into());
        }
        for leg in &quote.legs {
            assert!(router.router_for(leg.venue).is_some(), "quoted adapter must authenticate");
            assert!(U256::from_str_radix(&leg.amount_out, 10)? > U256::ZERO);
            assert!(U256::from_str_radix(&leg.minimum_out, 10)? <= U256::from_str_radix(&leg.amount_out, 10)?);
        }
        for pair in quote.legs.windows(2) {
            assert_eq!(pair[0].amount_out, pair[1].amount_in);
        }
        assert_eq!(quote.amount_out, quote.legs.last().unwrap().amount_out);
        eprintln!("TRADING_QUOTE {}", serde_json::to_string(&quote)?);
        Ok(())
    })
    .await;
}

/// The Network screen's statistics decode from the live explorer: all three algorithms hashing,
/// a day of hashrate history, two days of hourly activity, and a gas price people paid that is the
/// same order as the node's own suggestion.
#[tokio::test]
#[ignore = "network"]
async fn network_statistics_read_from_the_explorer() {
    let ctx = mainnet();
    let s = wallet_core::chainstats::chain_stats(&ctx).await.unwrap().value;
    assert!(s.hashrate.sha > 1e15 && s.hashrate.scrypt > 1e10 && s.hashrate.kawpow > 1e8, "{:?}", s.hashrate);
    assert!(s.hashrate_history.len() >= 20, "hashrate history: {}", s.hashrate_history.len());
    assert!(s.hours.len() >= 40, "hours: {}", s.hours.len());
    assert!(s.total_transactions.is_some_and(|t| t > 100_000_000));
    let paid = s.hours.last().and_then(|h| h.avg_gas_price_gwei()).unwrap();
    let node = ctx.node.provider.gas_price(wallet_core::network::ZONE).await.unwrap();
    let node_gwei = wallet_core::amount::to_f64(node, 9);
    println!("paid {paid:.0} gwei/gas last hour · node suggests {node_gwei:.0} · {} tx/h", s.hours.last().unwrap().transactions);
    assert!(paid > node_gwei / 20.0 && paid < node_gwei * 20.0, "paid {paid} vs node {node_gwei}");
}

/// Launch logos resolve through Quainance's media proxy, and each is an image the proxy serves.
#[tokio::test]
#[ignore = "network"]
async fn launch_logos_resolve_through_the_media_proxy() {
    let ctx = mainnet();
    let launches = wallet_core::launches::launches(&ctx, 50).await.unwrap();
    let logos = wallet_core::launches::logos(&ctx, &launches).await;
    let with_metadata = launches.iter().filter(|l| l.metadata_uri.is_some()).count();
    println!("{} of {} launches carry metadata; {} logos resolved", with_metadata, launches.len(), logos.len());
    assert!(!logos.is_empty(), "at least one launch has a logo");
    let (token, url) = logos.iter().next().unwrap();
    assert!(url.starts_with(wallet_core::launches::MEDIA_PROXY), "{url}");
    let rendition = wallet_core::media::load(&ctx.app, url, 32).await.unwrap();
    assert!(rendition.is_some(), "the logo for {token} decodes");
}

/// The marketplace indexer's own figures: every collection carries the counts a list sorts on,
/// the sale history comes back newest first, and a window over it adds only QUAI-paid sales.
#[tokio::test]
#[ignore = "network"]
async fn marketplace_stats_and_trades_read() {
    let ctx = mainnet();
    let collections = wallet_core::market::collection_stats(&ctx).await.unwrap();
    assert!(!collections.is_empty(), "the indexer lists collections");
    assert!(collections.iter().all(|c| c.address.starts_with("0x") && c.address.len() == 42));
    assert!(collections.iter().any(|c| c.floor.is_some()), "some collection has a floor");
    assert!(collections.iter().any(|c| c.trades.is_some_and(|t| t > 0)), "some collection has traded");
    let trades = wallet_core::market::trades(&ctx, None).await.unwrap();
    assert!(!trades.is_empty(), "the marketplace has sales");
    assert!(trades.windows(2).all(|w| w[0].at >= w[1].at), "newest first");
    assert!(trades.iter().all(|t| t.at > 1_700_000_000), "every sale has a readable time");
    let now = wallet_core::registry::now();
    let (all_volume, all_count) = wallet_core::market::trade_window(&trades, 3650, now);
    assert_eq!(all_count, trades.len());
    assert!(all_volume > 0.0, "sales paid in QUAI add up to something");
    let (week_volume, week_count) = wallet_core::market::trade_window(&trades, 7, now);
    assert!(week_count <= all_count && week_volume <= all_volume, "a week is inside all time");
    // One collection's own sales are a subset of the market's, and the indexer agrees.
    let busiest = collections.iter().max_by_key(|c| c.trades.unwrap_or(0)).unwrap();
    let mine = wallet_core::market::trades(&ctx, Some(&busiest.address)).await.unwrap();
    assert!(mine.iter().all(|t| t.contract == busiest.address));
    assert_eq!(mine.len() as u64, busiest.trades.unwrap_or(0), "the collection's trade count matches its sales");
}

/// The whole ABI chain against the real chain and the real gateway: read the message board's code,
/// take the CID out of its compiler tail, fetch the metadata through ipfs.qu.ai, and check the
/// bytes against the CID the bytecode committed to.
#[tokio::test]
#[ignore = "network"]
async fn a_contract_gives_up_its_own_abi() {
    let ctx = mainnet();
    let board = ctx.network.ecosystem.messages.clone().expect("the message board is pinned on mainnet");
    let found = ctx.discover_contract(&board.address, Trust::FirstHand).await.unwrap();
    assert!(found.is_contract() && found.code_len > 100, "{found:?}");
    assert_eq!(found.solc.as_deref(), Some("0.8.20"), "built with the compiler Quai needs");
    let metadata = found.metadata.as_ref().unwrap_or_else(|| panic!("no metadata: {:?}", found.metadata_error));
    assert_eq!(metadata.name, "Messages");
    assert_eq!(metadata.cid, "QmZHjrbTYGTTNfL9SoX3E3MQf2PdpB7iVwrax8qBdzj7DV");
    assert!(metadata.compiler.starts_with("0.8.20"), "{}", metadata.compiler);
    assert!(metadata.source.is_some(), "Quai builds inline the source");
    // The ABI parses and names the board's own calls.
    let interface = metadata.interface().unwrap();
    assert!(interface.functions().count() > 0);
    // Every function the dispatcher knows is declared: this contract's ABI is not hiding anything.
    assert!(found.undeclared.is_empty(), "undeclared selectors: {:?}", found.undeclared);
    assert!(found.trust_note().is_some_and(|n| n.contains("Messages")));

    // An account that is not a contract says so rather than failing.
    let plain = ctx.discover_contract("0x0000000000000000000000000000000000000001", Trust::FirstHand).await.unwrap();
    assert!(!plain.is_contract() && plain.metadata.is_none());
}

/// Reading a contract the wallet was never taught about, through the ABI it publishes itself.
#[tokio::test]
#[ignore = "network"]
async fn a_contract_can_be_read_through_its_own_abi() {
    let ctx = mainnet();
    let board = ctx.network.ecosystem.messages.clone().unwrap();
    let found = ctx.discover_contract(&board.address, Trust::FirstHand).await.unwrap();
    let interface = found.metadata.as_ref().unwrap().interface().unwrap();
    let list = wallet_core::contracts::callables(&interface);
    assert!(!list.is_empty(), "the board declares functions");
    // Reads sort first, and every one of them names its arguments and types.
    for c in &list {
        assert!(!c.signature.is_empty() && c.label().starts_with(&c.name));
        for (_, ty) in &c.inputs {
            assert!(!ty.is_empty(), "{}: an unnamed type", c.signature);
        }
    }
    assert!(list.windows(2).all(|w| w[0].read_only >= w[1].read_only), "reads first");
}

/// The SDK must decompose a wrap's spend outputs into the largest denominations, regardless of
/// how fragmented the coins paying for it are. Capping them at the input denominations (the rule
/// for ordinary Qi→Qi transfers) built a 15 Qi wrap as twelve outputs instead of two, which lost
/// the scarce Qi block slot every time — see quai-rust-sdk docs/QI_CONVERSION_SELECTION_GAP.md.
#[test]
fn a_wrap_aggregates_its_spend_outputs() {
    use quai_sdk::consensus::Denomination;
    use quai_sdk::wallet::{CandidateCoin, SelectionRequest, select_fewest, select_fewest_converting};
    use quai_sdk::{QiAddress, U256};

    // The coin set behind the stuck mainnet wrap: one 5 Qi coin and a pile of small ones.
    let address: QiAddress = "0x00f613162c07247188Cb1b3a138D15810aE147Bc".parse().expect("a Cyprus-1 Qi address");
    let mut coins = Vec::new();
    let push = |index: u8, count: usize, coins: &mut Vec<CandidateCoin>| {
        for i in 0..count {
            let mut hash = [0u8; 32];
            hash[2] = address.zone().byte();
            hash[8] = index;
            hash[9] = i as u8;
            coins.push(CandidateCoin::new(
                quai_sdk::consensus::OutPoint { transaction_hash: quai_sdk::primitives::Hash32::from_bytes(hash), index: i as u16 },
                address,
                Denomination::new(index).expect("a denomination"),
            ));
        }
    };
    push(7, 1, &mut coins); // 5000
    push(6, 12, &mut coins); // 1000 each
    push(5, 4, &mut coins); // 500 each

    // 15 Qi, paying a 78-qit fee capped at 500.
    let request = SelectionRequest::new(address.zone(), U256::from(1u64), U256::from(15_000u64), 64, 256)
        .with_fee(U256::from(78u64), U256::from(500u64));

    let wrap = select_fewest_converting(&coins, &request).expect("the wrap selects");
    let spend: Vec<u64> = wrap.spend_outputs.iter().map(|d| d.value()).collect();
    assert_eq!(spend, vec![10_000, 5_000], "a wrap must aggregate its spend outputs: {spend:?}");

    // An ordinary transfer keeps the node's denomination rule, which is what the far side needs.
    let ordinary = select_fewest(&coins, &request).expect("an ordinary send selects");
    let ordinary_spend: Vec<u64> = ordinary.spend_outputs.iter().map(|d| d.value()).collect();
    assert!(ordinary_spend.len() > spend.len(), "ordinary sends stay capped: {ordinary_spend:?}");
    assert_eq!(ordinary_spend.iter().sum::<u64>(), 15_000);
    assert_eq!(spend.iter().sum::<u64>(), 15_000, "both pay the same amount");
}

/// The HartiiLabs launchpad reads from the chain: its tokens, their curves, and which have bonded.
///
/// Observed 2026-09-21: 27 tokens, of which HRT and QAXE have bonded. Bonding sells out the curve
/// but does not retire it — both still quote — so every curve that answers becomes a market row.
#[tokio::test]
#[ignore = "network"]
async fn the_hartii_launchpad_reads_its_curves() {
    let ctx = mainnet();
    let rows = wallet_core::hartii::launches(&ctx).await.expect("the launcher reads");
    eprintln!("{} Hartii tokens, {} bonded", rows.len(), rows.iter().filter(|r| r.bonded).count());
    for r in rows.iter().take(6) {
        eprintln!("  {:<10} bonded={} raised={:.3} QUAI progress={:?}bps", r.symbol, r.bonded, r.raised_quai, r.progress_bps);
    }
    assert!(rows.len() >= 20, "the launcher held 27 on 2026-09-21");
    assert!(rows.iter().all(|r| !r.token.is_empty() && !r.curve.is_empty()));
    assert!(rows.iter().all(|r| r.progress_bps.is_some_and(|b| b <= 10_000)), "progress is a share, not a count");
    let bonded: Vec<&str> = rows.iter().filter(|r| r.bonded).map(|r| r.symbol.as_str()).collect();
    assert!(bonded.contains(&"HRT") && bonded.contains(&"QAXE"), "{bonded:?}");
    // A bonded curve has sold its whole supply, and still prices.
    for r in rows.iter().filter(|r| r.bonded) {
        assert_eq!(r.progress_bps, Some(10_000), "{} sold out", r.symbol);
        assert!(r.price_quai.is_some_and(|p| p > 0.0), "{} still quotes after bonding", r.symbol);
    }
    // Only the bonded curves are markets; the ones still raising live on the Launches screen.
    let wquai = ctx.network.wquai.clone().unwrap();
    let pools = wallet_core::hartii::curve_pools(&rows, &wquai);
    assert_eq!(pools.len(), 2, "HRT and QAXE, and nothing still raising");
    assert!(pools.iter().all(|p| ["HRT", "QAXE"].contains(&p.token0.symbol.as_str())), "{pools:?}");
    assert!(pools.iter().all(|p| p.curve.as_ref().and_then(|c| c.launchpad.as_deref()) == Some("HartiiLabs")));
    // The reserves cannot tell these two apart; their quotes differ by more than an order of
    // magnitude, which is why the price comes from the curve rather than from a ratio.
    let priced = |sym: &str| rows.iter().find(|r| r.symbol == sym).and_then(|r| r.price_quai).unwrap();
    assert!(priced("QAXE") / priced("HRT") > 10.0, "HRT {} QAXE {}", priced("HRT"), priced("QAXE"));
    // Every curve that quotes is priced from the reserves its quote confirms: on 2026-09-23 all 35
    // reproduced quoteBuy to the wei, the bonded ones from their pool, the rest from virtual reserves.
    use wallet_core::markets::PriceBasis;
    for r in rows.iter().filter(|r| r.price_quai.is_some()) {
        assert_eq!(r.price_basis, PriceBasis::ReserveSpot, "{} priced from its reserves", r.symbol);
    }
    // A bonded curve's depth is its locked pool: QAXE held 190,561 QUAI of it on 2026-09-23.
    for r in rows.iter().filter(|r| r.bonded) {
        assert!(r.locked_quai.is_some_and(|q| q > 1_000.0), "{} locked {:?}", r.symbol, r.locked_quai);
        eprintln!("  {:<10} spot {:.8} QUAI, locked {:.0} QUAI", r.symbol, r.price_quai.unwrap(), r.locked_quai.unwrap());
    }
    assert!(pools.iter().all(|p| p.curve.as_ref().is_some_and(|c| c.locked_quai.is_some() && c.price_basis == PriceBasis::ReserveSpot)));
}

/// A Hartii curve verifies and reads in a bounded time on the review path, where no pin is cached.
/// Its fifteen verification reads used to run one after another; they are one round now.
#[tokio::test]
#[ignore = "network"]
async fn a_hartii_curve_verifies_and_prices_from_its_pool() {
    let mut ctx = mainnet();
    ctx.trust = Trust::FirstHand;
    let rows = wallet_core::hartii::launches(&ctx).await.expect("the launcher reads");
    let qaxe = rows.iter().find(|r| r.symbol == "QAXE").expect("QAXE is on the launchpad");
    let started = std::time::Instant::now();
    let verified = wallet_core::hartii_tx::verified_curve(&ctx, &qaxe.token, &qaxe.curve).await.expect("QAXE's curve verifies");
    let verify = started.elapsed();
    assert!(verified.graduated && verified.fee_bps == 100, "{verified:?}");
    let started = std::time::Instant::now();
    let market = wallet_core::hartii_tx::market(&ctx, &qaxe.token, &qaxe.curve, &[]).await.expect("QAXE's market reads");
    eprintln!("verify {verify:?}, market {:?}; spot {} list {:?}", started.elapsed(), market.spot_price, qaxe.price_quai);
    // The card and the list price the same reserves, and neither includes the 1% fee.
    let list = qaxe.price_quai.unwrap();
    assert!((market.spot_price / list - 1.0).abs() < 0.01, "card {} list {list}", market.spot_price);
    assert!(market.points.is_empty() && market.target.is_zero(), "a bonded curve has no sell-out left to draw");
    // In the market list it has a 24h change from HartiiLabs, and it refreshes with the pools.
    let mut display = mainnet();
    display.trust = Trust::Cached;
    let (mut all, _) = wallet_core::markets::all_markets(&display).await.expect("the market list reads");
    let row = all.iter().find(|p| p.address.eq_ignore_ascii_case(&qaxe.curve)).expect("QAXE's curve is listed");
    eprintln!("QAXE listed at {:?}, 24h {:?}", row.spot_price(), row.change_24h());
    assert!(row.change_24h().is_some(), "HartiiLabs gives it a 24h change");
    let fresh = wallet_core::markets::refresh_reserves(&display, &all).await.expect("reserves refresh");
    let hit = fresh.iter().find(|(a, _, _)| a.eq_ignore_ascii_case(&qaxe.curve)).expect("the curve is refreshed with the pools");
    assert!((hit.2 / hit.1 / list - 1.0).abs() < 0.05, "refreshed {} vs listed {list}", hit.2 / hit.1);
    wallet_core::markets::apply_reserves(&mut all, &fresh, display.network.wquai.as_deref(), None);
}

/// The pool a deposit screen is opened from has to be findable by the deposit screen.
///
/// `markets::pools` is the main exchange only — the explorer's TVL endpoint indexes no other — so
/// resolving a pair against it answered "no pool at this address" for every launch-AMM and QuaiSwap
/// pair, including CHEEZ/QUAI, which the same screen had just listed. Liquidity resolves against
/// `all_markets` now, and this pins the difference that caused it.
#[tokio::test]
#[ignore = "network"]
async fn every_exchange_s_pairs_resolve_for_liquidity() {
    use wallet_core::markets::Venue;
    let ctx = mainnet();
    let (main, _) = wallet_core::markets::pools(&ctx).await.unwrap();
    let (all, _) = wallet_core::markets::all_markets(&ctx).await.unwrap();
    assert!(main.iter().all(|p| p.venue == Venue::Main), "the narrow list is Quainance's own");
    let found = |pools: &[wallet_core::markets::Pool], venue: Venue| pools.iter().filter(|p| p.venue == venue).count();
    eprintln!(
        "all_markets: {} main, {} launch AMM, {} QuaiSwap, {} curve",
        found(&all, Venue::Main),
        found(&all, Venue::LaunchAmm),
        found(&all, Venue::Legacy),
        found(&all, Venue::Curve)
    );
    assert!(found(&all, Venue::LaunchAmm) >= 2 && found(&all, Venue::Legacy) == 3);
    // The pair that reported the bug: listed by the screen, and now findable by it.
    let cheez = "0x004301019e1380d9d247dbd47097ec0b98d026b5";
    assert!(!main.iter().any(|p| p.address.eq_ignore_ascii_case(cheez)), "which is why it could not be found");
    let row = all.iter().find(|p| p.address.eq_ignore_ascii_case(cheez)).expect("CHEEZ/QUAI is in the full list");
    assert_eq!(row.venue, Venue::LaunchAmm);
    // Only bonded curves are markets, and none of them is offered as a pool to deposit into.
    assert!(all.iter().filter(|p| p.venue == Venue::Curve).all(|p| p.curve.as_ref().is_some_and(|c| c.progress_bps == Some(10_000))));
}

/// Every pinned exchange fills its own pairs, not merely lists them. A token that only one venue
/// holds forces the route onto that venue's router, and the exact calldata the wallet would sign
/// is simulated there — so `launch_amm` and `legacy_amm` are qualified against the deployed
/// routers rather than against copies of them. Nothing is signed or sent, and the only funds
/// needed are the QUAI the reading account already holds.
#[tokio::test]
#[ignore = "network"]
async fn each_venue_fills_the_pairs_only_it_holds() {
    use wallet_core::markets::Venue;
    use wallet_core::sdk::{BlockTag, QuaiAddress, U256};
    use wallet_core::swap::{ROUTER_ABI, Router, SwapAsset};
    let ctx = mainnet();
    let (all, _) = wallet_core::markets::all_markets(&ctx).await.unwrap();
    let router = Router::open(&ctx.app, &ctx.node, &ctx.network, Trust::Cached).await.unwrap();
    let wquai = ctx.network.wquai.clone().unwrap().to_lowercase();
    let from: QuaiAddress = funded_account(&ctx).await;
    let holder = from.to_string();
    let holder = holder.as_str();
    let amount = U256::from(10u128.pow(17));
    // Tokens the main venue also lists would not prove which router filled them.
    let on_main: std::collections::HashSet<String> = all
        .iter()
        .filter(|p| p.venue == Venue::Main)
        .flat_map(|p| [p.token0.address.to_lowercase(), p.token1.address.to_lowercase()])
        .collect();
    let mut qualified = Vec::new();
    for venue in [Venue::LaunchAmm, Venue::Legacy] {
        let candidates = all.iter().filter(|p| p.venue == venue).filter_map(|p| {
            let (a, b) = (p.token0.address.to_lowercase(), p.token1.address.to_lowercase());
            let other = if a == wquai {
                &p.token1
            } else if b == wquai {
                &p.token0
            } else {
                return None;
            };
            (!on_main.contains(&other.address.to_lowercase())).then_some(other)
        });
        for other in candidates {
            let asset = SwapAsset::Token { address: other.address.clone(), symbol: other.symbol.clone(), decimals: other.decimals };
            let Ok(q) = router.quote(&SwapAsset::Quai, &asset, amount, 500, None).await else { continue };
            assert_eq!(q.legs.len(), 1, "a venue-only token needs no second leg: {:?}", q.route);
            assert_eq!(q.legs[0].venue, venue, "{} routed to {:?}, not {venue:?}", other.symbol, q.legs[0].venue);
            let abi = wallet_core::sdk::abi::AbiInterface::from_human_readable(ROUTER_ABI).unwrap();
            let leg_router: QuaiAddress = q.legs[0].router.parse().unwrap();
            let contract = wallet_core::sdk::contracts::Contract::new(leg_router, abi, &ctx.node.provider);
            let path: Vec<serde_json::Value> = q.path.iter().map(|p| serde_json::json!(p)).collect();
            let deadline = (wallet_core::registry::now() + 600).to_string();
            let args =
                [serde_json::json!(q.minimum_out), serde_json::Value::Array(path), serde_json::json!(holder), serde_json::json!(deadline)];
            let call = contract.prepare("swapExactETHForTokens", &args, amount).unwrap();
            let Ok(out) = contract.simulate(from, &call, BlockTag::Latest, Some(600_000)).await else { continue };
            let amounts = out[0].as_array().unwrap();
            let received = U256::from_str_radix(amounts.last().unwrap().as_str().unwrap(), 10).unwrap();
            assert!(received >= U256::from_str_radix(&q.minimum_out, 10).unwrap(), "{} filled under its minimum", other.symbol);
            eprintln!("VENUE_FILL {venue:?} 0.1 QUAI -> {received} {} atoms via {}", other.symbol, q.legs[0].router);
            qualified.push(venue);
            break;
        }
    }
    assert!(qualified.contains(&Venue::LaunchAmm), "no launch-AMM-only pair filled");
    assert!(qualified.contains(&Venue::Legacy), "no QuaiSwap-only pair filled");

    // cross_venue_plan: a token one venue holds, paid for with a token only another holds, cannot
    // be filled by any single router. The planner has to cross, and the two legs have to agree —
    // the second spends exactly what the first is guaranteed to deliver, not what it hopes to.
    let venue_only = |venue: Venue| {
        all.iter()
            .filter(move |p| p.venue == venue)
            .filter_map(|p| {
                let (a, b) = (p.token0.address.to_lowercase(), p.token1.address.to_lowercase());
                let other = if a == wquai {
                    &p.token1
                } else if b == wquai {
                    &p.token0
                } else {
                    return None;
                };
                (!on_main.contains(&other.address.to_lowercase())).then_some(other)
            })
            .next()
    };
    let (Some(launch), Some(legacy)) = (venue_only(Venue::LaunchAmm), venue_only(Venue::Legacy)) else {
        panic!("both venues had a private pair a moment ago");
    };
    let pay = SwapAsset::Token { address: launch.address.clone(), symbol: launch.symbol.clone(), decimals: launch.decimals };
    let want = SwapAsset::Token { address: legacy.address.clone(), symbol: legacy.symbol.clone(), decimals: legacy.decimals };
    let units = U256::from(10u64).pow(U256::from(u64::from(launch.decimals)));
    let alternatives = router.quote_alternatives(&pay, &want, units, 500, None).await.unwrap();
    let crossing = alternatives
        .quotes
        .iter()
        .find(|q| q.legs.len() > 1)
        .unwrap_or_else(|| panic!("{} -> {} found no crossing route: {:?}", launch.symbol, legacy.symbol, alternatives.omitted));
    let venues: Vec<Venue> = crossing.legs.iter().map(|l| l.venue).collect();
    assert_eq!(venues.len(), 2, "{venues:?}");
    assert_ne!(venues[0], venues[1], "a crossing route uses two exchanges: {venues:?}");
    assert_ne!(crossing.legs[0].router, crossing.legs[1].router, "and two routers");
    // The two legs chain on different quantities, deliberately: the second is *sized* by what the
    // first is expected to deliver, but *guaranteed* from what the first is guaranteed to deliver,
    // so a first leg that fills at its minimum cannot leave the second promising more than it can
    // buy. Both halves of that are checked here.
    let atoms = |s: &str| U256::from_str_radix(s, 10).unwrap();
    assert_eq!(crossing.legs[1].amount_in, crossing.legs[0].amount_out, "the second leg is sized by the first's expectation");
    for leg in &crossing.legs {
        assert!(atoms(&leg.minimum_out) < atoms(&leg.amount_out), "every leg gives up something to slippage: {leg:?}");
        assert!(!atoms(&leg.minimum_out).is_zero(), "and still guarantees something: {leg:?}");
    }
    assert_eq!(crossing.minimum_out, crossing.legs[1].minimum_out, "the route promises exactly what its last leg does");
    eprintln!(
        "CROSS_VENUE {} -> {} over {:?} then {:?}, guarantees {} then {}",
        launch.symbol, legacy.symbol, venues[0], venues[1], crossing.legs[0].minimum_out, crossing.legs[1].minimum_out
    );
}

/// A funded account to simulate from, found on the chain rather than written into this file.
///
/// Several tests need an address holding QUAI to stand as the `from` of a `quai_call`; nothing is
/// ever signed or sent, and the account is never touched. Writing a real one down ties a stranger's
/// wallet — or the author's — to this repository for as long as it exists, so the tape supplies it
/// instead: whoever traded recently has just paid gas, so they hold QUAI and are an
/// externally owned account. Found once per test binary and reused.
async fn funded_account(ctx: &wallet_core::data::DataCtx) -> wallet_core::sdk::QuaiAddress {
    use wallet_core::sdk::{BlockTag, QuaiAddress, U256};
    static FOUND: tokio::sync::OnceCell<String> = tokio::sync::OnceCell::const_new();
    let address = FOUND
        .get_or_init(|| async {
            // From the launch index over HTTP, not `dex_flow`: the node caps concurrent
            // `quai_getLogs`, and a helper several tests call must not spend that budget.
            let trades = wallet_core::launches::curve_trades(ctx, 200).await.expect("recent curve trades");
            let least = U256::from(10u128.pow(18)) / U256::from(2u64);
            let mut traders: Vec<String> = trades.iter().map(|s| s.trader.to_lowercase()).filter(|t| !t.is_empty()).collect();
            traders.sort();
            traders.dedup();
            for trader in traders {
                let Ok(parsed) = trader.parse::<QuaiAddress>() else { continue };
                // An account, not a contract: a router holding QUAI would simulate differently.
                let code = ctx.node.provider.code(parsed, BlockTag::Latest).await.unwrap_or_default();
                if !code.bytes().is_empty() {
                    continue;
                }
                if ctx.node.provider.balance(parsed, BlockTag::Latest).await.unwrap_or(U256::ZERO) >= least {
                    eprintln!("FUNDED_ACCOUNT {trader} (a recent trader, discovered)");
                    return trader;
                }
            }
            panic!("no funded account among recent traders");
        })
        .await;
    address.parse().expect("a discovered account parses")
}

/// Scheduled probes have independent time bounds and machine-readable failure categories.
/// This helper has no wallet registry, signer, or transaction broadcast capability.
async fn live_trading_check<F>(case: &str, check: F)
where
    F: std::future::Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>>,
{
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(60), check).await;
    match outcome {
        Ok(Ok(())) => eprintln!("TRADING_RESULT {}", serde_json::json!({"case":case,"status":"passed","scope":"read_only"})),
        other => {
            let error = match other {
                Ok(Err(error)) => error.to_string(),
                Err(_) => "endpoint_failure: 60s deadline".into(),
                Ok(Ok(())) => unreachable!(),
            };
            let class = if error.starts_with("fixture_drift:") { "fixture_drift" } else { "endpoint_failure" };
            eprintln!("TRADING_RESULT {}", serde_json::json!({"case":case,"status":"failed","class":class,"error":error}));
            panic!("{case}: {class}: {error}");
        }
    }
}
