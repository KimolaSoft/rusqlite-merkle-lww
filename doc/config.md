# Configuration

## The shape of it

The crate does not impose a config format on its consumers. What it provides:

- a **`serde`-derived struct** with sane defaults;
- the ability to **override any field** from whatever config system the consumer already
  uses;
- **no opinion** about where the config comes from.

```rust
let config = SyncConfig {
    epoch_us: 1735689600000000,       // shared across the deployment
    ..Default::default()
};
```

The consumer wires it to `figment`, `config`, environment variables, or a handful of
`const` items. The crate ships a `from_toml`/`from_env` convenience only as an optional
feature, so it never becomes the reason someone adopts a config library.

See [ADR-0007](adr/0007-config-is-a-library-type.md).

## The full struct

```rust
pub struct SyncConfig {
    // ---- must agree across peers ----
    pub epoch_us:      i64,
    pub digest_seed:   [u8; 32],
    pub retention:     Duration,      // 30d
    pub purge_period:  Duration,      // 24h
    pub purge_offset_us: i64,         // offset within the period at which the purge instant falls
    pub visibility_hash: VisibilityHash,  // derived from `profiles`

    // ---- local identity ----
    pub node_id:  Uuid,               // issued by a master
    pub site:     String,
    pub role:     Role,               // Master | Child | Client

    // ---- credentials ----
    // The role string for this node's role. A master holds both; a child holds
    // only the child string. Absent for a client.
    pub credential: Option<SecretString>,

    // ---- visibility ----
    // The named projections, the role→profile mapping, and the peers this node
    // serves or talks to.
    pub profiles:     Vec<VisibilityProfile>,
    pub role_profile: HashMap<Role, String>,
    pub peers:        Vec<Peer>,

    // ---- local policy ----
    pub hot_window:    Duration,      // 24h
    pub warm_window:   Duration,      // 90d
    pub purge_quiesce: Duration,      // 60s
    pub max_skew:      Duration,      // 5s
    pub as_of_tolerance: Duration,    // 1s
    pub max_message_bytes: usize,     // 4 MiB
    pub max_rounds_per_session: u32,  // 1000
    pub max_tree_buckets: usize,      // 0 = no tripwire
    pub write_mode:    WriteMode,     // Enforced | Compatible
    pub audit:         bool,          // record attribution in _sync_audit
    pub cache:         CacheConfig,
    pub tables:        Vec<TableSpec>,
}
```

Three of these are **derived rather than configured**: `codec_version` is the crate's own
encoding version, `schema_hash` is computed from the registered tables, and
`visibility_hash` is computed from `profiles`. An application sets none of them
directly; the crate computes them and puts them on the wire. They are listed here
because a mismatch behaves exactly like a misconfigured shared value — it is detected at
handshake and it is fatal — and an operator reading an alert needs to know that the fix
is in the *registration* or the *profiles*, not in a shared configuration file.

### Secrets

Three values, and they are not interchangeable:

| Value | Scope | Config field |
|---|---|---|
| master string | shared by all masters | supplied as `credential` on a master |
| child string | shared by all children, known to all masters | supplied as `credential` on a child; held by a master for verification |
| digest seed | shared by every node that computes digests — masters **and** children | `digest_seed` |

**The crate persists none of them.** They are configuration, held in memory for the life
of the process. The crate does not implement a secret store and does not implement
rotation — both belong to the application.

A master needs to *verify* both role strings, so a master's configuration carries the
child string as well as its own; the `credential` field above is what the node
*presents*, and the verification side is separate.

## Must agree across peers

Some values cannot differ between peers, because a mismatch is a **correctness bug**
rather than a policy difference. These are checked at handshake and a mismatch is fatal.

| Field | Why it must match | Failure |
|---|---|---|
| `epoch_us` | Bucket boundaries. A mismatch means the trees are not comparable | `EPOCH_MISMATCH` |
| `digest_seed` | The digest is keyed. A mismatch means every row hash differs | `DIGEST_SEED_MISMATCH` |
| `retention` | Sets the purge horizon. Peers must agree on when a tombstone expires | `PURGE_SCHEDULE_MISMATCH` |
| `purge_period` | Derives the global purge instants | `PURGE_SCHEDULE_MISMATCH` |
| `purge_offset_us` | Where within the period each purge instant falls. A mismatch means peers purge at different moments | `PURGE_SCHEDULE_MISMATCH` |
| `codec_version` | The canonical encoding. A change re-digests every row | `CODEC_MISMATCH` |
| `schema_hash` | Exactly the inputs to the row digest — see below | `SCHEMA_MISMATCH` |
| `visibility_hash` | The visibility profile definitions — see below | `VISIBILITY_MISMATCH` |

