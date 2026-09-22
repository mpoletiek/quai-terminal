//! Durable client-side limit triggers. No daemon receives signing authority. A fixed input and
//! minimum output define the limit price; each signing client re-quotes and checks frozen bounds.
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
}

#[derive(Clone, Debug)]
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
}

#[derive(Clone, Debug, Serialize)]
pub struct Observation {
    pub id: String,
    pub state: State,
    pub triggered: bool,
    pub minimum_output_atoms: Option<String>,
    pub observed_at: Option<u64>,
    pub reason: String,
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
            || (atoms(&op.amount)? != atoms(&self.input_atoms)? && !(op.kind == "approve" && atoms(&op.amount)?.is_zero()))
            || atoms(&op.fee)? > atoms(&self.maximum_fee_atoms)?
        {
            return Err(CoreError::Rejected("order expired or prepared owner/input/fee exceeds its authorization".into()));
        }
        let text = |key| op.detail[key].as_str().unwrap_or_default();
        if op.kind == "approve" {
            if op.detail["decimals"].as_u64() != Some(u64::from(self.input_decimals))
                || !text("token").eq_ignore_ascii_case(&self.from)
                || !text("spender").eq_ignore_ascii_case(&self.router)
            {
                return Err(CoreError::Rejected("order approval token or spender changed".into()));
            }
        } else if op.kind == "swap" {
            if op.detail["decimals"].as_u64() != Some(u64::from(self.input_decimals))
                || op.detail["to_decimals"].as_u64() != Some(u64::from(self.output_decimals))
                || !op.counterparty.eq_ignore_ascii_case(&self.router)
                || !text("router").eq_ignore_ascii_case(&self.router)
                || !text("recipient").eq_ignore_ascii_case(&self.account)
                || !text("from_token").eq_ignore_ascii_case(&self.from)
                || !text("to_token").eq_ignore_ascii_case(&self.to)
                || atoms(text("minimum_out"))? < atoms(&self.minimum_output_atoms)?
                || op.detail["expires_at"].as_u64().is_none_or(|deadline| deadline > self.expires_at || deadline <= now)
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
        minimum_output_atoms: crate::amount::parse_amount(&request.minimum_output, q.to.decimals())?.to_string(),
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
        if op.network != plan.network || op.detail["plan_id"].as_str() != Some(plan.id.as_str()) {
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
            OpStatus::Confirmed | OpStatus::Settled if op.kind == "approve" => {}
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
    let why = if triggered {
        "limit reached; awaiting an unlocked client and a fresh authorized review"
    } else {
        "waiting for limit, sufficient balance and the originally pinned router to remain the chosen route"
    };
    persist(session, &mut plan, &value, why)?;
    Ok(observation(&plan, &value, triggered))
}
fn observation(plan: &TradePlan, value: &Record, triggered: bool) -> Observation {
    Observation {
        id: plan.id.clone(),
        state: value.state,
        triggered,
        minimum_output_atoms: value.last_minimum_output_atoms.clone(),
        observed_at: value.last_observed_at,
        reason: plan.reason.clone(),
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
    let Some(id) = op.detail["plan_id"].as_str() else { return Ok(None) };
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
    if let Some(detail) = current_detail.as_object_mut() {
        detail.remove(crate::appdb::TIMELINE);
    }
    if let Some(detail) = reviewed_detail.as_object_mut() {
        detail.remove(crate::appdb::TIMELINE);
    }
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
        }
    }
    fn op(spec: &Spec, id: &str, plan: &str, now: u64) -> Operation {
        Operation {
            id: id.into(),
            network: spec.network.clone(),
            kind: "swap".into(),
            store: "quai".into(),
            account: spec.account.clone(),
            status: OpStatus::Prepared,
            tx_hash: None,
            asset: "QUAI".into(),
            amount: spec.input_atoms.clone(),
            counterparty: spec.router.clone(),
            fee: "9".into(),
            detail: json!({"plan_id":plan,"router":spec.router,"recipient":spec.account,"from_token":spec.from,"to_token":spec.to,"decimals":18,"to_decimals":6,"minimum_out":"200","expires_at":spec.expires_at}),
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
            bad.detail[key] = value;
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
