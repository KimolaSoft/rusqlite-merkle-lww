# ADR-0001: `last_update` is the ordering key

- **Status:** accepted
- **Date:** 2026-11
- **Deciders:** project maintainers

## Context

Last-write-wins needs a total order on writes to the same key. The usual approaches:

- **A version key** — a per-row counter or UUID incremented on each write.
- **An HLC** — a hybrid logical clock combining physical time, a logical counter, and a
  node id.
- **A wall-clock timestamp** — the time of the last modification.

The original sketch for this project assumed a version key. The stated requirement was
that the tables already carry a `last_update` field holding a full RFC 3339 timestamp,
and that this is "inherently the precise time of the last modification".

There is a real argument for a version key: two peers can write the same key within the
same microsecond, and a plain timestamp does not order them. An HLC resolves this
without a schema change, since the logical counter can live outside the row.

## Decision

**`last_update` is the ordering key. There is no version key, no HLC counter, and no
writer id stored in the row.**

The ordering is:

```
order(row) = (last_update, digest)
```

The `digest` is consulted **only** on an exact microsecond collision. See ADR-0004.

### The monotonic issuance guard

Because timestamps from one node must never go backwards, each node persists one
scalar:

```
_sync_meta.last_issued_us : i64
```

and issues `last_update = max(wall_clock_now, last_issued_us + 1µs)`.

This is one integer per node, not per row. It exists because a clock step — an NTP
correction, a VM suspend — could otherwise let a node write a `last_update` older than
the row's current value. LWW would then discard the newer edit. The edit is not lost in
storage; it is lost in *ordering*, which is worse, because it is invisible.

With the guard, two writes by the same node never share a timestamp. The only remaining
collisions are cross-node, and those are resolved by the digest tie-break.

## Consequences

**Positive**

- The user's stated model is honoured: `last_update` is the ordering key.
- Rows remain human-readable. A version key would sit alongside a timestamp that is
  *not* the ordering key, which is a trap: someone would sort by the timestamp and get
  a subtly wrong order.
- No per-row storage overhead beyond the timestamp that was already there.
- The schema is what the application already expects.

**Negative**

- Ordering depends on clock accuracy between nodes. The requirement is ±1 s, which is
  documented and enforced by a skew check at handshake.
- A node whose clock is badly wrong can win conflicts it should lose. This is inherent
  to any timestamp-based LWW and is why the skew check refuses obviously-wrong peers.
- The exact-microsecond collision case is handled by a tie-break on the digest rather
  than by an ordering component in the row.

**Neutral**

- The `last_update` column is replicated and compared as text. Because it is fixed-width
  RFC 3339, lexicographic comparison is chronological comparison, so no parsing is needed
  on the hot path.

## Alternatives considered

**HLC with a per-row logical counter.** Rejected: it adds a column the application did
not ask for, and it makes the timestamp column no longer the ordering key — which is
exactly the confusion described above.

**A per-row UUID version key.** Rejected for the same reason, and it makes the row
larger and the merge rule harder to reason about.

**Timestamps only, with no tie-break.** Rejected: two peers writing the same key in the
same microsecond would never converge, because each would keep its own copy and neither
would know which was newer. The failure is permanent and silent.

**Rely on the site namespace to eliminate collisions.** Rejected: the namespace is part
of the key, so two peers at the *same* site can still write the same key concurrently,
which is precisely the case the mesh exists for.

## Related

- [../data-model.md](../data-model.md) — timestamps and canonical encoding
- [../protocol.md](../protocol.md) — the merge rule
- [ADR-0004](0004-digest-is-always-computed-and-keyed.md) — the tie-break