**Three of these are derived, not configured.** `codec_version` is the crate's own
encoding version, `schema_hash` is computed from the registered tables, and
`visibility_hash` is computed from `profiles`. An application sets none of them; the
crate computes them and puts them on the wire. They are listed here because a mismatch
behaves exactly like a misconfigured shared value — it is detected at handshake and it
is fatal — and an operator reading an alert needs to know that the fix is in the
*registration* or the *profiles*, not in a shared configuration file.

### What `schema_hash` covers

This is the field that decides whether two nodes can converge at all, so it is defined
exactly rather than loosely. It is a hash of **precisely the inputs to the row digest**:

```
schema_hash = seeded_xxh3(digest_seed,
                  codec_version ‖
                  for each table, in registration order:
                    table name ‖
                    key strategy ‖ key columns ‖
                    ordered (column name, declared SQL type)
                      of every replicated column ‖
                    managed column names)
```

Two consequences:

- **A peer whose column registration order differs is rejected.** The digest concatenates
  values in registration order, so a different order is a different digest for the same
  logical row — permanent, silent divergence. The registration order is part of the
  schema, not an implementation detail.
- **Local columns are excluded.** They are site-local by definition, and they are excluded
  from the digest, so a peer legitimately has a different set of them.

The `codec_version` is part of the hash because a change to the canonical encoding changes
every digest. It is also surfaced separately, so the failure says "the encoding changed"
rather than "the schema changed".

These are also the values most likely to live in one shared place in the consumer's
config — an environment variable, a mounted secret, a config-management system — which is
exactly right.

### What `visibility_hash` covers

The **profile definitions**: for each profile, its name, whether it is trusted, and the
per-table column lists.

```
visibility_hash = seeded_xxh3(digest_seed,
                  for each profile, in registration order:
                    profile name ‖ trusted flag ‖
                    for each table, in registration order:
                      table name ‖ visible flag ‖ ordered column names)
```

It is **separate from `schema_hash`** on purpose. `schema_hash` decides whether two
nodes compute the same digest for the same logical row; `visibility_hash` decides
whether they project that row the same way. Folding them together would force a
read-only client — whose registration is necessarily narrower — to fail the digest
check.

A **client** session compares neither. The client names its profile, the server
resolves it to a projection and sends it, and the client applies it.

`epoch_us` deserves a note: it is an arbitrary instant, and its only job is to make
bucket boundaries agree. Any fixed value works. Midnight UTC on some epoch is a
conventional choice because it makes the boundaries line up with the calendar, but it is
not required. Once chosen, **it cannot change** — changing it re-buckets every row and
invalidates every stored hash.

`purge_offset_us` is the same kind of value: an **offset within `purge_period`** at which
the purge instant falls. It is deployment-wide, not local policy — peers that disagree
about it purge at different moments, which is a correctness bug. Expressing it as an
offset rather than a wall-clock time of day keeps it free of timezone and daylight-saving
ambiguity, and it composes with a `purge_period` that is not a whole number of days.

## May differ

Everything else is local policy.

| Field | Default | Meaning |
|---|---|---|
| `hot_window` | 24 h | Ranges newer than this compare at hour depth |
| `warm_window` | 90 d | Ranges newer than this compare at cycle depth; older compare at month depth |
| `purge_quiesce` | 60 s | The window around each purge instant during which the tables being purged refuse sync |
| `max_skew` | 5 s | Refuse a peer whose clock is more than this far out, with `CLOCK_SKEW` |
| `as_of_tolerance` | 1 s | How far the two sides' chosen comparison depths may differ before the client reports adopting the server's |
| `max_message_bytes` | 4 MiB | The soft budget one protocol message may occupy |
| `max_rounds_per_session` | 1000 | Round trips before a session ends with a retryable `SESSION_LIMIT` |
| `max_tree_buckets` | 0 (off) | A tripwire on the number of buckets in the tree. Zero disables it |
| `write_mode` | `Enforced` | Which trigger dialect is in use |
| `audit` | `false` | Record which peer applied which row, in `_sync_audit` |
| `cache` | see below | Read-through cache limits |

`hot_window` and `warm_window` are local because they only affect **comparison depth**,
which is a performance choice. The bucket boundaries are absolute regardless.

`purge_quiesce` being local is safe: the quiesce window is a local refusal to serve
sync, not a shared instant. The *purge instant* is shared (it derives from `epoch_us` and
`purge_period`); the window around it need not be. The window is **per table** — a node
with no tombstones due does not quiesce at all.

`max_skew` is 5 s because 1 s is tighter than the clock discipline a set of
geographically distributed sites can actually guarantee. Cross-site NTP agreement is
usually within a second or two; refusing peers at a one-second threshold rejects
sessions that would have been perfectly ordered. 5 s is still far below the granularity
that matters for LWW here, and it is a **local** threshold: two nodes may disagree about
it without breaking anything.

