# ADR-0013: A site registry in the main database; a row names the site that owns its identifier

- **Status:** accepted
- **Date:** 2026-11
- **Deciders:** project maintainers

## Context

The range-allotment scheme in [ADR-0012](0012-sync-key-is-the-applications-key.md) needs a
shared mapping from site to identifier range:

```
site 1 → 10000 … 19999
site 2 → 20000 … 29999
```

Until now that mapping has been a convention, described in prose and carried in each node's
configuration. Two things make it more than that.

**First, peers must know each other's ranges.** A node receiving a row with `userid = 20015`
cannot tell whether that is a legitimate identifier from site 2 or a collision with a local
row, unless it knows where site 2's range begins and ends.

**Second, the mapping carries more than ranges.** A site has a name, a location, and
whatever else the deployment wants to record about it. Those are ordinary data, they
change, and they should propagate like everything else in a shared dataset.

So the mapping wants to be a table, not a config file:

> Presumably the sites should be likewise captured in a ... site DB with site name, and
> uid_start or similar which could include other whatever data, idk, geographic city,
> country, whatever kind of thing.

The question is what kind of table, where it lives, and what the rest of the design does
with it.

## Decision

### 1. The registry is a replicated table in the main database

```sql
CREATE TABLE sites (
    site_id     TEXT    NOT NULL,      -- immutable, globally unique — THE SYNC KEY
    site_name   TEXT    NOT NULL,      -- mutable display name
    uid_start   INTEGER NOT NULL,      -- inclusive lower bound of this site's range
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

**It lives in the main database, not a separate file.** This is not a preference — it is a
SQLite constraint. Verified:

> `CREATE TRIGGER … WHEN NOT EXISTS (SELECT 1 FROM reg.sites …)` where `reg` is an
> `ATTACH`ed database → **`trigger users_range cannot reference objects in database reg`**

Triggers cannot reference objects in another database. Since the range check must be a
trigger (it is a value-level invariant), and the trigger must read the registry, **the
registry and the data must be in the same file**. A separate `sites.db` is possible only
if the range check is abandoned in favour of an application-level check — a strictly worse
guarantee.

**It is replicated**, like the other tables: a change to `city` propagates like any other
write. The registry is also part of the join bundle (see § 5).

### 2. `site_id` is the identity; `site_name` and the range are not

`site_id` is the sync key, and it is immutable.

`site_name` is **mutable**. A site may be renamed — "Leeds Clinic" becomes "Leeds Clinic
North" — and a rename must not be a move, because the range would have to move with it and
every identifier already minted under it would become invalid.

The range (`uid_start`, `uid_end`) is **immutable**. Verified: a `BEFORE UPDATE` trigger
rejecting a range change aborts it, while a descriptive update passes:

```sql
CREATE TRIGGER sites_range_immutable BEFORE UPDATE ON sites
WHEN NEW.uid_start <> OLD.uid_start OR NEW.uid_end <> OLD.uid_end
BEGIN
    SELECT RAISE(ABORT, 'site range is immutable');
END;
```

This makes the range a **fact about the deployment** rather than a value that can be
edited. The descriptive columns around it are ordinary LWW data.

A decommissioned site is soft-deleted (`status = 'deleted'`) and **its range is retired
forever** — never reissued, exactly like a retired identifier.

### 3. A row names the site that owns its identifier, not the site that last wrote it

Every table with an allocated identifier carries a crate-managed `site` column. It names
the **id owner** — the site whose range contains the identifier.

It does **not** name the writer. When site 2 edits a user created by site 1, the row's
`site` stays `site1` and only `last_update` moves. If `site` tracked the writer, the row's
owner would change under it and the identifier would fall outside the new owner's range —
the invariant below would fire on a perfectly legitimate edit.

So `site` is a function of the identifier, and it is stable for the life of the row. It is
replicated and included in the digest, because both peers must agree on it.

### 4. The range check is a trigger, and it checks what it can

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

| Insert | Result |
|---|---|
| `(userid=10001, site='site1')` — own range | accepted |
| `(userid=99999, site='site1')` — outside own range | **rejected** |
| `(userid=20001, site='site2')` — a peer's row | accepted |
| `(userid=30001, site='site3')` — site not yet in the registry | accepted, provisionally |

The condition fires only when the registry **knows** the row's site. That is deliberate:
a new site may be created on one node while another node is mid-sync, and rows for that
site may arrive before the registry row does. Rejecting them would deadlock the
conversation. The row is accepted, and § 5 covers how it gets re-validated.

The trigger fires on the `ON CONFLICT DO UPDATE` path as well, so the upsert is covered by
the same rule.

The `site` column is populated by the engine, not the application:

```sql
site TEXT NOT NULL DEFAULT (local_site())
```

Verified: a scalar function in a column `DEFAULT` is evaluated per insert, and an
explicit value overrides it. On a connection where the function is not registered the
insert **fails closed** — `no such function: local_site` — which is the Enforced dialect's
existing behaviour applied to one more column.

`ALTER TABLE … ADD COLUMN` rejects a non-constant default, so this form is only available
in `CREATE TABLE`. For a table that already exists, the crate offers the alternative
verified form:

```sql
CREATE TRIGGER users_site_default AFTER INSERT ON users
WHEN NEW.site IS NULL
BEGIN
    UPDATE users SET site = local_site() WHERE userid = NEW.userid;
