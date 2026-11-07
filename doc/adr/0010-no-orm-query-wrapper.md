# ADR-0010: No ORM, no query wrapper

- **Status:** accepted
- **Date:** 2026-11
- **Deciders:** project maintainers

## Context

The application reads and writes the synced tables. The question raised was:

> we may need to offer wrappers for all standard sql operations, right? Or ... just read
> and write and we bury the fact that it's SQL and we're the new db interface?

This is the question of whether the crate becomes the database interface — providing
typed operations for `SELECT`, `INSERT`, `UPDATE`, `DELETE` and DDL, with the application
expressing queries through Rust types rather than SQL.

Three positions are possible:

- **Wrap everything.** The crate models SQL operations as Rust types. The application
  never writes SQL.
- **Bury SQL entirely.** The crate provides a query API — a builder, an ORM — and SQL
  is an implementation detail.
- **Neither.** The application writes SQL. The crate owns the connection and guarantees
  the sync invariants underneath.

## Decision

**Neither. The crate provides `Db::conn()` — a pooled `rusqlite::Connection` — and the
application writes plain SQL.**

There is no query builder, no ORM, and no wrapper for any SQL operation.

```rust
let conn = db.conn()?;                       // instrumented
conn.execute("INSERT INTO device(sync_id, payload) VALUES (?1, ?2)",
             params![sync_id, payload])?;       // plain SQL
```

A small managed layer exists for convenience — `Db::upsert`, `Db::delete`,
`Db::move_key` — and is documented as sugar.

The sync invariants are enforced inside SQLite, so they hold for the application's raw
SQL as well. See [ADR-0002](0002-enforce-in-sqlite-not-in-the-api.md).

## Consequences

**Positive**

- **The application keeps full access to SQLite.** Every feature, today and tomorrow.
- **No query language to learn, and no wrapper to escape.** Applications escape query
  builders the first time they need a feature the builder lacks — and then they are
  outside the guarantee the wrapper provided, which defeats the purpose.
- **No dependency.** A query builder or ORM is a substantial dependency with its own
  opinions, and this crate takes on none of that.
- **The invariants have no gaps.** Because enforcement is at the engine level, raw SQL
  is protected exactly as the managed API is. This is the strongest argument: a wrapper
  would protect only the paths that go through it.
- **Performance is the application's choice.** No wrapper imposes a query shape.

**Negative**

- **The application must know the crate's rules.** Writing `status` or `purge_on`
  directly, or hard-deleting a managed row, produces an authorisation error rather than
  a helpful message. The errors must be clear, and the documentation must say what is
  managed.
- **The application is responsible for its own query quality.** No query planner, no
  N+1 detection. This is what an application normally expects from a database driver,
  and it is not a regression.
- **The crate must keep the read cache honest.** The cache can only accelerate point
  lookups by primary key; arbitrary queries always hit SQLite. Documenting that
  honestly matters, or consumers will expect more than they get.

**Neutral**

- The crate is a library over `rusqlite`, not a replacement for it. An application that
  wants `sqlx` or `diesel` for other tables can use them, as long as writes to *managed*
  tables go through an instrumented connection.

## Alternatives considered

**Wrap all standard SQL operations.** Rejected: it is an ORM, and an ORM over SQLite is
permanently behind. SQLite has around thirty distinct feature families, and the
wrapper grows forever.

**Provide a query builder.** Rejected: same objection, with less of the convenience.
Builders are pleasant for simple cases and awkward for the queries that matter, and they
encourage a second data-access style alongside whatever the application already uses.

**Hide SQLite behind a repository interface.** Rejected: a repository interface with one
method per query shape does not scale past a handful of tables, and beyond that it
becomes a leaky abstraction that exposes the query language anyway.

**Expose the connection with no enforcement at all.** Rejected: then the crate's
guarantees are advisory, and a single `conn.execute("DELETE FROM device")` in
application code silently breaks replication.

## Related

- [../sqlite-interface.md](../sqlite-interface.md) — what the crate installs on the
  connection
- [ADR-0002](0002-enforce-in-sqlite-not-in-the-api.md) — engine-level enforcement
