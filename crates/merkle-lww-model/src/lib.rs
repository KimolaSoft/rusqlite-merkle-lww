//! The vocabulary of `merkle-lww`.
//!
//! Everything in this crate must mean the same thing on every node, every
//! peer, and every implementation: the configuration types, the values, the
//! canonical encoding, the digest, the schema hash, the visibility hash, the
//! merge rules, the timestamp rules, and the error codes.
//!
//! It is **pure**. No SQLite, no runtime, no locks, no interior mutability.
//! Everything in it is an ordinary function over ordinary data, and its only
//! dependency is `serde`.
//!
//! # Why it is its own crate
//!
//! Two reasons, and both are load-bearing:
//!
//! - **The compatibility surface must be testable without a database, and
//!   portable to another language.** The golden vectors for the digest are
//!   consumable by an implementation that has never heard of `rusqlite`. That
//!   is the point of specifying an encoding precisely, and it cannot be true
//!   if the encoder is welded to a SQLite dependency.
//! - **The extension point must not carry the engine's weight.**
//!   [`merge::MergePolicyFn`] is what an *application* implements, and it is a
//!   pure function of `(incoming, local) -> MergeDecision`. Under a
//!   single-crate layout, writing one meant depending on `rusqlite`, `r2d2`
//!   and the whole engine.
//!
//! See [ADR-0024](../../../doc/adr/0024-the-workspace-is-split-into-six-crates.md).
//!
//! # Status
//!
//! **Stage M1** — the types, the errors, and time. The canonical encoding and
//! the digest are M2; the schema and visibility hashes are M3; the merge rules
//! are M4. See [plan/model.md](../../../plan/model.md).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod config;
pub mod error;
pub mod merge;
pub mod time;
pub mod value;
pub mod visibility;

pub use config::{
    AllocationSpec, CacheConfig, ColumnSpec, ColumnType, KeyStrategy, MergeMode, MergePolicy, Peer,
    PeerAuth, Role, SecretString, SyncConfig, TableKind, TableSpec, Uuid, Visibility,
    WriteAuthority, WriteMode,
};
pub use error::{Error, ErrorCode, Severity};
pub use merge::{MergeContext, MergeDecision, MergePolicyFn, RejectReason, Row};
pub use time::{format_rfc3339_micros, issue_monotonic, parse_rfc3339, MonotonicGuard};
pub use value::Value;
pub use visibility::{ProfileTable, VisibilityHash, VisibilityProfile};
