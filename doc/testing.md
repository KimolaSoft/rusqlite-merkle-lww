# Testing

## The suite that matters

**Convergence.** Three nodes, random operation sequences — upserts, deletes, partitions,
heals — in random orders. After quiescence, every node must have identical tree hashes
*and* identical log contents.

This is the test that validates the entire design. If it fails, something fundamental is
wrong, and no amount of unit-test coverage elsewhere will compensate. It is a `proptest`
with a shrinking corpus so failures replay minimally.

Everything else in this document exists to support it.

## Suites

### 1. Convergence

- Three nodes, random operations, random partitions and heals.
- Assert identical tree hashes across all nodes.
- Assert identical `_sync_log` contents across all nodes.
- Include moves (`move_key`) and un-deletes in the operation set.
- Include both write dialects.

### 2. Engine enforcement

The critical suite, and it uses **raw SQL**, not the crate's API — that is the whole
point of the design.

- raw `INSERT` without `last_update` → the value is stamped and the row is logged;
- raw `INSERT` with an explicit `last_update` → **the application's value wins**;
- raw `UPDATE` of a payload column → `last_update` advances, the row is re-logged;
- raw `UPDATE` of `status` or `purge_on` → **denied** ("not authorized");
- raw `DELETE` on a managed table → **denied**;
- raw write to `_sync_log` → **denied**;
- raw `DROP TABLE` on a managed table → **denied**;
- a crate-privileged soft delete → **allowed**, and the log row reflects it;
- a crate-privileged purge (hard delete) → the log row is removed with it;
- the trigger's own writes to `_sync_log` are **not** denied by the authorizer (the
  `accessor` exemption).

### 3. Trigger mechanics

- A raw `UPDATE` produces exactly **one** log row and **one** stamp, not a cascade.
- The row count is unchanged after a trigger-driven update.
- Behaviour is identical with `recursive_triggers` ON and OFF.
- `DEFAULT (udf())` fires when the column is omitted; an explicit value is preserved.

### 4. Foreign connections

- **Enforced dialect:** a write from a connection without the UDFs registered fails with
  "no such function" and changes nothing.
- **Compatible dialect:** the same write succeeds and is picked up by the drainer.
- A row written and deleted within one drain interval is missed in compatible mode —
  asserted, so the behaviour is documented rather than incidental.

**The compatible-dialect guarantees**, each asserted from a *foreign* connection with no
authorizer installed:

- A hard `DELETE` of a live row is **converted to a soft delete** — the row survives with
  `status = 'deleted'`, and the tombstone is staged.
- A hard `DELETE` of an existing tombstone is **blocked**; the row survives.
- A write to `_sync_log` is **rejected** by the log guard.
- The crate's own purge — with the guard marker set — **does** remove the row physically.
- A write to `status` or `purge_on` **succeeds** (the documented gap). Asserted, so the
  gap is documented rather than incidental.

The last assertion matters as much as the others: a test that asserts the *absence* of a
guarantee will fail loudly if someone later adds a guard, prompting them to update the
documentation rather than silently changing the contract.

### 5. Resurrection

- A deletes K. B is offline before the delete. B returns after the purge instant.
  Assert the staleness rule refuses B, and that after re-seed both agree K is gone.
- A deletes K. B is offline for **less** than the retention. B returns. Assert the
  tombstone is still present and B applies the delete on reconnect.

### 6. Purge

- Run across a purge instant with a node offline. Assert the returning node purges
  locally before syncing and does not re-push stale copies.
- Assert all nodes purge the same set at the same instant.
- Assert sync is refused with `PURGE_IN_PROGRESS` during the quiesce window.

### 7. Clock skew

- Inject ±1 s skew. Assert convergence is unaffected.
- Assert the monotonic issuance guard prevents a backwards clock from regressing a row:
  a write after a clock step still gets a `last_update` greater than the previous one.

### 8. Tie-break

- Force two peers to produce the same `last_update` for the same key with different
  payloads. Assert both converge on the same winner.

### 9. Divergence detection

- Mutate a row via raw SQL without advancing `last_update`. Assert `verify` reports it.
- Assert that it is reported and **not** silently repaired.

### 10. Visibility

- Run a full sync to a client and assert that no `Restricted` column value appears
  anywhere in the wire bytes, in any log line, or in any error message. **This test must
  never regress.**
- Assert a client requesting a restricted column is rejected.
- Assert a table marked `client_visible = false` is absent from the client's handshake.
- Assert the client cannot recompute a digest, and that a brute-force over a
  low-entropy restricted value does not produce a matching bucket hash.

### 11. Key strategies

- For each of the four strategies, run the convergence test.
- Assert the expected cross-site behaviour: `GlobalId` merges; the others do not.
- Assert key columns are immutable — attempting to change one is refused, and
  `move_key` is atomic.

### 12. Progress

