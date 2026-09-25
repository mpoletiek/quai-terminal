//! Durable client-side limit triggers. No daemon receives signing authority. A fixed input and
//! minimum output define the limit price; each signing client re-quotes and checks frozen bounds.
use crate::journal::OpKind;
use crate::appdb::{OpStatus, Operation};
use crate::data::Trust;
use crate::error::{CoreError, Result};
use crate::execution::{TradingAction, TradingIntent};
use crate::markets::Venue;
use crate::plans::{PlanState, TradePlan};
use crate::session::Session;
use crate::swap::{SwapAsset, SwapQuote};
use crate::tx::Review;
use quai_sdk::U256;
use serde::{Deserialize, Serialize};
use serde_json::json;

const CLIENT: &str = "order";
const MAX_QUOTE_AGE: u64 = 30;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Trigger,
    ExecuteOnce,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Armed,
    Triggered,
    Review,
    Waiting,
    Complete,
    Cancelled,
    Expired,
    Stopped,
}

impl State {
    /// The state in plain words.
    pub fn label(self) -> &'static str {
        match self {
            State::Armed => "waiting for price",
            State::Triggered => "ready to sign",
            State::Review => "review open",
            State::Waiting => "confirming",
            State::Complete => "filled",
            State::Cancelled => "cancelled",
            State::Expired => "expired",
            State::Stopped => "stopped",
        }
    }

    /// Still watched: it may yet trigger or be signed.
    pub fn active(self) -> bool {
        matches!(self, State::Armed | State::Triggered | State::Review | State::Waiting)
    }
}

/// The expected output an order should wait for, from what was typed: `+5%` or `-2%` against
/// what the swap would return now, or a plain amount in receive units. More back is better, so
/// `+5%` waits for a price 5% better than now.
pub fn target_from_text(text: &str, current_out: U256, output_decimals: u8) -> Result<U256> {
    let text = text.trim().replace(',', "");
    if text.is_empty() {
        return Err(CoreError::Invalid("type a target: +5% (better than now) or an amount to receive".into()));
    }
    let target = match text.strip_suffix('%') {
        Some(pct) => {
            let pct = pct.trim();
            let (sign, magnitude) = match pct.strip_prefix('-') {
                Some(m) => (-1i64, m),
                None => (1, pct.strip_prefix('+').unwrap_or(pct)),
            };
            // Hundredths of a percent are basis points.
            let bps = crate::amount::parse_amount(magnitude.trim(), 2)
                .map_err(|_| CoreError::Invalid("a percentage is a number such as +5% or -2.5%".into()))?;
            let bps = u64::try_from(bps).map_err(|_| CoreError::Invalid("that percentage is too large".into()))?;
            if sign < 0 && bps >= 10_000 {
                return Err(CoreError::Invalid("a target cannot be 100% or more below the price now".into()));
            }
            let factor = if sign < 0 { 10_000 - bps } else { 10_000 + bps };
            crate::amount::mul_div(current_out, U256::from(factor), U256::from(10_000u64))
                .ok_or_else(|| CoreError::Invalid("that percentage is too large".into()))?
        }
        None => crate::amount::parse_amount(&text, output_decimals)
            .map_err(|_| CoreError::Invalid("a target is +5% (better than now) or an amount to receive".into()))?,
    };
    if target.is_zero() {
        return Err(CoreError::Invalid("the target must be more than zero".into()));
    }
    Ok(target)
}

/// The least an order guarantees for a target: the target less the slippage allowance, rounded
/// down as a swap's own minimum is. A quote meets it exactly when its expected output reaches
/// the target, since both go through the same floor.
pub fn minimum_for_target(target: U256, slippage_bps: u16) -> U256 {
    crate::swap::minimum_out(target, slippage_bps)
}

/// How far the expected output has to move to reach the target, in basis points (positive: it
/// has to rise; zero or less: met).
pub fn distance_bps(current_out: U256, target: U256) -> Option<i64> {
    if current_out.is_zero() {
        return None;
    }
    let (hi, lo, sign) = if target >= current_out { (target, current_out, 1) } else { (current_out, target, -1) };
    // Rounded to the nearest basis point, so +5% typed reads back as +5%, not +4.99%.
    let scaled = (hi - lo).checked_mul(U256::from(10_000u64))?.checked_add(current_out / U256::from(2u64))?;
    let diff = scaled / current_out;
    i64::try_from(diff).ok().map(|d| sign * d)
}

/// Fee bounds for an order when none are typed: each attempt may cost up to the network's fee
/// policy (the level a review starts warning at), and the whole order that times its attempts.
/// Generous on purpose: a cap that refused a normal swap would stop the order at the moment its
/// price arrived, and every attempt is still a review the user signs.
pub fn default_fees(network: &crate::network::NetworkProfile, attempts: u8) -> Result<(String, String)> {
    let per = network.fee_policy(0)?.max_total_fee;
    let total = per.saturating_mul(U256::from(attempts.max(1)));
    Ok((crate::amount::quai(per), crate::amount::quai(total)))
}

