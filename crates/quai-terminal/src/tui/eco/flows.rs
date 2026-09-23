//! Multi-step trades: checkpoints, advancing a flow and what each review outcome does to it.

use super::*;

impl App {
    pub(crate) fn flow_db(&self) -> wallet_core::Result<wallet_core::appdb::AppDb> {
        let meta = self.meta.as_ref().ok_or_else(|| wallet_core::CoreError::Invalid("no wallet selected".into()))?;
        wallet_core::appdb::AppDb::open(&self.paths.wallet_dir(&meta.id).join("app.sqlite"))
    }

    /// Persist a non-secret checkpoint before requesting or exposing an executable review.
    pub(crate) fn checkpoint_flow(&mut self) -> bool {
        let Some(mut flow) = self.eco.flow.take() else { return true };
        let result = (|| -> wallet_core::Result<()> {
            let db = self.flow_db()?;
            let intent = serde_json::to_value(&flow)?;
            let mut plan = match flow.checkpoint.clone() {
                Some(plan) => plan,
                None => wallet_core::plans::TradePlan::new(
                    self.network_id.clone(),
                    self.meta.as_ref().unwrap().id.clone(),
                    flow.kind.label(),
                    intent.clone(),
                )?,
            };
            if flow.lease.is_none() {
                let directory = self.paths.wallet_dir(&plan.owner).join("plan-locks");
                flow.lease = Some(Arc::new(wallet_core::plans::claim(&directory, &plan.id)?));
            }
            let old = plan.clone();
            plan.intent = intent;
            plan.state =
                if flow.waiting.is_some() { wallet_core::plans::PlanState::Waiting } else { wallet_core::plans::PlanState::Review };
            for id in [&flow.review_op, &flow.waiting, &flow.last_operation].into_iter().flatten() {
                if !plan.operations.contains(id) {
                    plan.operations.push(id.clone());
                }
            }
            if plan.revision == 0 || plan != old {
                db.save_trade_plan(&mut plan)?;
            }
            flow.checkpoint = Some(plan);
            Ok(())
        })();
        self.eco.flow = Some(flow);
        if let Err(error) = result {
            if !self.locked {
                self.toast(format!("trade checkpoint failed: {error}; execution paused"), true);
            }
            return false;
        }
        true
    }

    pub(crate) fn close_flow_checkpoint(&mut self, state: wallet_core::plans::PlanState, reason: &str) {
        if !self.checkpoint_flow() {
            return;
        }
        let result = (|| -> wallet_core::Result<()> {
            let db = self.flow_db()?;
            if let Some(plan) = self.eco.flow.as_mut().and_then(|flow| flow.checkpoint.as_mut()) {
                plan.state = state;
                plan.reason = reason.into();
                if state == wallet_core::plans::PlanState::Waiting {
                    plan.intent["final_submitted"] = serde_json::json!(true);
                }
                db.save_trade_plan(plan)?;
            }
            Ok(())
        })();
        if let Err(error) = result
            && !self.locked
        {
            self.toast(format!("could not update trade checkpoint: {error}"), true);
        }
    }

