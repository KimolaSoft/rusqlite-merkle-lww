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

- **Two nodes and three nodes**, random operations, random partitions and heals.
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
- **A live row is never refused by the purge floor.** Push a row whose `last_update` is
  older than the receiver's `last_purge_instant` — under a daily purge, that is most
  rows — and assert it is accepted.
- **A tombstone the receiver has already purged is refused** with `ROW_PURGED`, and the
  sender purges it at the next instant.
- **A stale peer's push is refused**, in both directions, with `STALE_PEER`. A session
  with a stale peer is pull-only.
- **The resurrection invariant is unconditional**: a peer offline across the whole
  retention window — long enough for its tombstone to be purged — cannot bring a
  deleted row back, because it re-seeds by wiping and never pushes.

### 6. Purge

- Run across a purge instant with a node offline. Assert the returning node purges
  locally before syncing and does not re-push stale copies.
- Assert all nodes purge the same set at the same instant.
- Assert sync is refused with `PURGE_IN_PROGRESS` during the quiesce window.
- **The quiesce window is per table.** Assert that a session about a table with no
  tombstones due proceeds while another table is quiescing, and that a node with
  nothing due does not quiesce at all.
- **Missing a purge instant is not fatal.** A peer whose `last_purge_instant` is behind
  catches up and the session continues; no error is raised.
- **`purge_on` is uniform per window.** A *window* is the interval between two
  consecutive purge instants. Two rows deleted in the same window get the same
  `purge_on`, whatever the ratio of `retention` to `purge_period` — assert this with a
  retention that is **not** a whole number of periods, which is the case a simpler
  rounding rule gets wrong.
- **A tombstone lives at least `retention`.** Assert `purge_on - delete_time ≥
  retention` for a delete at the start of a window and at the end of one.
- **A node that missed an instant purges its backlog.** A tombstone whose deadline has
  passed is removed at the next instant, not orphaned.
- **`purge_on` is only ever set on a deleted row.** A live row's `purge_on` is NULL,
  and a direct write to the column is denied.
- **`purge_offset_us` moves the instant.** A deployment with a non-zero offset purges
  at that offset, and every node agrees on the instant.
- **A `purge_offset_us` mismatch is fatal at handshake** with
  `PURGE_SCHEDULE_MISMATCH`, like `epoch_us`.

### 7. Clock skew

- Inject ±5 s skew — the default `max_skew`. Assert convergence is unaffected.
- Inject a skew beyond `max_skew`. Assert the peer is refused with **`CLOCK_SKEW`**.
- **The `as_of_us` tolerance is not a skew check.** Two peers choosing a comparison
  depth more than `as_of_tolerance` apart is **not** an error: the server's depth wins
  and the client adopts it. Assert that the session continues, and that the client
  reports having adopted the server's depth.
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
- Assert a client requesting a restricted column is refused with
  `COLUMN_NOT_PERMITTED`, and that the refusal is **not** fatal: the column is dropped
  from the projection, the request is logged, and the session continues.
- Assert a table excluded by the client's profile is absent from the client's handshake.
- Assert the client cannot recompute a digest, and that a brute-force over a
  low-entropy restricted value does not produce a matching bucket hash.
- **The profiles**, each asserted separately:
  - a credential column is visible master↔master and **not** to children;
  - a payload column is withheld from a client profile;
  - a table is excluded from one role and included in another;
  - a per-peer profile override gives one client a narrower view than the others;
  - a profile may never expose a `Restricted` column to an untrusted profile.
- **`visibility_hash`**: two trusted peers with different profile definitions are
  refused at handshake with `VISIBILITY_MISMATCH`; a client does not compare it and
  does not fail.
- **The digest's input is the full replicated column set.** A child whose profile hides
  a column and a master that can see it agree on the digest of the same row: the child
  receives the digest verbatim and never recomputes it.
- **The tree is unaffected by a narrow profile.** A peer holding a partial row still
  computes the same `row_hash` and its tree matches the master's — asserted by running
  a diff between a master and a child with a narrower profile.
- **A profile may narrow a `Public` column.** A profile that hides a public column from
  one role is honoured, and the column does not appear in that peer's rows.

### 11. Key strategies

- For each of the five strategies, run the convergence test.
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
- Handshake rejection cases: epoch mismatch, seed mismatch, unknown peer, stale peer.
  **Not** a restricted-column request — that one is refused without ending the session,
  and it is asserted in Suite 10.

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

### 17. Peer authentication and the UUID directory

- **A node outside the directory is refused**: an unknown UUID is rejected with
  `UNKNOWN_PEER`, whatever credential it presents.
- **A wrong-role credential is refused**: a master presenting the child string is
  rejected with `BAD_CREDENTIAL`, and a child presenting the master string likewise.
- **A child cannot be impersonated with a UUID it does not know**: the directory is never
  sent to a child, and a child's handshake carries only its own entry.
- **The directory merges across masters**: after a session between two masters, each holds
  the union of both directories.
- **Revocation is per node**: removing a UUID from the directory refuses that node
  immediately, with no credential rotation and no effect on any other node.
