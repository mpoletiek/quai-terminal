//! One runner for every multi-step trade: approvals, a pre-wrap, the action, a second swap, a
//! redemption. The CLI walks it in a loop; the engine walks it on the signing lane and tells its
//! client where the plan stands. Either way each step is its own review, signed only by the user,
//! and the next one is prepared only once the last one's receipt is in the journal.
//!
//! The runner owns a [`Coordinator`] (the durable plan, its lease, and the typed receipts that
//! advance it). What it adds is the waiting: which step it waits on, when that step counts as
//! done (confirmed, or for a Qi wrap, settled), and what it says about it.

use wallet_core::appdb::OpStatus;
use wallet_core::execution::{Coordinator, TradingAction, TradingIntent};
use wallet_core::journal::OpKind;
use wallet_core::plans::PlanState;
use wallet_core::session::Session;
use wallet_core::tx::{Review, Submitted};

/// Where a plan stands.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Phase {
    /// Its next review is being prepared.
    Preparing,
    /// A review is open.
    Reviewing(String),
    /// A step was sent; the next waits for it.
    Waiting(String),
    /// The last step is in; the next review can be prepared when the user can see it.
    Ready,
    /// Finished, with what to say.
    Done(String),
    /// Stopped (a step failed, a review was rejected, preparing failed), with what to say.
    Stopped(String),
}

/// What a client shows of a plan: the stepper and its state.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlanView {
    pub id: String,
    pub label: String,
    pub phase: Phase,
    /// Steps sent so far, in words ("approve", "swap").
    pub done: Vec<String>,
    /// What is known to follow, in words. Approvals are never guessed ahead of time.
    pub ahead: Vec<String>,
    /// The kind of the last step sent.
    pub last: Option<OpKind>,
    /// The step sent was the plan's last: what follows is only its confirmation.
    pub last_step: bool,
}

/// Where a step stands, for the stepper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepState {
    Done,
    Now,
    Next,
}

impl PlanView {
    /// The steps as the stepper draws them: those sent, the one under way (`current`, the step an
    /// open review is for; an approval is only known then), and what is known to follow.
    pub fn stepper(&self, current: Option<&str>) -> Vec<(String, StepState)> {
        let mut out: Vec<(String, StepState)> = self.done.iter().map(|d| (d.clone(), StepState::Done)).collect();
        // What follows, less what was already sent under the same name.
        let mut sent: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for d in &self.done {
            *sent.entry(d.as_str()).or_default() += 1;
        }
        let mut ahead: Vec<String> = Vec::new();
        for a in &self.ahead {
            match sent.get_mut(a.as_str()) {
                Some(n) if *n > 0 => *n -= 1,
                _ => ahead.push(a.clone()),
            }
        }
        if let Some(now) = current {
            if ahead.first().is_some_and(|a| a == now) {
                ahead.remove(0);
            }
            out.push((now.to_string(), StepState::Now));
            out.extend(ahead.into_iter().map(|a| (a, StepState::Next)));
        } else {
            for (i, a) in ahead.into_iter().enumerate() {
                out.push((a, if i == 0 { StepState::Now } else { StepState::Next }));
            }
        }
        out
    }

    /// Whether the plan is still going (not done, not stopped).
    pub fn active(&self) -> bool {
        !matches!(self.phase, Phase::Done(_) | Phase::Stopped(_))
    }
}

/// What a client asks of the plan runner.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PlanCmd {
    /// Start a plan (one runs at a time) and prepare its first review.
    Start { label: String, intent: TradingIntent },
    /// Prepare the next review of a plan that is [`Phase::Ready`], now that the user can see it.
    Next { id: String },
    /// Resume a saved plan (the newest unfinished one when no id is given).
    Resume { id: Option<String> },
    /// Cancel a plan's future steps.
    Cancel { id: String },
}

/// What the runner does next.
pub enum Next {
    /// Show this review.
    Review(Box<Review>),
    /// Nothing yet: the last step is not done (and why, when there is something to say).
    Wait(Option<String>),
    /// The plan is complete.
    Done(String),
    /// The plan stopped: what to say, and the error behind it.
    Stopped { said: String, error: String },
}

/// A plan being walked.
pub struct Runner {
    coordinator: Coordinator,
    label: String,
    phase: Phase,
    done: Vec<String>,
    last: Option<OpKind>,
    /// The step sent was the plan's last: when it confirms, the plan is complete.
    last_step: bool,
}

