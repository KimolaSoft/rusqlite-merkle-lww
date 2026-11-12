# The SQLite interface

How the crate drives SQLite, and why it does not wrap SQL.

## The central decision

The application needs five guarantees:

1. `last_update` is stamped on every write.
2. `_sync_log` is updated on every write.
3. Protected columns (`status`, `purge_on`) cannot be written by the application.
4. Hard `DELETE` on a managed table cannot happen outside the crate.
5. Internal tables (`_sync_*`) cannot be written by the application.

These are **engine-level** invariants, and SQLite can enforce all five itself:

| Guarantee | Mechanism |
|---|---|
| 1, 2 | `AFTER INSERT` / `AFTER UPDATE` / `AFTER DELETE` triggers |
| 3, 4, 5 | the authorizer callback (`sqlite3_set_authorizer`) |

The consequence is that **the application keeps writing plain SQL.** The crate owns the
*connection*, not the *query language*. There is no ORM, no query builder, no wrapper for
`SELECT` versus `INSERT` versus `UPDATE` versus `DELETE`.

This is the opposite of the obvious design, and it is deliberate. An ORM over SQLite is
permanently behind — SQLite has around thirty distinct feature families (JSON1, FTS5,
R-tree, window functions, recursive CTEs, generated columns, `RETURNING`, upsert, partial
and expression indexes, custom collations and functions, virtual tables, `STRICT`,
`WITHOUT ROWID`, deferrable constraints, and more), and applications escape the wrapper
the first time they need a feature it lacks. Worse, an API-level wrapper only protects
the code paths that go through it; raw SQL is one `conn.execute()` away from bypassing
every guarantee.

Engine-level enforcement has no such gap. See
[ADR-0002](adr/0002-enforce-in-sqlite-not-in-the-api.md) and
[ADR-0010](adr/0010-no-orm-query-wrapper.md).

## The interface

```rust
impl Db {
    /// Borrow a pooled connection for arbitrary SQL. Triggers and the authorizer are
    /// already installed. This is the primary interface.
    pub fn conn(&self) -> Result<PooledConnection>;

    /// Run a closure inside a transaction, on a pooled connection.
    pub fn transaction<T>(&self, f: impl FnOnce(&Transaction) -> Result<T>) -> Result<T>;

    /// Install the crate's UDFs and authorizer on a connection the **caller owns**.
    pub fn register_on(&self, conn: &rusqlite::Connection) -> Result<()>;

    /// Run a closure with crate privilege on a registered caller-owned connection.
    pub fn with_privilege<T>(&self, conn: &rusqlite::Connection,
                             f: impl FnOnce() -> Result<T>) -> Result<T>;

    // Managed convenience layer — sugar over the above.
    pub fn upsert(&self, table: &str, key: &Key, values: &[(&str, Value)]) -> Result<()>;
    pub fn delete(&self, table: &str, key: &Key) -> Result<()>;
    pub fn move_key(&self, table: &str, from: &Key, to: &Key) -> Result<()>;
}
```

The managed layer exists because it is convenient, and because `move_key` and soft
delete need crate privilege. It is **not** required for correctness. An application that
prefers `conn()` and raw SQL gets the same guarantees.

### Registering on a connection the caller owns

An application with an existing single-connection write path does not have to move every
write into the crate's pool. `register_on` installs the same UDFs and the same
authorizer on a connection the application opened:

```rust
let conn = rusqlite::Connection::open("app.sqlite")?;
db.register_on(&conn)?;                 // once, at startup
conn.execute("INSERT INTO users(…) VALUES (…)", params![…])?;
```

Two things make this work rather than merely appear to work:

- **The UDFs close over the node's shared timestamp issuer.** The monotonic issuance
  guard (§ "Monotonic issuance") is a per-node scalar, so a caller-owned connection must
  issue timestamps from the *same* issuer as the pooled ones. `register_on` installs
  closures over the shared `Arc<TimestampIssuer>`, and a timestamp issued on a
  caller-owned connection is strictly greater than the last one issued anywhere in the
  process.
