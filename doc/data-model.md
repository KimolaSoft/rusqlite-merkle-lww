# Data model

## Managed tables

A **managed table** is a table the crate syncs. For it, the crate owns some columns, the
triggers, the indexes it needs, and the DDL.

```sql
CREATE TABLE device (
    userid       INTEGER NOT NULL,       -- application: globally unique, never reused
    site         TEXT NOT NULL,          -- crate-managed: the site that owns the identifier
    last_update  TEXT NOT NULL,          -- crate-managed: RFC 3339
    status       TEXT NOT NULL,          -- crate-managed: 'live' | 'deleted'
    purge_on     TEXT,                   -- crate-managed: RFC 3339, NULL when live
    payload      TEXT NOT NULL,          -- application column
    PRIMARY KEY (userid)
);
```

The managed column names are configurable per table; these are the defaults.

| Column | Type | Managed? | Meaning |
|---|---|---|---|
| `last_update` | TEXT | always | RFC 3339 instant of the last modification. **The ordering key.** |
| `status` | TEXT | always | `'live'` or `'deleted'` |
| `purge_on` | TEXT | always | When the row may be physically removed. NULL while live. |
| `site` | TEXT | when the table has allocated identifiers | The site that owns the identifier. See below. |
| *the key column* | — | only if the app has no usable key | **The sync key.** See below. |

`last_update`, `status` and `purge_on` are **replicated**, and always managed by the
crate.

**The key column is managed only when the application cannot supply one.** With
`KeyStrategy::GlobalId`, the application's own globally unique column is the key and the
crate adds nothing. With `Derived`, `SiteNamespaced` or `Surrogate`, the crate manages a
`sync_id` (or `sync_ns`) column.

**The `site` column is managed only when the table has allocated identifiers.** It is
replicated and included in the digest. See § "The `site` column".

`last_update` is written by the engine, not the application — but an application that
supplies its own value has it respected, which is what makes backfill and import
possible. See [sqlite-interface.md](sqlite-interface.md).

### The sync key should be the application's own key, when it can be

The **sync key** identifies the entity across sites, so that two sites describing the same
thing converge on one row.

The best case is that the application already has a key that is **globally unique and
stable**, and then the sync key is simply that column — no extra column, no derivation.

| The app's key | Usable as the sync key? |
|---|---|
| A globally unique, never-reused identifier (a `userid` allocated from a per-site range) | **Yes, directly.** No extra column. |
| A stable, globally unique natural key (an external system id) | **Yes, directly.** |
| A locally generated UUID | **Yes** — but it merges nothing, only avoids collisions |
| A local integer sequence | **No** — every site has a `1` |
| A mutable natural key (email) | **No** — a change becomes a move |

So the rule is: **use the application's own key if it is already global and stable;
otherwise derive one.**

### When a derived key is needed

Some applications cannot offer a globally unique key — a local integer sequence, or a
mutable one. Then the crate manages a separate column:

```
sync_id = uuidv5(namespace, prefix + identity_columns)
```

The application's primary key is untouched and remains its own business. The derivation
input must be **stable**, because the sync key is the entity's identity; if it changes, the
key changes, and that is a **move** rather than an update.

| Derivation | On an email change | Merge identity |
|---|---|---|
| `sync_id` from `email` | A move, automatically | The current email |
| `sync_id` a stable surrogate | A plain update | The original identity, forever |

Both are supported. The stable surrogate is usually better — identity should not depend
on a mutable attribute — but it requires the application to have, or accept, an immutable
id. The table spec chooses.

The **namespace** in the derivation must be stable across the deployment. A per-table
constant derived from the table name and the deployment's `epoch_us` is enough to keep
ids stable within a deployment; ids differ between deployments. If data must move
between deployments, the namespace must be configurable.

### Globally unique identifiers need an allocation scheme

A globally unique, never-reused identifier does not come for free. A site needs either a
reserved range or a prefix, and the scheme must be shared across the deployment so peers
can validate the identifiers they receive.

The ranges live in a **site registry** — a replicated table, and part of the join bundle,
because a node cannot validate an identifier it has no range for. The registry lives in the
**main database**: triggers cannot reference an attached database, and the range check must
be a trigger.

