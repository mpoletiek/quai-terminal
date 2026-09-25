//! Sequences that need an exact approval before the action. Each call returns the next review to
//! sign — the approval while one is missing, then the action — so the CLI and the TUI walk the
//! same steps: sign what comes back, wait for it to confirm, and ask again.

use crate::journal::OpKind;
use crate::error::{CoreError, Result};
use crate::session::Session;
use crate::tx::Review;

/// Whether a review is a step before the action, after which the sequence asks again.
pub fn is_step(review: &Review) -> bool {
    is_step_kind(&review.kind)
}

/// [`is_step`] for a recorded operation's kind: an approval, or the QUAI wrapped to fund the
/// action ([`Session::prewrap_quai`]). No sequence ends on a wrap — Trade › Wrap is prepared on
/// its own, never through a sequence — so inside one a wrap is always a step.
pub fn is_step_kind(kind: &OpKind) -> bool {
    kind.is_sequence_step()
}

/// Gas a sequence may still spend once the shortfall is wrapped: the wrap itself, an approval
/// and the action. Kept back from the QUAI the wrap is allowed to take.
const PREWRAP_FEE_GAS: u64 = 1_000_000;

impl Session {
    /// Whether a token, named as a trade names it (`quai`, a symbol, an address), is this
    /// network's WQUAI.
    pub async fn is_wquai(&mut self, token: &str) -> Result<bool> {
        let Some(wquai) = self.network.wquai.clone() else { return Ok(false) };
        let token = token.trim();
        if token.eq_ignore_ascii_case(&wquai) {
            return Ok(true);
        }
        if token.eq_ignore_ascii_case("quai") {
            return Ok(false);
        }
        Ok(matches!(self.swap_asset(token).await?, crate::swap::SwapAsset::Token { address, .. } if address.eq_ignore_ascii_case(&wquai)))
    }

    /// Wrap QUAI to cover WQUAI a step is about to spend.
    ///
    /// Pools are priced in WQUAI, and a step that pays with the token itself — a swap from WQUAI,
    /// a deposit's WQUAI side — needs the token, not the coin. When the account holds too little
    /// WQUAI but enough QUAI for the rest and the fees still to come, this returns a review
    /// wrapping exactly the shortfall; the sequence asks again once it confirms. `None` when
    /// `token` is not WQUAI or none is missing. Too little of both is an error naming both.
    pub async fn prewrap_quai(
        &mut self,
        account: Option<&str>,
        token: &str,
        needed: crate::sdk::U256,
        purpose: &str,
        max_fee: Option<&str>,
    ) -> Result<Option<Review>> {
        use crate::sdk::{BlockTag, U256};
        if !self.is_wquai(token).await? {
            return Ok(None);
        }
        let Some(wquai) = self.network.wquai.clone() else { return Ok(None) };
        let owner = crate::chain::addr(&self.account(account)?.address)?;
        let held = crate::sdk::contracts::Erc20::new(crate::chain::addr(&wquai)?, &self.node.provider)?
            .balance_of(owner, owner, BlockTag::Latest)
            .await?;
        if held >= needed {
            return Ok(None);
        }
        let quai = self.node.provider.balance(owner, BlockTag::Latest).await?;
        let fees = self.node.provider.gas_price(crate::network::ZONE).await?.saturating_mul(U256::from(PREWRAP_FEE_GAS));
        let show = |atoms: U256| crate::amount::format_amount(atoms, 18);
        let missing = match prewrap_amount(needed, held, quai, fees) {
            Ok(None) => return Ok(None),
            Ok(Some(missing)) => missing,
            Err(missing) => {
                return Err(CoreError::Insufficient(format!(
                    "the {purpose} needs {} WQUAI; this account holds {} WQUAI and {} QUAI, too little to wrap the {} missing and pay the fees",
                    show(needed),
                    show(held),
                    show(quai),
                    show(missing)
                )));
            }
        };
        let mut review = self.review_wrap_quai(account, &show(missing), max_fee).await?;
        review.title = format!("Wrap QUAI → WQUAI for the {purpose}");
        review.warnings.insert(
            0,
            format!(
                "The {purpose} pays in WQUAI: this account holds {} and needs {}, so {} QUAI is wrapped first. The {purpose} is reviewed next.",
                show(held),
                show(needed),
                show(missing)
            ),
        );
        Ok(Some(review))
    }