- **The privilege flag is per connection, as it always was.** `register_on` gives the
  connection its own flag; the crate's own write path on that connection uses
  `with_privilege`, which sets the flag for the duration of the closure.

The caller is responsible for calling `register_on` on **every** connection it opens. A
connection that is not registered fails closed in the enforced dialect — the write is
refused with `no such function: sync_stamp` — which is the correct behaviour, but it is
a refusal, not a silent downgrade.

`rusqlite::Connection` is `Send` but not `Sync`, so a caller-owned connection used from
several threads needs external locking, exactly as it would without the crate.

### The privilege flag

The authorizer must distinguish the crate's own writes from the application's. The crate's
write path sets a per-connection privilege flag while it holds it, and the authorizer
consults that flag.

Because the flag is per-connection, the crate's write path must **acquire a connection
and set its flag on that connection** — a flag held on the `Db` struct would be wrong,
because another thread could be using a different connection from the same pool at the
same time.

## Triggers

For each managed table the crate installs triggers. The design has a subtlety worth
stating, because the obvious form does not work.

### `SELECT NEW.col = expr` does not assign

In SQLite, `SELECT NEW.last_update = stamp();` inside a trigger evaluates to a boolean
and discards it. `NEW.last_update` is unchanged. The two forms that do work:

- `UPDATE t SET col = expr WHERE rowid = NEW.rowid;` — a nested update inside the
  trigger. Works, but re-enters the trigger, so a `WHEN` guard is required to stop
  recursion.
- `DEFAULT (expr)` on the column — works for `INSERT` when the application omits the
  column, and lets an explicitly-supplied value win.

### The trigger set

```sql
-- last_update is stamped by the engine, but an explicit value is respected.
CREATE TABLE device (
    sync_id      TEXT NOT NULL,
    device_id    TEXT NOT NULL,          -- application column; may be a local sequence
    last_update  TEXT NOT NULL DEFAULT (sync_stamp()),
    status       TEXT NOT NULL DEFAULT 'live',
    purge_on     TEXT,
    payload      TEXT NOT NULL,
    PRIMARY KEY (sync_id)
);

-- INSERT: nothing to stamp, the DEFAULT fires. Log the new row.
CREATE TRIGGER sync_device_ai AFTER INSERT ON device BEGIN
    INSERT INTO _sync_log(tbl, k, last_update_us, purge_on_us, digest, deleted)
    VALUES ('device', NEW.sync_id,
            sync_us(NEW.last_update),
            sync_us(NEW.purge_on),
            sync_digest(NEW.sync_id, NEW.last_update,
                        NEW.status, NEW.purge_on, NEW.payload),
            NEW.status = 'deleted')
    ON CONFLICT(tbl, k) DO UPDATE SET
        last_update_us = excluded.last_update_us,
        purge_on_us    = excluded.purge_on_us,
        digest         = excluded.digest,
        deleted        = excluded.deleted;
END;

-- UPDATE: stamp only if the application did not supply a new last_update, then log.
-- The nested UPDATE re-enters this trigger, so the WHEN guard
-- (NEW.last_update IS OLD.last_update) is what stops the recursion.
CREATE TRIGGER sync_device_bu BEFORE UPDATE ON device
WHEN NEW.last_update IS OLD.last_update
BEGIN
    UPDATE device SET last_update = sync_stamp() WHERE rowid = NEW.rowid;
END;

CREATE TRIGGER sync_device_au AFTER UPDATE ON device BEGIN
    INSERT INTO _sync_log(tbl, k, last_update_us, purge_on_us, digest, deleted)
    VALUES ('device', NEW.sync_id,
            sync_us(NEW.last_update), sync_us(NEW.purge_on),
            sync_digest(...), NEW.status = 'deleted')
    ON CONFLICT(tbl, k) DO UPDATE SET ...;
END;

-- DELETE: the crate only ever hard-deletes during purge, so the log row goes too.
CREATE TRIGGER sync_device_ad AFTER DELETE ON device BEGIN
    DELETE FROM _sync_log WHERE tbl = 'device' AND k = OLD.sync_id;
END;
```

### The allocation triggers

