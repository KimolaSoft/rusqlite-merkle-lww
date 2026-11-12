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
the same table.

The expected deployment is a **shared dataset**: the sites are peers over one logical
dataset, so the same user is the same user everywhere. Under the default `GlobalId` key
strategy, two sites updating the same user converge on **one row**, and LWW resolves
which write wins.

The caveat that must be handled explicitly: this only works if both sites derive the
**same id** for the same entity. A randomly generated globally-unique id does not merge
anything — it merely avoids collisions. See
[data-model.md](data-model.md) § "Getting the id right".

For tables that are genuinely per-site, the `SiteNamespaced` strategy keeps each site's
rows in its own namespace. Those rows never contend, which is the safe behaviour for
data that must not be blended across sites.

Both strategies are available per table. The choice is a data-modelling decision, and
the default follows the expected deployment — see
[ADR-0006](adr/0006-site-namespaced-keys.md).

## Data placement

The hierarchy controls **connectivity and write authority, not storage**.

| Node | Holds |
|---|---|
| **master** | **all** application data, for the entire deployment |
| **child** | all application data, except the UUID directory |
| **client** | the application data its **visibility profile** allows — never a restricted column, and never a table the profile excludes |

Every node's Merkle tree therefore covers the same row set, modulo the security
exclusions, and every comparison is apples-to-apples.

The exclusion is decided by the **profile**, not by a per-node special case: a profile
is a named projection mapped to a role and overridable per peer, so "masters exchange
this column but children do not" and "this table is not for clients" are both
declarations rather than code. See [data-model.md](data-model.md) § "Visibility
profiles".

A child's data reaches other children by passing through its master — the master is the
only node a child talks to — but the end state is identical everywhere. The UUID
directory is a *routing* structure, not a replication boundary: a child never learns
another child's UUID, and does not need to, because the data comes to it regardless.

## The ordering invariant

Every node must compute the same digest for the same logical row, or it never
synchronises. Three things must therefore be identical everywhere:

| Must match | Governed by | Failure |
|---|---|---|
| Bucket assignment — a pure function of `last_update` and the shared epoch | `epoch_us` | `EPOCH_MISMATCH` |
| The **registered column order** — the digest concatenates values in registration order | `schema_hash` | `SCHEMA_MISMATCH` |
| The encoding rules themselves | `codec_version` | `CODEC_MISMATCH` |

Two peers that register the same columns in different orders compute different digests for
identical rows, and every bucket differs forever. The registration order is part of the
schema, not an implementation detail, and the schema hash covers it.

This is the "sort identically" rule. There is no other sort: within a bucket, the hour
hash is a commutative sum, so intra-bucket order does not matter — but the *digest input
order* does, and that is what the schema hash pins.

## Transport

**The crate does not implement TLS, and it does not dictate how the application exposes
sync over the network.** It provides the data exchange; the application supplies the
transport.

This is a deliberate division, and it is the reason the crate can be used from any web
framework:

```
┌─────────────────────────────────────────────────────────────┐
│  Application                                                │
│    • chooses REST, WebSocket, or both                       │
│    • terminates TLS (or delegates it to a proxy)            │
│    • authenticates the peer                                 │
│    • calls the crate to perform the exchange                │
└───────────────────────────┬─────────────────────────────────┘
                            │  a transport-agnostic call
┌───────────────────────────▼─────────────────────────────────┐
│  merkle-lww-core                                            │
│    perform_exchange(peer, transport) -> SyncReport          │
└─────────────────────────────────────────────────────────────┘
```

The core exposes the exchange as a **transport-agnostic operation**: the application
supplies something that can carry request and response messages, and the crate runs the
session. The application keeps full control of its HTTP surface, its TLS, and its
authentication.

### What the crate provides

| Layer | Provided by | Notes |
|---|---|---|
| The vocabulary — config, digest, hashes, merge rules | `merkle-lww-model` | pure; `serde` only |
| A message type | `merkle-lww-protocol` | serialisable, transport-independent |
| The wire format | `merkle-lww-protocol` | JSON, `merkle-lww/1` |
| The exchange logic | `merkle-lww-core` | diff, merge, LWW, progress |
| REST endpoints | `merkle-lww-axum`, optionally | |
| WebSocket handling | `merkle-lww-axum`, optionally | |
| TLS | **the application** | usually terminated by a proxy or ingress |
| Authentication | **the application** | the crate checks the credential it is given |
| Routing, middleware, session management | **the application** | |

### Why the crate ships REST and WebSocket anyway

Because writing a correct session handler — resumption, progress frames, the handshake
validation, the quiesce check — is work an application should not have to repeat, and
getting it subtly wrong produces divergence rather than an error.

