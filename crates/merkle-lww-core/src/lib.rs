//! The engine of `merkle-lww`, over SQLite.
//!
//! Everything that touches a database lives here: the store, the triggers, the
//! authorizer, the log, the bucket tree, the comparison, the bulk apply path,
//! the session, the paging, the lifecycle, the cache, the directory, the
//! registry, the allocator, and the session journal.
//!
//! It is **synchronous**: no `tokio`, no `axum`. The async boundary belongs to
//! the transport.
//!
//! See [ADR-0024](../../../doc/adr/0024-the-workspace-is-split-into-six-crates.md) for why the workspace is split this way.
//!
//! **Status: not started.** The plan is [plan/core.md](../../../plan/core.md).

// Nothing yet. See `plan/core.md` — C1 is the store and the write path.
