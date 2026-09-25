//! Sealed messages, v3: a messaging identity of its own, weekly keys published as they are used,
//! and one HPKE seal per message, all on the existing `Messages` contract. The format is
//! `docs/MESSAGING_V3.md`; the byte layouts are [`wire`].
//!
//! The payment-code conversations of v1 and v2 are gone: nothing reads or writes them.

pub mod keys;
pub mod service;
pub mod store;
pub use quai_messaging::wire;
