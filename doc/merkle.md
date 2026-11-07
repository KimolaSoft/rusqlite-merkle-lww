# The Merkle tree

The tree answers one question efficiently: **which keys differ between two nodes?**

It is indexed by wall-clock time rather than by key, because the thing that needs to be
diffed is almost always *recent changes*. Rows written long ago rarely change, so the
tree gives old ranges a coarse comparison and new ranges a fine one.

## Shape

Three levels, all boundaries absolute and derived from a shared epoch `E`:

```
hour_id   = floor((t - E) / 3_600_000_000)      1 hour
cycle_id  = floor(hour_id / 24)                 24 hours  ("cycle")
month_id  = floor(cycle_id / 30)                30 cycles ("month")
```

Because `E` is shared and the arithmetic is exact integer division, **every node computes
byte-identical bucket boundaries**. There is no "what is a day" disagreement, no timezone
involvement, and no requirement that sites be awake at the same moment.

A row belongs to the bucket containing its `last_update`.

## Why the boundaries are absolute

The original sketch was "hourly for the last 24 hours, then daily for 3 months, then
monthly beyond" as a *rolling* structure, where a bucket changes granularity as it ages.

That has a fatal property. At the transition, a bucket's hash changes for reasons
unrelated to any data change, so every peer sees a phantom difference. The usual patch is
a buffer — a 25-hour cycle, keeping an hour of slack so in-flight writes land in a bucket
that has not yet rolled up.

The design avoids the transition entirely by separating two ideas:

- **Bucket boundaries are absolute.** A row's bucket depends only on its `last_update`,
  and never changes identity. Modifying a 183-day-old row moves it out of its old month
  block into the current hour block — two bucket hashes change, both for real reasons.
  There is no rollup moment to miss, so no buffer is needed.
- **Age determines comparison depth, not bucket granularity.** A range is *first*
  compared at a depth chosen by its age, and only drilled further if it differs.

A modification to an old row does exactly what it should: it shows up as a difference in
both the old bucket and the new one, and the diff finds it in the new one.

See [ADR-0003](adr/0003-absolute-bucket-boundaries.md).

## Comparison depth

```
age of range (relative to as_of)   first comparison depth
  < 24 hours                        hour
  < 90 days                         cycle
  otherwise                         month
```

Drilling below the chosen depth is **always permitted** when a coarse comparison differs.
The depth is a fast path for skipping *equal* ranges, not a limit on resolution.

So the behaviour is exactly "hourly for the last 24 hours, then daily for 3 months, then
monthly beyond" — but without the churn that a rolling structure would introduce.

`as_of` comes from the handshake. In a client→server session the server supplies it, so
both sides choose identical depths.

## Hash construction

```
digest      = seeded_xxh3(sync_id || last_update || status || purge_on || replicated_values)
row_hash    = xxh3(sync_id || last_update || status || purge_on || digest)

hour_hash   = xxh3(hour_id_be  || row_count_be || Σ row_hash)
cycle_hash  = xxh3(cycle_id_be || concat over 24 hours of (hour_id_be || hour_hash))
month_hash  = xxh3(month_id_be || concat over 30 cycles of (cycle_id_be || cycle_hash))
```

All integer fields are big-endian.

### Why the hour hash is a commutative sum

`hour_hash` folds row hashes with a **sum modulo 2¹²⁸**, not an ordered fold.

That makes updates O(1) at the leaf. Moving a row from hour H₁ to hour H₂ subtracts the
old row hash from H₁ and adds the new one to H₂, with no rescan of either hour. An
ordered fold would require re-reading every row in the bucket on every write, which for a
busy hour is expensive.

Ordering information lives at the cycle and month levels, where the child count is small
(24 and 30) and a rescan is cheap.

The cost of commutativity is that an hour hash does not distinguish *which* rows are in
the hour, only the multiset of row hashes. That is sufficient: the row-level comparison
in the ROWS phase identifies the differing keys exactly.

### Empty buckets

