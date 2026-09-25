//! Venues: where a trade can happen, in one place.
//!
//! Four of them are UniswapV2 exchanges that differ only in what [`Amm`] says: their pins, the
//! storage slot of their factory's pair map, how their pairs are found, what they are called.
//! Discovery, quoting, routing, proofs and liquidity are one implementation over that table
//! (`markets`, `swap`, `routes`, `liquidity`). Bonding curves are the other kind. Anything that
//! needs to know about a venue asks [`kind`]; nothing else names a particular exchange.
//!
//! Adding a UniswapV2 exchange is a [`Venue`] variant, a row in [`AMMS`], and its pins in the
//! network's ecosystem: this crate only.

use crate::capabilities::{Action, Family, Support};
use crate::markets::Venue;
use crate::network::{NetworkProfile, PinnedContract};

/// How an exchange's pairs are found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Discovery {
    /// Every pair its factory lists.
    Factory,
    /// Only the pairs the network profile names (`ecosystem.legacy_pairs`): a factory with
    /// look-alike pairs is read from a shortlist rather than guessed from.
    Allowlist,
}

/// How lists mark a venue beside a pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Badge {
    /// The main exchange: no mark.
    None,
    /// An exchange launches graduate into.
    Launch,
    /// An older deployment.
    Legacy,
    /// Still on its bonding curve.
    Curve,
}

/// What every venue says about itself.
pub trait VenueKind: Sync {
    fn venue(&self) -> Venue;
    /// How lists name it.
    fn label(&self) -> &'static str;
    /// How a sentence places a pair there ("on Quainance").
    fn on(&self) -> &'static str;
    fn badge(&self) -> Badge;
    /// Whether the router can trade it (a curve is bought and sold on the curve, never routed).
    fn routable(&self) -> bool;
    /// Its router and factory on a network, when it has them there.
    fn pins<'a>(&self, network: &'a NetworkProfile) -> Option<(&'a PinnedContract, &'a PinnedContract)>;
    /// Its family, for the capability table.
    fn family(&self) -> Option<Family>;
    /// Whether it supports `action`, and why not.
    fn support(&self, action: Action) -> Support {
        match self.family() {
            Some(family) => family.support(action),
            None => Support { supported: false, reason: Some("this venue's family depends on the launch") },
        }
    }
}

/// One UniswapV2 exchange.
pub struct Amm {
    pub venue: Venue,
    pub label: &'static str,
    pub on: &'static str,
    pub badge: Badge,
    pub family: Family,
    /// How the directory names its factory (in its sources and errors).
    pub factory_name: &'static str,
    /// Where the factory keeps `getPair[a][b]`, for storage proofs. Read against `getPair` at
    /// the same block on mainnet (2026-09-23); the live test
    /// `a_route_s_output_is_proven_from_its_pools` reads it again.
    pub get_pair_slot: u64,
    pub discovery: Discovery,
    /// The directory's cache key. Kept as it is: the cache is shared across processes and
    /// versions.
    pub cache_key: &'static str,
    /// What to say when the network has no such exchange.
    pub missing: &'static str,
    router: fn(&NetworkProfile) -> Option<&PinnedContract>,
    factory: fn(&NetworkProfile) -> Option<&PinnedContract>,
}

impl Amm {
    /// Its factory on a network.
    pub fn factory<'a>(&self, network: &'a NetworkProfile) -> Option<&'a PinnedContract> {
        (self.factory)(network)
    }

    /// Its router on a network.
    pub fn router<'a>(&self, network: &'a NetworkProfile) -> Option<&'a PinnedContract> {
        (self.router)(network)
    }
}

impl VenueKind for Amm {
    fn venue(&self) -> Venue {
        self.venue
    }
    fn label(&self) -> &'static str {
        self.label
    }
    fn on(&self) -> &'static str {
        self.on
    }
    fn badge(&self) -> Badge {
        self.badge
    }
    fn routable(&self) -> bool {
        true
    }
    fn pins<'a>(&self, network: &'a NetworkProfile) -> Option<(&'a PinnedContract, &'a PinnedContract)> {
        Some((self.router(network)?, self.factory(network)?))
    }
    fn family(&self) -> Option<Family> {
        Some(self.family)
    }
}