`as_of_tolerance` is a different knob and a different kind of check. `max_skew` is
about the **clock** — two nodes disagreeing about what time it is, which is refused
with `CLOCK_SKEW` because no negotiation fixes it. `as_of_tolerance` is about the
**comparison depth** — the two sides choosing how deep to compare — where the server's
choice simply wins and the client adopts it. There is nothing to fix and nothing to
refuse; the tolerance only decides when the client reports having adopted the server's
depth rather than quietly doing it.

`max_message_bytes` and `max_rounds_per_session` are local too. They bound what this
node is willing to send or receive in one message or one session; the peer answers with
its own budget, and the effective limit is the smaller of the two.

## Cache configuration

```rust
pub struct CacheConfig {
    pub window:       Duration,       // 24h — eligibility by last_update
    pub max_bytes:    usize,          // 256 MiB
    pub max_entries:  usize,          // 200_000
    pub time_to_idle: Option<Duration>, // 10 min
}
```

`window` is the eligibility rule: rows whose `last_update` is within it are eligible for
caching. These are the rows an application actually re-reads.

`max_bytes` and `max_entries` bound the cache. The byte bound is the one that matters —
a cache of "the last 24 h of changes" naturally bounds itself by time, but a single key
updated every second would otherwise grow without limit.

`time_to_idle` evicts entries not read recently, so the working set does not accumulate
stale rows.

Disabling the cache entirely is a valid configuration: `max_entries = 0`.

## Table specification

```rust
pub struct TableSpec {
    pub name:          String,
    pub kind:          TableKind,          // Keyed | AppendOnly
    pub key:           KeyStrategy,
    pub columns:       Vec<ColumnSpec>,
    pub merge:         MergePolicy,        // LastWriteWins | Custom(..)
    pub write_authority: WriteAuthority,   // Any | MasterOnly | OwnerOnly | Roles(..)
    pub sync_priority: u8,                 // lower syncs first
    pub allocation:    Option<AllocationSpec>,
}

pub enum TableKind {
    /// Rows are keyed and updated in place. The default.
    Keyed,
    /// Rows are only ever inserted. No updates, no deletes.
    AppendOnly,
}
```

`AppendOnly` is a convenience: it removes `status` and `purge_on` from the table, since
there is nothing to delete, and it removes the soft-delete machinery from the trigger set.

### Key strategies

```rust
pub enum KeyStrategy {
    /// The application's own column, already globally unique and stable.
    /// The default.
    GlobalId { key_columns: Vec<String> },

    /// A deterministic id derived by the crate from identity columns.
    Derived {
        identity_columns: Vec<String>,
        prefix: Option<String>,
        /// The UUIDv5 namespace. `None` derives it per table from the table name
        /// and `epoch_us` — stable within a deployment, different between them.
        namespace: Option<Uuid>,
    },

    /// Owner-namespaced: the row's site is part of the key.
    SiteNamespaced { site_column: String, key_columns: Vec<String> },

    /// A random surrogate. Never merges across sites.
    Surrogate { column: String },

    /// SQLite's implicit rowid.
    Rowid,
}
```

See [data-model.md](data-model.md) § "Key strategies" for the trade-offs.

### Columns

```rust
pub struct ColumnSpec {
    pub name:       String,
    pub r#type:     ColumnType,         // Null | Integer | Real | Text | Blob
    pub replicated: bool,          // participates in sync and in the digest
    pub visibility: Visibility,    // Public | Restricted
    pub local:      bool,          // meaningful only on the writing site
    pub merge:      MergeMode,     // LastWriteWins | SetUnion
}
```

A column that is not `replicated` is local-only: it is not synced, not hashed, and not
part of the digest.

**The declared type is the column's affinity, not a guarantee about the value.** SQLite
is dynamically typed, so a `text` column can hold an integer. The type is carried on
the wire so a receiver can decode a row's positional values, and it is part of
`schema_hash`, because the digest's input depends on the storage class of each value.
See [protocol.md](protocol.md) § "Message encoding" for the wire form.

A column marked `local: true` **is** replicated, but is never overwritten by a remote
write, and is **excluded from the digest** — otherwise two peers holding the same entity
with different local values would compute different digests and never converge.

**The registration order is load-bearing.** The digest concatenates the replicated values
in registration order, so two nodes that register the same columns in a different order
compute different digests for identical rows. The order is part of `schema_hash`.

### Visibility profiles

```rust
pub struct VisibilityProfile {
    pub name:    String,
    pub trusted: bool,              // may see `Restricted` columns
    pub tables:  Vec<ProfileTable>,
}

pub struct ProfileTable {
    pub table:   String,
    pub visible: bool,              // the table is excluded entirely when false
    pub columns: Vec<String>,       // the columns this profile may see
}
```

