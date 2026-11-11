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

Around each instant is a **quiesce window** (default 60 s) during which sync of the
tables being purged is refused with `503 PURGE_IN_PROGRESS`:

```
T-ε        nodes compute the set of tables with tombstones due
           → that set is empty: no quiesce at all
           → otherwise: those tables refuse sync; other tables carry on
T          purge: remove rows with status='deleted' AND purge_on <= T
T+ε        nodes leave quiesce; sync resumes, trees are identical again
```

The window is **per table**, not global. A node whose tables have no tombstones due does
not quiesce, so the common case — a deployment whose tombstones do not fall on every
instant — has no sync outage at all. See [protocol.md](protocol.md) § "The purge
quiesce window" for how the due set is computed.

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

**Missing a purge instant is not a staleness failure.** It is a normal, recoverable
condition: the node catches up and continues in the same session. Only the retention
horizon (§ "The staleness rule") makes a peer stale, and only staleness forces a
re-seed.

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

**Staleness has exactly one definition in this design:**

```
now - last_sync_us > retention
```

A peer that has not completed a successful sync for longer than the retention window is
**stale**, and incremental sync is refused:

```
STALE_PEER: peer has been offline longer than the retention window;
it must re-seed (full resync) before incremental sync is allowed
```

The rule exists for **resurrection risk**, not for missed purges. A peer that missed a
purge instant catches up (§ "A node that was down across a purge instant") and carries
on in the same session. A peer that has been away longer than the retention window may
still hold a live row whose tombstone has been purged, and nothing can order those two
rows any more — so it re-seeds.

Each node tracks, per peer, the last successful sync (`_sync_cursors.synced_us`), and
the check is made at handshake.

### Mesh-wide, not per-link

In a master mesh the rule should be evaluated against the **mesh-wide** last sync rather
than per-peer, because a peer may have received the delete through a third node. A node
that has not synced with *anyone* for longer than `retention` is stale, regardless of
which individual link is quiet.

### What a re-seed does

A re-seed is a **merge, not a wipe**. The stale peer keeps what it has, pushes what only
it knows, and pulls everything else:

1. **Catch up on purges.** Purge locally to the peer's `last_purge_instant`, so the
   tombstones the peer has already removed are removed here too.
2. **Push.** Send the rows the peer has not acknowledged — those whose `last_update` is
   newer than this peer's cursor for that peer — under the table's merge policy
   ([data-model.md](data-model.md) § "Merge policies"). A child that was the primary
   for its own site while offline is the case this protects: without the push, its
   unsynced writes would be overwritten by the peer's older copies.
3. **Pull.** Receive the peer's full dataset, table by table, in pages
   ([protocol.md](protocol.md) § "Paging and message budgets"), applying each row under
   the table's merge policy.
4. **Keep.** Local rows the peer does not have are **kept**, not deleted. The only rows
   a re-seed removes are the tombstones removed in step 1 — live rows are never purged,
   so there is nothing else the peer can be authoritative about.

Nothing here is a "a server down for a month is a new installation" wipe. The peer keeps
its identity, its cursors and its local writes; it simply stops being trusted to sync
incrementally.

## The purge floor

Each node advertises `last_purge_instant` in the handshake. The floor is the rule that
stops a purged tombstone being resurrected by a peer that still holds it:

> **An incoming row is refused if, and only if, it is a tombstone whose `purge_on` is at
> or before the receiver's `last_purge_instant`.**

```
refuse(row) = row.status = 'deleted' AND row.purge_on <= receiver.last_purge_instant
```

Three cases, and the difference between them is the whole point:

| Case | Floor | Why |
|---|---|---|
| A live row, however old | **accepted** | Live rows are never purged by anyone. There is no tombstone to resurrect, so the floor has nothing to say about it. |
| A tombstone with `purge_on` in the future | **accepted** | The receiver has not reached the instant that would purge it. It is still live data. |
| A tombstone with `purge_on <= last_purge_instant` | **refused** (`ROW_PURGED`) | The receiver purged it. Accepting it would bring back a row that is gone everywhere. |

The comparison is on `purge_on`, **not** on the row's write timestamp. The write
timestamp says when the row was *written*; `purge_on` says when it *expires*. Comparing
a write timestamp against a purge instant rejects almost every live row — under a daily
purge, any row not modified since the last instant is older than the floor — which
breaks initial seeding, re-seeding and the catch-up of old rows.

The floor is the last line of defence. The first is the catch-up purge at handshake: a
node that learns a newer `last_purge_instant` purges locally before syncing, so the two
sides agree about which tombstones exist before any row is exchanged.

The floor itself is approximate, because `purge_on` is derived from the writer's clock.
A few seconds against a 30-day horizon is irrelevant; the floor's job is to catch a
tombstone that is obviously out of date, not to adjudicate a boundary case. A tombstone
whose `purge_on` falls within a skew-width of the instant is the only ambiguous case,
and the resolution is the same everywhere: `purge_on <= last_purge_instant` is refused,
and the node that still holds the tombstone purges it at the next instant.

## Decommissioning a site

A site is retired by soft-deleting its row in the `sites` registry — the same mechanism
as any other row, `status = 'deleted'`, with a `purge_on` set by the usual retention
policy.

**Its range is retired with it.** A retired range is never reissued, exactly like a
retired identifier. This is the reason the range is immutable in the first place: if a
site's range could be moved, and the site later came back with the same range, the
identifiers it had minted would collide with its own history.

A site row is a small, infrequent, administrative record, so it is a reasonable exception
to the purge schedule: it may be kept indefinitely. The `sites` table can be marked
`AppendOnly` — see [sqlite-interface.md](sqlite-interface.md) — so that retiring a site is
a soft delete that never becomes a physical removal.

Note the asymmetry this creates, and it is deliberate:

| | Identifiers | Site rows |
|---|---|---|
| Retired | Never reissued | Soft-deleted, may be retained forever |
| Purged | After the retention window | Only if the table is not `AppendOnly` |

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
| A purged tombstone does not resurrect | The staleness rule refuses any peer too far behind; the purge floor refuses a tombstone the receiver has already purged |
| A half-purged tree is never observed | The quiesce window, applied per table |
| A node that missed a purge catches up | The catch-up purge at handshake — **not** a fatal error |
| Live data is never lost | Only `status = 'deleted'` rows are purged |
| Live data is never refused | The purge floor applies to tombstones only |

## Configuration

| Field | Default | Meaning |
|---|---|---|
| `retention` | 30 days | Tombstone lifetime, and the staleness horizon |
| `purge_period` | 24 hours | Spacing of the globally scheduled purge instants |
| `purge_quiesce` | 60 seconds | The window around each instant during which the tables being purged refuse sync |

All three are deployment-wide and **must** agree across peers: a mismatch in `retention`
or `purge_period` means peers disagree about when a tombstone expires, which is a
correctness bug rather than a policy difference.

The quiesce window is **per table**, not global: a node enters it only for the tables
that actually have tombstones due at the instant, and a node with nothing due does not
quiesce at all. See [protocol.md](protocol.md) § "The purge quiesce window".

## Related documents

- [protocol.md](protocol.md) — the handshake where staleness is detected
- [data-model.md](data-model.md) — the `status` and `purge_on` columns
- [merkle.md](merkle.md) — how the tombstone participates in the tree
- [adr/0005](adr/0005-soft-deletes-with-a-shared-purge-schedule.md)
