# Plan — `merkle-lww-core`

**Crate.** `crates/merkle-lww-core` · depends on `merkle-lww-model`,
`merkle-lww-protocol`, `rusqlite` and `r2d2`.

**Goal.** The engine: everything that touches SQLite.

It is **synchronous**. No `tokio`, no `axum`. The async boundary is the transport.

**It is one crate, not several.** The store, the triggers, the authorizer, the log, the
tree, the diff, the bulk path and the session all take `Db` in nearly every signature.
Splitting them produces crates that cannot be used independently — the boundary cost is
paid and none of the benefit is received.

See [ADR-0024](../doc/adr/0024-the-workspace-is-split-into-six-crates.md).

## Contents

| Module | Holds |
|---|---|
| `store.rs` | `Db`, the `r2d2` pool, the connection factory, DDL management |
| `trigger.rs` | trigger installation for both dialects; the managed column DDL |
| `authorizer.rs` | the denial rules, the `accessor` exemption, the per-connection privilege flag |
| `log.rs` | `_sync_log` reads and writes |
| `registry.rs` | the optional site registry and its range triggers |
| `allocator.rs` | the optional id allocator (feature `alloc`) |
| `directory.rs` | the UUID directory |
| `session_store.rs` | `_sync_session` and `_sync_page` |
| `merkle.rs` | the bucket tree, the hashes, the snapshot |
| `diff.rs` | the comparison and the merge |
| `bulk.rs` | the bulk apply path |
| `paging.rs` | the page cursor, the byte budget |
| `session.rs` | the protocol state machine |
| `lifecycle.rs` | the purge schedule, the quiesce window, the staleness rule, the re-seed |
| `cache.rs` | the read-through cache |

---

## C1 — The store and the write path

**This is the highest-risk stage in the whole plan**, because it depends on subtle
SQLite behaviour that was verified experimentally rather than documented (see
[sqlite-interface.md](../doc/sqlite-interface.md) § "Verified SQLite behaviour").

### Deliverables

- `store.rs`: the `Db` type, the `r2d2` pool, the connection factory that registers
  UDFs and installs the authorizer, and `Db::conn()` / `Db::transaction()`.
- The `Db` surface the application sees: `Db::open`, `Db::conn`, `Db::transaction`,
  `Db::register_on`, `Db::with_privilege`, and the managed convenience layer —
  `Db::upsert`, `Db::delete`, `Db::move_key`.
- `Db::register_on(&rusqlite::Connection)`: install the UDFs and the authorizer on a
  **caller-owned** connection, so an application with an existing write path does not
  have to move its writes into the crate's pool. `Db::with_privilege(conn, f)` grants
  crate privilege on that connection for the crate's own writes.
- `trigger.rs`: trigger installation for both dialects; the managed column DDL.
- `authorizer.rs`: the denial rules, the `accessor` exemption, and the per-connection
  privilege flag.
- `log.rs`: `_sync_log` reads and writes. The log carries `last_update_us` (the row's
  `last_update` as integer microseconds) and `purge_on_us` (NULL while the row is live).
- `registry.rs`: the site registry — the replicated `sites` table, its immutable-range
  and no-overlap triggers, the per-row range-enforcement trigger, and the `site` column
  default. **Only when the `alloc` feature is enabled and a table declares an
  `AllocationSpec`.**
- `allocator.rs`: the globally-unique id allocator — per-site ranges and a durable
  monotonic counter. **Behind the optional `alloc` feature.**
- `session_store.rs`: the `_sync_session` table — a client's durable session journal,
  and the `_sync_page` table it stages received pages in.
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
  `ALTER TABLE … ADD COLUMN` rejects a non-constant default, so the `DEFAULT (fn())`
  form needs `CREATE TABLE`; for an existing table the crate installs an `AFTER INSERT`
  backfill trigger and backfills the rows.
- **The range trigger fires only when the registry knows the row's owner.** A site
  created mid-session may have its rows arrive before the registry row does; rejecting
  them would deadlock the conversation.
- **The `_sync_log` timestamp is the row's `last_update`, not a second clock.** The
  trigger writes both from one value, so they cannot disagree. The log needs it as an
  integer for bucketing and range scans, and it never reads a payload column.
- **`purge_on_us` is NULL while a row is live**, which makes "which tables have
  tombstones due at instant T" an indexed existence query.
- **The allocator guarantees "never reused".** Allocation is durable and happens
  *before* use: a gap is harmless, a reuse is corruption. The counter must not roll back
  with a transaction, so it is advanced outside the insert's transaction.
- **The registry's range is immutable.** A site is renamed; its range is not moved. A
  decommissioned site's range is retired forever, like a retired identifier.
- **A row names the site that owns its identifier, not the site that last wrote it.**
  The `site` column is replicated and in the digest; the range check is keyed on it, so
  a peer's rows are accepted on the same terms as local ones.
- **The registry is exchanged before the data** in every session — when it is in use —
  and is part of the join bundle alongside the epoch and the digest seed.

