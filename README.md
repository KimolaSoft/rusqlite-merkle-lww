# rusqlite-merkle-lww

Last-write-wins replication for SQLite, with a time-bucketed Merkle tree used to compute
the minimal set of differences between peers.

> **Status: design phase.** No implementation code exists yet. The documentation in
> [`doc/`](doc/) specifies the system; [`PLAN.md`](PLAN.md) sequences the work, one plan
> per crate.

## What it is

A Rust crate (workspace) that keeps several SQLite databases in sync with each other,
over HTTP(S), using a Merkle tree bucketed by wall-clock time to work out *what changed*
without transferring rows that did not.

- **Last-write-wins**, ordered by an RFC 3339 `last_update` timestamp.
- **Merkle diffing** over three levels of time bucket (hour / 24-hour cycle / 30-cycle
  month), hashed with xxh3-128.
- **Soft deletes** with a shared purge schedule, so deletions replicate and cannot
  resurrect.
- **The application keeps writing plain SQL.** Sync invariants are enforced inside SQLite
  by triggers and an authorizer callback, not by wrapping the query language.

## Reading order

| Document | What it covers |
|---|---|
| [doc/README.md](doc/README.md) | Index, glossary, conventions |
| [doc/implementation.md](doc/implementation.md) | **For application developers** — how to wire the crate in |
| [doc/architecture.md](doc/architecture.md) | Roles, topology, crate layout, threading, transport |
| [doc/data-model.md](doc/data-model.md) | Schema, key strategies, visibility, encoding |
| [doc/bulk-apply.md](doc/bulk-apply.md) | Bulk row application, parameter limits, chunking |
| [doc/examples.md](doc/examples.md) | Worked examples: a users DB and a groups DB |
| [doc/sqlite-interface.md](doc/sqlite-interface.md) | Triggers, authorizer, write dialects, DDL |
| [doc/merkle.md](doc/merkle.md) | Buckets, hashing, comparison depth, snapshots |
| [doc/protocol.md](doc/protocol.md) | Wire protocol, sessions, progress reporting |
| [doc/lifecycle.md](doc/lifecycle.md) | Deletes, purge schedule, resurrection, staleness |
| [doc/security.md](doc/security.md) | Trust model, peer identity, restricted columns |
| [doc/config.md](doc/config.md) | Configuration reference |
| [doc/testing.md](doc/testing.md) | Test strategy and acceptance criteria |
| [doc/operations.md](doc/operations.md) | Deployment, monitoring, CLI, troubleshooting |
| [doc/adr/](doc/adr/) | Architecture decision records — the *why* behind the *what* |

## Build status

There is nothing to build yet. The plan is in [`PLAN.md`](PLAN.md), which indexes one
plan per crate:

| Plan | Crate |
|---|---|
| [`plan/model.md`](plan/model.md) | `merkle-lww-model` — the vocabulary: config, codec, digest, hashes, merge rules |
| [`plan/protocol.md`](plan/protocol.md) | `merkle-lww-protocol` — the wire contract: messages, JSON shapes, handshake rules |
| [`plan/core.md`](plan/core.md) | `merkle-lww-core` — the engine over SQLite: store, tree, diff, session, lifecycle |
| [`plan/axum.md`](plan/axum.md) | `merkle-lww-axum` — the transport: REST, WebSocket, progress, compression |
| [`plan/cli.md`](plan/cli.md) | `merkle-lww-cli` — the binary: `serve`, `pull`, `push`, `inspect`, `tree`, `verify` |
| [`plan/demo.md`](plan/demo.md) | `merkle-lww-demo` — the conformance harness: in-memory transport, scripted clock, fixtures |

## Licence

Dual-licensed under MIT or Apache-2.0, at your option. See [LICENSE](LICENSE),
[LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE).