So `merkle-lww-axum` provides both, and an application that wants them uses them. An
application that needs neither — because it already has a session layer, or because it
wants to expose sync over something else entirely — calls the core directly.

The guidance for implementers is in
[implementation.md](implementation.md).

## Components

```
┌─────────────────────────────────────────────────────────────┐
│  Application code                                            │
│    reads/writes plain SQL on a pooled connection             │
└───────────────────────────┬─────────────────────────────────┘
                            │ rusqlite::Connection
┌───────────────────────────▼─────────────────────────────────┐
│  merkle-lww-core            the engine                       │
│   store ──────── managed tables, _sync_log, triggers         │
│   merkle ────── bucket tree, snapshot                       │
│   diff ──────── comparison, merge                            │
│   session ───── session state machine                       │
│   lifecycle ─── purge schedule, quiesce, staleness           │
└───────────────────────────┬─────────────────────────────────┘
                            │ trait boundaries
┌───────────────────────────▼─────────────────────────────────┐
│  merkle-lww-axum            the transport                   │
│    REST handlers · WebSocket sessions · progress endpoints   │
└───────────────────────────┬─────────────────────────────────┘
                            │
┌───────────────────────────▼─────────────────────────────────┐
│  merkle-lww-cli             the binary                       │
│    serve · pull · push · inspect · tree · verify            │
└─────────────────────────────────────────────────────────────┘
```

`merkle-lww-model` and `merkle-lww-protocol` sit **under** `merkle-lww-core` in the
graph and are not shown here: they are pure, they carry no I/O, and `core` depends on
them rather than the other way round.

```
merkle-lww-model        the vocabulary
  codec · config · merge · visibility · schema · time · error
merkle-lww-protocol     the wire contract
  messages · values · handshake · engine · transport · version
```

See § "The crate graph".

## The crate graph

The workspace has six crates, and the dependency edges point one way. The split is on
two independent axes — **what needs SQLite** and **what needs a runtime** — not on
"database versus transport" alone.

```
merkle-lww-model      config · merge · visibility · codec · schema · time · error
      ▲
merkle-lww-protocol   messages · values · handshake · engine · transport · version
      ▲
merkle-lww-core       store · trigger · authorizer · log · merkle · diff · bulk ·
      ▲               cache · session · paging · lifecycle · directory · registry ·
      ▲               allocator · session_store
merkle-lww-axum       REST · WebSocket · progress · compression
      ▲
merkle-lww-cli        serve · pull · push · inspect · tree · verify

merkle-lww-demo       in-memory transport · scripted clock · two-node fixture ·
                      topology and scenario fixtures  (depends on core, protocol, model)
```

See [ADR-0024](adr/0024-the-workspace-is-split-into-six-crates.md).

### `merkle-lww-model`

The vocabulary. Pure — `serde` and nothing else, no SQLite and no runtime.

It holds what must mean the same thing everywhere: the canonical encoding and the
seeded xxh3 digest, the `schema_hash`, the `visibility_hash`, the merge decision
rules, the configuration types, the timestamp rules, and the error codes.

Two things follow from it being pure, and both are why it is its own crate:

- **The compatibility surface is testable without a database.** The digest rules, the
  schema hash and the visibility hash are exactly what two independently implemented
  peers must agree on byte for byte. Golden vectors for them are consumable by an
  implementation in another language, which is the point of specifying the encoding
  precisely.
- **The extension point carries no engine weight.** `MergePolicyFn` is what an
  application implements, and it is a pure function of `(incoming, local) ->
  MergeDecision`. Under a single-crate layout, implementing one meant depending on
  `rusqlite` and the whole engine.

### `merkle-lww-protocol`

The wire contract. The message types, their JSON shapes, the wire version
`merkle-lww/1`, and the **handshake validation rules**.

The rules are pure functions over a gathered view:

```rust
pub fn validate_handshake(local: &LocalView, peer: &Hello) -> Result<Hello>;
```

`LocalView` is the set of facts the rules check — the epoch, the codec version, the two
hashes, the retention and purge schedule, the known peers, the last sync instant.
`core` gathers it from the database; the rules themselves need no database, so every
rejection case is a table-driven test with no fixture.

This crate also declares **the traits the session needs from the engine**, which `core`
implements. The consumer declaring the interface is what keeps the dependency edge
pointing one way.

It also declares **the `Transport` trait** — the seam the application implements to
carry messages. The trait is in terms of *messages*, not bytes, which is what keeps
compression outside the protocol: a transport that compresses is compressing the bytes
it puts on the wire, and the crate never sees them.

The wire version and the codec version are separate, and this is where that separation
lives: `merkle-lww/1` versions the protocol, `codec_version` versions the canonical
encoding. Changing the wire format re-digests nothing.

### `merkle-lww-core`

