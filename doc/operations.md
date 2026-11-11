# Operations

## Deployment shapes

### Single master, many children, many clients

The common shape. One master per site; children point at it; clients are read-only
replicas.

```
                 ┌──────────────┐
   clients ─────►│ master (A)   │◄─────► master (B)   (optional mesh)
   (pull)        └──┬────┬───┬──┘
                    │    │   │
                 children (bidirectional)
```

### Multi-master mesh

Masters at different sites replicate to each other. The sites are peers over one shared
dataset, so two masters can write the same logical row — and LWW resolves it. See
[architecture.md](architecture.md) § "Multi-master and the conflict domain".

For tables that are genuinely per-site, use `KeyStrategy::SiteNamespaced`, which keeps
each site's rows in its own namespace so they never contend.

Each master still needs its own children and clients.

### Single node

A master with no peers. Useful for development and for a single-site deployment. The
sync engine is idle; the store still enforces its invariants.

## Requirements

### Clock synchronisation

**All nodes must run NTP or chrony and agree within `max_skew`** — 5 seconds by
default.

This is a hard requirement, not a recommendation. It underpins:

- LWW ordering — a skewed clock can prefer a write that happened earlier in real time;
- bucket boundaries — a few seconds against an hour-long bucket is irrelevant, but the
  purge instant is wall-clock aligned, so skew shifts *when* a node purges by that much
  (which the quiesce window absorbs);
- the monotonic issuance guard, which assumes the clock is roughly monotonic.

The default of 5 s is deliberate. A one-second threshold is tighter than the clock
discipline a set of geographically distributed sites can actually guarantee, and
refusing a peer that is 1.2 s out rejects sessions that would have been perfectly
ordered. The tolerance is still far below the granularity that matters for LWW here,
and it is a **local** setting: two nodes may disagree about it without breaking
anything.

The crate exposes `clock_offset_estimate_us`, measured against peers at handshake, so a
node can log its own skew and refuse peers it believes are more than `max_skew` away.

### TLS

Terminated in front of the crate. See [security.md](security.md).

### Filesystem

The database file needs WAL mode, which requires the filesystem to support shared
memory. Network filesystems are **not** suitable for the database; the WAL and shared
memory files must be local.

## Configuration deployment

The values that must agree across peers — `epoch_us`, `digest_seed`, `retention`,
`purge_period` — are best delivered by the same mechanism the consumer already uses for
other shared state: an environment variable, a mounted secret, a config-management
system.

They **MUST NOT** differ between peers. A mismatch is a fatal handshake error, not a
degraded mode, except for `retention` and `purge_period` where a drift would cause
peers to purge different sets. Those two are checked at handshake and the peer is
rejected.

`epoch_us` additionally cannot change once data exists, because changing it re-buckets
every row.

## The CLI

```
merkle-lww serve   --config sync.toml
merkle-lww pull    --peer https://host --table device
merkle-lww push    --peer https://host --table device
merkle-lww inspect --table device --hour 29681      # bucket contents
merkle-lww tree    --table device                     # bucket hashes
merkle-lww verify  --table device                     # digest divergence check
```

`inspect`, `tree` and `verify` are not optional extras. Merkle bugs are undebuggable
without them: when two peers disagree, the question is always "which bucket, and what
is in it", and that needs a tool.

### `verify`

Reports rows where the stored digest disagrees with the digest recomputed from the user
table — that is, rows modified without advancing `last_update`.

It **reports and does not repair**. An edit that did not advance the timestamp cannot be
ordered correctly, because the information needed to order it was never recorded.
Repairing it would mean guessing, and a guess that is wrong causes divergence that is
much harder to diagnose than the original problem.

### `inspect`

Dumps the contents of a bucket: which keys, which `last_update`, which digest. Use it
to answer "does my peer have this row, and with what timestamp?"

## Metrics

The crate exposes counters and gauges for a Prometheus-style scrape:

| Metric | Type | Meaning |
|---|---|---|
| `sync_rows_sent_total` | counter | Rows transmitted |
| `sync_rows_received_total` | counter | Rows applied |
| `sync_rows_purged_rejected_total` | counter | Rows refused because the receiver had already purged them |
| `sync_rows_write_refused_total` | counter | Rows refused because their origin lacked write authority |
| `sync_bytes_total` | counter | Bytes transferred |
| `sync_sessions_active` | gauge | Sessions in flight |
| `sync_session_duration_seconds` | histogram | Session wall time |
| `sync_sessions_limit_total` | counter | Sessions ended by `max_rounds_per_session` |
| `sync_buckets_compared_total` | counter | Bucket comparisons |
| `sync_buckets_differing_total` | counter | Bucket comparisons that differed |
| `sync_purge_rows_total` | counter | Rows removed by purge |
| `sync_tree_buckets` | gauge | Non-empty buckets held |
| `sync_log_rows` | gauge | Rows in `_sync_log` |
| `sync_peer_last_sync_timestamp_seconds` | gauge | Per peer; last successful sync |
| `sync_peer_stale` | gauge | Per peer; 1 if past the staleness horizon |

