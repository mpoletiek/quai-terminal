//! Multi-step trades: the engine walks them (`quai_engine::plans`); the screen starts them, asks
//! for each next review when the user can see it, and says where they stand.

use super::*;
use quai_engine::plans::{Phase, PlanCmd, PlanView};
use wallet_core::execution::{TradingAction, TradingIntent};
use wallet_core::journal::OpKind;

impl App {
    /// Start a trade plan in the engine (one runs at a time). `claim` is the wrapped Qi a claim
    /// plan is for, so a declined claim is not prompted again.
    pub fn start_plan(&mut self, label: String, intent: TradingIntent, claim: Option<String>) {
        if let Some(plan) = &self.eco.plan {
            let text = format!("finish or cancel “{}” first", plan.view.label);
            self.toast(text, true);
            return;
        }
        let view = PlanView {
            id: String::new(),
            label: label.clone(),
            phase: Phase::Preparing,
            done: Vec::new(),
            ahead: Vec::new(),
            last: None,
            last_step: false,
        };
        self.eco.plan = Some(ActivePlan { view, requested: true, claim });
        self.send(Cmd::Plan(PlanCmd::Start { label, intent }));
    }

    /// A sequence a form or card asks for, as the engine's trade plan.
    pub fn start_steps(&mut self, prepare: Prepare, label: String) {
        match self.trading_intent(prepare) {
            Some(intent) => self.start_plan(label, intent, None),
            None => self.toast("select a signing account", true),
        }
    }

    fn start_claim(&mut self, account: Option<String>, qits: String) {
        let Some(account) = account.or_else(|| self.dash.active_account().map(|a| a.address.clone())) else {
            self.toast("select a signing account", true);
            return;
        };
        let label = format!("claim {} Qi as WQI", amount::qi(qits.parse().unwrap_or_default()));
        self.start_plan(label, TradingIntent { account, max_fee: None, action: TradingAction::ClaimWqi }, Some(qits));
    }

    /// A request for the next review of a sequence, as the intent the plan runner walks.
    pub(crate) fn trading_intent(&self, prepare: Prepare) -> Option<TradingIntent> {
        use TradingAction as A;
        let (account, action) = match prepare {
            Prepare::Trading { intent } => return Some(intent),
            Prepare::CurveSellNext { account, token, symbol, curve, amount, slippage, deadline } => (
                account,
                A::CurveSell { token, symbol, curve, amount, slippage, deadline: deadline.unwrap_or(self.config.swap_deadline_minutes) },
            ),
            Prepare::IncentivizeNext { account, pair, token, amount, days } => (account, A::Incentivize { pair, token, amount, days }),
            Prepare::AddLiquidityNext { account, pair, amount, token, slippage, deadline } => {
                (account, A::AddLiquidity { pair, amount, side: token, slippage, deadline })
            }
            Prepare::RemoveLiquidityNext { account, pair, percent, slippage, deadline } => {
                (account, A::RemoveLiquidity { pair, percent, slippage, deadline })
            }
            Prepare::StakeNext { account, pair, gauge, amount } => (account, A::Stake { pair, gauge, amount }),
            Prepare::Unstake { account, pair, gauge, amount } => (account, A::Unstake { pair, gauge, amount }),
            Prepare::NftBuyNext { account, contract, token_id, price } => (account, A::NftBuy { contract, token_id, price }),
            Prepare::NftListNext { account, contract, token_id, price, currency } => {
                (account, A::NftList { contract, token_id, price, currency })
            }
            Prepare::ClaimWqi { account } => (account, A::ClaimWqi),
            _ => return None,
        };
        let account = account.or_else(|| self.dash.active_account().map(|a| a.address.clone()))?;
        Some(TradingIntent { account, max_fee: None, action })
    }

    /// Resume the most recent unfinished trade, always through a new review.
    pub fn resume_trade_plan(&mut self) {
        if let Some(plan) = &self.eco.plan {
            let text = format!("“{}” is already running", plan.view.label);
            self.toast(text, true);
            return;
        }
        self.send(Cmd::Plan(PlanCmd::Resume { id: None }));
    }

    /// Ask the engine for the next review once the last step is in and the user can see it.
    /// Runs every frame, whatever screen is showing.
    pub fn advance_flow(&mut self) {
        let may_prepare = !self.lock.locked && self.can_sign() && matches!(self.modal, Modal::None);
        let Some(plan) = &mut self.eco.plan else {
            if may_prepare {
                self.maybe_prompt_claim();
            }
            return;
        };
        if may_prepare && !plan.requested && plan.view.phase == Phase::Ready && !plan.view.id.is_empty() {
            plan.requested = true;
            let id = plan.view.id.clone();
            self.send(Cmd::Plan(PlanCmd::Next { id }));
        }
    }