    /// Reset an existing nonzero allowance before replacing it. Every returned amount gets its
    /// own frozen review; the subsequent step must re-read allowance and the selected spender.
    pub(crate) async fn bounded_allowance(
        &self,
        token: &str,
        owner: crate::sdk::QuaiAddress,
        spender: crate::sdk::QuaiAddress,
        desired: crate::sdk::U256,
    ) -> Result<crate::sdk::U256> {
        let token = crate::chain::addr(token)?;
        let allowance = crate::sdk::contracts::Erc20::new(token, &self.node.provider)?
            .allowance(owner, owner, spender, crate::sdk::BlockTag::Latest)
            .await?;
        Ok(next_allowance(allowance, desired))
    }
    /// Stake LP: the LP approval for the gauge, then the stake.
    pub async fn stake_next(&mut self, account: Option<&str>, pair: &str, amount: &str, max_fee: Option<&str>) -> Result<Review> {
        match self.review_stake(account, pair, amount, max_fee).await {
            Err(CoreError::ApprovalNeeded { .. }) => self.review_stake_approval(account, pair, amount, max_fee).await,
            other => other,
        }
    }

    /// Stake in an explicitly selected verified gauge when the pair appears in several.
    pub async fn stake_next_in_gauge(
        &mut self,
        account: Option<&str>,
        pair: &str,
        gauge: Option<&str>,
        amount: &str,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        match self.review_stake_in_gauge(account, pair, gauge, amount, max_fee).await {
            Err(CoreError::ApprovalNeeded { .. }) => self.review_stake_approval_in_gauge(account, pair, gauge, amount, max_fee).await,
            other => other,
        }
    }

    /// Fund a pool's rewards: the reward token's approval, then the funding.
    pub async fn incentivize_next(
        &mut self,
        account: Option<&str>,
        pair: &str,
        token: &str,
        amount: &str,
        days: u32,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        match self.review_incentivize(account, pair, token, amount, days, max_fee).await {
            Err(CoreError::ApprovalNeeded { .. }) => self.review_incentivize_approval(account, pair, token, amount, max_fee).await,
            other => other,
        }
    }

    /// Add liquidity: each side's approval that is still missing (the one the error names), then
    /// the deposit.
    pub async fn add_liquidity_next(
        &mut self,
        account: Option<&str>,
        pair: &str,
        amount: &str,
        side: Option<&str>,
        slippage_bps: u16,
        deadline_minutes: u32,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        match self.review_add_liquidity(account, pair, amount, side, slippage_bps, deadline_minutes, max_fee).await {
            // A WQUAI side short of its deposit is wrapped from QUAI, but only once the other
            // side is covered: wrapping for a deposit that still cannot go through helps nobody.
            Err(error @ CoreError::Insufficient(_)) => {
                let quote = self.add_liquidity_quote(pair, amount, side, slippage_bps, crate::data::Trust::FirstHand).await?;
                let wquai = self.network.wquai.clone().unwrap_or_default();
                let owner = crate::chain::addr(&self.account(account)?.address)?;
                let mut wrap = None;
                // The WQUAI side is wrapped up to its approval cap (the derived side's slippage
                // included), so a trade against the pool before the deposit cannot leave it short.
                for (side1, token, deposit) in [(false, &quote.token0, quote.amount0), (true, &quote.token1, quote.amount1)] {
                    if token.address.eq_ignore_ascii_case(&wquai) {
                        wrap = Some(quote.approval_cap(side1));
                    } else if crate::sdk::contracts::Erc20::new(crate::chain::addr(&token.address)?, &self.node.provider)?
                        .balance_of(owner, owner, crate::sdk::BlockTag::Latest)
                        .await?
                        < deposit
                    {
                        return Err(error);
                    }
                }
                let Some(needed) = wrap else { return Err(error) };
                match self.prewrap_quai(account, &wquai, needed, "deposit", max_fee).await? {
                    Some(review) => Ok(review),
                    None => Err(error),
                }
            }
            Err(CoreError::ApprovalNeeded { token, .. }) => {
                let quote = self.add_liquidity_quote(pair, amount, side, slippage_bps, crate::data::Trust::FirstHand).await?;
                let approve1 = quote.token1.address.eq_ignore_ascii_case(&token);
                self.review_add_approval(account, pair, amount, side, slippage_bps, approve1, max_fee).await
            }
            other => other,
        }
    }

