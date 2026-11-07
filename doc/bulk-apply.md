# Bulk row application

How the crate applies many rows at once, and the SQLite limits that shape it.

## The problem

A sync of 50,000 rows must not turn into 50,000 individual statements, and it must not
exceed SQLite's limits. The limit that bites is the **bound parameter count**.

Verified against rusqlite 0.41.1 with bundled SQLite 3.54.1:

| Limit | Value | Notes |
|---|---|---|
| `SQLITE_LIMIT_VARIABLE_NUMBER` | **32,766** | The historical 999 was raised in SQLite 3.32.0 (2020). |
| `SQLITE_LIMIT_COMPOUND_SELECT` | 500 | Applies to `UNION`/`UNION ALL` chains, not to `VALUES` lists. |
| `SQLITE_LIMIT_SQL_LENGTH` | 1,000,000,000 | Not a practical constraint. |

The variable limit **cannot be raised at runtime** beyond the compiled-in maximum:
`set_limit(SQLITE_LIMIT_VARIABLE_NUMBER, 250_000)` returns `Ok` but leaves the value at
32,766. It is a compile-time ceiling.

The failure looks like this:

```
too many SQL variables
```

and it happens exactly when the application is busiest — a bulk sync, a re-seed, a large
initial load.

## Three bulk paths

All three were measured with 50,000 rows against an in-memory database.

### Path 1 — multi-row `VALUES` with bound parameters

```sql
INSERT INTO device(sync_id, payload, last_update, status, purge_on)
VALUES (?,?,?,?,?), (?,?,?,?,?), …
ON CONFLICT(sync_id) DO UPDATE SET
    payload     = excluded.payload,
    last_update = excluded.last_update,
    status      = excluded.status,
    purge_on    = excluded.purge_on
WHERE excluded.last_update > device.last_update;
```

**Chunk size must satisfy `rows × columns ≤ 32,766`.** With six columns that is 5,461
rows per statement. The crate computes the chunk size from the column count rather than
hardcoding a row count, so a wide table chunks smaller automatically.

Measured: **50,000 rows in ~95 ms.**

This is the default path. It is fast, transactional, and needs no extensions.

### Path 2 — JSON bulk insert

```sql
INSERT INTO device(sync_id, payload, last_update, status, purge_on)
SELECT json_extract(j.value, '$.sync_id'), json_extract(j.value, '$.payload'), …
FROM json_each(?1) AS j
WHERE true
ON CONFLICT(sync_id) DO UPDATE SET …;
```

**One bound parameter carries the whole batch.** The parameter count becomes 1 regardless
of batch size, which removes the chunking arithmetic entirely.

Two details that are easy to get wrong:

- The alias is required: `FROM json_each(?1) AS j`, and the expressions reference
  `j.value`. Without the alias, SQLite reports `no such column: j.value`.
- `WHERE true` is required before `ON CONFLICT`. SQLite parses `ON CONFLICT` as a join
  constraint otherwise, and the statement fails with a syntax error near "DO".

Measured: **50,000 rows in ~108 ms.**

This path is marginally slower than chunked `VALUES` but has one parameter instead of
thousands, and it scales to a batch of any size in one statement. It is worth using when
the batch is large or the column count is high.

### Path 3 — staging table

```sql
CREATE TEMP TABLE _sync_stage (…same shape as the target…);
-- bulk insert into the temp table, then:
INSERT INTO device SELECT * FROM _sync_stage WHERE true
ON CONFLICT(sync_id) DO UPDATE SET …;
```

The most flexible — the staging table can be populated by whatever means is cheapest
(path 1 or 2), and the merge is a single set-based statement. It costs an extra copy.

Worth it when a batch mixes inserts and updates in ways the other paths handle awkwardly,
or when the source data is already shaped like a table.

## Choosing

| Situation | Path |
|---|---|
| Default | 1 — chunked multi-row `VALUES` |
| Very wide tables, or batches that would need many chunks | 2 — JSON bulk insert |
| The source is already table-shaped, or the merge needs the full set | 3 — staging table |

All three produce the same result. The choice is a performance and ergonomics decision,
not a correctness one.

## LWW in the upsert itself

The upsert carries the last-write-wins filter directly:

```sql
ON CONFLICT(sync_id) DO UPDATE SET
    payload     = excluded.payload,
    last_update = excluded.last_update,
    status      = excluded.status,
    purge_on    = excluded.purge_on
WHERE excluded.last_update > device.last_update;
```

