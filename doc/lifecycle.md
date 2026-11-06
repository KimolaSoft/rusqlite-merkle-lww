# Lifecycle: deletes, purge, resurrection

This is the part of the design with real failure modes, so it gets the most detail and
the most scrutiny.

## The model

- A delete sets `status = 'deleted'` and `purge_on = now + retention` on the row, and
  bumps `last_update`. **The row stays in the table.**
- `status` and `purge_on` are replicated columns, so every peer computes the same
  deadline and purges the same set.
- Live rows are **never** purged. They persist indefinitely.
- The row is physically removed only when `purge_on` passes.

### Deleted rows replicate in full

A deleted row's **payload is replicated**, in the same bucket as everything else, with no
special stream and no extra round trip.

This is a deliberate choice, and the reasoning is diagnostic value. A few bytes per
deleted row is trivial next to being able to see *what was deleted*, from any peer, in
one hop. When something goes wrong in production and a record is unexpectedly gone, the
question is almost always "what did it contain?" — and answering that from any peer,
without archaeology, is worth the bandwidth.

The digest covers the replicated columns of the row as they are, including a deleted
row's payload, so both peers compute the same digest.

### Soft delete, not hard delete

A hard delete would destroy the evidence that the deletion happened. The tombstone *is*
the deletion: without it, a peer that was offline during the delete still holds the row,
and there is no timestamp to arbitrate.

## Replicating a delete

A deleted row is an ordinary row in `_sync_log` and an ordinary member of the tree. Its
digest covers `status` and `purge_on`, so the delete propagates through the same diff
machinery as any other change.

LWW resolves delete-versus-update by `last_update`:

| Sequence | Outcome |
|---|---|
| update, then delete | deleted — the delete is newer |
| delete, then update | live — the update is newer (an un-delete) |
| delete, then delete | deleted — the later one wins |

An un-delete is just an upsert with a later `last_update`. Nothing special is needed for
it.

## The purge instant

Tombstones are removed at a **globally scheduled instant**, derived from the shared
epoch:

```
purge instants = E' + k * purge_period        (E' = epoch aligned to the schedule)
```

The default period is **daily**, at an epoch-aligned UTC instant (12:00:00Z). All nodes
compute the same instants, so all nodes purge the same set of rows at the same moment.

Around each instant is a **quiesce window** (default 60 s) during which sync is refused
with `503 PURGE_IN_PROGRESS`:

```
T-ε        nodes enter quiesce (sync refused)
T          purge: remove rows with status='deleted' AND purge_on <= T
T+ε        nodes leave quiesce; sync resumes, trees are identical again
```

### Why the quiesce window exists

Purging changes the tree. A peer that syncs mid-purge sees a half-applied state: one
node has removed a tombstone, another has not, and the difference looks like a genuine
content change. The window makes the purge atomic from the outside.

The window is narrow and predictable. The whole operation is a bulk `DELETE` on an
indexed predicate.

### A node that was down across a purge instant

It detects this at handshake — the peer's `last_purge_instant` is newer than its own —
and **purges locally before syncing**.

Without this, the returning node would push stale tombstones that its peers had already
removed, resurrecting tombstones that were supposed to be gone. The peers would then
have to purge them again at the next instant.

## The resurrection problem

A tombstone is the only evidence that a row was deleted. If it is purged while some peer
still holds the pre-delete row, that peer can push the stale row back, and the row lives
again — with no timestamp left to arbitrate.

This is the hardest problem in the design, and it has a clean answer here because the
purge is scheduled rather than opportunistic.

### The invariant

> **Resurrection requires a peer's continuous downtime to exceed `retention`.**

### The argument

Let the delete occur at time `T`, with `purge_on = T + retention`. The tombstone is
purged at the first purge instant `P ≥ purge_on`.

For a peer B to resurrect the row, three things must hold:

1. B was offline when the delete happened, so it never received the tombstone.
2. B's copy of the tombstone was purged — that is, `P` has passed.
3. B reconnects and pushes its stale row.

