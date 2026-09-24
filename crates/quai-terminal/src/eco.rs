//! Ecosystem commands: portfolio, prices, token discovery, swaps, NFTs, marketplace, data sources.

use crate::args::*;
use crate::commands::Ctx;
use serde_json::json;
use wallet_core::amount;
use wallet_core::appdb::OpStatus;
use wallet_core::explorer::TokenKind;
use wallet_core::portfolio::{PriceKind, Trust};
use wallet_core::registry::now;
use wallet_core::sdk::U256;
use wallet_core::session::{Session, short_address};
use wallet_core::track::{describe, human_duration};
use wallet_core::{CoreError, Result};

fn age(secs: u64) -> String {
    let a = now().saturating_sub(secs);
    if secs == 0 {
        "—".into()
    } else if a < 60 {
        "now".into()
    } else {
        format!("{} ago", human_duration(a))
    }
}

fn trust_mark(ctx: &Ctx, t: Trust) -> String {
    match t {
        Trust::Verified => ctx.out.green("✓"),
        Trust::Unverified => ctx.out.yellow("⚠"),
        Trust::Unknown => ctx.out.dim("·"),
    }
}

/// All clients use the same durable operation dependencies and next-review builders.
pub async fn plan(ctx: &Ctx, cmd: PlanCmd) -> Result<()> {
    use wallet_core::execution::Coordinator;
    match cmd {
        PlanCmd::List => {
            let s = ctx.session().await?;
            let plans = s.app.trade_plans(&s.network.id)?;
            if ctx.out.json() {
                ctx.out.emit("plans", &plans);
            } else {
                ctx.out.table(
                    &["id", "state", "trade"],
                    &plans.iter().map(|p| vec![p.id.clone(), format!("{:?}", p.state), p.label.clone()]).collect::<Vec<_>>(),
                );
            }
        }
        PlanCmd::Show { id } => {
            let s = ctx.session().await?;
            let plan = s.app.trade_plan(&id)?.ok_or_else(|| CoreError::NotFound("trade plan".into()))?;
            if plan.network != s.network.id {
                return Err(CoreError::Rejected("select the plan's network".into()));
            }
            if ctx.out.json() {
                ctx.out.emit("plan", &plan);
            } else {
                println!(
                    "{} · {:?} · {}\n{}\noperations: {}",
                    plan.id,
                    plan.state,
                    wallet_core::explorer::clean(&plan.label, 120),
                    wallet_core::explorer::clean(&plan.reason, 400),
                    plan.operations.join(", ")
                );
            }
        }
        PlanCmd::Resume { id, discard_unsigned } => {
            let mut s = ctx.unlocked().await?;
            s.track().await?;
            let mut runner = Coordinator::resume(&s, &id)?;
            if discard_unsigned {
                runner.discard_unsigned(&mut s)?;
            }
            continue_plan(ctx, &mut s, &mut runner).await?;
        }
        PlanCmd::Cancel { id } => {
            let mut s = ctx.session().await?;
            let mut runner = Coordinator::resume(&s, &id)?;
            runner.cancel(&mut s)?;
            if ctx.out.json() {
                ctx.out.emit("plan cancelled", &runner.plan);
            } else {
                println!("plan {} cancelled; completed transactions and allowances remain", runner.plan.id);
            }
        }
    }
    Ok(())
}

pub(crate) async fn run_action(
    ctx: &Ctx,
    s: &mut Session,
    label: &str,
    account: Option<&str>,
    max_fee: Option<&str>,
    action: wallet_core::execution::TradingAction,
) -> Result<wallet_core::tx::Submitted> {
    let intent =
        wallet_core::execution::TradingIntent { account: s.account(account)?.address, max_fee: max_fee.map(str::to_owned), action };
    let mut runner = wallet_core::execution::Coordinator::create(s, label, intent)?;
    if ctx.out.json() {
        ctx.out.emit("plan created", &runner.plan);
    } else {
        eprintln!("plan {} · resume with `plan resume {}`", runner.plan.id, runner.plan.id);
    }
    continue_plan(ctx, s, &mut runner).await?.ok_or_else(|| CoreError::Invalid("plan already complete".into()))
}

async fn continue_plan(
    ctx: &Ctx,
    s: &mut Session,
    runner: &mut wallet_core::execution::Coordinator,
) -> Result<Option<wallet_core::tx::Submitted>> {
    let mut last_submission = None;
    loop {
        let Some(review) = runner.prepare(s).await? else {
            if ctx.out.json() {
                ctx.out.emit("plan complete", &runner.plan);
            } else {
                println!("plan {} complete · {}", runner.plan.id, runner.plan.reason);
            }
            return Ok(last_submission);
        };
        let step = wallet_core::flows::is_step(&review).then(|| review.kind.clone());
        let submitted = match ctx.authorize(s, review).await {
            Ok(submitted) => submitted,
            Err(error) => {
                runner.pause(s, &error.to_string())?;
                return Err(error);
            }
        };
        runner.submitted(s)?;
        ctx.print_submitted(step.as_deref().unwrap_or(&runner.plan.label), &submitted);
        let intent: wallet_core::execution::TradingIntent = serde_json::from_value(runner.plan.intent["intent"].clone())?;
        if step.is_none() && !intent.has_more_allocations() {
            return Ok(Some(submitted));
        }
        let operation_id = submitted.op_id.clone();
        last_submission = Some(submitted);
        if let Err(error) = wait_confirmed(ctx, s, &operation_id, 300).await {
            runner.pause(s, &error.to_string())?;
            return Err(error);
        }
    }
}

/// First-mainnet-use disclosure for explorer lookups (printed once to stderr).
pub fn disclose(ctx: &mut Ctx, network_id: &str) {
    if ctx.config.data_disclosure_shown || !ctx.config.explorer_lookups || wallet_core::http::offline() || network_id != "mainnet" {
        return;
    }
    eprintln!(
        "{}",
        ctx.out.dim("note: portfolio data comes from explorer.qu.ai, which can see your addresses and IP. Turn it off with `quai-terminal config set explorer_lookups false`.")
    );
    ctx.config.data_disclosure_shown = true;
    let _ = ctx.config.save(&ctx.paths);
}

// ============================================================== portfolio & prices

pub async fn pnl(ctx: &Ctx, args: PnlArgs) -> Result<()> {
    use wallet_core::pnl::{price_text, quai_text, signed_text, units_text};
    let mut s = ctx.session().await?;
    let pnl = s.pnl().await?;
    if ctx.out.json() {
        ctx.out.emit("pnl", &pnl);
        return Ok(());
    }
    if pnl.fills.is_empty() {
        println!("no trades yet · PnL counts the swaps and curve trades this wallet makes");
        return Ok(());
    }
    let tint = |v: f64, text: String| {
        if v > 0.00005 {
            ctx.out.green(&text)
        } else if v < -0.00005 {
            ctx.out.red(&text)
        } else {
            text
        }
    };
    println!(
        "{} {}  {}",
        ctx.out.bold("pnl"),
        tint(pnl.net, format!("{} QUAI", signed_text(pnl.net))),
        ctx.out.dim(&format!(
            "realized {} · unrealized {} · fees {} · {}",
            signed_text(pnl.realized),
            signed_text(pnl.unrealized),
            quai_text(pnl.fees),
            wallet_core::amount::count(pnl.fills.len(), "trade")
        ))
    );
    let rows = pnl
        .positions
        .iter()
        .map(|p| {
            let held = if p.open > 0.0 { units_text(p.open) } else { "closed".into() };
            vec![
                format!("{}{}", p.symbol, if p.estimated { " ~" } else { "" }),
                held,
                p.avg_cost.map(price_text).unwrap_or_else(|| "—".into()),
                p.mark.map(price_text).unwrap_or_else(|| "—".into()),
                p.value.map(quai_text).unwrap_or_else(|| "—".into()),
                p.unrealized.map(|u| tint(u, signed_text(u))).unwrap_or_else(|| "—".into()),
                tint(p.realized, signed_text(p.realized)),
                p.trades.to_string(),
            ]
        })
        .collect::<Vec<_>>();
    ctx.out.table(&["token", "held", "avg cost", "price", "value", "unrealized", "realized", "trades"], &rows);
    let mut notes = Vec::new();
    if pnl.positions.iter().any(|p| p.estimated) {
        notes.push("~ some figures come from the review; the receipt did not record them".to_string());
    }
    if pnl.unmarked > 0 {
        let (have, they) = if pnl.unmarked == 1 { ("has", "it counts") } else { ("have", "they count") };
        notes.push(format!(
            "{} {have} no WQUAI pool or curve to price {}; {they} at cost",
            wallet_core::amount::count(pnl.unmarked, "open position"),
            if pnl.unmarked == 1 { "it" } else { "them" }
        ));
    }
    for p in &pnl.positions {
        if p.unmatched_sold > 0.0 {
            notes.push(format!(
                "{}: {} sold with no buy recorded here, so no gain is claimed on it",
                p.symbol,
                units_text(p.unmatched_sold)
            ));
        }
        if p.moved_out > 0.0 {
            notes.push(format!("{}: {} left the wallet other than by a trade, removed at cost", p.symbol, units_text(p.moved_out)));
        }
        if p.incomplete_basis {
            notes.push(format!("{}: part of its cost is unknown (bought with a token that had none recorded)", p.symbol));
        }
    }
    for note in notes {
        println!("{}", ctx.out.dim(&note));
    }
    if args.trades {
        println!();
        let rows = pnl
            .fills
            .iter()
            .take(args.limit)
            .map(|f| {
                let when = chrono::DateTime::from_timestamp(f.at as i64, 0)
                    .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_default();
                let legs = f
                    .legs
                    .iter()
                    .map(|l| format!("{}{} {}", if l.units > 0.0 { "+" } else { "" }, units_text(l.units), l.symbol))
                    .collect::<Vec<_>>()
                    .join(" ");
                vec![
                    when,
                    f.side().into(),
                    legs,
                    if f.quai.abs() > 0.0 { signed_text(f.quai) } else { "—".into() },
                    f.tx.as_deref().map(short_address).unwrap_or_default(),
                ]
            })
            .collect::<Vec<_>>();
        ctx.out.table(&["when", "side", "tokens", "QUAI", "tx"], &rows);
    }
    Ok(())
}