See [examples.md](examples.md) § "Allocating locally, and never reusing" for the two
schemes and § "Example 3 — `sites`" for the registry, and
[adr/0013-site-registry-and-range-allocation.md](adr/0013-site-registry-and-range-allocation.md)
for the full decision.

### The `site` column

A table with globally unique identifiers carries a `site` column naming **the site that
owns the identifier** — the site whose range contains it.

It is **not** the site that last wrote the row. When site 2 edits a user created by site 1,
the row's `site` stays `site1` and only `last_update` moves. If `site` tracked the writer,
the identifier would fall outside the new owner's range and the range check would fire on a
legitimate edit.

| Property | Value |
|---|---|
| Managed by | The crate — the application creates the column, the crate fills it. |
| Replicated | Yes. |
| In the digest | Yes — both peers must agree on the owner. |
| Value | The local site by default: `site TEXT NOT NULL DEFAULT (local_site())`. |
| Immutable | Yes, for the life of the row. |

The column is populated by the engine, not the application. A scalar function in a column
`DEFAULT` is evaluated per insert, an explicit value overrides it, and a connection without
the function fails closed — the Enforced dialect's existing behaviour applied to one more
column.

The engine-level range check is then a trigger keyed on the row's own owner:

```sql
CREATE TRIGGER users_range BEFORE INSERT ON users
WHEN EXISTS (
    SELECT 1 FROM sites s
    WHERE s.site_id = NEW.site
      AND NOT (NEW.userid BETWEEN s.uid_start AND s.uid_end)
)
BEGIN
    SELECT RAISE(ABORT, 'userid outside own site range');
END;
```

It fires only when the registry **knows** the row's owner. A site created mid-session may
have its rows arrive before the registry row does, and rejecting them would deadlock the
conversation; such rows are accepted provisionally and re-validated later.

### Timestamps

RFC 3339, UTC, `Z` suffix, exactly six fractional digits:

```
2026-11-24T17:24:03.302238Z
```

Fixed width means **lexicographic order equals chronological order**, so `ORDER BY
last_update` is chronological and `WHERE last_update >= ?` is an index range scan. No date
functions are needed anywhere in the hot path.

Timestamps are generated in Rust, never with SQLite `strftime`. `strftime` yields
millisecond precision and three fractional digits, which silently breaks the fixed-width
property when mixed with Rust-generated values.

## Key strategies

The strategy decides **which column is the sync key**, and therefore how the key is
derived. Selected per table.

```rust
pub enum KeyStrategy {
    /// The application's own column, already globally unique and stable. Used
    /// directly as the sync key — no extra column, no derivation.
    ///
    /// For example a `userid` allocated from a per-site range.
    GlobalId { key_columns: Vec<String> },

    /// A deterministic id derived by the crate from identity columns. Both sites
    /// derive the same value for the same entity, so they converge on one row.
    /// The crate manages the column.
    Derived {
        /// Columns whose values form the derivation input.
        identity_columns: Vec<String>,
        /// Optional prefix distinguishing entity kinds within a table.
        prefix: Option<String>,
    },

    /// Owner-namespaced: the row's site is part of the key, so two sites never
    /// contend over the same row.
    SiteNamespaced { site_column: String, key_columns: Vec<String> },

    /// The crate generates a random surrogate. Never merges across sites.
    Surrogate { column: String },

    /// SQLite's implicit rowid. Zero schema impact, but VACUUM can renumber.
    Rowid,
}
```

| Strategy | Derivation | Merges across sites? | Extra column? |
|---|---|---|---|
| `GlobalId` | the application's own column | **yes**, if the column is globally unique | **no** |
| `Derived` | `uuidv5(namespace, prefix + identity columns)` | **yes** | yes, crate-managed |
| `SiteNamespaced` | `(site, key columns)` | no | yes, crate-managed |
| `Surrogate` | a random UUIDv7 | no | yes, crate-managed |
| `Rowid` | the rowid | no | no |

`GlobalId` is the default, because the expected deployment has a globally unique
identifier already. `Derived` covers the applications that do not.

