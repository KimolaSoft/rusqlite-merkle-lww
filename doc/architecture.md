# Architecture

## What the system does

Several SQLite databases, spread across machines and sites, are kept in sync. Each holds
a set of rows; rows are replicated to peers; conflicts are resolved by last-write-wins on
a timestamp column; deletions propagate; and the set of rows that differ between two
peers is computed with a Merkle tree rather than by comparing row sets.

The system is a library, not a service. It provides:

- a **store** — the local SQLite database, its managed tables, and the sync metadata;
- a **sync engine** — the diff and merge logic, transport-agnostic;
- a **server** — Axum handlers exposing the protocol over HTTP and WebSocket;
- a **client** — the same engine, driven from the other side.

## Roles and topology

```
                    ┌──────────────┐
                    │ master (A)   │◄──────────► master (B)     bidirectional
                    └──┬────┬───┬──┘
          pull-only   │    │   │   bidirectional
             ┌─────────┘    │   └──────────┐
             ▼              ▼              ▼
        child (a)      child (b)      child (c)        each child ⇄ one master
             │
             ▼ pull-only
        read-only client
```

| Role | Serves peers | Accepts pushes | Local writes | Purpose |
|---|---|---|---|---|
| **master** | children, other masters | yes | yes | Hub for its site's children; a peer in the master mesh |
| **child** | nobody | yes | yes | Exactly one upstream master |
| **client** | nobody | no | no | A read-only replica; safe to rebuild from scratch |

"Pull-only" is a protocol capability, not a separate implementation. A client runs the
same session logic as a master but never runs the upload phase (§ protocol.md).

### Why this shape

A single fully-connected mesh would be simpler but has two problems. Every node would
need to maintain a session with every other node, and — more seriously — every node
would be a potential writer for every table, which is the case LWW handles worst.

The hierarchy reduces both: masters talk to masters, children talk to one master, and
clients talk to one master. The number of connections per node is proportional to its
site, not to the size of the deployment.

Read-only clients are separated from children because they need none of the write
machinery, and because they are the case where a wrong answer is most visible.

### Multi-master and the conflict domain

Masters in different sites replicate to each other, which means two sites can write to
the same table. The design makes that safe by making the **site part of the key**: a row
authored at `eu-west` is keyed `(eu-west, id)`, and a row authored at `tokyo` with the
same `id` is a *different row*. The two never contend.

The consequence, stated plainly: **the same logical entity created independently at two
sites does not merge.** It becomes two rows. That is deliberate — see
[ADR-0006](adr/0006-site-namespaced-keys.md). If a deployment needs cross-site merging of
independently-created entities, the `GlobalId` key strategy is the alternative, and it
requires globally unique identifiers.

## Components

```
┌─────────────────────────────────────────────────────────────┐
│  Application code                                            │
│    reads/writes plain SQL on a pooled connection             │
└───────────────────────────┬─────────────────────────────────┘
                            │ rusqlite::Connection
┌───────────────────────────▼─────────────────────────────────┐
│  merkle-lww-core                                            │
│                                                             │
│   store ──────── managed tables, _sync_log, triggers         │
│   codec ─────── canonical encoding, digest                  │
│   merkle ────── bucket tree, snapshot                       │
│   diff ──────── comparison, merge                            │
│   sync ──────── session state machine                       │
│   time ──────── timestamps, monotonic issuance               │
└───────────────────────────┬─────────────────────────────────┘
                            │ trait boundaries
┌───────────────────────────▼─────────────────────────────────┐
│  merkle-lww-axum                                            │
│    REST handlers · WebSocket sessions · progress endpoints  │
└───────────────────────────┬─────────────────────────────────┘
                            │
┌───────────────────────────▼─────────────────────────────────┐
│  merkle-lww-cli                                             │
│    serve · pull · push · inspect · tree · verify            │
└─────────────────────────────────────────────────────────────┘
```

### `merkle-lww-core`

Transport- and runtime-agnostic. No tokio, no axum. It is a library over `rusqlite`, and
it can be driven from a blocking binary or a test harness.

The central abstraction is `Db`:

```rust
pub struct Db { /* pool, config, tree snapshot, log state */ }

impl Db {
    /// Borrow a pooled connection for arbitrary SQL. Triggers and the authorizer are
    /// already installed. This is the primary interface.
    pub fn conn(&self) -> Result<PooledConnection>;

    /// Run a closure inside a transaction.
    pub fn transaction<T>(&self, f: impl FnOnce(&Transaction) -> Result<T>) -> Result<T>;

    /// Managed convenience layer — sugar over the above.
    pub fn upsert(&self, table: &str, key: &Key, values: &[(&str, Value)]) -> Result<()>;
    pub fn delete(&self, table: &str, key: &Key) -> Result<()>;
    pub fn move_key(&self, table: &str, from: &Key, to: &Key) -> Result<()>;
}
```

`Db::conn()` is the primary interface because the application keeps writing plain SQL —
see [ADR-0002](adr/0002-enforce-in-sqlite-not-in-the-api.md) and
[ADR-0010](adr/0010-no-orm-query-wrapper.md).

### `merkle-lww-axum`

