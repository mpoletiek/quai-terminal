//! Trading performance in QUAI, from this wallet's own trades.
//!
//! Every swap, bonding-curve trade and Hartii trade the wallet made is a [`Fill`]: what left and
//! what arrived, from the receipt where the tracker recorded it (`actual_out`, `actual_in`), else
//! from the review's own figures, marked estimated. WQUAI counts as QUAI.
//!
//! Cost is average cost: each buy moves one average price per token, and a sale realizes the
//! difference between what it fetched and that average. A token bought with another token carries
//! the paid token's average cost across, so a route through USDT keeps its QUAI basis.
//!
//! Open positions are marked at the spot price of the deepest WQUAI pool, or the token's bonding
//! curve, and only what the wallet still holds counts: tokens sent away or unwrapped leave at
//! cost, neither gain nor loss. Only trades made through this wallet are here; a buy made
//! elsewhere shows up as a sale with no recorded cost, which is reported, never guessed.

use crate::journal::OpKind;
use crate::appdb::{OpStatus, Operation};
use crate::error::Result;
use crate::session::Session;
use serde::Serialize;
use std::collections::HashMap;

/// Operation kinds that are trades.
pub const TRADE_KINDS: [&str; 6] = ["swap", "swap_exact_output", "curve_buy", "curve_sell", "hartii_buy", "hartii_sell"];

/// Below this many units a leg is rounding, not a trade.
const DUST: f64 = 1e-12;

/// One token's side of a fill.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Leg {
    /// Contract (lowercase).
    pub token: String,
    pub symbol: String,
    pub decimals: u8,
    /// Received when positive, paid when negative, in token units.
    pub units: f64,
}

/// One confirmed trade.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Fill {
    pub op_id: String,
    pub at: u64,
    pub tx: Option<String>,
    pub kind: OpKind,
    pub account: String,
    /// QUAI received when positive, paid when negative (WQUAI included).
    pub quai: f64,
    /// The tokens that moved: one for a trade against QUAI, two for token for token.
    pub legs: Vec<Leg>,
    /// Some figure came from the review rather than the receipt.
    pub estimated: bool,
    /// Gas paid, in QUAI.
    pub fee: f64,
}

impl Fill {
    /// `buy`, `sell` or `swap` (token for token).
    pub fn side(&self) -> &'static str {
        match self.legs.as_slice() {
            [leg] if leg.units > 0.0 => "buy",
            [_] => "sell",
            _ => "swap",
        }
    }
}

/// One token's standing.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Position {
    pub token: String,
    pub symbol: String,
    pub decimals: u8,
    pub trades: usize,
    /// Units bought and sold through this wallet.
    pub bought: f64,
    pub sold: f64,
    /// Units still held from those buys.
    pub open: f64,
    /// QUAI cost of the open units.
    pub cost: f64,
    /// QUAI per unit paid for the open units.
    pub avg_cost: Option<f64>,
    /// QUAI paid for every buy, and fetched by every sale.
    pub spent: f64,
    pub proceeds: f64,
    pub realized: f64,
    /// QUAI per unit now, and what the open units are worth at it.
    pub mark: Option<f64>,
    pub value: Option<f64>,
    pub unrealized: Option<f64>,
    /// Units that left the wallet other than by a trade (sent, unwrapped), removed at cost.
    pub moved_out: f64,
    /// Units sold with no recorded buy behind them: their proceeds realize nothing.
    pub unmatched_sold: f64,
    /// Part of the cost is unknown: bought with a token that had no recorded cost.
    pub incomplete_basis: bool,
    /// A figure behind it came from a review, not a receipt.
    pub estimated: bool,
}

/// Performance across every token this wallet traded.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Pnl {
    /// Open positions first, largest first; closed ones after, by realized PnL.
    pub positions: Vec<Position>,
    /// Newest first.
    pub fills: Vec<Fill>,
    pub realized: f64,
    /// Over the positions that could be marked.
    pub unrealized: f64,
    /// Gas for every trade, failed ones included.
    pub fees: f64,
    /// Realized plus unrealized, less fees.
    pub net: f64,
    /// Open positions with no price to mark them at.
    pub unmarked: usize,
}