### Choosing: shared dataset versus independent sites

The decision hinges on one question:

> **When two sites create what should be the same logical entity — the same user, the same
> order — do they become one row or two?**

| | `Derived` / `GlobalId` | `SiteNamespaced` |
|---|---|---|
| Rows created | one row, if both sites derive the same id | two rows, both survive |
| After a sync | one row; LWW picks the winner | two rows, both present |
| Merge conflict | resolved by `last_update` | none |
| Coordination needed | a deterministic id derivation | none |

**If the sites share one dataset** — the same user is the same user everywhere, and the
sites are peers over a single logical dataset — then `Derived` is correct. Two sites
updating the same user converge on one row, and LWW resolves which write wins. This is
the expected deployment.

**If the sites own independent data** — multi-tenant, or separately operated — then
`SiteNamespaced` is correct, and `Derived` would be actively harmful: it would merge two
customers' records into one row and let LWW silently discard one of them.

The recommendation is **`Derived` as the default**, with `SiteNamespaced` available for
tables that are genuinely per-site. The failure modes are asymmetric: being wrong with
`Derived` loses data irrecoverably, while being wrong with `SiteNamespaced` leaves two
rows that can still be reconciled deliberately. That asymmetry argues for
`SiteNamespaced` as the *safe* default — but the common case here is a shared dataset,
so `Derived` is the better default and `SiteNamespaced` is the deliberate opt-in.

### Getting the id right

Merging only happens if **both sites derive the same id** for the same entity. Two
mechanisms:

| Mechanism | Merges? | Notes |
|---|---|---|
| UUIDv7 / ULID / Snowflake generated locally | **no** | Each site generates a *different* id, so the entities stay separate. Useful only when the id is already globally assigned by something upstream. |
| UUIDv5 derived from identity columns | **yes** | `uuidv5(namespace, "user:alice")` is deterministic, so both sites produce the same id. This is the mechanism for "the same user is the same user". |
| An id from an existing system of record | **yes** | The id already has global meaning. |

So "the same user is the same user" requires a **deterministic** id — derived from
identity columns, or assigned by something that already agrees across sites. A randomly
generated globally-unique id does not merge anything; it merely avoids collisions.

This is worth being explicit about, because it is the single most common way a
`Derived` deployment fails to behave as intended.

### Identity columns must be stable

The derivation input is the entity's identity, so it must not change. If it does, the
sync key changes, and that is a **move** rather than an update.

A `users` table whose identity is `email` therefore treats an email change as a move. A
`users` table whose identity is a stable surrogate treats it as an update. See
[examples.md](examples.md) § "Deriving the sync key".

### Keys are immutable

Changing a sync key is a **move**, not an update. `Db::move_key(table, from, to)`
tombstones the old key and inserts the new one atomically.

Every other column is freely mutable, including a `username` or an owner field. Only the
sync key is special. Under `SiteNamespaced` the site column is part of the key and is
therefore also immutable.

### The sync key encoding

The key is encoded canonically (§ "Canonical encoding") into a BLOB, and that BLOB is the
primary key of `_sync_log`. Under `SiteNamespaced` the encoding includes the namespace, so
`(eu-west, abc)` and `(tokyo, abc)` are distinct rows in the log.

The rule is one rule: **the key encoding is the value encoding, applied to each key
column in registered order.** `userid = 10001` encodes as a tagged INTEGER value, and the
same bytes appear in `_sync_log.k`, in the digest input, and on the wire. There is no
separate key format to keep in step.

## Column specifications

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

    /// The column's value is meaningful only within the site that wrote it — a local
    /// sequence number, a local counter, a site-specific flag. A remote write never
    /// overwrites it.
    pub local: bool,

    /// The column holds a set (a JSON array), and concurrent edits should merge as a
    /// set rather than replace the whole value.
    pub merge: MergeMode,
}

pub enum MergeMode {
    /// Last write wins on the whole value. The default.
    LastWriteWins,
    /// The values are sets; concurrent edits union. Additive only — removals do not
    /// propagate, so this is only correct for append-only sets.
    SetUnion,
}
```

### Allocation

A table whose identifiers come from a per-site range declares how the identifiers are
allocated. The crate then installs the range-enforcement trigger and manages the `site`
column.

```rust
pub struct AllocationSpec {
    /// The column holding the globally unique identifier.
    pub id_column: String,

