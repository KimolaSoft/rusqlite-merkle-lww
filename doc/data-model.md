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
| `purge_on` | TEXT | always | When the row may be physically removed. NULL while live. It is the engine's value, computed from the delete's window, so every tombstone in a window shares one deadline. See [lifecycle.md](lifecycle.md) |
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

**The allocation scheme is optional.** A deployment that already has an identifier
scheme — a central id service, a per-team leased range of its own — brings its own and
uses none of this. The crate's allocation machinery is an **optional feature** (`alloc`),
and it is used only when a table declares an `AllocationSpec`:

```rust
pub struct TableSpec {
    …
    /// Present when the identifiers are allocated from per-site ranges.
    /// Absent in a deployment that brings its own identifier scheme.
    pub allocation: Option<AllocationSpec>,
}
```

When no table declares one, there is no `sites` registry, no `site` column, no range
trigger, and no REGISTRY phase in the protocol. The sync core does not depend on any of
it. See [adr/0019](adr/0019-the-allocation-registry-is-optional.md).

See [examples.md](examples.md) § "Allocating locally, and never reusing" for the two
schemes and § "Example 3 — `sites`" for the registry, and
[adr/0019-the-allocation-registry-is-optional.md](adr/0019-the-allocation-registry-is-optional.md)
for the current decision.

### The `site` column

A table with globally unique identifiers carries a `site` column naming **the site that
owns the identifier** — the site whose range contains it. It exists only when the
crate's allocation scheme is in use.

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
sites are peers over a single logical dataset — then `GlobalId` (or `Derived`) is
correct. Two sites updating the same user converge on one row, and LWW resolves which
write wins. This is the expected deployment.

**If the sites own independent data** — multi-tenant, or separately operated — then
`SiteNamespaced` is correct, and `Derived` would be actively harmful: it would merge two
customers' records into one row and let LWW silently discard one of them.

**The default is `GlobalId`**, matching [ADR-0006](adr/0006-site-namespaced-keys.md)
and [ADR-0012](adr/0012-sync-key-is-the-applications-key.md): the application supplies
its own globally unique, stable column, and the crate adds no key column at all.
`Derived` is the fallback for applications that cannot offer one, and `SiteNamespaced`
is the deliberate opt-in for per-site data.

The failure modes are asymmetric, and the asymmetry is why the default is a **per-table
decision that gets reviewed**, not a silent one: being wrong with `GlobalId` loses data
irrecoverably, while being wrong with `SiteNamespaced` leaves two rows that can still be
reconciled deliberately. A deployment that turns out to be per-site can switch a table
to `SiteNamespaced`; a deployment that turns out to be shared cannot recover the rows a
`SiteNamespaced` merge destroyed.

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

    /// How two versions of a row are reconciled. The default is LWW.
    pub merge: MergePolicy,

    /// Which roles may write this table.
    pub write_authority: WriteAuthority,

    /// Sync order: lower numbers first. Core tables before large history tables.
    pub sync_priority: u8,

    /// Present when the identifiers are allocated from per-site ranges.
    /// Absent in a deployment that brings its own identifier scheme.
    pub allocation: Option<AllocationSpec>,
}
```

### Merge policies

`MergeMode` decides how two values of one **column** combine. `MergePolicy` decides how
two versions of one **row** combine — and for most tables that is the default:

```rust
pub enum MergePolicy {
    /// Last write wins on `(last_update, digest)`, per row. The default.
    LastWriteWins,

    /// The application decides. See below.
    Custom(Arc<dyn MergePolicyFn>),
}

pub trait MergePolicyFn: Send + Sync {
    /// Decide what to do with an incoming row.
    fn merge(&self, ctx: &MergeContext<'_>, incoming: &IncomingRow,
             local: Option<&StoredRow>) -> MergeDecision;

