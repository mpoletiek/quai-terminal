//! What "MAX" means for each ledger.
//!
//! Filling an amount field with the whole balance is only simple for ERC-20s. Native QUAI has to
//! keep back what the transaction itself will cost, and Qi is a fixed-denomination UTXO ledger
//! where the fee depends on how many coins get spent — so its maximum is not the balance either.
//!
//! Getting this wrong is worse than having no MAX at all: an over-filled amount strands the user
//! with no gas, or reproduces the "each small coin spent adds fee" refusal from [`crate::ops`].

use crate::amount::{self, QI_DECIMALS, QUAI_DECIMALS};
use quai_sdk::U256;

/// Gas a swap over `hops` pools may burn. Mirrors the `max_gas` in
/// [`crate::session::Session::review_swap`] so the reserve matches what the review will ask for.
pub fn swap_max_gas(hops: usize) -> u64 {
    400_000 + 150_000 * hops as u64
}

/// The deepest route the router will build, and so the gas MAX must budget for.
pub const MAX_ROUTE_HOPS: usize = 3;

/// What a MAX fill produced, and what it had to hold back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaxAmount {
    /// The amount to put in the field, in base units.
    pub amount: U256,
    /// Held back to pay for the transaction (native QUAI only).
    pub reserved: U256,
    /// Decimals of the asset, for formatting.
    pub decimals: u8,
}

impl MaxAmount {
    /// Nothing is left after the reserve: the balance cannot cover its own fee.
    pub fn is_zero(&self) -> bool {
        self.amount.is_zero()
    }

    /// The amount as the field should show it.
    pub fn text(&self) -> String {
        amount::format_amount(self.amount, self.decimals)
    }

    /// A one-line explanation for the toast, when something was held back or rounded away.
    pub fn note(&self) -> Option<String> {
        (!self.reserved.is_zero())
            .then(|| format!("MAX leaves {} QUAI for fees", amount::format_amount_short(self.reserved, QUAI_DECIMALS, 4)))
    }
}

/// MAX for an ERC-20: the whole balance, nothing held back.
pub fn token_max(balance: U256, decimals: u8) -> MaxAmount {
    MaxAmount { amount: balance, reserved: U256::ZERO, decimals }
}

/// MAX for native QUAI: the balance minus what the transaction will cost.
///
/// `gas_price` is the zone's current price. The reserve is deliberately the **worst-case** route,
/// because the user may change the receive token after hitting MAX and a 3-hop route costs more
/// gas than the 1-hop route that was on screen.
pub fn quai_max(balance: U256, gas_price: U256, max_gas: u64) -> MaxAmount {
    let reserved = gas_price.saturating_mul(U256::from(max_gas)).min(balance);
    MaxAmount { amount: balance.saturating_sub(reserved), reserved, decimals: QUAI_DECIMALS }
}

/// One spendable Qi coin: its value in qits, and the fee weight of spending it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Coin {
    /// Value in qits.
    pub qits: u64,
    /// Denomination index (smaller coins cost proportionally more to spend).
    pub denomination: u8,
}

/// What a Qi MAX could not include, so the caller can say why rather than silently shrink.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QiMax {
    /// Advisory fill amount in qits, whole-Qi floored when requested. Preparation must reprice it.
    pub amount: U256,
    /// Coins the transaction would spend.
    pub coins_used: usize,
    /// Coins omitted by the input limit or because their value does not exceed the fee heuristic.
    pub coins_left: usize,
    /// Value in qits of the coins left out.
    pub dust_qits: u64,
    /// Estimated fee for the selection, in qits.
    pub fee_qits: u64,
}

impl QiMax {
    /// Some of the balance could not be reached in one transaction.
    pub fn truncated(&self) -> bool {
        self.coins_left > 0
    }

    /// The amount as the field should show it.
    pub fn text(&self) -> String {
        amount::format_amount(self.amount, QI_DECIMALS)
    }

    /// Why the number is below the balance, when it is.
    pub fn note(&self) -> Option<String> {
        self.truncated().then(|| {
            format!(
                "{} coins ({} Qi) excluded by input limits or estimated cost — refresh fees before deciding whether to consolidate",
                self.coins_left,
                amount::format_amount_short(U256::from(self.dust_qits), QI_DECIMALS, 3)
            )
        })
    }
}

/// A Qi transaction cannot spend more inputs than this. Mirrors `QiPolicy::max_inputs` in
/// [`crate::ops`]; a balance spread over more coins than this simply cannot be spent at once.
pub const MAX_QI_INPUTS: usize = 64;

/// Legacy display heuristic per input, in qits. This is not a current network fee quote and does
/// not guarantee that the filled amount prepares. Exact sweep quotes use `qi_exit::quote_sweep`.
pub const QI_FEE_PER_INPUT: u64 = 5;