    /// The column naming the site that owns the identifier. The crate manages it.
    pub site_column: String,

    /// The registry table holding the site ranges. Must be in the same database as the
    /// table — triggers cannot reference an attached database.
    pub registry_table: String,
}
```

```rust
pub struct TableSpec {
    pub name: String,
    pub kind: TableKind,
    pub key: KeyStrategy,
    pub columns: Vec<ColumnSpec>,
    pub client_visible: bool,

    /// Present when the identifiers are allocated from per-site ranges.
    pub allocation: Option<AllocationSpec>,
}
```

### Visibility

| Peer class | Public columns | Restricted columns | May write |
|---|---|---|---|
| **Trusted** (master, child) | yes | yes | yes |
| **Client** (read-only) | yes | **no** | no |

Tables can also be marked `client_visible = false`, which excludes them from client
sessions entirely — their existence and shape are not advertised.

Enforcement points are listed in [security.md](security.md).

### Local columns

Some columns are meaningful only within the site that wrote them: a local sequence
number, a local counter, a site-specific flag. A remote write must not overwrite them,
because the remote's value is from a different sequence and is meaningless here.

This matters concretely. With a `users` table whose `userid` is a local sequence:

- The sync key is `sync_id`, derived from the user's identity. ✓
- A remote row arrives with the remote's `userid`, which is a different local number.
  **The local `userid` must be kept.**
- If the remote's `userid` collides with a *different* local row's `userid`, the insert
  fails on the `UNIQUE` constraint.

Verified behaviour (see [examples.md](examples.md)):

```
remote row (sync_id=sid-a, userid=77) against local row (sync_id=sid-a, userid=1)
  → the upsert fires on sync_id, the local userid=1 is kept, the remote's 77 is not written ✓
remote row (sync_id=sid-b, userid=77) against local row (sync_id=sid-a, userid=1)
  → succeeds, because 77 does not collide with 1
remote row (sync_id=sid-b, userid=1) against local row (sync_id=sid-a, userid=1)
  → UNIQUE constraint failed: users.userid   ✗