/// A QUAI total: `1,284.52`, `0.0342`. Display only.
pub fn quai_text(value: f64) -> String {
    if !value.is_finite() {
        return "—".into();
    }
    let v = value.abs();
    if v < 0.00005 {
        return "0.00".into();
    }
    let sign = if value < 0.0 { "-" } else { "" };
    if v >= 1.0 { format!("{sign}{}", crate::amount::group_thousands(&format!("{v:.2}"))) } else { format!("{sign}{v:.4}") }
}

/// A gain or loss in QUAI, always signed: `+12.40`, `-0.0310`.
pub fn signed_text(value: f64) -> String {
    let text = quai_text(value);
    if text.starts_with('-') || text == "—" || text == "0.00" { text } else { format!("+{text}") }
}

/// A token quantity: `22,957.7016` in full up to ten thousand, `4.1M` past it.
pub fn units_text(value: f64) -> String {
    if !value.is_finite() {
        return "—".into();
    }
    if value.abs() >= 10_000.0 {
        return crate::amount::compact(value);
    }
    // Grouped on the magnitude: grouping `-100.0000` itself reads `-,100.0000`.
    let grouped = crate::amount::group_thousands(&format!("{:.4}", value.abs()));
    if value < 0.0 { format!("-{grouped}") } else { grouped }
}

/// QUAI per unit, with enough significant digits for a sub-thousandth token: `0.0₆9835`.
pub fn price_text(value: f64) -> String {
    if !value.is_finite() || value <= 0.0 {
        return "—".into();
    }
    if let Some(tiny) = crate::amount::subscript_zeros(value, 4) {
        return tiny;
    }
    if value >= 1000.0 {
        crate::amount::group_thousands(&format!("{value:.2}"))
    } else if value >= 1.0 {
        format!("{value:.4}")
    } else {
        format!("{:.*}", ((-value.log10()).floor() as usize + 4).min(12), value)
    }
}

fn units(atoms: &str, decimals: u8) -> Option<f64> {
    let atoms = crate::sdk::U256::from_str_radix(atoms.trim(), 10).ok()?;
    Some(crate::amount::to_f64(atoms, decimals))
}

fn is_quai(token: &str, wquai: &str) -> bool {
    token.eq_ignore_ascii_case("quai") || (!wquai.is_empty() && token.eq_ignore_ascii_case(wquai))
}

/// The confirmed trades among `ops`, oldest first, and the gas spent on trades that failed.
pub fn fills(ops: &[Operation], wquai: &str) -> (Vec<Fill>, f64) {
    let mut out = Vec::new();
    let mut failed_fees = 0.0;
    for op in ops.iter().filter(|op| TRADE_KINDS.contains(&op.kind.as_str())) {
        let fee = units(&op.fee, 18).unwrap_or(0.0);
        match op.status {
            OpStatus::Confirmed | OpStatus::Settled => {}
            OpStatus::Failed => {
                failed_fees += fee;
                continue;
            }
            _ => continue,
        }
        if let Some(fill) = fill(op, wquai, fee) {
            out.push(fill);
        }
    }
    out.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.op_id.cmp(&b.op_id)));
    (out, failed_fees)
}

/// A buy paid for in native QUAI, whose curve names the token bought as `token`.
fn buys_on_curve(kind: &OpKind) -> bool {
    matches!(kind, OpKind::CurveBuy | OpKind::HartiiBuy)
}

