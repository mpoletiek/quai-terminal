//! Bounded, integer split planning for independently reviewed V2 transactions.
//!
//! No aggregator is assumed. Pool-disjoint routes are simulated independently; a proposal is
//! returned only when its estimated net output strictly exceeds every supplied single route.
//! Each leg still needs a fresh first-hand quote, review, reservation and receipt checkpoint.

use crate::amount;
use crate::error::{CoreError, Result};
use crate::markets::Venue;
use crate::swap::{self, SwapAsset, SwapQuote};
use quai_sdk::U256;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const MAX_CANDIDATES: usize = 8;
pub const MAX_SLICES: u16 = 100;
pub const MAX_QUOTE_AGE: u64 = 30;

#[derive(Clone, Debug, Serialize)]
pub struct SplitCandidate {
    /// An authoritative quote at a probe size. Its raw reserves must reproduce that quote.
    pub quote: SwapQuote,
    /// Conservative execution fee including approvals/resets for up to the entire input cap.
    pub fee_native_estimate: String,
    /// Missing conversion means no net-benefit recommendation can be made.
    pub fee_output_estimate: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SplitAllocation {
    pub candidate_index: usize,
    pub venue: Venue,
    pub router: String,
    pub path: Vec<String>,
    pub pools: Vec<String>,
    pub amount_in: String,
    pub expected_out: String,
    /// Applies only to this transaction, not atomically to the full split.
    pub minimum_out: String,
    pub observed_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SplitPlan {
    pub from: SwapAsset,
    pub to: SwapAsset,
    pub total_input: String,
    pub expected_output: String,
    pub net_output_estimate: String,
    pub baseline_net_output_estimate: String,
    pub fee_native_estimate: String,
    pub fee_output_estimate: String,
    pub allocations: Vec<SplitAllocation>,
    pub slippage_bps: u16,
    pub native_funds_sufficient: Option<bool>,
    pub sequential: bool,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SplitDecision {
    pub plan: Option<SplitPlan>,
    pub reason: String,
    pub baseline_net_output_estimate: Option<String>,
    pub evaluations: usize,
}

/// Validate a persisted proposal before reviewing any allocation. Execution additionally
/// authenticates deployments and re-quotes the selected path first-hand.
pub fn validate_plan(plan: &SplitPlan) -> Result<()> {
    swap::validate_slippage(plan.slippage_bps)?;
    if !plan.sequential || plan.allocations.len() != 2 || plan.from == plan.to {
        return Err(CoreError::Invalid("a split plan must contain two sequential allocations for different assets".into()));
    }
    let mut total = U256::ZERO;
    let mut pools = HashSet::new();
    for allocation in &plan.allocations {
        let input = atoms(&allocation.amount_in)?;
        let minimum = atoms(&allocation.minimum_out)?;
        if input.is_zero()
            || minimum.is_zero()
            || minimum > atoms(&allocation.expected_out)?
            || !allocation.venue.routable()
            || allocation.pools.is_empty()
            || allocation.pools.len() > 3
            || allocation.path.len() != allocation.pools.len() + 1
        {
            return Err(CoreError::Invalid("invalid protected split allocation".into()));
        }
        total = total.checked_add(input).ok_or_else(|| CoreError::Invalid("split allocation overflow".into()))?;
        for pool in &allocation.pools {
            if pool.is_empty() || !pools.insert(pool.to_lowercase()) {
                return Err(CoreError::Invalid("split allocations share a pool".into()));
            }
        }
    }
    if total != atoms(&plan.total_input)? {
        return Err(CoreError::Invalid("split allocations do not conserve the authorized input".into()));
    }
    Ok(())
}

fn atoms(raw: &str) -> Result<U256> {
    U256::from_str_radix(raw, 10).map_err(|_| CoreError::Invalid("split amounts must be decimal base-unit integers".into()))
}

struct Candidate {
    hops: Vec<(U256, U256)>,
    pools: HashSet<String>,
    fee_native: U256,
    fee_output: Option<U256>,
}

impl Candidate {
    fn output(&self, input: U256) -> Option<U256> {
        let out = self.hops.iter().fold(input, |amount, (rin, rout)| swap::amount_out(amount, *rin, *rout));
        (!out.is_zero()).then_some(out)
    }
    fn permitted_output(&self, input: U256) -> Option<U256> {
        (swap::impact_bps(input, &self.hops) < swap::IMPACT_REFUSE_BPS).then(|| self.output(input)).flatten()
    }
}

fn validate(sources: &[SplitCandidate], at: u64) -> Result<Vec<Candidate>> {
    let Some(first) = sources.first() else { return Err(CoreError::Invalid("split planning needs route candidates".into())) };
    let mut result = Vec::new();
    for source in sources {
        let q = &source.quote;
        if q.from != first.quote.from || q.to != first.quote.to || q.slippage_bps != first.quote.slippage_bps {
            return Err(CoreError::Invalid("split candidates must use identical asset identities, decimals and slippage".into()));
        }
        swap::validate_slippage(q.slippage_bps)?;
        if q.observed_at == 0 || q.observed_at > at || at - q.observed_at > MAX_QUOTE_AGE {
            return Err(CoreError::Invalid("split candidate is stale; refresh all routes".into()));
        }
        if q.legs.len() != 1
            || !q.legs[0].venue.routable()
            || q.legs[0].router != q.router
            || q.legs[0].path != q.path
            || q.pools.is_empty()
            || q.pools.len() > 3
            || q.path.len() != q.pools.len() + 1
            || q.fee_bps != swap::LP_FEE_BPS * q.pools.len() as u64
        {
            return Err(CoreError::Invalid("split candidates must be single-router conventional V2 paths".into()));
        }
        let mut pools = HashSet::new();
        let mut hops = Vec::new();
        for hop in &q.pools {
            if hop.pair.is_empty() || !pools.insert(hop.pair.to_lowercase()) {
                return Err(CoreError::Invalid("split route repeats a pool".into()));
            }
            let (rin, rout) = (atoms(&hop.reserve_in)?, atoms(&hop.reserve_out)?);
            if rin.is_zero() || rout.is_zero() || rin.bit_len() > 112 || rout.bit_len() > 112 {
                return Err(CoreError::Invalid("split route has invalid uint112 reserves".into()));
            }
            hops.push((rin, rout));
        }
        if q.path.iter().map(|s| s.to_lowercase()).collect::<HashSet<_>>().len() != q.path.len() {
            return Err(CoreError::Invalid("split path repeats a token".into()));
        }
        let candidate = Candidate {
            hops,
            pools,
            fee_native: atoms(&source.fee_native_estimate)?,
            fee_output: source.fee_output_estimate.as_deref().map(atoms).transpose()?,
        };
        if candidate.output(atoms(&q.amount_in)?) != Some(atoms(&q.amount_out)?) {
            return Err(CoreError::Invalid("split reserve snapshot does not reproduce the authoritative probe; refresh routes".into()));
        }
        result.push(candidate);
    }
    Ok(result)
}

fn allocation(total: U256, slice: u16, slices: u16) -> (U256, U256) {
    let first = amount::mul_div(total, U256::from(slice), U256::from(slices)).expect("fraction cannot exceed input");
    (first, total - first)
}

/// At most 28 route pairs × 99 allocations. `slices` describes a bounded grid, not a claim of
/// continuous global optimality. Exact remainder allocation conserves even non-divisible inputs.
pub fn optimize(sources: &[SplitCandidate], total: U256, slices: u16, at: u64, native_balance: Option<U256>) -> Result<SplitDecision> {
    if total.is_zero() || sources.len() > MAX_CANDIDATES || !(2..=MAX_SLICES).contains(&slices) {
        return Err(CoreError::Invalid("split requires positive input, at most 8 candidates and 2–100 slices".into()));
    }
    let candidates = validate(sources, at)?;
    let mut decision = SplitDecision {
        plan: None,
        reason: "no split improves estimated net output after all execution fees".into(),
        baseline_net_output_estimate: None,
        evaluations: 0,
    };
    if candidates.iter().any(|c| c.fee_output.is_none()) {
        decision.reason = "output-token fee conversion is unavailable; split net benefit is unknown".into();
        return Ok(decision);
    }
    let baseline =
        candidates.iter().filter_map(|c| c.output(total).map(|out| out.saturating_sub(c.fee_output.unwrap()))).max().unwrap_or_default();
    decision.baseline_net_output_estimate = Some(baseline.to_string());
    let mut best_net = baseline;
    let native_input = matches!(sources[0].quote.from, SwapAsset::Quai).then_some(total).unwrap_or_default();
    let mut funding_rejected = false;
    for i in 0..candidates.len() {
        for j in (i + 1)..candidates.len() {
            let (a, b) = (&candidates[i], &candidates[j]);
            // Shared-pool interactions require joint state simulation; never pretend independent outputs add up.
            if !a.pools.is_disjoint(&b.pools) {
                continue;
            }
            let fee_native =
                a.fee_native.checked_add(b.fee_native).ok_or_else(|| CoreError::Invalid("split native fee overflow".into()))?;
            let fee_output = a
                .fee_output
                .unwrap()
                .checked_add(b.fee_output.unwrap())
                .ok_or_else(|| CoreError::Invalid("split output fee overflow".into()))?;
            let funding = native_input.checked_add(fee_native).ok_or_else(|| CoreError::Invalid("split funding overflow".into()))?;
            if native_balance.is_some_and(|balance| balance < funding) {
                funding_rejected = true;
                continue;
            }
            for slice in 1..slices {
                decision.evaluations += 1;
                let (input_a, input_b) = allocation(total, slice, slices);
                if input_a.is_zero() || input_b.is_zero() {
                    continue;
                }
                let (Some(out_a), Some(out_b)) = (a.permitted_output(input_a), b.permitted_output(input_b)) else { continue };
                let gross = out_a.checked_add(out_b).ok_or_else(|| CoreError::Invalid("split output overflow".into()))?;
                let net = gross.saturating_sub(fee_output);
                if net <= best_net {
                    continue;
                }
                let minima =
                    [swap::minimum_out(out_a, sources[0].quote.slippage_bps), swap::minimum_out(out_b, sources[0].quote.slippage_bps)];
                if minima.iter().any(U256::is_zero) {
                    continue;
                }
                let allocation = |index: usize, input: U256, output: U256, minimum: U256| {
                    let q = &sources[index].quote;
                    SplitAllocation {
                        candidate_index: index,
                        venue: q.legs[0].venue,
                        router: q.router.clone(),
                        path: q.path.clone(),
                        pools: q.pools.iter().map(|p| p.pair.to_lowercase()).collect(),
                        amount_in: input.to_string(),
                        expected_out: output.to_string(),
                        minimum_out: minimum.to_string(),
                        observed_at: q.observed_at,
                    }
                };
                decision.plan = Some(SplitPlan {
                    from: sources[0].quote.from.clone(), to: sources[0].quote.to.clone(), total_input: total.to_string(),
                    expected_output: gross.to_string(), net_output_estimate: net.to_string(), baseline_net_output_estimate: baseline.to_string(),
                    fee_native_estimate: fee_native.to_string(), fee_output_estimate: fee_output.to_string(),
                    allocations: vec![allocation(i, input_a, out_a, minima[0]), allocation(j, input_b, out_b, minima[1])],
                    slippage_bps: sources[0].quote.slippage_bps, native_funds_sufficient: native_balance.map(|balance| balance >= funding), sequential: true,
                    warnings: vec!["Sequential split: each allocation is separately reviewed and confirmed; there is no atomic combined-output minimum.".into(),
                        "A failed later leg leaves the earlier fill and unspent input in the wallet. Resume from confirmed receipts, never restart filled legs.".into(),
                        "Gas, prices and net benefit are estimates from current snapshots; re-quote before each leg. This bounded grid is not a global optimum guarantee.".into()],
                });
                best_net = net;
            }
        }
    }
    if decision.plan.is_some() {
        decision.reason = "split improves estimated net output on the bounded allocation grid".into();
    } else if funding_rejected {
        decision.reason = "available native funds cannot pay the additional split execution fees".into();
    }
    Ok(decision)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swap::{PoolHop, SwapLeg};

    // Independent u128 oracle: no call to the implementation's amount_out/optimizer helpers.
    fn oracle_out(input: u128, rin: u128, rout: u128) -> u128 {
        input * 997 * rout / (rin * 1000 + input * 997)
    }

    fn candidate(id: usize, rin: u128, rout: u128, fee: u128) -> SplitCandidate {
        let input = 50;
        let from = SwapAsset::Token { address: "a".into(), symbol: "A".into(), decimals: 8 };
        let to = SwapAsset::Token { address: "b".into(), symbol: "B".into(), decimals: 6 };
        let hops = vec![PoolHop { pair: format!("pool{id}"), reserve_in: rin.to_string(), reserve_out: rout.to_string(), tvl_usd: None }];
        let path: Vec<String> = vec!["a".into(), "b".into()];
        let output = oracle_out(input, rin, rout).to_string();
        let leg = SwapLeg {
            venue: Venue::Main,
            router: format!("router{id}"),
            path: path.clone(),
            route: vec!["A".into(), "B".into()],
            pools: hops.clone(),
            amount_in: input.to_string(),
            amount_out: output.clone(),
            minimum_out: output.clone(),
            output_decimals: 6,
        };
        SplitCandidate {
            quote: SwapQuote {
                from,
                to,
                amount_in: input.to_string(),
                amount_out: output.clone(),
                minimum_out: output,
                slippage_bps: 0,
                path,
                route: vec!["A".into(), "B".into()],
                pools: hops,
                impact_bps: 0,
                fee_bps: 30,
                router: format!("router{id}"),
                allowance: Some("1000000000".into()),
                approval_needed: false,
                balance: Some("1000000000".into()),
                insufficient: false,
                warnings: vec![],
                observed_at: 100,
                liquidity_at: None,
                legs: vec![leg],
            },
            fee_native_estimate: fee.to_string(),
            fee_output_estimate: Some(fee.to_string()),
        }
    }

    #[test]
    fn persisted_allocations_reject_double_spending_shared_pools_and_weakened_shape() {
        let sources = [candidate(0, 500, 5000, 0), candidate(1, 500, 5000, 0)];
        let plan = optimize(&sources, U256::from(100), 100, 100, None).unwrap().plan.unwrap();
        assert!(validate_plan(&plan).is_ok());
        let mut broken = plan.clone();
        broken.allocations[1].amount_in = "100".into();
        assert!(validate_plan(&broken).is_err());
        let mut broken = plan.clone();
        broken.allocations[1].pools = broken.allocations[0].pools.clone();
        assert!(validate_plan(&broken).is_err());
        let mut broken = plan.clone();
        broken.allocations[1].minimum_out = "0".into();
        assert!(validate_plan(&broken).is_err());
        let mut broken = plan;
        broken.sequential = false;
        assert!(validate_plan(&broken).is_err());
    }

    #[test]
    fn allocations_conserve_full_width_inputs_and_exact_remainders() {
        for total in [U256::from(3), U256::from(101), U256::MAX] {
            for slice in 1..100 {
                let (a, b) = allocation(total, slice, 100);
                assert_eq!(a.checked_add(b), Some(total));
                assert!(a <= total && b <= total);
            }
        }
        assert_eq!(allocation(U256::from(101), 50, 100), (U256::from(50), U256::from(51)));
    }

    #[test]
    fn bounded_grid_matches_independent_exhaustive_integer_oracle() {
        for (rin_a, rout_a, rin_b, rout_b, fee) in [(300, 1000, 700, 2300, 0), (600, 1800, 500, 1400, 1), (400, 1000, 450, 1100, 3)] {
            let sources = [candidate(0, rin_a, rout_a, fee), candidate(1, rin_b, rout_b, fee)];
            for total in 2..=100u128 {
                let baseline = oracle_out(total, rin_a, rout_a).max(oracle_out(total, rin_b, rout_b)).saturating_sub(fee);
                let expected = (1..total)
                    .filter_map(|input| {
                        let (a, b) = (oracle_out(input, rin_a, rout_a), oracle_out(total - input, rin_b, rout_b));
                        (a > 0 && b > 0).then_some((a + b).saturating_sub(fee * 2))
                    })
                    .max()
                    .unwrap_or(0);
                let result = optimize(&sources, U256::from(total), 100, 100, None).unwrap();
                assert!(result.evaluations <= 99);
                match result.plan {
                    Some(plan) => {
                        assert!(expected > baseline);
                        assert_eq!(plan.net_output_estimate, expected.to_string());
                        let allocated: U256 = plan.allocations.iter().map(|a| atoms(&a.amount_in).unwrap()).sum();
                        assert_eq!(allocated, U256::from(total));
                        assert!(plan.sequential);
                    }
                    None => assert!(expected <= baseline, "total={total}, expected={expected}, baseline={baseline}"),
                }
            }
        }
    }

    #[test]
    fn extra_gas_shared_pools_missing_prices_and_funding_prevent_false_benefit() {
        let low_fee = [candidate(0, 500, 5000, 0), candidate(1, 500, 5000, 0)];
        assert!(optimize(&low_fee, U256::from(100), 100, 100, None).unwrap().plan.is_some());
        let high_fee = [candidate(0, 500, 5000, 100), candidate(1, 500, 5000, 100)];
        assert!(optimize(&high_fee, U256::from(100), 100, 100, None).unwrap().plan.is_none());
        let mut shared = low_fee.clone();
        shared[1].quote.pools[0].pair = shared[0].quote.pools[0].pair.to_uppercase();
        let result = optimize(&shared, U256::from(100), 100, 100, None).unwrap();
        assert!(result.plan.is_none());
        assert_eq!(result.evaluations, 0);
        let mut unknown = low_fee.clone();
        unknown[0].fee_output_estimate = None;
        assert!(optimize(&unknown, U256::from(100), 100, 100, None).unwrap().reason.contains("unknown"));
        let mut native = [candidate(0, 500, 5000, 1), candidate(1, 500, 5000, 1)];
        for source in &mut native {
            source.quote.from = SwapAsset::Quai;
        }
        assert!(optimize(&native, U256::from(100), 100, 100, Some(U256::from(101))).unwrap().plan.is_none());
        let plan = optimize(&native, U256::from(100), 100, 100, Some(U256::from(102))).unwrap().plan.unwrap();
        assert_eq!(plan.native_funds_sufficient, Some(true));
    }

    #[test]
    fn malformed_incoherent_stale_and_unbounded_proposals_are_rejected() {
        let sources = [candidate(0, 500, 5000, 0), candidate(1, 500, 5000, 0)];
        assert!(optimize(&sources, U256::from(100), 101, 100, None).is_err());
        assert!(optimize(&sources, U256::ZERO, 100, 100, None).is_err());
        assert!(optimize(&sources, U256::from(100), 100, 131, None).is_err());
        for mutation in 0..4 {
            let mut invalid = sources.clone();
            match mutation {
                0 => invalid[0].quote.amount_out = "1".into(),
                1 => invalid[0].quote.pools[0].reserve_out = "0".into(),
                2 => invalid[0].quote.path.push("c".into()),
                _ => invalid[0].quote.to = SwapAsset::Quai,
            }
            assert!(optimize(&invalid, U256::from(100), 100, 100, None).is_err());
        }
        let many: Vec<_> = (0..9).map(|id| candidate(id, 500, 5000, 0)).collect();
        assert!(optimize(&many, U256::from(100), 100, 100, None).is_err());
        let result = optimize(&many[..8], U256::from(100), 100, 100, None).unwrap();
        assert_eq!(result.evaluations, 2772);
        let odd = optimize(&sources, U256::from(101), 100, 100, None).unwrap().plan.unwrap();
        assert_eq!(atoms(&odd.allocations[0].amount_in).unwrap() + atoms(&odd.allocations[1].amount_in).unwrap(), U256::from(101));
        let mut overflow = sources;
        for source in &mut overflow {
            source.fee_native_estimate = U256::MAX.to_string();
        }
        assert!(optimize(&overflow, U256::from(100), 100, 100, None).is_err());
    }
}
