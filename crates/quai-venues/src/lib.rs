//! Venues: where a trade can happen, what each one can do, and the contracts each is pinned to.
//! Pure data and tables: no node, no store, no keys. Discovery, quoting, routing and proofs over
//! these tables are wallet-core's (`markets`, `swap`, `routes`, `liquidity`).

pub mod capabilities;
pub mod pins;
pub mod table;

use serde::{Deserialize, Serialize};

/// Where a market trades. Quainance runs three venues, and a router only reaches its own.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Venue {
    /// The main exchange: the pinned Quainance factory and router.
    #[default]
    Main,
    /// The exchange a bonding curve graduates into: its own factory and router.
    LaunchAmm,
    /// A launch still on its bonding curve. It is bought and sold on the curve, never routed.
    Curve,
    /// The older UniswapV2 deployment, which Quainance's own frontend calls `legacyFactory` and
    /// GeckoTerminal lists as `quaiswap`. Still traded, and its own router serves it.
    ///
    /// Only the pairs named in `ecosystem.legacy_pairs` are read. The factory holds eighteen, two
    /// symbols appear on it twice at different addresses, and a directory that cannot tell those
    /// apart is worse than one that admits it is a shortlist.
    Legacy,
    /// Quainance's second curve system (its frontend's `revenueCurveSystem`): a UniswapV2 factory
    /// and router that the revenue launcher's curves graduate into, independently pinned. Named
    /// `HartiiAmm` because it was first found through HartiiLabs, whose treasury seeded a QAXE pool
    /// on it and whose docs call it "Quainance V2"; it is Quainance's, and says so on screen.
    HartiiAmm,
}

impl Venue {
    /// How lists name it.
    pub fn label(self) -> &'static str {
        crate::table::kind(self).label()
    }

    /// `on Quainance`, `on the launch AMM`: where a swap happens, in a sentence.
    pub fn on(self) -> &'static str {
        crate::table::kind(self).on()
    }

    /// Whether a swap router trades here.
    pub fn routable(self) -> bool {
        crate::table::kind(self).routable()
    }

    /// How lists mark it beside a pair.
    pub fn badge(self) -> crate::table::Badge {
        crate::table::kind(self).badge()
    }
}