/// Trades recorded before reviews listed their effects: what was paid is the operation's own
/// asset and amount, what arrived the detail's `to_*` fields and the quoted output, which the
/// receipt's `actual_out` still replaces where it was recorded. `None` when it names no output.
fn legacy_effects(op: &Operation) -> Option<Vec<serde_json::Value>> {
    let detail = &op.detail;
    let native = buys_on_curve(&op.kind) || op.asset.eq_ignore_ascii_case("QUAI");
    let paid = if native { "quai".to_string() } else { detail.from_token().as_str()?.to_lowercase() };
    let received = detail.to_token().as_str().or_else(|| buys_on_curve(&op.kind).then(|| detail.token().as_str()).flatten())?;
    Some(vec![
        serde_json::json!({"direction": "out", "asset": op.asset, "token": paid, "decimals": if native { 18 } else { detail.decimals().as_u64().unwrap_or(18) }, "amount": op.amount}),
        serde_json::json!({"direction": "in", "asset": detail.to_symbol().as_str().unwrap_or("?"), "token": received.to_lowercase(),
            "decimals": detail.to_decimals().as_u64().unwrap_or(18), "amount": detail.expected_out().as_str()?, "estimated": true}),
    ])
}

fn fill(op: &Operation, wquai: &str, fee: f64) -> Option<Fill> {
    let detail = &op.detail;
    let legacy;
    let effects = match detail.financial_effects().as_array() {
        Some(effects) => effects,
        None => {
            legacy = legacy_effects(op)?;
            &legacy
        }
    };
    let text = |v: &serde_json::Value| v.as_str().map(str::to_lowercase);
    let to_token = text(detail.to_token()).or_else(|| buys_on_curve(&op.kind).then(|| text(detail.token())).flatten());
    let (mut actual_out, mut actual_in) = (text(detail.actual_out()), text(detail.actual_in()));
    // An exact-output swap spends at most its cap; the quote's input is the better figure.
    if op.kind == OpKind::SwapExactOutput && actual_in.is_none() {
        actual_in = text(detail.required_input());
    }
    let mut estimated = op.kind == OpKind::SwapExactOutput && detail.actual_in().is_null();
    let mut quai = 0.0;
    let mut legs: Vec<Leg> = Vec::new();
    for effect in effects {
        let token = effect["token"].as_str()?.to_lowercase();
        let decimals = u8::try_from(effect["decimals"].as_u64()?).ok()?;
        let incoming = effect["direction"].as_str()? == "in";
        let mut amount = effect["amount"].as_str()?.to_string();
        let mut from_review = effect["estimated"].as_bool().unwrap_or(false);
        // The receipt's figures replace the review's: what arrived as the swap's output, and
        // what the first thing paid actually took.
        let receives = to_token.as_deref().is_some_and(|t| t == token || (is_quai(t, wquai) && is_quai(&token, wquai)));
        if incoming
            && receives
            && let Some(out) = actual_out.take()
        {
            amount = out;
            from_review = false;
        } else if !incoming && let Some(paid) = actual_in.take() {
            amount = paid;
            from_review = false;
        }
        let value = units(&amount, decimals)? * if incoming { 1.0 } else { -1.0 };
        // An estimate of nothing (a curve buy's usual zero credit) changes no figure.
        estimated |= from_review && value != 0.0;
        if is_quai(&token, wquai) {
            quai += value;
        } else if let Some(leg) = legs.iter_mut().find(|l| l.token == token) {
            leg.units += value;
        } else {
            let symbol = crate::explorer::clean(effect["asset"].as_str().unwrap_or("?"), 32);
            legs.push(Leg { token, symbol, decimals, units: value });
        }
    }
    legs.retain(|l| l.units.abs() > DUST);
    (!legs.is_empty()).then(|| Fill {
        op_id: op.id.clone(),
        at: op.created,
        tx: op.tx_hash.clone(),
        kind: op.kind.clone(),
        account: op.account.to_lowercase(),
        quai,
        legs,
        estimated,
        fee,
    })
}

