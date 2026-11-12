# Fixtures

The published vectors for this crate live here.

**The crate that defines a format publishes its vectors.** `merkle-lww-model` defines
the canonical encoding, the digest, the `schema_hash` and the `visibility_hash`, so it
publishes the vectors for all four. `merkle-lww-protocol` publishes the wire vectors,
and `merkle-lww-demo` publishes the topologies and the scenarios.

Every fixture is JSON, so it can be shared with an implementation in another language.
That is the whole point of specifying the encodings precisely: a second implementation
that has never heard of this workspace should be able to reproduce these bytes and
check itself against them.

| Fixture | Stage | What it pins |
|---|---|---|
| `golden_vectors.json` | M2 | the canonical encoding and the digest |
| `schema_hash_vectors.json` | M3 | the `schema_hash` |
| `visibility_hash_vectors.json` | M3 | the `visibility_hash` |

These are **checked-in fixtures, not snapshots**. A change to one is a diff a reviewer
sees, not a snapshot that is regenerated on demand — which is the opposite of
`insta`, and deliberate.
