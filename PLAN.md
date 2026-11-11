# Implementation plan

How this gets built, in what order, and how each phase is judged complete.

## Principles

- **Each phase ends with something that works.** No phase is "set up for later".
- **The engine is testable without the transport.** The sync engine must not depend on
  axum, so the convergence test can drive several nodes in one process.
- **The enforcement layer comes before the sync layer.** Getting the SQLite mechanics
  right is the highest-risk part of the design, and it is independently verifiable.
- **A phase is not done until its test suite passes**, not when the code compiles.

## Dependencies

```
Phase 0 ── Foundations
   │
Phase 1 ── Store and enforcement
   │
Phase 2 ── Merkle tree
   │
Phase 3 ── Diff engine
   │
   ├── Phase 4 ── Transport
   │      │
   │   Phase 5 ── Lifecycle
   │
   └── Phase 5 ── Lifecycle
   
Phase 6 ── Hardening (after 4 and 5)
```

Phases 4 and 5 can proceed in parallel once Phase 3 lands.

---

## Phase 0 — Foundations

**Goal.** The crate skeleton, the types everything else depends on, and the canonical
encoding.

### Deliverables

- Workspace with three crates: `merkle-lww-core`, `merkle-lww-axum`, `merkle-lww-cli`.
- `config.rs`: `SyncConfig`, `TableSpec`, `ColumnSpec`, `KeyStrategy`, `Visibility`,
  `TableKind`, `MergeMode`, `MergePolicy`, `WriteAuthority`, `Peer`, `PeerAuth`,
  `CacheConfig`, `Role`, `WriteMode`. All `serde`-derived with `Default`.
- `merge.rs`: `MergePolicy`, `MergePolicyFn`, `MergeContext`, `MergeDecision` — the
  extension points and the rules they must obey.
- `visibility.rs`: `VisibilityProfile`, `ProfileTable`, the projection resolver, and
  the `visibility_hash`.
- `codec.rs`: the canonical encoding — one tagged-value rule for both keys and values;
  the seeded xxh3 digest; the two-level `row_hash`; the codec version.
- `schema.rs`: the `schema_hash` — exactly the inputs to the row digest, including the
  registered column order.
- `time.rs`: RFC 3339 parsing and formatting at fixed six-digit precision; the
  monotonic issuance guard.
- `allocator.rs`: the globally-unique id allocator — per-site ranges and a durable
  monotonic counter. **Behind the optional `alloc` feature**; the sync core does not
  depend on it.
- `error.rs`: the error type, including the protocol error codes.
- `directory.rs`: the UUID directory — the local table, the handshake merge, and the
  per-node revocation check.
- `SecretString`: a wrapper whose `Debug` does not print its contents.

**The crate persists no secret material.** The digest seed, the role strings and the peer
list are configuration, held in memory for the life of the process. Nothing secret is
written to the database, and the crate implements no rotation.

### Notes

- The digest is implemented here, not in the store, because it depends only on the
  canonical encoding.
- The monotonic guard is a small type here (`TimestampIssuer`) holding an `AtomicI64`,
  with the persisted value loaded at startup and written back on change.
- The canonical encoding is the one thing that must not change once data exists. Get it
  right now.
- **`KeyStrategy::GlobalId` uses the application's own column** when it is already globally
  unique and stable, so no extra column is added. `Derived` covers the applications that
  cannot offer one.
- **The id allocator guarantees "never reused".** Allocation is durable and happens
  *before* use: a gap is harmless, a reuse is corruption. The counter must not roll back
  with a transaction, so it is advanced outside the insert's transaction.
- **Local columns are excluded from the digest**, not merely from the merge. Including
  them would make two peers holding the same entity compute different digests, so every
  bucket would differ and nothing would ever converge.
- **The registered column order is part of the schema hash.** The digest concatenates
  values in registration order, so a different order is a different digest for the same
  logical row — permanent, silent divergence. The order is not an implementation detail.
- **The digest is 16 bytes, big-endian on the wire, compared as a `u128`.** Comparing raw
  bytes in memory order would let two peers disagree on an exact-timestamp tie.
- **The `site` column is in the digest**, because it is replicated and both peers must
  agree on it.
- **The digest seed is a third value**, separate from the two credential strings and
  rotated independently. Deriving it from a credential would couple a routine credential
  rotation to a full dataset rebuild.
- The range-allocation table is **deployment-wide state**, like the epoch and the digest
  seed, because peers need it to validate the identifiers they receive. It is the
  replicated **`sites` registry**, and it must live in the **main database** — triggers
  cannot reference an attached database.