    /// Where a plan stands, from the engine.
    pub fn on_plan(&mut self, view: PlanView) {
        self.dirty = true;
        // A plan this screen is not following (one finishing in the background, or restored by
        // `p` and not yet known here).
        let followed = self.eco.plan.as_ref().is_some_and(|p| p.view.id.is_empty() || p.view.id == view.id);
        if !followed {
            match &view.phase {
                // Restored: the screen follows it from here.
                Phase::Ready | Phase::Waiting(_) | Phase::Reviewing(_) | Phase::Preparing if self.eco.plan.is_none() => {
                    self.eco.plan = Some(ActivePlan { view, requested: false, claim: None });
                }
                _ => {}
            }
            return;
        }
        let plan = self.eco.plan.as_mut().expect("followed");
        let claim = plan.claim.clone();
        match &view.phase {
            Phase::Done(said) => {
                let said = said.clone();
                self.eco.plan = None;
                self.eco.swap.approving = false;
                if !self.lock.locked {
                    self.toast(said, false);
                }
            }
            Phase::Stopped(said) => {
                let said = said.clone();
                self.eco.plan = None;
                self.eco.swap.approving = false;
                if claim.is_some() {
                    self.eco.wrap.claim_declined = claim;
                }
                if !self.lock.locked {
                    self.toast_as(said, super::super::app::Severity::Attention, None);
                }
            }
            Phase::Reviewing(_) => {
                plan.requested = false;
                plan.view = view;
            }
            Phase::Ready => {
                plan.view = view;
                self.eco.swap.approving = false;
            }
            Phase::Waiting(_) | Phase::Preparing => plan.view = view,
        }
    }

    /// A review arrived: a plan that asked for one has it. Every review is shown (true); the
    /// engine said which plan it belongs to before it arrived.
    pub fn flow_on_review(&mut self, _op_id: &str) -> bool {
        if let Some(plan) = &mut self.eco.plan {
            plan.requested = false;
        }
        true
    }

    /// The engine could not prepare the next step (the plan's stop follows as its own view).
    pub fn flow_on_error(&mut self) {
        let Some(plan) = &mut self.eco.plan else { return };
        if !plan.requested {
            return;
        }
        plan.requested = false;
        // The engine never started it (its answer is the error itself): nothing was prepared.
        if plan.view.id.is_empty() {
            let said = format!("{} stopped before anything was sent.", plan.view.label);
            self.eco.plan = None;
            self.toast_as(said, super::super::app::Severity::Attention, None);
        }
    }

    /// A step was signed and broadcast. Returns true when the sequence continues (so the caller
    /// shows a toast instead of the result dialog).
    pub fn flow_on_submitted(&mut self, op_id: &str, kind: &OpKind) -> bool {
        let Some(plan) = &self.eco.plan else {
            self.eco.flow_summary = None;
            return false;
        };
        if plan.view.phase != Phase::Waiting(op_id.to_string()) {
            self.eco.flow_summary = None;
            return false;
        }
        let (label, done, last_step) = (plan.view.label.clone(), plan.view.done.clone(), plan.view.last_step);
        // The last step's result dialog lists the whole sequence.
        self.eco.flow_summary = Some((label.clone(), done));
        if last_step {
            // The engine records it complete when it confirms; the next trade can start now.
            self.eco.plan = None;
            return false;
        }
        if *kind == OpKind::Approve && self.on_card(Card::Swap) {
            self.eco.swap.approving = true;
        }
        let step = step_name(kind);
        self.toast(format!("{step} sent · {label} continues when it confirms (you can keep using the wallet)"), false);
        true
    }

    /// Locking drops open reviews; the engine asks again after unlocking.
    pub fn flow_on_lock(&mut self) {
        if self.status.committing_kind.is_some() {
            return;
        }
        if let Some(plan) = &mut self.eco.plan {
            plan.requested = false;
        }
    }

    /// A wrapped balance in atoms: WQI when `qi`, else WQUAI.
    pub(crate) fn wrapped_atoms(&self, qi: bool) -> u128 {
        let wrap = self.dash.wrap.as_ref();
        let value = if qi { wrap.and_then(|w| w.wqi_atoms.as_ref()) } else { wrap.and_then(|w| w.wquai_atoms.as_ref()) };
        value.and_then(|s| s.parse::<u128>().ok()).unwrap_or(0)
    }

