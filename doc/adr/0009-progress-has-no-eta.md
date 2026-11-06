# ADR-0009: Progress has no ETA

- **Status:** accepted
- **Date:** 2026-11
- **Deciders:** project maintainers

## Context

A sync can take a long time on a large dataset, and someone waiting for it wants to know
whether to keep waiting. The requirement was:

> Sync 'progress' needs a quantifier, whatever it is, X of Y such that we can present
> sync status. Estimating times might be hard but at least some value helps people to
> wait longer.

So: an X-of-Y quantifier is required. The question is whether the protocol should also
carry an **ETA** — a predicted completion time.

## Decision

**The protocol reports X of Y, with `Y` always known before the phase starts. There is no
ETA field.**

```rust
pub struct SyncProgress {
    pub session_id:  Uuid,
    pub peer:        Uuid,
    pub phase:       Phase,
    pub completed:   u64,
    pub total:       u64,           // always known
    pub unit:        Unit,          // Buckets | Rows
    pub bytes:       u64,
    pub started_at:  Instant,
    pub updated_at:  Instant,
}
```

A consumer that wants an estimate derives one from `completed` and `elapsed`, and
presents it as approximate. That is a display concern, not a protocol concern.

### The denominators

| Phase | Unit | Y is |
|---|---|---|
| Comparing | buckets | total buckets in the requested range |
| Expanding | buckets | differing buckets from the comparison phase |
| Transferring | rows | sum of row counts over the differing leaf buckets |
| Applying | rows | rows received |

Each is known before the phase starts: the bucket count in a range is computable from the
epoch and the range; the differing-bucket count comes from the comparison phase; the row
count comes from the bucket metadata in the DETAIL response.

## Consequences

**Positive**

- **The number is always real.** A denominator computed before the phase runs is a fact,
  not an estimate. There is no "indeterminate" state.
- **The progress is meaningful across phases.** The unit changes — buckets to rows — and
  each phase's denominator is exact.
- **No misleading number.** A consumer can compute whatever it wants and decide how to
  present it.
- **The protocol stays simple.** One field pair, one unit, no estimation logic to
  specify, implement, or agree on.

**Negative**

- **Every consumer that wants an ETA writes the estimation logic.** That is duplication
  across consumers, and each will get the smoothing wrong differently.
- **A consumer that shows nothing but a counter gives the user less to go on.** "4,200 of
  12,400 rows" is less reassuring than "about 40 seconds left". The requirement
  explicitly asked for something to help people wait.
- **No cross-consumer consistency.** Two UIs showing progress from the same node may
  display different estimates.

**Neutral**

- The byte count is exact and is in the protocol. A consumer that cares about volume
  rather than time has what it needs.

## Alternatives considered

**Include a best-effort ETA in the protocol.** Rejected: the estimate is computed from a
rate over an incomplete sample, and it is wrong in exactly the situations where it
matters — a slow transfer, a stalled peer, a comparison phase that is taking longer than
expected. A misleading ETA is worse than none, because it makes people wait when they
should give up, or give up when they should wait.

**Report a rate instead of an ETA.** Rejected: the rate is equally derivable from
`completed` and `elapsed`, and it has the same problem of being meaningless in the first
few seconds.

**Make the denominator optional.** Rejected: `total: Option<u64>` invites `None`, and a
progress bar that cannot say how much is left is a spinner. Every denominator in this
design is knowable in advance, so there is no reason to allow the indeterminate case.

**Report progress as a percentage.** Rejected as the primary representation: it loses
the absolute magnitude. "43%" of 40 rows and 43% of 4 million rows are very different
situations. The absolute count is available and the percentage is trivial to derive.

## Related

- [../protocol.md](../protocol.md) — the progress model and the endpoints
- [../merkle.md](../merkle.md) — where the row counts come from