pub async fn portfolio(ctx: &mut Ctx, args: PortfolioArgs) -> Result<()> {
    let mut s = ctx.session().await?;
    disclose(ctx, &s.network.id);
    let known = s.portfolio_known(!args.no_refresh).await?;
    let data = s.data_ctx()?;
    let p = wallet_core::portfolio::build(&data, &known).await?;
    if ctx.out.json() {
        ctx.out.emit("portfolio", &p);
        return Ok(());
    }
    let change = p.change_7d.map(|c| format!("  {}{c:.1}% 7d", if c >= 0.0 { "+" } else { "" })).unwrap_or_default();
    println!("{} {}{}", ctx.out.bold("portfolio"), ctx.out.bold(&amount::usd(p.total_usd)), ctx.out.dim(&change));
    let rows = p
        .rows
        .iter()
        .map(|r| {
            vec![
                trust_mark(ctx, r.trust),
                r.symbol.clone(),
                format!(
                    "{}{}",
                    amount::group_thousands(&amount::format_amount_short(r.amount(), r.decimals, 4)),
                    if r.exact { "" } else { " ~" }
                ),
                r.price_usd.map(amount::usd_price).unwrap_or_else(|| "—".into()),
                r.value_usd.map(amount::usd).unwrap_or_else(|| "—".into()),
                if r.value_usd.is_some() { format!("{:.0}%", r.allocation * 100.0) } else { "—".into() },
                match r.price_kind {
                    PriceKind::Market => "market".into(),
                    PriceKind::Protocol => "protocol".into(),
                    PriceKind::None => "unpriced".into(),
                },
            ]
        })
        .collect::<Vec<_>>();
    ctx.out.table(&["", "asset", "balance", "price", "value", "alloc", "src"], &rows);
    if p.nfts.items > 0 {
        println!(
            "{}",
            ctx.out.dim(&format!(
                "NFTs: {} in {} (not in the total) — `quai-terminal nft list`",
                wallet_core::amount::count(p.nfts.items, "item"),
                wallet_core::amount::count(p.nfts.collections, "collection")
            ))
        );
    }
    if let Some(b) = &p.prices {
        println!(
            "{}",
            ctx.out.dim(&format!(
                "prices: QUAI {} ({}) · Qi {} ({}) · {} · {}",
                b.quai_usd.map(amount::usd_price).unwrap_or_else(|| "—".into()),
                b.quai_source,
                b.qi_usd.map(amount::usd_price).unwrap_or_else(|| "—".into()),
                b.qi_source,
                p.sources.join(", "),
                age(b.taken_at)
            ))
        );
    }
    if p.stale {
        println!("{}", ctx.out.yellow("some data is from cache (the source did not answer)"));
    }
    for n in &p.notices {
        println!("{} {n}", ctx.out.yellow("!"));
    }
    if args.history && !p.history.is_empty() {
        let max = p.history.iter().map(|v| v.usd).fold(0.0f64, f64::max).max(1e-9);
        let bars = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
        let line: String = p.history.iter().map(|v| bars[((v.usd / max) * 7.0).round().clamp(0.0, 7.0) as usize]).collect();
        println!("7d {line}  {} → {}", amount::usd(p.history[0].usd), amount::usd(p.history.last().map_or(0.0, |v| v.usd)));
    }
    let _ = &mut s;
    Ok(())
}

pub async fn price(ctx: &mut Ctx, args: PriceArgs) -> Result<()> {
    let s = ctx.session().await?;
    let data = s.data_ctx()?;
    if !data.policy.market {
        return Err(CoreError::Rejected("market data is turned off (`config set market_data true`)".into()));
    }
    let board = data.cached("prices", 60, || data.explorer.prices()).await?;
    let assets = if args.assets.is_empty() { vec!["quai".to_string(), "qi".to_string()] } else { args.assets };
    let markets = data.cached("token_markets", 300, || data.explorer.token_markets()).await.map(|c| c.value).unwrap_or_default();
    let mut out = Vec::new();
    for a in assets {
        let lower = a.to_lowercase();
        let (symbol, usd, source, at) = match lower.as_str() {
            "quai" => ("QUAI".to_string(), board.value.quai_usd, board.value.quai_source.clone(), board.value.taken_at),
            "qi" => ("Qi".to_string(), board.value.qi_usd, board.value.qi_source.clone(), board.value.taken_at),
            _ => {
                let token = s.app.token(&s.network.id, &a).ok();
                let address = token.as_ref().map(|t| t.address.to_lowercase()).unwrap_or(lower.clone());
                match markets.iter().find(|m| m.address == address || (!a.starts_with("0x") && m.symbol.eq_ignore_ascii_case(&a))) {
                    Some(m) => (format!("{} {}", m.symbol, short_address(&m.address)), m.price_usd, m.price_source.clone(), m.price_at),
                    None => match data.explorer.token_quote(&address).await {
                        Ok((usd, source, at)) => (token.map(|t| t.symbol).unwrap_or(a.clone()), Some(usd), source, at),
                        Err(_) => (a.clone(), None, String::new(), 0),
                    },
                }
            }
        };
        out.push(json!({"asset": symbol, "usd": usd, "source": source, "observed_at": at}));
    }
    if ctx.out.json() {
        ctx.out.emit("price", &out);
        return Ok(());
    }
    let rows = out
        .iter()
        .map(|r| {
            vec![
                r["asset"].as_str().unwrap_or("").to_string(),
                r["usd"].as_f64().map(amount::usd_price).unwrap_or_else(|| "—".into()),
                r["source"].as_str().unwrap_or("").to_string(),
                age(r["observed_at"].as_u64().unwrap_or(0)),
            ]
        })
        .collect::<Vec<_>>();
    ctx.out.table(&["asset", "usd", "source", "observed"], &rows);
    Ok(())
}

pub async fn token_discover(ctx: &mut Ctx, import: bool) -> Result<()> {
    let mut s = ctx.session().await?;
    disclose(ctx, &s.network.id);
    let data = s.data_ctx()?;
    if !data.policy.explorer {
        return Err(CoreError::Rejected("explorer lookups are turned off (`config set explorer_lookups true`)".into()));
    }
    let known: std::collections::HashSet<String> =
        s.app.tokens(&s.network.id, true)?.into_iter().map(|t| t.address.to_lowercase()).collect();
    let mut found = std::collections::BTreeMap::new();
    for owner in s.quai_owner_addresses() {
        for h in data.explorer.holdings(&owner).await? {
            if h.kind == TokenKind::Erc20 && !known.contains(&h.token) {
                found.entry(h.token.clone()).or_insert(h);
            }
        }
    }
    let mut imported = Vec::new();
    if import {
        for address in found.keys() {
            match s.import_token(address).await {
                Ok(t) => imported.push(t.symbol),
                Err(e) => eprintln!("{} {address}: {e}", ctx.out.yellow("skipped")),
            }
        }
    }
    if ctx.out.json() {
        ctx.out.emit("token discover", &json!({"found": found.values().collect::<Vec<_>>(), "imported": imported}));
        return Ok(());
    }
    if found.is_empty() {
        println!("no tokens beyond your token list");
        return Ok(());
    }
    let rows = found.values().map(|h| vec![h.symbol.clone(), h.name.clone(), h.token.clone()]).collect::<Vec<_>>();
    ctx.out.table(&["symbol", "name", "contract"], &rows);
    if import {
        println!("{} imported {}", ctx.out.green("✓"), imported.join(", "));
    } else {
        println!("{}", ctx.out.dim("add them with `quai-terminal token discover --import` (token names are untrusted)"));
    }
    Ok(())
}

// ============================================================== swaps

fn print_quote(ctx: &Ctx, q: &wallet_core::swap::SwapQuote) {
    println!("{} {} → {}", ctx.out.bold("swap"), q.pay_text(), ctx.out.bold(&format!("≈ {}", q.receive_text())));
    println!("  route      {}", q.route_text());
    println!("  minimum    {}  (slippage {:.2}%)", q.minimum_text(), f64::from(q.slippage_bps) / 100.0);
    let impact = format!("{:.2}%", q.impact_bps as f64 / 100.0);
    println!("  impact     {}", if q.impact_bps >= wallet_core::swap::IMPACT_WARN_BPS { ctx.out.yellow(&impact) } else { impact });
    println!("  LP fee     {:.1}%", q.fee_bps as f64 / 100.0);
    if let Some(l) = q.liquidity_text() {
        println!("  liquidity  {l}");
    }
    for leg in &q.legs {
        println!("  router     {} {}", leg.router, ctx.out.green(&format!("✓ {}", leg.venue.label())));
    }
    println!(
        "  approval   {}",
        if q.approval_needed { ctx.out.yellow(&format!("needed · exactly {}", q.pay_text())) } else { "not needed".into() }
    );
    for w in &q.warnings {
        println!("  {} {}", ctx.out.yellow("!"), ctx.out.yellow(w));
    }
}