The engine. Everything that touches SQLite. It depends on `rusqlite` and `r2d2`, and
it stays **synchronous**: no `tokio`, no `axum`.

It is **one** crate, not several. The store, the triggers, the authorizer, the log, the
tree, the diff, the bulk path and the session all take `Db` in nearly every signature;
splitting them produces crates that cannot be used independently, so the boundary cost
is paid and none of the benefit is received.

The central abstraction is `Db`:

```rust
pub struct Db { /* pool, config, tree snapshot, log state */ }

impl Db {
    /// Borrow a pooled connection for arbitrary SQL. Triggers and the authorizer are
    /// already installed. This is the primary interface.
    pub fn conn(&self) -> Result<PooledConnection>;

    /// Run a closure inside a transaction.
    pub fn transaction<T>(&self, f: impl FnOnce(&Transaction) -> Result<T>) -> Result<T>;

    /// Install the crate's UDFs and authorizer on a connection the caller owns.
    pub fn register_on(&self, conn: &rusqlite::Connection) -> Result<()>;

    /// Run a closure with crate privilege on a registered caller-owned connection.
    pub fn with_privilege<T>(&self, conn: &rusqlite::Connection,
                             f: impl FnOnce() -> Result<T>) -> Result<T>;

    /// Managed convenience layer — sugar over the above.
    pub fn upsert(&self, table: &str, key: &Key, values: &[(&str, Value)]) -> Result<()>;
    pub fn delete(&self, table: &str, key: &Key) -> Result<()>;
    pub fn move_key(&self, table: &str, from: &Key, to: &Key) -> Result<()>;
}
```

`Db::conn()` is the primary interface because the application keeps writing plain SQL —
see [ADR-0002](adr/0002-enforce-in-sqlite-not-in-the-api.md) and
[ADR-0010](adr/0010-no-orm-query-wrapper.md). `register_on` is the escape hatch for an
application that cannot move its writes into the pool: it gets the same enforcement on
the connection it already owns.

### `merkle-lww-axum`

The transport. Depends on `core` and `protocol`. Provides the REST endpoints and the
WebSocket session handling described in [protocol.md](protocol.md).

This is where the async boundary is, and where `tokio` lives. It also owns
**compression**: zstd level 1 by default, which the application may disable. See
[ADR-0023](adr/0023-json-wire-format-and-transport-compression.md).

The crate does **not** terminate TLS. See [security.md](security.md).

### `merkle-lww-cli`

Operational tooling. The `inspect`, `tree` and `verify` subcommands are not optional
extras — Merkle bugs are undebuggable without them. See
[operations.md](operations.md).

### `merkle-lww-demo`

The conformance harness. It exists to answer one question: **how do we know the
interface is right?**

- **An in-memory `Transport`**, so a full session runs between two peers in one
  process with no ports and no network.
- **A scripted clock**, so purge instants, the quiesce window and the staleness horizon
  are reached by advancing time rather than by sleeping. The failure modes — a missed
  purge instant, a stale peer, a quiescing table, a resumed cursor — are the ones that
  are otherwise flaky.
- **A two-node fixture**: two `Db`s over in-memory SQLite, exchanged through the
  in-memory transport. This is what the convergence suite drives.
- **The topology and scenario fixtures**: the multi-node topologies for the convergence
  test, and the named lifecycle scenarios. The **wire vectors** themselves are published
  by `merkle-lww-protocol`, because the protocol crate is what defines the wire; this
  crate asserts against them, so a change to a message shape fails here as well as
  there.

It is a crate rather than a directory of tests so the examples are **compiled and run**
by CI. A worked example that is not executed is documentation that has already drifted.

## Crate layout

