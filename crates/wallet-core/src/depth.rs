//! What a trade does to the price of constant-product pools (x·y = k): the AMM's stand-in for
//! an order book's depth. Quai has no order book; a pool's depth at a price is how much can be
//! traded before its rate moves that far.
//!
//! Price impact here is the pools' own movement, LP fees aside (they are a fixed share on top,
//! `swap::LP_FEE_BPS` a hop). Display only: the quote reads the router on-chain regardless.

/// One pool along a route, in the direction traded: what goes in, what comes out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hop {
    pub reserve_in: f64,
    pub reserve_out: f64,
}

impl Hop {
    /// A pool's hop, `pay0` when the token paid in is its token0.
    pub fn of(pool: &crate::markets::Pool, pay0: bool) -> Hop {
        if pay0 {
            Hop { reserve_in: pool.reserve0, reserve_out: pool.reserve1 }
        } else {
            Hop { reserve_in: pool.reserve1, reserve_out: pool.reserve0 }
        }
    }

    fn usable(&self) -> bool {
        self.reserve_in.is_finite() && self.reserve_out.is_finite() && self.reserve_in > 0.0 && self.reserve_out > 0.0
    }
}

/// What `amount` in fetches through the hops, LP fees aside.
pub fn out(hops: &[Hop], amount: f64) -> f64 {
    hops.iter().fold(amount, |x, h| h.reserve_out * x / (h.reserve_in + x))
}

/// The route's rate for a trade too small to move it: out per unit in.
pub fn spot(hops: &[Hop]) -> f64 {
    hops.iter().map(|h| h.reserve_out / h.reserve_in).product()
}

/// How much worse than spot `amount`'s own rate is, as a fraction (0.01 is 1%).
pub fn impact(hops: &[Hop], amount: f64) -> f64 {
    if amount <= 0.0 || hops.is_empty() || !hops.iter().all(Hop::usable) {
        return 0.0;
    }
    (1.0 - out(hops, amount) / (amount * spot(hops))).clamp(0.0, 1.0)
}

/// The largest trade whose impact stays within `fraction`. Impact grows with size, so a bisection
/// finds it; for one hop it is exactly `fraction · reserve_in / (1 − fraction)`.
pub fn size_for(hops: &[Hop], fraction: f64) -> Option<f64> {
    if hops.is_empty() || !hops.iter().all(Hop::usable) || !(0.0..1.0).contains(&fraction) || fraction == 0.0 {
        return None;
    }
    let (mut lo, mut hi) = (0.0, hops[0].reserve_in);
    while impact(hops, hi) < fraction {
        hi *= 2.0;
        if !hi.is_finite() {
            return None;
        }
    }
    for _ in 0..80 {
        let mid = (lo + hi) / 2.0;
        if impact(hops, mid) < fraction {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Some(lo)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-9 * b.abs().max(1.0)
    }

    #[test]
    fn one_hop_matches_the_constant_product() {
        let h = [Hop { reserve_in: 1_000.0, reserve_out: 2_000.0 }];
        assert!(close(spot(&h), 2.0));
        // 100 in: 2000·100/1100 out, against 200 at spot.
        assert!(close(out(&h, 100.0), 2_000.0 * 100.0 / 1_100.0));
        assert!(close(impact(&h, 100.0), 100.0 / 1_100.0));
        // 1% impact: 1000·0.01/0.99.
        assert!(close(size_for(&h, 0.01).unwrap(), 1_000.0 * 0.01 / 0.99));
    }

    #[test]
    fn a_route_compounds_its_pools_and_a_shallow_hop_dominates() {
        let deep = Hop { reserve_in: 1_000_000.0, reserve_out: 1_000_000.0 };
        let shallow = Hop { reserve_in: 1_000.0, reserve_out: 500.0 };
        let route = [deep, shallow];
        assert!(close(spot(&route), 0.5));
        let alone = size_for(&[shallow], 0.02).unwrap();
        let through = size_for(&route, 0.02).unwrap();
        assert!(through < alone && through > alone * 0.95, "{through} vs {alone}");
        assert!(close(impact(&route, through), 0.02) || (impact(&route, through) - 0.02).abs() < 1e-9);
    }

    #[test]
    fn empty_or_broken_pools_say_nothing() {
        assert_eq!(impact(&[], 5.0), 0.0);
        assert_eq!(size_for(&[Hop { reserve_in: 0.0, reserve_out: 5.0 }], 0.01), None);
        assert_eq!(size_for(&[Hop { reserve_in: 5.0, reserve_out: 5.0 }], 0.0), None);
    }
}
