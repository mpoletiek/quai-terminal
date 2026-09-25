//! The messaging protocol, with nothing around it: how the board's posts are encoded and
//! decoded, how a sealed conversation is derived, sealed and opened, and the v3 wire format
//! (`docs/MESSAGING_V3.md`). Reading the board from a node, the stores and the session live in
//! wallet-core, which consumes this.
//!
//! Everything decoded here was written by strangers; see [`board`].

pub mod board;
pub mod wire;