Tables whose identifiers come from a per-site range get three more triggers. The registry
table holds the ranges and **must live in the same database** — a trigger cannot reference
an object in an attached database, so a separate `sites.db` is not an option.

```sql
-- The registry. The range is immutable; everything else is ordinary mutable data.
CREATE TABLE sites (
    site_id     TEXT NOT NULL,
    site_name   TEXT NOT NULL,
    uid_start   INTEGER NOT NULL,
    uid_end     INTEGER NOT NULL,
    city        TEXT,
    country     TEXT,
    last_update TEXT NOT NULL DEFAULT (sync_stamp()),
    status      TEXT NOT NULL DEFAULT 'live',
    purge_on    TEXT,
    PRIMARY KEY (site_id),
    CHECK (uid_start <= uid_end),
    UNIQUE (uid_start)
);

-- A range may not be edited. A site is renamed; its range is not moved.
CREATE TRIGGER sync_sites_bu_range BEFORE UPDATE ON sites
WHEN NEW.uid_start <> OLD.uid_start OR NEW.uid_end <> OLD.uid_end
BEGIN
    SELECT RAISE(ABORT, 'site range is immutable');
END;

-- Ranges may not overlap, or two sites could mint the same identifier.
CREATE TRIGGER sync_sites_bi_range BEFORE INSERT ON sites
WHEN EXISTS (
    SELECT 1 FROM sites s
    WHERE s.uid_start <= NEW.uid_end AND NEW.uid_start <= s.uid_end
)
BEGIN
    SELECT RAISE(ABORT, 'site ranges overlap');
END;

-- A row's identifier must fall in the range of the site that owns it.
-- The condition fires only when the registry KNOWS the owner: a site created
-- mid-session may have its rows arrive before the registry row does, and
-- rejecting them would deadlock the conversation.
CREATE TRIGGER sync_users_bi_range BEFORE INSERT ON users
WHEN EXISTS (
    SELECT 1 FROM sites s
    WHERE s.site_id = NEW.site
      AND NOT (NEW.userid BETWEEN s.uid_start AND s.uid_end)
)
BEGIN
    SELECT RAISE(ABORT, 'userid outside own site range');
END;
```

The `site` column itself is defaulted by the engine:

```sql
site TEXT NOT NULL DEFAULT (local_site())
```

Verified: a scalar function in a column `DEFAULT` is evaluated per insert, an explicit
value overrides it, and a connection where the function is not registered fails closed.
`ALTER TABLE … ADD COLUMN` rejects a non-constant default, so this form needs
`CREATE TABLE`; for a table that already exists the crate offers an `AFTER INSERT`
trigger that fills a NULL `site` — which does not fire on the `ON CONFLICT DO UPDATE`
path, correctly, since that path leaves an existing row's `site` alone.

The `sync_users_bi_range` trigger fires on the `ON CONFLICT DO UPDATE` path too, so the
upsert is covered by the same rule.

### The UDFs

| Function | Signature | Returns |
|---|---|---|
| `sync_stamp()` | none | The node's next issued timestamp, as RFC 3339 text |
| `sync_us(text)` | RFC 3339 text, or NULL | The same instant as integer microseconds; NULL in, NULL out |
| `sync_digest(...)` | the row's key, timestamp, status, purge_on and replicated values | The 16-byte digest |
| `local_site()` | none | The local node's `site_id` — only on tables with allocated identifiers |

`sync_stamp()` is the monotonic-issuance mechanism (see below). `sync_us()` avoids
parsing the timestamp back out of text inside SQLite, and returns NULL for a NULL input
— which is what makes `purge_on_us` NULL for a live row. `sync_digest()` takes the row's
values as arguments because it must hash exactly the replicated column set, in the
registered order. `local_site()` is what populates the `site` column, so the application
never writes it.

### Behaviour

| Action | Result |
|---|---|
| `INSERT` without `last_update` | Stamped, logged |
| `INSERT` with an explicit `last_update` | **The application's value wins** |
| `UPDATE` of another column | `last_update` advances, row re-logged |
| `UPDATE` with an explicit `last_update` | Respected |
| `DELETE` (crate-privileged only) | Log row removed with it |

