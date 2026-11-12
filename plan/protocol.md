# Plan — `merkle-lww-protocol`

**Crate.** `crates/merkle-lww-protocol` · depends on `merkle-lww-model` only.

**Goal.** The wire contract: the messages, their JSON shapes, and the rules that decide
whether a handshake is acceptable.

This crate is what two independently-written implementations must agree on. It owns
the wire version, the message types, the tagged value form, and the handshake
validation rules.

**It is pure.** The validation rules are ordinary functions over ordinary data. The
`LocalView` they check is gathered by `core` from the database, and passed in.

**Why it is separate from `model`.** The wire version and the codec version have
different lifecycles, and the module graph should reflect that rather than the prose
having to insist on it:

| | versions | changes when |
|---|---|---|
| `merkle-lww/1` | the **protocol** | a message shape changes |
| `codec_version` | the **canonical encoding** | the digest rules change |

A change to the wire format re-digests **no rows** and invalidates **no stored data**,
because `schema_hash` covers the digest's inputs and never covered the wire.

See [ADR-0023](../doc/adr/0023-json-wire-format-and-transport-compression.md) and
[ADR-0024](../doc/adr/0024-the-workspace-is-split-into-six-crates.md).

## Contents

| Module | Holds |
|---|---|
| `messages.rs` | `Hello`, `Summary`, `Detail`, `Rows`, `Commit`, `Progress`, `Cursor`, `Projection`, `Origin` |
| `values.rs` | the tagged wire value form |
| `handshake.rs` | `LocalView`, `validate_handshake` |
| `engine.rs` | the traits the session needs from the engine |
| `transport.rs` | the `Transport` trait the application implements |
| `version.rs` | the wire version: `merkle-lww/1` |

---

## P1 — The messages and the wire values

### Deliverables

- `version.rs`: the wire version constant.
- `messages.rs`: every message type, `serde`-derived, with the JSON shapes in
  [protocol.md](../doc/protocol.md) § "Message encoding".
- `values.rs`: the tagged wire value form — `null`, an integral-syntax JSON number,
  `{"$f": …}` for a real, a JSON string for text, `{"$b": …}` for a blob.
- The row shape: a positional array over the projection's column list, with the key and
  the digest first.

### Notes

- **The wire format is JSON.** See [ADR-0023](../doc/adr/0023-json-wire-format-and-transport-compression.md)
  for the measurements and the reasoning.
- **Rows are positional.** A row's values are an array in the order given by the
  **projection** for that table — not an object keyed by column name. The columns are
  declared once, in the handshake, with their names and types, and the projection
  carries the type too, so a projection is self-sufficient for decoding a row.
- **The digest is present in both modes.** It is required in `row` mode because a peer
  whose projection is narrower than the table's replicated column set cannot recompute
  it, and because a peer that *can* recompute it should, as a check.
- **Values are tagged because SQLite is dynamically typed.** The digest distinguishes
  `INTEGER` from `REAL` and `TEXT` from `BLOB`, so the wire must too. A plain JSON
  number with a fraction or an exponent is an **error** — a producer that means `REAL`
  must write `{"$f": …}`. The asymmetry is deliberate: integers are the common case and
  stay plain, while the two cases JSON cannot express unambiguously are wrapped.
- **BLOBs are base64**, RFC 4648 standard alphabet, padded.
- **Unknown fields are ignored** by a receiver. That is what makes the wire format
  extensible without a version bump for every addition.
- **The `i64` range is a trap for non-Rust clients.** A JSON number above 2^53 loses
  precision in a JavaScript parser. The timestamps in the protocol are all well below
  2^53, but an application column holding a large `i64` is the client's problem to
  solve, and it should be documented as such.

### Exit criteria

- **Every message type serialises to a fixture and back.** The golden wire vectors are
  published and stable — a change to a message shape is a visible diff in the fixture,
  not a silent change.
- **The value form round-trips every SQLite storage class exactly**, including `REAL`
  values that are integral (`5.0` stays a real, not an integer) and BLOB values that
  are valid UTF-8.
- **A plain JSON number with a fraction or an exponent is rejected**, with an error
  that names the offending field.
- A row in `digest` mode and a row in `row` mode are distinguished unambiguously, and a
  receiver that gets the wrong mode for a message errors rather than misreads it.
- The fixtures are consumable by an implementation in another language: they are JSON,
  with the bytes spelled in a way another language can reproduce exactly.

---

## P2 — The handshake rules

### Deliverables

- `handshake.rs`: `LocalView`, and `validate_handshake(local: &LocalView, peer: &Hello)
  -> Result<Hello>`.
