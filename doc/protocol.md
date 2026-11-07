# The wire protocol

JSON on the wire. The same message set is used over REST and WebSocket; a binary
encoding (postcard) is a possible later addition.

## Roles in a session

| Local role | Remote role | Direction |
|---|---|---|
| Child | Master | push local writes, pull master data |
| Master | Child | pull child writes, push master data |
| Master | Master | bidirectional |
| Master | Client | push only; the client pulls |

"Pull-only" is a role flag, not a separate protocol. In a bidirectional session the
upload and download phases are interleaved in one ROWS step — each side sends its
differing rows and receives the peer's — so there is no separate push API.

## Handshake

```jsonc
{
  "protocol": "merkle-lww/1",
  "node_id": "…",              // 128-bit hex
  "site": "eu-west",
  "role": "master",            // master | child | client
  "epoch_us": 1735689600000000,
  "as_of_us": 1792953600000000,
  "last_purge_instant": 1792617600000000,
  "retention_us": 2592000000000,
  "last_sync_us": 1792950000000000,
  "tables": [ { "name": "device",
                "kind": "keyed",
                "key": { "strategy": "global_id",
                         "key_columns": ["id"] },
                "columns": [ { "name": "payload", "replicated": true,
                               "visibility": "public" },
                             { "name": "secret", "replicated": true,
                               "visibility": "restricted" } ],
                "client_visible": true } ],
  "schema_hash": "…"
}
```

### Validation

| Check | Failure |
|---|---|
| `epoch_us` differs | `EPOCH_MISMATCH` — fatal; the trees are not comparable |
| `protocol` differs | `PROTOCOL_UNSUPPORTED` |
| `digest_seed` differs (trusted peers) | `DIGEST_SEED_MISMATCH` — fatal; every row hash differs |
| `node_id` not in the peer list | `UNKNOWN_PEER` |
| `as_of_us` differs by more than the tolerance | the server's wins; the client adopts it |
| `last_purge_instant` — peer is older than ours | `STALE_PEER` (see [lifecycle.md](lifecycle.md)) |
| peer requests a `Restricted` column | `COLUMN_NOT_PERMITTED` |
| unknown `tables`/`columns` | sync the intersection, log a warning |

`as_of_us` is the single timestamp driving the comparison-depth policy and the "what
changed since" computation. In a client→server session the server supplies it, so both
sides choose identical depths.

## Session phases

```
1. HELLO     client → server
2. SUMMARY   exchange bucket hashes for a range
3. DETAIL    exchange child hashes for the buckets that differ
4. ROWS      exchange the differing rows
5. COMMIT    advance the cursor
```

### SUMMARY

```jsonc
// request
{ "session_id": "…", "tbl": "device",
  "from_us": …, "to_us": …, "level": "hour|cycle|month" }

// response
{ "session_id": "…",
  "differing": [ { "bucket_id": 29681, "level": "hour" } ],
  "total_buckets": 8766 }
```

Each side sends the hashes it holds for the range; each computes the symmetric
difference. The differing set is symmetric, so both sides learn the same set.

`total_buckets` is the number of buckets in the range at the requested depth, exactly
computable from the time range and the depth. It is the progress denominator for the
comparison phase (§ "Progress").

### DETAIL

```jsonc
// request
{ "session_id": "…", "tbl": "device",
  "buckets": [ { "id": 1237, "level": "cycle" } ] }

// response
{ "children": [ { "id": 29681, "level": "hour", "hash": "…", "count": 42 } ] }
```

Only differing buckets are expanded, one level per round trip. The `count` is the row
count, which becomes the denominator for the transfer phase.

### ROWS

Where bandwidth is won or lost. Two modes:

**`mode = "digest"` (default).** For each differing hour, each side sends a compact list
of `(key, digest)`. From the two lists both sides compute the exact differing key set,
then exchange only those rows' contents.

**`mode = "row"` (simple, more bytes).** Each side sends all rows for the differing
hours; the peer merges by LWW. Simpler and idempotent, but re-transmits unchanged rows in
a busy hour.

Both modes are idempotent and safe to retry.

Deleted rows are transmitted with their full payload and their `status`/`purge_on`, in
the same bucket as everything else — no separate tombstone stream, no extra round trip.

### COMMIT

```jsonc
{ "session_id": "…" }
→ { "as_of_us": …, "synced_us": … }
```

Writes the peer cursor in `_sync_cursors` and, on the server side, triggers a snapshot
publication.

## Applying a row

The merge rule is LWW on `(last_update, digest)`:

```
apply(incoming, local):
    if incoming.last_update >  local.last_update  -> take incoming
    if incoming.last_update <  local.last_update  -> keep local
    if equal:
        if incoming.digest > local.digest          -> take incoming
        else                                       -> keep local
```

The tie-break on `digest` only matters on an exact microsecond collision. Both peers hold
the same digest for the same row, so they reach the same conclusion.

Because the digest is keyed, only trusted peers can compute it. A client never performs
the tie-break — it applies what the server sends.

