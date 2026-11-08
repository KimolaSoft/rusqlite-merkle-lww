# ADR-0012: The sync key is the application's key when it is globally unique and stable

- **Status:** accepted
- **Date:** 2026-11
- **Deciders:** project maintainers

## Context

The crate syncs tables the application already owns. Those tables have their own primary
keys, chosen by the application for its own reasons.

A worked example (`doc/examples.md`) makes the problem concrete. A `users` table:

```sql
CREATE TABLE users (
    userid   INTEGER PRIMARY KEY,
    username TEXT NOT NULL,
    "Groups" TEXT NOT NULL,     -- a JSON array of group names
    email    TEXT NOT NULL
);
```

Read initially as a *local* integer sequence, `userid` looked unusable as a sync key:
two sites each minting their own sequence both have `userid = 1`, for different people.

The application's own description of the schema resolved it:

> userid would be unique globally and would not be able to be reused, while userid would
> still be unique, it cannot be reused which means it cannot auto increment. So perhaps
> the example needs an allotment system for a range of id's per site? Perhaps based on
> something like 10_000 = site1, 11_000 = site2, etc. userids can never overlap. emails can
> change due to spelling mistakes, marriage, etc, but a userid would never change.

So `userid` is **globally unique by construction**, allocated from a range reserved per
site, and **never reused**. It is a permanent reference to a person.

## Decision

**The sync key is the application's own primary key, when that key is globally unique and
stable.**

The crate manages no key column in that case. The application's `userid` *is* the sync
key, the primary key *is* the conflict target, and the upsert is:

```sql
INSERT INTO users (userid, username, "Groups", email, last_update, status, purge_on)
VALUES (?,?,?,?,?,?,?)
ON CONFLICT (userid) DO UPDATE SET …
WHERE excluded.last_update > users.last_update;
```

Verified: the conflict target on the primary key works, and the `WHERE` clause keeps the
newer row.

### When the application's key does not qualify

Some applications cannot offer a globally unique, stable key. Then the crate manages a
separate column:

```
sync_id = uuidv5(namespace, prefix + identity_columns)
```

and the application's primary key is untouched, remaining its own business.

The rule is: **use the application's own key if it is already global and stable;
otherwise derive one.**

| The app's key | Sync key |
|---|---|
| A globally unique, never-reused identifier | **The app's own column.** No extra column. |
| A stable, globally unique natural key | **The app's own column.** No extra column. |
| A locally generated UUID | The app's own column — but it merges nothing |
| A local integer sequence | **Derived.** The app's key collides across sites. |
| A mutable natural key (email) | **Derived**, and a change becomes a move |

### The derivation input must be stable

If a derived key is needed, the identity columns it derives from must not change. If they
do, the sync key changes, and that is a **move** rather than an update.

| Derivation | On an email change | Merge identity |
|---|---|---|
| `sync_id` from `email` | A move, automatically | The current email |
| `sync_id` a stable surrogate | A plain update | The original identity, forever |

Both are supported. The stable surrogate is usually better — identity should not depend
on a mutable attribute — but it requires the application to have, or accept, an immutable
id. The table spec chooses.

### Globally unique identifiers need an allocation scheme

A globally unique, never-reused identifier is not free. A site needs a reserved range or
a prefix, and the scheme must be shared across the deployment so peers can validate the
identifiers they receive.

Two schemes, both verified:

- **Site prefix** — `userid = site_id * 10000 + local_counter`. Derived, not allocated;
  a site needs only its own `site_id`.
- **Explicit allocation** — a registry of `(site, lo, hi)` ranges, minted from the range.

The allocation must be **durable**, and allocation must happen **before** use. A gap is
harmless; a reuse is corruption.

The scheme can be enforced by the engine. Verified: a `BEFORE INSERT` trigger that checks
the identifier against the range belonging to the row's *own* site rejects a bad mint and
accepts a peer's rows on the same terms. It fires on the `ON CONFLICT DO UPDATE` path
too.

Where the ranges live, and what the row's `site` column means, is settled in
[ADR-0013](0013-site-registry-and-range-allocation.md).

### Local columns

A local sequence number is meaningful only within the site that wrote it. A remote write
must not overwrite it. This table has no local columns — `userid` is global — but the
concept applies wherever a table mixes global and local state, and it has a hard
requirement:

**Local columns are excluded from the digest.** Otherwise two peers holding the same
entity with different local values would compute different digests, every bucket would
differ, and nothing would ever converge.

## Consequences

**Positive**

- **The application keeps its own primary key, and usually needs no extra column.** The
  common case — a globally unique id — involves nothing added to the schema.
- **The conflict target is the primary key.** The upsert is the obvious form, and the
  engine's own uniqueness enforcement does the arbitration.
- **Local sequences still work.** Where a table does have local columns, the merged row
  keeps the local site's value for them.
- **A deterministic derivation remains available** for applications whose key is local or
  mutable, with no coordination required.

**Negative**

- **The application must be able to offer a globally unique key**, or accept the derived
  column. An application with a local integer sequence has to change how it allocates, or
  accept a derived key.
- **The allocation scheme is deployment-wide state**, like the epoch and the digest seed.
  Peers need it to validate the identifiers they receive.
- **A derived key has two id columns**, and they are not the same thing. That is a source
  of confusion and must be documented clearly.
- **A derived key's identity columns must be chosen deliberately.** Deriving from a
  mutable attribute turns ordinary updates into moves.

**Neutral**

- The `site` column is a **crate-managed** column on tables with allocated identifiers,
  naming the site that owns the identifier. It is what makes the range check possible.
  See [ADR-0013](0013-site-registry-and-range-allocation.md).
- The `Surrogate` strategy remains available for tables that genuinely must not merge.

## Alternatives considered

**Always use a separate crate-managed `sync_id` column.** This was the previous revision
of this ADR. It is correct, but it adds a column to every synced table when the
application already has a perfectly good globally unique key. The rule is now conditional
rather than universal.

**Require every application to adopt a crate-provided key type.** Rejected: it dictates
the application's schema rather than accommodating it.

**Derive `sync_id` in SQL as a generated column.** Verified to work
(`GENERATED ALWAYS AS (expr) STORED` with a `UNIQUE` constraint is usable as a conflict
target), but `uuidv5` is not available in SQLite, so the derivation would be limited to
expressions SQLite can evaluate — which excludes the hashing the format wants. The crate
computes `sync_id` in Rust.

**Use a random UUID as the sync key and accept no merging.** Rejected as the default: it
is `Surrogate`, and it does not satisfy "the same user is the same user". It remains
available per table.

**Composite sync key of (namespace, application key).** This is `SiteNamespaced`, and it
remains available for genuinely per-site tables.

## Related

- [../examples.md](../examples.md) — the worked examples this decision came from
- [../data-model.md](../data-model.md) — key strategies, local columns, the digest
- [../protocol.md](../protocol.md) — the merge rule
- [ADR-0006](0006-site-namespaced-keys.md) — the choice of default key strategy