/// Every UniswapV2 exchange, the main one first.
pub static AMMS: [Amm; 4] = [
    Amm {
        venue: Venue::Main,
        label: "Quainance",
        on: "on Quainance",
        badge: Badge::None,
        family: Family::MainAmm,
        factory_name: "Quainance factory",
        get_pair_slot: 2,
        discovery: Discovery::Factory,
        cache_key: "main_factory_pools",
        missing: "no DEX factory configured on",
        router: |n| n.ecosystem.quainance_router.as_ref(),
        factory: |n| n.ecosystem.quainance_factory.as_ref(),
    },
    Amm {
        venue: Venue::LaunchAmm,
        label: "launch AMM",
        on: "on the launch AMM",
        badge: Badge::Launch,
        family: Family::LaunchAmm,
        factory_name: "launch AMM factory",
        get_pair_slot: 4,
        discovery: Discovery::Factory,
        cache_key: "launch_amm_pools_v2",
        missing: "no launch AMM on",
        router: |n| n.ecosystem.launch_amm_router.as_ref(),
        factory: |n| n.ecosystem.launch_amm_factory.as_ref(),
    },
    Amm {
        venue: Venue::Legacy,
        label: "QuaiSwap",
        on: "on QuaiSwap",
        badge: Badge::Legacy,
        family: Family::LegacyAmm,
        factory_name: "QuaiSwap factory",
        get_pair_slot: 2,
        discovery: Discovery::Allowlist,
        cache_key: "legacy_pools",
        missing: "no legacy exchange on",
        router: |n| n.ecosystem.legacy_router.as_ref(),
        factory: |n| n.ecosystem.legacy_factory.as_ref(),
    },
    Amm {
        venue: Venue::HartiiAmm,
        label: "revenue AMM",
        on: "on Quainance's revenue AMM",
        badge: Badge::Launch,
        family: Family::HartiiAmm,
        factory_name: "revenue AMM factory",
        get_pair_slot: 4,
        discovery: Discovery::Factory,
        cache_key: "hartii_amm_pools",
        missing: "Quainance's revenue AMM is not configured on",
        router: |n| n.ecosystem.hartii_amm_router.as_ref(),
        factory: |n| n.ecosystem.hartii_amm_factory.as_ref(),
    },
];

/// A launch still on its bonding curve.
pub struct Curve;

impl VenueKind for Curve {
    fn venue(&self) -> Venue {
        Venue::Curve
    }
    fn label(&self) -> &'static str {
        "bonding curve"
    }
    fn on(&self) -> &'static str {
        "on its bonding curve"
    }
    fn badge(&self) -> Badge {
        Badge::Curve
    }
    fn routable(&self) -> bool {
        false
    }
    fn pins<'a>(&self, _network: &'a NetworkProfile) -> Option<(&'a PinnedContract, &'a PinnedContract)> {
        None
    }
    /// Which curve system it is depends on the launch (`Pool::curve.venue_kind`).
    fn family(&self) -> Option<Family> {
        None
    }
}

static CURVE: Curve = Curve;

/// What a venue is.
pub fn kind(venue: Venue) -> &'static dyn VenueKind {
    match amm(venue) {
        Some(amm) => amm,
        None => &CURVE,
    }
}

/// The UniswapV2 exchange a venue is, if it is one.
pub fn amm(venue: Venue) -> Option<&'static Amm> {
    AMMS.iter().find(|a| a.venue == venue)
}

/// Every routable venue, the main one first.
pub fn routable() -> impl Iterator<Item = Venue> {
    AMMS.iter().map(|a| a.venue)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_exchange_is_one_row_and_every_row_is_complete() {
        let mainnet = NetworkProfile::builtins().into_iter().find(|n| n.id == "mainnet").unwrap();
        let mut seen = std::collections::HashSet::new();
        for amm in &AMMS {
            assert!(seen.insert(amm.venue), "{:?} appears once", amm.venue);
            assert!(amm.pins(&mainnet).is_some(), "{} is pinned on mainnet", amm.label);
            assert!(amm.routable() && kind(amm.venue).routable());
            assert!(Family::support(amm.family, Action::Swap).supported, "{} swaps", amm.label);
            assert!(matches!(amm.get_pair_slot, 2 | 4));
        }
        assert_eq!(routable().next(), Some(Venue::Main), "the main exchange first");
        assert!(!kind(Venue::Curve).routable() && kind(Venue::Curve).pins(&mainnet).is_none());
        assert_eq!(kind(Venue::Legacy).label(), "QuaiSwap");
        assert_eq!(kind(Venue::Legacy).on(), "on QuaiSwap");
    }
}