The `WHERE` clause on `DO UPDATE` means an older incoming row is silently dropped rather
than overwriting a newer one. This is the primary defence against a peer sending stale
data, and it is enforced by the database rather than by the calling code.

Verified behaviour:

- An older write arriving after a newer one → the newer value is kept.
- An equal timestamp → the `WHERE` is false, so the existing row is kept. The digest
  tie-break resolves this case; see [merkle.md](merkle.md).

The tie-break means the digest must be part of the conflict resolution, not just the
timestamp. Two options:

1. Add the digest to the `WHERE` clause: `WHERE excluded.last_update > device.last_update
   OR (excluded.last_update = device.last_update AND excluded.digest > device.digest)`.
   This requires storing the digest in the target table.
2. Filter in Rust before the upsert: read the current row, compare `(last_update, digest)`
   in application code, and only include rows that should win.

**Option 2 is the default.** It keeps the digest out of the user's table, and the
comparison logic lives in one place — the same function that implements the merge — rather
than being duplicated as SQL. Option 1 is available for callers who want the filter
enforced in the database and are willing to add a digest column.

The cost of option 2 is a read per batch rather than per row: the existing rows for the
batch's keys are fetched in one indexed query, compared in memory, and only the winners
are upserted.

## Transactions and batching

A bulk apply is one transaction. The chunking happens **within** it, not across
transactions, so the batch is atomic:

```rust
db.transaction(|tx| {
    for chunk in rows.chunks(chunk_size) {
        tx.bulk_upsert(table, chunk)?;
    }
    Ok(())
})?;
```

If the whole batch fails, nothing is applied. If the batch is interrupted by a crash, the
transaction rolls back and the sync resumes from the unchanged cursor.

A transaction that writes 50,000 rows holds 50,000 row changes in the WAL before it
commits. That is fine — it is disk, not memory — but it does mean a long-running batch
delays other writers in WAL mode only for the duration of the commit, not the duration of
the transaction. WAL readers are unaffected throughout.

## The `_sync_log` write

Each applied row also updates `_sync_log`. That is a second statement per chunk, with the
same chunking rules. The two are written in the same transaction, so the log and the user
table cannot diverge.

This doubles the write volume, which is the main cost of the bulk path. It is worth it:
the log is what makes the next sync cheap, and what makes a re-seed possible.

## Row counts and the protocol

The wire protocol's ROWS phase (§ [protocol.md](protocol.md)) already sends rows grouped
by bucket. The receiver's batch size is bounded by the bucket's row count, and a busy
hour could be large. So the receiving side applies a bucket's rows in chunks, and reports
progress as it goes — which is what the `Transferring` phase's denominator counts.

## Verified behaviour

| # | Question | Result |
|---|---|---|
| 1 | What is the bound-parameter limit in bundled SQLite? | **32,766.** The historical 999 was raised in 3.32.0. |
| 2 | Can it be raised at runtime? | **No.** `set_limit` returns `Ok` but the value stays at 32,766 — it is a compile-time ceiling. |
| 3 | Is there a row-count limit on a multi-row `VALUES` clause? | **No.** 5,000 literal rows in one statement works. `SQLITE_LIMIT_COMPOUND_SELECT` (500) applies to `UNION` chains, not `VALUES`. |
| 4 | Does a multi-row `VALUES` upsert work? | **Yes**, including `ON CONFLICT … DO UPDATE`, and it is re-runnable without duplicating rows. |
| 5 | Does the JSON bulk path work? | **Yes**, with the `AS j` alias and `WHERE true` before `ON CONFLICT`. Both are required. |
| 6 | How fast is a 50,000-row apply? | ~95 ms (chunked `VALUES`) and ~108 ms (JSON), in memory. |
| 7 | Can the LWW filter be expressed in the upsert? | **Yes** — `ON CONFLICT … DO UPDATE … WHERE excluded.last_update > device.last_update` keeps the newer row and silently drops the older one. |
| 8 | Is `json1` available in bundled SQLite? | **Yes.** `json_each` and `json_extract` are both present. |

## Related documents

- [protocol.md](protocol.md) — the ROWS phase that produces these batches
- [merkle.md](merkle.md) — the digest tie-break referenced above
- [sqlite-interface.md](sqlite-interface.md) — the trigger and authorizer layer
- [../PLAN.md](../PLAN.md) — where bulk apply is implemented