The second row is what makes backfill and import possible: an application that knows the
historical timestamp of a row can write it directly, and the crate will not overwrite it.

### The `status`/`purge_on` problem

`AFTER UPDATE` cannot distinguish an application's write from the crate's soft delete,
because both are `UPDATE`s. The authorizer solves it: `status` and `purge_on` writes are
denied unless the connection's privilege flag is set.

The crate's soft delete sets **both** columns in one statement, and `purge_on` is the
engine's value — computed from the delete's window, so every tombstone in a window
shares one deadline. The application never supplies it. See
[lifecycle.md](lifecycle.md) § "The purge instant".

### Recursion

The nested `UPDATE` inside `sync_device_bu` re-enters that trigger. The `WHEN
NEW.last_update IS OLD.last_update` guard stops it: on the nested update the timestamp
has already changed, so the guard is false. Verified: one row, one stamp, one log entry —
identical with `recursive_triggers` both on and off.

## The authorizer

`sqlite3_set_authorizer` (rusqlite: `Connection::authorizer`, a closure over
`AuthContext`) is invoked for every operation and returns `Allow`, `Ignore`, or `Deny`.
`Deny` raises `SQLITE_AUTH` and the statement fails with "not authorized".

### Rules

| Action | Denied to the application | Reason |
|---|---|---|
| `INSERT`/`UPDATE` on `_sync_*` | yes | internal tables |
| `UPDATE` of `status` or `purge_on` on a managed table | yes | soft delete must go through the crate |
| `DELETE` on a managed table | yes | a hard delete loses the tombstone |
| `CREATE`/`DROP` on a managed table | yes | DDL goes through the crate |
| everything else | no | raw SQL is allowed |

### Trigger-internal writes need an exemption

The authorizer **does** see writes made by triggers, and denying one aborts the enclosing
statement. The `AuthContext` carries an `accessor` field: "the inner-most trigger or
view responsible for the access attempt", `None` for top-level SQL.

```rust
if let Some(accessor) = ctx.accessor {
    if accessor.starts_with("sync_") { return Authorization::Allow; }  // our own trigger
}
```

This exemption is load-bearing. Without it, either every trigger fails or the internal
tables are writable by the application.

### Per-connection installation

The authorizer is per-connection. It must be installed on **every** pooled connection,
and on any connection the application opens itself. A pooled connection that was checked
out before installation, or created by a caller rather than the pool, would be
unprotected.

## Two dialects

The triggers above call Rust UDFs. That has a sharp consequence:

> A trigger that calls a Rust UDF **fails on any connection where the UDF is not
> registered**, with `no such function: sync_stamp`. The write is rejected.

So a UDF-based trigger is a **fail-closed** guarantee: an application using its own raw
`rusqlite::Connection` cannot corrupt the sync state, because its writes simply fail. It
cannot, however, write at all — which is unacceptable for an application that wants its
own connection.

The alternative is a **pure-SQL trigger** that stages the change using only SQL, drained
by a background task.

| | Enforced (UDF) | Compatible (pure SQL) |
|---|---|---|
| Works on a foreign connection | **no** (fails closed) | **yes** |
| Latency | immediate | up to the drain interval |
| Digest computed in | the write transaction | the drain task |
| Extra machinery | none | staging table + drainer |
| Timestamp resolution | microseconds | milliseconds |
| A row written and deleted within one drain interval | captured | **missed** |
| Authorizer rules enforced on foreign connections | n/a (writes fail anyway) | **no** — see below |

The last row is a real difference, and it is the substance of the dialect choice. See
"Compatible mode and foreign connections" below.

### Choosing

The dialect is a deployment decision, configured per node.

- **Enforced** — recommended. Correctness by construction; a misconfigured application
  fails loudly rather than diverging silently.
- **Compatible** — for applications that must use their own `rusqlite::Connection` and
  cannot be changed. Costs drain latency, and the digest is not computed in the write
  transaction.

A middle ground exists: the crate provides a `rusqlite::Connection` factory that
registers the UDFs, so an application that *wants* its own connection can have one and
still get the strict guarantees. This is the recommended way to use enforced mode, and it
is the answer for an application that uses `rusqlite` but manages its own pool.

