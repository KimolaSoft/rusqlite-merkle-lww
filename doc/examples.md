# Worked examples

Two example databases, worked through against the design: a **users** table and a
**groups** table.

These are not toy schemas — they surface three of the design's harder questions, and the
answers were verified against real SQLite rather than reasoned about.

---

## Example 1 — `users`

The application schema:

```sql
CREATE TABLE users (
    userid   INTEGER PRIMARY KEY,
    username TEXT NOT NULL,
    "Groups" TEXT NOT NULL,        -- a JSON array
    email    TEXT NOT NULL
);
```

### The problem: `userid` is local, not global

`userid` is an integer primary key. Two sites each minting their own sequence will
produce `userid = 1` for *different people*.

That rules out using `userid` as the sync key:

| Key | `userid` at eu-west | `userid` at tokyo | Result |
|---|---|---|---|
| `GlobalId(userid)` | 1 = Alice | 1 = Bob | **merged into one row.** Two people become one. |
| `SiteNamespaced(userid)` | (eu-west, 1) = Alice | (tokyo, 1) = Bob | Two rows. Never merge. |

Neither is right for a shared dataset. The identity that *is* global here is `email`.

### The fix: a separate sync key

The sync key must be a **distinct column** from the local primary key:

```sql
CREATE TABLE users (
    sync_id     TEXT PRIMARY KEY,      -- crate-managed: the sync key
    userid      INTEGER NOT NULL,      -- application: local sequence, UNIQUE per site
    username    TEXT NOT NULL,
    "Groups"    TEXT NOT NULL,         -- JSON array
    email       TEXT NOT NULL,
    last_update TEXT NOT NULL,         -- crate-managed
    status      TEXT NOT NULL DEFAULT 'live',
    purge_on    TEXT,                  -- crate-managed
    UNIQUE (userid)
);
```

`sync_id` is derived deterministically from `email`, so both sites compute the same
value for the same person:

```
sync_id = uuidv5(table_namespace, "user:" || lower(email))
```

SQLite can even compute it, if the derivation is a pure expression:

```sql
sync_id TEXT GENERATED ALWAYS AS ('u:' || lower(email)) STORED
```

Verified: a `STORED` generated column with a `UNIQUE` constraint works, and can be the
conflict target of an upsert. But the crate should not rely on SQL expression syntax for
the derivation, because `uuidv5` is not available in SQLite. The crate computes `sync_id`
in Rust and writes it.

**The rule this establishes:** *the sync key and the application's primary key are
different things.* The application's PK is whatever it likes — a local sequence, a
natural key, a UUID. The sync key is whatever identifies the entity *across sites*.

This means the crate's managed columns include a `sync_id`, not just `last_update`,
`status` and `purge_on`.

### Why `ON CONFLICT(sync_id)` is not enough on its own

With `sync_id` as the conflict target, the upsert works — but the **local** `userid`
can still collide:

```sql
INSERT INTO users (sync_id, userid, username, "Groups", email, …)
VALUES ('u:alice@x.com', 77, 'alice', '["admin"]', 'alice@x.com', …)
ON CONFLICT (sync_id) DO UPDATE SET
    username = excluded.username,
    "Groups" = excluded."Groups"
WHERE excluded.last_update > users.last_update;
```

Verified behaviour:

- A remote row for a user we already have, with a *different* remote `userid` → the
  upsert fires on `sync_id`, and the **local `userid` is kept**. The remote's local
  sequence number is not meaningful here, so it must not overwrite ours.
- A remote row with a *different* `sync_id` but a `userid` that collides with a
  different local row → **the insert fails** with `UNIQUE constraint failed:
  users.userid`.

That last case is a real problem: site A has Bob at `userid=77`, and site B sends a
different user who also happens to be `userid=77` locally.

The fix: **`userid` must not be unique across the merged dataset, or the remote's local
sequence must never be written.**

Two options:

| Option | How | Cost |
|---|---|---|
| **Drop the UNIQUE on `userid`** | `userid` is a plain column | The application loses the guarantee that `userid` is unique in the merged table. It is unique per site, not globally. |
| **Namespace the local sequence** | `userid` becomes `(site, userid)` | The application's queries must include the site. |

The first is usually right, because `userid` is *already* only meaningful locally. The
crate should therefore **not** treat the application's `UNIQUE` constraints as
inviolable during a merge — it should resolve collisions by keeping the local row's
value for the conflicting column and applying the incoming values to the rest.