Profiles are mapped to roles by `role_profile`, and a specific peer may override the
mapping with its own `profile` field. A `Public` column is visible to every profile
**unless that profile narrows it** — a profile may hide a public column from a
particular role or peer. A `Restricted` column is visible only to `trusted` profiles.

### Merge policies and write authority

```rust
pub enum MergePolicy {
    LastWriteWins,                  // the default
    Custom(Arc<dyn MergePolicyFn>),
}

pub enum WriteAuthority {
    Any,                            // the default
    MasterOnly,
    OwnerOnly,
    Roles(Vec<Role>),
}
```

A custom merge policy must be **pure and convergent** — the same pair of rows must
produce the same decision on every peer. See
[data-model.md](data-model.md) § "Merge policies" for the rules, and
[adr/0015](adr/0015-per-table-merge-policies-and-write-authority.md) for the reasoning.

### Managed column names

The four crate-owned columns can be renamed per table, for tables whose existing schema
uses different names. The defaults are `last_update`, `status` and `purge_on`, plus a
key column (`sync_id`) when the application has no usable key of its own.

## Peers

The peer list is deployment-local configuration, and it is the trust boundary.

```rust
pub struct Peer {
    pub node_id:   Uuid,
    pub name:      String,
    pub role:      Role,
    pub endpoint:  String,
    pub auth:      PeerAuth,

    /// Overrides the role→profile mapping for this peer. `None` uses the mapping.
    pub profile:   Option<String>,
}
```

A peer may override the visibility profile the role mapping would give it — a
read-only TUI client that must not see a payload column gets its own profile, while
every other client keeps the default one.

See [security.md](security.md).

A child has exactly one upstream master, configured as `upstream: Option<Peer>`. A master
may have several peers: other masters for the mesh, children it serves.

### The directory is not config

The peer list above is the **static** part — the endpoints this node knows about. The
**UUID directory** is the dynamic part: every node's UUID, and who issued it.

| | Peer list | UUID directory |
|---|---|---|
| Contents | endpoints and roles | UUIDs only |
| Source | configuration | propagated master-to-master at handshake |
| Stored | config | `_sync_directory` (node-local) |
| Replicated | no | **no** |
| Given to children | their master's entry only | **never** — it is the child security boundary |

The two merge: a node is admissible if its UUID is in the directory **and** its endpoint
is in the peer list.

## Where things live

| Item | Location |
|---|---|
| `SyncConfig` | The crate — serde struct with `Default` |
| `TableSpec` | The crate type, but the values are the consumer's |
| Peer list | The consumer's config, passed to the crate |
| TLS material | The consumer's, referenced by path or bytes |
| `epoch_us`, `digest_seed` | Shared across the deployment; the consumer distributes them |
| `retention`, `purge_period`, `purge_offset_us` | Shared across the deployment |
| `profiles` | Shared across the deployment — a divergence is `VISIBILITY_MISMATCH` |
| master / child string | The consumer's; **never written to the database** |
| per-node secret | The consumer's; **never written to the database** |
| UUID directory | Node-local `_sync_directory`; merged from peers at handshake |
| session journal | Node-local `_sync_session`; a client's own resume state |
| attribution audit | Node-local `_sync_audit`; off by default, never replicated |

The crate provides the **types** and the **defaults**. It does not provide a config file
format that consumers are expected to adopt, because that is the one thing every consumer
already has an opinion about.

## A worked example

```toml
# eu-west-1 — a master
node_id  = "6f1e...-uuid"            # issued by a master
site     = "eu-west"
role     = "master"
epoch_us = 1735689600000000          # same everywhere
retention = "30d"
purge_period = "24h"
purge_offset_us = 0                    # same everywhere; the instant within the period

# The three secrets. The consumer's business, not ours — and never written
# to the database by the crate.
[digest_seed]
source = "env:SYNC_DIGEST_SEED"

[credentials.master]
source = "env:SYNC_MASTER_STRING"    # presented by this node

[credentials.verify]
child = "env:SYNC_CHILD_STRING"     # used to verify a child's session

[[peers]]
node_id  = "a91b...-uuid"            # master B
name     = "eu-west-2"
role     = "master"
endpoint = "https://eu-west-2.internal:8443"

[[peers]]
node_id  = "c72d...-uuid"            # a child, issued by this master
name     = "eu-west-1-child-a"
role     = "child"
endpoint = "https://child-a.internal:8443"
```

Note what is *not* here: a per-peer token. A master's credential is the master string,
a child's is the child string — the same string for every child, which is why the UUID
directory is what distinguishes them. The `peers` table carries only endpoints and roles;
membership comes from the directory, which is merged from other masters at handshake.

## Related documents

- [data-model.md](data-model.md) — table and column specifications
- [security.md](security.md) — the peer list and the digest seed
- [lifecycle.md](lifecycle.md) — retention, purge period and the quiesce window
- [adr/0007](adr/0007-config-is-a-library-type.md)