    /// Optional: repair a batch before it is replicated. Runs on the **sender**.
    fn prepare(&self, ctx: &MergeContext<'_>, batch: &mut Vec<Row>) -> Result<()>;
}

pub enum MergeDecision {
    /// Take the incoming row wholesale.
    TakeIncoming,
    /// Keep the local row.
    KeepLocal,
    /// A row synthesised from both — e.g. a field-wise merge.
    Merged(Row),
    /// The policy cannot decide this pair. The default LWW + digest tie-break applies.
    ///
    /// This is the escape hatch for a policy that meets a case it does not
    /// understand. Without it, a policy author has to invent a decision for every
    /// case, and inventing one is how a custom policy diverges.
    Undecided,
    /// Refuse the row, with a reason.
    Reject(RejectReason),
}

pub struct MergeContext<'a> {
    pub table:        &'a str,
    /// The node that wrote the row, not the node that sent it.
    pub origin_node:  Uuid,
    pub origin_role:  Role,
    pub origin_site:  &'a str,
    pub local_role:   Role,
    pub local_site:   &'a str,
}
```

The default is LWW because it is a **total order**: given the same pair of rows, every
peer reaches the same conclusion, with no further conditions. That property is what the
whole convergence argument rests on, so a custom policy has to preserve it.

**A custom policy must be pure and convergent.** The same `(incoming, local)` pair must
produce the same decision on every peer. A policy may not consult the wall clock, a
node-local counter, or anything else that differs between peers. "The newer wall-clock
wins" is the default rule; "the node that has seen this row more times wins" is a
divergence generator.

**Batch repair runs on the sender.** `prepare` is called before the rows are
replicated, not after they arrive. A repair applied on receipt changes the stored bytes,
and therefore the digest — so the two peers would compute different digests for the
same logical row, and the trees would never converge. A receiver-side repair is only
safe if it is a deterministic function of the replicated row set that every peer
computes identically, which in practice means doing it before the rows are sent.

The digest tie-break remains the fallback: a policy that returns `Undecided` falls
through to LWW on `(last_update, digest)`. The fallback covers a policy that **cannot
decide**; it does not cover one that decides wrongly. A non-convergent policy returns a
definite decision, so the tie-break never fires and the crate cannot detect it — which
is why the convergence suite runs with a custom policy in the loop
([testing.md](testing.md) § 19).

What the hook is for, concretely:

| Need | How |
|---|---|
| First-allocation-wins on a lease table | Compare an allocation instant column; the earlier allocation wins; break an exact tie on the digest |
| An epoch-ordered ownership field re-asserted after LWW | Take the LWW winner, then overwrite the ownership field from the row with the higher epoch |
| Batch repair before apply, to satisfy a partial unique index | `prepare`, on the sender, rewriting the conflicting column before the rows are sent |
| Reject a table's rows from a child | `merge` returns `Reject`, or the table declares `write_authority: MasterOnly` |

See [adr/0015](adr/0015-per-table-merge-policies-and-write-authority.md).

### Write authority

Not every table is writable by every role. A master-authoritative policy table accepts
writes from masters and refuses them from children; a site-owned table accepts them only
from the site that owns the rows.

```rust
pub enum WriteAuthority {
    /// Any trusted peer may write. The default.
    Any,
    /// Only masters may write.
    MasterOnly,
    /// Only the site's own rows may be written by that site.
    OwnerOnly,
    /// Only the listed roles may write.
    Roles(Vec<Role>),
}
```

The check is made **when the row is applied**, against the **origin** of the row — the
node that wrote it — and not against the node that forwarded it. A master's rows travel
master → master → child, and the child's authority must still be evaluated against the
master that wrote them. This is why every ROWS batch carries its origin; see
[protocol.md](protocol.md) § "Write authority".

A row whose origin lacks authority for the table is refused with
`WRITE_NOT_PERMITTED`, and the table is dropped from the session.

**A peer may write a table only if its profile exposes every replicated column of that
table.** A peer that cannot see a column cannot compute the row's digest, and a row it
cannot digest is a row it must not write. Write authority and visibility are therefore
related, and the crate checks both.

### Visibility profiles

The visibility model has more than two classes in practice. A deployment typically needs
a column that masters exchange with each other but children never see; a large payload
column that is withheld from read-only clients; and whole tables that are excluded from
one role and included in another.

A **visibility profile** is a named projection of the schema — which tables a class of
peer may see, and which columns of them:

```rust
pub struct VisibilityProfile {
    pub name: String,

