//! Public, resumable trading intent shared by interactive clients. Checkpoints never authorize signing.
use crate::{
    CoreError, Result,
    plans::{PlanReadiness, PlanState, TradePlan},
    session::Session,
    tx::Review,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum TradingAction {
    ProtocolConversion {
        direction: crate::qi_market::Direction,
        amount: String,
        slippage: Option<u16>,
    },
    MarketConversion {
        direction: crate::qi_market::Direction,
        amount: String,
        stage: u8,
        wqi: String,
        slippage: u16,
        deadline: u32,
        residual_atoms: String,
    },
    CrossVenue {
        from: String,
        hub: String,
        to: String,
        amount: String,
        stage: u8,
        slippage: u16,
        deadline: u32,
    },
    Split {
        plan: Box<crate::split_routes::SplitPlan>,
        index: usize,
        deadline: u32,
    },
    Swap {
        from: String,
        to: String,
        amount: String,
        slippage: u16,
        deadline: u32,
    },
    BoundedSwap {
        from: String,
        to: String,
        amount: String,
        slippage: u16,
        deadline: u32,
        bounds: crate::swap::SwapBounds,
    },
    ExactOutput {
        from: String,
        to: String,
        output: String,
        max_input: String,
        deadline: u32,
    },
    AddLiquidity {
        pair: String,
        amount: String,
        side: Option<String>,
        slippage: u16,
        deadline: u32,
    },
    RemoveLiquidity {
        pair: String,
        percent: u8,
        slippage: u16,
        deadline: u32,
    },
    Stake {
        pair: String,
        gauge: Option<String>,
        amount: String,
    },
    Incentivize {
        pair: String,
        token: String,
        amount: String,
        days: u32,
    },
    CurveSell {
        token: String,
        symbol: String,
        curve: String,
        amount: String,
        slippage: u16,
        deadline: u32,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TradingIntent {
    pub account: String,
    pub max_fee: Option<String>,
    pub action: TradingAction,
}

impl TradingIntent {
    pub fn has_more_allocations(&self) -> bool {
        matches!(&self.action, TradingAction::Split { plan, index, .. } if index + 1 < plan.allocations.len())
            || matches!(&self.action, TradingAction::CrossVenue { stage: 0, .. })
            || matches!(
                &self.action,
                TradingAction::MarketConversion { direction: crate::qi_market::Direction::QuaiToQi, stage: 0, .. }
                    | TradingAction::MarketConversion { direction: crate::qi_market::Direction::QiToQuai, stage: 0 | 1, .. }
            )
    }

    /// Move a saved plan past the operation that just completed, on its typed receipt. Anything
    /// the receipt cannot prove (another signer, another token, an output never attributed) stops
    /// the plan with the reason; nothing is inferred from an absent field.
    pub fn advance_allocation(&mut self, op: &crate::appdb::Operation) -> Result<bool> {
        use crate::journal::{ReceiptError, SwapReceipt, WqiClaim, WrapDeposit};
        let rejected = |why: &str| CoreError::Rejected(why.into());
        if let TradingAction::MarketConversion { direction, amount, stage, wqi, residual_atoms, .. } = &mut self.action {
            use crate::qi_market::Direction;
            use crate::sdk::U256;
            let output_unknown = || rejected("receipt-attributed conversion output remains unavailable");
            let names_wqi = op.detail.to_token().as_str().is_some_and(|t| t.eq_ignore_ascii_case(wqi));
            match (*direction, *stage) {
                (Direction::QuaiToQi, 0) => {
                    let receipt = match SwapReceipt::of(op, &self.account) {
                        Ok(r) if r.delivered(wqi) => r,
                        Err(ReceiptError::OutputUnknown) if names_wqi => return Err(output_unknown()),
                        _ => return Err(rejected("market swap receipt does not match the conversion plan")),
                    };
                    let unit = U256::from(10).pow(U256::from(18));
                    let whole = receipt.actual_out / unit;
                    *residual_atoms = (receipt.actual_out % unit).to_string();
                    *amount = whole.to_string();
                    *stage = if whole.is_zero() { 2 } else { 1 };
                    return Ok(!whole.is_zero());
                }
                (Direction::QiToQuai, 0) => {
                    let wanted = crate::amount::parse_qi(amount)?;
                    let settled = WrapDeposit::of(op).is_ok_and(|d| d.beneficiary.eq_ignore_ascii_case(&self.account) && d.qits == wanted);
                    if !settled {
                        return Err(rejected("the saved Qi deposit has not settled for this beneficiary"));
                    }
                    *stage = 1;
                    return Ok(true);
                }
                (Direction::QiToQuai, 1) => {
                    let claim = match WqiClaim::of(op, &self.account) {
                        Ok(c) if c.to_token.eq_ignore_ascii_case(wqi) => c,
                        Err(ReceiptError::OutputUnknown) if names_wqi => return Err(output_unknown()),
                        _ => return Err(rejected("wrapped-token claim receipt does not match this plan")),
                    };
                    let expected = crate::sdk::wrappers::qits_to_wqi_atoms(crate::amount::parse_qi(amount)?)?;
                    if claim.actual_out < expected {
                        return Err(rejected("claim did not deliver the plan's wrapped tokens"));
                    }
                    // Other claimable deposits remain in the wallet; only this plan's amount is spent.
                    *amount = crate::amount::format_amount(expected, 18);
                    *stage = 2;
                    return Ok(true);
                }
                _ => return Ok(false),
            }
        }
        if let TradingAction::CrossVenue { from, hub, amount, stage, .. } = &mut self.action {
            if *stage != 0 {
                return Ok(false);
            }
            // What the second leg would spend must be attributed, non-zero and in known units
            // before the route's identity is even compared.
            if op.detail.actual_out_atoms().is_none_or(|v| v.is_zero()) {
                return Err(rejected("first swap has no attributable onward output"));
            }
            if op.detail.to_decimals_u8().is_none() {
                return Err(rejected("first swap output units are unknown"));
            }
            let receipt = SwapReceipt::of(op, &self.account)
                .ok()
                .filter(|r| r.delivered(hub))
                .ok_or_else(|| rejected("first swap receipt does not match the saved route"))?;
            let decimals = receipt.decimals().map_err(|_| rejected("first swap output units are unknown"))?;
            *amount = crate::amount::format_amount(receipt.actual_out, decimals);
            *from = hub.clone();
            *stage = 1;
            return Ok(true);
        }
        let TradingAction::Split { plan, index, .. } = &mut self.action else { return Ok(false) };
        if *index + 1 >= plan.allocations.len() {
            return Ok(false);
        }
        let selected = &plan.allocations[*index];
        let minimum =
            crate::sdk::U256::from_str_radix(&selected.minimum_out, 10).map_err(|_| CoreError::Invalid("split minimum".into()))?;
        let paid = SwapReceipt::of(op, &self.account).ok().map(|r| r.actual_out);
        if paid.is_none_or(|paid| paid < minimum)
            || op.detail.split_allocation_index() != Some(*index as u64)
            || op.amount != selected.amount_in
        {
            return Err(rejected("split receipt is not attributed to the confirmed allocation; reconcile before continuing"));
        }
        *index += 1;
        Ok(true)
    }
    /// A swap paying WQUAI the account lacks wraps the shortfall from QUAI first
    /// ([`Session::prewrap_quai`]). What a step pays: a swap's input, a route's first leg, one
    /// split allocation, an exact-output swap's input cap. A route's second leg is left alone: it
    /// pays with what the first delivered.
    async fn prewrap(&self, session: &mut Session) -> Result<Option<Review>> {
        let (from, needed, purpose) = match &self.action {
            TradingAction::Swap { from, amount, .. } | TradingAction::BoundedSwap { from, amount, .. } => (from.clone(), amount, "swap"),
            TradingAction::CrossVenue { from, amount, stage: 0, .. } => (from.clone(), amount, "first swap"),
            TradingAction::ExactOutput { from, max_input, .. } => (from.clone(), max_input, "swap"),
            TradingAction::Split { plan, index, .. } => {
                let (crate::swap::SwapAsset::Token { address, .. }, Some(allocation)) = (&plan.from, plan.allocations.get(*index)) else {
                    return Ok(None);
                };
                let Ok(atoms) = crate::sdk::U256::from_str_radix(&allocation.amount_in, 10) else { return Ok(None) };
                let fee = self.max_fee.as_deref();
                return session.prewrap_quai(Some(&self.account), address, atoms, "split swap", fee).await;
            }
            _ => return Ok(None),
        };
        if !session.is_wquai(&from).await? {
            return Ok(None);
        }
        // An amount the step itself will not accept is left for the step to refuse.
        let Ok(atoms) = crate::amount::parse_amount(needed, 18) else { return Ok(None) };
        session.prewrap_quai(Some(&self.account), &from, atoms, purpose, self.max_fee.as_deref()).await
    }

    pub async fn next_review(&self, session: &mut Session) -> Result<Review> {
        let owner = session.account(Some(&self.account))?;
        if !owner.address.eq_ignore_ascii_case(&self.account) {
            return Err(CoreError::Rejected("saved trading owner must be an address".into()));
        }
        if let Some(review) = self.prewrap(session).await? {
            return Ok(review);
        }
        let account = Some(self.account.as_str());
        let fee = self.max_fee.as_deref();
        match &self.action {
            TradingAction::ProtocolConversion { direction, amount, slippage } => {
                let quote = session.conversion_quote(direction.as_str(), amount).await?;
                let tolerance = slippage.unwrap_or(quote.suggested_slippage_bps);
                if slippage.is_some() && tolerance < quote.suggested_slippage_bps {
                    return Err(CoreError::Rejected("manual conversion tolerance is below fresh advice; revise the plan".into()));
                }
                match direction {
                    crate::qi_market::Direction::QuaiToQi => session.review_convert_quai_to_qi(account, amount, tolerance, fee).await,
                    crate::qi_market::Direction::QiToQuai => session.review_convert_qi_to_quai(account, amount, tolerance, fee).await,
                }
            }
            TradingAction::MarketConversion { direction, amount, stage, wqi, slippage, deadline, .. } => {
                use crate::qi_market::Direction;
                if !session.network.wqi.as_ref().is_some_and(|configured| configured.eq_ignore_ascii_case(wqi)) {
                    return Err(CoreError::Rejected("saved conversion wrapper differs from this network".into()));
                }
                match (*direction, *stage) {
                    (Direction::QuaiToQi, 0) => {
                        session.swap_bounded_next(account, "quai", wqi, amount, *slippage, *deadline, fee, &Default::default()).await
                    }
                    (Direction::QuaiToQi, 1) => session.review_unwrap_wqi(account, amount, fee).await,
                    (Direction::QiToQuai, 0) => {
                        let address = crate::chain::addr(&self.account)?;
                        let gas = session.node.provider.gas_price(crate::network::ZONE).await?;
                        let needed = gas
                            .checked_mul(crate::sdk::U256::from(1_500_000))
                            .ok_or_else(|| CoreError::Invalid("conversion fee estimate overflow".into()))?;
                        if session.node.provider.balance(address, crate::sdk::BlockTag::Latest).await? < needed {
                            return Err(CoreError::Insufficient(
                                "fund this beneficiary with QUAI for the later claim, approvals and swap before wrapping Qi".into(),
                            ));
                        }
                        session.review_wrap_qi(account, amount, fee).await
                    }
                    (Direction::QiToQuai, 1) => session.review_claim_wqi(account, fee).await,
                    (Direction::QiToQuai, 2) => {
                        session.swap_bounded_next(account, wqi, "quai", amount, *slippage, *deadline, fee, &Default::default()).await
                    }
                    _ => Err(CoreError::Invalid("conversion plan has no remaining executable stage".into())),
                }
            }
            TradingAction::CrossVenue { from, hub, to, amount, stage, slippage, deadline } => {
                let destination = match stage {
                    0 => hub,
                    1 => to,
                    _ => return Err(CoreError::Invalid("invalid route stage".into())),
                };
                session
                    .swap_bounded_next(account, from, destination, amount, *slippage, *deadline, fee, &crate::swap::SwapBounds::default())
                    .await
            }
            TradingAction::Split { plan, index, deadline } => {
                session.review_split_allocation_next(account, plan, *index, *deadline, fee).await
            }
            TradingAction::Swap { from, to, amount, slippage, deadline } => {
                let q = session.swap_quote(account, from, to, amount, *slippage, crate::data::Trust::FirstHand).await?;
                if q.insufficient {
                    return Err(CoreError::Insufficient("swap input balance is insufficient".into()));
                }
                if q.legs.len() > 1 {
                    return Err(CoreError::Rejected("sequential routes require separate receipt-linked steps".into()));
                }
                if q.approval_needed {
                    session.review_swap_approval(account, from, to, amount, fee).await
                } else {
                    session.review_swap(account, from, to, amount, *slippage, *deadline, fee).await
                }
            }
            TradingAction::BoundedSwap { from, to, amount, slippage, deadline, bounds } => {
                session.swap_bounded_next(account, from, to, amount, *slippage, *deadline, fee, bounds).await
            }
            TradingAction::ExactOutput { from, to, output, max_input, deadline } => {
                session.swap_exact_output_next(account, from, to, output, max_input, *deadline, fee).await
            }
            TradingAction::AddLiquidity { pair, amount, side, slippage, deadline } => {
                session.add_liquidity_next(account, pair, amount, side.as_deref(), *slippage, *deadline, fee).await
            }
            TradingAction::RemoveLiquidity { pair, percent, slippage, deadline } => {
                session.remove_liquidity_next(account, pair, *percent, *slippage, *deadline, fee).await
            }
            TradingAction::Stake { pair, gauge, amount } => session.stake_next_in_gauge(account, pair, gauge.as_deref(), amount, fee).await,
            TradingAction::Incentivize { pair, token, amount, days } => {
                session.incentivize_next(account, pair, token, amount, *days, fee).await
            }
            TradingAction::CurveSell { token, symbol, curve, amount, slippage, deadline } => {
                session.curve_sell_next(account, token, symbol, curve, amount, *slippage, *deadline, fee).await
            }
        }
    }
}

/// Holds the interprocess coordinator lease until the client stops advancing this plan.
pub struct Coordinator {
    pub plan: TradePlan,
    _lease: std::fs::File,
}

impl Coordinator {
    pub fn create(session: &Session, label: &str, intent: TradingIntent) -> Result<Self> {
        let mut plan = TradePlan::new(
            session.network.id.clone(),
            intent.account.clone(),
            label.into(),
            serde_json::json!({"client": "trading", "intent": intent, "final_operation": null}),
        )?;
        let lease = Self::lease(session, &plan.id)?;
        session.app.save_trade_plan(&mut plan)?;
        Ok(Self { plan, _lease: lease })
    }

    fn lease(session: &Session, id: &str) -> Result<std::fs::File> {
        crate::plans::claim(&session.registry.paths().wallet_dir(&session.meta.id).join("plan-locks"), id)
    }

    pub fn resume(session: &Session, id: &str) -> Result<Self> {
        let lease = Self::lease(session, id)?;
        let mut plan = session.app.trade_plan(id)?.ok_or_else(|| CoreError::NotFound("trade plan".into()))?;
        if plan.network != session.network.id || plan.intent["client"] != "trading" {
            return Err(CoreError::Rejected("plan requires its original network and client".into()));
        }
        let intent: TradingIntent = serde_json::from_value(plan.intent["intent"].clone())?;
        if !intent.account.eq_ignore_ascii_case(&plan.owner) {
            return Err(CoreError::Rejected("plan owner changed".into()));
        }
        session.account(Some(&intent.account))?;
        let mut repaired = false;
        for op in session.app.operations_for_plan(&plan.network, &plan.id)? {
            if op.status == crate::appdb::OpStatus::Cancelled && op.tx_hash.is_none() {
                continue;
            }
            if !plan.operations.contains(&op.id) {
                plan.operations.push(op.id.clone());
                if !crate::flows::is_step_kind(&op.kind) {
                    plan.intent["final_operation"] = op.id.into();
                }
                repaired = true;
            }
        }
        if repaired {
            session.app.save_trade_plan(&mut plan)?;
        }
        Ok(Self { plan, _lease: lease })
    }

    pub fn discard_unsigned(&mut self, session: &mut Session) -> Result<()> {
        let mut retained = Vec::new();
        for id in &self.plan.operations {
            let op = session.app.operation(id)?.ok_or_else(|| CoreError::NotFound(format!("operation {id}")))?;
            if op.tx_hash.is_none() && matches!(op.status, crate::appdb::OpStatus::Prepared | crate::appdb::OpStatus::Cancelled) {
                if op.status == crate::appdb::OpStatus::Prepared {
                    session.abandon(id)?;
                }
                if self.plan.intent["final_operation"].as_str() == Some(id) {
                    self.plan.intent["final_operation"] = serde_json::Value::Null;
                }
            } else {
                retained.push(id.clone());
            }
        }
        self.plan.operations = retained;
        session.app.save_trade_plan(&mut self.plan)
    }

    /// Prepare only after the caller has refreshed canonical operation tracking. A prior signed
    /// candidate blocks new preparation even after a lost broadcast response or client crash.
    pub async fn prepare(&mut self, session: &mut Session) -> Result<Option<Review>> {
        match self.plan.readiness(&session.app)? {
            PlanReadiness::Stopped => return Err(CoreError::Rejected("trade plan is stopped".into())),
            PlanReadiness::Reconcile(id) => return Err(CoreError::Rejected(format!("reconcile operation {id} before resuming"))),
            PlanReadiness::ReviewRequired => {}
        }
        if let Some(id) = self.plan.intent["final_operation"].as_str() {
            let mut intent: TradingIntent = serde_json::from_value(self.plan.intent["intent"].clone())?;
            let op = session.app.operation(id)?.ok_or_else(|| CoreError::NotFound("split receipt".into()))?;
            let advanced = intent.advance_allocation(&op)?;
            self.plan.intent["intent"] = serde_json::to_value(intent)?;
            if advanced {
                self.plan.intent["final_operation"] = serde_json::Value::Null;
                session.app.save_trade_plan(&mut self.plan)?;
            } else {
                self.plan.state = PlanState::Complete;
                self.plan.reason =
                    "final operation included; inspect saved residuals; canonical tracking continues, irreversible finality is unverified"
                        .into();
                session.app.save_trade_plan(&mut self.plan)?;
                return Ok(None);
            }
        }
        if self.plan.operations.len() >= 8 {
            self.pause(session, "step budget exhausted; inspect approvals and balances")?;
            return Err(CoreError::Rejected(self.plan.reason.clone()));
        }
        self.plan.state = PlanState::Review;
        self.plan.reason = "preparing a fresh review; no stored authorization".into();
        session.app.save_trade_plan(&mut self.plan)?;
        let intent: TradingIntent = serde_json::from_value(self.plan.intent["intent"].clone())?;
        session.preparing_plan = Some(self.plan.id.clone());
        let result = intent.next_review(session).await;
        session.preparing_plan = None;
        let review = match result {
            Ok(review) => review,
            // Nothing was ever prepared under this plan, so there is nothing to resume: a trade
            // that could not start (not enough to pay, no route) ends here. Left paused, it became
            // the newest unfinished trade, which is what "resume" picks, over a real one stopped
            // halfway with a swap done and an allowance live.
            Err(error) if self.plan.operations.is_empty() => {
                self.plan.state = PlanState::Cancelled;
                self.plan.reason = format!("stopped before anything was prepared: {error}");
                session.app.save_trade_plan(&mut self.plan)?;
                return Err(error);
            }
            Err(error) => {
                self.pause(session, &error.to_string())?;
                return Err(error);
            }
        };
        self.plan.operations.push(review.op_id.clone());
        if !crate::flows::is_step(&review) {
            self.plan.intent["final_operation"] = review.op_id.clone().into();
        }
        self.plan.reason = "review prepared; explicit authorization required".into();
        if let Err(error) = session.app.save_trade_plan(&mut self.plan) {
            let _ = session.discard(&review.op_id);
            return Err(error);
        }
        Ok(Some(review))
    }

    pub fn pause(&mut self, session: &Session, reason: &str) -> Result<()> {
        self.plan.state = PlanState::Paused;
        self.plan.reason = format!("{reason}; completed transactions, assets and allowances remain");
        session.app.save_trade_plan(&mut self.plan)
    }

    pub fn submitted(&mut self, session: &Session) -> Result<()> {
        self.plan.state = PlanState::Waiting;
        self.plan.reason = "tracking the saved candidate before another step".into();
        session.app.save_trade_plan(&mut self.plan)
    }

    /// Cancel only future preparation. Release an unsigned saved review explicitly; signed bytes
    /// remain tracked and cancellation makes no claim that an onchain transaction was undone.
    pub fn cancel(&mut self, session: &mut Session) -> Result<()> {
        for id in &self.plan.operations {
            if let Some(op) = session.app.operation(id)?
                && op.status == crate::appdb::OpStatus::Prepared
                && op.tx_hash.is_none()
            {
                session.abandon(id)?;
            }
        }
        self.plan.state = PlanState::Cancelled;
        self.plan.reason = "future steps cancelled; inspect operation history for completed trades and unused allowances".into();
        session.app.save_trade_plan(&mut self.plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{appdb::OpStatus, config::AppConfig, network::NetworkProfile, paths::Paths, registry::Registry};
    use serde_json::json;

    fn fixture() -> (tempfile::TempDir, Session, TradingIntent) {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::fast(Paths::resolve(Some(dir.path().into())).unwrap());
        let meta = reg
            .create_hd(
                "plan",
                "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
                "english",
                "",
                "password123",
                true,
            )
            .unwrap();
        let session = Session::open(reg, AppConfig::default(), meta, NetworkProfile::builtins().remove(0)).unwrap();
        let intent = TradingIntent {
            account: session.account(None).unwrap().address,
            max_fee: Some("1".into()),
            action: TradingAction::Swap { from: "quai".into(), to: "USDT".into(), amount: "1".into(), slippage: 50, deadline: 10 },
        };
        (dir, session, intent)
    }

    #[test]
    fn market_conversion_conserves_attributed_output_and_explicit_residuals() {
        use crate::qi_market::Direction;
        let (_dir, session, mut intent) = fixture();
        let wqi = session.network.wqi.clone().unwrap();
        let action = TradingAction::MarketConversion {
            direction: Direction::QuaiToQi,
            amount: "1".into(),
            stage: 0,
            wqi: wqi.clone(),
            slippage: 50,
            deadline: 10,
            residual_atoms: "0".into(),
        };
        intent.action = action.clone();
        let mut op = session.new_op(
            crate::sdk::wallet::storage::ReservationId([63; 16]),
            crate::journal::OpKind::Swap,
            "quai",
            &intent.account,
            "QUAI",
            crate::sdk::U256::from(1),
            "router",
            json!({"actual_out":"2750000000000000000","to_token":wqi,"unrelated_balance_change":"3000000000000000000"}),
        );
        op.status = OpStatus::Confirmed;
        assert!(intent.advance_allocation(&op).unwrap());
        let TradingAction::MarketConversion { amount, stage, residual_atoms, .. } = &intent.action else { panic!() };
        assert_eq!(amount, "2");
        assert_eq!(*stage, 1);
        assert_eq!(residual_atoms, "750000000000000000");
        assert!(!intent.advance_allocation(&op).unwrap(), "a previous receipt cannot repeat the swap stage");
        intent.action = action.clone();
        op.detail.set_actual_out(json!("750000000000000000"));
        assert!(!intent.advance_allocation(&op).unwrap(), "sub-one-Qi fills finish with WQI residuals");
        assert!(
            matches!(&intent.action,TradingAction::MarketConversion{stage:2,residual_atoms,..} if residual_atoms=="750000000000000000")
        );
        intent.action = action;
        op.status = OpStatus::Unknown;
        assert!(intent.advance_allocation(&op).is_err(), "noncanonical prerequisite cannot advance");
    }

    #[test]
    fn reverse_market_conversion_waits_for_deposit_and_leaves_other_claims_unspent() {
        use crate::qi_market::Direction;
        let (_dir, session, mut intent) = fixture();
        let wqi = session.network.wqi.clone().unwrap();
        intent.action = TradingAction::MarketConversion {
            direction: Direction::QiToQuai,
            amount: "2".into(),
            stage: 0,
            wqi: wqi.clone(),
            slippage: 50,
            deadline: 10,
            residual_atoms: "0".into(),
        };
        let mut op = session.new_op(
            crate::sdk::wallet::storage::ReservationId([64; 16]),
            crate::journal::OpKind::WrapQi,
            "qi",
            "Qi wallet",
            "Qi",
            crate::sdk::U256::from(2000),
            &intent.account,
            json!({"beneficiary":intent.account}),
        );
        op.status = OpStatus::Confirmed;
        assert!(intent.advance_allocation(&op).is_err());
        op.status = OpStatus::Settled;
        assert!(intent.advance_allocation(&op).unwrap());
        let mut claim = session.new_op(
            crate::sdk::wallet::storage::ReservationId([65; 16]),
            crate::journal::OpKind::ClaimWqi,
            "quai",
            &intent.account,
            "WQI",
            crate::sdk::U256::from(5000),
            &wqi,
            json!({"to_token":wqi,"actual_out":"5000000000000000000"}),
        );
        claim.status = OpStatus::Confirmed;
        assert!(intent.advance_allocation(&claim).unwrap());
        assert!(matches!(&intent.action,TradingAction::MarketConversion{stage:2,amount,..} if amount=="2"));
        assert!(!intent.has_more_allocations());
    }

    #[test]
    fn journal_checkpoint_gap_and_signed_candidate_survive_restart_without_replay() {
        let (_dir, mut session, intent) = fixture();
        let runner = Coordinator::create(&session, "swap", intent.clone()).unwrap();
        let id = runner.plan.id.clone();
        assert!(Coordinator::resume(&session, &id).is_err(), "a second coordinator must not advance the same plan");
        session.begin_plan_preparation(&id).unwrap();
        let mut op = session.new_op(
            crate::sdk::wallet::storage::ReservationId([77; 16]),
            crate::journal::OpKind::Swap,
            "quai",
            &intent.account,
            "QUAI",
            crate::sdk::U256::from(1),
            "router",
            json!({}),
        );
        session.end_plan_preparation();
        op.status = OpStatus::Unknown;
        op.tx_hash = Some(format!("0x{}", "12".repeat(32)));
        session.app.insert_operation(&op).unwrap();
        drop(runner);
        let resumed = Coordinator::resume(&session, &id).unwrap();
        assert_eq!(resumed.plan.operations, vec![op.id.clone()]);
        assert_eq!(resumed.plan.intent["final_operation"], op.id);
        assert_eq!(resumed.plan.readiness(&session.app).unwrap(), PlanReadiness::Reconcile(op.id));
        drop(resumed);
        let again = Coordinator::resume(&session, &id).unwrap();
        assert_eq!(again.plan.operations.len(), 1, "repair is idempotent");
    }

    #[test]
    fn cancel_stops_future_work_and_scope_tampering_cannot_resume() {
        let (_dir, mut session, intent) = fixture();
        let mut runner = Coordinator::create(&session, "swap", intent).unwrap();
        runner.cancel(&mut session).unwrap();
        assert_eq!(runner.plan.readiness(&session.app).unwrap(), PlanReadiness::Stopped);
        let id = runner.plan.id.clone();
        let mut tampered = runner.plan.clone();
        tampered.owner = "other".into();
        session.app.save_trade_plan(&mut tampered).unwrap();
        drop(runner);
        assert!(Coordinator::resume(&session, &id).is_err());
    }
}