- **The allocator is optional.** A deployment that brings its own identifier scheme
  declares no `AllocationSpec`, and then there is no registry, no `site` column, no
  range trigger and no REGISTRY phase. The allocator lives behind the `alloc` feature,
  and the sync core does not depend on it. See
  [ADR-0019](doc/adr/0019-the-allocation-registry-is-optional.md).
- **The registry's range is immutable.** A site is renamed; its range is not moved. A
  decommissioned site's range is retired forever, like a retired identifier.
- **A row names the site that owns its identifier, not the site that last wrote it.** The
  `site` column is replicated and in the digest; the range check is keyed on it, so a
  peer's rows are accepted on the same terms as local ones.
- **The range check fires only when the registry knows the owner.** A site created
  mid-session may have its rows arrive before the registry row does; rejecting them would
  deadlock the conversation, so they are accepted provisionally and re-validated later.
- **The registry is exchanged before the data** in every session — when it is in use —
  and is part of the join bundle alongside the epoch and the digest seed.
- **The visibility profiles are deployment-wide state too**, and a divergence is fatal
  between trusted peers. `visibility_hash` is separate from `schema_hash`, because a
  client's registration is necessarily narrower and could never match the full schema
  hash.
- **A custom merge policy must be pure and convergent.** The crate cannot check it, so
  it is a stated requirement on the application; the digest tie-break remains the
  fallback for "undecided". See
  [ADR-0015](doc/adr/0015-per-table-merge-policies-and-write-authority.md).

### Exit criteria

- The workspace builds; `cargo clippy` is clean.
- Canonical encoding golden vectors pass, and are published as a fixture.
- The encoding is injective on a generated set of values: two distinct rows never encode
  to the same bytes (`proptest`).
- Timestamps round-trip through parse and format at fixed six-digit precision, and sort
  lexicographically in chronological order.
- **The derived sync id is deterministic**: the same identity columns produce the same
  id on every node, and different identity columns produce different ids.
- **The allocator never reissues an identifier**: after a crash and restart, the next
  allocated id is strictly greater than every previously allocated one.
- **The range check rejects a bad mint and accepts a peer's rows**: an identifier outside
  the row's own site's range is refused, and one inside it is accepted — including on the
  `ON CONFLICT DO UPDATE` path.
- **The registry enforces its own invariants**: an overlapping range is refused on insert,
  a range change is refused on update, and a descriptive update is accepted.
- **The `site` column is filled by the engine, not the application**: an insert that
  omits it gets the local site, an explicit value wins, and a connection without the UDF
  fails closed.
- **The key encoding is the value encoding**: `userid = 10001` produces the same tagged
  bytes in `_sync_log.k`, in the digest input, and on the wire — and the same rule covers
  a composite key.
- **The schema hash covers exactly the digest's inputs**: two nodes differing only in
  registered column order are rejected at handshake, not discovered as divergence.
- **The visibility hash covers exactly the profile definitions**, and is separate from
  the schema hash: a client with a narrower registration still matches a master's
  projection.
- **The crate builds without the `alloc` feature**, and a deployment with no
  `AllocationSpec` has no registry, no `site` column and no range trigger.

**Test suites:** 14 (canonical encoding), plus the allocator and registry suites.

---

## Phase 1 — Store and enforcement

**Goal.** The local database: managed tables, the trigger set, the authorizer rules, the
internal log, and DDL management.

This is the highest-risk phase, because it depends on subtle SQLite behaviour that was
verified experimentally rather than documented (see
[doc/sqlite-interface.md](doc/sqlite-interface.md) § "Verified SQLite behaviour").

### Deliverables

- `store.rs`: the `Db` type, the `r2d2` pool, the connection factory that registers
  UDFs and installs the authorizer, and `Db::conn()` / `Db::transaction()`.
- `Db::register_on(&rusqlite::Connection)`: install the UDFs and the authorizer on a
  **caller-owned** connection, so an application with an existing write path does not
  have to move its writes into the crate's pool. `Db::with_privilege(conn, f)` grants
  crate privilege on that connection for the crate's own writes.
- `trigger.rs`: trigger installation for both dialects; the managed column DDL.
- `authorizer.rs`: the denial rules, the `accessor` exemption, and the per-connection
  privilege flag.
- `log.rs`: `_sync_log` reads and writes. The log carries `last_update_us` (the row's
  `last_update` as integer microseconds) and `purge_on_us` (NULL while the row is live).
- `registry.rs`: the site registry — the replicated `sites` table, its immutable-range and
  no-overlap triggers, the per-row range-enforcement trigger, and the `site` column
  default. **Only when the `alloc` feature is enabled and a table declares an
  `AllocationSpec`.**