    /// Start the market route for the amount on the Convert card.
    pub fn start_qi_route(&mut self, direction: wallet_core::qi_market::Direction, amount: String, slippage: u16) {
        let Some(account) = self.dash.active_account().map(|a| a.address.clone()) else {
            self.toast("select a signing account", true);
            return;
        };
        let Some(wqi) = self.net().and_then(|n| n.wqi.clone()) else {
            self.toast("WQI is not configured", true);
            return;
        };
        let (pay, receive) = direction.assets();
        let label = format!("{amount} {pay} → {receive} through the market");
        let intent = wallet_core::execution::TradingIntent {
            account,
            max_fee: None,
            action: wallet_core::execution::TradingAction::MarketConversion {
                direction,
                amount,
                stage: 0,
                wqi,
                slippage,
                deadline: self.config.swap_deadline_minutes,
                residual_atoms: "0".into(),
            },
        };
        self.start_plan(label, intent, None);
    }

    pub fn start_protocol_conversion(
        &mut self,
        direction: wallet_core::qi_market::Direction,
        amount: String,
        slippage: Option<u16>,
        account: Option<String>,
    ) {
        let Some(account) = account.or_else(|| self.dash.active_account().map(|a| a.address.clone())) else {
            self.toast("select a signing account", true);
            return;
        };
        let intent = wallet_core::execution::TradingIntent {
            account,
            max_fee: None,
            action: wallet_core::execution::TradingAction::ProtocolConversion { direction, amount, slippage },
        };
        self.start_plan("protocol conversion".into(), intent, None);
    }

    /// Settled wrapped Qi waiting for its claim: open the claim review (once per amount).
    pub(crate) fn maybe_prompt_claim(&mut self) {
        if !self.dash.unlocked {
            return;
        }
        let Some(qits) = self.dash.wrap.as_ref().and_then(|w| w.unclaimed_qits.clone()) else { return };
        if qits.parse::<u128>().map_or(true, |v| v == 0) || self.eco.wrap.claim_declined.as_deref() == Some(qits.as_str()) {
            return;
        }
        // A claim already on its way.
        if self.dash.ops.iter().any(|o| o.kind == OpKind::ClaimWqi && !o.status.is_terminal()) {
            return;
        }
        let account = self.dash.wrap.as_ref().map(|w| w.account.clone());
        self.toast(
            format!("{} Qi of wrapped Qi is ready · review the claim to receive WQI", amount::qi(qits.parse().unwrap_or_default())),
            false,
        );
        self.start_claim(account, qits);
    }

    /// Claim on request (Wrap card, palette): the same sequence the automatic prompt uses.
    pub fn claim_now(&mut self, account: Option<String>) {
        let qits = self.dash.wrap.as_ref().and_then(|w| w.unclaimed_qits.clone()).unwrap_or_default();
        if qits.parse::<u128>().map_or(true, |v| v == 0) {
            self.toast("nothing to claim yet · wrapped Qi can be claimed once the wrap settles", true);
            return;
        }
        self.eco.wrap.claim_declined = None;
        self.start_claim(account, qits);
    }

    /// Re-check asks and holdings after an NFT operation is submitted.
    pub fn after_submit(&mut self, kind: &OpKind) {
        match kind {
            OpKind::Approve if self.on_card(Card::Swap) => {
                self.eco.swap.approving = true;
                // The next quote comes at the approving pace, counted from now.
                self.eco.swap.quote_read.rest();
            }
            OpKind::Approve | OpKind::NftBuy | OpKind::NftTransfer => {
                if let Some(Detail::Nft(c, id)) = self.nav.detail.last().cloned() {
                    let buyer = self.dash.active_account().map(|a| a.address.clone());
                    self.send_data(DataCmd::CheckAsk { contract: c, token_id: id, buyer });
                }
                // Holdings reload when the operation confirms (see `after_confirm`): reloading
                // now would cache a list from before the transfer was mined.
                if *kind != OpKind::Approve {
                    self.eco.nft.listings.clear();
                }
            }
            OpKind::Swap => {
                self.eco.swap.amount.clear();
                self.eco.swap.quote = None;
                self.eco.feeds.portfolio_signature = None;
            }
            _ => {}
        }
    }

    /// Reload what a confirmed operation changed.
    pub fn after_confirm(&mut self, kind: &OpKind) {
        if matches!(kind, OpKind::NftList | OpKind::NftReprice | OpKind::NftUnlist) {
            self.eco.nft.listings.clear();
            self.load_my_listings();
        }
        if matches!(kind, OpKind::NftBuy | OpKind::NftTransfer) {
            self.eco.nft.listings.clear();
            if self.nav.screen == Screen::Collected || self.eco.nft.nfts.latest().is_some() {
                self.load_nfts(true);
            } else {
                self.eco.nft.nfts.clear();
            }
        }
    }
}