/// A step, in the words the stepper uses.
pub fn step_name(kind: &OpKind) -> String {
    match kind {
        OpKind::Approve => "approve".into(),
        OpKind::WrapQuai => "wrap QUAI".into(),
        OpKind::UnwrapQuai => "unwrap WQUAI".into(),
        OpKind::WrapQi => "wrap Qi".into(),
        OpKind::UnwrapWqi => "redeem WQI".into(),
        OpKind::ClaimWqi => "claim".into(),
        k if k.as_str().starts_with("nft_buy") => "buy".into(),
        k if k.as_str().starts_with("nft_list") => "list".into(),
        k => k.as_str().replace('_', " "),
    }
}

impl Runner {
    /// Start a plan: it is saved (and leased) before anything is prepared.
    pub fn start(session: &Session, label: &str, intent: TradingIntent) -> wallet_core::Result<Runner> {
        let coordinator = Coordinator::create(session, label, intent)?;
        Ok(Runner::over(coordinator, label))
    }

    /// Resume a saved plan (the newest unfinished one when no id is given). With
    /// `discard_unsigned`, a review it had prepared and never signed is let go, so every
    /// remaining step is reviewed afresh; without, such a review stops it until it is reconciled.
    pub fn resume(session: &mut Session, id: Option<&str>, discard_unsigned: bool) -> wallet_core::Result<Runner> {
        let id = match id {
            Some(id) => id.to_string(),
            None => session
                .app
                .trade_plans(&session.network.id)?
                .into_iter()
                .filter(|p| p.intent["client"] == "trading")
                .find(|p| !matches!(p.state, PlanState::Complete | PlanState::Cancelled))
                .map(|p| p.id)
                .ok_or_else(|| wallet_core::CoreError::NotFound("no unfinished trade remains".into()))?,
        };
        let mut coordinator = Coordinator::resume(session, &id)?;
        if discard_unsigned {
            coordinator.discard_unsigned(session)?;
        }
        let label = coordinator.plan.label.clone();
        let mut runner = Runner::over(coordinator, &label);
        // Whatever it sent before counts as done, and the newest of it is what it waits on.
        for op_id in runner.coordinator.plan.operations.clone() {
            if let Some(op) = session.app.operation(&op_id)?
                && op.tx_hash.is_some()
            {
                runner.done.push(step_name(&op.kind));
                runner.last_step =
                    !wallet_core::flows::is_step_kind(&op.kind) && !runner.intent().is_some_and(|i| i.has_more_allocations());
                runner.last = Some(op.kind);
                runner.phase = Phase::Waiting(op_id);
            }
        }
        if runner.phase == Phase::Preparing {
            runner.phase = Phase::Ready;
        }
        Ok(runner)
    }

    fn over(coordinator: Coordinator, label: &str) -> Runner {
        Runner { coordinator, label: label.to_string(), phase: Phase::Preparing, done: Vec::new(), last: None, last_step: false }
    }

    pub fn id(&self) -> &str {
        &self.coordinator.plan.id
    }

    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    /// Whether this runner has `op_id` open for review.
    pub fn reviewing(&self, op_id: &str) -> bool {
        matches!(&self.phase, Phase::Reviewing(id) if id == op_id)
    }

    /// The saved intent, as it stands.
    fn intent(&self) -> Option<TradingIntent> {
        serde_json::from_value(self.coordinator.plan.intent["intent"].clone()).ok()
    }

    /// What the client shows.
    pub fn view(&self) -> PlanView {
        PlanView {
            id: self.coordinator.plan.id.clone(),
            label: self.label.clone(),
            phase: self.phase.clone(),
            done: self.done.clone(),
            ahead: self.intent().map(|i| ahead(&i.action)).unwrap_or_default(),
            last: self.last.clone(),
            last_step: self.last_step,
        }
    }

