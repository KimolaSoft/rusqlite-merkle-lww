# ADR-0012: The sync key is separate from the application's primary key

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
    "Groups" TEXT NOT NULL,     -- a JSON array
    email    TEXT NOT NULL
);
```

Two sites each minting their own `userid` sequence will both have `userid = 1`, for
**different people**. So `userid` cannot be the sync key:

| Key | eu-west | tokyo | Result |
|---|---|---|---|
| `GlobalId(userid)` | 1 = Alice | 1 = Bob | **merged into one row.** Two people become one. |
| `SiteNamespaced(userid)` | (eu-west, 1) | (tokyo, 1) | Two rows. Never merge — which contradicts "the same user is the same user". |

Neither is acceptable for a shared dataset. The identity that *is* global in this schema
is `email`.

The same problem appears with a UUID generated locally at insert: unique, yes, but
different at each site, so it merges nothing.

## Decision

**The crate manages a `sync_id` column, separate from the application's primary key.**

```
sync_id = uuidv5(namespace, prefix + identity_columns)
```

The application's primary key is untouched and remains its own business — a local
sequence, a UUID, a natural key, whatever it likes.

### The identity columns must be stable

The derivation input is the entity's identity, so it must not change. If it does, the
sync key changes, and that is a **move** rather than an update.

| Derivation | On an email change | Merge identity |
|---|---|---|
| `sync_id` from `email` | A move, automatically | The current email |
| `sync_id` a stable surrogate | A plain update | The original identity, forever |

Both are supported. The stable surrogate is usually better — identity should not depend
on a mutable attribute — but it requires the application to have, or accept, an immutable
id. The table spec chooses.

### Local columns

A local sequence number is meaningful only within the site that wrote it. A remote write
must not overwrite it. Verified against SQLite:

```
remote (sync_id=sid-a, userid=77) vs local (sync_id=sid-a, userid=1)
  → the upsert fires on sync_id; the local userid=1 is kept ✓
remote (sync_id=sid-b, userid=77) vs local (sync_id=sid-a, userid=1)
  → succeeds; 77 does not collide with 1 ✓
remote (sync_id=sid-b, userid=1)  vs local (sync_id=sid-a, userid=1)
  → UNIQUE constraint failed: users.userid  ✗
```

So the application generally should **not** put a `UNIQUE` constraint on a local
sequence: the constraint holds within one site but not across the merged dataset.

Local columns are marked `local: true` in the column spec, are never overwritten by a
remote write, and are **excluded from the digest**.

### Key strategies

| Strategy | Derivation | Merges across sites? |
|---|---|---|
| `Derived` | `uuidv5(namespace, prefix + identity columns)` | **yes** |
| `GlobalId` | the application's own globally unique column | **yes** |
| `SiteNamespaced` | `(site, key columns)` | no |
| `Surrogate` | a random UUIDv7 | no |
| `Rowid` | the rowid | no |

`Derived` is the default, since the expected deployment is a shared dataset.

## Consequences

**Positive**

- **The application keeps its own primary key.** No schema upheaval, no "you must use our
  key type" requirement.
- **Local sequences work.** The merged row keeps the local site's `userid`, so the
  application's own queries and sequences are unaffected.
- **The merge identity is explicit.** It is a column the crate owns and documents, not
  something inferred from whatever the application happened to choose.
- **A deterministic derivation means the same entity converges without coordination** —
  no id allocator, no registry.

**Negative**

- **Two id columns.** The application has its PK and the crate has `sync_id`, and they
  are not the same thing. That is a source of confusion, and the crate must document it
  clearly.
- **The identity columns must be chosen deliberately.** Deriving from a mutable attribute
  turns ordinary updates into moves.
- **The derivation namespace must be stable.** Changing it changes every `sync_id` in the
  deployment, which is a re-key of the whole dataset.
- **The application may need to drop a `UNIQUE` constraint** on a local sequence column,
  or accept that a remote insert can fail on it.

**Neutral**

- `sync_id` is a `TEXT` column holding a UUID. It could be a BLOB holding the raw 16
  bytes; the choice is the crate's, and it does not affect the protocol.

## Alternatives considered

**Use the application's primary key as the sync key.** Rejected: it only works when the
application's key is already globally unique and stable, which the example's `userid`
demonstrably is not. It also pushes the problem onto every application rather than
handling it in the crate.

**Require every application to use a crate-provided key type.** Rejected: it is the same
problem with a worse developer experience — the crate would dictate the application's
schema rather than accommodating it.

**Derive `sync_id` in SQL as a generated column.** Verified to work
(`GENERATED ALWAYS AS (expr) STORED` with a `UNIQUE` constraint is usable as a conflict
target), but `uuidv5` is not available in SQLite, so the derivation would be limited to
expressions SQLite can evaluate — which excludes the hashing the format wants. The crate
computes `sync_id` in Rust.

**Use a random UUID as the sync key and accept no merging.** Rejected for the default: it
is `Surrogate`, it does not satisfy "the same user is the same user", and it is available
per table for tables that genuinely do not merge.

**Composite sync key of (namespace, application key).** This is `SiteNamespaced`, and it
remains available for genuinely per-site tables.

## Related

- [../examples.md](../examples.md) — the worked examples this decision came from
- [../data-model.md](../data-model.md) — key strategies, local columns, the digest
- [../protocol.md](../protocol.md) — the merge rule and how local columns are kept
- [ADR-0006](0006-site-namespaced-keys.md) — the choice of default key strategy
