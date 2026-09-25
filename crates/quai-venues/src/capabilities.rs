//! Stable adapter families and actions. This table selects a workflow; execution must still
//! authenticate the selected deployment and token/pair relationships first-hand.
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

    #[test]
    fn legacy_lp_actions_and_curve_claims_are_separate_capabilities() {
        assert!(Family::LegacyAmm.support(Action::AddLiquidity).supported);
        assert!(Family::LegacyAmm.support(Action::RemoveLiquidity).supported);
        assert!(Family::QuainanceCurve.support(Action::ClaimCurve).supported);
        assert!(!Family::HartiiCurve.support(Action::ClaimCurve).supported);
        assert!(!Family::MainAmm.support(Action::BuyCurve).supported);
    }
}