### Exit criteria

- Suite 2 (engine enforcement) passes in full, using raw SQL.
- Suite 3 (trigger mechanics) passes: one log row and one stamp per update, no
  recursion, identical behaviour with `recursive_triggers` on and off.
- Suite 13 (DDL) passes.
- Suite 18 (site registry and allocation) passes.
- The SQLite behaviour assertions are preserved as tests, so a SQLite upgrade that
  changes the semantics the design relies on fails loudly.
- A raw `INSERT` on a managed table from an instrumented connection results in a
  stamped row and a correct `_sync_log` entry, verified by inspecting the database
  directly rather than through the crate's API.
- **The allocator never reissues an identifier**: after a crash and restart, the next
  allocated id is strictly greater than every previously allocated one.
- **The registry enforces its invariants**: an overlapping range is refused on insert, a
  range change is refused on update, and a descriptive update is accepted.
- **The range check rejects a bad mint, accepts a peer's rows, and defers an unknown
  owner** — including on the `ON CONFLICT DO UPDATE` path.
- **The `site` column is filled by the engine**: an insert that omits it gets the local
  site, an explicit value wins, and a connection without the UDF fails closed.
- **The crate builds without the `alloc` feature**, and a deployment with no
  `AllocationSpec` has no registry, no `site` column and no range trigger.
- **A caller-owned connection behaves like a pooled one**: after `register_on`, a write
  is stamped and logged, and the authorizer denies the same operations.
- **The monotonic guard holds across connection kinds**: a timestamp issued on a
  caller-owned connection is greater than the last one issued on a pooled one.

**Test suites:** 2, 3, 13, 18.

---

## C2 — The bucket tree

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
- Memory matches the table in [merkle.md](../doc/merkle.md) § "Memory".

**Test suites:** tree units, plus the incremental-vs-rebuild property.

---

## C3 — Comparison, merge and bulk apply

### Deliverables

- `diff.rs`: the summary / detail / rows comparison logic, operating on abstract trees
  rather than on a transport.
- The merge rule — LWW on `(last_update, digest)`, with the digest tie-break — plus the
  per-table merge policy hook and the write-authority check.
- The row-level diff in both modes: `digest` (compact) and `row` (simple).
- `bulk.rs`: the bulk apply path — chunked multi-row `VALUES`, the JSON bulk path, and
  the staging-table path. See [bulk-apply.md](../doc/bulk-apply.md).

### Notes

- **The engine must not depend on axum.** The convergence test drives several nodes
  in one process through an in-memory transport — see
  [demo](demo.md).
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

**Test suites:** 1, 8, 9, 16, 19.

---

## C4 — The session

### Deliverables

- `session.rs`: the protocol state machine, transport-independent.
- `paging.rs`: the page cursor, the byte budget, and the resumable page sequence.
- `directory.rs`: the UUID directory — the local table, the handshake merge, and the
  per-node revocation check.
- The durable client session: the `_sync_session` journal and the `_sync_page` staging
  table, so a client that needs to survive a restart re-opens its session by id.
- The progress model: `SyncProgress`.
- Auth: the peer list check, the credential check, and the binding of the
  application's authenticated identity to the claimed node id.
- The message budget (`max_message_bytes`) and the round limit
  (`max_rounds_per_session`).
- The engine traits from `merkle-lww-protocol` are implemented here.

### Notes

- **The session must not know about HTTP.** It carries request and response *messages*,
  not bytes.
- **The in-memory session registry is the transport's, not the engine's.** The opaque
  `session_id` and its idle TTL are HTTP-level routing state, and they live in
  `merkle-lww-axum`. What the engine owns is the durable journal, which is what lets a
  client survive a restart.
- **A page is bounded by bytes, not by rows.** The sender stops when the encoded message
  would exceed `max_message_bytes` and returns a cursor.
- **The cursor is deterministic**: buckets in order, rows within a bucket by canonical
  key bytes, so both sides agree on the order without exchanging it.
- **The budget is measured on the JSON message**, before any transport-level content
  encoding. See [ADR-0023](../doc/adr/0023-json-wire-format-and-transport-compression.md).
- **A session that hits the round limit ends with `SESSION_LIMIT`**, which is retryable:
  the caller re-opens with the last cursor.
- **Tables are synced in `sync_priority` order, newest buckets first**, so a client
  catching up is usable before it is complete.
- **The core's transport abstraction is what lets the convergence test drive several
  nodes in one process.** It is a testability requirement as much as an extensibility
  one.
- **The application passes the authenticated identity in**, and the crate binds it to
  the claimed node id. The crate does not authenticate the transport.
- Progress frames are throttled to 250 ms and only sent when `completed` advances.

### Exit criteria

- Suite 20 (paging and budgets) passes.
- **A page never exceeds `max_message_bytes`,** and a page re-issued with a cursor
  continues exactly where the last one stopped.
- **Suite 22 (durable client sessions) passes**: a client that restarts mid-transfer
  resumes from its cursor.