- `session_store.rs`: the `_sync_session` table — a client's durable session journal.
- DDL management: `create_table`, `alter_table`, `adopt_table`, `drop_table`.
- The UDFs: `sync_stamp()`, `sync_us(text)`, `sync_digest(...)`, and — on tables with
  allocated identifiers — `local_site()`.

### Notes

- **The privilege flag is per-connection.** It cannot live on `Db`, because another
  thread may hold a different connection from the same pool.
- **Every pooled connection gets the UDFs and the authorizer.** A connection that
  escapes uninstrumented is a hole, so the connection factory is the only way to make
  one.
- **A caller-owned connection is instrumented by `register_on`.** The UDFs close over
  the node's shared `TimestampIssuer`, so a timestamp issued on a caller-owned
  connection is strictly greater than the last one issued anywhere in the process — the
  monotonic guard holds across both kinds of connection. An unregistered connection
  still fails closed.
- **Every DDL path reinstalls the triggers** and rebuilds the tree.
- The compatible dialect's staging table and drainer are implemented here, since they
  are part of the write path.
- **The registry lives in the main database.** SQLite forbids a trigger from referencing
  an object in an attached database, so a separate `sites.db` is not an option while the
  range check is a trigger.
- **The `site` column defaults to `local_site()`**, so the application never writes it.
  `ALTER TABLE … ADD COLUMN` rejects a non-constant default, so the `DEFAULT (fn())` form
  needs `CREATE TABLE`; for an existing table the crate installs an `AFTER INSERT`
  backfill trigger and backfills the rows.
- **The range trigger fires only when the registry knows the row's owner.** A site
  created mid-session may have its rows arrive before the registry row does; rejecting
  them would deadlock the conversation.
- **The `_sync_log` timestamp is the row's `last_update`, not a second clock.** The
  trigger writes both from one value, so they cannot disagree. The log needs it as an
  integer for bucketing and range scans, and it never reads a payload column.
- **`purge_on_us` is NULL while a row is live**, which makes "which tables have
  tombstones due at instant T" an indexed existence query.

### Exit criteria

- Suite 2 (engine enforcement) passes in full, using raw SQL.
- Suite 3 (trigger mechanics) passes: one log row and one stamp per update, no
  recursion, identical behaviour with `recursive_triggers` on and off.
- Suite 13 (DDL) passes.
- The SQLite behaviour assertions are preserved as tests, so a SQLite upgrade that
  changes the semantics the design relies on fails loudly.
- A raw `INSERT` on a managed table from an instrumented connection results in a
  stamped row and a correct `_sync_log` entry, verified by inspecting the database
  directly rather than through the crate's API.
- **The registry enforces its invariants**: an overlapping range is refused on insert, a
  range change is refused on update, and a descriptive update is accepted.
- **The range check rejects a bad mint, accepts a peer's rows, and defers an unknown
  owner** — including on the `ON CONFLICT DO UPDATE` path.
- **The `site` column is filled by the engine**: an insert that omits it gets the local
  site, an explicit value wins, and a connection without the UDF fails closed.
- **A caller-owned connection behaves like a pooled one**: after `register_on`, a write
  is stamped and logged, and the authorizer denies the same operations.
- **The monotonic guard holds across connection kinds**: a timestamp issued on a
  caller-owned connection is greater than the last one issued on a pooled one.

**Test suites:** 2, 3, 13, plus the registry suite.

---

## Phase 2 — Merkle tree

**Goal.** The bucket tree: definitions, hashing, the snapshot, and maintenance.

### Deliverables

- `merkle.rs`: bucket id computation from the epoch; the three hash levels; the
  `SyncSnapshot`; the `ArcSwap` publication.
- Rebuild from `_sync_log` at startup.
- Incremental maintenance: apply a row change, apply a purge.
- The `max_tree_buckets` tripwire.

### Notes

- The hour hash is a commutative sum, so a row move is two O(1) updates. The cycle and
  month hashes are ordered folds over 24 and 30 children, recomputed on change.
- Publication is coalesced. A burst of writes must not produce a burst of snapshots.
- The rebuild is one indexed pass over `_sync_log`; it must not read payload columns.

### Exit criteria

- The tree unit tests pass: bucket assignment is a pure function of `last_update`; the
  same timestamp always lands in the same bucket.
- **The incremental hash matches a full rebuild**, always (`proptest` over random write
  sequences). Drift here would be invisible until it caused permanent divergence.
- Rebuild at startup produces the same tree as incremental maintenance from empty.
- Memory matches the table in [doc/merkle.md](doc/merkle.md) § "Memory".

**Test suites:** tree units, plus the incremental-vs-rebuild property.

---

## Phase 3 — Diff engine

**Goal.** Compare two trees, find the differing rows, and merge them.

### Deliverables