This is a genuine extension to the design: the merge needs to know which columns are
*locally meaningful* (local sequences, local counters) and which are *entity attributes*
(name, email, groups). The former are never overwritten by a remote write.

### The `"Groups"` column: an array is not a value

`"Groups"` is a JSON array of group names. Two sites concurrently editing the same user's
groups is a **set** problem, not a last-write-wins problem.

Under LWW, the whole array is replaced:

```
site A: ["admin", "dev"]     at t=100
site B: ["admin", "ops"]     at t=101
result: ["admin", "ops"]     ← "dev" is lost
```

The union would be `["admin", "dev", "ops"]`, and the set union is the semantically
correct answer for group membership. But a union is **not** last-write-wins — it is a
different merge, and it is not commutative with respect to *removal*: a user removed
from "dev" at site A would re-acquire it when site B's stale array merges in.

Three options:

| Option | Semantics | Notes |
|---|---|---|
| **LWW on the whole array** | Simple. Concurrent edits lose one side's change. | The default. Simple, and correct for the common case where group membership changes rarely relative to writes. |
| **Set union** | Additive only. Removals do not propagate. | Needs a separate "removed" marker to be correct. |
| **Normalise into a membership table** | Each membership is its own row with its own LWW. | Removals work. Adds a table. |

The third is the correct answer for a group-membership column, and it is what the
`groups` example below ends up doing anyway. The crate should support the first
(LWW on the array) as the default and document the limitation, rather than pretending
an array is a scalar.

## Example 2 — `groups`

The application schema:

```sql
CREATE TABLE groups (
    "group"       TEXT NOT NULL,
    email         TEXT NOT NULL,
    access_level  TEXT NOT NULL,
    PRIMARY KEY ("group")
);
```

### The primary key is wrong for the data

Verified: `pk(group)` allows exactly **one row per group**, so a second member cannot
be inserted:

```
INSERT INTO groups VALUES ('admins','a@x.com','admin');    -- OK
INSERT INTO groups VALUES ('admins','b@x.com','member');   -- UNIQUE constraint failed: groups.group
```

But the columns say this is a **membership** table: `group` + `email` + `access_level`,
where each member of a group has their own access level. The natural key is the pair:

```sql
CREATE TABLE groups (
    sync_id      TEXT PRIMARY KEY,      -- crate-managed
    "group"      TEXT NOT NULL,
    email        TEXT NOT NULL,
    access_level TEXT NOT NULL,
    last_update  TEXT NOT NULL,
    status       TEXT NOT NULL DEFAULT 'live',
    purge_on     TEXT,
    UNIQUE ("group", email)
);
```

The composite conflict target works directly in the upsert:

```sql
ON CONFLICT ("group", email) DO UPDATE SET
    access_level = excluded.access_level
WHERE excluded.last_update > groups.last_update;
```

Verified: the upsert fires on the composite key and updates `access_level` correctly.

### Choosing the sync key for a membership row

The entity here is *"this person's membership of this group"* — so:

```
sync_id = uuidv5(namespace, "membership:" || group || ":" || lower(email))
```

Two independent memberships of the same group are independent rows, and two people in
the same group are independent rows. That is the right granularity: removing someone
from a group is an update to one row, not a rewrite of an array.

And this is the **normalised form of `users."Groups"`**. The array column and the
membership table are two representations of the same fact. The membership table is the
one that merges correctly.

### What the two tables look like together

```
users                      groups (memberships)
──────────────────────     ──────────────────────────
sync_id      (derived)     sync_id       (derived)
userid       (local seq)   "group"       TEXT
username                   email         TEXT
"Groups"   (JSON array)    access_level  TEXT
email                      last_update   (crate)
last_update  (crate)
status       (crate)
purge_on     (crate)
```

`users."Groups"` is then a denormalised convenience — a cached list of the user's group
names, derivable from `groups`. It can drift, and the crate should say so.

## The generalised rules

These two examples produce four rules that apply to any schema.

### 1. The sync key is not the primary key

The application's primary key is a local implementation detail. The sync key identifies
the entity **across sites**.

| The app's PK | Use as sync key? |
|---|---|
| A local integer sequence | **No** — it collides across sites |
| A local UUID generated at insert | **No** — different UUID at each site |
| A natural key that is globally unique and stable (email, external system id) | **Yes**, directly |
| A natural key, needing normalisation | **Yes**, derived — `uuidv5(namespace, …)` |

