# Plan — `merkle-lww-model`

**Crate.** `crates/merkle-lww-model` · depends on `serde` and nothing else.

**Goal.** The vocabulary: everything that must mean the same thing on every node,
every peer, and every implementation.

This crate holds the rules that two independently-implemented peers must agree on
byte for byte — the canonical encoding, the digest, the schema hash, the visibility
hash — together with the configuration types, the merge rules, the timestamp rules and
the error codes.

It is pure. No SQLite, no runtime, no locks, no interior mutability. Everything in it
is an ordinary function over ordinary data.

**Why it is its own crate.** Two reasons, and both are load-bearing:

- **The compatibility surface must be testable without a database, and portable to
  another language.** The golden vectors for the digest are consumable by an
  implementation that has never heard of `rusqlite`. That is the point of specifying an
  encoding precisely, and it cannot be true if the encoder is welded to a SQLite
  dependency.
- **The extension point must not carry the engine's weight.** `MergePolicyFn` is what
  an *application* implements, and it is a pure function of
  `(incoming, local) -> MergeDecision`. Under a single-crate layout, writing one meant
  depending on `rusqlite`, `r2d2` and the whole engine.

See [ADR-0024](../doc/adr/0024-the-workspace-is-split-into-six-crates.md).

## Contents

| Module | Holds |
|---|---|
| `config.rs` | `SyncConfig`, `TableSpec`, `ColumnSpec`, `ColumnType`, `KeyStrategy`, `Visibility`, `TableKind`, `MergeMode`, `MergePolicy`, `WriteAuthority`, `Peer`, `PeerAuth`, `CacheConfig`, `Role`, `WriteMode`, `AllocationSpec`, `SecretString` |
| `merge.rs` | `MergePolicy`, `MergePolicyFn`, `MergeContext`, `MergeDecision`, the write-authority rule |
| `visibility.rs` | `VisibilityProfile`, `ProfileTable`, the projection resolver, the `visibility_hash` |
| `codec.rs` | the canonical encoding, the seeded xxh3 digest, the two-level `row_hash`, the codec version |
| `schema.rs` | the `schema_hash` |
| `time.rs` | RFC 3339 parsing and formatting at fixed six-digit precision, the monotonic issuance guard |
| `error.rs` | the error type and the error codes |

Everything here is `serde`-derived with `Default`.

---

## M1 — Types, errors and time

### Deliverables

- `config.rs`: every configuration type in the table above, `serde`-derived with
  `Default`.
- `error.rs`: the error type, including the error codes.
- `SecretString`: a wrapper whose `Debug` does not print its contents.
- `time.rs`: RFC 3339 parsing and formatting at fixed six-digit precision; the
  monotonic issuance guard.

### Notes

- **The crate persists no secret material.** The digest seed, the role strings and the
  peer list are configuration, held in memory for the life of the process. Nothing
  secret is written to the database, and the crate implements no rotation.
- **`SecretString` is the only way to hold a secret.** Its `Debug` prints a redacted
  marker, and it is the type every configuration field that can carry a credential
  uses. The audit — that no credential appears in a log line or an error message — is
  the [CLI](cli.md)'s exit criterion, because that is where the output is.
- **Timestamps are microseconds, always.** The canonical form is RFC 3339 with exactly
  six fractional digits, so the lexicographic order of two formatted timestamps is
  their chronological order. That property is load-bearing: the Merkle buckets are
  ordered by timestamp, and a format whose sort order is not chronological would
  silently misplace rows.
- **The monotonic issuance guard** issues a timestamp strictly greater than the last
  one issued anywhere in the process, so two writes in the same microsecond still order
  deterministically.

### Exit criteria

- `cargo clippy` is clean.
- Timestamps round-trip through parse and format at fixed six-digit precision, and sort
  lexicographically in chronological order.
- `SecretString`'s `Debug` output contains no part of the secret.
- Every configuration type round-trips through `serde` — a `Default` value serialises and
  deserialises to itself. This is what makes a config file round-trippable.

---

## M2 — The canonical encoding and the digest

### Deliverables

- `codec.rs`: the canonical encoding — one tagged-value rule for both keys and values;
  the seeded xxh3 digest; the two-level `row_hash`; the codec version.

### Notes

- **One rule for keys and values.** The key encoding is the value encoding applied to
  each key column, in registered order. `userid = 10001` therefore produces the same
  tagged bytes in `_sync_log.k`, in the digest input, and on the wire.
- **The digest is keyed.** The seed never leaves the process, so a client cannot
  recompute a digest and cannot brute-force one from a low-entropy value.
- **`row_hash` is two-level**: the digest of the row, and the hash of the digest. The
  tree is a function of the digest and not of the payload, which is why a peer with a
  narrow visibility profile has a matching tree.
