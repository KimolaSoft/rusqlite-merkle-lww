//! The conformance harness of `merkle-lww`.
//!
//! The in-memory transport, the scripted clock, the two-node fixture, and the
//! topology and scenario fixtures. It exists so the interface is **provable**
//! and the failure modes are **deterministic**: a purge instant that arrives,
//! a quiesce window that opens and closes, a staleness horizon that is crossed
//! — all of them are otherwise a `sleep`, and a `sleep` is a flaky test.
//!
//! It is a crate, and not a directory of examples, so that the examples are
//! compiled and run.
//!
//! It is **not published**.
//!
//! **Status: not started.** The plan is [plan/demo.md](../../../plan/demo.md).

// Nothing yet. See `plan/demo.md` — D1 is the in-memory transport and the
// scripted clock.