### 2. Local columns are never overwritten by a remote write

Some columns are meaningful only within the site that wrote them: local sequence
numbers, local counters, site-specific flags. A remote write must not overwrite them.

The crate needs a way to mark these:

```rust
pub struct ColumnSpec {
    pub name: String,
    pub replicated: bool,
    pub visibility: Visibility,
    /// Local values are never overwritten by a remote write. Use for local
    /// sequences and counters.
    pub local: bool,
}
```

The merge then keeps the local value for those columns, and takes the remote value for
the rest.

### 3. Arrays and sets need a different merge

A JSON array column is a value under LWW, but the user behind it probably thinks of it as
a set. Concurrent edits lose one side's change.

The crate's default is LWW on the whole value, documented as such. Normalising a set
into its own table gets correct semantics for free, and that table merges per-element.

### 4. The sync key is derived from the entity's identity, not its mutable attributes

`sync_id` must derive from something stable: an email, an external id, a natural key.
Deriving it from a mutable attribute means the key changes when the attribute changes —
which is a *move*, not an update, and moves are expensive.

For `users`, `email` is the identity. If the email changes, that is a move — the row's
identity changed. Deriving `sync_id` from the email makes this automatic but means
every email change becomes a move.

**This is worth deciding explicitly.** The alternative is a stable id that never changes
(a surrogate), with `email` as a mutable attribute. Then an email change is a plain
update, and the *only* cost is that a person changing their email becomes a different
entity to the merge — two rows, if the change happens on one site only.

The trade-off:

| Derivation | Email change | Merge identity |
|---|---|---|
| `sync_id` from `email` | A move, automatically | Always the current email |
| `sync_id` a stable surrogate | A plain update | The original identity, forever |

The stable surrogate is usually better: identity should not depend on a mutable
attribute. But it requires the application to have, or accept, an immutable id.

## Verified SQLite behaviour

Everything above was checked against rusqlite 0.41.1 with bundled SQLite 3.54.1.

| # | Question | Result |
|---|---|---|
| 1 | Can `ON CONFLICT` target a `UNIQUE` index that is not the primary key? | **Yes.** |
| 2 | With the conflict target on a non-PK unique index, is the local PK preserved? | **Yes** — the local `userid` is kept, and the remote's is not written. |
| 3 | Can a remote row collide with a *different* local row on a local `UNIQUE` column? | **Yes** — `UNIQUE constraint failed: users.userid`. The remote insert fails. |
| 4 | Is a `GENERATED ALWAYS AS (expr) STORED` column with a `UNIQUE` constraint usable as a conflict target? | **Yes.** |
| 5 | Does `PRIMARY KEY (group)` forbid a second member of the same group? | **Yes** — `UNIQUE constraint failed: groups.group`. The schema is wrong for the data. |
| 6 | Does a composite conflict target `ON CONFLICT ("group", email)` work? | **Yes.** |
| 7 | How does a JSON array merge under LWW? | Whole-value replacement. `["admin","dev"]` then `["admin","ops"]` yields `["admin","ops"]`. |
| 8 | Can a union of two JSON arrays be computed in SQL? | **Yes**, via `json_each` + `UNION` + `json_group_array`. But it is additive-only and does not model removal. |

## Open questions these examples raise

1. **Should the crate manage the `sync_id` column, or require the application to
   provide it?** Managing it is more convenient; providing it keeps the crate out of the
   schema. This document assumes the crate manages it.
2. **How is the derivation namespace chosen?** A per-table constant derived from the
   table name and the deployment's `epoch_us` is enough to keep ids stable within a
   deployment, but ids differ between deployments. If data must move between
   deployments, the namespace must be configurable.
3. **Should `local: true` columns be excluded from the digest?** If they are, two peers
   with different local sequences compute the same digest for the same entity — which is
   what you want for tree parity. If they are included, the tree hashes differ between
   peers and every bucket differs. **They must be excluded.**
4. **Should the crate offer a `membership` table helper?** Given how common
   "entity has a set of things" is, a helper that materialises a JSON array into a
   child table would remove a recurring source of merge bugs.

## Related documents

- [data-model.md](data-model.md) — key strategies, columns, the digest
- [merkle.md](merkle.md) — how the digest and tree hashes are computed
- [bulk-apply.md](bulk-apply.md) — applying many rows at once
- [../PLAN.md](../PLAN.md) — where the column-locality and derivation work lands