- **The codec version is part of the schema hash.** A change to the encoding rules
  changes every digest, so it is not a silent change.

### Exit criteria

- Canonical encoding golden vectors pass, and are published as a fixture.
- The encoding is injective on a generated set of values: two distinct rows never encode
  to the same bytes (`proptest`).
- **The key encoding is the value encoding**: `userid = 10001` produces the same tagged
  bytes in `_sync_log.k`, in the digest input, and on the wire — and the same rule
  covers a composite key.
- **The derived sync id is deterministic**: the same identity columns produce the same
  id on every node, and different identity columns produce different ids.

**Test suites:** 14 (canonical encoding).

---

## M3 — The schema hash and the visibility hash

### Deliverables

- `schema.rs`: the `schema_hash` — exactly the inputs to the row digest, including the
  registered column order.
- `visibility.rs`: `VisibilityProfile`, `ProfileTable`, the projection resolver, and the
  `visibility_hash`.

### Notes

- **`schema_hash` covers exactly the digest's inputs**: key strategy, key columns,
  replicated column names **and types** in registration order, managed column names, and
  `codec_version`.
- **The two hashes are separate on purpose.** `schema_hash` decides whether two nodes
  can compute the same digest for the same logical row; `visibility_hash` decides
  whether they project that row the same way. Mixing them would force a client — whose
  registration is necessarily narrower — to fail the digest check.
- **A profile may narrow a `Public` column.** Narrowing is not restricted to
  `Restricted` columns; a profile that hides a public column from one role is honoured.
- **The digest's input is always the full replicated column set.** A profile changes
  what a peer *sees*, never what a row *is*. See
  [ADR-0016](../doc/adr/0016-visibility-profiles-mapped-to-peer-roles.md).

### Exit criteria

- **The schema hash covers exactly the digest's inputs**: two nodes differing only in
  registered column order are rejected at handshake, not discovered as divergence.
- **The visibility hash covers exactly the profile definitions**, and is separate from
  the schema hash: a client with a narrower registration still matches a master's
  projection.
- **A peer with a narrower profile agrees with a peer with a wider one** on the digest
  of the same row, and its tree matches: the digest is received verbatim, and the tree
  is a function of the digest rather than of the payload.
- The projection resolver is a pure function of the profiles and the registered schema,
  and is exhaustively testable with no fixture.

---

## M4 — The merge rules

### Deliverables

- `merge.rs`: `MergePolicy`, `MergePolicyFn`, `MergeContext`, `MergeDecision` — the
  extension points and the rules they must obey.
- The write-authority rule.

### Notes

- **The default rule is LWW on `(last_update, digest)`.** It is a total order, and a
  total order is what makes two peers converge on the same row.
- **A custom policy must be pure and convergent.** The same `(incoming, local)` pair
  must produce the same decision on every peer. A policy that consults the wall clock
  or a node-local counter is a divergence generator.
- **`Undecided` falls through to the default rule.** The fallback covers a policy that
  *cannot decide*; it does not cover one that decides wrongly. A non-convergent policy
  returns a definite decision, so the tie-break never fires and the crate cannot detect
  it. This is the one extension point that can break the core guarantee, and it is
  documented as a hard rule rather than as advice.
- **Batch repair runs on the sender.** A repair applied on the receiving side changes
  the stored bytes, and therefore the digest, so the two peers would compute different
  digests for the same logical row.
- **Write authority is evaluated against the row's origin** — the node that wrote it —
  not against the node that forwarded it.

### Exit criteria

- The default rule is idempotent: applying the same row twice is a no-op.
- A policy returning `Undecided` falls through to LWW on `(last_update, digest)`.
- The digest tie-break resolves an exact `last_update` collision deterministically, and
  the comparison is on the digest as an unsigned 128-bit integer — comparing the raw
  bytes in memory order would let two peers disagree on a tie.
- **The crate can be used to implement a custom policy without depending on SQLite** —
  asserted by a compile check on the `model` crate's own dependency tree.

**Test suites:** the merge-policy unit tests. The convergence suite with a custom policy
in the loop is [core's](core.md) responsibility.

---

## Crate-level exit criteria

- The crate builds with **no dependency other than `serde`** (and `serde` itself
  optionally). No `rusqlite`, no `tokio`.
- `cargo clippy` is clean.
- Every golden vector fixture for the digest, the schema hash and the visibility hash is
  published under this crate's `fixtures/` directory, in a form another language can
  consume.
- The public API is documented, because it is now published surface: a change to
  `SyncConfig`, `MergePolicyFn` or the encoding is a breaking change.

**Test suites:** 14, plus the `schema_hash` and `visibility_hash` checks.
