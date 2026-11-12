//! The transport of `merkle-lww`.
//!
//! REST, WebSocket, progress reporting, and compression. This is where the
//! async boundary is, and where `tokio` lives: the engine below it is
//! synchronous.
//!
//! Compression is a **transport-level content encoding**, not part of the
//! protocol. See
//! [ADR-0023](../../../doc/adr/0023-json-wire-format-and-transport-compression.md)
//! for why the wire format is JSON and why zstd level 1 is the default on the
//! REST path.
//!
//! **Status: not started.** The plan is [plan/axum.md](../../../plan/axum.md).

// Nothing yet. See `plan/axum.md` — A1 is REST.
