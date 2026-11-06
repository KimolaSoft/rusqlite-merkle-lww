# ADR-0005: Soft deletes with a shared purge schedule

- **Status:** accepted
- **Date:** 2026-11
- **Deciders:** project maintainers

## Context

Deleted rows must replicate. A peer that was offline during a delete must learn about it.

The classic approach is a **tombstone**: a marker that outlives the delete and tells
peers the key is gone. Tombstones are typically aged out individually, after some
retention period.

That creates a known hazard: once a tombstone is gone, a peer still holding the
pre-delete row can push it back, and the row resurrects with no timestamp left to
arbitrate. The usual fix is a per-key "absence floor" — extra state, per key, recording
how far back deletions are known.

The requirement stated for this project was:

> Deletions will be handled by a configurable purge_on item that defaults to 30 days.

and, on the purge mechanics:

> The purge_on metric just means at ... 12:01 or something all nodes wipe everything.
> Then refuse to sync until 12:02 causing a single minute of forced downtime for
> reconciliation or something.

That description is a **globally scheduled purge**, not per-row ageing.

## Decision

**Deletions are soft. Tombstones are purged at a globally scheduled instant, on a
schedule all nodes compute identically.**

- A delete sets `status = 'deleted'` and `purge_on = now + retention`, and bumps
  `last_update`. The row stays in the table.
- `status` and `purge_on` are **replicated** columns.
- Live rows are **never** purged.
- Tombstones are physically removed at instants derived from the shared epoch:
  `E' + k * purge_period` (default: daily at an epoch-aligned UTC instant).
- A **quiesce window** (default 60 s) around each instant refuses sync with
  `503 PURGE_IN_PROGRESS`.
- The staleness rule refuses incremental sync to any peer offline longer than
  `retention`.

**Deleted rows replicate their payload in full**, in the same bucket as everything else.

## The invariant that makes it safe

> Resurrection requires a peer's continuous downtime to exceed `retention`.

*Proof.* Let the delete occur at `T`, with `purge_on = T + retention`, purged at the
first instant `P ≥ purge_on`. For a peer B to resurrect the row, B must have been
offline when the delete happened (so it never got the tombstone) and must still be
offline when `P` passes. Its last successful sync was therefore at some point `≤ T`,
and its downtime at reconnect is `≥ P - T ≥ retention`, strictly greater once the
delete is accounted for as happening after that last sync. ∎

The staleness rule refuses exactly those peers. So the two mechanisms are a matched
pair: the scheduled purge makes tombstones collectable, and the staleness rule ensures
no peer is in a position to resurrect one.

## Consequences

**Positive**

- **No per-key absence floors.** The state that would otherwise be needed to prevent
  resurrection is replaced by one rule and one per-peer timestamp.
- **No clock-dependent judgements.** A tombstone is not "29.4 days old, so almost
  expired". Every node purges the same set at the same instant, computed from the shared
  epoch.
- **The purge is atomic from the outside.** The quiesce window means no peer observes a
  half-purged tree.
- **Live data is never lost.** Only `status = 'deleted'` rows are purged, so the tier
  structure stays meaningful and the cold tier remains populated.
- **Deleted rows are diagnosable.** Replicating the payload means that when a record is
  unexpectedly gone, the question "what did it contain?" can be answered from any peer,
  in one hop. A few bytes per deleted row is a trivial cost for that.

**Negative**

- **A peer offline longer than the retention window must re-seed.** This is the
  "a server down for a month is a new installation" rule, and it is load-bearing rather
  than merely conservative.
- **The quiesce window is a brief sync outage.** At most `purge_quiesce` seconds per
  purge instant, by default 60. During it, sync returns 503.
- **The `purge_on` deadline is replicated, so it must agree across peers.** A mismatch
  means peers disagree about when a tombstone expires.
- **A purge is not instantaneous across the fleet.** Nodes purge at the same instant but
  the operation is not transactional across machines. The quiesce window and the
  `last_purge_instant` handshake handle the residue.
- **A tombstone that reappears is purged at the next instant.** If a node was offline
  across a purge and comes back, it purges locally first, so its stale tombstones do not
  circulate.

**Neutral**

- The tombstone is an ordinary row in `_sync_log` and an ordinary member of the tree.
  Deletes propagate through the same diff machinery as any other change; there is no
  separate tombstone stream.

## Alternatives considered

**Per-row tombstone ageing.** Rejected: whether a tombstone is expired depends on the
local clock, so nodes purge at slightly different times, and the tree churns as
tombstones appear and disappear. It also requires per-key absence floors to prevent
resurrection.

**Keep tombstones forever.** Rejected: unbounded growth, and it is not necessary — the
staleness rule already bounds how long a peer can be behind.

**Hard deletes with a separate tombstone table.** Rejected: the row's contents are lost
immediately, so "what did it contain?" becomes an archaeology problem. Soft deletion
keeps the bytes until the purge.

**Do not replicate deleted-row payloads.** Rejected: the diagnostic value outweighs the
bandwidth, and it avoids a special case in the digest (the digest covers the row's
content as it is, deleted or not).

**Purge immediately on delete.** Rejected: peers offline at that moment would never
learn of the deletion, and their stale copies would be authoritative forever.

## Related

- [../lifecycle.md](../lifecycle.md) — the full argument and the guarantee table
- [../data-model.md](../data-model.md) — the `status` and `purge_on` columns
- [../config.md](../config.md) — retention, purge period, quiesce window