- **The core has no dependency on axum**, and the exchange works over a non-HTTP
  transport.
- The session runs against the fake engine in `merkle-lww-protocol`'s test support, with
  no database.

**Test suites:** 20, 22.

---

## C5 — The lifecycle

### Deliverables

- `lifecycle.rs`: the scheduled purge — computing the instants from the epoch, the
  period and the offset; the quiesce window; the staleness check; the re-seed path.
- The purge floor on incoming rows.
- `last_purge_instant` advertisement.
- The `move_key` operation, atomic across the tombstone and the insert.

### Notes

- **The purge floor applies to tombstones only.** An incoming row is refused iff it is
  a tombstone whose `purge_on` is at or before the receiver's `last_purge_instant`. A
  live row is never refused, however old — under a daily purge, a write-timestamp
  comparison would refuse nearly every live row and break seeding, re-seeding and
  catch-up. A stale peer's live rows cannot arrive at all, because a stale peer's
  pushes are refused.
- **`purge_on` is uniform per window.** It is computed from the delete's window — the
  interval between two consecutive purge instants — so every tombstone in a window
  shares one deadline, whatever the ratio of `retention` to `purge_period`, and the
  tombstone lives at least `retention`. See
  [ADR-0022](../doc/adr/0022-one-purge-instant-per-window.md).
- **`purge_offset_us` is deployment-wide**, not local policy, and a mismatch is fatal
  at handshake — the same class as `epoch_us`.
- **Missing a purge instant is not fatal.** The side that is behind purges locally to
  the peer's instant and carries on. Only the staleness horizon — `now - last_sync_us >
  retention` — is fatal.
- **The staleness rule is evaluated against the mesh-wide last sync**, not per link.
- **The staleness rule is symmetric.** A stale peer may pull, but it may not push: its
  pushes are refused with `STALE_PEER`. A session with a stale peer is pull-only.
- **A re-seed wipes; it never merges.** The stale peer purges locally to the peer's
  instant, discards its synced state, and pulls the peer's dataset. It does not push:
  its unsynced writes are discarded, because it cannot tell a row that was deleted and
  purged while it was away from one that was merely old. See
  [ADR-0021](../doc/adr/0021-re-seed-is-a-wipe.md).
- The purge at an instant removes everything due — `purge_on <= T` — so a node that
  missed an instant catches up rather than orphaning its backlog.
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
- **Suite 21 (re-seed) passes:** a stale peer wipes and pulls; its unsynced writes are
  not pushed, the catch-up purge happens **before** the wipe, and a purged live row is
  not resurrected.
- The resurrection invariant is stated as a test, not only as a proof in the docs: a
  randomised sequence of offline intervals asserts that resurrection never happens —
  within the retention window or beyond it — and that a peer beyond the horizon
  re-seeds by wiping.

**Test suites:** 5, 6, 21.

---

## C6 — The cache, the audit table and the metrics

### Deliverables

- `cache.rs`: the read-through cache, with the byte and entry bounds.
- The optional `_sync_audit` table: which peer applied which row, locally.
- The counters and gauges from [operations.md](../doc/operations.md) § "Metrics".

### Notes

- The cache accelerates point lookups by primary key and nothing else. It must not
  pretend otherwise.
- **The audit table is off by default, node-local, and not in the digest** — turning it
  on cannot affect convergence.
- **The engine owns the counters**, because the engine knows what it compared, sent,
  applied and purged. The transport exposes them; see
  [axum's A4](axum.md).
- `clock_offset_estimate_us` is the node's estimate of its peers' clock offsets, and it
  is what makes a `CLOCK_SKEW` refusal diagnosable.

### Exit criteria

- The cache is bounded, and its bounds are enforced.
- Turning the audit table on changes no digest and no tree.
- Every metric in [operations.md](../doc/operations.md) § "Metrics" is produced by the
  engine, and the names match that table exactly.
- A counter is monotonic and a gauge is not.

---

## Crate-level exit criteria

- The crate builds with **no `tokio` and no `axum`**, and the exchange works over a
  non-HTTP transport.
- `cargo clippy` is clean; `cargo fmt --check` is clean.
- The crate builds **without the `alloc` feature**.
- Suite 10 (visibility) passes at the engine level: the digest's input is the full
  replicated column set, a narrow peer's tree matches a wide peer's, and no restricted
  value reaches a log line or an error message. The wire-bytes half of Suite 10 is
  [axum's](axum.md) responsibility, and the CLI's output half is [the CLI's](cli.md).
- Suite 4 (foreign connections) passes: enforced mode fails closed; compatible mode
  succeeds and is drained.
- Suite 7 (clock skew) passes, at the ±5 s default.
- Suite 11 (key strategies) passes for all five strategies.
- Suite 17 (peer authentication) passes at the engine level: the directory, the
  credential check and the identity binding. The transport's half is
  [axum's](axum.md).
- Documentation is updated to match the implementation.

**Test suites:** 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 16, 17, 18, 19, 20, 21, 22, plus
the tree properties.
