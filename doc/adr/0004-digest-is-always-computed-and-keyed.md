# ADR-0004: The digest is always computed, and keyed

- **Status:** accepted
- **Date:** 2026-11
- **Deciders:** project maintainers

## Context

The design needs a per-row hash for four distinct purposes:

1. **Bucket hashing** — the leaf value folded into the tree.
2. **Tie-break** — ordering two writes with the identical `last_update`.
3. **Divergence detection** — noticing that two peers hold different bytes under the
   same timestamp.
4. **Role-independent tree parity** — letting a restricted client compute the same tree
   hash as a master without holding the master's restricted values.

There was a proposal to make content hashing an **option**, on the reasoning that for
low-write-rate applications the extra accuracy is unnecessary. The question asked was:

> Why would we hash a row when last_update is already accurate to microseconds?

The answer is that precision and hashing are unrelated. Microsecond-accurate timestamps
make the *order* total; they say nothing about whether two peers hold the same *bytes*
under the same timestamp.

Two peers holding different content under an identical timestamp happens when:

- a row is edited without advancing `last_update` (a direct SQL write, an ORM bypass, a
  bug);
- the monotonic issuance counter is lost and re-allocated, producing an identical
  timestamp for different content;
- a bug in the write path serialises two different rows identically.

In every case LWW says "same version, no conflict", and the peers diverge **permanently
and silently** — nothing ever reports it.

## Decision

**The digest is always computed.** It is not an option.

```
digest   = seeded_xxh3(seed, digest_input)
row_hash = xxh3(key_enc || last_update_enc || status_enc || purge_on_enc || digest)
```

`digest_input` is defined in [../data-model.md](../data-model.md) § "The digest input":
the table name, the canonical key, the three managed columns, and then every remaining
replicated, non-local column in the registered order.

**The digest is keyed** with a deployment secret (`seed`), using seeded xxh3.

The secret is shared by every node that computes a digest — all trusted peers, **including
children**, which write rows — and is **absent from clients**. It is a third value,
separate from the two peer credential strings, and it is rotated independently of them;
deriving it from a credential would couple a routine credential rotation to a full
dataset rebuild. See [ADR-0014](0014-peer-credentials-and-the-uuid-directory.md).

The digest is 16 bytes, big-endian on the wire, and compared as an unsigned 128-bit
integer.

## Consequences

**Positive**

- **Tie-break works.** On an exact `last_update` collision, the digest breaks the tie
  deterministically. This is a routine case, not a theoretical one: the clock underneath
  may be coarser than our microsecond timestamps, and when two nodes write at the same
  instant they have written *different data*. Both peers hold the same digest, so both
  reach the same conclusion.
- **Divergence is detectable.** `verify` reports rows whose stored digest disagrees with
  a recomputed one.
- **No negotiation.** Because the digest is always computed, peers cannot disagree about
  whether content is hashed. The handshake loses a failure mode, and the protocol has one
  fewer parameter.
- **The two-level row hash gives tree parity.** A client receives the digest and folds
  it into the same `row_hash` formula. It never computes a digest itself, and never
  holds a restricted value, yet arrives at the same bucket hash as a master.
- **Keying defeats brute-force confirmation.** A restricted column holding a boolean
  would otherwise be confirmable: submit a candidate, watch the bucket hash change.
  Keying makes the digest a function of a secret the client does not have.

**Negative**

- **One hash per write.** A write to a row costs one xxh3 over its replicated content.
  For high-write-rate tables this is measurable, though xxh3 is fast enough that it is
  rarely the bottleneck.
- **16 bytes per row** in `_sync_log`.
- **The digest seed is deployment-wide shared state.** A mismatch between trusted peers
  is fatal at handshake (`DIGEST_SEED_MISMATCH`), and rotation requires a full re-digest
  — effectively a full re-seed. Rotation is not supported in v1.
- **Computing the digest in a trigger requires a Rust UDF**, which constrains the
  compatible dialect: pure-SQL triggers cannot compute it, so those rows must be staged
  for a Rust drainer. See [ADR-0005](../sqlite-interface.md) and the two-dialect
  discussion.

**Neutral**

- The digest covers `status` and `purge_on`, so a delete changes the row hash exactly
  like any other edit, and a purge changes it again.

## Alternatives considered

**Make content hashing optional.** Rejected: it introduces a protocol parameter that
must be negotiated, and a mismatch means two peers compute different hashes for
identical rows — which is a permanent diff loop, not a degraded mode. The cost of
always-on is one hash per write; the cost of optional is a whole class of bug.

**Hash only the key and timestamp, and rely on `last_update` alone.** Rejected: this is
the same as no tie-break, and it makes undetectable divergence permanent.

**Use an unkeyed digest.** Rejected: it makes low-entropy restricted values confirmable
by any client, which defeats the visibility model.

**Hash the payload but not the metadata.** Rejected: the digest must cover `status` and
`purge_on`, or a delete and an un-delete of the same payload would hash identically.

**Use BLAKE3 instead of xxh3.** Rejected for now: xxh3 is faster and the digest is not
used for anything requiring cryptographic strength — the keying is there to prevent
confirmation, not to resist a targeted attack. BLAKE3 remains a reasonable swap if
that assumption changes.

## Related

- [../data-model.md](../data-model.md) — the digest and canonical encoding
- [../security.md](../security.md) — the visibility model the keying protects
- [ADR-0001](0001-last-update-is-the-ordering-key.md) — the ordering key
