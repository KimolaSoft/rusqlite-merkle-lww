# ADR-0024: The workspace is split into six crates

- **Status:** accepted
- **Date:** 2026-11
- **Deciders:** project maintainers

- **Partially supersedes:** the crate layout in [../architecture.md](../architecture.md)
  § "Crate layout", which previously described three crates

## Context

The design was written against a single crate, `merkle-lww-core`, holding 21 modules.
Three crates were then sketched — `merkle-lww-core`, `merkle-lww-axum`,
`merkle-lww-cli` — and the split that mattered was the one the transport boundary
forced.

But "the DB part and the transport part" is two axes, not one. The crate split has to
answer two independent questions: **what needs SQLite**, and **what needs a runtime**.
The 21 modules answer to both:

| Modules | SQLite | Runtime |
|---|---|---|
| `config`, `merge`, `visibility`, `codec`, `schema`, `time`, `error` | no | no |
| `store`, `trigger`, `authorizer`, `log`, `merkle`, `diff`, `bulk`, `cache`, `registry`, `allocator`, `directory`, `session_store`, `session`, `paging` | yes | no |
| the REST and WebSocket handlers | yes | yes |

`lifecycle.rs` is **new in this split** — the purge schedule, the quiesce window and the
staleness rule were spread across `session.rs` and `store.rs` before it.

Three things were wrong with folding the first group into the second.

**The compatibility surface was not independently testable.** The digest rules, the
`schema_hash` and the `visibility_hash` are exactly what two independently implemented
peers must agree on byte for byte. They are the things `codec_version` versions, and
the things a golden-vector fixture pins. Tested inside a crate that depends on
`rusqlite`, they cannot be exercised without a database — and the fixtures are supposed
to be checkable by an implementation in another language, which cannot use them at all
if they are welded to a SQLite dependency.

**The extension-point surface carried the heaviest dependency.** `MergePolicyFn` is what
an *application* implements. Under the single-crate layout, writing a pure function of
`(incoming, local) -> MergeDecision` meant depending on `rusqlite`, `r2d2` and the whole
engine. That is a bad dependency shape for the one place the design most wants
third-party code.

**The wire contract had no home.** ADR-0011 says the crate owns the exchange, so two
implementations must agree on the bytes. The message types were scattered across the
core, and the handshake validation — the part where `EPOCH_MISMATCH`,
`DIGEST_SEED_MISMATCH`, `VISIBILITY_MISMATCH` and the rest are raised — was entangled
with the database queries that gather the facts it checks.

There was also a practical reason to split now rather than later: **there is no code
yet.** A crate boundary drawn before the first line exists is a diagram; the same
boundary drawn afterwards is a refactor across four crates and a published API. The
cost of splitting is lowest now and rises with every module written.

## Decision

**The workspace has six crates, and the dependency edges point one way.**

```
merkle-lww-model      pure types, codec, digest, merge, visibility, hashes, time, errors
      ▲
merkle-lww-protocol   the wire contract: message types, JSON shapes, handshake rules
      ▲
merkle-lww-core       the engine over SQLite: store, triggers, log, tree, diff,
      ▲               session, paging, lifecycle, cache, registry, allocator
merkle-lww-axum       the transport: REST, WebSocket, progress, compression
      ▲
merkle-lww-cli        the binary: serve · pull · push · inspect · tree · verify

merkle-lww-demo       the conformance harness: in-memory transport, scripted clock,
                      two-node fixture, topology and scenario fixtures
                      (depends on core, protocol and model)
```

### `merkle-lww-model`

The vocabulary. `config.rs`, `merge.rs`, `visibility.rs`, `codec.rs`, `schema.rs`,
`time.rs`, `error.rs`. It depends on `serde` and on nothing else — no SQLite, no
runtime.

It holds the things that must mean the same thing everywhere: the canonical encoding
and the seeded xxh3 digest, the `schema_hash`, the `visibility_hash`, the merge
decision rules, the configuration types, the timestamp rules, and the error codes.

### `merkle-lww-protocol`

The wire contract. The message types (`Hello`, `Summary`, `Detail`, `Rows`, `Commit`,
`Progress`, `Cursor`, `Projection`, `Origin`), their JSON shapes, the tagged value form,
the wire version `merkle-lww/1`, and the **handshake validation rules**.

The validation rules are **pure functions over a gathered view**:

```rust
pub fn validate_handshake(local: &LocalView, peer: &Hello) -> Result<Hello>;
```

`LocalView` is the set of facts the rules need — the epoch, the codec version, the two
hashes, the retention and purge schedule, the known peers, the last sync instant.
`core` gathers it from the database and calls the function. The rules themselves need
no database, so they are unit-testable in isolation and are the same code on both sides
of a session.

The crate also owns **the traits the session needs from the engine**, which `core`
implements, and **the `Transport` trait**, which the application implements. That
direction — the consumer declares the interface — is what keeps the dependency edge
pointing one way. The `Transport` trait is in terms of *messages*, not bytes, which is
what keeps compression outside the protocol.

### `merkle-lww-core`

The engine. Everything that touches SQLite: `store.rs`, `trigger.rs`, `authorizer.rs`,
`log.rs`, `merkle.rs`, `diff.rs`, `bulk.rs`, `cache.rs`, `session.rs`, `paging.rs`,
`lifecycle.rs`, `directory.rs`, `registry.rs`, `allocator.rs`, `session_store.rs`. It
depends on
`rusqlite` and `r2d2`, and it stays **synchronous**: no `tokio`, no `axum`.

It is **one** crate, not several. The store, the triggers, the authorizer, the log, the
tree, the diff, the bulk path and the session all take `Db` in nearly every signature;
splitting them produces crates that cannot be used independently, so the boundary cost
is paid and none of the benefit is received.