    /// Explicitly resume the most recent unfinished trade, always through a new review.
    pub fn resume_trade_plan(&mut self) {
        if self.eco.flow.is_some() {
            self.toast("a trade is already active", true);
            return;
        }
        let result = (|| -> wallet_core::Result<Option<ResumableFlow>> {
            use wallet_core::plans::{PlanReadiness, PlanState};
            let db = self.flow_db()?;
            let Some(mut plan) = db
                .trade_plans(&self.network_id)?
                .into_iter()
                .filter(|plan| self.meta.as_ref().is_some_and(|meta| plan.owner == meta.id))
                .find(|plan| !matches!(plan.state, PlanState::Complete | PlanState::Cancelled))
            else {
                return Ok(None);
            };
            if self.meta.as_ref().is_none_or(|meta| plan.owner != meta.id) || plan.network != self.network_id {
                return Err(wallet_core::CoreError::Rejected("trade checkpoint belongs to another wallet or network".into()));
            }
            let lease = Arc::new(wallet_core::plans::claim(&self.paths.wallet_dir(&plan.owner).join("plan-locks"), &plan.id)?);
            if plan.intent["final_submitted"] == true {
                if plan.readiness(&db)? == PlanReadiness::ReviewRequired {
                    plan.state = PlanState::Complete;
                    plan.reason =
                        "final step receipt observed; provisional chain results remain subject to canonical reconciliation".into();
                    db.save_trade_plan(&mut plan)?;
                    return Ok(None);
                }
                return Err(wallet_core::CoreError::Invalid(
                    "final step still requires transaction reconciliation; refresh Activity".into(),
                ));
            }
            let mut flow: Flow = serde_json::from_value(plan.intent.clone())?;
            for op in db.operations_for_plan(&self.network_id, &plan.id)? {
                if !plan.operations.contains(&op.id) {
                    plan.operations.push(op.id.clone());
                    flow.review_op = Some(op.id);
                }
            }
            let mut submitted = None;
            if let Some(id) = flow.review_op.take() {
                let op = db
                    .operation(&id)?
                    .ok_or_else(|| wallet_core::CoreError::Invalid("review operation is missing; reconcile the wallet first".into()))?;
                if op.tx_hash.is_some() {
                    flow.review_op = Some(id.clone());
                    submitted = Some((id, op.kind));
                } else if op.status == wallet_core::appdb::OpStatus::Prepared {
                    let meta = self.meta.as_ref().unwrap().clone();
                    let mut session = wallet_core::session::Session::open(
                        self.registry.clone(),
                        self.config.clone(),
                        meta,
                        self.config.network(&self.network_id)?.clone(),
                    )?;
                    session.abandon(&id)?;
                    plan.operations.retain(|operation| operation != &id);
                }
            }
            if plan.readiness(&db)? == PlanReadiness::Stopped {
                return Err(wallet_core::CoreError::Invalid(
                    "a trade step failed or was refunded; inspect holdings and create a fresh trade".into(),
                ));
            }
            flow.requested = false;
            flow.lease = Some(lease);
            flow.checkpoint = Some(plan);
            Ok(Some((flow, submitted)))
        })();
        match result {
            Ok(Some((flow, submitted))) => {
                self.eco.flow = Some(flow);
                if let Some((id, kind)) = submitted {
                    self.flow_on_submitted(&id, &kind);
                }
                self.toast("trade restored; reconcile previous receipts and review each remaining step", false);
                self.advance_flow();
            }
            Ok(None) => self.info("no unfinished trade remains"),
            Err(error) => self.toast(error.to_string(), true),
        }
    }

    /// Start a sequence (replacing none: one runs at a time).
    pub fn start_flow(&mut self, kind: FlowKind) {
        if let Some(f) = &self.eco.flow {
            let text = format!("finish or cancel “{}” first", f.kind.label());
            self.toast(text, true);
            return;
        }
        self.eco.flow = Some(Flow {
            checkpoint: None,
            lease: None,
            kind,
            swapped: false,
            requested: false,
            review_op: None,
            waiting: None,
            last_operation: None,
            steps: 0,
            done: Vec::new(),
            last_poll: Instant::now(),
        });
        if self.checkpoint_flow() {
            self.advance_flow();
        }
    }