For (1) and (3) together, B must have been offline since **before** `T`. Its last
successful sync was at some point `≤ T`. Its downtime at reconnect is therefore

```
downtime ≥ P - T ≥ retention
```

and strictly greater once we account for the delete happening *after* B's last sync
(`≥ retention + ε`).

So resurrection requires downtime greater than the retention window. ∎

### Why this matters

The staleness rule is not an extra burden layered on top of the purge design — it is the
thing that makes the retention window meaningful. Purging a tombstone is only safe
because any peer still holding the stale row is by definition too far behind to sync
incrementally.

## The staleness rule

Each node tracks, per peer, the last successful sync (`_sync_cursors.synced_us`). At
handshake, if

```
now - peer.last_sync_us > retention
```

the peer is **stale** and incremental sync is refused:

```
STALE_PEER: peer has been offline longer than the retention window;
it must re-seed (full resync) before incremental sync is allowed
```

Re-seeding means discarding the peer's local synced state and rebuilding it from the
peer. This is the "a server down for a month is a new installation" semantics.

### Mesh-wide, not per-link

In a master mesh the rule should be evaluated against the **mesh-wide** last sync rather
than per-peer, because a peer may have received the delete through a third node. A node
that has not synced with *anyone* for longer than `retention` is stale, regardless of
which individual link is quiet.

## The purge floor

Each node advertises `last_purge_instant` in the handshake. A row whose `hlc_us` is
**older than the receiver's `last_purge_instant`** is rejected:

> Such a row belongs to a pre-purge generation and must not be resurrected into a
> post-purge world.

This single rule handles both the missed-purge case and the clock-skew race where node A
has already purged and node B has not. B learns A's `last_purge_instant`, wipes itself,
and the two agree again.

Because `hlc_us` is the *writer's* clock, this comparison is subject to skew. ±1 s
against a 30-day horizon is irrelevant, but it does mean the floor is approximate. That
is acceptable, because the alternative — an exact per-key floor — is exactly the state
the scheduled purge exists to avoid.

## Retention versus the comparison tiers

`purge_on` is a **tombstone** lifetime, not a data lifetime. Live data never expires, so
the month tier stays populated indefinitely and the tier windows are purely a
comparison-cost policy:

| Tier | Depth | Range | Note |
|---|---|---|---|
| hot | hour | last 24 h | finest comparison |
| warm | cycle | 24 h … 90 d | |
| cold | month | beyond 90 d | always populated, since live data persists |

A consequence worth stating: if the retention is short (say one day), tombstones are
collected quickly and the tree is dominated by long-lived live rows. The cold tier
becomes large but cheap to compare at month granularity.

## Summary of the guarantees

| Guarantee | Mechanism |
|---|---|
| A delete propagates to peers | The tombstone is an ordinary row in the tree |
| Peers purge the same rows at the same moment | `purge_on` is replicated; the instant is globally scheduled |
| A purged tombstone does not resurrect | The staleness rule refuses any peer too far behind |
| A half-purged tree is never observed | The quiesce window around the purge instant |
| A node that missed a purge catches up | `last_purge_instant` comparison at handshake |
| Live data is never lost | Only `status = 'deleted'` rows are purged |

## Configuration

| Field | Default | Meaning |
|---|---|---|
| `retention` | 30 days | Tombstone lifetime, and the staleness horizon |
| `purge_period` | 24 hours | Spacing of the globally scheduled purge instants |
| `purge_quiesce` | 60 seconds | The window around each instant during which sync is refused |

All three are deployment-wide and **must** agree across peers: a mismatch in `retention`
or `purge_period` means peers disagree about when a tombstone expires, which is a
correctness bug rather than a policy difference.

## Related documents

- [protocol.md](protocol.md) — the handshake where staleness is detected
- [data-model.md](data-model.md) — the `status` and `purge_on` columns
- [merkle.md](merkle.md) — how the tombstone participates in the tree
- [adr/0005](adr/0005-soft-deletes-with-a-shared-purge-schedule.md)
