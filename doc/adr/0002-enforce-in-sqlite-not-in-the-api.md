# ADR-0002: Enforce invariants in SQLite, not in the API

- **Status:** accepted
- **Date:** 2026-11
- **Deciders:** project maintainers

## Context

The crate must guarantee five invariants:

1. `last_update` is stamped on every write.
2. `_sync_log` is updated on every write.
3. Protected columns (`status`, `purge_on`) cannot be written by the application.
4. Hard `DELETE` on a managed table cannot happen outside the crate.
5. Internal tables (`_sync_*`) cannot be written by the application.

There are two places these could live.

**In the Rust API.** The crate exposes a write API and the application uses it. The
invariants hold for code that goes through the API.

**In the engine.** SQLite enforces them, using triggers and the authorizer callback.
The invariants hold for any SQL, from any connection.

The question came up as: *"we may need to offer wrappers for all standard SQL
operations, right? Or … just read and write and we bury the fact that it's SQL and we're
the new db interface?"*

## Decision

**Enforce the invariants inside SQLite.** The crate owns the *connection*, not the
*query language*.

The application borrows a pooled `rusqlite::Connection` and writes plain SQL. There is
no ORM, no query builder, and no wrapper for `SELECT` versus `INSERT` versus `UPDATE`
versus `DELETE`.

| Guarantee | Mechanism |
|---|---|
| 1, 2 | `AFTER INSERT` / `AFTER UPDATE` / `AFTER DELETE` triggers |
| 3, 4, 5 | `sqlite3_set_authorizer` callback |

## Consequences

**Positive**

- **The guarantees have no gaps.** Raw SQL is one `conn.execute()` away from bypassing
  an API-level wrapper. Engine-level enforcement has no such gap: the trigger fires and
  the authorizer denies, regardless of how the statement was issued.
- **The application keeps its SQL.** No query builder to learn, no wrapper to escape,
  no performance tax.
- **A managed convenience layer is still available.** `Db::upsert`, `Db::delete` and
  `Db::move_key` exist because they are convenient and because `move_key` and soft delete
  need crate privilege. They are sugar, not the mechanism.
- **The authorizer is a real security boundary**, not a convenience. It is the only
  thing standing between the application and the internal tables.

**Negative**

- Every connection must be instrumented. A pooled connection checked out before
  installation, or opened by the caller rather than the pool, is unprotected. This is a
  sharp edge, and the crate provides a connection factory to close it.
- The authorizer is per-connection, so the rules must be installed on each one.
- The trigger set must be kept in step with the schema. Every DDL path must reinstall the
  triggers, which is easy to forget and is covered by a test.
- SQLite-specific behaviour becomes load-bearing. `SELECT NEW.col = expr` not assigning,
  the `accessor` field on the authorizer context, and the recursion guard on a nested
  trigger update are all now things the design depends on. They are documented and
  tested.

**Neutral**

- The crate requires `rusqlite` features `functions` and `hooks`.

## Alternatives considered

**An ORM or query wrapper.** Rejected. SQLite has around thirty distinct feature
families — JSON1, FTS5, R-tree, window functions, recursive CTEs, generated columns,
`RETURNING`, upsert, partial and expression indexes, custom collations and functions,
virtual tables, `STRICT`, `WITHOUT ROWID`, deferrable constraints — and any wrapper is
permanently behind. Applications escape the wrapper the first time they need a feature
it lacks, and the escaped code is exactly the code the wrapper was supposed to protect.

**Expose a `Connection` and hope.** Rejected: without the authorizer, guarantees 3, 4
and 5 are advisory at best.

**Triggers only, no authorizer.** Rejected: triggers cannot distinguish an application's
write to `status` from the crate's soft delete, because both are `UPDATE`s. The
authorizer can, using a per-connection privilege flag.

**Authorizer only, no triggers.** Rejected: the authorizer can deny, but it cannot
*compute* — it cannot stamp a timestamp or maintain `_sync_log`.

## Related

- [../sqlite-interface.md](../sqlite-interface.md) — the trigger set, the authorizer
  rules, and the verified SQLite behaviour this decision rests on
- [ADR-0010](0010-no-orm-query-wrapper.md) — the no-ORM decision, in full
