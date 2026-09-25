//! Quai Terminal's domain types: amounts, operations and their kinds, journal details and
//! receipts, and the error every layer speaks. Nothing here reaches a network, a vault or a
//! database of its own; everything above it (the engine, the venues, the feeds, the clients)
//! shares these types.

pub mod amount;
pub mod chain;
pub mod diag;
pub mod error;
pub mod journal;
pub mod qi_exit;
pub mod ser;
#[doc(hidden)]
pub mod testutil;
pub mod text;
pub mod time;

pub use error::{CoreError, Result};
pub use quai_sdk as sdk;
