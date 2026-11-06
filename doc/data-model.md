# Data model

## Managed tables

A **managed table** is a table the crate syncs. For it, the crate owns four columns, the
triggers, the indexes it needs, and the DDL.

```sql
CREATE TABLE device (
    sync_ns      TEXT NOT NULL,          -- crate-managed: namespace
    id           TEXT NOT NULL,          -- application key column
    last_update  TEXT NOT NULL,          -- crate-managed: RFC 3339
    status       TEXT NOT NULL,          -- crate-managed: 'live' | 'deleted'
    purge_on     TEXT,                   -- crate-managed: RFC 3339, NULL when live
    payload      TEXT NOT NULL,          -- application column
    PRIMARY KEY (sync_ns, id)
);
```

The managed column names are configurable per table; these are the defaults.

| Column | Type | Meaning |
|---|---|---|
| `sync_ns` | TEXT | The namespace the row belongs to (§ "Key strategies") |
| `last_update` | TEXT | RFC 3339 instant of the last modification. **The ordering key.** |
| `status` | TEXT | `'live'` or `'deleted'` |
| `purge_on` | TEXT | When the row may be physically removed. NULL while live. |

`last_update` and `purge_on` are **replicated**. `status` is replicated. `sync_ns` is part
of the key.

`last_update` is written by the engine, not the application — but an application that
supplies its own value has it respected, which is what makes backfill and import
possible. See [sqlite-interface.md](sqlite-interface.md).

### Timestamps

RFC 3339, UTC, `Z` suffix, exactly six fractional digits:

```
2026-11-24T17:24:03.302238Z
```

Fixed width means **lexicographic order equals chronological order**, so `ORDER BY
last_update` is chronological and `WHERE last_update >= ?` is an index range scan. No date
functions are needed anywhere in the hot path.

Timestamps are generated in Rust, never with SQLite's `strftime`. `strftime` yields
millisecond precision and three fractional digits, which silently breaks the fixed-width
property when mixed with Rust-generated values.

## Key strategies

The application brings its own primary key. Four strategies, selected per table.

```rust
pub enum KeyStrategy {
    /// Owner-namespaced: key = (site_column, user_key_columns…).
    SiteNamespaced { site_column: String, key_columns: Vec<String> },

    /// The application already has a globally unique column (UUIDv7, ULID, Snowflake).
    GlobalId { key_columns: Vec<String> },

    /// The crate adds and maintains its own surrogate key column.
    Surrogate { column: String },

    /// SQLite's implicit rowid. Zero schema impact, but VACUUM can renumber.
    Rowid,
}
```

| Strategy | Coordination needed | Cross-site same entity | Notes |
|---|---|---|---|
| `GlobalId` | globally unique IDs | **merges into one row** | `site` becomes an ordinary mutable column |
| `SiteNamespaced` | none | does **not** merge | Each site owns its own namespace. A row created at `eu-west` stays under `eu-west` forever. |
| `Surrogate` | none | does not merge | The crate guarantees key uniqueness; the application's PK stays its own business |
| `Rowid` | none | does not merge | Only when a table has no natural key |

### Choosing: shared dataset versus independent sites

The decision hinges on one question:

> **When two sites create what should be the same logical entity — the same user, the same
> order — do they become one row or two?**

| | `GlobalId` | `SiteNamespaced` |
|---|---|---|
| Rows created | one row, if both sites derive the same id | two rows, both survive |
| After a sync | one row; LWW picks the winner | two rows, both present |
| Merge conflict | resolved by `last_update` | none |
| Coordination needed | a globally unique id scheme | none |

**If the sites share one dataset** — the same user is the same user everywhere, and the
sites are peers over a single logical dataset — then `GlobalId` is correct. Two sites
updating the same user converge on one row, and LWW resolves which write wins. This is the
expected deployment.

**If the sites own independent data** — multi-tenant, or separately operated — then
`SiteNamespaced` is correct, and `GlobalId` would be actively harmful: it would merge two
customers' records into one row and let LWW silently discard one of them.