```

That last case is a genuine failure, and it is why the application should generally
**not** put a `UNIQUE` constraint on a local sequence. The constraint is meaningful
within one site but not across the merged dataset.

Local columns are also **excluded from the digest**. If they were included, two peers
holding the same entity with different local sequences would compute different digests,
every bucket would differ, and no sync would ever converge.

### Set-valued columns

A JSON array column is a value under LWW, but the application usually thinks of it as a
set. Concurrent edits lose one side's change:

```
site A: ["admin", "dev"]   at t=100
site B: ["admin", "ops"]   at t=101
LWW result: ["admin", "ops"]     ← "dev" is lost
```

The default is `LastWriteWins`, and the limitation is documented rather than papered
over. `SetUnion` is available for append-only sets, but it is **additive only**: a
removal never propagates, because the removed element comes back on the next union.

The correct answer for group membership is to **normalise the set into its own table**,
where each element is an independent row with its own LWW and its own deletion. See
[examples.md](examples.md) § "The `"Groups"` column".

## Internal tables

```sql
CREATE TABLE _sync_meta (
    key   TEXT PRIMARY KEY,
    value BLOB NOT NULL
);
```

Keys: `node_id`, `site`, `epoch_us`, `schema_version`, `last_purge_instant`,
`retention_us`, `last_issued_us`.

**No secret material is stored here.** The digest seed and the role strings are supplied
by the application as configuration and held in memory for the life of the process. See
[adr/0014-peer-credentials-and-the-uuid-directory.md](adr/0014-peer-credentials-and-the-uuid-directory.md)
§ 5.

```sql
CREATE TABLE _sync_log (
    tbl          TEXT    NOT NULL,
    k            BLOB    NOT NULL,   -- canonical encoded sync key
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
-- The UUID directory. Node-local control state, NOT replicated.
-- Every master holds every node's UUID; a child holds only its own.
CREATE TABLE _sync_directory (
    node_id   TEXT PRIMARY KEY,
    role      TEXT NOT NULL,       -- master | child
    parent_id TEXT,                -- the issuing master; NULL for a master
    added_us  INTEGER NOT NULL
);
```

The directory is **control plane, not data plane**. It is not in `_sync_log`, not in the
Merkle tree, and not in any application table. Masters merge it at handshake and take the
union; children are never sent it.

A child that learned another child's UUID could present `(that UUID, the child string)`
and be accepted as that child — the child string is shared by all children, so the UUID is
the only thing distinguishing them. The directory is therefore the child security
boundary, and it is withheld from children for exactly that reason.

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
digest   = seeded_xxh3(seed, digest_input)
row_hash = xxh3(key_enc || value_enc(last_update) || value_enc(status)
                || value_enc(purge_on) || digest)
```

`digest_input` is defined in § "Canonical encoding". `row_hash` deliberately does **not**
include the table name: it is a function of the digest, so a client that receives a
digest it cannot recompute still computes the same `row_hash`. That is what lets a
read-only client participate in tree diffing without ever holding a restricted value.

The digest is **16 bytes**, big-endian on the wire, and compared as an unsigned 128-bit
integer — never as a byte slice in memory order. See § "The digest is keyed".

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

### Local columns are excluded

The digest covers replicated columns **except** those marked `local`.

This is required for convergence. Two peers holding the same entity have different local
sequence values — `userid = 1` at one site and `userid = 77` at the other. If those were
in the digest, the two peers would compute different digests for the same entity, every
bucket would differ, and no sync would ever converge.

Excluding them means the digest describes the **entity**, not the site's local view of
it. The local column is still stored and still transmitted — it is simply not part of the
identity of the row.

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
value_enc := tag(u8) || len(u32 LE) || bytes
key_enc   := concat over the key columns, in registered order, of value_enc
```

| `tag` | Type | Bytes |
|---|---|---|
| 0 | NULL | none |
| 1 | INTEGER | `i64`, little-endian |
| 2 | REAL | `f64`, raw bit pattern |
| 3 | TEXT | UTF-8 |
| 4 | BLOB | raw |

`len` is `u32`, little-endian. The length is of the **payload**, not including the tag.

**The key encoding is the value encoding, applied to each key column.** One rule, no
special cases:

| Strategy | `key_enc` |
|---|---|
| `GlobalId { key_columns: ["userid"] }` | `value_enc` of the `userid` column |
| `Derived` | `value_enc` of the derived UUID, as a 16-byte BLOB |
| `SiteNamespaced { site_column, key_columns }` | `value_enc` of the site column, then each key column |
| `Surrogate` | `value_enc` of the surrogate, as a 16-byte BLOB |
| `Rowid` | `value_enc` of the rowid as an INTEGER |

So `userid = 10001` encodes as `01 08 00 00 00 11 27 00 00 00 00 00 00` — tag `1`,
length `8`, then the `i64` little-endian. The same rule produces the bytes for
`_sync_log.k`, for the digest input, and for the wire.

Using the **runtime** type rather than the declared SQL type is deliberate: the tag
distinguishes `INTEGER 1` from `TEXT "1"`, so two peers that disagree about a value's
storage cannot accidentally agree on its digest. A peer whose *declared* types differ is
caught earlier, by `schema_hash`.

Column order is fixed by the registered spec, so both peers hash the same logical row to
the same bytes.

### The digest input

The digest covers each of these exactly once, in this order:

```
digest_input := table_name_enc
             || key_enc
             || value_enc(last_update)
             || value_enc(status)
             || value_enc(purge_on)
             || concat over the remaining replicated, non-local columns,
                in registered order, of value_enc
```

"the remaining columns" is every replicated, non-local column **except** the three named
above and the key columns. It includes the `site` column on tables with allocated
identifiers, because it is replicated and both peers must agree on it.

The **table name** is in the input, so a digest identifies a row *of a particular table*,
not just a row. Two tables with identical columns and identical values do not produce
the same digest.

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
