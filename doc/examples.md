# Worked examples

Two example databases, taken all the way through the design: a **users** table and a
**groups** table.

These are not toy schemas. They drove three of the design's harder decisions, and every
SQLite claim below was verified against rusqlite 0.41.1 / bundled SQLite 3.54.1.

---

## Example 1 — `users`

The application schema:

```sql
CREATE TABLE users (
    userid   INTEGER PRIMARY KEY,     -- globally unique, never reused
    username TEXT NOT NULL,
    "Groups" TEXT NOT NULL,           -- a JSON array of group names
    email    TEXT NOT NULL            -- mutable
);
```

### `userid` is globally unique by construction

This is the key property, and it changes everything. `userid` is not a local sequence —
it is a **globally unique, never-reused** identifier, allocated from a range reserved for
the site that mints it:

```
site 1 → 10000 … 19999
site 2 → 20000 … 29999
site 3 → 30000 … 39999
```

Within its own range, a site allocates sequentially. Across the deployment, no two
identifiers ever collide, and **an identifier is never reissued** — so a `userid` is a
permanent reference to a person, for as long as the record exists.

That makes `userid` usable as the sync key directly. There is no separate `sync_id`
column needed for this table, because the application's own primary key is already a
globally unique, stable identifier.

Two ways to arrange the ranges:

| Scheme | Layout | Notes |
|---|---|---|
| **Site prefix** | `userid = site_id * 10000 + local_counter` | Derived, not allocated. The site needs only its own `site_id`. Verified: site 1 mints 10001, 10002, 10003 and never leaves its range. |
| **Explicit allocation** | A table of `(site, lo, hi)` ranges, minted from the range | Flexible; ranges can be sized per site. The table must be shared with peers, since a peer must accept ids from ranges it does not mint from. |

The site-prefix scheme is simpler and is what the numbers above describe. The explicit
allocation scheme is better when sites differ wildly in size.

