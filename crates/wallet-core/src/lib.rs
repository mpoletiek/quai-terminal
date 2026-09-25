//! Quai Terminal engine: identity, networks, state and wallet services shared by the CLI and TUI.

pub mod alerts;
pub mod anchor;
pub mod appdb;
pub mod capabilities;
pub mod chainstats;
pub mod chat;
pub mod cockpit;
mod commitments;
pub mod config;
pub mod contracts;
pub mod curve;
pub mod custody;
pub mod data;
pub mod execution;
pub mod extras;
pub mod flows;
pub mod gauge;
pub mod hartii;
pub mod hartii_tx;
pub mod identity;
pub mod launches;
pub mod liquidity;
pub mod market;
pub mod markets;
pub mod messages;
pub mod messaging;
pub mod multicall;
pub mod network;
pub mod ops;
pub mod orders;
pub mod paths;
pub mod plans;
pub mod pnl;
pub mod portfolio;
pub mod qi_market;
pub mod recipient;
mod recovery;
pub mod registry;
pub mod review_decoder;
pub mod routes;
pub mod session;
pub mod spendable;
pub mod split_routes;
pub mod subgraph;
pub mod swap;
pub mod track;
pub mod tx;
pub mod venues;
pub mod zone;

pub use error::{CoreError, Result};
// The domain types live in quai-model; these paths are kept for everything written against them.
pub use quai_model::{amount, chain, diag, error, journal, qi_exit, ser, testutil};
// Untrusted inputs live in quai-feeds.
pub use quai_feeds::{explorer, http, ipfs, media, media_helper, nft_uri};
pub use quai_sdk as sdk;