END;
```

Verified: it fills a NULL `site` on insert. It does **not** fire on the
`ON CONFLICT DO UPDATE` path — which is correct, because that path leaves an existing
row's `site` alone, and an existing row already has one.

### 5. The registry is exchanged before the data, and is part of the join bundle

A node cannot validate an identifier it has no range for. So the registry is **applied
first** in every session, ahead of the ordinary tables.

A new node cannot validate anything until it has the registry, so the registry is also
part of the **join bundle** — alongside the epoch and the digest seed, which are already
deployment-wide state that a node cannot proceed without.

For a site created *during* a session, the registry row may not have reached the peer
before the site's data rows do. § 4 handles that by accepting rows whose owner is unknown.
The crate re-validates them when the registry catches up, and `validate` reports any that
remain inconsistent.

### 6. Ranges are assigned centrally, and overlap is prevented locally

Two nodes creating sites concurrently can each accept a range that overlaps the other's —
each check passes locally, because locally there is no conflict. The merge then produces
an overlap that no trigger re-checks.

So the allocation is **not** a distributed agreement problem to be solved by the crate. It
is an administrative act:

- Ranges are assigned by an operator or a small allocation service, and **seeded
  identically on every node** at join.
- The registry then replicates as ordinary data, carrying descriptive changes.

Local invariants are enforced by the engine:

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

Verified: an overlapping insert is rejected, a disjoint one accepted. Together with
`UNIQUE(uid_start)` and `CHECK (uid_start <= uid_end)`, a malformed allocation is refused
at the point of entry.

The alternative — deriving the range from the site id, so that uniqueness of the id
implies non-overlap — is available and simpler, but it fixes the range width and requires
a central site-id allocation of its own. See § Alternatives.

## Consequences

**Positive**

- **The deployment has one place that describes its sites**, and it is data, not config.
  Descriptive changes propagate with the rest of the dataset.
- **The range invariant is enforced by the engine**, on every write, including the upsert
  path — not by a convention the application must remember.
- **A peer's rows are accepted on the same terms as local ones.** The check is keyed on
  the row's own owner, so there is no "is this mine?" special case.
- **The `site` column makes ownership queryable.** "Show me site 1's users" is an indexed
  equality test, not a range join over the registry.
- **Ranges are retired forever** along with a decommissioned site, so a retired identifier
  can never be reissued.

**Negative**

- **Every table with allocated identifiers carries an extra column**, and the application
  must create it with the right default (or accept the backfill trigger).
- **The registry must be in the main database.** A separate file is not possible without
  giving up trigger-level enforcement.
- **The application cannot change a site's range.** Re-allocating a site's identifiers is
  a migration with no support in this design — it would invalidate every identifier
  already minted.
- **Sites are an administrative affair.** Creating a site involves assigning a range, and
  until that is centralised, the crate cannot promise that ranges never overlap across
  the deployment.

**Neutral**

- A row's `site` is not the writer's site, so "who last changed this row" is not answerable
  from the row. `last_update` carries the writer's clock but not its identity.
- The registry is one more table for the application to describe, and one more for the
  crate to sync. It is applied first in a session, which slightly delays the data phase.

## Alternatives considered

**A separate `sites.db` file, attached.** Rejected: triggers cannot reference objects in
an attached database, so the range check would have to move out of the engine and into the
application, which is exactly the guarantee being bought.

**Make the range part of the `site` column, rather than a separate registry.** Rejected:
the range must be immutable and shared, while `site` is a per-row replicated column. Mixing
them would put deployment state in every row.

**Track the writer's site rather than the id owner's.** Rejected: a legitimate edit by a
second site would move the row's owner and break the range invariant.

**Reject rows whose owner is unknown.** Rejected: a site created mid-session would deadlock
the conversation between the node that created it and every peer. Accepting provisionally
and re-validating later is strictly better.

**Derive the range from the site id** — `uid_start = site_id * width`. Not rejected; it is
the simpler alternative, and it makes overlap impossible by construction. It was not
chosen as the default because it fixes the range width and still requires a central
allocation of site ids. It remains a valid configuration: store `uid_start` and `uid_end`
explicitly and let the operator choose any scheme, including this one.

**Let the crate allocate ranges by consensus.** Rejected: it is a distributed agreement
problem, and the crate's scope is replication, not allocation. The requirement — that
identifiers never collide — is met by central allocation plus local enforcement.

## Related

- [../examples.md](../examples.md) — the `sites` table worked through
- [../data-model.md](../data-model.md) — the `site` column, managed columns, the digest
- [../sqlite-interface.md](../sqlite-interface.md) — the trigger set
- [../lifecycle.md](../lifecycle.md) — decommissioning a site, and retired ranges
- [ADR-0012](0012-sync-key-is-the-applications-key.md) — the sync key decision