### `merkle-lww-axum`

The transport. REST endpoints, the WebSocket handler, progress frames, and **zstd level
1 by default**. It owns the async boundary: this is where `tokio` lives.

### `merkle-lww-cli`

The binary. `serve`, `pull`, `push`, `inspect`, `tree`, `verify`. It depends on
`merkle-lww-axum` and `merkle-lww-core`.

### `merkle-lww-demo`

The conformance harness, and the answer to "how do we know the interface is right?".

- **An in-memory `Transport`**, so a full session runs between two peers in one
  process with no ports and no network.
- **A scripted clock**, so purge instants, the quiesce window and the staleness horizon
  are reached by advancing time rather than by sleeping. The failure modes — a missed
  purge instant, a stale peer, a quiescing table, a resumed cursor — are the ones that
  are otherwise flaky.
- **A two-node fixture**: two `Db`s over in-memory SQLite, exchanged through the
  in-memory transport. This is what the convergence suite drives.
- **The topology and scenario fixtures**: the multi-node topologies for the convergence
  test, and the named lifecycle scenarios.

The **wire vectors** — the exact bytes of every message type — are published by
`merkle-lww-protocol`, because the protocol crate is what defines the wire. The wire
contract is pinned by bytes rather than by prose, so a second implementation in another
language can be checked against it, and a change to the wire format is a visible diff
rather than a silent one. The demo crate asserts against those vectors, so a
message-shape change fails the harness as well as the protocol crate.

It is a crate rather than a directory of tests so that the examples are **compiled and
run** by CI. A worked example that is not executed is documentation that has already
drifted.

### The `alloc` feature stays a feature

`registry.rs` and `allocator.rs` remain behind the `alloc` feature of
`merkle-lww-core`. They are tightly coupled to `Db`, to the trigger installer and to
the table DDL; a crate boundary here would force a trait through the trigger installer
for no benefit, and the feature already achieves the thing the boundary would achieve —
the code is not compiled when it is not wanted.

## Consequences

**Positive**

- **The digest rules are testable without a database, and portable to another
  language.** The golden vectors are consumable by an implementation that has never
  heard of `rusqlite`, which is the point of specifying the encoding precisely.
- **Writing a custom merge policy no longer implies a SQLite dependency.** The
  extension point is a pure function, and it stays one.
- **The handshake rules are testable as pure functions.** Every rejection case is a
  table-driven test with no fixture and no database.
- **The wire contract has a home that is not the engine.** The wire version and the
  codec version are separate things with separate lifecycles, and the module graph
  reflects that rather than the prose having to insist on it.
- **The transport stays optional.** A deployment that already has a session layer
  depends on `model`, `protocol` and `core` and never compiles `axum`.
- **The failure modes are testable deterministically.** The scripted clock replaces
  sleeps with time advancement, which is the difference between a suite that is fast and
  a suite that is trustworthy.

**Negative**

- **Six crates is six versions and six sets of release decisions.** The workspace
  publishes together, so the cost is a single release script rather than six
  independent ones, but the metadata is real.
- **Every type that crosses a boundary becomes public API.** `SyncConfig`,
`MergePolicyFn`, the message types and the engine traits are all published surface
  now, and changing them is a breaking change.
- **The traits the session needs from the engine are a real design cost.** They have to
  be small enough to implement and complete enough to be useful, and they are a place
  where the design can be got wrong in a way a single crate could not.
- **The demo crate is a dependency the workspace carries.** It is not published, and it
  must not become a place where product code accumulates.

**Neutral**

- `codec_version` and the wire version remain separate, and the crate graph now makes
  that structural.
- The `alloc` feature is unchanged.
- Nothing about the protocol, the digest, or the storage model changes. This record is
  about where the code lives.

## Alternatives considered

**One crate.** Rejected. The compatibility surface cannot be tested without a database,
the extension point carries the engine's dependency weight, and the wire contract has
nowhere to live.

**Three crates — core, axum, cli — as previously sketched.** Rejected: it splits on the
transport boundary alone and leaves the two problems this record addresses untouched.
It is the layout this record replaces.

**Splitting the engine — a `merkle-lww-store` crate, a `merkle-lww-merkle` crate, a
`merkle-lww-diff` crate.** Rejected. They share `Db` in nearly every signature and
would have to be used together, so the boundary cost is paid without the isolation
benefit. This is the over-splitting failure: the crates cannot stand alone.

**A separate `merkle-lww-alloc` crate.** Rejected. The allocator needs to install
triggers and read the registry table inside the same database as everything else; a
crate boundary forces a trait through the trigger installer. The feature flag already
does the job.

**The demo as a directory of integration tests rather than a crate.** Rejected: the
examples would not be compiled by `cargo build`, and a worked example that is not
executed drifts. As a crate, CI runs it.

**Splitting later, once there is code.** This is the strongest argument against this
record, and it was considered seriously. The counter-argument is that the cost of a
boundary is lowest before the first module exists and rises with every module written
against it. A design that has already decided to split later will find that the
boundary it wanted was the one it had to push through four modules of existing code.

## Related

- [ADR-0011](0011-crate-provides-the-exchange.md) — the crate owns the exchange, the
  application owns the transport
- [ADR-0015](0015-per-table-merge-policies-and-write-authority.md) — the merge policy
  extension point that `merkle-lww-model` now hosts
- [ADR-0023](0023-json-wire-format-and-transport-compression.md) — the wire format and
  where compression lives
- [../architecture.md](../architecture.md) § "The crate graph" — the normative layout
- [../../PLAN.md](../../PLAN.md) — the per-crate plans
