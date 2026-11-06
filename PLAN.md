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
  `TableKind`, `Peer`, `PeerAuth`, `CacheConfig`, `Role`, `WriteMode`. All `serde`-derived
  with `Default`.
- `codec.rs`: canonical key and value encoding; seeded xxh3 digest; the two-level
  `row_hash`.
- `time.rs`: RFC 3339 parsing and formatting at fixed six-digit precision; the
  monotonic issuance guard.
- `error.rs`: the error type, including the protocol error codes.
- `SecretString`: a wrapper whose `Debug` does not print its contents.

### Notes

- The digest is implemented here, not in the store, because it depends only on the
  canonical encoding.
- The monotonic guard is a small type here (`TimestampIssuer`) holding an `AtomicI64`,
  with the persisted value loaded at startup and written back on change.
- The canonical encoding is the one thing that must not change once data exists. Get it
  right now.

### Exit criteria

- The workspace builds; `cargo clippy` is clean.
- Canonical encoding golden vectors pass, and are published as a fixture.
- The encoding is injective on a generated set of values: two distinct rows never encode
  to the same bytes (`proptest`).
- Timestamps round-trip through parse and format at fixed six-digit precision, and sort
  lexicographically in chronological order.

**Test suites:** 14 (canonical encoding).

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
- `trigger.rs`: trigger installation for both dialects; the managed column DDL.
- `authorizer.rs`: the denial rules, the `accessor` exemption, and the per-connection
  privilege flag.
- `log.rs`: `_sync_log` reads and writes.
- DDL management: `create_table`, `alter_table`, `adopt_table`, `drop_table`.
- The three UDFs: `sync_stamp()`, `sync_us(text)`, `sync_digest(...)`.

### Notes

- **The privilege flag is per-connection.** It cannot live on `Db`, because another
  thread may hold a different connection from the same pool.
- **Every pooled connection gets the UDFs and the authorizer.** A connection that
  escapes uninstrumented is a hole, so the connection factory is the only way to make
  one.
- **Every DDL path reinstalls the triggers** and rebuilds the tree.
- The compatible dialect's staging table and drainer are implemented here, since they
  are part of the write path.

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

**Test suites:** 2, 3, 13.

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
- The merge rule: LWW on `(last_update, digest)`, with the digest tie-break.
- The row-level diff in both modes: `digest` (compact) and `row` (simple).
- `session.rs`: the protocol state machine, transport-independent.
- Canonical serialisation of rows for the wire.
- `bulk.rs`: the bulk apply path — chunked multi-row `VALUES`, the JSON bulk path, and
  the staging-table path. See [doc/bulk-apply.md](doc/bulk-apply.md).

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

**Test suites:** 1, 8, 9, plus the bulk-apply suite.

---

## Phase 4 — Transport

**Goal.** The protocol over HTTP and WebSocket, as an optional convenience over a
transport-agnostic core.

### Deliverables

- The core's transport-agnostic exchange: `perform_exchange(peer, transport)`, where
  `transport` carries request and response messages. The session state machine must not
  know about HTTP.
- `merkle-lww-axum`: the REST endpoints and the WebSocket handler, built on the core.
- Sessions: opaque `session_id`, in-memory, five-minute TTL.
- The handshake and its validation.
- The progress model: `SyncProgress`, the endpoints, the WS frame.
- Auth: the peer list check and the credential check.
- `doc/implementation.md`, covering the integration for application developers.

### Notes

- The handshake validation is where `EPOCH_MISMATCH`, `DIGEST_SEED_MISMATCH`,
  `UNKNOWN_PEER`, `STALE_PEER` and `COLUMN_NOT_PERMITTED` are raised.
- `as_of_us` is server-supplied in a client→server session, so both sides choose
  identical comparison depths.
- Progress frames are throttled to 250 ms and only sent when `completed` advances.
- **The crate does not terminate TLS.** The application does, and handles authentication.
  The crate's own axum crate assumes TLS is terminated in front of it.
- The core's transport abstraction is what lets the convergence test drive several nodes
  in one process. It is a testability requirement as much as an extensibility one.

### Exit criteria