- **Mutual authentication works with one secret**: a child verifies a master's proof
  against its own child string.
- **The digest seed reaches children**: a child computes the same digest for the same
  row as a master.
- **No secret is written to the database**: after a full session, the digest seed, the
  role strings and the credentials are nowhere in the file — verified by scanning the
  database bytes, not by inspecting the API.
- **A schema mismatch is fatal for a trusted peer and a warning for a client**: two
  trusted peers differing only in registered column order are rejected at handshake.
- **The authenticated identity binds to the claimed node id**: a HELLO whose `node_id`
  differs from the identity the transport authenticated is refused with
  `IDENTITY_MISMATCH`.
- **Per-node credentials attribute the session**: with `PerNodeSecret`, the
  authenticating node is known, and the audit table records it.
- **The audit table is local and optional**: it is never replicated, never in the
  digest, and enabling it does not change a single stored digest.

### 18. Site registry and allocation

- **The range check**: an identifier inside the row's own site's range is accepted; one
  outside it is refused — on the plain `INSERT` path and on the `ON CONFLICT DO UPDATE`
  path.
- **A peer's rows are accepted on the same terms**: a row whose owner is a different site
  is inserted without complaint.
- **An unknown owner is deferred**: a row whose owner is not yet in the registry is
  accepted, and is re-validated once the registry catches up.
- **The registry enforces its own invariants**: an overlapping range is refused on insert,
  a range change is refused on update, a descriptive update is accepted, and a duplicate
  `uid_start` is refused.
- **The `site` column is filled by the engine**: an insert that omits it gets the local
  site; an explicit value wins; a connection without the UDF fails closed.
- **The allocator never reissues an identifier**: after a crash and restart, the next
  allocated id is strictly greater than every previously allocated one, and a rolled-back
  insert leaves a gap rather than a reusable id.
- **A decommissioned site's range is retired**: after the site is soft-deleted, its range
  is never reissued.
- **The allocator is optional**: a deployment with no `AllocationSpec` has no registry
  table, no `site` column, no range trigger, and no REGISTRY phase — and it syncs
  correctly. The crate builds without the `alloc` feature.

### 19. Merge policies and write authority

- **The default is LWW**, and the default is what runs when no policy is declared.
- **A custom policy converges.** Run the convergence suite with a custom policy in the
  loop — first-allocation-wins on a lease table — and assert the nodes agree on the
  winner. A non-convergent policy is a bug in the policy, and this test is how it is
  caught.
- **An epoch-ordered ownership field is re-asserted after LWW**: the higher epoch wins
  the ownership field even when the lower epoch won the row.
- **Batch repair runs on the sender**: the repair is applied before replication, both
  peers store the same bytes, and the digests agree. A receiver-side repair would
  diverge — asserted by checking that the digests match after a repaired batch.
- **Write authority is evaluated against the origin**: a row written by a child for a
  `MasterOnly` table is refused with `WRITE_NOT_PERMITTED`, even when it arrives via a
  master.
- **A peer may not write a table whose columns its profile hides.**
- **A policy that returns `Undecided` falls through to the LWW + digest tie-break.**
  The variant exists and is reachable; a policy author must never have to invent a
  decision for a case they do not understand.

### 20. Paging and message budgets

- **No message exceeds `max_message_bytes`**, whatever the batch size.
- **A page is resumable**: re-issuing a ROWS with the returned cursor continues exactly
  where the last page stopped, and the transfer converges to the same state as an
  unpaged one.
- **Re-sending a page is harmless** — the merge is idempotent.
- **The SUMMARY hash list is truncated as a prefix**, and the two sides still agree on
  the differing set.
- **The round limit is retryable**: a session that hits `max_rounds_per_session` ends
  with `SESSION_LIMIT`, and re-opening it with the last cursor continues the transfer.
- **The pull-only client flow uses the same bounds**: the server's proactive push is
  paged, and the client applies every page.
- **The cursor has one shape.** A `next_cursor` returned by a response is accepted
  verbatim as the `cursor` of the next request — same fields, including `as_of_us`.
- **A cursor from a different `as_of_us` is rejected**, not applied.
- **The ROWS response carries the origin**, because that is where the rows are: a row
  whose origin lacks write authority is refused with `WRITE_NOT_PERMITTED` on receipt,
  not only when it is requested.

### 21. Re-seed

- **A re-seed is a wipe.** A child accepts writes while offline, goes stale, and
  re-seeds. Assert the child's unsynced writes are **not** pushed, that they are gone
  from the child afterwards, and that the child ends up with the peer's copy of the
  rows.
- **A stale peer does not push.** A session with a stale peer is pull-only: the stale
  peer's pushes are refused with `STALE_PEER`, in both directions.
- **A re-seed does not resurrect a purged tombstone**: the stale peer purges locally to
  the peer's instant first, and the tombstone is gone from both sides afterwards.
- **A re-seed does not resurrect a purged live row either**: a peer that was offline
  across the whole retention window holds a stale *live* copy of a row that was deleted
  and purged while it was away. Assert the master refuses it and the row stays gone.