The transport. Depends on `core`. Provides the REST endpoints and the WebSocket session
handling described in [protocol.md](protocol.md).

The crate does **not** terminate TLS. See [security.md](security.md).

### `merkle-lww-cli`

Operational tooling. The `inspect`, `tree` and `verify` subcommands are not optional
extras — Merkle bugs are undebuggable without them. See
[operations.md](operations.md).

## Crate layout

```
rusqlite-merkle-lww/
├── Cargo.toml                 # workspace
├── PLAN.md                    # implementation plan
├── README.md
├── doc/                       # these documents
│   └── adr/
├── crates/
│   ├── merkle-lww-core/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── config.rs      # SyncConfig, TableSpec, Peer, CacheConfig
│   │       ├── codec.rs       # canonical encoding, digest, seeded xxh3
│   │       ├── time.rs        # RFC 3339, monotonic issuance
│   │       ├── store.rs       # Db, pool, managed tables, DDL
│   │       ├── trigger.rs     # trigger installation, both dialects
│   │       ├── authorizer.rs  # sqlite3_set_authorizer rules
│   │       ├── log.rs         # _sync_log reads/writes
│   │       ├── merkle.rs      # bucket tree, hashes, snapshot
│   │       ├── diff.rs        # comparison and merge
│   │       ├── session.rs     # protocol state machine
│   │       └── cache.rs       # read-through cache
│   ├── merkle-lww-axum/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── rest.rs
│   │       ├── ws.rs
│   │       └── progress.rs
│   └── merkle-lww-cli/
│       └── src/
│           └── main.rs
```

## Threading model

`rusqlite::Connection` is `Send` but not `Sync`, so the pool is the concurrency unit.

- **WAL mode.** One writer plus many readers, which matches the access pattern: the sync
  engine reads heavily and writes in bursts.
- **Separate read and write paths.** Reads come from the `r2d2` pool. Writes go through a
  single dedicated connection, so write contention never becomes `SQLITE_BUSY` retries.
- **No blocking on the reactor.** Every database call from async context goes through
  `spawn_blocking`. Axum handlers never touch the database on the reactor thread.
- **Tree maintenance off the reactor.** Dirty-bucket rehashing is a `spawn_blocking` job;
  its result is published into an `arc-swap` snapshot.
- **Synchronous core.** `merkle-lww-core` is synchronous. The async boundary is the
  transport, which is where `tokio` lives.

### The snapshot

The Merkle tree is held as an immutable `Arc<SyncSnapshot>` behind an `ArcSwap`. Readers
(the HTTP handlers, the WebSocket sessions) call `arc.load_full()` — a refcount bump, no
lock, no contention. Writers mutate a working copy and publish a new snapshot.

Publication is coalesced: a batch of writes results in one snapshot, not one per row.
This matters because rebuilding the snapshot touches every dirty bucket's ancestors.

## Data flow of a sync

```
    initiating node                                responding node
    ────────────────                                ────────────────
    HELLO  ───────────────────────────────────────►  validate epoch, seed,
                                                      retention, schema hash
    ◄─────────────────────────────────────────────  HELLO (node_id, layout,
                                                      as_of, purge instant)

    SUMMARY(range, level) ───────────────────────►  compare bucket hashes
    ◄─────────────────────────────────────────────  differing bucket ids
                                                      + total bucket count

    DETAIL(differing buckets) ────────────────────►  expand one level
    ◄─────────────────────────────────────────────  child bucket hashes + counts

    ROWS(differing buckets) ──────────────────────►  compare row digests,
                                                      exchange differing rows
    ◄─────────────────────────────────────────────  rows

    COMMIT ───────────────────────────────────────►  advance cursor
    ◄─────────────────────────────────────────────  as_of, synced_us
```

Both directions happen in the same session for bidirectional roles: the ROWS phase
exchanges rows in both directions simultaneously, so there is no separate push API.

Progress is reported throughout. See [protocol.md](protocol.md) § "Progress".

## Failure modes this shape creates

The design deliberately creates three, and each has a documented answer:

| Failure | Answer |
|---|---|
| A peer is offline longer than the retention window | The staleness rule refuses incremental sync and requires a re-seed — [lifecycle.md](lifecycle.md) |
| A purge happens while a peer is offline | The returning peer detects the newer purge instant at handshake and purges locally first — [lifecycle.md](lifecycle.md) |
| A row is edited without moving `last_update` | Detected by the stored digest; reported by `verify` — [data-model.md](data-model.md) § "Divergence detection" |

The first two are the only ones that can cause permanent divergence, and both are
prevented by rules rather than by hope. The third is detected but not repaired — an
out-of-band edit cannot be ordered correctly, because the information needed to order it
was never recorded.

## Related documents

- [data-model.md](data-model.md) — what is stored
- [sqlite-interface.md](sqlite-interface.md) — how SQLite is driven
- [merkle.md](merkle.md) — the diffing structure
- [protocol.md](protocol.md) — the wire format
- [lifecycle.md](lifecycle.md) — deletes and purge
- [security.md](security.md) — trust
- [adr/](adr/) — the decisions behind all of this