### Local columns are kept

When taking the incoming row, columns marked `local` are **not** taken. The remote's value
for a local sequence number belongs to a different sequence and is meaningless here:

```
apply(incoming, local):
    winner = the row chosen by the LWW rule above
    result = winner, but with every `local` column taken from `local`
```

This is what keeps a merged row's local `userid` intact when a remote copy arrives, and
it is why local columns are excluded from the digest — see
[data-model.md](data-model.md) § "Local columns".

### Set-valued columns

A column whose merge mode is `SetUnion` is merged as a union rather than replaced. The
default is whole-value replacement, under which a concurrent edit to a JSON array loses
one side's change. See [data-model.md](data-model.md) § "Set-valued columns".

A row is applied to the user table and to `_sync_log` in one transaction, and the tree is
updated incrementally.

## Progress

Every phase reports **X of Y**, with `Y` always known before the phase starts. That is
what makes the number meaningful rather than a spinner.

```rust
pub enum Phase {
    Comparing { range_from: i64, range_to: i64 },
    Expanding,
    Transferring,
    Applying,
    Done,
    Failed,
}

pub enum Unit { Buckets, Rows }

pub struct SyncProgress {
    pub session_id:  Uuid,
    pub peer:        Uuid,
    pub phase:       Phase,
    pub completed:   u64,
    pub total:       u64,           // always known
    pub unit:        Unit,
    pub bytes:       u64,
    pub started_at:  Instant,
    pub updated_at:  Instant,
}
```

| Phase | Unit | Y is |
|---|---|---|
| Comparing | buckets | total buckets in the requested range |
| Expanding | buckets | differing buckets from the comparison phase |
| Transferring | rows | sum of row counts over the differing leaf buckets |
| Applying | rows | rows received |

The denominators are known beforehand: the bucket count in a range is computable from the
epoch and the range; the differing-bucket count comes from the comparison phase; the row
count comes from the bucket metadata carried in the DETAIL response.

### No ETA

There is deliberately no `eta_seconds` in the protocol. An estimate computed from a
moving denominator is misleading, and a misleading ETA is worse than none: it makes
people wait when they should give up.

What is exact is the **rate** and the **byte count**. A consumer that wants an estimate
can derive one from `completed` and `elapsed` and present it as approximate — a display
concern, not a protocol one. See [ADR-0009](adr/0009-progress-has-no-eta.md).

### Where progress is surfaced

```
GET /v1/sync/sessions/{id}/progress    → SyncProgress for one session
GET /v1/sync/progress                  → every active session, grouped by peer
GET /v1/sync/peers/{peer}/progress     → this node's view of one peer
```

Over WebSocket, the server pushes a progress frame when the value changes materially —
at most once per 250 ms, and only when `completed` advances:

```jsonc
{ "type": "progress", "session_id": "…", "phase": "transferring",
  "completed": 3800, "total": 12400, "unit": "rows",
  "bytes": 4194304 }
```

### Bulk seeding

A peer that has fallen out of the retention window must be re-seeded wholesale. That is
the case a human actually waits on, so it gets its own progress path: "row N of M", where
M is the table's row count from `SELECT count(*)` at the start.

### Persistence

Per-peer progress is written to `_sync_cursors.progress`, so a restarted node can show
"last synced with master B: 62% (interrupted)" rather than nothing at all.

## Idempotency and overlap

- Sync is **idempotent**. Applying the same row twice is a no-op under LWW.
- Overlapping sessions between the same pair are fine. Cursors advance monotonically.
- A session is resumable by re-issuing SUMMARY with the same range.
- A session that fails mid-ROWS leaves the cursor at its previous value; the next session
  redoes the work. There is no partial-commit state.

## WebSocket

```
GET /v1/sync/ws   (upgrade)
```

- The server pushes `{"type":"changed","buckets":[…]}` when its snapshot advances. A
  pull-only client uses this as a trigger to open a session.
- Progress frames flow on the same socket.
- The connection *is* the session; `session_id` is allowed but not required.
- Heartbeat every 30 s; drop the connection after two missed heartbeats.

## REST

```
POST /v1/sync/hello
POST /v1/sync/summary
POST /v1/sync/detail
POST /v1/sync/rows
POST /v1/sync/commit
```

Sessions are addressed by an opaque `session_id`, held in memory with a 5-minute TTL. A
single `POST /v1/sync` with a `step` discriminator is an acceptable simplification if the
route count matters more than clarity.

## Pull-only clients

A client that only ever receives can use a simplified flow: `HELLO` → the server sends
the full diff proactively → the client applies → `COMMIT`. The client does not compute
symmetric differences, and the server drives the progress report directly.

## Related documents

- [merkle.md](merkle.md) — what is being compared
- [lifecycle.md](lifecycle.md) — `STALE_PEER` and the purge handshake
- [security.md](security.md) — handshake validation as part of the trust model
- [adr/0009](adr/0009-progress-has-no-eta.md)
