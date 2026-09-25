//! Stable adapter families and actions. This table selects a workflow; execution must still
//! authenticate the selected deployment and token/pair relationships first-hand.
use crate::markets::Pool;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Family {
    MainAmm,
    LaunchAmm,
    LegacyAmm,
    HartiiAmm,
    QuainanceCurve,
    HartiiCurve,
    CoreGauge,
    ZoneGauge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Discover,
    Quote,
    Swap,
    AddLiquidity,
    RemoveLiquidity,
    BuyCurve,
    SellCurve,
    ClaimCurve,
    Stake,
    Unstake,
    Harvest,
    NativeSwap,
    ExactOutput,
    Split,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Support {
    pub supported: bool,
    pub reason: Option<&'static str>,
}

impl Family {
    pub fn for_pool(pool: &Pool) -> Option<Self> {
        match crate::venues::kind(pool.venue).family() {
            Some(family) => Some(family),
            None => pool.curve.as_ref()?.venue_kind,
        }
    }

    pub fn for_launch_venue(venue: &str) -> Option<Self> {
        match venue {
            "QUAINANCE_CURVE" => Some(Self::QuainanceCurve),
            "QUAINANCE_CURVE_AMM" => Some(Self::LaunchAmm),
            "QUAINANCE_AMM" => Some(Self::MainAmm),
            "HARTII_CURVE" => Some(Self::HartiiCurve),
            _ => None,
        }
    }

    pub fn support(self, action: Action) -> Support {
        use Action::*;
        use Family::*;
        let supported = match self {
            MainAmm | LaunchAmm | LegacyAmm | HartiiAmm => {
                matches!(action, Discover | Quote | Swap | AddLiquidity | RemoveLiquidity | NativeSwap | ExactOutput | Split)
            }
            QuainanceCurve => matches!(action, Discover | Quote | BuyCurve | SellCurve | ClaimCurve),
            HartiiCurve => matches!(action, Discover | Quote | BuyCurve | SellCurve),
            CoreGauge | ZoneGauge => matches!(action, Discover | Stake | Unstake | Harvest),
        };
        let reason = if supported {
            None
        } else {
            Some(match self {
                HartiiCurve => "Hartii sends native proceeds and excess directly; it has no claim action",
                _ => "this adapter does not provide this action",
            })
        };
        Support { supported, reason }
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
        assert_eq!(Family::for_pool(&pool), None);
        pool.curve.as_mut().unwrap().venue_kind = Some(Family::QuainanceCurve);
        assert_eq!(Family::for_pool(&pool), Some(Family::QuainanceCurve));
        pool.curve.as_mut().unwrap().launchpad = Some("attacker supplied label".into());
        assert_eq!(Family::for_pool(&pool), Some(Family::QuainanceCurve));
    }

    #[test]
    fn legacy_lp_actions_and_curve_claims_are_separate_capabilities() {
        assert!(Family::LegacyAmm.support(Action::AddLiquidity).supported);
        assert!(Family::LegacyAmm.support(Action::RemoveLiquidity).supported);
        assert!(Family::QuainanceCurve.support(Action::ClaimCurve).supported);
        assert!(!Family::HartiiCurve.support(Action::ClaimCurve).supported);
        assert!(!Family::MainAmm.support(Action::BuyCurve).supported);
    }
}