An empty bucket has a well-defined hash: `row_count = 0`, `Σ = 0`. So a missing bucket
resolves to that constant and needs no special storage. Only non-empty buckets are kept.

### The two-level row hash

`row_hash` folds the `digest` rather than the payload directly. That is what lets a
read-only client compute the same tree hash as a trusted master **without holding the
master's restricted values** — the client is given the digest and folds it into the same
formula.

## The snapshot

```rust
struct SyncSnapshot {
    epoch_us:           i64,
    last_purge_instant: i64,
    hour:   HashMap<i64, [u8; 16]>,   // hour_id  -> hash
    cycle:  HashMap<i64, [u8; 16]>,   // cycle_id -> hash
    month:  HashMap<i64, [u8; 16]>,   // month_id -> hash
    counts: HashMap<i64, u32>,        // hour_id  -> row count
}
```

Held in an `arc_swap::ArcSwap<SyncSnapshot>`. Readers call `load_full()` — a refcount
bump, no lock. Writers mutate a working copy and publish.

### Memory

Only non-empty buckets are stored, so memory tracks **activity**, not table age. The
number of non-empty hour buckets is bounded by the number of distinct hours in which some
row was modified.

| Activity span | hour buckets | cycles | months | ≈ memory @ 64 B/entry |
|---|---|---|---|---|
| 1 year | 8,766 | 365 | 12 | 0.6 MB |
| 5 years | 43,830 | 1,826 | 61 | 2.8 MB |
| 10 years | 87,660 | 3,652 | 122 | 5.6 MB |
| 20 years | 175,320 | 7,305 | 244 | 11.2 MB |

Per table. A table that has existed for ten years but was only touched this year holds
8,766 buckets.

`max_tree_buckets` remains as a tripwire against a misconfigured epoch, not as a working
limit.

## Rebuild and incremental maintenance

### At startup

One indexed pass over `_sync_log` ordered by `(tbl, hlc_us)`. For each row, fold its row
hash into its hour bucket, then recompute the affected cycle and month hashes.

O(rows), no payload reads. The digest is stored in `_sync_log`, so the payload columns
are never touched.

### On write

The write path knows the old row and the new row, so it applies the incremental update
directly:

1. subtract the old row hash from the old hour;
2. add the new row hash to the new hour;
3. recompute the touched cycle hash and month hash.

A status change keeps the same hour and replaces the hash in place.

### On purge

The removed row's row hash is subtracted from its hour, and the ancestors recomputed.

### Publication

After a batch of updates, publish a fresh `Arc<SyncSnapshot>`. Publication is coalesced —
end of transaction batch, or a timer — so a burst of writes results in one snapshot
rather than one per row. Rebuilding the snapshot touches every dirty bucket's ancestors,
so per-row publication would be wasteful.

## What the tree is not

- **It is not a hash of the whole table.** It is a hash of *time ranges*, so a diff can
  skip ranges entirely.
- **It is not content-addressed storage.** The tree does not store rows; `_sync_log` does.
  The tree stores bucket hashes and row counts.
- **It does not detect content changes that did not move `last_update`.** The tree hashes
  the timestamp, so such a change is invisible to it. The stored digest detects it
  separately — see [data-model.md](data-model.md) § "Divergence detection".

## Comparison with the original sketch

| Property | Original | This design |
|---|---|---|
| Bucket identity | changes as data ages | fixed forever |
| Hash churn | at every tier transition | none |
| Buffer needed | yes (25h cycle) | no |
| Late writes to old data | may be missed at a rollup | always detected, at the new bucket |
| Granularity available | limited by tier | full, at any depth |
| Memory | grows with rows | grows with distinct changed hours |

## Related documents

- [data-model.md](data-model.md) — the digest and canonical encoding
- [protocol.md](protocol.md) — how the tree is used in a session
- [architecture.md](architecture.md) — the snapshot and threading
- [adr/0003](adr/0003-absolute-bucket-boundaries.md) ·
  [adr/0004](adr/0004-digest-is-always-computed-and-keyed.md)
