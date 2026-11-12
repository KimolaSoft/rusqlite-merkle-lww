//! The wire contract of `merkle-lww`.
//!
//! This crate holds the message types, the tagged value form, the handshake
//! validation rules, the traits the session needs from the engine, and the
//! `Transport` trait the application implements.
//!
//! It is **pure**: no SQLite, no runtime, no locks. Everything in it is an
//! ordinary function over ordinary data, so every handshake rejection case is
//! a unit test with no fixture.
//!
//! See [ADR-0024](../../../doc/adr/0024-the-workspace-is-split-into-six-crates.md) for why the workspace is split this way.
//!
//! **Status: not started.** The plan is [plan/protocol.md](../../../plan/protocol.md).

// Nothing yet. See `plan/protocol.md` — P1 is the messages and the wire values.