In both cases the ranges live in a **`sites` registry table**, described in
[Example 3](#example-3--sites) below. The registry is what makes the scheme enforceable
rather than merely conventional.

### Allocating locally, and never reusing

The local counter must be **durable and monotonic**, and must survive a crash without
reissuing an identifier.

Verified: `UPDATE ... RETURNING` gives a monotonic allocator in a single statement:

```sql
CREATE TABLE _seq (id INTEGER PRIMARY KEY CHECK (id = 1), v INTEGER NOT NULL);

UPDATE _seq SET v = v + 1 WHERE id = 1 RETURNING v;   -- → 10000, 10001, …
```

It must be a separate transaction from the insert, or a rolled-back insert could reissue an
identifier. Two safe options:

- Allocate the identifier in its own transaction, then insert.
- Allocate and insert together, but keep the counter **outside** the transaction — e.g. in
  the allocator's own file, or by advancing the counter in a way that survives rollback.

The simplest correct approach: **allocate durably first, insert second.** An identifier
that was allocated but never used is simply skipped — a gap, not a reuse. That is the right
trade: gaps are harmless, reuse is corruption.

### The range check can be enforced by the engine

A site must not mint an identifier outside its range, and — more importantly — a row
arriving from a peer must be *accepted* even though its identifier is outside the local
range.

Every table with allocated identifiers carries a `site` column naming **the site that
owns the identifier**, not the site that last wrote the row. (When site 2 edits a user
created by site 1, `site` stays `site1`; otherwise the identifier would fall outside the
new owner's range and the check below would fire on a legitimate edit.)

The check is keyed on the row's own owner, so a peer's rows are accepted on the same terms
as local ones. Verified, with the range check as a trigger against the `sites` registry:

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

Verified behaviour:

```
(10001, site1) → OK          (own range)
(99999, site1) → rejected    (outside own range)         ✓
(20001, site2) → OK          (a peer's row)
(30001, site3) → OK          (site not yet in the registry — provisional)
```

The trigger fires on the `ON CONFLICT DO UPDATE` path too, so the upsert is covered by
the same rule.

Two consequences:

- **Every row carries a `site` column.** Not a namespace key — a plain, replicated
  attribute recording which site owns the identifier. It is what makes the range check
  possible, and it makes "show me site 1's users" an indexed equality test.
- **Peers must know each other's ranges**, which is why the registry is deployment-wide
  state — like the epoch and the digest seed — and part of the join bundle.

The `site` column is populated by the engine, not the application:

```sql
site TEXT NOT NULL DEFAULT (local_site())
```

Verified: a scalar function in a column `DEFAULT` is evaluated per insert, an explicit
value overrides it, and on a connection where the function is not registered the insert
fails closed.

### `email` is mutable, and that is fine

The user's observation: *emails can change due to spelling mistakes, marriage, etc, but a
userid would never change.*

So `userid` is the identity, and `email` is an ordinary attribute. **A changed email is a
plain update, not a move.** No key change, no tombstone-and-reinsert, no
`uuid5(namespace, email)` derivation that would break when the address changes.

This is the resolution of the open question from the previous revision of the design: the
sync key should be derived from something that never changes, and here that something is
already present — `userid`.

### `username` and `"Groups"` are ordinary attributes

`username` is mutable and replicated; a change syncs as an update.

`"Groups"` is the interesting one, and it is addressed below.

### The merge

```sql
INSERT INTO users (userid, username, "Groups", email, last_update, status, purge_on)
VALUES (?,?,?,?,?,?,?)
ON CONFLICT (userid) DO UPDATE SET
    username    = excluded.username,
    "Groups"    = excluded."Groups",
    email       = excluded.email,
    last_update = excluded.last_update,
    status      = excluded.status,
    purge_on    = excluded.purge_on
WHERE excluded.last_update > users.last_update;
```

Verified: the conflict target on the primary key works, and the `WHERE` clause keeps the
newer row.

Because `userid` is globally unique, there is **no local column in this table** — every
column is a property of the person, and every column may be overwritten by a remote write.
The `local` column concept is still needed in general (see below), but this table does not
use it.

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

### What this table is

`pk(group)` is the **group name**, and there is one row per group. So:

- `email` — the group's owner, or a contact.
- `access_level` — the permission the group confers.

And `users."Groups"` holds the **names** of the groups a user belongs to. Together:

```
users."Groups" = ["admins", "dev"]      → which groups this user is in
groups        = ("dev", owner, "write") → what the "dev" group confers
```

The user's effective permission is the union of the `access_level` values of the groups
they are in.

Verified: `pk(group)` does mean one row per group name — inserting a second row with the
same name fails with `UNIQUE constraint failed: groups.group`. That is consistent with the
schema's intent, not a mistake in it.

### The group's identity is its name

`group` is a natural key, and it is stable — a group is renamed rarely, and a rename
*should* be a move, since every `users."Groups"` array references the name.

So the sync key for `groups` is the group name. But there is a subtlety: **is a group name
global or per-site?**

| Model | Key | Consequence |
|---|---|---|
| **Global** | the name alone | A group called `admins` is one group across the deployment. A rename affects every site. |
| **Per-site** | `(site, name)` | Each site has its own `admins`. A rename affects one site. |

With the `users` table using globally unique `userid`s, the dataset is shared, so
**global group names** are the consistent choice. A group is one group, everywhere.

That has a consequence worth stating: **renaming a group is a move** that also invalidates
every `users."Groups"` array referencing it. The rename must be applied to the `groups`
table *and* to every affected user's array, or the references dangle.

This is the strongest argument for normalising membership into its own table, which the
next section addresses.

### `access_level` is a whole-group value

Because there is one row per group, `access_level` is the group's permission — not a
per-member one. A member with a different permission than the group default is not
expressible in this schema. If per-member permissions are needed, the key must be
`(group, email)`, which changes the table's meaning from "group definition" to "membership".

Worth deciding which one is intended. The user's description — *"the groups table is
mapping group names to their associated permissions"* — reads as the group definition
model, one row per group.

## The `users."Groups"` array, reconsidered

The array is the membership list, and it is the one part of this schema that merges
incorrectly.

### Under LWW, concurrent edits lose one side's change

```
site A: user 10001 → ["admins"]        at t=100
site B: user 10001 → ["admins", "dev"]  at t=101
result: ["admins", "dev"]               ← "admins" membership at A survives, but if A had
                                          ["dev"] and B had ["ops"], one is lost
```

More precisely:

```
site A: ["dev"]        site B: ["ops"]        →  one of them is lost entirely
```

Whole-value LWW is correct only if group membership changes rarely relative to other
writes. For an access-control system, membership changes are the *point*, so the array is
the wrong shape.

### The array also holds names, not identifiers

A group rename changes the meaning of every array referencing it, with no referential
integrity to catch it. A membership table keyed by identifier has the same problem — the
group's key changes — but at least it is one row rather than a string inside an array.

### The normalised form

```sql
CREATE TABLE memberships (
    userid       INTEGER NOT NULL,
    "group"      TEXT    NOT NULL,
    access_level TEXT    NOT NULL,
    last_update  TEXT    NOT NULL,
    status       TEXT    NOT NULL DEFAULT 'live',
    purge_on     TEXT,
    PRIMARY KEY (userid, "group")
);
```

Now:

- **Each membership is an independent row** with its own `last_update`, so concurrent
  membership changes at different groups do not conflict at all.
- **Adding a membership** is an insert; **removing** one is a soft delete. Both propagate
  correctly, and removals work — which a set-union merge cannot do.
- **`users."Groups"` becomes a derived convenience** — a denormalised cache of the user's
  group names, readable from `memberships`. It can drift, and the crate should say so.

The cost is a third table. Given that group membership is the thing the schema exists to
express, that cost is worth paying.

### Three options for the crate

| Option | Semantics | Notes |
|---|---|---|
| **LWW on the whole array** | Simple. Concurrent edits lose one side's change. | The default. Documented as a limitation, not papered over. |
| **Set union** | Additive only. Removals never propagate. | `MergeMode::SetUnion`. Only correct for append-only sets. |
| **Normalise into a membership table** | Each membership merges independently; removals work. | The correct answer for this schema. |

The crate should ship the first two and document the third as the recommended shape.

## Example 3 — `sites`

The registry that makes the range scheme enforceable:

```sql
CREATE TABLE sites (
    site_id     TEXT    NOT NULL,      -- immutable, globally unique — THE SYNC KEY
    site_name   TEXT    NOT NULL,      -- mutable display name
    uid_start   INTEGER NOT NULL,      -- inclusive lower bound
    uid_end     INTEGER NOT NULL,      -- inclusive upper bound
    city        TEXT,                  -- descriptive, mutable
    country     TEXT,                  -- descriptive, mutable
    last_update TEXT    NOT NULL,      -- crate-managed
    status      TEXT    NOT NULL,      -- crate-managed
    purge_on    TEXT,                  -- crate-managed
    PRIMARY KEY (site_id),
    CHECK (uid_start <= uid_end)
);
```

### Where it lives, and why it cannot be anywhere else

**In the main database.** Not a separate `sites.db`, not a config file. This is a SQLite
constraint rather than a preference:

> `CREATE TRIGGER … WHEN NOT EXISTS (SELECT 1 FROM reg.sites …)` where `reg` is an
> `ATTACH`ed database → **`trigger users_range cannot reference objects in database reg`**

Triggers cannot reference objects in another database. The range check must be a trigger
(it is a value-level invariant) and the trigger must read the registry, so the registry and
the data must share a file. Verified.

### What is immutable and what is not

| Column | Mutable? | Why |
|---|---|---|
| `site_id` | **No** | The sync key. It is the identity of the site. |
| `site_name` | Yes | A rename is an update, not a move. A rename must not move the range. |
| `uid_start`, `uid_end` | **No** | The range is a fact about the deployment. Changing it would invalidate every identifier already minted under it. |
| `city`, `country`, … | Yes | Ordinary descriptive data. |

The immutability of the range is enforceable. Verified:

```sql
CREATE TRIGGER sites_range_immutable BEFORE UPDATE ON sites
WHEN NEW.uid_start <> OLD.uid_start OR NEW.uid_end <> OLD.uid_end
BEGIN
    SELECT RAISE(ABORT, 'site range is immutable');
END;
```

```
UPDATE sites SET city='York'      WHERE site_id='site1'  → OK
UPDATE sites SET uid_start=11000  WHERE site_id='site1'  → rejected
```

A **decommissioned site** is soft-deleted (`status = 'deleted'`) and its range is retired
forever — never reissued, exactly like a retired identifier.

### Ranges must not overlap

Two sites whose ranges overlap can mint the same identifier. Overlap is refused at the
point of entry. Verified:

```sql
CREATE TRIGGER sites_no_overlap BEFORE INSERT ON sites
WHEN EXISTS (
    SELECT 1 FROM sites s
    WHERE s.uid_start <= NEW.uid_end AND NEW.uid_start <= s.uid_end
)
BEGIN
    SELECT RAISE(ABORT, 'site ranges overlap');
END;
```

Together with `UNIQUE (uid_start)` and `CHECK (uid_start <= uid_end)`, a malformed
allocation is refused by the engine.

### The registry is replicated — and also part of the join bundle

A change to `city` propagates like any other write. But a node cannot validate an
identifier it has no range for, so the registry is also **deployment-wide state**, like the
epoch and the digest seed, and is **seeded identically at join**.

For a site created *during* a session, the registry row may reach the peer after the site's
data rows do. The range trigger therefore fires only when the registry **knows** the row's
owner — an unknown owner is accepted provisionally and re-validated once the registry
catches up. See § "The range check can be enforced by the engine".

### Who assigns the ranges?

Not the crate. Two nodes creating sites concurrently can each accept a range that overlaps
the other's, because each check passes locally and the merge happens afterwards. Range
allocation is an administrative act — an operator, or a small allocation service, assigns
ranges and seeds them identically on every node. The registry then replicates as ordinary
data.

A simpler variant exists: derive the range from the site id (`uid_start = site_id * width`),
which makes overlap impossible by construction. It fixes the range width and still needs a
central allocation of site ids. The table stores explicit bounds, so either scheme can be
used without changing the schema.

## The generalised rules

These two examples produce four rules that apply to any schema.

### 1. The sync key should be the application's own key, when it is already global and stable

The previous revision of this document proposed a separate `sync_id` column, because
`userid` looked like a local sequence. It is not — it is globally unique by construction.
So:

| The app's key | Usable as the sync key? |
|---|---|
| A globally unique, never-reused identifier (`userid` from a site range) | **Yes, directly.** No extra column. |
| A stable, globally unique natural key (an external system id) | **Yes, directly.** |
| A locally generated UUID | **Yes** — but it merges nothing, only avoids collisions |
| A local integer sequence | **No** — every site has a `1` |
| A mutable natural key (email) | **No** — a change becomes a move |

The rule generalises: **use the application's own key if it is already global and stable;
otherwise add a derived one.**

### 2. Global identifiers need an allocation scheme, and a registry

Globally unique, never-reused identifiers do not come for free. A site needs either a
reserved range or a prefix, and the scheme must be shared across the deployment so peers
can validate the identifiers they receive.

The allocation is durable, and allocation happens **before** use. A gap is harmless; a reuse
is corruption.

The ranges live in a **registry table** that is replicated and part of the join bundle, so
that:

- a peer can validate an identifier it did not mint;
- the engine can enforce the invariant on every write, including the upsert path;
- the deployment has one place that describes its sites.

The registry must be in the **main database** — triggers cannot reference an attached
database — and the range it records is **immutable**.

This whole mechanism is **optional**. An application that already has an identifier
scheme — a central id service, a per-team leased range of its own — declares no
`AllocationSpec` and gets none of it: no registry, no `site` column, no range trigger,
and no REGISTRY phase. See
[adr/0019](adr/0019-the-allocation-registry-is-optional.md).

### 3. Local columns are never overwritten by a remote write

Some columns are meaningful only within the site that wrote them — a local counter, a
site-specific flag. A remote write must not overwrite them.

This table has none, because `userid` is global. But the concept still applies wherever a
table mixes global and local state, and it has a hard requirement:

**Local columns are excluded from the digest.** Otherwise two peers holding the same
entity with different local values would compute different digests, every bucket would
differ, and nothing would ever converge.

### 4. Arrays and sets need a different merge

A JSON array column is a value under LWW, but the application thinks of it as a set.
Concurrent edits lose one side's change, and a removal cannot be expressed correctly at
all.

Normalising a set into its own table gets correct semantics for free, and the table
merges per element.

## Verified SQLite behaviour

Everything above was checked against rusqlite 0.41.1 with bundled SQLite 3.54.1.

| # | Question | Result |
|---|---|---|
| 1 | Can `UPDATE ... RETURNING` serve as a monotonic allocator? | **Yes** — `UPDATE _seq SET v = v + 1 WHERE id = 1 RETURNING v` returns an increasing value. |
| 2 | Can a trigger reject an insert whose id is outside an allocated range? | **Yes** — `RAISE(ABORT, …)`. |
| 3 | Does the range trigger fire on the `ON CONFLICT DO UPDATE` path? | **Yes** — an out-of-range upsert is aborted too. |
| 4 | Can the range check be per-row-site rather than per-local-site? | **Yes** — `WHEN EXISTS (SELECT 1 FROM sites WHERE site_id = NEW.site AND …)`. Verified: site 2's id 10001 is rejected, site 1's own 10001 accepted. |
| 5 | Can the check accept a row whose owner is not yet in the registry? | **Yes** — the trigger fires only when the registry knows the owner, so a site created mid-session does not deadlock the conversation. |
| 6 | Can a trigger read a table in an `ATTACH`ed database? | **No** — `trigger users_range cannot reference objects in database reg`. The registry must be in the main database. |
| 7 | Can a scalar function be used in a column `DEFAULT`? | **Yes** — `site TEXT NOT NULL DEFAULT (local_site())` is evaluated per insert, an explicit value overrides it, and a connection without the function fails closed. |
| 8 | Can `ALTER TABLE … ADD COLUMN` use a non-constant default? | **No** — rejected by SQLite. The `DEFAULT (fn())` form needs `CREATE TABLE`; for an existing table use an `AFTER INSERT` trigger. |
| 9 | Does an `AFTER INSERT` trigger fire on the `ON CONFLICT DO UPDATE` path? | **No** — which is correct: that path leaves an existing row's `site` alone, and it already has one. |
| 10 | Can a registry reject an overlapping range? | **Yes** — a `BEFORE INSERT` trigger over `(uid_start, uid_end)` rejects overlap; `UNIQUE (uid_start)` and `CHECK (uid_start <= uid_end)` are also available. |
| 11 | Can a range be made immutable while descriptive columns stay mutable? | **Yes** — a `BEFORE UPDATE` trigger rejects a range change while a descriptive update passes. |
| 12 | Does `PRIMARY KEY (group)` mean one row per group name? | **Yes** — a second row with the same name fails with `UNIQUE constraint failed: groups.group`. |
| 13 | Does a composite conflict target work for a membership table? | **Yes** — `ON CONFLICT ("group", email) DO UPDATE`. |
| 14 | How does a JSON array merge under LWW? | Whole-value replacement — `["admin","dev"]` then `["admin","ops"]` yields `["admin","ops"]`. |
| 15 | Can a union of two JSON arrays be computed in SQL? | **Yes**, via `json_each` + `UNION` + `json_group_array`. But it is additive-only and does not model removal. |

## Open questions these examples raise

1. **Which range scheme?** Site prefix (`site_id * 10000 + n`) is simpler; explicit
   allocation is more flexible. See § "Allocating locally, and never reusing".
2. **Is `groups` a group definition or a membership table?** One row per group (the
   schema as given) means `access_level` is the group default and `email` is a contact.
   Per-member permissions would need `PRIMARY KEY ("group", email)`.
3. **Are group names global or per-site?** With globally unique `userid`s, global is
   consistent. A per-site namespace would need `(site, name)` and a `site` column.
4. **Should the crate manage the allocator?** It could provide `IdAllocator` that owns the
   range table and the durable counter, so the "never reused" property is guaranteed by
   the crate rather than by convention.
5. **Should the crate offer a membership-table helper?** Given how common
   "entity has a set of things" is, a helper that materialises a JSON array into a child
   table would remove a recurring source of merge bugs.
6. **Who assigns ranges, and how?** The crate cannot prevent two nodes from concurrently
   creating sites with overlapping ranges. Central assignment plus local enforcement is
   the current answer; a crate-side allocator is a possible extension.
7. **Should `site_id` be an opaque id or a readable slug?** A slug is nicer to read and
   awkward to keep immutable; an opaque id is the reverse. The `sites` table is the
   application schema, so this is the application's choice — but the crate must not
   assume the slug is stable.

## Related documents

- [data-model.md](data-model.md) — key strategies, columns, the digest
- [merkle.md](merkle.md) — how the digest and tree hashes are computed
- [bulk-apply.md](bulk-apply.md) — applying many rows at once
- [sqlite-interface.md](sqlite-interface.md) — the trigger set
- [adr/0013-site-registry-and-range-allocation.md](adr/0013-site-registry-and-range-allocation.md) — the registry decision
- [adr/0019-the-allocation-registry-is-optional.md](adr/0019-the-allocation-registry-is-optional.md) — the registry is optional
- [../PLAN.md](../PLAN.md) — where the allocator and column-locality work lands