    /// Prepare the next review, or say why not. Only when [`Phase::Ready`] or
    /// [`Phase::Preparing`]; the caller refreshes tracking first (the CLI) or has the pending lane
    /// doing it (the engine).
    pub async fn next(&mut self, session: &mut Session) -> Next {
        self.phase = Phase::Preparing;
        match self.coordinator.prepare(session).await {
            Ok(Some(review)) => {
                self.phase = Phase::Reviewing(review.op_id.clone());
                Next::Review(Box::new(review))
            }
            Ok(None) => {
                let said = self.completed_text();
                self.phase = Phase::Done(said.clone());
                Next::Done(said)
            }
            Err(error) => {
                // A plan the coordinator paused or cancelled has stopped; otherwise the receipt it
                // needs is not complete yet (an output not attributed, a deposit not settled).
                if matches!(self.coordinator.plan.state, PlanState::Paused | PlanState::Cancelled) {
                    let said = self.stopped_text(&error.to_string());
                    self.phase = Phase::Stopped(said.clone());
                    Next::Stopped { said, error: error.to_string() }
                } else {
                    let last = self.coordinator.plan.operations.last().cloned().unwrap_or_default();
                    self.phase = Phase::Waiting(last);
                    Next::Wait(Some(error.to_string()))
                }
            }
        }
    }

    /// The review was signed and broadcast.
    pub fn committed(&mut self, session: &Session, sub: &Submitted, kind: &OpKind) -> wallet_core::Result<()> {
        self.coordinator.submitted(session)?;
        self.done.push(step_name(kind));
        self.last = Some(kind.clone());
        self.last_step = !wallet_core::flows::is_step_kind(kind) && !self.intent().is_some_and(|i| i.has_more_allocations());
        self.phase = Phase::Waiting(sub.op_id.clone());
        Ok(())
    }

    /// Whether this step was the plan's last: the CLI returns when it is sent.
    pub fn last_step_sent(&self) -> bool {
        self.last_step
    }

    /// Signing ended without a clear answer (a timeout, an ambiguous broadcast): the step may
    /// have gone out. The plan waits on it, and tracking says which.
    pub fn sent_uncertain(&mut self, op_id: &str) {
        self.phase = Phase::Waiting(op_id.to_string());
    }

    /// Look at the step waited on (a journal read, no network): [`Phase::Ready`] once it is
    /// done, [`Phase::Stopped`] if it failed. Returns whether the phase changed.
    pub fn poll(&mut self, session: &Session) -> bool {
        let Phase::Waiting(op_id) = &self.phase else { return false };
        let Ok(Some(op)) = session.app.operation(op_id) else { return false };
        let before = self.phase.clone();
        match op.status {
            // A Qi wrap is only spent once it settles; a claim cannot come sooner.
            OpStatus::Confirmed if op.kind == OpKind::WrapQi => {}
            OpStatus::Confirmed | OpStatus::Settled => self.phase = Phase::Ready,
            OpStatus::Failed | OpStatus::Cancelled | OpStatus::Refunded | OpStatus::Replaced => {
                let said = self.stopped_text("a step did not confirm");
                let _ = self.coordinator.pause(session, "a step did not confirm");
                self.phase = Phase::Stopped(said);
            }
            _ => {}
        }
        self.phase != before
    }

    /// The review was rejected: nothing further is signed. The plan is paused, so what already
    /// went through can be resumed from.
    pub fn rejected(&mut self, session: &Session) {
        let _ = self.coordinator.pause(session, "review rejected");
        self.phase = Phase::Stopped(format!("{} cancelled; nothing further will be signed", self.label));
    }

    /// Preparing the next review failed (the error is said on its own).
    pub fn failed(&mut self, session: &mut Session, error: &str) {
        if self.coordinator.plan.operations.is_empty() {
            let _ = self.coordinator.cancel(session);
        } else {
            let _ = self.coordinator.pause(session, error);
        }
        self.phase = Phase::Stopped(self.stopped_text(error));
    }

    /// The wallet locked: an open review went with the keys; the step is asked for again after
    /// the unlock.
    pub fn locked(&mut self) {
        if matches!(self.phase, Phase::Reviewing(_) | Phase::Preparing) {
            self.phase = Phase::Ready;
        }
    }

    /// Cancel future steps. What was signed stays signed.
    pub fn cancel(&mut self, session: &mut Session) -> wallet_core::Result<()> {
        self.coordinator.cancel(session)?;
        self.phase = Phase::Stopped(format!("{} cancelled; completed steps and allowances remain", self.label));
        Ok(())
    }

