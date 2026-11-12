# ADR-0025: The model crate defines its own UUID type

- **Status:** accepted
- **Date:** 2026-11
- **Deciders:** project maintainers

- **Extends:** [ADR-0024](0024-the-workspace-is-split-into-six-crates.md)

## Context

[ADR-0024](0024-the-workspace-is-split-into-six-crates.md) split the workspace into six
crates, and fixed `merkle-lww-model` as a crate that "depends on `serde` and on nothing
else — no SQLite, no runtime". The reason was that the model crate holds the
**compatibility surface**: the canonical encoding, the digest, the schema hash and the
visibility hash. Those are the artifacts a second implementation — in another language —
is checked against, and a crate that drags in a runtime or a database is a crate whose
output a second implementation cannot easily reproduce.

The configuration types need a 128-bit identifier. `SyncConfig.node_id`, `Peer.node_id`,
`PeerAuth::PerNodeSecret.node_id` and `KeyStrategy::Derived.namespace` are all UUIDs, and
the first of those is on the wire.

The obvious implementation is the `uuid` crate. It is pure Rust, it has no runtime
dependency, and it is what any reader would expect.

## Decision

**`merkle-lww-model` defines its own `Uuid` type, and depends on `serde` alone.**

The type is a 128-bit value with these properties:

- **The byte layout is 16 bytes, big-endian as RFC 4122 lays them out.** It is what
  appears in `_sync_meta`, what the directory compares, and what goes on the wire.
- **The text form is the hyphenated lowercase form** — `6f1e0c2a-1b3d-4e5f-8a9b-0c1d2e3f4a5b`.
  The bare 32-digit hexadecimal form is accepted on input, because some tooling emits
  it, but the hyphenated form is what the crate writes.
- **`serde` serialises it as that text form**, not as an array of sixteen numbers.
- **The all-zero UUID is `nil`**, and `is_nil()` distinguishes "not provisioned" from
  "provisioned as some specific node" — which matters, because a `node_id` of nil in a
  handshake is a different failure from a `node_id` that is not in the directory.

### Why not the `uuid` crate

Three reasons, in order of weight.

1. **The dependency rule is the crate's whole point.** ADR-0024's "no runtime" is not
   about `tokio`; it is about the compatibility surface being reproducible by something
   that is not this workspace. Every dependency on that surface is a thing a second
   implementation must agree with — including its version, and including whatever it
   changes between versions.
2. **The layout is part of the contract, so it should be pinned by the contract.** The
   `uuid` crate's representation is an implementation detail of *that* crate. If the
   bytes on disk are the `uuid` crate's representation, then the `uuid` crate is part
   of the on-disk format, and a change in it is a migration. Sixteen bytes, defined
   here, is not.
3. **The type is small.** A UUID is a byte array, a hexadecimal form, and a parser.
   That is a page of code and one unit test, and the crate needs it to be exactly
   right rather than to be featureful.

### What it costs

The cost is real, and it is worth stating.

- **UUID version and variant bits are not interpreted.** The type stores sixteen bytes
  and does not claim that they are version 4 or version 7. When M2 derives a UUIDv5, it
  sets the version and variant bits itself; when it mints a UUIDv7, it sets the version
  bit and the variant bits, and fills the rest with entropy. A UUID that arrives from
  elsewhere with the wrong bits is stored as it arrived.
- **No comparison by time.** UUIDv7's sort-by-time property is a property of the
  *values*, not of the type. It falls out of the derivation, not out of `Uuid`.
- **Interop at the boundary.** The other crates — `core` in particular, which stores a
  `node_id` in SQLite — convert to and from whatever the `uuid` crate would have given
  them, at the boundary, in one place.

### The wire form

`protocol.md` describes the handshake's `node_id` as "128-bit hex", which is ambiguous
between the hyphenated form and the bare form. **This ADR picks the hyphenated form**,
because `Uuid` is the type named throughout the design documents and the hyphenated
form is what a UUID is everywhere else. The wire vectors published by
`merkle-lww-protocol` pin it.

## Consequences

- `merkle-lww-model` has exactly one dependency: `serde`. The crate-level exit
  criterion in [plan/model.md](../../plan/model.md) is checkable with `cargo tree`, and
  it is checked.
- The `Uuid` type is part of the public API of a published crate, so its behaviour is
  part of the compatibility surface. Changing the text form is a breaking change.
- A second implementation in another language reproduces the UUID bytes from this
  document, not from a third party's crate.
- If the `uuid` crate is ever adopted, it is adopted at the **crate boundaries** —
  `core` converts — and the model's type stays, because the compatibility surface
  should not move when a convenience dependency changes.