    /// Drive the sequence: request the next review, or wait for the last step to confirm.
    /// Runs every frame, whatever screen is showing.
    pub fn advance_flow(&mut self) {
        let may_prepare = !self.locked && self.can_sign() && matches!(self.modal, Modal::None);
        let Some(mut flow) = self.eco.flow.clone() else {
            if may_prepare {
                self.maybe_prompt_claim();
            }
            return;
        };
        if flow.requested || flow.review_op.is_some() {
            return;
        }
        if let Some(op_id) = flow.waiting.clone() {
            use wallet_core::appdb::OpStatus;
            match self.dash.ops.iter().find(|o| o.id == op_id).map(|o| o.status) {
                Some(OpStatus::Confirmed | OpStatus::Settled) => {
                    if let FlowKind::Steps { prepare, .. } = &mut flow.kind
                        && let Prepare::Trading { intent } = prepare.as_mut()
                        && intent.has_more_allocations()
                        && let Some(op) = self.dash.ops.iter().find(|op| op.id == op_id && !wallet_core::flows::is_step_kind(&op.kind))
                    {
                        if op.kind == "wrap_qi" && op.status != OpStatus::Settled {
                            if flow.last_poll.elapsed() > Duration::from_secs(5) {
                                flow.last_poll = Instant::now();
                                self.send(Cmd::Refresh { full: false });
                            }
                            self.eco.flow = Some(flow);
                            return;
                        }
                        match intent.advance_allocation(op) {
                            Ok(true) => {}
                            Ok(false) => {
                                let residual = match &intent.action {
                                    wallet_core::execution::TradingAction::MarketConversion { residual_atoms, .. } => {
                                        format!("market conversion complete; {} WQI atoms remain below one redeemable Qi", residual_atoms)
                                    }
                                    _ => "trading steps complete".into(),
                                };
                                self.eco.flow = Some(flow);
                                self.close_flow_checkpoint(wallet_core::plans::PlanState::Complete, &residual);
                                self.eco.flow = None;
                                if !self.locked {
                                    self.toast(residual, false);
                                }
                                return;
                            }
                            Err(error) => {
                                if flow.last_poll.elapsed() > Duration::from_secs(5) {
                                    flow.last_poll = Instant::now();
                                    if !self.locked {
                                        self.toast(error.to_string(), false);
                                    }
                                    self.send(Cmd::Refresh { full: false });
                                }
                                self.eco.flow = Some(flow);
                                return;
                            }
                        }
                    }
                    flow.waiting = None;
                    if let FlowKind::Swap { .. } = flow.kind {
                        self.eco.swap.approving = false;
                    }
                }
                Some(OpStatus::Failed | OpStatus::Cancelled | OpStatus::Refunded | OpStatus::Replaced) => {
                    let text = format!("{} stopped: a step did not confirm", flow.kind.label());
                    self.eco.flow = None;
                    self.eco.swap.approving = false;
                    if !self.locked {
                        self.toast(text, true);
                    }
                    return;
                }
                _ => {
                    // Poll the node faster than the idle refresh while a step is confirming.
                    if flow.last_poll.elapsed() > Duration::from_secs(5) {
                        flow.last_poll = Instant::now();
                        self.send(Cmd::Refresh { full: false });
                    }
                    self.eco.flow = Some(flow);
                    return;
                }
            }
        }
        // The first swap of a two-exchange route confirmed: the second takes exactly what it paid.
        if matches!(&flow.kind, FlowKind::Swap { then: Some(NextSwap { first: Some(_), .. }), .. }) {
            if self.begin_second_swap(&mut flow) {
                let text = format!("first swap confirmed · reviewing the second: {}", flow.kind.label());
                if !self.locked {
                    self.toast(text, false);
                }
            } else {
                let label = flow.kind.label();
                let FlowKind::Swap { then: Some(next), .. } = &mut flow.kind else { return };
                if next.polls >= SECOND_SWAP_POLLS {
                    self.eco.flow = None;
                    if !self.locked {
                        self.toast(
                            format!(
                                "{label}: the first swap confirmed, but what it paid could not be read · finish the route from Trade › Swap"
                            ),
                            true,
                        );
                    }
                    return;
                }
                if flow.last_poll.elapsed() > Duration::from_secs(3) {
                    next.polls += 1;
                    flow.last_poll = Instant::now();
                    self.send(Cmd::Refresh { full: false });
                }
                self.eco.flow = Some(flow);
                return;
            }
        }
        if !may_prepare {
            self.eco.flow = Some(flow);
            self.checkpoint_flow();
            return;
        }
        let prepare = match &flow.kind {
            // Wrap what the pool needs, swap, then redeem WQUAI the swap paid out.
            FlowKind::Swap { account, prewrap: Some(missing), .. } => {
                Prepare::WrapQuai { account: account.clone(), amount: missing.clone() }
            }
            FlowKind::Swap { account, unwrap_after: true, .. } if flow.steps > 0 && self.swap_done(&flow) => {
                match self.receipt_output(flow.last_operation.as_deref()).filter(|v| !v.is_zero()) {
                    Some(atoms) => Prepare::UnwrapQuai { account: account.clone(), amount: wallet_core::amount::format_amount(atoms, 18) },
                    None => {
                        if flow.last_poll.elapsed() > Duration::from_secs(3) {
                            flow.last_poll = Instant::now();
                            self.send(Cmd::Refresh { full: false });
                        }
                        self.eco.flow = Some(flow);
                        return;
                    }
                }
            }
            FlowKind::Swap { account, from, to, amount, slippage, deadline, .. } => Prepare::SwapNext {
                account: account.clone(),
                from: from.clone(),
                to: to.clone(),
                amount: amount.clone(),
                slippage: *slippage,
                deadline: *deadline,
            },
            FlowKind::NftBuy { account, contract, token_id, price, .. } => Prepare::NftBuyNext {
                account: account.clone(),
                contract: contract.clone(),
                token_id: token_id.clone(),
                price: price.clone(),
            },
            FlowKind::NftList { account, contract, token_id, price, currency, .. } => Prepare::NftListNext {
                account: account.clone(),
                contract: contract.clone(),
                token_id: token_id.clone(),
                price: price.clone(),
                currency: currency.clone(),
            },
            FlowKind::Claim { account, .. } => Prepare::ClaimWqi { account: account.clone() },
            FlowKind::Steps { prepare, .. } => (**prepare).clone(),
        };
        flow.requested = true;
        self.eco.flow = Some(flow);
        if self.checkpoint_flow()
            && let Some(id) = self.eco.flow.as_ref().and_then(|flow| flow.checkpoint.as_ref()).map(|plan| plan.id.clone())
        {
            self.send(Cmd::Prepare(Prepare::InPlan { id, request: Box::new(prepare) }));
        }
    }