    /// Trusted profiles may see `Restricted` columns; untrusted ones may not.
    pub trusted: bool,

    pub tables: Vec<ProfileTable>,
}

pub struct ProfileTable {
    pub table: String,
    /// The table is not part of this profile at all.
    pub visible: bool,
    /// The columns of the table this profile may see. Empty means none.
    pub columns: Vec<String>,
}
```

Profiles are **mapped to roles**, and a specific peer may override the mapping:

```rust
SyncConfig {
    profiles: vec![master_profile, child_profile, client_profile],
    role_profile: { Role::Master => "master", Role::Child => "child",
                    Role::Client => "client" },
    peers: vec![ Peer { …, profile: Some("tui-client".into()) } ],
}
```

The `visibility` classification on a column (`Public` / `Restricted`) still says
something useful: it is the **default** projection. A `Public` column is visible to
every profile; a `Restricted` column is visible only to profiles marked `trusted`. A
profile may narrow that further — it may hide a `Public` column from one role, or expose
a `Restricted` column to one — but it may never expose a `Restricted` column to an
untrusted profile.

| Peer class | Public columns | Restricted columns | May write |
|---|---|---|---|
| **Trusted** (master, child) | yes | yes | yes, subject to `write_authority` |
| **Client** (read-only) | yes, per its profile | **no** | no |

A table is excluded from a profile by setting `visible = false` on its `ProfileTable`.
There is no separate `client_visible` flag: the profile is the single source of truth
for what a class of peer sees, so the two cannot disagree.

### The two hashes

| Hash | Covers | Compared between |
|---|---|---|
| `schema_hash` | **exactly the inputs to the row digest** — key strategy, key columns, replicated column names and types in registration order, managed column names, `codec_version` | Trusted peers — fatal on mismatch |
| `visibility_hash` | The **profile definitions** — profile names, trusted flags, per-table column lists | Trusted peers — fatal on mismatch |

They are separate on purpose. `schema_hash` decides whether two nodes can compute the
same digest for the same logical row; `visibility_hash` decides whether they project
that row the same way. Mixing them would force a client — whose registration is
necessarily narrower — to fail the digest check.

A **client** session does not compare either hash. The client names its profile in the
handshake, the server resolves it to a **projection** (the table → column list the
profile resolves to) and sends it, and the client applies that projection. A client that
does not know the profile definition gets the projection as it arrives; a client that
does know it can compare the projection against its own expectation and log a warning on
a difference. It never fails the session — see
[protocol.md](protocol.md) § "The projection".

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
[adr/0020-application-authenticated-peer-identity-and-optional-per-node-credentials.md](adr/0020-application-authenticated-peer-identity-and-optional-per-node-credentials.md).

```sql
CREATE TABLE _sync_log (
    tbl          TEXT    NOT NULL,
    k            BLOB    NOT NULL,   -- canonical encoded sync key
    last_update_us INTEGER NOT NULL, -- unix micros; == the row's last_update
    purge_on_us  INTEGER,            -- unix micros; NULL while the row is live
    digest       BLOB    NOT NULL,   -- 16 bytes
    deleted      INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (tbl, k)
) WITHOUT ROWID;

CREATE INDEX _sync_log_last_update ON _sync_log (tbl, last_update_us);
CREATE INDEX _sync_log_purge ON _sync_log (tbl, purge_on_us)
    WHERE purge_on_us IS NOT NULL;