- `diff.rs`: the summary / detail / rows comparison logic, operating on abstract trees
  rather than on a transport.
- `merge.rs`: the merge rule — LWW on `(last_update, digest)`, with the digest
  tie-break — plus the per-table merge policy hook and the write-authority check.
- The row-level diff in both modes: `digest` (compact) and `row` (simple).
- `session.rs`: the protocol state machine, transport-independent.
- Canonical serialisation of rows for the wire.
- `bulk.rs`: the bulk apply path — chunked multi-row `VALUES`, the JSON bulk path, and
  the staging-table path. See [doc/bulk-apply.md](doc/bulk-apply.md).
- `paging.rs`: the page cursor, the byte budget, and the resumable page sequence.

### Notes

- **The engine must not depend on axum.** The convergence test drives several nodes
  in one process through an in-memory transport.
- The merge must be idempotent: applying the same row twice is a no-op.
- A deleted row is an ordinary row. No special case in the diff, the merge, or the
  serialisation.
- **Bulk apply must respect the 32,766 bound-parameter limit.** The chunk size is
  derived from the column count, not hardcoded: `rows × columns ≤ 32,766`. A wide table
  chunks smaller automatically. The limit cannot be raised at runtime — it is a
  compile-time ceiling in SQLite.
- The LWW filter is expressible in the upsert itself
  (`ON CONFLICT … DO UPDATE … WHERE excluded.last_update > device.last_update`), but the
  digest tie-break is applied in Rust, so the existing rows for a batch's keys are read
  once and compared in memory before the upsert.
- **A custom merge policy bypasses the upsert-level filter.** The decision is made in
  Rust, and only the winners are upserted. The `WHERE` clause is the *default* policy's
  filter, not a universal one.
- **Batch repair runs on the sender.** A receiver-side repair changes the stored bytes
  and therefore the digest, and the peers would never converge.
- **Write authority is evaluated against the row's origin**, which the wire carries —
  not against the node that forwarded the row.
- **A page is bounded by bytes, not by rows.** The sender stops when the encoded message
  would exceed `max_message_bytes` and returns a cursor.
- **The cursor is deterministic**: buckets in order, rows within a bucket by canonical
  key bytes, so both sides agree on the order without exchanging it.
- Bulk apply and the `_sync_log` update happen in the same transaction, so the log and
  the user table cannot diverge.

### Exit criteria

- Suite 1 (convergence) passes for two and for three nodes.
- Suite 8 (tie-break) passes: two peers with an identical `last_update` and different
  payloads converge on the same winner.
- Suite 9 (divergence detection) passes: an out-of-band edit is detected and reported.
- Applying a row updates the user table, `_sync_log` and the tree in one transaction.
- **A 50,000-row batch applies without exceeding the parameter limit**, and the chunk
  size adapts to the column count. Measured reference: ~95 ms for 50,000 rows via
  chunked `VALUES`, ~108 ms via the JSON path.
- A bulk apply is atomic: a failure part-way through leaves nothing applied.
- **Suite 19 (merge policies and write authority) passes**, including convergence with
  a custom policy in the loop.
- **A page never exceeds `max_message_bytes`,** and a page re-issued with a cursor
  continues exactly where the last one stopped.

**Test suites:** 1, 8, 9, 19, plus the bulk-apply suite.

---

## Phase 4 — Transport

**Goal.** The protocol over HTTP and WebSocket, as an optional convenience over a
transport-agnostic core.

### Deliverables

- The core's transport-agnostic exchange: `perform_exchange(peer, transport)`, where
  `transport` carries request and response messages. The session state machine must not
  know about HTTP.
- `merkle-lww-axum`: the REST endpoints and the WebSocket handler, built on the core.
- Sessions: opaque `session_id`, in-memory, five-minute **idle** TTL. A client that
  needs to survive a restart persists its session in `_sync_session` and re-opens it by
  id.
- The handshake and its validation, including the projection and the `visibility_hash`.
- The progress model: `SyncProgress`, the endpoints, the WS frame.
- Auth: the peer list check, the credential check, and the binding of the
  application's authenticated identity to the claimed node id.
- The message budget (`max_message_bytes`) and the round limit
  (`max_rounds_per_session`).
- `doc/implementation.md`, covering the integration for application developers.

### Notes

- The handshake validation is where `EPOCH_MISMATCH`, `DIGEST_SEED_MISMATCH`,
  `UNKNOWN_PEER`, `IDENTITY_MISMATCH`, `VISIBILITY_MISMATCH`, `STALE_PEER` and
  `COLUMN_NOT_PERMITTED` are raised.
- **`STALE_PEER` is raised by the staleness horizon alone** — `now - last_sync_us >
  retention`. A peer that merely missed a purge instant catches up and carries on; that
  is not an error.