```
rusqlite-merkle-lww/
├── Cargo.toml                 # workspace
├── PLAN.md                    # the index: open questions, risks, cross-cutting work
├── plan/                      # one plan per crate
│   ├── model.md
│   ├── protocol.md
│   ├── core.md
│   ├── axum.md
│   ├── cli.md
│   └── demo.md
├── README.md
├── doc/                       # these documents
│   └── adr/
├── crates/
│   ├── merkle-lww-model/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── config.rs      # SyncConfig, TableSpec, Peer, CacheConfig
│   │       ├── merge.rs       # MergePolicy, MergePolicyFn, WriteAuthority
│   │       ├── visibility.rs  # VisibilityProfile, projections, visibility_hash
│   │       ├── codec.rs       # canonical encoding, digest, seeded xxh3
│   │       ├── schema.rs      # schema_hash
│   │       ├── time.rs        # RFC 3339, monotonic issuance
│   │       └── error.rs       # the error type, including the error codes
│   ├── merkle-lww-protocol/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── messages.rs    # Hello, Summary, Detail, Rows, Commit, Progress
│   │       ├── values.rs      # the tagged wire value form
│   │       ├── handshake.rs   # LocalView, validate_handshake
│   │       ├── engine.rs      # the traits the session needs from the engine
│   │       ├── transport.rs   # the Transport trait the application implements
│   │       └── version.rs     # the wire version: merkle-lww/1
│   ├── merkle-lww-core/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── store.rs       # Db, pool, managed tables, DDL
│   │       ├── trigger.rs     # trigger installation, both dialects
│   │       ├── authorizer.rs  # sqlite3_set_authorizer rules
│   │       ├── log.rs         # _sync_log reads/writes
│   │       ├── registry.rs    # the optional site registry and range triggers
│   │       ├── allocator.rs   # the optional id allocator (feature "alloc")
│   │       ├── directory.rs   # the UUID directory
│   │       ├── session_store.rs # the durable client session journal
│   │       ├── merkle.rs      # bucket tree, hashes, snapshot
│   │       ├── diff.rs        # comparison and merge
│   │       ├── bulk.rs        # bulk apply, chunking, the parameter limit
│   │       ├── paging.rs      # page cursors and the byte budget
│   │       ├── session.rs     # protocol state machine
│   │       ├── lifecycle.rs   # the purge schedule, quiesce, staleness, re-seed
│   │       └── cache.rs       # read-through cache
│   ├── merkle-lww-axum/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── rest.rs
│   │       ├── ws.rs
│   │       ├── progress.rs
│   │       └── compression.rs  # the zstd content encoding
│   ├── merkle-lww-cli/
│   │   └── src/
│   │       └── main.rs
│   └── merkle-lww-demo/
│       └── src/
│           ├── lib.rs
│           ├── transport.rs    # the in-memory Transport
│           ├── clock.rs        # the scripted clock
│           ├── fixture.rs      # the two-node fixture
│           └── vectors.rs      # the topology and scenario fixtures
```

The optional pieces — the registry, the allocator and the `site` column — live behind
the `alloc` feature and are not dependencies of the sync core. See
[ADR-0019](adr/0019-the-allocation-registry-is-optional.md).

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
- **Pure below the core.** `merkle-lww-model` and `merkle-lww-protocol` have no
  concurrency at all — no `Send` bounds, no interior mutability, no locks. The
  handshake rules in `protocol` are ordinary functions over ordinary data.
- **The demo runs in one thread.** The in-memory transport and the scripted clock are
  single-threaded by construction, so a test that drives two peers is deterministic
  rather than merely fast.

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
    ◄─────────────────────────────────────────────  rows, in bounded pages

    COMMIT ───────────────────────────────────────►  advance cursor
    ◄─────────────────────────────────────────────  as_of, synced_us
```

Both directions happen in the same session for bidirectional roles: the ROWS phase
exchanges rows in both directions simultaneously, so there is no separate push API.

The ROWS phase is **paged**: a message is bounded by `max_message_bytes`, and the
session by `max_rounds_per_session`. Both bounds are resumable — a session that runs out
of rounds is continued with the cursor it reached, not restarted. See
[protocol.md](protocol.md) § "Paging and message budgets".

Progress is reported throughout. See [protocol.md](protocol.md) § "Progress".

## Failure modes this shape creates

The design deliberately creates four, and each has a documented answer:

| Failure | Answer |
|---|---|
| A peer is offline longer than the retention window | The staleness rule refuses incremental sync, in both directions; the peer re-seeds by **wiping** and pulling the peer's dataset cleanly — [lifecycle.md](lifecycle.md) |
| A purge happens while a peer is offline | The returning peer detects the newer purge instant at handshake and purges locally first — a **catch-up purge**, not an error — [lifecycle.md](lifecycle.md) |
| A row is edited without moving `last_update` | Detected by the stored digest; reported by `verify` — [data-model.md](data-model.md) § "Divergence detection" |
| Two peers apply a custom merge policy to the same pair of rows and disagree | The policy was not convergent; the crate cannot detect this, so the policy must be pure — [data-model.md](data-model.md) § "Merge policies" |

The first two are the only ones that can cause permanent divergence, and both are
prevented by rules rather than by hope. The third is detected but not repaired — an
out-of-band edit cannot be ordered correctly, because the information needed to order it
was never recorded. The fourth is the one failure the crate **cannot** detect: a
non-convergent merge policy returns a definite decision, so the digest tie-break never
fires. That is why the convergence suite runs with a custom policy in the loop.

## Related documents

- [data-model.md](data-model.md) — what is stored
- [sqlite-interface.md](sqlite-interface.md) — how SQLite is driven
- [merkle.md](merkle.md) — the diffing structure
- [protocol.md](protocol.md) — the wire format
- [lifecycle.md](lifecycle.md) — deletes and purge
- [security.md](security.md) — trust
- [adr/](adr/) — the decisions behind all of this