    /// Sell to a bonding curve: the token's exact approval for the curve, then the sale.
    #[allow(clippy::too_many_arguments)]
    pub async fn curve_sell_next(
        &mut self,
        account: Option<&str>,
        token: &str,
        symbol: &str,
        curve: &str,
        amount: &str,
        slippage_bps: u16,
        deadline_minutes: u32,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        match self.review_curve_sell(account, token, symbol, curve, amount, slippage_bps, deadline_minutes, max_fee).await {
            Err(CoreError::ApprovalNeeded { .. }) => self.review_curve_sell_approval(account, token, symbol, curve, amount, max_fee).await,
            other => other,
        }
    }

    /// Remove liquidity: the LP approval for the router, then the withdrawal.
    pub async fn remove_liquidity_next(
        &mut self,
        account: Option<&str>,
        pair: &str,
        percent: u8,
        slippage_bps: u16,
        deadline_minutes: u32,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        match self.review_remove_liquidity(account, pair, percent, slippage_bps, deadline_minutes, max_fee).await {
            Err(CoreError::ApprovalNeeded { .. }) => self.review_remove_approval(account, pair, percent, max_fee).await,
            other => other,
        }
    }
}

/// How much QUAI to wrap so `held` WQUAI covers `needed`: nothing when it already does, the
/// shortfall when `quai` pays it and `fees` besides, and the shortfall as the error when not.
fn prewrap_amount(
    needed: crate::sdk::U256,
    held: crate::sdk::U256,
    quai: crate::sdk::U256,
    fees: crate::sdk::U256,
) -> std::result::Result<Option<crate::sdk::U256>, crate::sdk::U256> {
    let missing = needed.saturating_sub(held);
    if missing.is_zero() {
        Ok(None)
    } else if quai >= missing.saturating_add(fees) {
        Ok(Some(missing))
    } else {
        Err(missing)
    }
}

fn next_allowance(current: crate::sdk::U256, desired: crate::sdk::U256) -> crate::sdk::U256 {
    if current.is_zero() { desired } else { crate::sdk::U256::ZERO }
}

#[cfg(test)]
mod allowance_tests {
    use super::*;
    use crate::sdk::U256;
    #[test]
    fn bounded_replacement_requires_distinct_reset_then_approval() {
        assert_eq!(next_allowance(U256::from(4), U256::from(10)), U256::ZERO);
        assert_eq!(next_allowance(U256::ZERO, U256::from(10)), U256::from(10));
        assert_eq!(next_allowance(U256::MAX, U256::from(10)), U256::ZERO);
        assert_eq!(next_allowance(U256::ZERO, U256::ZERO), U256::ZERO);
    }

    /// Only the shortfall is wrapped, never the whole amount, and only when the QUAI left over
    /// still pays the fees for the wrap and what follows it.
    #[test]
    fn a_wquai_shortfall_is_wrapped_only_when_quai_covers_it_and_the_fees() {
        let n = U256::from;
        assert_eq!(prewrap_amount(n(100), n(100), n(0), n(5)), Ok(None), "enough WQUAI: nothing to wrap");
        assert_eq!(prewrap_amount(n(100), n(250), n(0), n(5)), Ok(None), "more than enough: nothing to wrap");
        assert_eq!(prewrap_amount(n(100), n(40), n(65), n(5)), Ok(Some(n(60))), "exactly the 60 missing, not the 100");
        assert_eq!(prewrap_amount(n(100), n(0), n(105), n(5)), Ok(Some(n(100))), "none held: all of it");
        assert_eq!(prewrap_amount(n(100), n(40), n(64), n(5)), Err(n(60)), "QUAI for the shortfall but not the fees");
        assert_eq!(prewrap_amount(n(100), n(40), n(10), n(5)), Err(n(60)), "too little of both");
    }

    /// A funding wrap continues a sequence as an approval does; nothing else is a step.
    #[test]
    fn a_funding_wrap_is_a_step_and_the_action_is_not() {
        assert!(is_step_kind(&crate::journal::OpKind::Approve));
        assert!(is_step_kind(&crate::journal::OpKind::WrapQuai));
        for kind in ["swap", "swap_exact_output", "add_liquidity", "wrap_qi", "unwrap_quai", "curve_buy"] {
            assert!(!is_step_kind(&crate::journal::OpKind::parse(kind)), "{kind}");
        }
    }
}