- **The catch-up purge happens before the wipe**, so the stale peer does not carry its
  own tombstones into the rebuild.
- **A client re-seed is a pull** — it has no local writes, and the result is the same.

### 22. Durable client sessions

- **A client resumes after a restart**: interrupt a transfer mid-page, restart the
  client, and assert it continues from its cursor rather than starting again.
- **The cursor advances only after the commit**: a crash between "received" and
  "applied" loses nothing — the staged rows are applied on restart.
- **Staging is dialect-independent.** The durable staging table exists and is used in
  the **enforced** dialect, which is the default. The compatible dialect's drainer
  staging is a different thing and does not stand in for it.
- **The session survives the idle TTL** when it has a persisted cursor.
- **Ordering**: tables are synced in `sync_priority` order — core tables first — and
  within a table the newest buckets arrive first.

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
- `unixepoch('now','subsec')` is millisecond-resolution;
- a trigger cannot reference an object in an attached database;
- `ALTER TABLE … ADD COLUMN` rejects a non-constant default;
- an `AFTER INSERT` trigger does not fire on the `ON CONFLICT DO UPDATE` path.

A test that asserts each of these will fail loudly if a future SQLite version changes
the behaviour the design depends on, rather than failing mysteriously later.

## Fixtures

| Fixture | Purpose | Published by |
|---|---|---|
| `golden_vectors.json` | Canonical encoding → digest | `merkle-lww-model` |
| `schema_hash_vectors.json` | Registered schema → `schema_hash` | `merkle-lww-model` |
| `visibility_hash_vectors.json` | Profile definitions → `visibility_hash` | `merkle-lww-model` |
| `wire_vectors.json` | The exact bytes of every message type | `merkle-lww-protocol` |
| `topologies.json` | Multi-node topologies for the convergence test | `merkle-lww-demo` |
| `scenarios.json` | Named lifecycle scenarios (resurrection, purge race, staleness) | `merkle-lww-demo` |

Fixtures are JSON so they can be shared with an implementation in another language, which
is the point of specifying the encoding precisely.

**The crate that defines a format publishes its vectors.** The digest, the schema hash
and the visibility hash are defined by `merkle-lww-model`; the wire is defined by
`merkle-lww-protocol`; the topologies and the lifecycle scenarios are the harness's,
and belong to `merkle-lww-demo`.

## Property test seeds

The convergence test should record the seed for every failure. A failing seed is a
regression test from that point on.

## Acceptance criteria per crate

The plan is split by crate, and each crate's exit criteria are measured against these
suites. See [../PLAN.md](../PLAN.md) for the plans.

| Crate | Plan | Must pass |
|---|---|---|
| `merkle-lww-model` | [M](../plan/model.md) | 14, plus the `schema_hash` and `visibility_hash` checks |
| `merkle-lww-protocol` | [P](../plan/protocol.md) | the handshake validation table, the wire golden vectors, the value-form round-trip |
| `merkle-lww-core` | [C](../plan/core.md) | 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 16, 17, 18, 19, 20, 21, 22, plus the tree properties |
| `merkle-lww-axum` | [A](../plan/axum.md) | 10, 12, 15, 17, 20, 22 |
| `merkle-lww-cli` | [X](../plan/cli.md) | the operational tooling tests, and Suite 10 as it applies to the CLI's output |
| `merkle-lww-demo` | [D](../plan/demo.md) | the wire-vector assertions, the two-node fixture, the scripted-clock scenarios |

The numbering refers to the suites above. The tree criteria are not numbered because
they are properties of the tree rather than a suite; see "Property tests elsewhere".

Some suites span crates, and each has a **primary** crate — the one where the code
under test lives:

| Suite | Primary | Also | Notes |
|---|---|---|---|
| 1 (convergence) | core | demo | driven by the demo crate's in-memory transport |
| 10 (visibility) | core | axum, cli | the engine half is core's; the wire bytes and the CLI's output are the other two halves |
| 17 (peer authentication) | core | axum | the directory, the credential check and the identity binding live in core; the transport carries them |
| 20 (paging and budgets) | core | axum | the budget is the engine's; the transport must not break it |
| 22 (durable client sessions) | core | axum | the staging and the resume are the engine's; the transport only carries them |

## Test dependencies

The convergence test needs multiple nodes in one process or across processes. In-process
is simpler and faster, and the crate's separation makes it possible: several `Db`
instances, each with its own SQLite file, exchanging through an in-memory transport.

That requires the transport to be abstracted behind a trait rather than being HTTP-only.
This is a design constraint that should be honoured in the implementation: **the sync
engine must not depend on axum.** The in-memory transport and the two-node fixture live
in `merkle-lww-demo`, and the scripted clock there replaces sleeps with time
advancement — which is what makes the lifecycle failure modes deterministic rather than
flaky.

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
- [security.md](security.md) — the trust model behind Suite 10
- [../PLAN.md](../PLAN.md) — the crate index, the open questions and the risks
- [../plan/](../plan/) — one plan per crate