The practical question is not "does the application use `rusqlite`?" — it is "does the
application open connections the crate did not instrument?" An application using
`rusqlite` but taking connections from `Db::conn()` is fine under enforced mode. An
application using `diesel`, `sqlx`, or the `sqlite3` CLI is not, because those open
their own connections and cannot register the crate's UDFs.

### Compatible mode and foreign connections

In compatible mode the triggers are pure SQL, so they fire on **any** connection. But the
**authorizer is per-connection**, so a foreign connection has none of the denial rules.
This was verified, and the results are:

| Action from a foreign connection | Enforced | Compatible |
|---|---|---|
| `INSERT` / `UPDATE` a managed table | **fails** (no such function) | **allowed**, staged |
| `DELETE` a live row | **fails** | **allowed** — but converted to a soft delete (§ below) |
| `DELETE` a tombstone | **fails** | **blocked** by the delete guard |
| Write to `_sync_log` | **fails** | **blocked** by the log guard |
| Write `status` or `purge_on` | **fails** | **allowed** — a known gap |

Two of these gaps are closed with **data-level guards**, which are pure SQL and therefore
apply to every connection:

**The delete guard.** A `BEFORE DELETE` trigger converts a hard delete into a soft
delete, and aborts the statement:

```sql
CREATE TRIGGER sync_device_bd BEFORE DELETE ON device
WHEN NOT EXISTS (SELECT 1 FROM _sync_purge_armed WHERE tbl = 'device')
BEGIN
    UPDATE device SET status = 'deleted',
        purge_on = strftime('%Y-%m-%dT%H:%M:%f','now') || '000Z'
        WHERE rowid = OLD.rowid;
    SELECT RAISE(IGNORE);          -- skip the physical delete
END;
```

The `_sync_purge_armed` marker table lives in the **main** database, not `temp` —
triggers cannot reference `temp` objects, which was verified (`triggers cannot reference
objects in database temp`). The crate sets the marker immediately around its own purge
statement, so the legitimate physical delete still works.

**The log guard.** The same technique protects the internal tables:

```sql
CREATE TRIGGER sync_log_guard BEFORE INSERT ON _sync_log
WHEN NOT EXISTS (SELECT 1 FROM _sync_drain_armed)
BEGIN
    SELECT RAISE(ABORT, 'internal table');
END;
```

Both guards are strictly stronger than the authorizer for these two cases, because they
hold regardless of which connection issued the statement. The authorizer remains the
primary mechanism, because it covers a wider set of actions.

**The remaining gap.** A foreign connection in compatible mode can write `status` or
`purge_on` directly, because the authorizer is not present to deny it. Such a write is
caught by the AFTER UPDATE trigger and staged, so the change *propagates* — but the
application has bypassed the crate's soft-delete path. The **drainer normalises** the
value: when it stages a tombstone whose `purge_on` is not one of the schedule's
instants, it rewrites it to the deadline its window implies, so
the uniform-per-window rule holds even in compatible mode. The remaining gap is
therefore only the *timing* of the delete — the tombstone is created when the drainer
runs rather than when the application wrote it — and not its deadline.

This is acceptable because it requires the application to deliberately write a
crate-managed column, which is already documented as forbidden, and the consequence is a
possibly-late tombstone rather than silent divergence. It is **not** acceptable
in enforced mode, which is one more reason enforced mode is the default.

A foreign connection can also forge the guard markers, since it can write any table.
That is a deliberate trade-off: the markers are an integrity mechanism against accidents,
not an authentication mechanism. A hostile application with filesystem access to the
database can corrupt it regardless — the database file is not a trust boundary against
someone who can open it.


## Monotonic issuance

Each node persists one scalar:

```
_sync_meta.last_issued_us : i64
```

and issues `last_update = max(wall_clock_now, last_issued_us + 1µs)`, storing the new
value. This is one integer per node, not per row.