    fn completed_text(&self) -> String {
        match self.intent().map(|i| i.action) {
            Some(TradingAction::MarketConversion { residual_atoms, .. }) if residual_atoms != "0" => {
                format!("market conversion complete; {residual_atoms} WQI atoms remain below one redeemable Qi")
            }
            _ => format!("{} complete", self.label),
        }
    }

    /// Where the money is: what went through before the stop, and that nothing after it was sent.
    fn stopped_text(&self, _why: &str) -> String {
        match self.done.as_slice() {
            [] => format!("{} stopped before anything was sent.", self.label),
            done => format!(
                "{} stopped after {}: {} went through; nothing after it was sent.",
                self.label,
                wallet_core::amount::count(done.len(), "step"),
                done.join(", ")
            ),
        }
    }
}

/// The steps known to follow, from the intent as it stands.
fn ahead(action: &TradingAction) -> Vec<String> {
    use wallet_core::qi_market::Direction;
    let v = |s: &[&str]| s.iter().map(|s| s.to_string()).collect();
    match action {
        TradingAction::SwapThenUnwrap { stage: 0, .. } => v(&["swap", "unwrap WQUAI"]),
        TradingAction::SwapThenUnwrap { .. } => v(&["unwrap WQUAI"]),
        TradingAction::CrossVenue { stage: 0, redeem: true, .. } => v(&["swap", "swap on", "unwrap WQUAI"]),
        TradingAction::CrossVenue { stage: 0, .. } => v(&["swap", "swap on"]),
        TradingAction::CrossVenue { stage: 1, redeem: true, .. } => v(&["swap on", "unwrap WQUAI"]),
        TradingAction::CrossVenue { stage: 1, .. } => v(&["swap on"]),
        TradingAction::CrossVenue { .. } => v(&["unwrap WQUAI"]),
        TradingAction::MarketConversion { direction: Direction::QuaiToQi, stage: 0, .. } => v(&["swap", "redeem WQI"]),
        TradingAction::MarketConversion { direction: Direction::QuaiToQi, .. } => v(&["redeem WQI"]),
        TradingAction::MarketConversion { direction: Direction::QiToQuai, stage: 0, .. } => v(&["wrap Qi", "claim", "swap"]),
        TradingAction::MarketConversion { direction: Direction::QiToQuai, stage: 1, .. } => v(&["claim", "swap"]),
        TradingAction::MarketConversion { .. } => v(&["swap"]),
        TradingAction::Split { plan, index, .. } => (*index..plan.allocations.len()).map(|i| format!("swap {}", i + 1)).collect(),
        TradingAction::NftBuy { .. } => v(&["buy"]),
        TradingAction::NftList { price: None, .. } => v(&["cancel listing"]),
        TradingAction::NftList { .. } => v(&["list"]),
        TradingAction::ClaimWqi => v(&["claim"]),
        TradingAction::AddLiquidity { .. } => v(&["deposit"]),
        TradingAction::RemoveLiquidity { .. } => v(&["withdraw"]),
        TradingAction::Stake { .. } => v(&["stake"]),
        TradingAction::Unstake { .. } => v(&["unstake"]),
        TradingAction::Incentivize { .. } => v(&["fund rewards"]),
        TradingAction::CurveSell { .. } => v(&["sell"]),
        TradingAction::ProtocolConversion { .. } => v(&["convert"]),
        TradingAction::Swap { .. } | TradingAction::BoundedSwap { .. } | TradingAction::ExactOutput { .. } => v(&["swap"]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wallet_core::appdb::Operation;

    const PHRASE: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    fn fixture() -> (tempfile::TempDir, Session, TradingIntent) {
        let dir = tempfile::tempdir().unwrap();
        let paths = wallet_core::paths::Paths::resolve(Some(dir.path().into())).unwrap();
        let registry = wallet_core::registry::Registry::new(paths);
        let meta = registry.create_hd("plan", PHRASE, "english", "", "password123", true).unwrap();
        let network = wallet_core::network::NetworkProfile::builtins().remove(0);
        let session = Session::open(registry, wallet_core::config::AppConfig::default(), meta, network).unwrap();
        let intent = TradingIntent {
            account: session.account(None).unwrap().address,
            max_fee: None,
            action: TradingAction::Swap { from: "USDT".into(), to: "quai".into(), amount: "1".into(), slippage: 50, deadline: 10 },
        };
        (dir, session, intent)
    }

    /// A step the runner sent, as the journal holds it.
    fn sent(session: &Session, runner: &mut Runner, n: u8, kind: OpKind, status: OpStatus) -> String {
        let id = format!("{:032x}", u128::from(n) + 1);
        let op = Operation {
            id: id.clone(),
            network: session.network.id.clone(),
            kind,
            store: "quai".into(),
            account: runner.coordinator.plan.owner.clone(),
            status,
            tx_hash: Some(format!("0x{}", format!("{n:02x}").repeat(32))),
            asset: "QUAI".into(),
            amount: "1".into(),
            counterparty: "router".into(),
            fee: "0".into(),
            detail: serde_json::json!({}).into(),
            created: 1,
            updated: 1,
        };
        session.app.insert_operation(&op).unwrap();
        runner.coordinator.plan.operations.push(id.clone());
        id
    }

    fn set_status(session: &Session, id: &str, status: OpStatus) {
        session.app.update_operation(id, status, None, None, None).unwrap();
    }

    fn submitted(id: &str) -> Submitted {
        Submitted { op_id: id.into(), tx_hash: "0x".into(), status: OpStatus::Submitted, explorer: None, message: String::new() }
    }

    #[test]
    fn a_step_is_waited_on_until_it_confirms_and_then_the_next_is_ready() {
        let (_dir, session, intent) = fixture();
        let mut runner = Runner::start(&session, "swap USDT to QUAI", intent).unwrap();
        assert_eq!(runner.view().ahead, vec!["swap".to_string()]);
        let approval = sent(&session, &mut runner, 1, OpKind::Approve, OpStatus::Submitted);
        runner.committed(&session, &submitted(&approval), &OpKind::Approve).unwrap();
        assert!(!runner.last_step_sent(), "an approval is a step before the action");
        assert_eq!(runner.phase(), &Phase::Waiting(approval.clone()));
        assert!(!runner.poll(&session), "not mined yet");
        set_status(&session, &approval, OpStatus::Confirmed);
        assert!(runner.poll(&session));
        assert_eq!(runner.phase(), &Phase::Ready);
        assert_eq!(runner.view().done, vec!["approve".to_string()]);
    }

    #[test]
    fn a_qi_wrap_counts_only_once_it_settles() {
        let (_dir, session, intent) = fixture();
        let mut runner = Runner::start(&session, "Qi → QUAI through the market", intent).unwrap();
        let wrap = sent(&session, &mut runner, 2, OpKind::WrapQi, OpStatus::Confirmed);
        runner.committed(&session, &submitted(&wrap), &OpKind::WrapQi).unwrap();
        assert!(!runner.poll(&session), "confirmed is not settled: the claim cannot come yet");
        set_status(&session, &wrap, OpStatus::Settled);
        assert!(runner.poll(&session));
        assert_eq!(runner.phase(), &Phase::Ready);
    }

    #[test]
    fn a_failed_step_stops_the_plan_and_says_what_went_through() {
        let (_dir, session, intent) = fixture();
        let mut runner = Runner::start(&session, "swap", intent).unwrap();
        let approval = sent(&session, &mut runner, 3, OpKind::Approve, OpStatus::Confirmed);
        runner.committed(&session, &submitted(&approval), &OpKind::Approve).unwrap();
        assert!(runner.poll(&session));
        let swap = sent(&session, &mut runner, 4, OpKind::Swap, OpStatus::Submitted);
        runner.committed(&session, &submitted(&swap), &OpKind::Swap).unwrap();
        assert!(runner.last_step_sent(), "the swap is the plan's last step");
        set_status(&session, &swap, OpStatus::Failed);
        assert!(runner.poll(&session));
        let Phase::Stopped(said) = runner.phase() else { panic!("{:?}", runner.phase()) };
        assert_eq!(said, "swap stopped after 2 steps: approve, swap went through; nothing after it was sent.");
        let plan = session.app.trade_plan(runner.id()).unwrap().unwrap();
        assert_eq!(plan.state, PlanState::Paused, "paused, so what went through can be resumed from");
    }

    #[test]
    fn a_rejected_review_pauses_and_a_lock_asks_again() {
        let (_dir, session, intent) = fixture();
        let mut runner = Runner::start(&session, "swap", intent).unwrap();
        runner.phase = Phase::Reviewing("op".into());
        assert!(runner.reviewing("op"));
        runner.locked();
        assert_eq!(runner.phase(), &Phase::Ready, "the review went with the keys; the step is asked for again");
        runner.phase = Phase::Reviewing("op".into());
        runner.rejected(&session);
        assert!(matches!(runner.phase(), Phase::Stopped(s) if s.contains("nothing further will be signed")));
        assert_eq!(session.app.trade_plan(runner.id()).unwrap().unwrap().state, PlanState::Paused);
    }

    #[test]
    fn resuming_waits_on_what_was_sent_and_lets_an_unsigned_review_go() {
        let (_dir, mut session, intent) = fixture();
        let mut runner = Runner::start(&session, "swap", intent).unwrap();
        let approval = sent(&session, &mut runner, 5, OpKind::Approve, OpStatus::Submitted);
        runner.committed(&session, &submitted(&approval), &OpKind::Approve).unwrap();
        let id = runner.id().to_string();
        drop(runner);
        let mut resumed = Runner::resume(&mut session, None, true).unwrap();
        assert_eq!(resumed.id(), id, "the newest unfinished plan");
        assert_eq!(resumed.phase(), &Phase::Waiting(approval.clone()));
        assert_eq!(resumed.view().done, vec!["approve".to_string()]);
        set_status(&session, &approval, OpStatus::Confirmed);
        assert!(resumed.poll(&session));
        drop(resumed);
        assert!(Runner::resume(&mut session, Some("0".repeat(32).as_str()), true).is_err(), "no such plan");
    }

    /// A plan says where it stands: what was sent, what is under way, and what is known to
    /// follow. An approval the engine asked for appears as the step under review, never guessed.
    #[test]
    fn the_stepper_shows_what_was_sent_and_what_follows() {
        use StepState::*;
        let action = |stage| TradingAction::CrossVenue {
            from: "quai".into(),
            hub: "0x00aa".into(),
            to: "quai".into(),
            amount: "1".into(),
            stage,
            slippage: 50,
            deadline: 10,
            redeem: true,
        };
        let view = |done: &[&str], stage| PlanView {
            id: "p".into(),
            label: "swap".into(),
            phase: Phase::Ready,
            done: done.iter().map(|s| s.to_string()).collect(),
            ahead: ahead(&action(stage)),
            last: None,
            last_step: false,
        };
        let s = |v: &[(&str, StepState)]| v.iter().map(|(n, st)| (n.to_string(), *st)).collect::<Vec<_>>();
        assert_eq!(view(&[], 0).stepper(None), s(&[("swap", Now), ("swap on", Next), ("unwrap WQUAI", Next)]));
        assert_eq!(
            view(&[], 0).stepper(Some("approve")),
            s(&[("approve", Now), ("swap", Next), ("swap on", Next), ("unwrap WQUAI", Next)]),
            "an approval under review sits before the swap it unlocks"
        );
        assert_eq!(
            view(&["approve", "swap"], 1).stepper(None),
            s(&[("approve", Done), ("swap", Done), ("swap on", Now), ("unwrap WQUAI", Next)])
        );
    }

    /// A trade that fails before anything was sent has nothing to resume, so it ends cancelled:
    /// left paused, it would be the newest unfinished trade, which resuming picks, over a real one
    /// stopped halfway. One that got a step through pauses.
    #[test]
    fn a_trade_that_never_started_is_not_left_to_resume() {
        let (_dir, mut session, intent) = fixture();
        let mut runner = Runner::start(&session, "swap", intent.clone()).unwrap();
        runner.failed(&mut session, "not enough USDT");
        assert!(matches!(runner.phase(), Phase::Stopped(s) if s == "swap stopped before anything was sent."));
        assert_eq!(session.app.trade_plan(runner.id()).unwrap().unwrap().state, PlanState::Cancelled);
        drop(runner);
        let mut runner = Runner::start(&session, "swap", intent).unwrap();
        let approval = sent(&session, &mut runner, 9, OpKind::Approve, OpStatus::Confirmed);
        runner.committed(&session, &submitted(&approval), &OpKind::Approve).unwrap();
        runner.failed(&mut session, "quote expired");
        assert_eq!(session.app.trade_plan(runner.id()).unwrap().unwrap().state, PlanState::Paused);
    }
}