/// QUAI per unit for each token that trades against WQUAI: the deepest such pool's spot price,
/// or the token's own bonding curve.
pub fn marks(pools: &[crate::markets::Pool], wquai: &str) -> HashMap<String, f64> {
    let mut best: HashMap<String, (f64, f64)> = HashMap::new();
    for pool in pools {
        let Some(spot) = pool.spot_price() else { continue };
        let (a, b) = (pool.token0.address.to_lowercase(), pool.token1.address.to_lowercase());
        let (token, price, depth) = if is_quai(&b, wquai) {
            (a, spot, pool.curve.as_ref().map_or(pool.reserve1, |c| c.raised_quai))
        } else if is_quai(&a, wquai) {
            (b, 1.0 / spot, pool.reserve0)
        } else {
            continue;
        };
        if price.is_finite() && price > 0.0 && best.get(&token).is_none_or(|(_, d)| depth > *d) {
            best.insert(token, (price, depth));
        }
    }
    best.into_iter().map(|(token, (price, _))| (token, price)).collect()
}

/// Walk the fills in order at average cost, then mark what is still held.
///
/// `held` caps each open position at what the wallet holds now (a token missing from it is not
/// capped); `marks` prices them.
pub fn compute(fills: &[Fill], failed_fees: f64, marks: &HashMap<String, f64>, held: &HashMap<String, f64>) -> Pnl {
    let mut book: HashMap<String, Position> = HashMap::new();
    let mut fees = failed_fees;
    for fill in fills {
        fees += fill.fee;
        let (paid, received): (Vec<&Leg>, Vec<&Leg>) = fill.legs.iter().partition(|l| l.units < 0.0);
        // What was paid, in QUAI: the QUAI itself, and any token at its average cost.
        let mut value = (-fill.quai).max(0.0);
        let mut basis_known = true;
        for leg in &paid {
            let p = entry(&mut book, leg, fill.estimated);
            let units = -leg.units;
            let take = units.min(p.open);
            let basis = if p.open > 0.0 { p.cost * take / p.open } else { 0.0 };
            // A token paid for a token is disposed of at cost: its gain or loss shows in what it
            // bought, once that is sold. Paid for QUAI, it realizes what it fetched above cost.
            let proceeds = if received.is_empty() { fill.quai.max(0.0) } else { basis };
            if units > 0.0 {
                p.realized += proceeds * (take / units) - basis;
            }
            p.proceeds += proceeds;
            p.cost -= basis;
            p.open -= take;
            p.sold += units;
            if take < units {
                p.unmatched_sold += units - take;
                basis_known = false;
            }
            value += basis;
        }
        for leg in &received {
            let p = entry(&mut book, leg, fill.estimated);
            p.bought += leg.units;
            p.open += leg.units;
            let share = value / received.len() as f64;
            p.cost += share;
            p.spent += share;
            p.incomplete_basis |= !basis_known;
        }
    }
    let mut pnl = Pnl { fees, fills: fills.iter().rev().cloned().collect(), ..Default::default() };
    for (_, mut p) in book {
        if let Some(h) = held.get(&p.token).copied()
            && h < p.open
        {
            let moved = p.open - h;
            p.cost -= if p.open > 0.0 { p.cost * moved / p.open } else { 0.0 };
            p.moved_out += moved;
            p.open = h.max(0.0);
        }
        if p.open <= DUST {
            p.open = 0.0;
            p.cost = 0.0;
        }
        p.avg_cost = (p.open > 0.0).then(|| p.cost / p.open);
        if p.open > 0.0 {
            p.mark = marks.get(&p.token).copied();
            p.value = p.mark.map(|m| m * p.open);
            p.unrealized = p.value.map(|v| v - p.cost);
            match p.unrealized {
                Some(u) => pnl.unrealized += u,
                None => pnl.unmarked += 1,
            }
        }
        pnl.realized += p.realized;
        pnl.positions.push(p);
    }
    pnl.positions.sort_by(|a, b| {
        (b.open > 0.0)
            .cmp(&(a.open > 0.0))
            .then_with(|| b.value.unwrap_or(b.cost).total_cmp(&a.value.unwrap_or(a.cost)))
            .then_with(|| b.realized.abs().total_cmp(&a.realized.abs()))
    });
    pnl.net = pnl.realized + pnl.unrealized - pnl.fees;
    pnl
}