- The full validation table from [protocol.md](../doc/protocol.md) § "Validation",
  implemented as rules over the view.

### Notes

- **The rules are pure.** `LocalView` is the set of facts the rules check: the epoch,
  the codec version, the schema hash, the visibility hash, the retention and purge
  schedule, the known peers, and the last sync instant. `core` gathers it; the rules
  do the checking. Every rejection case is therefore a table-driven test with no
  database and no fixture.
- **This is where the fatal errors are raised**: `EPOCH_MISMATCH`, `PROTOCOL_UNSUPPORTED`,
  `CODEC_MISMATCH`, `DIGEST_SEED_MISMATCH`, `SCHEMA_MISMATCH`, `VISIBILITY_MISMATCH`,
  `UNKNOWN_PEER`, `IDENTITY_MISMATCH`, `BAD_CREDENTIAL`, `STALE_PEER`,
  `PURGE_SCHEDULE_MISMATCH`, `COLUMN_NOT_PERMITTED`.
- **Two failure modes, two meanings.** A mismatch between two trusted peers is fatal;
  a client whose registration is narrower than the server's is not, and the server
  syncs the intersection and logs a warning. The distinction is in the rules, not in
  the caller's discretion.
- **`STALE_PEER` is raised by the staleness horizon alone** — `now - last_sync_us >
  retention`. A peer that merely missed a purge instant catches up and carries on; that
  is not an error.
- **The staleness rule is symmetric.** A stale peer may pull, but it may not push: its
  pushes are refused with `STALE_PEER`, and a session with a stale peer is pull-only in
  both directions.
- **`as_of_us` is server-supplied in a client→server session**, so both sides choose
  identical comparison depths.

### Exit criteria

- **Every handshake rejection case returns the documented error code** — asserted by a
  table-driven test over the whole validation table, with no database.
- `validate_handshake` is a pure function: the same `(LocalView, Hello)` pair always
  produces the same result.
- A **client** whose registration is narrower than the server's is not refused; the
  intersection is synced and a warning is logged.
- Two trusted peers with different profile definitions are refused with
  `VISIBILITY_MISMATCH`; a client does not compare it and does not fail.
- A purge-schedule mismatch — `retention_us`, `purge_period_us` or `purge_offset_us` —
  is refused with `PURGE_SCHEDULE_MISMATCH`.

---

## P3 — The engine traits and the Transport trait

### Deliverables

- `engine.rs`: the traits the session needs from the engine, which `core` implements.
- `transport.rs`: the `Transport` trait the application implements.

### Notes

- **The consumer declares the interface.** The session needs a handful of things from
  the database — the rows of a bucket, the tree snapshot, the log entries, the session
  journal. Declaring them here, rather than in `core`, is what keeps the dependency
  edge pointing one way and stops the session from depending on `Db` itself.
- **The traits must be small.** Every method is a place where the session and the
  engine can disagree about what a bucket contains or what order rows come back in.
  A wide trait here is a design risk, and it is the one place in the crate graph where
  a mistake is expensive.
- **Ordering is part of the contract.** Rows within a bucket are ordered by their
  canonical key bytes, so both sides agree on the order without exchanging it. That
  ordering is stated in the trait, not left to each implementation.
- **The `Transport` trait is in terms of messages, not bytes.** That is what keeps
  compression outside the protocol: a transport that compresses is compressing the
  bytes it puts on the wire, and the crate never sees them. It is also what lets the
  convergence suite drive several nodes in one process through an in-memory transport.
- **A fake engine implements the traits**, and the session is exercised end to end
  against it with no database. The fake is test support, and it lives here rather than
  in `merkle-lww-demo` — the demo crate's job is the *transport* fake, not the
  *engine* fake.

### Exit criteria

- `core` implements the traits, and the session runs against them with no reference to
  `Db` in `merkle-lww-protocol`.
- **A fake engine implements the traits**, and the session is exercised end to end
  against it with no database. This is the testability the split exists for.
- The transport trait is in terms of the message types, and **an implementation that
  carries messages over anything at all — a channel, a file, a socket — satisfies it
  without the crate knowing what is underneath**.
- The trait surface is documented, because it is published surface.

---

## Crate-level exit criteria

- The crate builds with **no dependency other than `merkle-lww-model`** — no SQLite,
  no runtime.
- `cargo clippy` is clean.
- The golden wire vectors are published and stable.
- The wire version and the codec version are separate constants, in separate modules,
  and neither is derived from the other.

**Test suites:** the handshake validation table, the wire golden vectors, the value-form
round-trip.