It exists because timestamps from one node must never go backwards. Without it, a clock
step — an NTP correction, a VM suspend — could make a node write a `last_update` older
than the row's current value, and LWW would then silently discard the newer edit. The
edit is not lost in storage; it is lost in *ordering*, which is worse, because it is
invisible.

With the guard:

- timestamps from a node are strictly increasing;
- two writes by the same node never share a timestamp;
- the only remaining collisions are cross-node, handled by the digest tie-break.

In compatible mode the trigger uses the staging dialect and issues timestamps at drain
time rather than write time; the same guard applies, but the recorded instant is the
drain instant rather than the write instant.

## DDL management

The crate owns DDL for managed tables, because it owns their triggers, their managed
columns, and their indexes. The application owns DDL for everything else.

```rust
db.create_table(&spec)?;        // creates with managed columns and triggers
db.alter_table(&spec)?;         // adds missing managed columns
db.adopt_table(&spec)?;         // registers an existing table, adding managed columns
db.drop_table("device")?;
```

Every DDL path must also reinstall the triggers and rebuild the tree, since a dropped or
recreated table invalidates both.

### SQLite constraints

- `ALTER TABLE ... ADD COLUMN` works, but a new `NOT NULL` column needs a default.
  Managed columns are added with defaults (`status` → `'live'`, `last_update` → the
  current timestamp) and backfilled by the next reconcile pass.
- A primary key **cannot** be added to an existing table. A strategy change requires a
  table rebuild (create → copy → drop → rename) inside a transaction, which rewrites the
  table and invalidates any `Rowid` strategy. The crate **refuses** rather than silently
  rebuilding, unless `allow_rebuild = true`.
- `RENAME COLUMN` (SQLite ≥ 3.25) is safe and maps onto the spec.
- A `DROP TABLE` on a managed table is denied by the authorizer, so it must go through
  `drop_table`, which cleans up the triggers and the log.

## Reads and the cache

Reads are ordinary SQL on any connection and need no special support.

The read-through cache is an **opt-in accelerator**, and it can only help the access
pattern it is built for: point lookups by primary key. Arbitrary `SELECT`s — joins,
aggregates, `ORDER BY` over many rows — cannot be served from a key-value cache without
reimplementing the query planner, so they always hit SQLite.

The cache's honest scope is therefore: *"the row I just looked up by primary key."* The
crate exposes a cached `get(table, key)` alongside the raw connection. Anything more
would be a lie.

## Verified SQLite behaviour

Every claim above was checked against real SQLite (rusqlite 0.41.1, bundled) rather than
assumed. These results are the basis for the design.

