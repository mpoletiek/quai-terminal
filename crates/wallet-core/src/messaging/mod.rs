//! Sealed messages, v3: a messaging identity of its own, weekly keys published as they are used,
//! and one HPKE seal per message, all on the existing `Messages` contract. The format is
//! `docs/MESSAGING_V3.md`; the byte layouts are [`wire`].
//!
//! v1 and v2 (payment-code conversations, [`crate::messages`]) are still read, never written.

pub mod keys;
pub mod service;
pub mod store;
pub use quai_messaging::wire;