The two most useful in practice:

- `sync_buckets_differing_total / sync_buckets_compared_total` — the *diff ratio*. A
  healthy deployment with peers in sync has a ratio near zero. A ratio that climbs
  steadily means peers are diverging.
- `sync_peer_last_sync_timestamp_seconds` — the staleness of each peer. A peer creeping
  toward the staleness horizon is about to require a re-seed.

## Health endpoint

```
GET /v1/health
```

Returns per-peer state: last successful sync, whether it is stale, the current session
and its progress, and whether the node is inside a purge quiesce window.

## Diagnosing the failure modes

### Two peers will not converge

1. `merkle-lww tree --table device` on both. Compare the bucket hashes.
2. `merkle-lww inspect --table device --hour <id>` on the differing bucket on both.
   Find the keys that differ.
3. For each differing key, compare `last_update` and the digest.
4. If the digests differ but the timestamps match, one side has a row modified without
   advancing `last_update`. `merkle-lww verify` finds these.

### A peer is refused with `STALE_PEER`

It has been offline longer than the retention window. This is by design — it cannot be
trusted to sync incrementally, because it may hold rows whose deletions were purged
before it heard about them.

The remedy is a re-seed. A re-seed is a **merge, not a wipe**: the stale peer pushes its
unsynced local writes to the peer, then pulls the peer's full dataset, and keeps the
rows the peer does not have. Nothing local is thrown away — in particular, a child that
was the primary for its own site while offline keeps those writes. Expect the transfer
to take a while and to move a lot of data; that is the cost of a long outage.

**A peer that merely missed a purge instant is not stale.** It catches up by purging
locally and continues in the same session; there is no error and no re-seed.

### A session ends with `SESSION_LIMIT`

The session hit `max_rounds_per_session` — a **retryable** outcome, not a failure. Re-open
the session with the last cursor and continue. If it happens on every session, the
diff is larger than the round limit allows; raise the limit, or lower
`max_message_bytes` so that pages are smaller and more of the work fits in the same
number of rounds.

### A peer is refused with `EPOCH_MISMATCH` or `DIGEST_SEED_MISMATCH`

The deployment configuration has diverged. Fix the configuration on the odd node out.
Note that changing `epoch_us` after data exists is not a fix — it re-buckets every row
and invalidates every stored hash. The correct remedy is to make the configurations
match again, and only then re-seed if the data has already diverged.

### Writes fail with "no such function: sync_stamp"

The application is writing on a connection that does not have the crate's UDFs
registered. In the enforced dialect this is the fail-closed behaviour working as
intended: the write is refused rather than silently corrupting the sync state.

The fix is to use a connection from `Db::conn()`, or a connection built with the
crate's connection factory.

### Sync is refused with `PURGE_IN_PROGRESS`

Expected during the quiesce window around a purge instant, and **only for the tables
being purged**. The window lasts `purge_quiesce` seconds, by default 60.

If it happens every day at the same time for the whole fleet, the node is purging a table
every instant. If it happens for a table that has no tombstones due, that is a bug — the
due set is computed from `_sync_log.purge_on_us`, and an empty set means no quiesce at
all.

### Everything is slow

Check the diff ratio. If most buckets differ on every sync, the peers are genuinely
diverging and each sync is transferring real data. If the ratio is low but syncs are
still slow, the bottleneck is the bucket count — check `sync_tree_buckets` against
`max_tree_buckets`.

## Backup and restore

The database file is the whole state. A backup is a file copy, and a restore is a file
copy back.

Two cautions:

- **Copy the WAL too**, or use `VACUUM INTO` for a consistent snapshot. Copying the
  main file while a WAL exists can produce a database missing recent commits.
- **A restored node has a different `last_issued_us`** if the restore point predates
  recent writes. The monotonic issuance guard will still hold — it reads the restored
  value — but the restored node will issue timestamps from the restored instant onward,
  which may be behind a live peer. This is a clock-skew problem, and it resolves itself
  as the clock advances, but it means a restored node may initially lose LWW
  comparisons. Re-seed instead where possible.

## Upgrades

The crate stores a `schema_version` in `_sync_meta`. A version mismatch between peers
is a handshake failure.

An upgrade that changes the internal schema (adding a column to `_sync_log`, say) is
applied by the crate on startup, in a transaction. An upgrade that changes the *managed*
table shape — adding a managed column — goes through `Db::alter_table`.

An upgrade that changes the digest computation or the canonical encoding invalidates
every stored digest. That requires a full re-digest, which means a full re-seed. The
crate refuses to start with a changed encoding version unless explicitly told to
re-digest.

## Related documents

- [architecture.md](architecture.md) — topology
- [lifecycle.md](lifecycle.md) — the staleness and purge rules
- [security.md](security.md) — deployment trust
- [config.md](config.md) — configuration