The recommendation is **`GlobalId` as the default**, with `SiteNamespaced` available for
tables that are genuinely per-site. The failure modes are asymmetric: being wrong with
`GlobalId` loses data irrecoverably, while being wrong with `SiteNamespaced` leaves two
rows that can still be reconciled deliberately. That asymmetry argues for `SiteNamespaced`
as the *safe* default — but the common case here is a shared dataset, so `GlobalId` is the
better default and `SiteNamespaced` is the deliberate opt-in.

### Getting the id right

`GlobalId` only merges if **both sites derive the same id** for the same entity. Two
mechanisms:

| Mechanism | Merges? | Notes |
|---|---|---|
| UUIDv7 / ULID / Snowflake generated locally | **no** | Each site generates a *different* id, so the entities stay separate. Useful only when the id is already globally assigned by something upstream. |
| UUIDv5 derived from a natural key | **yes** | `uuidv5(namespace, "user:alice")` is deterministic, so both sites produce the same id. This is the mechanism for "the same user is the same user". |
| An id from an existing system of record | **yes** | The id already has global meaning. |

So "the same user is the same user" requires a **deterministic** id — derived from a
natural key, or assigned by something that already agrees across sites. A randomly
generated globally-unique id does not merge anything; it merely avoids collisions.

This is worth being explicit about, because it is the single most common way a
`GlobalId` deployment fails to behave as intended.

### Keys are immutable

Changing a key is a **move**, not an update. `Db::move_key(table, from, to)` tombstones
the old key and inserts the new one atomically.

Every other column is freely mutable, including a `username` or an owner field. Only the
key columns are special. Under `SiteNamespaced` the namespace column is part of the key
and is therefore also immutable.

### The sync key encoding

The key is encoded canonically (§ "Canonical encoding") into a BLOB, and that BLOB is the
primary key of `_sync_log`. The encoding includes the namespace, so `(eu-west, abc)` and
`(tokyo, abc)` are distinct rows in the log.

## Visibility

```rust
pub enum Visibility {
    /// Sent to any peer, including read-only clients.
    Public,
    /// Stored and synced between trusted peers only. Never transmitted to a client.
    Restricted,
}

pub struct ColumnSpec {
    pub name: String,
    pub replicated: bool,       // participates in sync and in the digest at all
    pub visibility: Visibility,
}
```

| Peer class | Public columns | Restricted columns | May write |
|---|---|---|---|
| **Trusted** (master, child) | yes | yes | yes |
| **Client** (read-only) | yes | **no** | no |

Tables can also be marked `client_visible = false`, which excludes them from client
sessions entirely — their existence and shape are not advertised.

Enforcement points are listed in [security.md](security.md).

## Internal tables

```sql
CREATE TABLE _sync_meta (
    key   TEXT PRIMARY KEY,
    value BLOB NOT NULL
);
```

Keys: `node_id`, `site`, `epoch_us`, `schema_version`, `last_purge_instant`,
`retention_us`, `digest_seed`, `last_issued_us`.

```sql
CREATE TABLE _sync_log (
    tbl          TEXT    NOT NULL,
    k            BLOB    NOT NULL,   -- canonical encoded key
    hlc_us       INTEGER NOT NULL,   -- unix micros; == the row's last_update
    digest       BLOB    NOT NULL,   -- 16 bytes
    deleted      INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (tbl, k)
) WITHOUT ROWID;

CREATE INDEX _sync_log_hlc ON _sync_log (tbl, hlc_us);
```

`_sync_log` is a **per-key register**: writing a key overwrites its row. Old versions are
not retained. That is what makes it a per-key LWW register rather than a set of
operations, and it is why the log does not grow without bound.

`hlc_us` is the same instant as the row's `last_update` text, stored as an integer for
range scans. The two cannot disagree, because the trigger writes both from one value.

```sql
CREATE TABLE _sync_cursors (
    peer_id   TEXT PRIMARY KEY,
    as_of_us  INTEGER NOT NULL,   -- watermark: peer has all changes <= this
    synced_us INTEGER NOT NULL,
    progress  BLOB                -- last known SyncProgress for this peer
);
```