/// Advisory Qi amount fill until the caller obtains a shape-specific fee quote.
///
/// Coins are taken largest first, because the fee is per input — spending two big coins beats
/// spending forty small ones for the same value. Everything that does not fit is reported rather
/// than quietly dropped, so the caller can point at `qi consolidate` instead of showing a number
/// the user cannot explain.
///
/// `whole_qi` floors the result for whole-Qi redemption. Conversion and wrapping can be
/// fractional; their executable MAX comes from `Session::quote_qi_special_max` instead.
pub fn qi_max(coins: &[Coin], whole_qi: bool) -> QiMax {
    let mut sorted: Vec<Coin> = coins.iter().copied().filter(|c| c.qits > 0).collect();
    sorted.sort_by(|a, b| b.qits.cmp(&a.qits).then(a.denomination.cmp(&b.denomination)));
    let mut used = Vec::new();
    let mut left = Vec::new();
    for coin in sorted {
        if used.len() < MAX_QI_INPUTS && coin.qits > QI_FEE_PER_INPUT {
            used.push(coin);
        } else {
            left.push(coin);
        }
    }
    let gross: u64 = used.iter().map(|c| c.qits).sum();
    let fee = QI_FEE_PER_INPUT.saturating_mul(used.len() as u64);
    let mut net = gross.saturating_sub(fee);
    if whole_qi {
        // Qi has 3 decimals; a whole Qi is 1000 qits.
        let per_qi = 10u64.pow(u32::from(QI_DECIMALS));
        net -= net % per_qi;
    }
    QiMax {
        amount: U256::from(net),
        coins_used: used.len(),
        coins_left: left.len(),
        dust_qits: left.iter().map(|c| c.qits).sum(),
        fee_qits: fee,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wei(n: u128) -> U256 {
        U256::from(n)
    }

    #[test]
    fn a_token_max_is_the_whole_balance() {
        let m = token_max(wei(12_345), 6);
        assert_eq!(m.amount, wei(12_345));
        assert!(m.reserved.is_zero());
        assert_eq!(m.note(), None, "nothing was held back, so nothing to explain");
        assert_eq!(m.text(), "0.012345");
    }

    /// The reserve is real money on Quai: gas runs about 2.4e13 wei, so a 3-hop swap is ~20 QUAI.
    #[test]
    fn quai_max_holds_back_a_worst_case_fee() {
        let price = wei(23_800_691_942_794);
        let balance = wei(100) * wei(10u128.pow(18));
        let m = quai_max(balance, price, swap_max_gas(MAX_ROUTE_HOPS));
        assert_eq!(m.amount + m.reserved, balance, "nothing is lost, only moved");
        assert!(!m.reserved.is_zero());
        // 850,000 gas at that price is about 20 QUAI.
        let reserved: f64 = amount::to_f64(m.reserved, QUAI_DECIMALS);
        assert!((20.0..21.0).contains(&reserved), "reserved {reserved} QUAI");
        assert!(m.note().unwrap().starts_with("MAX leaves 20."));
        // A deeper route reserves more, so changing the receive token after MAX cannot strand you.
        assert!(quai_max(balance, price, swap_max_gas(1)).amount > m.amount);
    }

    #[test]
    fn quai_max_never_goes_negative() {
        let price = wei(23_800_691_942_794);
        // A balance far too small to cover its own fee yields zero, not an underflow.
        let m = quai_max(wei(1_000), price, swap_max_gas(MAX_ROUTE_HOPS));
        assert!(m.is_zero());
        assert_eq!(m.reserved, wei(1_000), "the reserve is capped at the balance");
        assert_eq!(quai_max(U256::ZERO, price, 21_000).amount, U256::ZERO);
    }

    #[test]
    fn qi_max_prefers_big_coins_and_pays_per_input() {
        let coins = vec![Coin { qits: 10_000, denomination: 10 }, Coin { qits: 5_000, denomination: 9 }, Coin { qits: 1, denomination: 0 }];
        let m = qi_max(&coins, false);
        assert_eq!(m.coins_used, 2);
        assert_eq!(m.coins_left, 1);
        assert_eq!(m.fee_qits, 10, "two economical inputs at the display heuristic");
        assert_eq!(m.amount, U256::from(15_000u64 - 10));
        assert_eq!(m.amount, qi_max(&coins[..2], false).amount, "adding uneconomic dust cannot lower the fill");
        assert!(m.note().unwrap().contains("estimated cost"));
    }

    /// The limit that makes a naive `MAX = spendable` wrong: 200 coins cannot be spent at once.
    #[test]
    fn qi_max_stops_at_the_input_limit_and_says_so() {
        let coins: Vec<Coin> = (0..200).map(|i| Coin { qits: 1_000 + i, denomination: 7 }).collect();
        let m = qi_max(&coins, false);
        assert_eq!(m.coins_used, MAX_QI_INPUTS);
        assert_eq!(m.coins_left, 200 - MAX_QI_INPUTS);
        assert!(m.truncated());
        assert!(m.note().unwrap().contains("consolidate"), "{:?}", m.note());
        // The biggest coins were taken, so the amount beats a naive first-64 selection.
        let biggest: u64 = (200 - MAX_QI_INPUTS as u64..200).map(|i| 1_000 + i).sum();
        assert_eq!(m.amount, U256::from(biggest - m.fee_qits));
    }

    #[test]
    fn qi_max_floors_to_whole_qi_when_the_destination_needs_it() {
        let coins = vec![Coin { qits: 7_777, denomination: 10 }];
        let loose = qi_max(&coins, false);
        assert_eq!(loose.amount, U256::from(7_772u64), "7777 less a 5-qit fee");
        let whole = qi_max(&coins, true);
        assert_eq!(whole.amount, U256::from(7_000u64), "whole Qi only");
        assert_eq!(whole.text(), "7", "trailing zeros are trimmed, so the field reads cleanly");
        // Below one whole Qi there is nothing to send.
        assert_eq!(qi_max(&[Coin { qits: 400, denomination: 3 }], true).amount, U256::ZERO);
    }

    #[test]
    fn qi_max_handles_an_empty_or_dust_only_wallet() {
        assert_eq!(qi_max(&[], false).amount, U256::ZERO);
        assert_eq!(qi_max(&[], false).coins_used, 0);
        // Coins worth less than their own fee net to zero rather than underflowing.
        let m = qi_max(&[Coin { qits: 1, denomination: 0 }, Coin { qits: 2, denomination: 0 }], false);
        assert_eq!(m.amount, U256::ZERO);
        // Zero-value coins are ignored entirely.
        assert_eq!(qi_max(&[Coin { qits: 0, denomination: 0 }], false).coins_used, 0);
    }
}