fn entry<'a>(book: &'a mut HashMap<String, Position>, leg: &Leg, estimated: bool) -> &'a mut Position {
    let p = book.entry(leg.token.clone()).or_default();
    if p.token.is_empty() {
        (p.token, p.symbol, p.decimals) = (leg.token.clone(), leg.symbol.clone(), leg.decimals);
    }
    p.trades += 1;
    p.estimated |= estimated;
    p
}

impl Session {
    /// This wallet's trading performance in QUAI (see the module notes).
    pub async fn pnl(&mut self) -> Result<Pnl> {
        let ops = self.app.operations(&self.network.id, 10_000)?;
        let wquai = self.network.wquai.clone().unwrap_or_default().to_lowercase();
        let (fills, failed_fees) = fills(&ops, &wquai);
        // Marks come from the market directory, cached like every other read of it. Without it
        // the positions stand at cost, unmarked, rather than failing the whole view.
        let pools = match self.data_ctx() {
            Ok(ctx) => crate::markets::all_markets(&ctx).await.map(|(pools, _)| pools).unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        let marks = marks(&pools, &wquai);
        let held = self.held_units(&fills).await;
        Ok(compute(&fills, failed_fees, &marks, &held))
    }

    /// What the trading accounts hold now of each token they traded, in units. A token whose
    /// balance could not be read is left out, so its position is not capped.
    async fn held_units(&self, fills: &[Fill]) -> HashMap<String, f64> {
        use crate::multicall::{Arg, Call, Multicall, word};
        let mut accounts: Vec<&str> = fills.iter().map(|f| f.account.as_str()).collect();
        accounts.sort_unstable();
        accounts.dedup();
        let mut tokens: Vec<(&str, u8)> = fills.iter().flat_map(|f| f.legs.iter().map(|l| (l.token.as_str(), l.decimals))).collect();
        tokens.sort_unstable();
        tokens.dedup_by(|a, b| a.0 == b.0);
        let pairs: Vec<(&str, &str, u8)> = accounts.iter().flat_map(|a| tokens.iter().map(move |(t, d)| (*a, *t, *d))).collect();
        let mut held: HashMap<String, f64> = HashMap::new();
        let mut unread: Vec<String> = Vec::new();
        let add = |held: &mut HashMap<String, f64>, token: &str, atoms: crate::sdk::U256, decimals: u8| {
            *held.entry(token.to_string()).or_default() += crate::amount::to_f64(atoms, decimals);
        };
        let batched = match Multicall::on(&self.app, &self.node, &self.network, crate::data::Trust::Cached).await {
            Some(mc) => {
                let calls: Vec<Call> =
                    pairs.iter().map(|(a, t, _)| Call::view(t, "balanceOf(address)", &[Arg::Addr((*a).to_string())])).collect();
                mc.try_all(&calls).await.ok()
            }
            None => None,
        };
        match batched {
            Some(results) => {
                for ((_, token, decimals), data) in pairs.iter().zip(results) {
                    match data {
                        Some(d) => add(&mut held, token, word(&d, 0), *decimals),
                        None => unread.push((*token).to_string()),
                    }
                }
            }
            None => {
                for (account, token, decimals) in &pairs {
                    let read = async {
                        let (owner, contract) = (crate::chain::addr(account)?, crate::chain::addr(token)?);
                        crate::sdk::contracts::Erc20::new(contract, &self.node.provider)?
                            .balance_of(owner, owner, crate::sdk::BlockTag::Latest)
                            .await
                            .map_err(crate::error::CoreError::from)
                    };
                    match read.await {
                        Ok(atoms) => add(&mut held, token, atoms, *decimals),
                        Err(_) => unread.push((*token).to_string()),
                    }
                }
            }
        }
        for token in unread {
            held.remove(&token);
        }
        held
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const WQUAI: &str = "0x00wquai";

    fn op(id: &str, at: u64, kind: &str, status: OpStatus, detail: serde_json::Value) -> Operation {
        let kind = OpKind::parse(kind);
        Operation {
            id: id.into(),
            network: "mainnet".into(),
            kind,
            store: String::new(),
            account: "0xME".into(),
            status,
            tx_hash: Some(format!("0x{id}")),
            asset: String::new(),
            amount: String::new(),
            counterparty: String::new(),
            fee: "1000000000000000".into(),
            detail: detail.into(),
            created: at,
            updated: at,
        }
    }

    fn e18(units: u64) -> String {
        (crate::sdk::U256::from(units) * crate::sdk::U256::from(10u64).pow(crate::sdk::U256::from(18))).to_string()
    }

    fn effect(direction: &str, asset: &str, token: &str, amount: String, estimated: bool) -> serde_json::Value {
        json!({"direction": direction, "asset": asset, "token": token, "decimals": 18, "amount": amount, "estimated": estimated})
    }

    /// A swap buying `tokens` MOON for `quai` QUAI, the receipt saying `actual` arrived.
    fn buy(id: &str, at: u64, quai: u64, tokens: u64, actual: Option<u64>) -> Operation {
        let mut detail = crate::journal::Detail::from(json!({"to_token": "0xmoon", "financial_effects": [
            effect("out", "QUAI", "quai", e18(quai), false),
            effect("in", "MOON", "0xmoon", e18(tokens), true),
        ]}));
        if let Some(a) = actual {
            detail.set_actual_out(json!(e18(a)));
        }
        op(id, at, "swap", OpStatus::Confirmed, detail.into_json())
    }

    fn sell(id: &str, at: u64, tokens: u64, quai: u64) -> Operation {
        op(
            id,
            at,
            "swap",
            OpStatus::Confirmed,
            json!({"to_token": "quai", "actual_out": e18(quai), "financial_effects": [
                effect("out", "MOON", "0xmoon", e18(tokens), false),
                effect("in", "QUAI", "quai", e18(quai - 1), true),
            ]}),
        )
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn the_receipt_s_output_replaces_the_review_s_estimate() {
        let (fills, _) = fills(&[buy("b1", 1, 100, 1000, Some(990))], WQUAI);
        assert_eq!(fills.len(), 1);
        assert!(close(fills[0].legs[0].units, 990.0), "the receipt said 990 arrived, not the 1000 quoted");
        assert!(close(fills[0].quai, -100.0));
        assert!(!fills[0].estimated, "every figure is the receipt's");
        let (fills, _) = super::fills(&[buy("b2", 1, 100, 1000, None)], WQUAI);
        assert!(fills[0].estimated, "no receipt output: the quote stands, marked");
        assert_eq!(fills[0].side(), "buy");
    }

    /// Average cost: two buys set one price, a partial sale realizes against it, and the rest
    /// is marked at the pool's price.
    #[test]
    fn average_cost_realizes_a_sale_and_marks_the_rest() {
        let ops = [buy("b1", 1, 100, 1000, Some(1000)), buy("b2", 2, 300, 1000, Some(1000)), sell("s1", 3, 500, 150)];
        let (fills, failed) = fills(&ops, WQUAI);
        let marks = HashMap::from([("0xmoon".to_string(), 0.4)]);
        let pnl = compute(&fills, failed, &marks, &HashMap::new());
        let p = &pnl.positions[0];
        // 2,000 bought for 400 QUAI: 0.2 each. 500 sold for 150 against a cost of 100.
        assert!(close(p.realized, 50.0), "{}", p.realized);
        assert!(close(p.open, 1500.0) && close(p.cost, 300.0));
        assert_eq!(p.avg_cost.map(|c| (c * 1e9).round() / 1e9), Some(0.2));
        // 1,500 left at 0.4 is worth 600 against 300 of cost.
        assert!(close(p.unrealized.unwrap(), 300.0));
        assert!(close(pnl.fees, 0.003), "three trades' gas: {}", pnl.fees);
        assert!(close(pnl.net, 50.0 + 300.0 - 0.003));
        assert_eq!(pnl.fills[0].op_id, "s1", "newest first");
    }

    /// WQUAI is QUAI, a failed trade still cost its gas, and a trade still in flight is no fill.
    #[test]
    fn wquai_counts_as_quai_and_only_confirmed_trades_are_fills() {
        let wquai_buy = op(
            "w1",
            1,
            "swap",
            OpStatus::Confirmed,
            json!({"to_token": "0xmoon", "actual_out": e18(50), "financial_effects": [
                effect("out", "WQUAI", WQUAI, e18(10), false),
                effect("in", "MOON", "0xmoon", e18(50), true),
            ]}),
        );
        let failed = op("f1", 2, "swap", OpStatus::Failed, json!({}));
        let pending = op("p1", 3, "swap", OpStatus::Submitted, buy("x", 3, 1, 1, None).detail.into_json());
        let (fills, failed_fees) = fills(&[wquai_buy, failed, pending], WQUAI);
        assert_eq!(fills.len(), 1);
        assert!(close(fills[0].quai, -10.0), "paid in WQUAI is paid in QUAI");
        assert!(close(failed_fees, 0.001));
    }

    /// A curve buy's cost is what it paid less the credit it left on the curve.
    #[test]
    fn a_curve_buy_costs_its_payment_less_the_credit_it_leaves() {
        let curve = op(
            "c1",
            1,
            "curve_buy",
            OpStatus::Confirmed,
            json!({"to_token": "0xmoon", "actual_out": e18(900), "financial_effects": [
                effect("out", "QUAI", "quai", e18(100), false),
                effect("in", "MOON", "0xmoon", e18(1000), true),
                effect("in", "QUAI curve credit", "quai", e18(5), true),
            ]}),
        );
        let (fills, _) = fills(&[curve], WQUAI);
        assert!(close(fills[0].quai, -95.0));
        assert!(close(fills[0].legs[0].units, 900.0), "the receipt's tokens, not the credit's QUAI");
    }

    /// Token for token carries the paid token's cost across: no gain appears until a sale.
    #[test]
    fn a_token_bought_with_a_token_inherits_its_cost() {
        let swap = op(
            "t1",
            2,
            "swap",
            OpStatus::Confirmed,
            json!({"to_token": "0xstar", "actual_out": e18(10), "financial_effects": [
                effect("out", "MOON", "0xmoon", e18(1000), false),
                effect("in", "STAR", "0xstar", e18(10), true),
            ]}),
        );
        let (fills, _) = fills(&[buy("b1", 1, 100, 1000, Some(1000)), swap], WQUAI);
        assert_eq!(fills[1].side(), "swap");
        let pnl = compute(&fills, 0.0, &HashMap::new(), &HashMap::new());
        let star = pnl.positions.iter().find(|p| p.symbol == "STAR").unwrap();
        let moon = pnl.positions.iter().find(|p| p.symbol == "MOON").unwrap();
        assert!(close(star.cost, 100.0) && !star.incomplete_basis, "STAR cost what the MOON behind it did");
        assert!(close(moon.realized, 0.0) && close(moon.open, 0.0), "MOON left at cost");
        assert_eq!(pnl.unmarked, 1, "STAR has no price here, so it is counted as unmarked");
    }

    /// Tokens that left without a trade go at cost; a sale with no recorded buy realizes nothing.
    #[test]
    fn transfers_out_leave_at_cost_and_unrecorded_buys_are_not_guessed() {
        let (fills, _) = fills(&[buy("b1", 1, 100, 1000, Some(1000))], WQUAI);
        let held = HashMap::from([("0xmoon".to_string(), 400.0)]);
        let marks = HashMap::from([("0xmoon".to_string(), 0.1)]);
        let pnl = compute(&fills, 0.0, &marks, &held);
        let p = &pnl.positions[0];
        assert!(close(p.moved_out, 600.0) && close(p.open, 400.0) && close(p.cost, 40.0));
        assert!(close(p.unrealized.unwrap(), 0.0), "marked at cost: 400 at 0.1 is the 40 they cost");
        let (fills, _) = super::fills(&[sell("s1", 1, 500, 150)], WQUAI);
        let pnl = compute(&fills, 0.0, &HashMap::new(), &HashMap::new());
        assert!(close(pnl.positions[0].unmatched_sold, 500.0));
        assert!(close(pnl.realized, 0.0), "no recorded cost, so no realized gain is claimed");
    }

    /// Trades recorded before reviews listed their effects still count: a curve buy paid in
    /// native QUAI for the curve's `token`, and a swap from its `from_token` to QUAI.
    #[test]
    fn older_trade_records_without_effects_still_count() {
        let curve = op(
            "c0",
            1,
            "curve_buy",
            OpStatus::Confirmed,
            json!({"token": "0xQMON", "to_symbol": "QMON", "to_decimals": 18, "expected_out": e18(8000)}),
        );
        let curve = Operation { asset: "QUAI".into(), amount: e18(100), ..curve };
        let swap = op(
            "s0",
            2,
            "swap",
            OpStatus::Confirmed,
            json!({"from_token": "0xqmon", "decimals": 18, "to_token": "quai", "to_symbol": "QUAI", "to_decimals": 18,
                "expected_out": e18(70), "actual_out": e18(60)}),
        );
        let swap = Operation { asset: "QMON".into(), amount: e18(4000), ..swap };
        let (fills, _) = fills(&[curve, swap], WQUAI);
        assert_eq!(fills.len(), 2, "both older records are trades");
        assert!(close(fills[0].quai, -100.0) && close(fills[0].legs[0].units, 8000.0) && fills[0].estimated);
        assert_eq!(fills[0].legs[0].symbol, "QMON");
        assert!(close(fills[1].quai, 60.0), "the receipt's 60, not the 70 quoted");
        assert!(!fills[1].estimated);
        let pnl = compute(&fills, 0.0, &HashMap::new(), &HashMap::new());
        // Half the 8,000 bought for 100 sold for 60: a 10 QUAI gain on a 50 QUAI cost.
        assert!(close(pnl.realized, 10.0), "{}", pnl.realized);
    }

    #[test]
    fn quantities_and_totals_read_cleanly_either_side_of_zero() {
        assert_eq!(units_text(-100.0), "-100.0000", "never `-,100.0000`");
        assert_eq!(units_text(-1_234.5), "-1,234.5000");
        assert_eq!(units_text(4_052_292.39), "4.1M");
        assert_eq!(signed_text(12.4), "+12.40");
        assert_eq!(signed_text(-0.031), "-0.0310");
        assert_eq!(signed_text(0.0), "0.00");
        assert_eq!(price_text(0.000123), "0.0₃123");
    }

    #[test]
    fn marks_take_the_deepest_wquai_pool_either_way_round() {
        use crate::markets::{Pool, PoolToken};
        let token = |a: &str| PoolToken { address: a.into(), symbol: a.into(), decimals: 18 };
        let pool = |t0: &str, t1: &str, r0: f64, r1: f64| Pool {
            token0: token(t0),
            token1: token(t1),
            reserve0: r0,
            reserve1: r1,
            ..Default::default()
        };
        let pools = [
            pool("0xmoon", WQUAI, 1000.0, 100.0), // 0.1 QUAI, 100 deep
            pool("0xmoon", WQUAI, 10.0, 5.0),     // 0.5 QUAI, shallow
            pool(WQUAI, "0xstar", 50.0, 10.0),    // 5 QUAI per STAR
            pool("0xstar", "0xusdt", 1.0, 1.0),   // not against WQUAI
        ];
        let m = marks(&pools, WQUAI);
        assert!(close(m["0xmoon"], 0.1), "the deep pool wins");
        assert!(close(m["0xstar"], 5.0), "inverted when WQUAI is token0");
        assert!(!m.contains_key("0xusdt"));
    }
}
