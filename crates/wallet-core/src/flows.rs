//! Sequences that need an exact approval before the action. Each call returns the next review to
//! sign — the approval while one is missing, then the action — so the CLI and the TUI walk the
//! same steps: sign what comes back, wait for it to confirm, and ask again.

use crate::error::{CoreError, Result};
use crate::session::Session;
use crate::tx::Review;

/// Whether a review is a step before the action, after which the sequence asks again.
pub fn is_step(review: &Review) -> bool {
    review.kind == "approve"
}

impl Session {
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
}
