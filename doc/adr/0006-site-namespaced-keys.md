# ADR-0006: Site-namespaced keys, with `GlobalId` as the default for shared datasets

- **Status:** accepted
- **Date:** 2026-11
- **Deciders:** project maintainers

## Context

The deployment is multi-master: several sites, each with a master, replicating to each
other. This creates a write-conflict problem that LWW handles poorly — the case where
two nodes write the same logical entity concurrently, and each write invalidates the
other's work.

The requirement was:

> we would ... to avoid high-multi-writer tables or have some kind of site keyword added
> to ensure uniqueness.

and, on the ownership question:

> A row with (site, id) would mean site was the 'owner' at creation but if modified by
> another site the 'owner' doesn't change even if some data object does.

The key question is what "uniqueness" means here. Two readings:

- **Collision avoidance** — two sites must not write to the same row. Adding the site to
  the key achieves this.
- **Entity merging** — the same logical entity created independently at two sites should
  end up as one row. Adding the site to the key does *not* achieve this; it deliberately
  prevents it.

Initially this ADR chose site-namespacing as the default, on the reasoning that it is
safe under every interpretation. That was revised once the deployment's actual shape was
clarified:

> For most cases, I think sites would be sharing one dataset, so the same user would be
> the same user

That is the **entity-merging** case. The default has changed accordingly.

## Decision

**The site is part of the key only when the sites own independent data.**

Four strategies are supported, selected per table:

```rust
pub enum KeyStrategy {
    SiteNamespaced { site_column: String, key_columns: Vec<String> },
    GlobalId { key_columns: Vec<String> },
    Surrogate { column: String },
    Rowid,
}
```

**The default is `GlobalId`**, because the expected deployment is a shared dataset where
the same user is the same user everywhere.

`SiteNamespaced` remains available, and is the correct choice for tables that are
genuinely per-site — multi-tenant data, or separately operated datasets where merging
would be actively harmful.

### The id must be deterministic

`GlobalId` only merges if **both sites derive the same id** for the same entity. A
randomly generated globally-unique id (UUIDv7, ULID) does not merge anything; it merely
avoids collisions.

For "the same user is the same user" to hold, the id must be:

- derived deterministically from a natural key (`uuidv5(namespace, "user:alice")`), or
- assigned by something upstream that already agrees across sites.

This is stated prominently because it is the most common way a `GlobalId` deployment
fails to behave as intended.

## Consequences

**Positive**

- **The shared-dataset case works as intended.** Two sites updating the same user
  converge on one row, and LWW resolves which write wins.
- **The per-site case remains expressible.** A table of per-site data can use
  `SiteNamespaced` without affecting the tables that should merge.
- **The site becomes ordinary data** under `GlobalId` — a mutable column that records
  where a row was last written, which is useful on its own.
- **A single primary key strategy covers most deployments**, so the default rarely needs
  to be overridden.

**Negative**

- **`GlobalId` requires a deterministic id**, which is a real constraint on the
  application. Randomly generated ids must be replaced with derived ones, or the
  deployment behaves differently from what it looks like.
- **The two failure modes are asymmetric.** With `GlobalId` on independent data, two
  unrelated records merge and one is silently lost to LWW — not recoverable. With
  `SiteNamespaced` on shared data, two rows exist that should be one — recoverable by
  deliberate reconciliation. The default follows the *expected* deployment rather than
  the *safest* one, and that is a deliberate choice.
- **`SiteNamespaced` users must address the full key** when updating a row created at
  another site — the namespace is part of the key.

**Neutral**

- The namespace column is part of the primary key under `SiteNamespaced`, so it is
  included in the digest and in the canonical key encoding. Two rows differing only in
  site are genuinely different rows and hash differently.
- The `site` column remains meaningful under `GlobalId` as an ordinary replicated
  attribute.

## Alternatives considered

**Site-namespacing as the default.** Initially chosen, and still the *safest* default,
because it cannot merge unrelated records. Revised once it was clear that the sites
share a dataset. It remains available per table.

**No namespace at all — plain application keys.** Rejected: two sites writing the same
id contend directly, which is the high-multi-writer case LWW handles worst. This is
exactly the problem the strategies exist to solve.

**A globally unique id generated locally.** Supported as `GlobalId`, but rejected as a
*complete* answer without the determinism caveat: locally generated ids do not merge
independently-created entities. They avoid collisions, which is a different property.

**A surrogate key generated by the crate.** Supported as `Surrogate`. Rejected as the
default because it adds a column the application did not ask for, and because a random
surrogate does not merge across sites either — it is `SiteNamespaced` with extra steps.

**Allow the site to change on a move.** Rejected: it would mean a key change, which is
already handled by `move_key`. There is no reason to make it a special case.

## Related

- [../data-model.md](../data-model.md) — key strategies and the canonical key encoding
- [../architecture.md](../architecture.md) — the conflict domain
- [../merkle.md](../merkle.md) — how the namespace enters the digest