- Assert each phase's `total` is known **before** the phase runs.
- Assert `completed` never exceeds `total`.
- Assert the denominator changes correctly between phases.
- Assert a WS progress frame is emitted at the configured cadence.

### 13. DDL

- Create, alter (add managed column), adopt an existing table.
- Confirm the refuse-rather-than-rebuild behaviour on a primary key change.
- Confirm every DDL path reinstalls the triggers and rebuilds the tree.

### 14. Canonical encoding

Golden vectors: fixed bytes → fixed digest. Published as a test fixture so another
language can implement the same format.

### 15. Transport integration

- REST: `tower::ServiceExt::oneshot`.
- WebSocket: a real client against a real server.
- Handshake rejection cases: epoch mismatch, seed mismatch, unknown peer, stale peer,
  restricted column request.

## Testing the SQLite mechanics

The behaviour in [sqlite-interface.md](sqlite-interface.md) § "Verified SQLite behaviour"
was established experimentally against rusqlite 0.41.1 with bundled SQLite. Those
experiments should be preserved as tests, because SQLite behaviour is version-dependent
and the design leans on several subtle points:

- `SELECT NEW.col = expr` does not assign;
- `DEFAULT (udf())` works and an explicit value wins;
- the authorizer sees trigger-internal writes, and `accessor` identifies them;
- the authorizer can deny a write to a single column;
- a nested `UPDATE` in a trigger recurses unless guarded;
- a UDF-calling trigger fails on a connection where the UDF is not registered;
- `unixepoch('now','subsec')` is millisecond-resolution.

A test that asserts each of these will fail loudly if a future SQLite version changes
the behaviour the design depends on, rather than failing mysteriously later.

## Fixtures

| Fixture | Purpose |
|---|---|
| `golden_vectors.json` | Canonical encoding → digest |
| `topologies.json` | Multi-node topologies for the convergence test |
| `scenarios.json` | Named lifecycle scenarios (resurrection, purge race, staleness) |

Fixtures are JSON so they can be shared with an implementation in another language, which
is the point of specifying the encoding precisely.

## Property test seeds

The convergence test should record the seed for every failure. A failing seed is a
regression test from that point on.

## Acceptance criteria per implementation phase

Each phase of [../PLAN.md](../PLAN.md) is measured against these suites:

| Phase | Must pass |
|---|---|
| 0 — Foundations | 14 |
| 1 — Store and enforcement | 2, 3, 13 |
| 2 — Merkle tree | tree units, and the incremental-vs-rebuild property |
| 3 — Diff engine | 1, 8, 9 |
| 4 — Transport | 12, 15 |
| 5 — Lifecycle | 5, 6 |
| 6 — Hardening | 4, 7, 10, 11, and the full suite |

The numbering refers to the suites above. Phase 2's criteria are not numbered because
they are properties of the tree rather than a suite; see "Property tests elsewhere".

## Test dependencies

The convergence test needs multiple nodes in one process or across processes. In-process
is simpler and faster, and the crate's separation makes it possible: several `Db`
instances, each with its own SQLite file, exchanging through an in-memory transport.

That requires the transport to be abstracted behind a trait rather than being HTTP-only.
This is a design constraint that should be honoured in the implementation: **the sync
engine must not depend on axum.**

### 16. Bulk apply

- A 50,000-row batch applies without exceeding SQLite's bound-parameter limit.
- The chunk size adapts to the column count: `rows × columns ≤ 32,766` for every chunk,
  at several table widths.
- A bulk apply is **atomic** — a failure part-way through leaves nothing applied, and
  the `_sync_log` is consistent with the user table.
- The LWW filter works in the upsert: an older incoming row does not overwrite a newer
  one.
- The digest tie-break is applied in Rust: an equal timestamp with a higher digest wins,
  consistently on both peers.
- All three bulk paths (chunked `VALUES`, JSON, staging table) produce identical
  results.
- The JSON path requires `FROM json_each(?1) AS j` and `WHERE true` before
  `ON CONFLICT` — asserted, so a refactor cannot silently break it.

## Property tests elsewhere

Beyond convergence:

- The canonical encoder is injective on a generated set of values — two distinct rows
  never encode to the same bytes.
- The bucket assignment is a function of `last_update` alone — the same timestamp always
  lands in the same bucket, on any node.
- The commutative hour-hash sum matches a full recompute — incremental maintenance never
  drifts from a rebuild.

The last one is important: it is the invariant that makes the incremental update
trustworthy, and drift would be invisible until it caused a permanent divergence.

## Related documents

- [sqlite-interface.md](sqlite-interface.md) — the mechanics under test
- [lifecycle.md](lifecycle.md) — the resurrection and purge scenarios
- [security.md](security.md) — the visibility leak test
- [../PLAN.md](../PLAN.md) — phase ordering and exit criteria