- `as_of_us` is server-supplied in a client→server session, so both sides choose
  identical comparison depths.
- Progress frames are throttled to 250 ms and only sent when `completed` advances.
- **The crate does not terminate TLS.** The application does, and handles authentication.
  The crate's own axum crate assumes TLS is terminated in front of it. The application
  passes the authenticated identity in as `SessionContext`, and the crate binds it to
  the claimed node id.
- **A session that hits the round limit ends with `SESSION_LIMIT`**, which is retryable:
  the caller re-opens with the last cursor.
- **Tables are synced in `sync_priority` order, newest buckets first**, so a client
  catching up is usable before it is complete.
- The core's transport abstraction is what lets the convergence test drive several nodes
  in one process. It is a testability requirement as much as an extensibility one.

### Exit criteria

- Suite 15 (transport integration) passes for REST and WebSocket.
- Suite 12 (progress) passes: every phase's `total` is known before the phase runs,
  `completed` never exceeds `total`, and the denominator changes correctly between
  phases.
- Every handshake rejection case returns the documented error code.
- The pull-only client flow works: the server pushes the diff, the client applies it —
  **in bounded pages**, with the same budget as a bidirectional session.
- **Suite 20 (paging and budgets) passes**, and **Suite 22 (durable client sessions)**
  passes: a client that restarts mid-transfer resumes from its cursor.
- The core has no dependency on axum, and the exchange works over a non-HTTP transport.

**Test suites:** 12, 15, 20, 22.

---

## Phase 5 — Lifecycle

**Goal.** Deletes, the purge schedule, the quiesce window, and the staleness rule.

### Deliverables

- The scheduled purge: computing the instants from the epoch and the period.
- The quiesce window: refusing sync with `503 PURGE_IN_PROGRESS`, **per table**, and
  only for tables that actually have tombstones due.
- `last_purge_instant` advertisement, and the purge floor on incoming rows.
- The staleness check at handshake, evaluated mesh-wide.
- The re-seed path: catch-up purge, push, pull, keep.
- The `move_key` operation, atomic across the tombstone and the insert.

### Notes

- **The purge floor applies to tombstones only.** An incoming row is refused iff it is
  a tombstone whose `purge_on` is at or before the receiver's `last_purge_instant`. A
  live row is never refused, however old — under a daily purge, a write-timestamp
  comparison would refuse nearly every live row and break seeding, re-seeding and
  catch-up.
