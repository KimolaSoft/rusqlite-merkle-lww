# ADR-0021: A re-seed is a wipe; a stale peer pulls and never pushes

- **Status:** accepted
- **Date:** 2026-11
- **Deciders:** project maintainers

- **Supersedes:** [ADR-0017](0017-re-seed-is-a-merge-not-a-wipe.md)

## Context

[ADR-0017](0017-re-seed-is-a-merge-not-a-wipe.md) decided that a re-seed is a **merge**:
the stale peer keeps what it has, pushes what only it knows, and pulls everything else.
The motivation was sound — a child is the primary for its own site, it accepts writes
while it is disconnected, and a wipe discards them silently.

The merge, however, opens a path to **resurrection**, which is the one failure this
design is built to prevent. The path is narrow, and it is worth stating exactly:

```
T    a peer deletes row K; the tombstone gets purge_on ≈ T + retention
     peer B was disconnected before T, so B still holds K live
P    the tombstone is purged, ≈ 30 days later; K is now absent everywhere
T'   B reconnects, more than retention after T
     B's re-seed pushes its live copy of K; the peer re-creates it
```

B must be disconnected from before `T` until after `P`, so the path requires a
**continuous outage longer than the retention window** — thirty days or more. Below
that, the peer still holds the tombstone and last-write-wins resolves the delete
correctly.

Thirty days is not an outage any production deployment tolerates, and the retention
window is set precisely so that the tombstone evidence outlives every disconnection it
could plausibly face. The merge therefore defends a branch that the operational
envelope makes unreachable, and it does so at the cost of the crate's headline
guarantee.

The choice is between paying to keep offline writes in an unreachable case, and making
the resurrection guarantee unconditional.

## Decision

**A re-seed is a wipe.** A stale peer regenerates and pulls the peer's dataset cleanly.

1. **Catch up on purges.** Purge locally to the peer's `last_purge_instant`, so the
   tombstones the peer has already removed are removed here too. This is the ordinary
   missed-purge catch-up, not a special case.
2. **Wipe.** Discard this peer's synced state: the replicated state of the managed
   tables, the `_sync_log` entries, and the tree built from them. Local-only state that
   the peer is not authoritative for is left alone.
3. **Pull.** Receive the peer's full dataset, table by table, in pages
   ([protocol.md](../protocol.md) § "Paging and message budgets"), applying each row
   under the table's merge policy.

**The stale peer does not push.** Its unsynced writes are discarded, not offered. This
is not an optimisation: a stale peer holds rows that may have been deleted and purged
while it was away, and it cannot tell which. Offering them would resurrect them.

**Staleness is symmetric.** A peer beyond the horizon may pull, but the peer it pulls
from refuses its pushes. A session whose peer's `last_sync_us` is beyond the horizon
is a pull-only session.

### The invariant this restores

> **Retention must exceed the maximum tolerable disconnection window.**

The tombstone evidence must outlive every peer that could still be holding a stale copy
of the row. When that holds, the staleness rule refuses exactly the peers that could
resurrect a row, and **a purged tombstone never comes back to life** — not for a short
outage, not for a long one.

## Consequences

**Positive**

- **The resurrection guarantee is unconditional.** It no longer depends on an
  operational assumption about how long outages last. A peer that was away for a year
  and comes back cannot resurrect a row either.
- **No per-key state.** The merge would have needed a record of which keys had been
  purged, to tell a stale live row from a legitimate new one. The wipe needs none.
- **One code path.** A stale peer is a fresh installation with a populated database.
  There is no merge ordering to get right, and no push to arbitrate.
- **The staleness rule becomes meaningful.** It is not a warning; it is the boundary
  between "trust this peer's incremental state" and "rebuild it".

**Negative**

- **A stale peer loses the writes it accepted while it was disconnected.** They are
  discarded, not pushed. This is the cost of the guarantee, and it is paid only by a
  peer that has been disconnected for longer than `retention`.
- **A re-seed is expensive.** It transfers the whole dataset. For a peer that was
  merely partitioned, that is a large transfer for a state the peer could have kept —
  but that peer is not stale, and syncs incrementally.
- **A stale peer needs write access to its own site again after it re-seeds.** Its
  unsynced writes are gone, so it resumes from the peer's copy of its site's rows.

**Neutral**

- The staleness rule itself is unchanged: `now - last_sync_us > retention` still
  refuses incremental sync, and a stale peer still re-seeds.
- The purge floor is unchanged: a tombstone the receiver has already purged is still
  refused with `ROW_PURGED`.
- A read-only client's re-seed is unchanged — it has no local writes, so a wipe costs it
  nothing.

## Alternatives considered

**Keep the merge and record which keys have been purged.** A bounded per-key set would
let the master refuse a stale live row for a key it had purged, and so preserve both
properties. Rejected: it is per-key state that must be pruned, and it exists to defend
a branch the operational envelope makes unreachable. The guarantee is worth more than
the offline writes of a peer that has been gone for a month.

**Push the stale peer's writes, then wipe it.** Rejected: the push is the resurrection
path. The stale peer cannot tell a row that was deleted and purged from one that was
merely old.

**Make the retention window shorter, so the merge's exposure shrinks.** Rejected: the
window is what gives a tombstone time to reach every peer. Shortening it trades the
resurrection guarantee for a narrower exposure window.

**Let the operator choose at the time of the re-seed.** Rejected: an operator choosing
under time pressure will not know which rows were purged, and the safe option must not
depend on operator judgement.

## Related

- [ADR-0005](0005-soft-deletes-with-a-shared-purge-schedule.md) — the purge schedule
  and the resurrection invariant
- [../lifecycle.md](../lifecycle.md) — the staleness rule and the re-seed
- [../protocol.md](../protocol.md) — the handshake where staleness is detected