/// An order in plain words, one (label, text) row each: what it trades, what it waits for and
/// how far that is, what it guarantees, where it stands, and what it may cost.
pub fn describe(value: &Record, now: u64) -> Vec<(&'static str, String)> {
    let spec = &value.spec;
    let amount = |raw: &str, decimals: u8| {
        U256::from_str_radix(raw, 10)
            .map(|v| crate::amount::group_thousands(&crate::amount::format_amount_short(v, decimals, 6)))
            .unwrap_or_else(|_| "?".into())
    };
    let name = |symbol: &Option<String>, id: &str| {
        symbol.clone().unwrap_or_else(|| {
            if id == "quai" { "QUAI".into() } else { format!("{}…{}", &id[..6.min(id.len())], &id[id.len().saturating_sub(4)..]) }
        })
    };
    let (from, to) = (name(&spec.from_symbol, &spec.from), name(&spec.to_symbol, &spec.to));
    let out = |raw: &str| format!("{} {to}", amount(raw, spec.output_decimals));
    let pct = |bps: u16| format!("{}%", crate::amount::format_amount(U256::from(bps), 2));
    let mut rows = vec![("trade", format!("{} {from} → {to}", amount(&spec.input_atoms, spec.input_decimals)))];
    let now_out = value.last_expected_output_atoms.as_deref();
    let waits = match (&spec.target_output_atoms, now_out) {
        (Some(target), Some(current)) => {
            let distance = distance_bps(atoms(current).unwrap_or_default(), atoms(target).unwrap_or_default());
            let gap = match distance {
                Some(d) if d > 0 => format!(" · needs +{}%", crate::amount::format_amount(U256::from(d as u64), 2)),
                Some(_) => " · met now".into(),
                None => String::new(),
            };
            format!("{} back (now {}{gap})", out(target), out(current))
        }
        (Some(target), None) => format!("{} back", out(target)),
        (None, _) => format!("a quote of at least {} after slippage", out(&spec.minimum_output_atoms)),
    };
    rows.push(("waits for", waits));
    rows.push(("guarantees", format!("at least {} ({} slippage)", out(&spec.minimum_output_atoms), pct(spec.slippage_bps))));
    let checked = value
        .last_observed_at
        .map(|at| format!(" · checked {} ago", crate::track::human_duration(now.saturating_sub(at))))
        .unwrap_or_default();
    rows.push(("state", format!("{}{checked}", value.state.label())));
    // A finished order has nothing left to expire.
    if value.state.active() {
        let left = spec.expires_at.saturating_sub(now);
        rows.push(("expires", if left == 0 { "expired".to_string() } else { format!("in {}", crate::track::human_duration(left)) }));
    }
    rows.push(("attempts", format!("{} of {} used", value.attempts.len(), spec.max_attempts)));
    rows.push((
        "fees",
        format!(
            "up to {} QUAI an attempt · {} of {} QUAI budget used",
            amount(&spec.maximum_fee_atoms, 18),
            amount(&value.fee_budget_used_atoms, 18),
            amount(&spec.total_fee_budget_atoms, 18)
        ),
    ));
    rows.push(("signer", spec.account.clone()));
    rows.push(("router", format!("{} (pinned)", spec.router)));
    rows
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub network: String,
    pub chain_id: u64,
    pub genesis: String,
    pub account: String,
    pub from: String,
    pub to: String,
    pub input_decimals: u8,
    pub output_decimals: u8,
    pub input_atoms: String,
    pub minimum_output_atoms: String,
    pub venue: Venue,
    pub router: String,
    pub router_hash: String,
    pub factory_hash: String,
    pub slippage_bps: u16,
    pub expires_at: u64,
    pub maximum_fee_atoms: String,
    pub total_fee_budget_atoms: String,
    pub max_attempts: u8,
    pub mode: Mode,
    /// What the order waits for: the swap's expected output, before slippage. The minimum above
    /// is this less the slippage allowance, which is what the trigger compares. Display only;
    /// orders made before it existed have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_output_atoms: Option<String>,
    /// Symbols of the two assets when the order was made (untrusted display text).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_symbol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_symbol: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Attempt {
    pub at: u64,
    pub operation: Option<String>,
    pub reserved_fee_atoms: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub version: u32,
    pub spec: Spec,
    pub state: State,
    pub attempts: Vec<Attempt>,
    /// Consumed authorization budget, including declined or failed attempts; not actual fees paid.
    pub fee_budget_used_atoms: String,
    pub last_observed_at: Option<u64>,
    pub last_minimum_output_atoms: Option<String>,
    /// The expected output at the last check, before slippage: what the target is compared with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_expected_output_atoms: Option<String>,
    /// When the order was first found reachable and said so. Set once, under the order's lease,
    /// by whichever process checked it first (the open terminal or the daemon), so it is
    /// announced once and a price that wavers around the target does not announce it again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notified_at: Option<u64>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Create {
    pub account: Option<String>,
    pub from: String,
    pub to: String,
    pub input: String,
    pub minimum_output: String,
    pub slippage_bps: u16,
    pub expires_at: u64,
    pub maximum_fee: String,
    pub total_fee_budget: String,
    pub max_attempts: u8,
    pub mode: Mode,
    /// What the order waits for, as typed: `+5%` against the fresh quote taken when the order is
    /// made, or an amount to receive ([`target_from_text`]). The minimum above is then the target
    /// less the slippage allowance; without a target, the minimum is taken as given.
    pub target_output: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Observation {
    pub id: String,
    pub state: State,
    pub triggered: bool,
    pub minimum_output_atoms: Option<String>,
    pub expected_output_atoms: Option<String>,
    pub observed_at: Option<u64>,
    pub reason: String,
    /// This check found the order reachable for the first time, and wrote the notification.
    pub announced: bool,
}

fn atoms(value: &str) -> Result<U256> {
    U256::from_str_radix(value, 10).map_err(|_| CoreError::Invalid("invalid order atom quantity".into()))
}
fn identity(asset: &SwapAsset) -> String {
    match asset {
        SwapAsset::Quai => "quai".into(),
        SwapAsset::Token { address, .. } => address.to_lowercase(),
    }
}
fn lease(session: &Session, id: &str) -> Result<std::fs::File> {
    crate::plans::claim(&session.registry.paths().wallet_dir(&session.meta.id).join("plan-locks"), id)
}
fn record(plan: &TradePlan) -> Result<Record> {
    if plan.intent["client"] != CLIENT {
        return Err(CoreError::Invalid("this plan is not a limit order".into()));
    }
    let value: Record = serde_json::from_value(plan.intent["order"].clone())?;
    if value.version != 1 {
        return Err(CoreError::Invalid("unsupported limit-order version".into()));
    }
    value.spec.validate()?;
    if plan.version != 1 || plan.network != value.spec.network || !plan.owner.eq_ignore_ascii_case(&value.spec.account) {
        return Err(CoreError::Rejected("order plan owner or network disagrees with its bounds".into()));
    }
    let reserved = value.attempts.iter().try_fold(U256::ZERO, |total, attempt| -> Result<U256> {
        if attempt.reserved_fee_atoms != value.spec.maximum_fee_atoms {
            return Err(CoreError::Rejected("order attempt fee authorization changed".into()));
        }
        total.checked_add(atoms(&attempt.reserved_fee_atoms)?).ok_or_else(|| CoreError::Invalid("order fee accounting overflow".into()))
    })?;
    if value.attempts.len() > usize::from(value.spec.max_attempts)
        || reserved != atoms(&value.fee_budget_used_atoms)?
        || reserved > atoms(&value.spec.total_fee_budget_atoms)?
    {
        return Err(CoreError::Rejected("order attempt or fee budget accounting is inconsistent".into()));
    }
    Ok(value)
}
fn persist(session: &Session, plan: &mut TradePlan, record: &Record, reason: &str) -> Result<()> {
    plan.intent["order"] = serde_json::to_value(record)?;
    plan.reason = reason.into();
    plan.state = match record.state {
        State::Complete => PlanState::Complete,
        State::Cancelled => PlanState::Cancelled,
        State::Waiting => PlanState::Waiting,
        State::Review | State::Triggered => PlanState::Review,
        _ => PlanState::Paused,
    };
    session.app.save_trade_plan(plan)
}

impl Spec {
    fn validate(&self) -> Result<()> {
        crate::chain::addr(&self.account)?;
        for token in [&self.from, &self.to] {
            if token != "quai" {
                crate::chain::addr(token)?;
            }
        }
        if self.from.eq_ignore_ascii_case(&self.to) || self.venue == Venue::Curve || !(1..=8).contains(&self.max_attempts) {
            return Err(CoreError::Invalid("invalid order assets, venue or attempt bound".into()));
        }
        crate::swap::validate_slippage(self.slippage_bps)?;
        for value in [&self.input_atoms, &self.minimum_output_atoms, &self.maximum_fee_atoms, &self.total_fee_budget_atoms] {
            if atoms(value)?.is_zero() {
                return Err(CoreError::Invalid("order bounds must be positive".into()));
            }
        }
        if atoms(&self.maximum_fee_atoms)? > atoms(&self.total_fee_budget_atoms)? {
            return Err(CoreError::Invalid("total fee budget is smaller than one attempt".into()));
        }
        if let Some(target) = &self.target_output_atoms
            && atoms(target)?.is_zero()
        {
            return Err(CoreError::Invalid("order target must be positive".into()));
        }
        if [&self.from_symbol, &self.to_symbol].into_iter().flatten().any(|s| s.chars().count() > 32) {
            return Err(CoreError::Invalid("order symbol is too long".into()));
        }
        Ok(())
    }
    fn context(&self, session: &Session) -> Result<()> {
        if self.network != session.network.id
            || self.chain_id != session.network.chain_id
            || !self.genesis.eq_ignore_ascii_case(&session.network.genesis)
        {
            return Err(CoreError::Rejected("order belongs to its original network and genesis".into()));
        }
        let account = session.account(Some(&self.account))?;
        if !account.address.eq_ignore_ascii_case(&self.account) {
            return Err(CoreError::Rejected("order account changed".into()));
        }
        let (router, factory) = crate::swap::venue_pins(&session.network, self.venue)
            .ok_or_else(|| CoreError::Rejected("order venue is no longer configured".into()))?;
        if !router.address.eq_ignore_ascii_case(&self.router)
            || router.code_hash.as_deref() != Some(&self.router_hash)
            || factory.code_hash.as_deref() != Some(&self.factory_hash)
        {
            return Err(CoreError::Rejected("order's pinned venue configuration changed".into()));
        }
        Ok(())
    }
    fn matches_quote(&self, quote: &SwapQuote, now: u64) -> Result<bool> {
        if quote.observed_at > now || now.saturating_sub(quote.observed_at) > MAX_QUOTE_AGE {
            return Err(CoreError::Rejected("order quote is stale or future-dated".into()));
        }
        if identity(&quote.from) != self.from
            || identity(&quote.to) != self.to
            || quote.from.decimals() != self.input_decimals
            || quote.to.decimals() != self.output_decimals
            || atoms(&quote.amount_in)? != atoms(&self.input_atoms)?
        {
            return Err(CoreError::Rejected("order token identity, units or input changed".into()));
        }
        if !quote.router.eq_ignore_ascii_case(&self.router) || quote.legs.len() > 1 || quote.insufficient {
            return Ok(false);
        }
        Ok(atoms(&quote.minimum_out)? >= atoms(&self.minimum_output_atoms)?)
    }
    fn check_operation(&self, op: &Operation, now: u64) -> Result<()> {
        if now.saturating_sub(op.created) > 120
            || op.created > now
            || now >= self.expires_at
            || op.network != self.network
            || !op.account.eq_ignore_ascii_case(&self.account)
            || (atoms(&op.amount)? != atoms(&self.input_atoms)? && !(op.kind == OpKind::Approve && atoms(&op.amount)?.is_zero()))
            || atoms(&op.fee)? > atoms(&self.maximum_fee_atoms)?
        {
            return Err(CoreError::Rejected("order expired or prepared owner/input/fee exceeds its authorization".into()));
        }
        let text = |v: &serde_json::Value| v.as_str().unwrap_or_default().to_string();
        if op.kind == OpKind::Approve {
            if op.detail.decimals().as_u64() != Some(u64::from(self.input_decimals))
                || !text(op.detail.token()).eq_ignore_ascii_case(&self.from)
                || !text(op.detail.spender()).eq_ignore_ascii_case(&self.router)
            {
                return Err(CoreError::Rejected("order approval token or spender changed".into()));
            }
        } else if op.kind == OpKind::Swap {
            if op.detail.decimals().as_u64() != Some(u64::from(self.input_decimals))
                || op.detail.to_decimals().as_u64() != Some(u64::from(self.output_decimals))
                || !op.counterparty.eq_ignore_ascii_case(&self.router)
                || !text(op.detail.router()).eq_ignore_ascii_case(&self.router)
                || !text(op.detail.recipient()).eq_ignore_ascii_case(&self.account)
                || !text(op.detail.from_token()).eq_ignore_ascii_case(&self.from)
                || !text(op.detail.to_token()).eq_ignore_ascii_case(&self.to)
                || atoms(&text(op.detail.minimum_out()))? < atoms(&self.minimum_output_atoms)?
                || op.detail.expires_at().as_u64().is_none_or(|deadline| deadline > self.expires_at || deadline <= now)
            {
                return Err(CoreError::Rejected("frozen swap does not preserve the order's venue, recipient, limit or expiry".into()));
            }
        } else {
            return Err(CoreError::Rejected("order permits only an exact approval and one atomic swap".into()));
        }
        Ok(())
    }
}

impl Record {
    /// Whether this check is the one that says the order is reachable: the first that finds it
    /// so. It stamps the order, so no later check (here or in another process) says it again,
    /// even after the price has wandered away and back.
    fn mark_reachable(&mut self, triggered: bool, now: u64) -> bool {
        let first = triggered && self.notified_at.is_none();
        if first {
            self.notified_at = Some(now);
        }
        first
    }

    fn charge(&mut self, now: u64) -> Result<()> {
        if now >= self.spec.expires_at || self.attempts.len() >= usize::from(self.spec.max_attempts) {
            return Err(CoreError::Rejected("order expired or attempt budget exhausted".into()));
        }
        let total = atoms(&self.fee_budget_used_atoms)?
            .checked_add(atoms(&self.spec.maximum_fee_atoms)?)
            .ok_or_else(|| CoreError::Invalid("order fee budget overflow".into()))?;
        if total > atoms(&self.spec.total_fee_budget_atoms)? {
            return Err(CoreError::Rejected("order fee budget exhausted".into()));
        }
        self.fee_budget_used_atoms = total.to_string();
        self.attempts.push(Attempt { at: now, operation: None, reserved_fee_atoms: self.spec.maximum_fee_atoms.clone() });
        self.state = State::Review;
        Ok(())
    }
}

pub async fn create(session: &mut Session, request: Create) -> Result<TradePlan> {
    if request.expires_at <= crate::registry::now() + 60 {
        return Err(CoreError::Invalid("order expiry must be more than one minute away".into()));
    }
    let account = session.account(request.account.as_deref())?.address;
    let q = session.swap_quote(Some(&account), &request.from, &request.to, &request.input, request.slippage_bps, Trust::FirstHand).await?;
    if q.legs.len() > 1 {
        return Err(CoreError::Rejected("limit orders require an atomic route".into()));
    }
    let venue = [Venue::Main, Venue::LaunchAmm, Venue::Legacy, Venue::HartiiAmm]
        .into_iter()
        .find(|venue| crate::swap::venue_pins(&session.network, *venue).is_some_and(|(r, _)| r.address.eq_ignore_ascii_case(&q.router)))
        .ok_or_else(|| CoreError::Rejected("order router is not a configured venue".into()))?;
    let (router, factory) = crate::swap::venue_pins(&session.network, venue).unwrap();
    // A target sets the minimum (less slippage); without one, the minimum is as typed.
    let target = request.target_output.as_deref().map(|t| target_from_text(t, atoms(&q.amount_out)?, q.to.decimals())).transpose()?;
    let minimum = match target {
        Some(t) => minimum_for_target(t, request.slippage_bps),
        None => crate::amount::parse_amount(&request.minimum_output, q.to.decimals())?,
    };
    let symbol = |s: &str| Some(crate::explorer::clean_text(s).chars().take(32).collect::<String>()).filter(|s| !s.is_empty());
    let spec = Spec {
        network: session.network.id.clone(),
        chain_id: session.network.chain_id,
        genesis: session.network.genesis.clone(),
        account: account.clone(),
        from: identity(&q.from),
        to: identity(&q.to),
        input_decimals: q.from.decimals(),
        output_decimals: q.to.decimals(),
        input_atoms: q.amount_in,
        minimum_output_atoms: minimum.to_string(),
        venue,
        router: q.router,
        router_hash: router.code_hash.clone().ok_or_else(|| CoreError::Rejected("orders require pinned routers".into()))?,
        factory_hash: factory.code_hash.clone().ok_or_else(|| CoreError::Rejected("orders require pinned factories".into()))?,
        slippage_bps: request.slippage_bps,
        expires_at: request.expires_at,
        maximum_fee_atoms: crate::amount::parse_quai(&request.maximum_fee)?.to_string(),
        total_fee_budget_atoms: crate::amount::parse_quai(&request.total_fee_budget)?.to_string(),
        max_attempts: request.max_attempts,
        mode: request.mode,
        target_output_atoms: target.map(|t| t.to_string()),
        from_symbol: symbol(q.from.symbol()),
        to_symbol: symbol(q.to.symbol()),
    };
    spec.validate()?;
    let value = Record {
        version: 1,
        spec,
        state: State::Armed,
        attempts: vec![],
        fee_budget_used_atoms: "0".into(),
        last_observed_at: None,
        last_minimum_output_atoms: None,
        last_expected_output_atoms: Some(q.amount_out.clone()),
        notified_at: None,
    };
    let mut plan = TradePlan::new(session.network.id.clone(), account, "limit trigger".into(), json!({"client":CLIENT,"order":value}))?;
    let _lease = lease(session, &plan.id)?;
    persist(session, &mut plan, &value, "armed; only an unlocked client can prepare a fresh review; the daemon never signs")?;
    Ok(plan)
}

pub fn load(session: &Session, id: &str) -> Result<TradePlan> {
    let plan = session.app.trade_plan(id)?.ok_or_else(|| CoreError::NotFound("limit order".into()))?;
    let value = record(&plan)?;
    value.spec.context(session)?;
    Ok(plan)
}
pub fn details(plan: &TradePlan) -> Result<Record> {
    record(plan)
}
pub fn list(session: &Session) -> Result<Vec<TradePlan>> {
    Ok(session.app.trade_plans(&session.network.id)?.into_iter().filter(|p| p.intent["client"] == CLIENT).collect())
}

/// Repair the crash gap from the operation's durable plan_id, never from the last visible row.
fn reconcile(session: &Session, plan: &mut TradePlan, value: &mut Record) -> Result<bool> {
    let mut ready = true;
    for id in &plan.operations {
        let op = session.app.operation(id)?.ok_or_else(|| CoreError::Rejected("order operation history is missing".into()))?;
        if op.network != plan.network || op.detail.plan_id().as_str() != Some(plan.id.as_str()) {
            return Err(CoreError::Rejected("order operation history belongs to another plan".into()));
        }
    }
    for op in session.app.operations_for_plan(&plan.network, &plan.id)? {
        if !plan.operations.contains(&op.id) {
            let attempt = value
                .attempts
                .iter_mut()
                .rev()
                .find(|a| a.operation.is_none())
                .ok_or_else(|| CoreError::Rejected("unbudgeted operation attached to order".into()))?;
            attempt.operation = Some(op.id.clone());
            plan.operations.push(op.id.clone());
        }
        match op.status {
            OpStatus::Cancelled if op.tx_hash.is_none() => {}
            OpStatus::Confirmed | OpStatus::Settled if op.kind == OpKind::Approve => {}
            OpStatus::Confirmed | OpStatus::Settled => {
                value.state = State::Complete;
                ready = false;
            }
            OpStatus::Failed | OpStatus::Refunded | OpStatus::Replaced => {
                value.state = State::Stopped;
                ready = false;
            }
            _ => {
                value.state = State::Waiting;
                ready = false;
            }
        }
    }
    Ok(ready)
}
async fn quote(session: &mut Session, spec: &Spec) -> Result<SwapQuote> {
    let input = crate::amount::format_amount(atoms(&spec.input_atoms)?, spec.input_decimals);
    session.swap_quote(Some(&spec.account), &spec.from, &spec.to, &input, spec.slippage_bps, Trust::FirstHand).await
}

pub async fn observe(session: &mut Session, id: &str) -> Result<Observation> {
    let _lease = lease(session, id)?;
    let mut plan = load(session, id)?;
    let mut value = record(&plan)?;
    let now = crate::registry::now();
    if value.state == State::Cancelled {
        return Ok(observation(&plan, &value, false));
    }
    if now >= value.spec.expires_at {
        value.state = State::Expired;
        persist(session, &mut plan, &value, "order expired; existing transactions remain tracked")?;
        return Ok(observation(&plan, &value, false));
    }
    if !reconcile(session, &mut plan, &mut value)? {
        persist(session, &mut plan, &value, "reconcile the existing operation; no duplicate execution")?;
        return Ok(observation(&plan, &value, false));
    }
    if matches!(value.state, State::Stopped | State::Complete) {
        return Ok(observation(&plan, &value, false));
    }
    let q = quote(session, &value.spec).await?;
    let triggered = value.spec.matches_quote(&q, crate::registry::now())?;
    value.state = if triggered { State::Triggered } else { State::Armed };
    value.last_observed_at = Some(q.observed_at);
    value.last_minimum_output_atoms = Some(q.minimum_out);
    value.last_expected_output_atoms = Some(q.amount_out);
    let announce = value.mark_reachable(triggered, now);
    let why = if triggered {
        "limit reached; awaiting an unlocked client and a fresh authorized review"
    } else {
        "waiting for limit, sufficient balance and the originally pinned router to remain the chosen route"
    };
    persist(session, &mut plan, &value, why)?;
    // Written once the stamp is saved, so a crash in between loses a notice rather than
    // repeating one. The daemon puts this wallet's new notifications on the desktop.
    if announce {
        let (title, body) = reachable_notice(&value);
        let _ = session.app.notify("alert", &title, &body);
    }
    let mut seen = observation(&plan, &value, triggered);
    seen.announced = announce;
    Ok(seen)
}

/// What a reachable order says: which trade, and where to sign it.
pub fn reachable_notice(value: &Record) -> (String, String) {
    let rows = describe(value, crate::registry::now());
    let get = |k: &str| rows.iter().find(|(l, _)| *l == k).map(|(_, t)| t.clone()).unwrap_or_default();
    let target = get("waits for").split(" (").next().unwrap_or_default().to_string();
    (
        "Limit order reachable".into(),
        format!("{}: {target} is available now. Open Quai Terminal › Trade › Orders to review and sign.", get("trade")),
    )
}
fn observation(plan: &TradePlan, value: &Record, triggered: bool) -> Observation {
    Observation {
        id: plan.id.clone(),
        state: value.state,
        triggered,
        minimum_output_atoms: value.last_minimum_output_atoms.clone(),
        expected_output_atoms: value.last_expected_output_atoms.clone(),
        observed_at: value.last_observed_at,
        reason: plan.reason.clone(),
        announced: false,
    }
}

/// Charge authorization budget durably before preparation; crashes/declines never reset it.
/// Returns at most one operation. Approval inclusion must be tracked before another call.
pub async fn prepare(session: &mut Session, id: &str) -> Result<Option<Review>> {
    if !session.is_unlocked() {
        return Err(CoreError::Locked("an order waits for an unlocked signing client".into()));
    }
    let _lease = lease(session, id)?;
    let mut plan = load(session, id)?;
    let mut value = record(&plan)?;
    if matches!(value.state, State::Cancelled | State::Expired | State::Stopped | State::Complete) {
        return Err(CoreError::Rejected("order is stopped".into()));
    }
    if !reconcile(session, &mut plan, &mut value)? {
        persist(session, &mut plan, &value, "existing order operation must be tracked before another attempt")?;
        return Ok(None);
    }
    let q = quote(session, &value.spec).await?;
    if !value.spec.matches_quote(&q, crate::registry::now())? {
        value.state = State::Armed;
        persist(session, &mut plan, &value, "fresh quote no longer meets the limit, balance or captured router")?;
        return Ok(None);
    }
    let now = crate::registry::now();
    let deadline = value.spec.expires_at.saturating_sub(now + 30) / 60;
    if deadline == 0 {
        return Err(CoreError::Rejected("order expiry leaves too little time for fresh preparation".into()));
    }
    value.charge(now)?;
    persist(session, &mut plan, &value, "attempt and maximum fee authorization consumed before preparation")?;
    let intent = TradingIntent {
        account: value.spec.account.clone(),
        max_fee: Some(crate::amount::quai(atoms(&value.spec.maximum_fee_atoms)?)),
        action: TradingAction::Swap {
            from: value.spec.from.clone(),
            to: value.spec.to.clone(),
            amount: crate::amount::format_amount(atoms(&value.spec.input_atoms)?, value.spec.input_decimals),
            slippage: value.spec.slippage_bps,
            deadline: deadline.min(30) as u32,
        },
    };
    session.preparing_plan = Some(plan.id.clone());
    let result = intent.next_review(session).await;
    session.preparing_plan = None;
    let review = match result {
        Ok(r) => r,
        Err(e) => {
            persist(session, &mut plan, &value, "preparation failed; consumed attempt budget is retained")?;
            return Err(e);
        }
    };
    let op = session.app.operation(&review.op_id)?.ok_or_else(|| CoreError::NotFound("order review operation".into()))?;
    if let Err(e) = value.spec.check_operation(&op, crate::registry::now()) {
        let _ = session.discard(&review.op_id);
        return Err(e);
    }
    value.attempts.last_mut().unwrap().operation = Some(review.op_id.clone());
    plan.operations.push(review.op_id.clone());
    if let Err(e) = persist(session, &mut plan, &value, "fresh review ready; cancellation can still prevent signing") {
        let _ = session.discard(&review.op_id);
        return Err(e);
    }
    Ok(Some(review))
}

/// Commit hook: hold this guard through signing/broadcast. Cancellation uses the same lease,
/// so it either prevents signing or reports that another client is advancing the order.
pub fn submission_guard(session: &Session, op: &Operation) -> Result<Option<std::fs::File>> {
    let Some(id) = op.detail.plan_id().as_str() else { return Ok(None) };
    let Some(candidate) = session.app.trade_plan(id)? else {
        return Err(CoreError::Rejected("operation plan is missing".into()));
    };
    if candidate.intent["client"] != CLIENT {
        return Ok(None);
    }
    let guard = lease(session, id)?;
    let current = session.app.operation(&op.id)?.ok_or_else(|| CoreError::NotFound("order operation".into()))?;
    if current.status != OpStatus::Prepared || current.tx_hash.is_some() {
        return Err(CoreError::Rejected("order operation was cancelled or already signed".into()));
    }
    let mut plan = load(session, id)?;
    let mut value = record(&plan)?;
    if value.state != State::Review || !value.attempts.iter().any(|a| a.operation.as_deref() == Some(&op.id)) {
        return Err(CoreError::Rejected("order is cancelled, stopped or lacks a durably budgeted attempt".into()));
    }
    // Journal insertion adds timeline metadata after the frozen pending operation was built.
    // Compare the review payload, while allowing that bookkeeping-only field.
    let mut current_detail = current.detail.clone();
    let mut reviewed_detail = op.detail.clone();
    current_detail.take_timeline();
    reviewed_detail.take_timeline();
    if current_detail != reviewed_detail || current.account != op.account || current.amount != op.amount || current.fee != op.fee {
        return Err(CoreError::Rejected("order review changed after it was displayed".into()));
    }
    value.spec.check_operation(&current, crate::registry::now())?;
    value.state = State::Waiting;
    persist(session, &mut plan, &value, "authorized attempt may be signed; reconcile its exact candidate before retrying")?;
    Ok(Some(guard))
}

pub fn cancel(session: &mut Session, id: &str) -> Result<TradePlan> {
    let _lease = lease(session, id)?;
    let mut plan = load(session, id)?;
    let mut value = record(&plan)?;
    value.state = State::Cancelled;
    persist(session, &mut plan, &value, "future execution cancelled; signed or broadcast transactions cannot be undone by this action")?;
    for op in session.app.operations_for_plan(&plan.network, &plan.id)? {
        if op.status == OpStatus::Prepared && op.tx_hash.is_none() {
            session.abandon(&op.id)?;
        }
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn spec() -> Spec {
        let network = crate::network::NetworkProfile::builtins().remove(0);
        let (router, factory) = crate::swap::venue_pins(&network, Venue::Main).unwrap();
        Spec {
            network: network.id.clone(),
            chain_id: network.chain_id,
            genesis: network.genesis.clone(),
            account: "0x0000000000000000000000000000000000000001".into(),
            from: "quai".into(),
            to: "0x0000000000000000000000000000000000000002".into(),
            input_decimals: 18,
            output_decimals: 6,
            input_atoms: "100".into(),
            minimum_output_atoms: "200".into(),
            venue: Venue::Main,
            router: router.address.clone(),
            router_hash: router.code_hash.clone().unwrap(),
            factory_hash: factory.code_hash.clone().unwrap(),
            slippage_bps: 50,
            expires_at: 1000,
            maximum_fee_atoms: "10".into(),
            total_fee_budget_atoms: "20".into(),
            max_attempts: 3,
            mode: Mode::ExecuteOnce,
            target_output_atoms: None,
            from_symbol: None,
            to_symbol: None,
        }
    }
    fn value(spec: Spec) -> Record {
        Record {
            version: 1,
            spec,
            state: State::Armed,
            attempts: vec![],
            fee_budget_used_atoms: "0".into(),
            last_observed_at: None,
            last_minimum_output_atoms: None,
            last_expected_output_atoms: None,
            notified_at: None,
        }
    }

    /// `+5%` waits for 5% more back than now, a plain number is an amount, and the minimum a
    /// target guarantees is exactly what the trigger compares a quote's minimum against.
    #[test]
    fn a_target_is_a_percentage_or_an_amount_and_meets_the_trigger_exactly() {
        let now = U256::from(1_000_000u64);
        assert_eq!(target_from_text("+5%", now, 6).unwrap(), U256::from(1_050_000u64));
        assert_eq!(target_from_text("5%", now, 6).unwrap(), U256::from(1_050_000u64));
        assert_eq!(target_from_text("-2.5%", now, 6).unwrap(), U256::from(975_000u64));
        assert_eq!(target_from_text("1,234.5", now, 6).unwrap(), U256::from(1_234_500_000u64));
        for bad in ["", "abc", "-100%", "0", "+x%"] {
            assert!(target_from_text(bad, now, 6).is_err(), "{bad}");
        }
        // The real trigger rule: a quote whose expected output reaches the target triggers, one
        // that falls short does not, at any slippage.
        for (target, slippage) in [(1_050_000u64, 50u16), (999_999, 100), (20_000, 1)] {
            let mut order = spec();
            order.slippage_bps = slippage;
            order.minimum_output_atoms = minimum_for_target(U256::from(target), slippage).to_string();
            let at = |out: u64| {
                let mut q = quote(&order);
                q.slippage_bps = slippage;
                q.amount_out = out.to_string();
                q.minimum_out = crate::swap::minimum_out(U256::from(out), slippage).to_string();
                order.matches_quote(&q, 100).unwrap()
            };
            assert!(at(target), "{target} at {slippage} bps: the target itself triggers");
            assert!(at(target + target / 100), "better than the target triggers");
            assert!(!at(target - target / 100), "1% short does not");
        }
        assert_eq!(distance_bps(now, U256::from(1_050_000u64)), Some(500));
        assert_eq!(distance_bps(now, U256::from(990_000u64)), Some(-100));
        assert_eq!(distance_bps(U256::ZERO, now), None);
        assert_eq!(State::Triggered.label(), "ready to sign");
        assert!(State::Armed.active() && !State::Complete.active());
    }

    /// Reachable is said once per order: the first check that finds it, and never again after
    /// the price wanders away and back. The notice names the trade and where to sign it.
    #[test]
    fn a_reachable_order_is_announced_once() {
        let mut spec = spec();
        spec.from_symbol = Some("QUAI".into());
        spec.to_symbol = Some("USD".into());
        spec.target_output_atoms = Some("202".into());
        let mut v = value(spec);
        assert!(!v.mark_reachable(false, 10), "not reachable: nothing to say");
        assert!(v.mark_reachable(true, 20), "the first reachable check says it");
        assert_eq!(v.notified_at, Some(20));
        assert!(!v.mark_reachable(true, 30), "the next check does not repeat it");
        assert!(!v.mark_reachable(false, 40));
        assert!(!v.mark_reachable(true, 50), "nor does reaching it again later");
        // It survives a save and a reload (another process reading the same record).
        let again: Record = serde_json::from_value(serde_json::to_value(&v).unwrap()).unwrap();
        assert_eq!(again.notified_at, Some(20));
        v.last_expected_output_atoms = Some("202".into());
        let (title, body) = reachable_notice(&v);
        assert_eq!(title, "Limit order reachable");
        assert!(body.contains("QUAI → USD: 0.000202 USD back is available now"), "{body}");
        assert!(body.contains("Trade › Orders"), "{body}");
    }

    /// Orders saved before targets and symbols existed still load, and still validate.
    #[test]
    fn an_order_saved_without_a_target_still_loads() {
        let mut old = serde_json::to_value(value(spec())).unwrap();
        for field in ["target_output_atoms", "from_symbol", "to_symbol"] {
            assert!(old["spec"].as_object_mut().unwrap().remove(field).is_none(), "{field} is not written when unset");
        }
        old.as_object_mut().unwrap().remove("last_expected_output_atoms");
        let back: Record = serde_json::from_value(old).unwrap();
        assert!(back.spec.target_output_atoms.is_none());
        back.spec.validate().unwrap();
        let mut zero = spec();
        zero.target_output_atoms = Some("0".into());
        assert!(zero.validate().is_err());
    }
    fn op(spec: &Spec, id: &str, plan: &str, now: u64) -> Operation {
        Operation {
            id: id.into(),
            network: spec.network.clone(),
            kind: OpKind::Swap,
            store: "quai".into(),
            account: spec.account.clone(),
            status: OpStatus::Prepared,
            tx_hash: None,
            asset: "QUAI".into(),
            amount: spec.input_atoms.clone(),
            counterparty: spec.router.clone(),
            fee: "9".into(),
            detail: json!({"plan_id":plan,"router":spec.router,"recipient":spec.account,"from_token":spec.from,"to_token":spec.to,"decimals":18,"to_decimals":6,"minimum_out":"200","expires_at":spec.expires_at}).into(),
            created: now,
            updated: now,
        }
    }
    fn quote(spec: &Spec) -> SwapQuote {
        SwapQuote {
            from: SwapAsset::Quai,
            to: SwapAsset::Token { address: spec.to.clone(), symbol: "USD".into(), decimals: 6 },
            amount_in: "100".into(),
            amount_out: "202".into(),
            minimum_out: "200".into(),
            slippage_bps: 50,
            path: vec![],
            route: vec![],
            pools: vec![],
            impact_bps: 0,
            fee_bps: 30,
            router: spec.router.clone(),
            allowance: None,
            approval_needed: false,
            balance: Some("1000".into()),
            insufficient: false,
            warnings: vec![],
            observed_at: 100,
            liquidity_at: None,
            legs: vec![],
        }
    }
    #[test]
    fn attempts_and_fee_authorization_survive_restart_and_cannot_be_reset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("orders.sqlite");
        let db = crate::appdb::AppDb::open(&path).unwrap();
        let mut v = value(spec());
        v.charge(100).unwrap();
        let mut plan =
            TradePlan::new(v.spec.network.clone(), v.spec.account.clone(), "order".into(), json!({"client":CLIENT,"order":v})).unwrap();
        db.save_trade_plan(&mut plan).unwrap();
        drop(db);
        let db = crate::appdb::AppDb::open(&path).unwrap();
        let plan = db.trade_plan(&plan.id).unwrap().unwrap();
        let mut v = record(&plan).unwrap();
        assert_eq!(v.attempts.len(), 1);
        assert_eq!(v.fee_budget_used_atoms, "10");
        v.charge(101).unwrap();
        assert!(v.charge(102).is_err(), "fee budget is exhausted before attempt count");
        let mut corrupted = plan;
        corrupted.intent["order"]["fee_budget_used_atoms"] = json!("0");
        assert!(record(&corrupted).is_err());
    }
    #[test]
    fn stale_future_wrong_units_and_changed_router_cannot_trigger_execution() {
        let s = spec();
        let mut q = quote(&s);
        assert!(s.matches_quote(&q, 100).unwrap());
        assert!(s.matches_quote(&q, 131).is_err());
        assert!(s.matches_quote(&q, 99).is_err());
        q.minimum_out = "199".into();
        assert!(!s.matches_quote(&q, 100).unwrap());
        q.minimum_out = "200".into();
        q.router = "another router".into();
        assert!(!s.matches_quote(&q, 100).unwrap());
        q.router = s.router.clone();
        q.to = SwapAsset::Token { address: s.to.clone(), symbol: "USD".into(), decimals: 18 };
        assert!(s.matches_quote(&q, 100).is_err());
    }
    #[test]
    fn frozen_operation_enforces_price_fee_recipient_deadline_and_freshness() {
        let s = spec();
        let base = op(&s, "a", "b", 100);
        assert!(s.check_operation(&base, 100).is_ok());
        for (key, value) in [
            ("minimum_out", json!("199")),
            ("recipient", json!("another")),
            ("expires_at", json!(1001)),
            ("to_decimals", json!(18)),
            ("router", json!("another")),
        ] {
            let mut bad = base.clone();
            bad.detail.test_set(key, value);
            assert!(s.check_operation(&bad, 100).is_err(), "{key}");
        }
        let mut bad = base.clone();
        bad.fee = "11".into();
        assert!(s.check_operation(&bad, 100).is_err());
        assert!(s.check_operation(&base, 221).is_err());
        assert!(s.check_operation(&base, 1000).is_err());
    }
    fn session() -> (tempfile::TempDir, Session) {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::paths::Paths::resolve(Some(dir.path().to_path_buf())).unwrap();
        let registry = crate::registry::Registry::fast(paths);
        let meta = registry
            .create_hd(
                "orders",
                "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
                "english",
                "",
                "password123",
                true,
            )
            .unwrap();
        let session =
            Session::open(registry, crate::config::AppConfig::default(), meta, crate::network::NetworkProfile::builtins().remove(0))
                .unwrap();
        (dir, session)
    }
    #[test]
    fn submission_and_cancellation_serialize_and_preserve_signed_history() {
        let (_dir, mut session) = session();
        let now = crate::registry::now();
        let mut s = spec();
        s.account = session.account(None).unwrap().address;
        s.expires_at = now + 600;
        let mut v = value(s);
        v.charge(now).unwrap();
        let mut plan =
            TradePlan::new(v.spec.network.clone(), v.spec.account.clone(), "order".into(), json!({"client":CLIENT,"order":v})).unwrap();
        let operation = op(&v.spec, "00000000000000000000000000000001", &plan.id, now);
        v.attempts[0].operation = Some(operation.id.clone());
        plan.operations.push(operation.id.clone());
        session
            .quai_store
            .reserve_nonce(crate::session::parse_op_id(&operation.id).unwrap(), crate::chain::addr(&operation.account).unwrap(), 0)
            .unwrap();
        session.app.insert_operation(&operation).unwrap();
        persist(&session, &mut plan, &v, "review").unwrap();
        let guard = submission_guard(&session, &operation).unwrap().unwrap();
        assert!(cancel(&mut session, &plan.id).is_err(), "submission lease blocks cancellation from claiming to undo broadcast");
        drop(guard);
        cancel(&mut session, &plan.id).unwrap();
        assert!(submission_guard(&session, &operation).is_err());
        assert_eq!(session.app.operation(&operation.id).unwrap().unwrap().status, OpStatus::Cancelled);
    }
    #[test]
    fn cancellation_preserves_an_already_signed_candidate() {
        let (_dir, mut session) = session();
        let now = crate::registry::now();
        let mut s = spec();
        s.account = session.account(None).unwrap().address;
        s.expires_at = now + 600;
        let mut v = value(s);
        v.charge(now).unwrap();
        let mut plan =
            TradePlan::new(v.spec.network.clone(), v.spec.account.clone(), "order".into(), json!({"client":CLIENT,"order":v})).unwrap();
        let mut operation = op(&v.spec, "00000000000000000000000000000003", &plan.id, now);
        operation.status = OpStatus::Signed;
        operation.tx_hash = Some("0xknown-candidate".into());
        v.attempts[0].operation = Some(operation.id.clone());
        plan.operations.push(operation.id.clone());
        session.app.insert_operation(&operation).unwrap();
        persist(&session, &mut plan, &v, "signed").unwrap();
        cancel(&mut session, &plan.id).unwrap();
        let tracked = session.app.operation(&operation.id).unwrap().unwrap();
        assert_eq!(tracked.status, OpStatus::Signed);
        assert_eq!(tracked.tx_hash, operation.tx_hash);
        assert!(submission_guard(&session, &operation).is_err());
    }

    #[test]
    fn crash_between_preparation_and_plan_link_recovers_exact_operation_without_new_budget() {
        let (_dir, mut session) = session();
        let now = crate::registry::now();
        let mut s = spec();
        s.account = session.account(None).unwrap().address;
        s.expires_at = now + 600;
        let mut v = value(s);
        v.charge(now).unwrap();
        let mut plan =
            TradePlan::new(v.spec.network.clone(), v.spec.account.clone(), "order".into(), json!({"client":CLIENT,"order":v})).unwrap();
        persist(&session, &mut plan, &v, "budgeted").unwrap();
        let operation = op(&v.spec, "00000000000000000000000000000002", &plan.id, now);
        session.app.insert_operation(&operation).unwrap();
        assert!(!reconcile(&session, &mut plan, &mut v).unwrap());
        assert_eq!(v.attempts.len(), 1);
        assert_eq!(v.attempts[0].operation.as_deref(), Some(operation.id.as_str()));
        assert_eq!(v.fee_budget_used_atoms, "10");
        assert!(!reconcile(&session, &mut plan, &mut v).unwrap());
        assert_eq!(plan.operations.len(), 1, "duplicate refresh cannot create another execution");
        session.network.genesis = "different genesis".into();
        assert!(v.spec.context(&session).is_err());
        let mut tampered = plan.clone();
        tampered.owner = "0x0000000000000000000000000000000000000003".into();
        assert!(record(&tampered).is_err());
    }
    #[tokio::test]
    async fn locked_client_cannot_prepare_even_a_triggered_order() {
        let (_dir, mut session) = session();
        assert!(matches!(prepare(&mut session, "missing").await, Err(CoreError::Locked(_))));
    }
}