async fn wait_confirmed(ctx: &Ctx, s: &mut Session, op_id: &str, timeout: u64) -> Result<()> {
    let started = std::time::Instant::now();
    if !ctx.out.json() {
        eprintln!("{}", ctx.out.dim("waiting for it to be included…"));
    }
    loop {
        let op = s.app.find_operation(&s.network.id, op_id)?;
        match op.status {
            OpStatus::Settled => return Ok(()),
            OpStatus::Confirmed if op.kind != "wrap_qi" => return Ok(()),
            OpStatus::Failed | OpStatus::Replaced | OpStatus::Cancelled => {
                return Err(CoreError::Execution(format!("{} ended {}", describe(&op), op.status.as_str())));
            }
            _ => {}
        }
        if started.elapsed().as_secs() >= timeout {
            return Err(CoreError::Timeout(format!(
                "{} still {} after {timeout}s; carry on once it confirms",
                describe(&op),
                op.status.as_str()
            )));
        }
        let _ = s.track().await;
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
}

pub async fn swap(ctx: &Ctx, args: SwapArgs) -> Result<()> {
    if let Some(command) = args.cmd {
        match command {
            SwapCmd::Quote { from, to, amount, slippage, account } => {
                let mut s = ctx.session().await?;
                let q = s
                    .swap_quote(
                        account.as_deref(),
                        &from,
                        &to,
                        &amount,
                        slippage.unwrap_or(ctx.config.swap_slippage_bps),
                        wallet_core::data::Trust::Cached,
                    )
                    .await?;
                if ctx.out.json() {
                    ctx.out.emit("swap quote", &q);
                } else {
                    print_quote(ctx, &q);
                }
            }
            SwapCmd::Alternatives { from, to, amount, slippage, account } => {
                let mut s = ctx.session().await?;
                let alternatives =
                    s.swap_alternatives(account.as_deref(), &from, &to, &amount, slippage.unwrap_or(ctx.config.swap_slippage_bps)).await?;
                let gas = s.provider().gas_price(wallet_core::network::ZONE).await?;
                let costs = alternatives
                    .quotes
                    .iter()
                    .map(|quote| {
                        wallet_core::routes::estimate_cost(
                            quote,
                            gas,
                            matches!(quote.to, wallet_core::swap::SwapAsset::Quai).then_some((U256::from(1), U256::from(1))),
                            None,
                        )
                    })
                    .collect::<Result<Vec<_>>>()?;
                if ctx.out.json() {
                    ctx.out.emit("swap alternatives", &serde_json::json!({"alternatives": alternatives, "cost_estimates": costs}));
                } else {
                    for (index, (quote, cost)) in alternatives.quotes.iter().zip(&costs).enumerate() {
                        println!("route {} · {}", index + 1, if cost.sequential { "sequential; separate reviews" } else { "atomic swap" });
                        print_quote(ctx, quote);
                        println!(
                            "estimated gas cost {} QUAI; output-currency cost {}",
                            amount::quai(cost.fee_native_estimate.parse().unwrap_or_default()),
                            cost.fee_output_estimate.as_deref().unwrap_or("unknown")
                        );
                    }
                    for omitted in alternatives.omitted {
                        eprintln!("route unavailable: {omitted}");
                    }
                }
            }
            SwapCmd::Split { from, to, amount, quote, slices, slippage, account, fee } => {
                let mut s = if quote { ctx.session().await? } else { ctx.unlocked().await? };
                let decision = s
                    .swap_split_quote(account.as_deref(), &from, &to, &amount, slippage.unwrap_or(ctx.config.swap_slippage_bps), slices)
                    .await?;
                if ctx.out.json() {
                    ctx.out.emit("split comparison", &decision);
                } else {
                    println!("{}", wallet_core::explorer::clean(&decision.reason, 400));
                    if let Some(plan) = &decision.plan {
                        println!(
                            "{} {} across {} separate transactions; expected {} {}, net after estimated gas {} {}",
                            amount::format_amount(plan.total_input.parse().unwrap_or_default(), plan.from.decimals()),
                            plan.from.symbol(),
                            plan.allocations.len(),
                            amount::format_amount(plan.expected_output.parse().unwrap_or_default(), plan.to.decimals()),
                            plan.to.symbol(),
                            amount::format_amount(plan.net_output_estimate.parse().unwrap_or_default(), plan.to.decimals()),
                            plan.to.symbol()
                        );
                        println!("Each allocation has its own review and minimum; a later failure cannot undo an earlier fill.");
                    }
                }
                if !quote {
                    let plan = decision.plan.ok_or(CoreError::Rejected(decision.reason))?;
                    run_action(
                        ctx,
                        &mut s,
                        "split swap",
                        account.as_deref(),
                        fee.max_fee.as_deref(),
                        wallet_core::execution::TradingAction::Split {
                            plan: Box::new(plan),
                            index: 0,
                            deadline: ctx.config.swap_deadline_minutes,
                        },
                    )
                    .await?;
                }
            }
            SwapCmd::ExactOutput { from, to, output_amount: output, max_input, quote, deadline, account, fee } => {
                if quote {
                    let mut s = ctx.session().await?;
                    let result = s
                        .swap_exact_output_quote(account.as_deref(), &from, &to, &output, &max_input, wallet_core::data::Trust::Cached)
                        .await?;
                    if ctx.out.json() {
                        ctx.out.emit("swap exact-output quote", &result);
                    } else {
                        println!(
                            "receive {} {}; required input {} {}, maximum {} {}",
                            amount::format_amount(result.amount_out.parse().unwrap_or_default(), result.to.decimals()),
                            result.to.symbol(),
                            amount::format_amount(result.required_input.parse().unwrap_or_default(), result.from.decimals()),
                            result.from.symbol(),
                            max_input,
                            result.from.symbol()
                        );
                    }
                } else {
                    let mut s = ctx.unlocked().await?;
                    run_action(
                        ctx,
                        &mut s,
                        "swap exact-output",
                        account.as_deref(),
                        fee.max_fee.as_deref(),
                        wallet_core::execution::TradingAction::ExactOutput {
                            from,
                            to,
                            output,
                            max_input,
                            deadline: deadline.unwrap_or(ctx.config.swap_deadline_minutes),
                        },
                    )
                    .await?;
                }
            }
        }
        return Ok(());
    }
    let (Some(from), Some(to), Some(value)) = (args.from, args.to, args.amount) else {
        return Err(CoreError::Invalid("usage: quai-terminal swap FROM TO --amount N (or `swap quote FROM TO AMOUNT`)".into()));
    };
    let slippage = args.slippage.unwrap_or(ctx.config.swap_slippage_bps);
    let deadline = args.deadline.unwrap_or(ctx.config.swap_deadline_minutes);
    let mut s = ctx.unlocked().await?;
    let quote = s.swap_quote(args.account.as_deref(), &from, &to, &value, slippage, wallet_core::data::Trust::Cached).await?;
    if !ctx.out.json() {
        print_quote(ctx, &quote);
    }
    let account = args.account.as_deref();
    let max_fee = args.fee.max_fee.as_deref();
    if args.min_output.is_some() || args.max_impact_bps.is_some() {
        let bounds = wallet_core::swap::SwapBounds { minimum_output: args.min_output, maximum_impact_bps: args.max_impact_bps };
        run_action(
            ctx,
            &mut s,
            "bounded swap",
            account,
            max_fee,
            wallet_core::execution::TradingAction::BoundedSwap { from, to, amount: value, slippage, deadline, bounds },
        )
        .await?;
        return Ok(());
    }
    let Some((hub, hub_symbol)) = quote.hub() else {
        swap_once(ctx, &mut s, account, (&from, &to), &value, (slippage, deadline), max_fee, None).await?;
        return Ok(());
    };
    run_action(
        ctx,
        &mut s,
        &format!("sequential swap via {hub_symbol}"),
        account,
        max_fee,
        wallet_core::execution::TradingAction::CrossVenue { from, hub, to, amount: value, stage: 0, slippage, deadline },
    )
    .await?;
    Ok(())
}

/// One swap on one exchange: its exact approval first when needed, then the swap itself.
async fn swap_once(
    ctx: &Ctx,
    s: &mut Session,
    account: Option<&str>,
    (from, to): (&str, &str),
    value: &str,
    (slippage, deadline): (u16, u32),
    max_fee: Option<&str>,
    step: Option<(&str, &str)>,
) -> Result<wallet_core::tx::Submitted> {
    // Only a step of a longer trade prints its own quote line; a lone swap printed its quote
    // already, and quoting it again here was a second full round of reads before the review's.
    if let Some((label, _)) = step
        && !ctx.out.json()
    {
        let quote = s.swap_quote(account, from, to, value, slippage, wallet_core::data::Trust::Cached).await?;
        println!("\n{} · {} → ≈ {}", ctx.out.bold(label), quote.pay_text(), quote.receive_text());
    }
    run_action(
        ctx,
        s,
        "swap",
        account,
        max_fee,
        wallet_core::execution::TradingAction::Swap { from: from.into(), to: to.into(), amount: value.into(), slippage, deadline },
    )
    .await
}

// ============================================================== NFTs & market

fn owners(s: &Session, account: Option<&str>) -> Result<Vec<String>> {
    Ok(match account {
        Some(a) => vec![s.account(Some(a))?.address],
        None => s.quai_owner_addresses(),
    })
}

pub async fn nft(ctx: &mut Ctx, cmd: NftCmd) -> Result<()> {
    match cmd {
        NftCmd::List { account, limit } => {
            let s = ctx.session().await?;
            disclose(ctx, &s.network.id);
            let data = s.data_ctx()?;
            let items = wallet_core::market::holdings(&data, &owners(&s, account.as_deref())?, limit, false).await?;
            if ctx.out.json() {
                ctx.out.emit("nft list", &items);
                return Ok(());
            }
            if items.is_empty() {
                println!("no NFTs found for this wallet");
                return Ok(());
            }
            let rows = items
                .iter()
                .map(|n| {
                    vec![
                        ctx.out.green("✓"),
                        n.item.name.clone(),
                        n.item.token_id.clone(),
                        if n.kind == TokenKind::Erc1155 { format!("×{}", n.quantity) } else { String::new() },
                        short_address(&n.item.contract),
                        short_address(&n.owner),
                    ]
                })
                .collect::<Vec<_>>();
            ctx.out.table(&["", "name", "id", "qty", "collection", "account"], &rows);
            println!("{}", ctx.out.dim("✓ ownership re-checked on-chain"));
            Ok(())
        }
        NftCmd::Show { contract, token_id } => {
            let s = ctx.session().await?;
            let data = s.data_ctx()?;
            let item = data.explorer.nft(&contract, &token_id).await?;
            let kind = item.kind.unwrap_or(wallet_core::explorer::TokenKind::Erc721);
            let item = data.with_own_metadata(item, kind).await;
            let owner = data.erc721_owner(&contract, &token_id, wallet_core::data::READ_CALLER).await.ok();
            let listing = wallet_core::market::listings(&data, Some(&contract))
                .await
                .ok()
                .and_then(|l| l.into_iter().find(|x| x.token_id == token_id));
            let link = wallet_core::market::bazarr_url(&s.network, &contract, &token_id);
            if ctx.out.json() {
                ctx.out.emit("nft show", &json!({"item": item, "owner_on_chain": owner, "listing": listing, "bazarr": link}));
                return Ok(());
            }
            println!("{} {}", ctx.out.bold(&item.name), ctx.out.dim(&item.collection));
            println!("  contract   {}", item.contract);
            println!("  token id   {}", item.token_id);
            println!("  owner      {}", owner.clone().unwrap_or_else(|| item.owner.clone().unwrap_or_else(|| "—".into())));
            if !item.description.is_empty() {
                println!("  about      {}", item.description);
            }
            for (k, v) in &item.traits {
                println!("  {:<10} {v}", k.to_lowercase());
            }
            if let Some(l) = listing {
                println!("  listed     {} ({})", l.price_text_on(&s.network), l.protocol);
            }
            if let Some(url) = link {
                println!("  bazarr     {url}");
            }
            Ok(())
        }
        NftCmd::Transfer { contract, token_id, to, quantity, account, fee } => {
            let mut s = ctx.unlocked().await?;
            let review =
                s.review_nft_transfer(account.as_deref(), &contract, &token_id, &to, quantity.as_deref(), fee.max_fee.as_deref()).await?;
            let submitted = ctx.authorize(&mut s, review).await?;
            ctx.print_submitted("nft transfer", &submitted);
            Ok(())
        }
    }
}

pub async fn market(ctx: &mut Ctx, cmd: MarketCmd) -> Result<()> {
    match cmd {
        MarketCmd::Listings { collection, sort, limit } => {
            let s = ctx.session().await?;
            let data = s.data_ctx()?;
            let mut rows = wallet_core::market::listings(&data, collection.as_deref()).await?;
            wallet_core::market::sort_listings(&mut rows, sort.core());
            if ctx.out.json() {
                ctx.out.emit("market listings", &rows.iter().take(limit).collect::<Vec<_>>());
                return Ok(());
            }
            let table = rows
                .iter()
                .take(limit)
                .map(|l| {
                    vec![
                        l.price_text_on(&s.network),
                        l.name.clone().unwrap_or_else(|| format!("#{}", l.token_id)),
                        l.token_id.clone(),
                        short_address(&l.contract),
                        if l.buyable() { "zora".into() } else { ctx.out.dim("seaport · view on Bazarr") },
                        age(l.created_at),
                    ]
                })
                .collect::<Vec<_>>();
            ctx.out.table(&["price", "item", "id", "collection", "market", "listed"], &table);
            println!("{}", ctx.out.dim("listings come from the Bazarr indexer; `market check` re-reads one on-chain"));
            Ok(())
        }
        MarketCmd::Collections { query, limit } => {
            let s = ctx.session().await?;
            let data = s.data_ctx()?;
            let rows = data
                .cached(&format!("collections:{}", query.clone().unwrap_or_default()), 600, || {
                    data.explorer.collections(query.as_deref(), limit)
                })
                .await?
                .value;
            if ctx.out.json() {
                ctx.out.emit("market collections", &rows);
                return Ok(());
            }
            let table = rows
                .iter()
                .map(|c| {
                    vec![
                        c.name.clone(),
                        c.symbol.clone(),
                        c.holders.map(|h| h.to_string()).unwrap_or_else(|| "—".into()),
                        c.floor_quai.map(|f| format!("{f} QUAI")).unwrap_or_else(|| "—".into()),
                        c.address.clone(),
                    ]
                })
                .collect::<Vec<_>>();
            ctx.out.table(&["collection", "symbol", "holders", "floor", "contract"], &table);
            Ok(())
        }
        MarketCmd::Check { contract, token_id } => {
            let s = ctx.session().await?;
            let check = s.check_listing(None, &contract, &token_id).await?;
            if ctx.out.json() {
                ctx.out.emit("market check", &check);
                return Ok(());
            }
            match &check.ask {
                Some(a) => println!(
                    "ask: {} from {}",
                    if a.currency.trim_start_matches("0x").chars().all(|c| c == '0') {
                        format!("{} QUAI", amount::format_amount(U256::from_str_radix(&a.price, 10).unwrap_or_default(), 18))
                    } else {
                        format!("{} of {}", a.price, a.currency)
                    },
                    a.seller
                ),
                None => println!("no active ask"),
            }
            println!("  seller owns item        {}", if check.seller_owns { ctx.out.green("yes") } else { ctx.out.red("no") });
            println!("  seller module approval  {}", if check.seller_module_approved { ctx.out.green("yes") } else { ctx.out.red("no") });
            println!("  seller transfer helper  {}", if check.seller_helper_approved { ctx.out.green("yes") } else { ctx.out.red("no") });
            for p in &check.problems {
                println!("  {} {p}", ctx.out.yellow("!"));
            }
            println!("{}", if check.valid { ctx.out.green("✓ buyable") } else { ctx.out.red("× not buyable") });
            Ok(())
        }
        MarketCmd::Sell { contract, token_id, price, currency, account, fee } => {
            let mut s = ctx.unlocked().await?;
            let state = s.seller_state(account.as_deref(), &contract, &token_id).await?;
            if !state.owns {
                return Err(CoreError::Rejected(format!("this account does not own the item (owner {})", state.owner)));
            }
            if !state.module_approved {
                println!("{}", ctx.out.bold("step · approve marketplace module (once)"));
                let review = s.review_zora_module_approval(account.as_deref(), fee.max_fee.as_deref()).await?;
                let submitted = ctx.authorize(&mut s, review).await?;
                ctx.print_submitted("market approve module", &submitted);
                wait_confirmed(ctx, &mut s, &submitted.op_id, 300).await?;
            }
            if !state.helper_approved {
                println!("{}", ctx.out.bold("step · approve the marketplace for this collection (once)"));
                let review = s.review_zora_collection_approval(account.as_deref(), &contract, fee.max_fee.as_deref()).await?;
                let submitted = ctx.authorize(&mut s, review).await?;
                ctx.print_submitted("market approve collection", &submitted);
                wait_confirmed(ctx, &mut s, &submitted.op_id, 300).await?;
            }
            let review = s.review_nft_list(account.as_deref(), &contract, &token_id, &price, &currency, fee.max_fee.as_deref()).await?;
            let submitted = ctx.authorize(&mut s, review).await?;
            ctx.print_submitted("market sell", &submitted);
            if let Some(url) = wallet_core::market::bazarr_url(&s.network, &contract, &token_id)
                && !ctx.out.json()
            {
                println!("{} {url}", ctx.out.dim("Bazarr page (shows the listing within a minute):"));
            }
            Ok(())
        }
        MarketCmd::Unlist { contract, token_id, account, fee } => {
            let mut s = ctx.unlocked().await?;
            let review = s.review_nft_unlist(account.as_deref(), &contract, &token_id, fee.max_fee.as_deref()).await?;
            let submitted = ctx.authorize(&mut s, review).await?;
            ctx.print_submitted("market unlist", &submitted);
            Ok(())
        }
        MarketCmd::Mine { account } => {
            let s = ctx.session().await?;
            let data = s.data_ctx()?;
            let rows = wallet_core::market::listings_by(&data, &owners(&s, account.as_deref())?).await?;
            if ctx.out.json() {
                ctx.out.emit("market mine", &rows);
                return Ok(());
            }
            if rows.is_empty() {
                println!("nothing listed · `quai-terminal market sell CONTRACT ID --price N`");
                return Ok(());
            }
            let table = rows
                .iter()
                .map(|l| {
                    vec![
                        l.price_text_on(&s.network),
                        l.name.clone().unwrap_or_else(|| format!("#{}", l.token_id)),
                        l.token_id.clone(),
                        short_address(&l.contract),
                        if l.buyable() { "zora".into() } else { ctx.out.dim("seaport") },
                        age(l.created_at),
                    ]
                })
                .collect::<Vec<_>>();
            ctx.out.table(&["price", "item", "id", "collection", "market", "listed"], &table);
            println!(
                "{}",
                ctx.out.dim("`market sell … --price N` changes a price · `market unlist` cancels · sales notify (daemon or TUI)")
            );
            Ok(())
        }
        MarketCmd::Buy { contract, token_id, account, fee } => {
            let mut s = ctx.unlocked().await?;
            let check = s.check_listing(account.as_deref(), &contract, &token_id).await?;
            if !check.valid {
                if let Some(url) = wallet_core::market::bazarr_url(&s.network, &contract, &token_id) {
                    eprintln!("{} {url}", ctx.out.dim("listing page:"));
                }
                return Err(CoreError::Rejected(format!("cannot buy: {}", check.problems.join("; "))));
            }
            let expected = check.ask.as_ref().map(|a| a.price.clone());
            if check.buyer_module_approval_needed {
                println!("{}", ctx.out.bold("step · approve marketplace module (once)"));
                let review = s.review_zora_module_approval(account.as_deref(), fee.max_fee.as_deref()).await?;
                let submitted = ctx.authorize(&mut s, review).await?;
                ctx.print_submitted("market approve module", &submitted);
                wait_confirmed(ctx, &mut s, &submitted.op_id, 300).await?;
            }
            if check.buyer_token_approval_needed {
                println!("{}", ctx.out.bold("step · approve payment token"));
                let review = s.review_zora_token_approval(account.as_deref(), &contract, &token_id, fee.max_fee.as_deref()).await?;
                let submitted = ctx.authorize(&mut s, review).await?;
                ctx.print_submitted("market approve token", &submitted);
                wait_confirmed(ctx, &mut s, &submitted.op_id, 300).await?;
            }
            let review = s.review_nft_buy(account.as_deref(), &contract, &token_id, expected.as_deref(), fee.max_fee.as_deref()).await?;
            let submitted = ctx.authorize(&mut s, review).await?;
            ctx.print_submitted("market buy", &submitted);
            Ok(())
        }
    }
}

// ============================================================== data sources

pub async fn data(ctx: &Ctx, cmd: DataCmd) -> Result<()> {
    let network = ctx.network()?;
    let policy = ctx.config.data_policy();
    let explorer = wallet_core::explorer::Explorer::for_network(&network);
    match cmd {
        DataCmd::Status => {
            let value = json!({
                "network": network.id,
                "offline_data": wallet_core::http::offline(),
                "explorer_lookups": policy.explorer,
                "market_data": policy.market,
                "images": policy.images,
                "token_icons": policy.icons,
                "backend": format!("{:?}", explorer.backend),
                "explorer": explorer.base,
                "swaps": network.ecosystem.quainance_router.as_ref().map(|r| r.address.clone()),
                "marketplace": network.ecosystem.bazarr_indexer,
                "ipfs_gateway": wallet_core::ipfs::gateway(wallet_core::ipfs::Content::Media).display(),
                "abi_ipfs_gateway": wallet_core::ipfs::gateway(wallet_core::ipfs::Content::Abi).display(),
                "budgets": wallet_core::http::host_statuses(),
            });
            if ctx.out.json() {
                ctx.out.emit("data status", &value);
                return Ok(());
            }
            let onoff = |b: bool| if b { ctx.out.green("on") } else { ctx.out.dim("off") };
            println!("{} on {}", ctx.out.bold("data sources"), network.name);
            println!("  explorer lookups  {}  (holdings, NFTs, history — sees your addresses)", onoff(policy.explorer));
            println!("  market data       {}  (prices, collections, listings)", onoff(policy.market));
            println!("  NFT images        {}", onoff(policy.images));
            println!("  token icons       {}", onoff(policy.icons));
            println!("  backend           {:?} {}", explorer.backend, explorer.base);
            println!(
                "  swaps             {}",
                network.ecosystem.quainance_router.as_ref().map(|r| format!("Quainance {}", r.address)).unwrap_or_else(|| "—".into())
            );
            println!("  marketplace       {}", network.ecosystem.bazarr_indexer.clone().unwrap_or_else(|| "—".into()));
            for content in [wallet_core::ipfs::Content::Abi, wallet_core::ipfs::Content::Media] {
                let gateway = wallet_core::ipfs::gateway(content);
                println!(
                    "  IPFS {:<13} {}{}",
                    content.label(),
                    gateway.display(),
                    if gateway.is_default_for(content) {
                        format!("  (`config set {} http://127.0.0.1:8080` for your own node)", content.config_key())
                    } else {
                        String::new()
                    }
                );
            }
            if wallet_core::http::offline() {
                println!("  {}", ctx.out.yellow("--offline-data: every lookup is disabled for this command"));
            }
            Ok(())
        }
        DataCmd::Test => {
            let mut results = Vec::new();
            let started = std::time::Instant::now();
            match explorer.backend {
                wallet_core::explorer::Backend::Quai => {
                    let r = explorer.prices().await.map(|p| format!("QUAI {}", p.quai_usd.map(amount::usd_price).unwrap_or_default()));
                    results.push(("explorer".to_string(), r, started.elapsed().as_millis()));
                }
                wallet_core::explorer::Backend::Blockscout => {
                    let r = explorer.token_info(network.wquai.as_deref().unwrap_or_default()).await.map(|t| format!("token {}", t.symbol));
                    results.push(("explorer".to_string(), r, started.elapsed().as_millis()));
                }
                wallet_core::explorer::Backend::ChainOnly => {}
            }
            if let Some(base) = &network.ecosystem.bazarr_indexer {
                let t = std::time::Instant::now();
                let r = wallet_core::http::get_json(&format!("{}/listings", base.trim_end_matches('/')))
                    .await
                    .map(|v| wallet_core::amount::count(wallet_core::market::parse_listings(&v).len(), "listing"));
                results.push(("marketplace indexer".to_string(), r, t.elapsed().as_millis()));
            }
            let t = std::time::Instant::now();
            let node = network.node()?;
            let r = wallet_core::network::check_node(&network, &node).await.map(|h| format!("block {}", h.height));
            results.push(("node".to_string(), r, t.elapsed().as_millis()));
            let t = std::time::Instant::now();
            for (ipfs_label, r) in wallet_core::ipfs::test_lines().await {
                results.push((ipfs_label, r, t.elapsed().as_millis()));
            }
            if ctx.out.json() {
                let v: Vec<_> = results.iter().map(|(n, r, ms)| json!({"service": n, "ok": r.is_ok(), "detail": r.as_ref().map_or_else(|e| e.to_string(), String::clone), "ms": ms})).collect();
                ctx.out.emit("data test", &v);
                return Ok(());
            }
            for (name, r, ms) in &results {
                match r {
                    Ok(detail) => println!("{} {name:<20} {detail} · {ms} ms", ctx.out.green("✓")),
                    Err(e) => println!("{} {name:<20} {e}", ctx.out.red("×")),
                }
            }
            if results.iter().any(|(_, r, _)| r.is_err()) {
                return Err(CoreError::Network("some data sources did not answer".into()));
            }
            Ok(())
        }
    }
}

// ============================================================== DEX markets

/// Price with significant digits: `118.5782`, `0.009102`, `1.639e-6`.
fn price_text(p: f64) -> String {
    if !p.is_finite() || p <= 0.0 {
        "—".into()
    } else if p >= 1.0 {
        format!("{p:.4}")
    } else if p >= 0.001 {
        let digits = (-p.log10()).ceil() as usize + 3;
        format!("{p:.digits$}")
    } else {
        amount::subscript_zeros(p, 4).unwrap_or_else(|| format!("{p:.3e}"))
    }
}

pub async fn markets(ctx: &Ctx, args: MarketsArgs) -> Result<()> {
    use wallet_core::markets::{TIMEFRAMES, Venue, all_markets, base_is_token0, pair_stats, pool_events, trades};
    let s = ctx.session().await?;
    let data = s.data_ctx()?;
    let (pools, overview) = all_markets(&data).await?;
    let network = &s.network;
    let usdt = network.ecosystem.usdt.as_ref().map(|u| u.address.to_lowercase());
    let wquai = network.wquai.as_ref().map(|w| w.to_lowercase());
    let wqi = network.wqi.as_ref().map(|w| w.to_lowercase());
    let sym = |t: &wallet_core::markets::PoolToken| {
        if wquai.as_deref() == Some(t.address.as_str()) { "QUAI".to_string() } else { t.symbol.clone() }
    };
    let orient = |p: &wallet_core::markets::Pool| base_is_token0(p, usdt.as_deref(), wquai.as_deref(), wqi.as_deref());
    let name = |p: &wallet_core::markets::Pool| {
        let b0 = orient(p);
        let (b, q) = if b0 { (&p.token0, &p.token1) } else { (&p.token1, &p.token0) };
        format!("{}/{}", sym(b), sym(q))
    };
    let Some(wanted) = args.pair else {
        if ctx.out.json() {
            ctx.out.emit("markets", &json!({"overview": overview, "pools": pools}));
            return Ok(());
        }
        let rows: Vec<Vec<String>> = pools
            .iter()
            .map(|p| {
                // Price in the orientation the pair is named: token1 per token0, inverted when token1 is the base.
                let price = p.spot_price().map(|v| if orient(p) { v } else { 1.0 / v });
                let venue = match (p.venue, &p.curve) {
                    // A bonded curve trades against its own locked pool; its depth is that pool.
                    (Venue::Curve, Some(c)) if c.locked_quai.is_some() => "curve, locked".to_string(),
                    (Venue::Curve, Some(c)) => format!("curve {}%", c.progress_bps.unwrap_or(0) / 100),
                    (v, _) => v.label().to_string(),
                };
                vec![
                    name(p),
                    price.map(price_text).unwrap_or_else(|| "—".into()),
                    p.tvl_usd.map(wallet_core::swap::usd_compact).unwrap_or_else(|| "—".into()),
                    p.volume_24h_usd.map(wallet_core::swap::usd_compact).unwrap_or_else(|| "—".into()),
                    venue,
                    short_address(&p.address),
                ]
            })
            .collect();
        ctx.out.table(&["pair", "price", "TVL", "24h vol", "venue", "market"], &rows);
        if let (Some(tvl), Some(vol)) = (overview.tvl_usd, overview.volume_24h_usd) {
            println!(
                "{}",
                ctx.out.dim(&format!("Quainance TVL {} · 24h volume {} · source {}", amount::usd(tvl), amount::usd(vol), overview.source))
            );
        }
        // A list a ceiling shortened says so rather than reading as the whole market.
        for omitted in &overview.omitted {
            println!("⚠ {} — this list is partial", omitted.text());
        }
        return Ok(());
    };
    let w = wanted.trim().to_lowercase();
    let pool = pools
        .iter()
        .find(|p| {
            p.address == w || name(p).to_lowercase() == w || {
                let parts: Vec<&str> = w.split('/').collect();
                parts.len() == 2 && name(p).to_lowercase() == format!("{}/{}", parts[1], parts[0])
            }
        })
        .ok_or_else(|| CoreError::NotFound(format!("no pool matches `{wanted}` (run `markets` to list pairs)")))?;
    // `B/A` for a pool listed as `A/B` flips the orientation.
    let listed = name(pool).to_lowercase();
    let base0 = if listed == w || pool.address == w { orient(pool) } else { !orient(pool) };
    let (base, quote) = if base0 { (&pool.token0, &pool.token1) } else { (&pool.token1, &pool.token0) };
    let bucket = TIMEFRAMES
        .iter()
        .find(|(l, _)| *l == args.timeframe)
        .map(|(_, b)| *b)
        .ok_or_else(|| CoreError::Invalid("timeframe: 15m, 1h, 4h or 1d".into()))?;
    let now = now();
    let since = now.saturating_sub((bucket * 49).max(86_400)).max(now.saturating_sub(30 * 86_400));
    let events = pool_events(&data, pool, since, 10).await?;
    let stats = pair_stats(&events, pool, base0, now);
    let offset = i64::from(chrono::Local::now().offset().local_minus_utc());
    let cs = wallet_core::markets::candles_in_zone(&events, pool, base0, bucket, offset, now, 48);
    let tr = trades(&events, pool, base0);
    if ctx.out.json() {
        ctx.out.emit("markets", &json!({"pool": pool, "base": sym(base), "quote": sym(quote), "stats": stats, "candles": cs, "trades": tr.iter().take(args.trades).collect::<Vec<_>>()}));
        return Ok(());
    }
    let (bs, qs) = (sym(base), sym(quote));
    let price = stats.price.map(price_text).unwrap_or_else(|| "—".into());
    println!(
        "{} {} {qs}  {}",
        ctx.out.bold(&format!("{bs}/{qs}")),
        ctx.out.bold(&price),
        stats.change_24h.map(|c| format!("{c:+.2}% 24h")).unwrap_or_default()
    );
    if let (Some(h), Some(l)) = (stats.high_24h, stats.low_24h) {
        println!("  24h high   {}  low {}", price_text(h), price_text(l));
    }
    println!("  24h volume {:.4} {qs} · {}", stats.volume_24h, wallet_core::amount::count(stats.trades_24h, "trade"));
    println!("  TVL        {}", pool.tvl_usd.map(amount::usd).unwrap_or_else(|| "—".into()));
    println!("  pool       {}", pool.address);
    let closes: Vec<f64> = cs.iter().map(|c| c.close).collect();
    if !closes.is_empty() {
        let (lo, hi) = closes.iter().fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
        let bars = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
        let line: String =
            closes.iter().map(|v| if hi - lo < 1e-18 { bars[3] } else { bars[(((v - lo) / (hi - lo)) * 7.0).round() as usize] }).collect();
        println!("  {} closes  {line}", args.timeframe);
    }
    let rows: Vec<Vec<String>> = tr
        .iter()
        .take(args.trades)
        .map(|t| {
            vec![
                age(t.at),
                if t.buy { ctx.out.green("buy") } else { ctx.out.yellow("sell") },
                price_text(t.price),
                format!("{:.4} {bs}", t.base),
                format!("{:.4} {qs}", t.quote),
                short_address(&t.trader),
            ]
        })
        .collect();
    if !rows.is_empty() {
        println!();
        ctx.out.table(&["when", "side", "price", "size", "total", "trader"], &rows);
    }
    println!("{}", ctx.out.dim(&format!("prices from pool reserves; history from {}", overview.source)));
    Ok(())
}

/// The on-chain message board. Reading is a log query and needs no keys.
pub async fn board(ctx: &Ctx, cmd: BoardCmd) -> Result<()> {
    use wallet_core::messages::{KIND_TEXT, channel, channel_tag};
    match cmd {
        BoardCmd::Read { channel: name, blocks } => {
            let s = ctx.session().await?;
            let data = s.data_ctx()?;
            let tag = channel_tag(&name)?;
            let mut posts = channel(&data, &tag, blocks).await?;
            // The tape reads newest first; a conversation reads the other way round.
            posts.reverse();
            if ctx.out.json() {
                ctx.out.emit("board", &json!({"channel": name, "posts": posts}));
                return Ok(());
            }
            if posts.is_empty() {
                println!("no messages in #{name} over the last {}", wallet_core::amount::count(blocks, "block"));
                return Ok(());
            }
            let rows: Vec<Vec<String>> = posts
                .iter()
                .map(|p| {
                    // A body is whatever a stranger wrote: shown only when it is really text.
                    let body = match (p.kind, p.text()) {
                        (KIND_TEXT, Some(text)) => text,
                        (KIND_TEXT, None) => "<not text>".into(),
                        _ => format!("<sealed, {} bytes>", p.body.len()),
                    };
                    vec![human_duration(now().saturating_sub(p.at)), short_address(&p.from), body]
                })
                .collect();
            ctx.out.table(&["age", "from", "message"], &rows);
            println!("{} in #{name}", wallet_core::amount::count(posts.len(), "message"));
            Ok(())
        }
        BoardCmd::Subscribe { chat, dm } => {
            let s = ctx.session().await?;
            let target = chat_target(&s, &chat, dm)?;
            let on = s.toggle_chat_subscription(&target)?;
            let label = if on { format!("notifying you about {target}") } else { format!("no more notifications from {target}") };
            println!("{} {label}", ctx.out.green("✓"));
            Ok(())
        }
        BoardCmd::Subscriptions => {
            let s = ctx.session().await?;
            let subs = s.chat_subscriptions();
            if ctx.out.json() {
                ctx.out.emit("board subscriptions", &subs);
            } else if subs.is_empty() {
                println!("no subscriptions · quai-terminal board subscribe general");
            } else {
                for sub in subs {
                    println!("{sub}");
                }
            }
            Ok(())
        }
        BoardCmd::Pin { chat, dm, clear } => {
            let s = ctx.session().await?;
            let target = match (clear, chat) {
                (true, _) => None,
                (false, Some(chat)) => Some(chat_target(&s, &chat, dm)?),
                (false, None) => return Err(wallet_core::CoreError::Invalid("name a chat, or --clear".into())),
            };
            s.set_chat_pin(target.as_deref())?;
            println!("{} {}", ctx.out.green("✓"), target.map_or("unpinned".to_string(), |t| format!("{t} pinned")));
            Ok(())
        }
        BoardCmd::News => {
            let s = ctx.session().await?;
            let news = s.chat_news().await?;
            if ctx.out.json() {
                ctx.out
                    .emit("board news", &news.iter().map(|n| serde_json::json!({"chat": n.title, "messages": n.body})).collect::<Vec<_>>());
            } else if news.is_empty() {
                println!("nothing new");
            } else {
                for n in news {
                    println!("{} {} · {}", ctx.out.green("●"), n.title, n.body);
                }
            }
            Ok(())
        }
        BoardCmd::Channels { blocks } => {
            let s = ctx.session().await?;
            let data = s.data_ctx()?;
            let found = wallet_core::messages::channels(&data, blocks).await?;
            if ctx.out.json() {
                ctx.out.emit("board channels", &json!({"channels": found}));
                return Ok(());
            }
            if found.is_empty() {
                println!("no channel has a message in the last {}", wallet_core::amount::count(blocks, "block"));
                return Ok(());
            }
            let rows: Vec<Vec<String>> = found
                .iter()
                .map(|c| {
                    let last = if c.last_at > 0 { human_duration(now().saturating_sub(c.last_at)) } else { "—".into() };
                    vec![format!("#{}", c.name), c.messages.to_string(), last]
                })
                .collect();
            ctx.out.table(&["channel", "messages", "last"], &rows);
            Ok(())
        }
        BoardCmd::Dm { peer, text, from, fee } => {
            let mut s = ctx.unlocked().await?;
            let review = s.review_dm(from.as_deref(), &peer, &text, fee.max_fee.as_deref()).await?;
            let submitted = ctx.authorize(&mut s, review).await?;
            ctx.print_submitted("board dm", &submitted);
            Ok(())
        }
        BoardCmd::Inbox { peer, blocks } => {
            let s = ctx.unlocked().await?;
            let lines = s.read_conversation(&peer, blocks).await?;
            if ctx.out.json() {
                ctx.out.emit("board inbox", &json!({"peer": peer, "messages": lines}));
                return Ok(());
            }
            if lines.is_empty() {
                println!("no sealed messages with {} over the last {}", short_address(&peer), wallet_core::amount::count(blocks, "block"));
                return Ok(());
            }
            let rows: Vec<Vec<String>> = lines
                .iter()
                .map(|l| {
                    let text = l.text.clone().unwrap_or_else(|| "<cannot read this>".into());
                    let who = if l.mine { "you".to_string() } else { short_address(&l.from) };
                    vec![human_duration(now().saturating_sub(l.at)), who, text]
                })
                .collect();
            ctx.out.table(&["age", "from", "message"], &rows);
            println!("{} · sealed: the text is private, the transactions are not", wallet_core::amount::count(rows.len(), "message"));
            Ok(())
        }
        BoardCmd::Post { channel: name, text, from, fee } => {
            let mut s = ctx.unlocked().await?;
            let review = s.review_post(from.as_deref(), &name, &text, fee.max_fee.as_deref()).await?;
            let submitted = ctx.authorize(&mut s, review).await?;
            ctx.print_submitted("board post", &submitted);
            Ok(())
        }
    }
}

// ============================================================== liquidity & the gauge

/// Resolve `WQI/WQUAI` or a pool address to a pair contract. Symbols are matched against the pool
/// list in either order, so `WQUAI/WQI` finds the same pool as `WQI/WQUAI`.
async fn resolve_pair(ctx: &Ctx, s: &mut Session, text: &str) -> Result<String> {
    let t = text.trim();
    if t.starts_with("0x") {
        return t
            .parse::<wallet_core::sdk::QuaiAddress>()
            .map(|address| address.to_string().to_lowercase())
            .map_err(|_| CoreError::Invalid("invalid pair address".into()));
    }
    let data = s.data_ctx()?;
    // Every exchange that mints LP, so a launch-AMM or QuaiSwap pair resolves like a Quainance one.
    let (all, _) = wallet_core::markets::all_markets(&data).await?;
    let pools: Vec<wallet_core::markets::Pool> = all.into_iter().filter(|p| p.venue != wallet_core::markets::Venue::Curve).collect();
    let (a, b) = t.split_once('/').ok_or_else(|| CoreError::Invalid("pair looks like BASE/QUOTE, e.g. WQI/WQUAI".into()))?;
    let matches: Vec<&wallet_core::markets::Pool> = pools
        .iter()
        .filter(|p| {
            let (s0, s1) = (p.token0.symbol.as_str(), p.token1.symbol.as_str());
            (s0.eq_ignore_ascii_case(a) && s1.eq_ignore_ascii_case(b)) || (s0.eq_ignore_ascii_case(b) && s1.eq_ignore_ascii_case(a))
        })
        .collect();
    match matches.as_slice() {
        [one] => Ok(one.address.clone()),
        [] => Err(CoreError::NotFound(format!("no Quainance pool for {t}"))),
        // Symbols are not unique on a permissionless DEX; make the user pick rather than guess.
        many => {
            let list: Vec<String> =
                many.iter().map(|p| format!("{} ({})", p.address, wallet_core::swap::usd_compact(p.tvl_usd.unwrap_or(0.0)))).collect();
            let _ = ctx;
            Err(CoreError::Invalid(format!("{} pools match {t}; name one by address: {}", many.len(), list.join(", "))))
        }
    }
}

/// `pool` subcommands.
pub async fn pool(ctx: &Ctx, cmd: PoolCmd) -> Result<()> {
    match cmd {
        PoolCmd::Curve { token } => {
            let s = ctx.session().await?;
            let data = s.data_ctx()?;
            let launch = launch_by(&data, &token).await?;
            let curve = launch.curve.clone().ok_or_else(|| CoreError::NotFound(format!("{} has no bonding curve", launch.symbol)))?;
            let m = wallet_core::curve::market(&data, &launch.token, &curve, &s.quai_owner_addresses()).await?;
            if ctx.out.json() {
                ctx.out.emit("curve", &m);
                return Ok(());
            }
            println!("{} {}  {}", ctx.out.bold(&launch.symbol), launch.name, ctx.out.dim(&launch.token));
            println!("raised   {:.2} / {:.0} QUAI ({:.2}% to graduation)", m.raised_quai(), m.target_quai(), m.progress_bps as f64 / 100.0);
            println!("sold     {:.2}% of the curve's tokens · fee {:.2}%", m.sold_bps() as f64 / 100.0, m.fee_bps as f64 / 100.0);
            println!("price    {:.10} QUAI (at graduation {:.10})", m.spot_price, m.points.last().map_or(0.0, |p| p.1));
            println!("curve    {curve}");
            if !m.held.is_zero() {
                println!("yours    {} {}", amount::format_amount_short(m.held, 18, 4), launch.symbol);
            }
            if !m.claimable.is_zero() {
                println!(
                    "credit   {} QUAI to claim (`pool curve-claim {}`)",
                    amount::format_amount_short(m.claimable, 18, 6),
                    launch.symbol
                );
            }
            Ok(())
        }
        PoolCmd::CurveBuy { token, amount: value, slippage, account, fee } => {
            let slippage = slippage.unwrap_or(ctx.config.swap_slippage_bps);
            let mut s = ctx.unlocked().await?;
            let launch = launch_by(&s.data_ctx()?, &token).await?;
            let curve = launch.curve.clone().ok_or_else(|| CoreError::NotFound(format!("{} has no bonding curve", launch.symbol)))?;
            let deadline = ctx.config.swap_deadline_minutes;
            let review = s
                .review_curve_buy(
                    account.as_deref(),
                    &launch.token,
                    &launch.symbol,
                    &curve,
                    &value,
                    slippage,
                    deadline,
                    fee.max_fee.as_deref(),
                )
                .await?;
            let submitted = ctx.authorize(&mut s, review).await?;
            ctx.print_submitted("curve buy", &submitted);
            Ok(())
        }
        PoolCmd::CurveQuote { token, amount, sell, account, slippage } => {
            let s = ctx.session().await?;
            let launch = launch_by(&s.data_ctx()?, &token).await?;
            let curve = launch.curve.ok_or_else(|| CoreError::NotFound("token has no curve".into()))?;
            let quote = s
                .curve_trade_quote(
                    account.as_deref(),
                    &launch.token,
                    &curve,
                    &amount,
                    sell,
                    slippage.unwrap_or(ctx.config.swap_slippage_bps),
                )
                .await?;
            if ctx.out.json() {
                ctx.out.emit("curve quote", &quote);
            } else {
                let (pay, receive, pd, rd) = if sell {
                    (quote.token.symbol.as_str(), "QUAI", quote.token.decimals, 18)
                } else {
                    ("QUAI", quote.token.symbol.as_str(), 18, quote.token.decimals)
                };
                println!(
                    "pay {} {}; expected {} {}, minimum {} {}",
                    amount::format_amount(quote.input.parse().unwrap_or_default(), pd),
                    pay,
                    amount::format_amount(quote.expected_output.parse().unwrap_or_default(), rd),
                    receive,
                    amount::format_amount(quote.minimum_output.parse().unwrap_or_default(), rd),
                    receive
                );
                println!(
                    "curve fee at most {} QUAI; {}",
                    amount::quai(quote.maximum_fee.parse().unwrap_or_default()),
                    if quote.immediate_native_payment {
                        "Hartii refunds/proceeds pay directly; no on-chain deadline"
                    } else {
                        "Quainance QUAI credits require a separate claim"
                    }
                );
            }
            Ok(())
        }
        PoolCmd::CurveSell { token, amount: value, slippage, account, fee } => {
            let slippage = slippage.unwrap_or(ctx.config.swap_slippage_bps);
            let mut s = ctx.unlocked().await?;
            let launch = launch_by(&s.data_ctx()?, &token).await?;
            let curve = launch.curve.clone().ok_or_else(|| CoreError::NotFound(format!("{} has no bonding curve", launch.symbol)))?;
            let deadline = ctx.config.swap_deadline_minutes;
            run_action(
                ctx,
                &mut s,
                "curve sale",
                account.as_deref(),
                fee.max_fee.as_deref(),
                wallet_core::execution::TradingAction::CurveSell {
                    token: launch.token,
                    symbol: launch.symbol,
                    curve,
                    amount: value,
                    slippage,
                    deadline,
                },
            )
            .await
            .map(|_| ())
        }
        PoolCmd::CurveClaim { token, account, fee } => {
            let mut s = ctx.unlocked().await?;
            let launch = launch_by(&s.data_ctx()?, &token).await?;
            let curve = launch.curve.clone().ok_or_else(|| CoreError::NotFound(format!("{} has no bonding curve", launch.symbol)))?;
            let review = s.review_curve_claim(account.as_deref(), &launch.token, &launch.symbol, &curve, fee.max_fee.as_deref()).await?;
            let submitted = ctx.authorize(&mut s, review).await?;
            ctx.print_submitted("curve claim", &submitted);
            Ok(())
        }
        PoolCmd::Launches => {
            let s = ctx.session().await?;
            let data = s.data_ctx()?;
            let launches = wallet_core::launches::launches(&data, 200).await?;
            if ctx.out.json() {
                ctx.out.emit("launches", &launches);
                return Ok(());
            }
            let rows = launches
                .iter()
                .map(|l| {
                    vec![
                        l.symbol.clone(),
                        match (l.phase, l.progress_bps) {
                            (wallet_core::launches::Phase::Bonding, Some(bps)) => format!("bonding {}%", bps / 100),
                            (phase, _) => phase.text().to_string(),
                        },
                        l.price_quai.map(|p| format!("{p:.10}")).unwrap_or_else(|| "—".into()),
                        l.trades().to_string(),
                        l.token.clone(),
                    ]
                })
                .collect::<Vec<_>>();
            ctx.out.table(&["token", "stage", "price QUAI", "trades", "contract"], &rows);
            Ok(())
        }
        PoolCmd::List => {
            let s = ctx.session().await?;
            let data = s.data_ctx()?;
            let owners = s.quai_owner_addresses();
            let (pools, _) = wallet_core::markets::pools(&data).await?;
            let gauge = wallet_core::gauge::open(&data, &owners).await.ok();
            let zone = wallet_core::zone::open(&data, &owners).await.ok();
            let positions = wallet_core::liquidity::positions(&data, &owners, &pools, gauge.as_ref(), zone.as_ref()).await;
            if ctx.out.json() {
                ctx.out.emit("positions", &positions);
                return Ok(());
            }
            if positions.is_empty() {
                println!("no liquidity positions");
                return Ok(());
            }
            for p in &positions {
                let staked = if p.lp_staked.is_zero() {
                    String::new()
                } else {
                    let place = p.gauge.map_or("", |g| if g == wallet_core::gauge::GaugeKind::Zone { " (launch-zone)" } else { "" });
                    format!("  staked {}{place}", amount::format_amount_short(p.lp_staked, 18, 6))
                };
                println!("{:<16} {:>8}  {:>10}{}", p.name(), p.share_text(), p.usd.map(amount::usd).unwrap_or_else(|| "—".into()), staked);
                println!("  {}", p.underlying_text());
            }
            Ok(())
        }
        PoolCmd::Add { pair, amount: value, token, slippage, account, fee, quote: quote_only } => {
            let slippage = slippage.unwrap_or(ctx.config.swap_slippage_bps);
            let mut s = if quote_only { ctx.session().await? } else { ctx.unlocked().await? };
            let pair = resolve_pair(ctx, &mut s, &pair).await?;
            let side = token.as_deref();
            let quote =
                s.add_liquidity_quote_for(account.as_deref(), &pair, &value, side, slippage, wallet_core::data::Trust::FirstHand).await?;
            if quote_only {
                if ctx.out.json() {
                    ctx.out.emit("add liquidity quote", &quote);
                } else {
                    println!("deposit {} · estimated pool share {}", quote.deposit_text(), quote.share_text());
                }
                return Ok(());
            }
            if !ctx.out.json() {
                println!("deposit {}", quote.deposit_text());
                println!("pool share after {}", quote.share_text());
            }
            let deadline = ctx.config.swap_deadline_minutes;
            run_action(
                ctx,
                &mut s,
                "add liquidity",
                account.as_deref(),
                fee.max_fee.as_deref(),
                wallet_core::execution::TradingAction::AddLiquidity { pair, amount: value, side: token, slippage, deadline },
            )
            .await
            .map(|_| ())
        }
        PoolCmd::Remove { pair, percent, slippage, account, fee, quote: quote_only } => {
            let slippage = slippage.unwrap_or(ctx.config.swap_slippage_bps);
            let mut s = if quote_only { ctx.session().await? } else { ctx.unlocked().await? };
            let pair = resolve_pair(ctx, &mut s, &pair).await?;
            if quote_only {
                let quote = s.remove_liquidity_quote_for(account.as_deref(), &pair, percent, slippage).await?;
                if ctx.out.json() {
                    ctx.out.emit("remove liquidity quote", &quote);
                } else {
                    println!(
                        "burn {} LP; receive at least {} {} and {} {}",
                        amount::format_amount(quote.liquidity, 18),
                        amount::format_amount(quote.amount0_min, quote.token0.decimals),
                        quote.token0.symbol,
                        amount::format_amount(quote.amount1_min, quote.token1.decimals),
                        quote.token1.symbol
                    );
                }
                return Ok(());
            }
            let deadline = ctx.config.swap_deadline_minutes;
            run_action(
                ctx,
                &mut s,
                "remove liquidity",
                account.as_deref(),
                fee.max_fee.as_deref(),
                wallet_core::execution::TradingAction::RemoveLiquidity { pair, percent, slippage, deadline },
            )
            .await
            .map(|_| ())
        }
    }
}

/// A launch by symbol (case-insensitive, must be unique) or token address.
async fn launch_by(data: &wallet_core::data::DataCtx, token: &str) -> Result<wallet_core::launches::Launch> {
    let wanted = token.trim();
    let matches: Vec<_> = wallet_core::launches::launches(data, 200)
        .await?
        .into_iter()
        .filter(|l| l.token.eq_ignore_ascii_case(wanted) || l.symbol.eq_ignore_ascii_case(wanted))
        .collect();
    match matches.len() {
        1 => Ok(matches.into_iter().next().unwrap_or_default()),
        0 => Err(CoreError::NotFound(format!("no Quainance launch `{wanted}` (see `pool launches`)"))),
        n => Err(CoreError::Invalid(format!("{n} launches are called `{wanted}`; name one by contract address"))),
    }
}

/// The LP a pair holds in the wallet or the gauge, for the `--amount`-less forms of stake/unstake.
async fn lp_balances(s: &mut Session, pair: &str, account: Option<&str>, gauge: Option<&str>, staked: bool) -> Result<U256> {
    if staked {
        return Ok(s.stake_target_in_gauge(account, pair, gauge).await?.staked);
    }
    let owner = s.account(account)?.address.parse().map_err(|_| CoreError::Invalid("invalid account".into()))?;
    let pair = pair.parse().map_err(|_| CoreError::Invalid("invalid pair".into()))?;
    Ok(wallet_core::sdk::contracts::Erc20::new(pair, s.provider())?.balance_of(owner, owner, wallet_core::sdk::BlockTag::Latest).await?)
}

/// `farm` subcommands.
pub async fn farm(ctx: &Ctx, cmd: FarmCmd) -> Result<()> {
    match cmd {
        FarmCmd::List => {
            let s = ctx.session().await?;
            let data = s.data_ctx()?;
            let owners = s.quai_owner_addresses();
            let view = wallet_core::gauge::open(&data, &owners).await?;
            let zone = wallet_core::zone::open(&data, &owners).await.ok();
            if ctx.out.json() {
                // Additive: the core gauge's fields stay where they were, with the zone alongside.
                let mut out = serde_json::to_value(&view).unwrap_or_default();
                out["zone"] = serde_json::to_value(&zone).unwrap_or_default();
                ctx.out.emit("gauge", &out);
                return Ok(());
            }
            let now = wallet_core::registry::now();
            for pool in &view.pools {
                println!("pid {}  {}", pool.pid, pool.lp_token);
                println!(
                    "  staked {} of {}",
                    amount::format_amount_short(pool.staked, 18, 6),
                    amount::format_amount_short(pool.total_staked, 18, 6)
                );
                for r in &pool.rewards {
                    println!(
                        "  {} · {:.4}/day to the pool · {} · claimable {}",
                        r.token.symbol,
                        r.per_day(),
                        r.period_text(now),
                        r.earned_text()
                    );
                }
            }
            // Launch-zone pools: every campaign still running, and anything these accounts hold in one.
            for pool in zone.iter().flat_map(|z| &z.pools) {
                if !pool.active(now) && pool.staked.is_zero() && !pool.has_rewards() {
                    continue;
                }
                println!("launch-zone pid {}  {}  ({})", pool.pid, pool.lp_token, pool.campaign.state(now).text());
                println!(
                    "  staked {} of {}",
                    amount::format_amount_short(pool.staked, 18, 6),
                    amount::format_amount_short(pool.total_staked, 18, 6)
                );
                if let Some(text) = pool.reward_text(now) {
                    println!("  {text}");
                }
                for r in pool.rewards.iter().filter(|r| !r.earned.is_zero()) {
                    println!("  claimable {}", r.earned_text());
                }
            }
            Ok(())
        }
        FarmCmd::Stake { pair, gauge, amount: value, account, fee } => {
            let mut s = ctx.unlocked().await?;
            let pair = resolve_pair(ctx, &mut s, &pair).await?;
            let value = match value {
                Some(v) => v,
                None => amount::format_amount(lp_balances(&mut s, &pair, account.as_deref(), gauge.as_deref(), false).await?, 18),
            };
            run_action(
                ctx,
                &mut s,
                "stake",
                account.as_deref(),
                fee.max_fee.as_deref(),
                wallet_core::execution::TradingAction::Stake { pair, gauge, amount: value },
            )
            .await
            .map(|_| ())
        }
        FarmCmd::Unstake { pair, gauge, amount: value, account, fee } => {
            let mut s = ctx.unlocked().await?;
            let pair = resolve_pair(ctx, &mut s, &pair).await?;
            let value = match value {
                Some(v) => v,
                None => amount::format_amount(lp_balances(&mut s, &pair, account.as_deref(), gauge.as_deref(), true).await?, 18),
            };
            let review = s.review_unstake_in_gauge(account.as_deref(), &pair, gauge.as_deref(), &value, fee.max_fee.as_deref()).await?;
            let submitted = ctx.authorize(&mut s, review).await?;
            ctx.print_submitted("unstake", &submitted);
            Ok(())
        }
        FarmCmd::Harvest { pair, gauge, account, fee } => {
            let mut s = ctx.unlocked().await?;
            let pair = resolve_pair(ctx, &mut s, &pair).await?;
            let review = s.review_harvest_in_gauge(account.as_deref(), &pair, gauge.as_deref(), false, fee.max_fee.as_deref()).await?;
            let submitted = ctx.authorize(&mut s, review).await?;
            ctx.print_submitted("harvest", &submitted);
            Ok(())
        }
        FarmCmd::Exit { pair, gauge, account, fee } => {
            let mut s = ctx.unlocked().await?;
            let pair = resolve_pair(ctx, &mut s, &pair).await?;
            let review = s.review_harvest_in_gauge(account.as_deref(), &pair, gauge.as_deref(), true, fee.max_fee.as_deref()).await?;
            let submitted = ctx.authorize(&mut s, review).await?;
            ctx.print_submitted("exit", &submitted);
            Ok(())
        }
        FarmCmd::Incentivize { pair, reward, amount: value, days, account, fee } => {
            let mut s = ctx.unlocked().await?;
            let pair = resolve_pair(ctx, &mut s, &pair).await?;
            run_action(
                ctx,
                &mut s,
                "incentivize",
                account.as_deref(),
                fee.max_fee.as_deref(),
                wallet_core::execution::TradingAction::Incentivize { pair, token: reward, amount: value, days },
            )
            .await
            .map(|_| ())
        }
    }
}

/// `general` / `#general` names a channel; with `--dm`, a payment code or a contact who has one.
fn chat_target(s: &wallet_core::session::Session, chat: &str, dm: bool) -> wallet_core::Result<String> {
    if !dm {
        let name = chat.trim_start_matches('#');
        wallet_core::messages::channel_tag(name)?;
        return Ok(wallet_core::chat::channel_target(name));
    }
    let code = s
        .app
        .contacts()?
        .into_iter()
        .find(|c| c.name.eq_ignore_ascii_case(chat))
        .and_then(|c| c.payment_code)
        .unwrap_or_else(|| chat.to_string());
    wallet_core::sdk::payments::PaymentCode::from_base58(&code)
        .map_err(|_| wallet_core::CoreError::Invalid(format!("`{chat}` is not a payment code or a contact with one")))?;
    Ok(wallet_core::chat::dm_target(&code))
}
