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

    // ---- local identity ----
    pub node_id:  Uuid,
    pub site:     String,
    pub role:     Role,               // Master | Child | Client

    // ---- local policy ----
    pub hot_window:    Duration,      // 24h
    pub warm_window:   Duration,      // 90d
    pub purge_quiesce: Duration,      // 60s
    pub max_skew:      Duration,      // 1s
    pub write_mode:    WriteMode,     // Enforced | Compatible
    pub cache:         CacheConfig,
    pub tables:        Vec<TableSpec>,
}
```

## Must agree across peers

Some values cannot differ between peers, because a mismatch is a **correctness bug**
rather than a policy difference. These are checked at handshake and a mismatch is fatal.

| Field | Why it must match | Failure |
|---|---|---|
| `epoch_us` | Bucket boundaries. A mismatch means the trees are not comparable | `EPOCH_MISMATCH` |
| `digest_seed` | The digest is keyed. A mismatch means every row hash differs | `DIGEST_SEED_MISMATCH` |
| `retention` | Sets the purge horizon. Peers must agree on when a tombstone expires | rejected |
| `purge_period` | Derives the global purge instants | rejected |
| `schema_hash` | The shape of the synced data | rejected |

These are also the values most likely to live in one shared place in the consumer's
config — an environment variable, a mounted secret, a config-management system — which is
exactly right.

`epoch_us` deserves a note: it is an arbitrary instant, and its only job is to make
bucket boundaries agree. Any fixed value works. Midnight UTC on some epoch is a
conventional choice because it makes the boundaries line up with the calendar, but it is
not required. Once chosen, **it cannot change** — changing it re-buckets every row and
invalidates every stored hash.

## May differ

Everything else is local policy.

| Field | Default | Meaning |
|---|---|---|
| `hot_window` | 24 h | Ranges newer than this compare at hour depth |
| `warm_window` | 90 d | Ranges newer than this compare at cycle depth; older compare at month depth |
| `purge_quiesce` | 60 s | The window around each purge instant during which sync is refused |
| `max_skew` | 1 s | Refuse peers believed to be more than this far out of clock |
| `write_mode` | `Enforced` | Which trigger dialect is in use |
| `cache` | see below | Read-through cache limits |

`hot_window` and `warm_window` are local because they only affect **comparison depth**,
which is a performance choice. The bucket boundaries are absolute regardless.

`purge_quiesce` being local is safe: the quiesce window is a local refusal to serve
sync, not a shared instant. The *purge instant* is shared (it derives from `epoch_us` and
`purge_period`); the window around it need not be.

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
    pub client_visible: bool,
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
    SiteNamespaced { site_column: String, key_columns: Vec<String> },
    GlobalId { key_columns: Vec<String> },
    Surrogate { column: String },
    Rowid,
}
```

See [data-model.md](data-model.md) § "Key strategies" for the trade-offs.

### Columns

```rust
pub struct ColumnSpec {
    pub name:       String,
    pub replicated: bool,          // participates in sync and in the digest
    pub visibility: Visibility,    // Public | Restricted
}
```

A column that is not `replicated` is local-only: it is not synced, not hashed, and not
part of the digest.

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
}
```

See [security.md](security.md).

A child has exactly one upstream master, configured as `upstream: Option<Peer>`. A master
may have several peers: other masters for the mesh, children it serves.

## Where things live

| Item | Location |
|---|---|
| `SyncConfig` | The crate — serde struct with `Default` |
| `TableSpec` | The crate type, but the values are the consumer's |
| Peer list | The consumer's config, passed to the crate |
| TLS material | The consumer's, referenced by path or bytes |
| `epoch_us`, `digest_seed` | Shared across the deployment; the consumer distributes them |
| `retention`, `purge_period` | Shared across the deployment |

The crate provides the **types** and the **defaults**. It does not provide a config file
format that consumers are expected to adopt, because that is the one thing every consumer
already has an opinion about.

## A worked example

```toml
# eu-west-1 — a master
node_id  = "6f1e...-uuid"
site     = "eu-west"
role     = "master"
epoch_us = 1735689600000000          # same everywhere
retention = "30d"
purge_period = "24h"

[digest_seed]
source = "env:SYNC_DIGEST_SEED"      # the consumer's business, not ours

[[peers]]
node_id  = "a91b...-uuid"            # master B
name     = "eu-west-2"
role     = "master"
endpoint = "https://eu-west-2.internal:8443"
auth     = { token_env = "SYNC_TOKEN_EU_WEST_2" }

[[peers]]
node_id  = "c72d...-uuid"            # a child
name     = "eu-west-1-child-a"
role     = "child"
endpoint = "https://child-a.internal:8443"
auth     = { token_env = "SYNC_TOKEN_CHILD_A" }
```

## Related documents

- [data-model.md](data-model.md) — table and column specifications
- [security.md](security.md) — the peer list and the digest seed
- [lifecycle.md](lifecycle.md) — retention, purge period and the quiesce window
- [adr/0007](adr/0007-config-is-a-library-type.md)
