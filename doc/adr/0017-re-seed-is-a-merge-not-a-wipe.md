# ADR-0017: Re-seed is a merge, not a wipe

- **Status:** superseded by [ADR-0021](0021-re-seed-is-a-wipe.md)
- **Date:** 2026-11
- **Deciders:** project maintainers

> **Superseded.** [ADR-0021](0021-re-seed-is-a-wipe.md) decides that a re-seed is a
> **wipe**. The merge is retained below as the record of the reasoning that led to it,
> and the reasoning that overturned it.

## Context

The staleness rule refuses incremental sync to a peer that has been offline longer than
the retention window. The remedy was described as a **re-seed**:

> Re-seeding means discarding the peer's local synced state and rebuilding it from the
> peer. This is the "a server down for a month is a new installation" semantics.

That is right for a peer that is a **pure replica** — a read-only client has nothing of
its own to lose, and rebuilding it from scratch is simple and safe.

It is wrong for a peer that is a **primary in its own right**, which is exactly what a
child is. A child is the primary for its own site: it accepts writes for that site's
rows while it is up, and it keeps accepting them while it is **down** — that is the
point of a primary. When it comes back after a long outage, it holds writes that the
master has never seen, and the master holds older versions of some of the same rows.

A wipe discards exactly those writes. The child comes back, pulls the master's
dataset, and the site's own unsynced work is gone — silently, because from the
master's point of view the pull succeeded.

The other half of the problem is tombstones. The stale peer missed the purge instants
that happened while it was away, so it still holds tombstones that have been purged
everywhere else. Those must go — but **only** those. Live rows are never purged, so
there is no other class of row the peer is authoritative about.

## Decision

**A re-seed is a merge, not a wipe.** The stale peer keeps what it has, pushes what
only it knows, and pulls everything else.

1. **Catch up on purges.** Purge locally to the peer's `last_purge_instant`, so the
   tombstones the peer has already removed are removed here too. This is the same
   catch-up purge as an ordinary missed-purge handshake — a re-seed is not a special
   case of it.
2. **Push.** Send the rows the peer has not acknowledged — those whose `last_update`
   is newer than this peer's cursor for that peer — under the table's merge policy. The
   peer applies them; they are usually newer than what it holds, so they win.
3. **Pull.** Receive the peer's full dataset, table by table, in pages, applying each
   row under the table's merge policy.
4. **Keep.** Local rows the peer does not have are **kept**, not deleted. The only rows
   a re-seed removes are the tombstones removed in step 1.

The order matters: **push before pull**. If the stale peer pulled first, the master's
older copies of the rows the child had just written would come back and win or lose by
LWW, and the child's writes would be resolved twice for no benefit. Pushing first means
the master has the child's rows before it sends its own view back.

## Consequences

**Positive**

- **A primary keeps its work.** The case that made the wipe unacceptable — a child
  that accepted writes while offline — is now safe by construction.
- **One mechanism, not two.** The catch-up purge in step 1 is the same operation as
  the missed-purge catch-up. A re-seed adds a push and a pull, not a new purge path.
- **No data is discarded silently.** Anything the stale peer had and the master does
  not have is either pushed to the master (step 2) or kept (step 4). There is no third
  outcome.
- **The client case is unchanged.** A read-only client has no local writes, so its
  re-seed is a pull — the same sequence with an empty push.

**Negative**

- **The re-seed is no longer idempotent in the "discard and rebuild" sense.** It
  merges, so the result depends on both sides' contents. This is the point, but it
  means a re-seed is no longer a way to *reset* a node to a known state.
- **The push can be large.** A site that was offline for a month with a busy write
  path has a lot to push. That is the honest cost of the work it did.
- **A stale peer can be stale for a reason the master cannot see.** The master accepts
  the push under the table's merge policy, so a table with a custom policy may reject
  some of it — correctly, and visibly.

**Neutral**

- The staleness rule itself is unchanged: `now - last_sync_us > retention` still
  refuses incremental sync, and a stale peer still re-seeds.
- Rows the stale peer has that the master has **and** that are older are resolved by
  LWW as usual.

## Alternatives considered

**Keep the wipe, and require a primary to push before being re-seeded.** Rejected: it
makes the safety of a primary depend on the operator remembering a step, and the
failure mode is data loss.

**Refuse to re-seed a peer that has local writes.** Rejected: it leaves the peer
stuck — it can never rejoin, because it is exactly the peer that has something worth
keeping.

**Let the master push to the stale peer and let LWW decide.** Rejected: without a
push from the child, the master's older copy of the child's row wins or loses by
timestamp, and the child's write can be lost without anybody noticing. The push makes
the child's write part of the master's state before the pull begins.

**Purge the stale peer completely and re-fetch.** Rejected: that is the wipe again,
and it discards live rows the master never had.

**Keep per-key absence floors instead.** Rejected for the same reason as in
[ADR-0005](0005-soft-deletes-with-a-shared-purge-schedule.md): it is per-key state
that the scheduled purge exists to avoid.

## Related

- [../lifecycle.md](../lifecycle.md) — the staleness rule and what a re-seed does
- [../protocol.md](../protocol.md) — bulk seeding, paging
- [../data-model.md](../data-model.md) — merge policies
- [ADR-0005](0005-soft-deletes-with-a-shared-purge-schedule.md) — the purge schedule