- Suite 15 (transport integration) passes for REST and WebSocket.
- Suite 12 (progress) passes: every phase's `total` is known before the phase runs,
  `completed` never exceeds `total`, and the denominator changes correctly between
  phases.
- Every handshake rejection case returns the documented error code.
- The pull-only client flow works: the server pushes the diff, the client applies it.
- The core has no dependency on axum, and the exchange works over a non-HTTP transport.

**Test suites:** 12, 15.

---

## Phase 5 — Lifecycle

**Goal.** Deletes, the purge schedule, the quiesce window, and the staleness rule.

### Deliverables

- The scheduled purge: computing the instants from the epoch and the period.
- The quiesce window: refusing sync with `503 PURGE_IN_PROGRESS`.
- `last_purge_instant` advertisement, and the purge floor on incoming rows.
- The staleness check at handshake, evaluated mesh-wide.
- The re-seed path.
- The `move_key` operation, atomic across the tombstone and the insert.

### Notes

- The purge floor rejects a row whose `hlc_us` is older than the receiver's
  `last_purge_instant`. It is approximate, because `hlc_us` is the writer's clock.
- The staleness rule is evaluated against the mesh-wide last sync, not per link.
- `move_key` is the only operation that changes a key, and it is atomic.

### Exit criteria

- Suite 5 (resurrection) passes in both directions: a peer offline longer than the
  retention is refused and must re-seed; a peer offline for less than the retention
  receives the tombstone and applies it.
- Suite 6 (purge) passes: all nodes purge the same set at the same instant; a node
  offline across a purge purges locally before syncing; sync is refused during the
  quiesce window.
- The resurrection invariant is stated as a test, not only as a proof in the docs: a
  randomised sequence of offline intervals asserts that resurrection never happens
  within the retention window, and always requires a re-seed beyond it.

**Test suites:** 5, 6.

---

## Phase 6 — Hardening

**Goal.** Visibility, the cache, the CLI, and observability.

### Deliverables

- The visibility model end to end: filtering at the serialisation boundary, the
  handshake restriction, and the `client_visible` table exclusion.
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

### Exit criteria

- Suite 10 (visibility) passes: no restricted value appears anywhere in the wire bytes,
  in any log line, or in any error message. **This test must never regress.**
- Suite 4 (foreign connections) passes: enforced mode fails closed; compatible mode
  succeeds and is drained.
- Suite 7 (clock skew) passes.
- Suite 11 (key strategies) passes for all four strategies.
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
| 4 | **How is the digest seed distributed?** Environment variable, mounted secret, or KMS. The crate stores it but does not implement a secret store. | Deployment; not implementation | — |
| 5 | **Wire format.** JSON, or binary from the start? | Protocol stability | Phase 4 |
| 6 | **Is the purge period daily, or aligned to the retention window?** Daily gives predictable instants; aligned to retention makes tombstones live exactly 30 days. | Purge cadence | Phase 5 |
| 7 | **Does the compatible dialect's drainer compute digests for the whole table or only for staged rows?** Only staged rows, presumably, but a row written and deleted within one interval is lost — acceptable, since it was never visible to a peer. | Drain semantics | Phase 1 |
| 8 | **Should `AppendOnly` tables exist?** They remove the delete machinery from the trigger set, at the cost of a second code path. | Scope of the trigger set | Phase 1 |

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
| Clock skew beyond the documented ±1 s | Medium | A skew check at handshake; peers believed to be skewed are refused |
| The month tier's memory grows unexpectedly | Low | Bounded by activity, not age; `max_tree_buckets` is a tripwire |
| A bulk apply exceeds SQLite's 32,766 bound-parameter limit | Medium | The chunk size is derived from the column count, and the JSON path uses a single parameter regardless of batch size. A 50,000-row batch is covered by a test |
| The application generates random ids where deterministic ones were needed, so rows never merge | Medium | Documented prominently in the key-strategy section and ADR-0006; `verify` can report entities that exist at more than one site with the same natural key |
| A site uses `GlobalId` on data that is genuinely per-site, and two records merge | Low | `SiteNamespaced` is a per-table option; the failure is data loss, so the default should be reviewed when the deployment shape changes |