- **Missing a purge instant is not fatal.** The side that is behind purges locally to
  the peer's instant and carries on. Only the staleness horizon — `now - last_sync_us >
  retention` — is fatal.
- **The staleness rule is evaluated against the mesh-wide last sync**, not per link.
- **A re-seed merges; it never wipes.** The stale peer pushes its unsynced local rows
  first, then pulls the peer's dataset, and keeps the rows the peer does not have. See
  [ADR-0017](doc/adr/0017-re-seed-is-a-merge-not-a-wipe.md).
- `move_key` is the only operation that changes a key, and it is atomic.

### Exit criteria

- Suite 5 (resurrection) passes in both directions: a peer offline longer than the
  retention is refused and must re-seed; a peer offline for less than the retention
  receives the tombstone and applies it.
- Suite 6 (purge) passes: all nodes purge the same set at the same instant; a node
  offline across a purge purges locally before syncing; sync is refused during the
  quiesce window.
- **A live row older than the receiver's `last_purge_instant` is accepted**, and a
  tombstone the receiver has already purged is refused with `ROW_PURGED`.
- **A peer that missed a purge instant is not refused** — it catches up and continues.
- **The quiesce window is per table:** a node with no tombstones due does not quiesce
  at all, and a session about an unrelated table proceeds during another table's
  quiesce.
- **Suite 21 (re-seed) passes:** a child that accepted writes while offline keeps them
  through a re-seed, and the tombstone is still purged.
- The resurrection invariant is stated as a test, not only as a proof in the docs: a
  randomised sequence of offline intervals asserts that resurrection never happens
  within the retention window, and always requires a re-seed beyond it.

**Test suites:** 5, 6, 21.

---

## Phase 6 — Hardening

**Goal.** Visibility, the cache, the CLI, and observability.

### Deliverables

- The visibility model end to end: the profiles, the projection resolver, filtering at
  the serialisation boundary, the handshake restriction, and the per-table exclusion.
- The optional `_sync_audit` table: which peer applied which row, locally.
- The read-through cache, with the byte and entry bounds.
- The CLI: `serve`, `pull`, `push`, `inspect`, `tree`, `verify`.
- Metrics and the health endpoint.
- The `SecretString` audit: no credential in any log line or error message.

### Notes

- The cache accelerates point lookups by primary key and nothing else. It must not
  pretend otherwise.
- Restricted-column filtering happens at serialisation, not at storage, so the cache
  can hold full rows.
- `verify` reports and does not repair.
- **The audit table is off by default, node-local, and not in the digest** — turning it
  on cannot affect convergence.

### Exit criteria

- Suite 10 (visibility) passes: no restricted value appears anywhere in the wire bytes,
  in any log line, or in any error message. **This test must never regress.**
- **Suite 10 also covers the profiles**: a credential column visible master-to-master
  but not to children; a payload column withheld from clients; a table excluded from
  one role and included in another.
- Suite 4 (foreign connections) passes: enforced mode fails closed; compatible mode
  succeeds and is drained.
- Suite 7 (clock skew) passes, at the ±5 s default.
- Suite 11 (key strategies) passes for all five strategies.
- The full suite passes.
- `cargo clippy` is clean with no warnings; `cargo fmt --check` is clean.
- Documentation is updated to match the implementation.

**Test suites:** the full suite.

---

## Cross-cutting work

### Documentation

The documents in [doc/](doc/) are written before implementation and updated as the
implementation reveals the design to be wrong. An ADR that turns out to be wrong is
superseded, not edited.

### Fixtures

| Fixture | Created in | Used by |
|---|---|---|
| `golden_vectors.json` | Phase 0 | Suite 14 |
| `topologies.json` | Phase 3 | Suite 1 |
| `scenarios.json` | Phase 5 | Suites 5, 6 |

### Tooling

- `cargo fmt`, `cargo clippy` with `-D warnings`.
- `cargo nextest` for the test runner, once the suite is large enough to warrant it.
- `cargo-insta` for snapshot tests of the wire format, once it stabilises.
- CI on Linux and macOS, with the SQLite behaviour assertions run against both the
  bundled and the system SQLite where the system build is new enough.

---

## Open questions

These are unresolved, and the ones marked **blocking** hold up a specific phase.

| # | Question | Impact | Blocking |
|---|---|---|---|
| 1 | **Default write dialect.** Enforced (UDF triggers) or compatible (pure-SQL staging)? | Determines whether the application can use `Db::conn()` at all | Phase 1 |
| 2 | ~~Does the crate terminate TLS?~~ | **Resolved: no.** The crate has no TLS dependency. The application terminates TLS and handles authentication; the crate provides the exchange and the peer check. See [doc/implementation.md](doc/implementation.md) | — |
| 3 | ~~`GlobalId` or `SiteNamespaced` as the default key strategy?~~ | **Resolved: `GlobalId` default.** The sites share one dataset, so the same user is the same user. `SiteNamespaced` remains available per table for genuinely per-site data. See [ADR-0006](doc/adr/0006-site-namespaced-keys.md) | — |
| 4 | **How is the digest seed distributed?** Environment variable, mounted secret, or KMS. The crate holds it in memory and **never writes it to the database**; it implements no secret store and no rotation. See Q16 and [ADR-0014](doc/adr/0014-peer-credentials-and-the-uuid-directory.md) | Deployment; not implementation | — |
| 5 | **Wire format.** JSON, or binary from the start? | Protocol stability | Phase 4 |
| 6 | ~~Is the purge period daily, or aligned to the retention window?~~ | **Resolved: daily.** The purge instant is every 24 h, epoch-aligned. The cadence only decides how soon a tombstone is collected; with a 30-day retention a tombstone lives 30–31 days. It does not interact with the purge floor, which applies to tombstones only. See [doc/lifecycle.md](doc/lifecycle.md) | — |
| 7 | **Does the compatible dialect's drainer compute digests for the whole table or only for staged rows?** Only staged rows, presumably, but a row written and deleted within one interval is lost — acceptable, since it was never visible to a peer. | Drain semantics | Phase 1 |
| 8 | **Should `AppendOnly` tables exist?** They remove the delete machinery from the trigger set, at the cost of a second code path. | Scope of the trigger set | Phase 1 |
| 9 | ~~Should the crate manage the `sync_id` column, or use the application's own key?~~ | **Resolved: the application's own key.** The application's `userid` is already globally unique and never reused, so it *is* the sync key and no extra column is needed. `Derived` covers applications whose key is local or mutable. See [ADR-0012](doc/adr/0012-sync-key-is-the-applications-key.md) | — |
| 10 | **How is the derivation namespace chosen?** A per-table constant from the table name and `epoch_us` keeps ids stable within a deployment but differs between deployments. | Id stability | Phase 0 |
| 11 | **Should the crate own the id allocator?** The "never reused" property needs a durable, monotonic, pre-allocated counter and a range check. Owning it makes the property a guarantee rather than a convention. | API scope | Phase 0 |
| 12 | **Should the crate offer a membership-table helper?** Materialising a JSON array into a child table removes a recurring source of merge bugs. | API scope | Phase 6 |
| 13 | **Who assigns the site ranges, and how?** The crate cannot prevent two nodes from concurrently creating sites with overlapping ranges — each check passes locally, and the merge happens afterwards. Central assignment plus local enforcement is the current answer; a crate-side allocator is a possible extension. | Deployment invariant | Phase 0 |
| 14 | **Is the `site` column's owner id an opaque id or a readable slug?** A slug is nicer to read and awkward to keep immutable; an opaque id is the reverse. The application owns the `sites` schema, so this is the application's choice — but the crate must not assume a slug is stable. | Schema ownership | Phase 0 |
| 15 | ~~How is the peer credential structured?~~ | **Resolved.** Master-issued UUIDs; a UUID directory propagated master-to-master; role-level master and child strings; the digest seed as a third, independently-rotated value. Per-node revocation is by removing a UUID from the directory. The application's authenticated identity is bound to the claimed node id, and per-node credentials and local attribution are available as options. See [ADR-0014](doc/adr/0014-peer-credentials-and-the-uuid-directory.md) and [ADR-0020](doc/adr/0020-application-authenticated-peer-identity-and-optional-per-node-credentials.md) | — |
| 16 | ~~Where does the digest seed live?~~ | **Resolved.** It is configuration, held in memory, and **never written to the database**. The crate implements no rotation. | — |
| 17 | ~~How do two peers merge a row when LWW is the wrong rule?~~ | **Resolved.** A per-table merge policy, with two requirements: it must be pure and convergent, and batch repair runs on the sender. See [ADR-0015](doc/adr/0015-per-table-merge-policies-and-write-authority.md) | — |
| 18 | ~~How is a row's write authority decided?~~ | **Resolved.** Per table, against the **origin** of the row — the node that wrote it — which the wire now carries. See [ADR-0015](doc/adr/0015-per-table-merge-policies-and-write-authority.md) | — |
| 19 | ~~How are visibility classes beyond trusted/client expressed?~~ | **Resolved.** Named visibility profiles mapped to roles, overridable per peer, hashed separately from the schema. See [ADR-0016](doc/adr/0016-visibility-profiles-mapped-to-peer-roles.md) | — |
| 20 | ~~What does a re-seed do with the stale peer's local writes?~~ | **Resolved.** It merges: push the unsynced local rows first, then pull, and keep the rows the peer does not have. See [ADR-0017](doc/adr/0017-re-seed-is-a-merge-not-a-wipe.md) | — |
| 21 | ~~How are large transfers bounded?~~ | **Resolved.** A byte budget per message, a round limit per session, resumable cursors, and a durable client session. See [ADR-0018](doc/adr/0018-bounded-messages-paged-transfer-and-durable-client-sessions.md) | — |
| 22 | ~~Must an application use the crate's allocator?~~ | **Resolved.** No. The registry, the `site` column, the range trigger and the REGISTRY phase are all optional. See [ADR-0019](doc/adr/0019-the-allocation-registry-is-optional.md) | — |
| 23 | ~~Can an application keep its own write path?~~ | **Resolved.** `Db::register_on(&Connection)` installs the UDFs and the authorizer on a caller-owned connection. See [doc/sqlite-interface.md](doc/sqlite-interface.md) | — |

Questions 1 and 3 change the shape of the code. The rest can be settled during the phase
they affect.
### Question 1, elaborated — the write dialect

The question is not "does the application use `rusqlite`?" It is:

> **Does the application open connections the crate did not instrument?**

That determines whether the crate's write-time invariants can be enforced at all.

**How the invariant is enforced.** Every write to a managed table must (a) stamp
`last_update` and (b) update `_sync_log`. Doing that in Rust means the trigger calls a
Rust function — the digest is xxh3, which SQLite does not have.

- **Enforced.** The trigger calls `sync_stamp()` and `sync_digest()`, registered on the
  connection by the crate. The work happens inside the write transaction, so the log is
  never stale. On a connection without the functions registered, the write **fails** with
  `no such function: sync_stamp`.
- **Compatible.** The trigger stages the row into a table using pure SQL, and a Rust
  drainer computes the digest afterwards. Works on any connection. Costs up to one drain
  interval of latency, and millisecond rather than microsecond timestamp resolution.

**What it means in practice.**

| Application | Enforced | Compatible |
|---|---|---|
| Uses `rusqlite`, takes connections from `Db::conn()` | **yes, recommended** | works, with latency |
| Uses `rusqlite`, opens its own connections | no — needs the connection factory | works |
| Uses `diesel` | no — diesel opens its own connections | works |
| Uses `sqlx` | no | works |
| Uses the `sqlite3` CLI | no | works, but only for reads |

**The compatibility gap, verified.** The authorizer is per-connection, so in compatible
mode a foreign connection has no denial rules. It can write `_sync_log` and hard-delete
managed rows. Two pure-SQL guards close the important cases: a `BEFORE DELETE` trigger
converts a hard delete into a soft delete, and a guard trigger rejects writes to the
internal log. The remaining gap is that a foreign connection can write `status` or
`purge_on` directly. That is caught and propagated, but the tombstone deadline may be
inconsistent. See [doc/sqlite-interface.md](doc/sqlite-interface.md) § "Compatible mode and
foreign connections".

**Recommendation.** Default to **enforced**, and document that the application must use
`Db::conn()` or the crate's connection factory. The failure mode for a misconfigured
application is a loud error at the first write, not silent divergence. If the target
application genuinely cannot be changed, compatible mode is the fallback and the
documented gaps apply.

**The question to answer:** *can the application be changed to take its connections from
the crate?* If yes, enforced. If no, compatible, and the gap is accepted knowingly.

### Question 3, elaborated — the key strategy

> **When two sites create what should be the same logical entity, do they become one row
> or two?**

**Resolved: `GlobalId` as the default.** The sites share one dataset, so the same user
is the same user everywhere, and two sites updating that user should converge on one row.

The consequence that must be handled explicitly: **`GlobalId` only merges if both sites
derive the same id.** A randomly generated globally-unique id (UUIDv7, ULID, Snowflake)
does not merge anything — it merely avoids collisions. For "the same user is the same
user" to hold, the id must be:

- derived deterministically from a natural key — `uuidv5(namespace, "user:alice")`; or
- assigned by something upstream that already agrees across sites.

The crate should offer a `Surrogate` variant that derives a deterministic UUIDv5 from the
table's natural key columns, so an application can opt into merging without changing its
schema.

`SiteNamespaced` stays available per table for data that is genuinely per-site — where
merging two records would be actively harmful.

See [doc/data-model.md](doc/data-model.md) § "Key strategies" and
[ADR-0006](doc/adr/0006-site-namespaced-keys.md).


---

## Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| A SQLite version changes behaviour the design relies on | Low | The behaviour assertions are preserved as tests; they fail loudly rather than subtly |
| The digest seed leaks to a client | Low | The seed is never sent; a client cannot recompute a digest; the visibility test asserts it |
| A deployment mixes enforced and compatible nodes | Medium | The two dialects produce comparable timestamps and identical digests, so mixing is safe; the drain latency is the only difference |
| The digest or encoding changes after data exists | Low | Both are versioned in `_sync_meta`; the crate refuses to start with a changed version unless told to re-digest |
| Clock skew beyond the documented `max_skew` (5 s) | Medium | A skew check at handshake; peers believed to be skewed are refused |
| The month tier's memory grows unexpectedly | Low | Bounded by activity, not age; `max_tree_buckets` is a tripwire |
| A bulk apply exceeds SQLite's 32,766 bound-parameter limit | Medium | The chunk size is derived from the column count, and the JSON path uses a single parameter regardless of batch size. A 50,000-row batch is covered by a test |
| The application generates random ids where deterministic ones were needed, so rows never merge | Medium | Documented prominently in the key-strategy section and ADR-0006; `verify` can report entities that exist at more than one site with the same natural key |
| A site uses `GlobalId` on data that is genuinely per-site, and two records merge | Low | `SiteNamespaced` is a per-table option; the failure is data loss, so the default should be reviewed when the deployment shape changes |
| A custom merge policy is not convergent, and two peers diverge silently | Medium | The requirement is documented as a hard rule and in ADR-0015; the digest tie-break remains the fallback; the convergence suite runs with a custom policy in the loop. **The crate cannot detect this** — it is the one extension point that can break the core guarantee |
| A re-seed discards a peer's unsynced local writes | Low | The re-seed is a merge, not a wipe: push, then pull, then keep. Suite 21 asserts it explicitly |
| A page cursor is stale or from a different `as_of_us`, and a transfer resumes at the wrong place | Low | The cursor is derived from deterministic ordering, re-sending a page is idempotent, and a mismatched cursor is rejected rather than applied |
| A visibility profile diverges between peers, and they project the data differently | Low | `visibility_hash` is compared at handshake and is fatal between trusted peers |
| A client restarts mid-transfer and restarts from the beginning | Low | The session journal is durable, pages are staged before they are applied, and the cursor advances only after the commit |
