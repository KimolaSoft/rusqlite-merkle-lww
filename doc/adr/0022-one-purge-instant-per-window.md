# ADR-0022: One purge instant per window, and a tunable offset

- **Status:** accepted
- **Date:** 2026-11
- **Deciders:** project maintainers

- **Partially supersedes:** [ADR-0005](0005-soft-deletes-with-a-shared-purge-schedule.md)

## Context

[ADR-0005](0005-soft-deletes-with-a-shared-purge-schedule.md) decided that deletions are
soft and that tombstones are purged at a globally scheduled instant. Two details of the
mechanics are worth revisiting.

**First, `purge_on` was per-row.** The rule was `purge_on = now + retention`, so every
tombstone carried its own deadline, and the purge at an instant was a range predicate:

```
purge: remove rows with status='deleted' AND purge_on <= T
```

The set due at an instant therefore depended on when each row was deleted, not on the
schedule. Nodes still agreed — `purge_on` is replicated — but the agreement came from
replicating a value rather than from arithmetic.

**Second, the schedule had no offset.** The instants were `E' + k * purge_period`,
anchored to the epoch, with no way for a deployment to move the purge off the working
day. A deployment that wants a 03:00 local purge has to choose an epoch that produces
it, which then also fixes the Merkle bucket boundaries to the same clock.

## Decision

**Every tombstone created in the same window has the same `purge_on`, and the schedule
carries a deployment-wide offset.**

A **window** is the interval between two consecutive purge instants. A delete belongs to
the window it falls in, and every tombstone in a window shares one deadline:

```
purge instants = E' + k * purge_period + purge_offset_us

g_next(d)      = the first purge instant strictly after the delete time d
purge_on(d)    = the first purge instant at or after (g_next(d) + retention)
```

Two properties of this formulation, and both matter:

- **It is uniform, unconditionally.** Every delete in the window `[g_k, g_{k+1})` has
  the same `g_next`, so it has the same `purge_on`. The rule does not depend on
  `retention` being a whole number of periods, which a simpler
  "round the deadline up to the grid" rule would.
- **The tombstone lives at least `retention`.** `g_next(d) > d`, so
  `purge_on > d + retention`. The resurrection bound is therefore unchanged: a peer has
  to be disconnected for longer than the retention window before it can miss the
  evidence.

The purge at an instant `T` removes everything due:

```
purge: remove rows with status='deleted' AND purge_on <= T
```

In steady state that is exactly the rows with `purge_on = T`, because the earlier
deadlines were purged at their own instants. The `<=` is what makes a node that missed
an instant catch up: its backlog is purged too, rather than orphaned.

Two properties follow from `purge_on` being uniform:

- **The due set is identical on every node by arithmetic.** Every node computes the
  same instants from the same epoch, and every tombstone's `purge_on` is one of those
  instants. Nothing has to be agreed about at runtime.
- **The purge is one indexed range scan.** `_sync_log` is indexed on `purge_on`
  (and `purge_on_us` is NULL while a row is live, which is what makes "which tables
  have tombstones due at `T`" an existence query).

`purge_offset_us` is the **offset within `purge_period`** at which the instant falls.
It defaults to the epoch-aligned instant.

**`purge_offset_us` is deployment-wide.** Peers that disagree about it purge at
different moments, which is a correctness bug rather than a policy difference, so it
sits in the same class as `epoch_us`, `retention` and `purge_period`: it must agree
across peers, and a mismatch is rejected at handshake with
`PURGE_SCHEDULE_MISMATCH`.

**`purge_on` is the engine's.** It is set when a row becomes deleted, cleared to NULL
when it becomes live again, and a direct write to it is denied. A live row has no
`purge_on`.

## Consequences

**Positive**

- **The purged set is computed, not negotiated.** Every node derives the same set from
  the same schedule.
- **The purge is a constant predicate**, which is what makes it a single bulk `DELETE`
  rather than a scan.
- **A deployment can choose when the purge happens** without disturbing the bucket
  boundaries, because the offset is independent of the epoch.
- **The tombstone's lifetime is uniform**, which makes the schedule's behaviour easy to
  reason about: every tombstone in a window lives for the same span.

**Negative**

- **The tombstone lifetime is now quantised.** A tombstone lives between `retention`
  and `retention + purge_period`, depending on where in its window the delete fell. The
  window is the unit, not the row.
- **The stored `purge_on` values change shape**, from a per-row instant to one of a
  small set of schedule instants. A deployment that already has data must re-derive
  them.

**Neutral**

- `purge_on` remains a replicated column, and the digest still covers it.
- The quiesce window is unchanged: the purge still changes the tree, so the window
  still makes it atomic from the outside.
- The staleness rule is unchanged.

## Alternatives considered

**Keep `purge_on = now + retention`, per row.** Rejected: the due set then depends on
each row's delete time, so the purge is a range predicate and the "same set on every
node" property comes from replication rather than from the schedule. The uniform form
is simpler to compute, simpler to index, and easier to reason about.

**Round the deadline up to the grid: `purge_on = first instant ≥ (delete_time +
retention)`.** Rejected: it looks simpler, but it is only uniform when `retention` is a
whole number of `purge_period`. With a retention of 30 days and a period of 7 days, two
deletes an hour apart can land on different deadlines, and the property the whole
decision rests on quietly stops holding. Quantising the **delete time** to its window
first is uniform for every ratio of retention to period.

**Floor the deadline to the grid.** Rejected for a different reason: it would let a
tombstone be collected up to one period *before* its retention had elapsed, which
weakens the resurrection bound by exactly that period.

**Keep the instant but let each deployment choose a wall-clock time of day.** Rejected:
a time of day is ambiguous across daylight-saving transitions and across jurisdictions,
and it does not compose with a `purge_period` that is not a whole number of days. An
offset within the period has neither problem.

**Move the epoch to move the purge.** Rejected: the epoch also fixes the Merkle bucket
boundaries. Coupling the two would mean a deployment could not choose its purge time
without also moving its bucket grid.

**Add a purge generation counter instead of a timestamp.** Rejected: a counter is
opaque across nodes — a peer cannot tell whether its generation is current — and it
cannot express the "how long has this tombstone been collectable" question the
retention horizon asks.

## Related

- [ADR-0005](0005-soft-deletes-with-a-shared-purge-schedule.md) — the soft-delete and
  purge schedule decision
- [../lifecycle.md](../lifecycle.md) — the purge instant and the purge floor
- [../config.md](../config.md) — `purge_offset_us` and the values that must agree
