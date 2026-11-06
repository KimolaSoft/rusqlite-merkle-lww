# ADR-0003: Absolute bucket boundaries, not rolling tiers

- **Status:** accepted
- **Date:** 2026-11
- **Deciders:** project maintainers

## Context

The Merkle tree buckets rows by their `last_update`. The original design was:

> bucketizing on that by hourly for 24 hours, then daily for 3 months, then monthly
> beyond that

Read one way, that is a **rolling** structure: a bucket's granularity depends on how old
it is. An hour-bucket becomes a day-bucket after 24 hours, and a day-bucket becomes a
month-bucket after three months.

There is a subtle problem with that reading, and it surfaced during design discussion as
a proposal to use **25-hour cycles** instead of 24-hour ones:

> with 25h the 'daily' rollover of 24h leaves one hour of 'tomorrow' which became 'today'
> meaning the prior 24 hours can safely be rolled up while still retaining a single hour
> of buffer for 'now' changes.

The buffer is not a refinement. It is a **patch for a defect**. It exists because in a
rolling structure, a bucket's hash changes when it changes granularity — for reasons
unrelated to any data change. Every peer sees a phantom difference at every transition.
The buffer reduces the frequency of those transitions but does not remove them.

## Decision

**Bucket boundaries are absolute and epoch-anchored. A row's bucket depends only on its
`last_update` and never changes identity.**

```
hour_id   = floor((t - E) / 3_600_000_000)
cycle_id  = floor(hour_id / 24)
month_id  = floor(cycle_id / 30)
```

`E` is a shared epoch, identical on every node.

Age then selects only the **comparison depth** — the level at which two trees are first
compared:

```
age < 24h    → hour
age < 90d    → cycle
otherwise    → month
```

Drilling below the chosen depth is always permitted when a coarse comparison differs. The
depth is a fast path for skipping *equal* ranges, not a limit on resolution.

## Consequences

**Positive**

- **No churn.** A bucket hash changes only when its contents change. There is no
  transition moment to miss and no buffer to maintain.
- **Late writes are always detected.** In a rolling structure, a write arriving after a
  bucket has rolled up changes a hash that peers have already agreed on, and the
  detection granularity has coarsened. With absolute boundaries, the write lands in its
  own hour bucket and is detected at full resolution.
- **The behaviour matches the original intent** — hourly for recent data, daily for
  medium, monthly for old — without the phantom differences.
- **The 25-hour proposal is unnecessary.** The buffer it provided is not needed,
  because there is no rollup.

**Negative**

- **All three levels are always stored.** Memory is bounded by *activity* — the number
  of distinct hours in which a row was modified — rather than by the tier structure. At
  64 bytes per entry, 10 years of continuous activity is about 5.6 MB per table. This is
  acceptable, but it is memory the rolling design would have avoided.
- **The tier policy is a second concept.** Absolute boundaries plus a comparison-depth
  policy is two ideas where the original had one. It is more machinery, but each part is
  simpler than the rolling alternative.
- **`epoch_us` becomes deployment-wide shared state** that cannot change once data
  exists. Changing it re-buckets every row.

**Neutral**

- A row modified after 183 days moves from an old month block to the current hour
  block, changing two bucket hashes. Both changes are real, and the diff finds the row
  at its new location.

## Alternatives considered

**Rolling tiers with a 24-hour cycle.** Rejected: phantom differences at every
transition.

**Rolling tiers with a 25-hour cycle.** Rejected: it reduces the transition frequency
but does not remove the problem, and it makes the cycle length a strange 25 hours, which
complicates the calendar alignment that the shared epoch already provides.

**A single flat granularity everywhere.** Rejected: an hour-bucket everywhere means
comparing 87,660 buckets for ten years of history instead of 122 month blocks. The
comparison-depth policy exists precisely to avoid that, while keeping the underlying
structure simple.

**Keep rolling tiers but re-hash on transition.** Rejected: it amounts to the absolute
scheme with extra steps, and it introduces a moment where the tree is not comparable.

## Related

- [../merkle.md](../merkle.md) — bucket definitions, depth policy, memory
- [../config.md](../config.md) — `epoch_us` as shared state