| # | Question | Result |
|---|---|---|
| 1 | Does the authorizer see writes made *by a trigger*? | **Yes.** Denying one aborts the enclosing statement. |
| 2 | Can trigger-internal writes be exempted? | **Yes.** `AuthContext.accessor` names the inner-most trigger; `None` for top-level SQL. |
| 3 | Can the authorizer deny a write to one *column*? | **Yes.** `AuthAction::Update { table_name, column_name }`; `column_name` is populated for `UPDATE`. |
| 4 | Does `SELECT NEW.col = expr` assign inside a trigger? | **No.** It evaluates to a boolean and discards it. |
| 5 | Does `DEFAULT (udf())` work, and does an explicit value still win? | **Yes / yes.** |
| 6 | Can a nested `UPDATE` inside a trigger stamp a column without infinite recursion? | **Yes**, with a `WHEN NEW.last_update IS OLD.last_update` guard. Identical behaviour with `recursive_triggers` ON and OFF. |
| 7 | Does a trigger calling a Rust UDF work on a *foreign* connection? | **No.** `no such function: sync_stamp` — the write is rejected. Fail-closed. |
| 8 | Does a pure-SQL trigger work on a foreign connection? | **Yes.** |
| 9 | Does `unixepoch('now','subsec')` give sub-second precision? | Milliseconds only. **Not** microseconds. A pure-SQL trigger must pad (`strftime(...) \|\| '000' \|\| 'Z'`), yielding a valid fixed-width timestamp at ms resolution. |
| 10 | Is `sha3()` available in bundled SQLite? | **No.** It cannot stand in for the digest in a pure-SQL trigger; the digest must be computed in Rust, or the row staged for the drainer. |
| 11 | Do the two dialects produce *comparable* timestamps? | **Yes.** The UDF path yields `…302238Z` and the padded pure-SQL path yields `…302000Z` — both exactly 27 characters, so they stay fixed-width and lexicographically comparable. Only the resolution differs. |
| 12 | Does the authorizer apply to a foreign connection? | **No.** The authorizer is per-connection, so a foreign connection has none of the denial rules. Verified: a foreign connection can write `_sync_log` and hard-delete managed rows under the compatible dialect. |
| 13 | Can a trigger's `WHEN` clause reference a `temp` table? | **No.** `triggers cannot reference objects in database temp`. A guard marker must live in the main database. |
| 14 | Can a `BEFORE DELETE` trigger convert a hard delete into a soft delete? | **Yes.** `UPDATE` the row to `status='deleted'`, then `SELECT RAISE(IGNORE)` to skip the physical delete. The statement succeeds; the row survives as a tombstone. |
| 15 | Can the same trigger let the crate's legitimate purge through? | **Yes.** Gate the `WHEN` clause on a main-database marker table that the crate sets around its own purge statement. Verified: an app hard-delete is converted to a soft delete, a second app hard-delete of the tombstone is blocked, and the crate's purge removes the row. |
| 16 | Can a trigger read a table in an `ATTACH`ed database? | **No.** `trigger users_range cannot reference objects in database reg`. The site registry must live in the main database. |
| 17 | Can a scalar function be used in a column `DEFAULT`? | **Yes.** Evaluated per insert; an explicit value overrides it; a connection without the function fails closed (`no such function: local_site`). |
| 18 | Can `ALTER TABLE … ADD COLUMN` use a non-constant default? | **No.** Rejected by SQLite. The `DEFAULT (fn())` form needs `CREATE TABLE`; an existing table uses an `AFTER INSERT` trigger instead. |
| 19 | Does an `AFTER INSERT` trigger fire on the `ON CONFLICT DO UPDATE` path? | **No.** Correct — that path leaves an existing row's value alone. |
| 20 | Can a range-enforcement trigger fire only when the owner is known? | **Yes.** `WHEN EXISTS (SELECT 1 FROM sites WHERE site_id = NEW.site AND NOT (… BETWEEN …))` — an unknown owner does not fire, so a site created mid-session does not deadlock the conversation. |
| 21 | Can a registry trigger reject an overlapping range, and a second one freeze a range? | **Yes / yes.** `BEFORE INSERT` rejects overlap; `BEFORE UPDATE` rejects a range change while allowing descriptive updates. |

Consequences that follow directly:

- The **authorizer is the enforcement primitive**, not a convenience. It is the only
  thing standing between the application and the internal tables — on the connections it
  is installed on.
- The **`accessor` exemption is load-bearing** (row 2).
- **Enforced mode cannot support a foreign connection** (row 7). This is a feature
  (fail-closed) and a hard deployment constraint, which is why the staging dialect
  exists (row 8).
- **The pure-SQL dialect cannot compute the digest** (rows 9–10), so compatible mode
  *must* stage rows for a Rust drainer. It cannot be made synchronous.
- The two dialects stay **wire-compatible** (row 11), so a deployment can mix enforced and
  compatible nodes without a timestamp-format fork.
- **The authorizer is per-connection** (row 12), so compatible mode needs the data-level
  guards to hold on foreign connections. The guards are pure SQL and therefore apply
  everywhere.
- **Guard markers must live in the main database** (row 13), not `temp`.
- The **delete guard** (rows 14–15) makes a hard delete impossible from any connection in
  compatible mode, which is the strongest form of the invariant.

## Related documents

- [data-model.md](data-model.md) — what is stored
- [merkle.md](merkle.md) — how the digest is used
- [lifecycle.md](lifecycle.md) — deletes and purge, which need crate privilege
- [security.md](security.md) — the authorizer as part of the trust model
- [adr/0002](adr/0002-enforce-in-sqlite-not-in-the-api.md) ·
  [adr/0010](adr/0010-no-orm-query-wrapper.md)