```sql
-- Compatible dialect only (§ sqlite-interface.md)
CREATE TABLE _sync_staging (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    tbl       TEXT NOT NULL,
    op        TEXT NOT NULL,
    payload   BLOB NOT NULL
);
```

## The digest

```rust
digest  = seeded_xxh3(seed,
                      key_enc || last_update || status || purge_on
                      || concat over replicated columns of value_enc)

row_hash = xxh3(key_enc || last_update || status || purge_on || digest)
```

Four jobs, one value:

1. **Bucket hashing** — `digest` is the leaf input to the tree.
2. **Tie-break** — on an exact microsecond collision in `last_update`, the digest breaks
   the tie deterministically (see [merkle.md](merkle.md)).
3. **Divergence detection** — two peers holding different bytes under the same timestamp
   is detectable, because the digest differs.
4. **Role-independent tree parity** — a client that receives a `digest` it cannot
   recompute still computes the *same* `row_hash`, because `row_hash` is a function of the
   digest rather than of the payload. This is what lets a read-only client participate in
   tree diffing without ever holding a restricted value.

### The digest is keyed

Restricted columns may hold low-entropy values — a boolean, a status enum. An unkeyed
digest would let a client confirm a guess: submit a candidate value, observe the
resulting bucket hash change. So the digest uses **seeded xxh3** with a deployment
secret shared by trusted peers and absent from clients.

Consequence: the digest seed is deployment-wide state that must be identical across
trusted peers. A mismatch is fatal at handshake.

### Divergence detection

The digest stored in `_sync_log` can be compared against a digest recomputed from the
user table. A mismatch means the row was modified without `last_update` advancing —
almost always a raw SQL write that bypassed the trigger.

The crate's `verify` command reports such rows. It does **not** repair them: an edit that
did not advance the timestamp cannot be ordered correctly, because the information needed
to order it was never recorded. Repairing it would mean guessing.

## Canonical encoding

Determinism matters for hashing and for the wire format. Two peers must derive identical
bytes from identical logical rows.

```
key_enc   := concat over key columns of       tag(u8) || len(u32 LE) || bytes
value_enc := concat over replicated value columns of tag(u8) || len(u32 LE) || bytes

tag: 0 = NULL
     1 = INTEGER (i64, little-endian)
     2 = REAL    (f64, raw bit pattern)
     3 = TEXT    (UTF-8)
     4 = BLOB    (raw bytes)
```

Column order is fixed by the registered spec, so both peers hash the same logical row to
the same bytes.

The type tags are load-bearing:

- `NULL` ≠ empty string.
- `INTEGER 1` ≠ `TEXT "1"`.
- `REAL` uses the raw bit pattern, so `-0.0` and `0.0` differ. They are different values,
  and treating them as equal would make the digest depend on how the value was written.

### The digest of a deleted row

The digest covers the replicated columns of the row **as they are**, including a deleted
row's payload. Deleted rows replicate in full — see [lifecycle.md](lifecycle.md) § "Deleted
rows replicate in full" — so both peers hold the same bytes and compute the same digest.

## Divergence between peers

Two kinds, with different answers:

| Kind | Detected? | Repaired? |
|---|---|---|
| Two peers hold different bytes for the same key and timestamp | Yes — digests differ | No — cannot be ordered |
| Two peers hold the same key with different timestamps | Yes — normal LWW | Yes — the later timestamp wins |

The first is a bug in the write path. The second is the system working.

## Related documents

- [architecture.md](architecture.md) — roles and topology
- [sqlite-interface.md](sqlite-interface.md) — how these columns are written
- [merkle.md](merkle.md) — how the digest is used
- [lifecycle.md](lifecycle.md) — deletes and purge
- [security.md](security.md) — the visibility model
- [adr/0001](adr/0001-last-update-is-the-ordering-key.md) ·
  [adr/0004](adr/0004-digest-is-always-computed-and-keyed.md) ·
  [adr/0006](adr/0006-site-namespaced-keys.md)