    /// A wrapped balance in atoms: WQI when `qi`, else WQUAI.
    pub(crate) fn wrapped_atoms(&self, qi: bool) -> u128 {
        let wrap = self.dash.wrap.as_ref();
        let value = if qi { wrap.and_then(|w| w.wqi_atoms.as_ref()) } else { wrap.and_then(|w| w.wquai_atoms.as_ref()) };
        value.and_then(|s| s.parse::<u128>().ok()).unwrap_or(0)
    }

    /// Whether this swap sequence already submitted its swap (the unwrap comes after).
    pub(crate) fn swap_done(&self, flow: &Flow) -> bool {
        flow.swapped
    }

    pub(crate) fn receipt_output(&self, op_id: Option<&str>) -> Option<U256> {
        let op = self.dash.ops.iter().find(|op| Some(op.id.as_str()) == op_id)?;
        if !matches!(op.status, wallet_core::appdb::OpStatus::Confirmed | wallet_core::appdb::OpStatus::Settled) {
            return None;
        }
        op.detail["actual_out"].as_str().and_then(|s| U256::from_str_radix(s, 10).ok())
    }

    /// Start the market route for the amount on the Convert card.
    pub fn start_qi_route(&mut self, direction: wallet_core::qi_market::Direction, amount: String, slippage: u16) {
        let Some(account) = self.dash.accounts.first().map(|a| a.address.clone()) else {
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
        self.start_flow(FlowKind::Steps { prepare: Box::new(Prepare::Trading { intent }), label });
    }

    pub fn start_protocol_conversion(
        &mut self,
        direction: wallet_core::qi_market::Direction,
        amount: String,
        slippage: Option<u16>,
        account: Option<String>,
    ) {
        let Some(account) = account.or_else(|| self.dash.accounts.first().map(|a| a.address.clone())) else {
            self.toast("select a signing account", true);
            return;
        };
        let intent = wallet_core::execution::TradingIntent {
            account,
            max_fee: None,
            action: wallet_core::execution::TradingAction::ProtocolConversion { direction, amount, slippage },
        };
        self.start_flow(FlowKind::Steps { prepare: Box::new(Prepare::Trading { intent }), label: "protocol conversion".into() });
    }

    /// Settled wrapped Qi waiting for its claim: open the claim review (once per amount).
    pub(crate) fn maybe_prompt_claim(&mut self) {
        if !self.dash.unlocked {
            return;
        }
        let Some(qits) = self.dash.wrap.as_ref().and_then(|w| w.unclaimed_qits.clone()) else { return };
        if qits.parse::<u128>().map_or(true, |v| v == 0) || self.eco.claim_declined.as_deref() == Some(qits.as_str()) {
            return;
        }
        // A claim already on its way.
        if self.dash.ops.iter().any(|o| o.kind == "claim_wqi" && !o.status.is_terminal()) {
            return;
        }
        let account = self.dash.wrap.as_ref().map(|w| w.account.clone());
        self.toast(
            format!("{} Qi of wrapped Qi is ready · review the claim to receive WQI", amount::qi(qits.parse().unwrap_or_default())),
            false,
        );
        self.start_flow(FlowKind::Claim { account, qits });
    }

    /// Claim on request (Wrap card, palette): the same sequence the automatic prompt uses.
    pub fn claim_now(&mut self, account: Option<String>) {
        let qits = self.dash.wrap.as_ref().and_then(|w| w.unclaimed_qits.clone()).unwrap_or_default();
        if qits.parse::<u128>().map_or(true, |v| v == 0) {
            self.toast("nothing to claim yet · wrapped Qi can be claimed once the wrap settles", true);
            return;
        }
        self.eco.claim_declined = None;
        self.start_flow(FlowKind::Claim { account, qits });
    }

    /// A review arrived; attach it to the sequence that asked for it.
    pub fn flow_on_review(&mut self, op_id: &str) -> bool {
        if let Some(flow) = &mut self.eco.flow
            && flow.requested
        {
            flow.requested = false;
            flow.review_op = Some(op_id.to_string());
            flow.steps += 1;
        }
        self.checkpoint_flow()
    }

    /// The worker could not prepare the next step.
    pub fn flow_on_error(&mut self) {
        if let Some(flow) = &self.eco.flow
            && flow.requested
        {
            // Where the money is: what went through before the stop, and that nothing after it
            // was sent. The error itself follows in its own toast.
            let label = flow.kind.label();
            let said = match flow.done.as_slice() {
                [] => format!("{label} stopped before anything was sent."),
                done => format!(
                    "{label} stopped after {}: {} went through; nothing after it was sent.",
                    wallet_core::amount::count(done.len(), "step"),
                    done.join(", ")
                ),
            };
            if let FlowKind::Claim { qits, .. } = &flow.kind {
                self.eco.claim_declined = Some(qits.clone());
            }
            self.close_flow_checkpoint(
                wallet_core::plans::PlanState::Paused,
                "execution paused; inspect completed assets and allowances before resuming",
            );
            self.eco.flow = None;
            self.eco.swap.approving = false;
            self.toast_as(said, super::super::app::Severity::Attention, None);
        }
    }

    /// A review was rejected: the sequence ends (nothing further is signed).
    pub fn flow_on_rejected(&mut self, op_id: &str) {
        if let Some(flow) = &self.eco.flow
            && flow.review_op.as_deref() == Some(op_id)
        {
            let label = flow.kind.label();
            if let FlowKind::Claim { qits, .. } = &flow.kind {
                self.eco.claim_declined = Some(qits.clone());
            }
            self.close_flow_checkpoint(
                wallet_core::plans::PlanState::Paused,
                "execution paused; inspect completed assets and allowances before resuming",
            );
            self.eco.flow = None;
            self.eco.swap.approving = false;
            self.toast(format!("{label} cancelled; nothing further will be signed"), false);
        }
    }

    /// A step was signed and broadcast. Returns true when the sequence continues (so the caller
    /// shows a toast instead of the result dialog).
    pub fn flow_on_submitted(&mut self, op_id: &str, kind: &str) -> bool {
        let Some(mut flow) = self.eco.flow.clone() else {
            self.eco.flow_summary = None;
            return false;
        };
        if flow.review_op.as_deref() != Some(op_id) {
            self.eco.flow_summary = None;
            return false;
        }
        flow.review_op = None;
        flow.last_operation = Some(op_id.to_string());
        flow.done.push(super::step_name(kind));
        // The last step's result dialog lists the whole sequence.
        self.eco.flow_summary = Some((flow.kind.label(), flow.done.clone()));
        if !wallet_core::flows::is_step_kind(kind)
            && let FlowKind::Steps { prepare, .. } = &flow.kind
            && let Prepare::Trading { intent } = prepare.as_ref()
            && intent.has_more_allocations()
        {
            flow.waiting = Some(op_id.to_string());
            flow.last_poll = Instant::now();
            self.eco.flow = Some(flow);
            self.checkpoint_flow();
            self.toast("step submitted; the next review waits for its attributed receipt and required settlement", false);
            return true;
        }
        // The first of two swaps: wait for it, then size the second from what it paid.
        if kind == "swap"
            && let FlowKind::Swap { then: Some(next), .. } = &mut flow.kind
            && next.first.is_none()
        {
            next.first = Some(op_id.to_string());
            flow.waiting = Some(op_id.to_string());
            flow.last_poll = Instant::now();
            let label = flow.kind.label();
            self.eco.flow = Some(flow);
            self.checkpoint_flow();
            self.toast(format!("{label}: first swap sent · the second review opens when it confirms"), false);
            return true;
        }
        // The pre-wrap and the swap both continue the sequence.
        if let FlowKind::Swap { prewrap, unwrap_after, .. } = &mut flow.kind {
            let wrapped = kind == "wrap_quai" && prewrap.is_some();
            if wrapped {
                *prewrap = None;
            }
            let swapped = kind == "swap" && *unwrap_after;
            if wrapped || swapped {
                flow.swapped |= swapped;
                flow.waiting = Some(op_id.to_string());
                flow.last_poll = Instant::now();
                let label = flow.kind.label();
                let note = if wrapped {
                    "wrapped · the swap review opens when it confirms"
                } else {
                    "swapped · the redemption review opens when it confirms"
                };
                self.eco.flow = Some(flow);
                self.checkpoint_flow();
                self.toast(format!("{label}: {note}"), false);
                return true;
            }
        }
        if wallet_core::flows::is_step_kind(kind) {
            if kind == "approve"
                && let FlowKind::Swap { .. } = flow.kind
            {
                self.eco.swap.approving = true;
            }
            flow.waiting = Some(op_id.to_string());
            flow.last_poll = Instant::now();
            let label = flow.kind.label();
            self.eco.flow = Some(flow);
            self.checkpoint_flow();
            let step = if kind == "approve" { "approval" } else { "wrap" };
            self.toast(format!("{step} sent · {label} continues when it confirms (you can keep using the wallet)"), false);
            true
        } else {
            flow.waiting = Some(op_id.to_string());
            self.eco.flow = Some(flow);
            self.checkpoint_flow();
            self.close_flow_checkpoint(
                wallet_core::plans::PlanState::Waiting,
                "final step submitted; reconcile its receipt before marking complete",
            );
            self.eco.flow = None;
            false
        }
    }

    /// Locking drops open reviews; ask again after unlocking.
    pub fn flow_on_lock(&mut self) {
        if self.committing_kind.is_some() {
            return;
        }
        if let Some(flow) = &mut self.eco.flow {
            flow.requested = false;
            flow.review_op = None;
        }
    }

    /// Re-check asks and holdings after an NFT operation is submitted.
    pub fn after_submit(&mut self, kind: &str) {
        match kind {
            "approve" if self.screen == Screen::Swap => {
                self.eco.swap.approving = true;
                self.eco.swap.quoted_at = Some(Instant::now());
            }
            "approve" | "nft_buy" | "nft_transfer" => {
                if let Some(Detail::Nft(c, id)) = self.detail.last().cloned() {
                    let buyer = self.dash.accounts.first().map(|a| a.address.clone());
                    self.send_data(DataCmd::CheckAsk { contract: c, token_id: id, buyer });
                }
                // Holdings reload when the operation confirms (see `after_confirm`): reloading
                // now would cache a list from before the transfer was mined.
                if kind != "approve" {
                    self.eco.listings.clear();
                }
            }
            "swap" => {
                self.eco.swap.amount.clear();
                self.eco.swap.quote = None;
                self.eco.portfolio_signature = None;
            }
            _ => {}
        }
    }

    /// Reload what a confirmed operation changed.
    pub fn after_confirm(&mut self, kind: &str) {
        if matches!(kind, "nft_list" | "nft_reprice" | "nft_unlist") {
            self.eco.listings.clear();
            self.load_my_listings();
        }
        if matches!(kind, "nft_buy" | "nft_transfer") {
            self.eco.listings.clear();
            if self.screen == Screen::Collected || self.eco.nfts.is_some() {
                self.load_nfts(true);
            } else {
                self.eco.nfts = None;
            }
        }
    }
}
