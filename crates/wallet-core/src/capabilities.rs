//! The capability table ([`quai_venues::capabilities`]) and what needs a pool to answer.
pub use quai_venues::capabilities::*;

use crate::markets::Pool;

/// The family a pool trades in: its venue's, or for a curve, the one its launch was verified as.
/// Display labels never select one.
pub fn family_for_pool(pool: &Pool) -> Option<Family> {
    match crate::venues::kind(pool.venue).family() {
        Some(family) => Some(family),
        None => pool.curve.as_ref()?.venue_kind,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markets::Venue;
    #[test]
    fn hartii_amm_pins_match_reproducible_execution_evidence() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!("../tests/fixtures/hartii_amm_runtime_evidence.json")).unwrap();
        let network = crate::network::NetworkProfile::builtins().remove(0);
        let (router, factory) = crate::swap::venue_pins(&network, Venue::HartiiAmm).unwrap();
        for (pin, key) in [(router, "router"), (factory, "factory")] {
            assert!(pin.address.eq_ignore_ascii_case(fixture[key]["address"].as_str().unwrap()));
            assert_eq!(pin.code_hash.as_deref(), fixture[key]["hash"].as_str());
        }
        assert_eq!(
            fixture["token"]["runtimes"].as_array().unwrap().iter().map(|v| v["decimals"].as_u64().unwrap()).collect::<Vec<_>>(),
            vec![6, 8, 18]
        );
        assert!(Family::HartiiAmm.support(Action::NativeSwap).supported);
        assert!(Family::HartiiAmm.support(Action::AddLiquidity).supported);
        assert!(!Family::HartiiAmm.support(Action::ClaimCurve).supported);
    }

    #[test]
    fn every_advertised_launch_family_has_an_explicit_adapter() {
        for venue in crate::launches::LAUNCH_VENUES {
            assert!(Family::for_launch_venue(venue).is_some(), "{venue}");
        }
        assert!(Family::for_launch_venue("new_unknown_curve").is_none());
    }
    #[test]
    fn display_labels_never_select_a_curve_adapter() {
        let mut pool = Pool {
            venue: Venue::Curve,
            curve: Some(crate::markets::CurveMark { launchpad: Some("HartiiLabs".into()), ..Default::default() }),
            ..Default::default()
        };
        assert_eq!(family_for_pool(&pool), None);
        pool.curve.as_mut().unwrap().venue_kind = Some(Family::QuainanceCurve);
        assert_eq!(family_for_pool(&pool), Some(Family::QuainanceCurve));
        pool.curve.as_mut().unwrap().launchpad = Some("attacker supplied label".into());
        assert_eq!(family_for_pool(&pool), Some(Family::QuainanceCurve));
    }
}