```

`_sync_log` is a **per-key register**: writing a key overwrites its row. Old versions are
not retained. That is what makes it a per-key LWW register rather than a set of
operations, and it is why the log does not grow without bound.

### Why the log carries its own timestamp

`last_update_us` is **the same instant as the row's `last_update`** — not a second
clock, and not a second source of truth. The trigger writes both from one value, so
they cannot disagree.

The log carries it because the log is **shared across every managed table** and
deliberately stores no payload. Three things need the timestamp as an integer number of
microseconds rather than as RFC 3339 text:

1. **Bucket assignment.** The Merkle hour bucket is integer division of microseconds by
   the epoch. The tree is rebuilt in one indexed pass over `_sync_log` and **never reads
   a payload column** — see [merkle.md](merkle.md).
2. **"What changed since X".** The watermark comparisons are integer range scans.
3. **The digest tie-break.** Comparing `(last_update, digest)` needs the timestamp
   beside the digest.

Without it, the tree rebuild would have to join the log back to every managed table and
**re-encode each key** — `_sync_log.k` is a canonical BLOB, while the user table's key
is the raw column or columns — which is not expressible in SQL for composite, derived
or site-namespaced keys. It would become a full decode of every row at startup.

`purge_on_us` is the same idea for the tombstone deadline: it is `NULL` while the row
is live, and it makes "which tables have tombstones due at instant T" an indexed
existence query instead of a scan. Because `purge_on` is uniform per window, the purge
is an equality predicate on it. See [protocol.md](protocol.md) § "The purge quiesce
window".

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
-- The compatible dialect's drainer staging: rows written by a connection the crate
-- did not instrument, waiting for the drainer to compute their digests.
CREATE TABLE _sync_staging (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    tbl       TEXT NOT NULL,
    op        TEXT NOT NULL,
    payload   BLOB NOT NULL
);
```

```sql
-- Durable staging for received pages. Node-local, NOT replicated.
-- Both dialects: a client (or any peer resuming a durable session) stages a page
-- here before applying it, so a crash between "received" and "applied" loses
-- nothing. The cursor advances only after the applying transaction commits.
CREATE TABLE _sync_page (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT    NOT NULL,
    tbl        TEXT    NOT NULL,
    page_seq   INTEGER NOT NULL,
    payload    BLOB    NOT NULL,   -- the encoded page, exactly as received
    applied_us INTEGER             -- NULL until the page has been applied
);
```

```sql
-- The client's session journal. Node-local, NOT replicated.
-- A read-only client persists its session here so it can resume after a restart.
CREATE TABLE _sync_session (
    session_id TEXT    PRIMARY KEY,
    peer_id    TEXT    NOT NULL,
    profile    TEXT    NOT NULL,
    state      TEXT    NOT NULL,   -- active | paused | complete
    cursor     BLOB,               -- the last applied page cursor
    as_of_us   INTEGER,
    created_us INTEGER NOT NULL,
    updated_us INTEGER NOT NULL
);
```

```sql
-- Optional attribution audit. Node-local, NOT replicated, NOT in the digest.
-- Records which peer applied which row, on this node.
CREATE TABLE _sync_audit (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    tbl        TEXT    NOT NULL,
    k          BLOB    NOT NULL,
    peer_id    TEXT    NOT NULL,   -- the node that wrote the row
    applied_us INTEGER NOT NULL
);
```

The audit table is **off by default**. It is enabled per node, it is never replicated,
and it is not part of the digest — so turning it on cannot affect convergence. It
answers "which peer wrote this row, as far as this node knows", which is the honest
form of attribution: a node can only vouch for the rows it applied itself. See
[security.md](security.md) § "Attribution".

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

**The input is the full replicated column set, whatever the receiver may see.** A
visibility profile decides what is *sent*, never what is *hashed*. A peer whose profile
hides a replicated column therefore cannot compute that table's digest — it receives the
digest verbatim and never recomputes it, the same treatment a client gets for not
holding the seed. Two consequences follow, and both are wanted:

- **Peers with different views agree on the digest.** A child that cannot see a
  credential column and a master that can still agree on the digest of the same row.
- **The tree is unaffected.** `row_hash` is a function of the digest, not of the
  payload, so a peer holding a partial row still computes the same `row_hash` and its
  tree still matches.

The peer that *may* write a table is the one whose profile exposes every replicated
column of it, because a row it cannot digest is a row it cannot produce — see
[ADR-0015](adr/0015-per-table-merge-policies-and-write-authority.md) § 5.

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
