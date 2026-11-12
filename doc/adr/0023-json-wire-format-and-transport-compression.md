# ADR-0023: JSON is the wire format; compression is a transport concern

- **Status:** accepted
- **Date:** 2026-11
- **Deciders:** project maintainers

- **Supersedes:** the wire-format row in [../architecture.md](../architecture.md)
  § "What the crate provides", which read "JSON, or binary behind a feature", and the
  postcard note in [../protocol.md](../protocol.md) § "The wire protocol"

## Context

The wire format was an open question (Q5 in the plan): JSON, or a binary format from the
start? The answer matters because the protocol is versioned and lives as long as the
deployment does.

Three things were true at the same time, and together they settle the question.

**First, the wire format is already decoupled from everything that must not change.**
The digest is computed over the **canonical encoding** (`value_enc`), not over the wire
bytes, and it is versioned by `codec_version`. The `schema_hash` covers the key strategy,
the key columns, the replicated column names and types in registration order, the managed
column names and `codec_version` — and nothing about the wire
([../data-model.md](../data-model.md) § "The two hashes"). So a change to the wire format
re-digests **no rows** and invalidates **no stored data**. The stakes are much lower
than they look, and the decision is reversible.

**Second, the bulk of the traffic is not payload.** The default ROWS mode is `digest`
([../protocol.md](../protocol.md) § "ROWS"): each side sends a compact list of
`(key, digest)` pairs, and only the rows that actually differ have their contents
exchanged. Digests are 16 bytes and keys are canonical key bytes, so this is the traffic
that a binary format would help most — and it is also the traffic that compresses best.

**Third, the crate does not own the transport.** ADR-0011 is explicit: the crate owns
the exchange, the application owns the transport. Compression is a property of the bytes
on the wire, so it belongs to whoever owns the wire.

The compression question had never been asked, because the wire format question was
standing in for it. They are separate questions at separate layers, and this record
answers both.

## Decision

**The wire format is JSON. The wire version is `merkle-lww/1`.**

**The row shape is positional.** A row's values are an array, in the order given by the
**projection** for that table — not an object keyed by column name. The columns are
declared once, in the handshake, with their names and types.

```
"tables": [ { "name": "device",
              "kind": "keyed",
              "key": { "strategy": "global_id", "key_columns": ["id"] },
              "columns": [ { "name": "payload", "type": "text",
                             "replicated": true, "visibility": "public" },
                           { "name": "secret",  "type": "blob",
                             "replicated": true, "visibility": "restricted" } ],
              "write_authority": "any" } ]
```

A row carries its canonical key and its digest, and — in `row` mode — its values:

```
mode = "digest":  "rows": [ [ <key>, <digest> ], … ]
mode = "row":     "rows": [ [ <key>, <digest>, [ <value>, … ] ], … ]
```

The values are in the order of the **projection** — the ordered list of columns the
session actually carries — and the projection carries each column's type, so it is
self-sufficient for decoding a row.

The digest is present in **both** modes. It is required in `row` mode because a peer
whose projection is narrower than the table's replicated column set cannot recompute it
([ADR-0016](0016-visibility-profiles-mapped-to-peer-roles.md)), and because a peer that
*can* recompute it should, as a check.

**Values are tagged, because SQLite is dynamically typed.** The digest distinguishes
`INTEGER` from `REAL` and `TEXT` from `BLOB`, so the wire must too:

| Type | On the wire |
|---|---|
| NULL | `null` |
| INTEGER | a JSON number in integral syntax — no fraction, no exponent |
| REAL | `{"$f": <JSON number>}` |
| TEXT | a JSON string |
| BLOB | `{"$b": "<base64>"}` — RFC 4648, standard alphabet, padded |

A plain JSON number with a fraction or an exponent is an **error**: a producer that
means `REAL` must write `{"$f": …}`. The asymmetry is deliberate — integers are the
common case and stay plain, while the two cases JSON cannot express unambiguously are
wrapped. The wrapper costs a handful of bytes on values that are rare and, in the BLOB
case, large.

**Compression is not part of the protocol.** A transport **may** apply a content
encoding. If it does, both peers must agree, and the encoding is negotiated at the
transport layer. `merkle-lww-axum` enables **zstd level 1** by default, and the
application may turn it off.

**`max_message_bytes` is measured on the JSON message**, before any content encoding.
The budget bounds the crate's own page-assembly work and its staged-page writes, so it
is measured on the message the crate produces. Compressed bytes are always fewer, so
the wire is never the binding constraint.

**The WebSocket path is the exception.** The deployed WebSocket compression extension is
`permessage-deflate`, which is deflate, not zstd. The zstd recommendation is therefore
a REST-path recommendation; the WebSocket path uses whatever the transport negotiates.

**A transport must not mix attacker-controlled values and secret values in one
compressed stream.** Compression side channels — the BREACH shape — need both in the
same stream and a measurable compressed length. This protocol's threat model does not
obviously expose it: a client session's projection excludes restricted columns, and a
trusted peer already holds them. The rule is stated because the transport is the
application's, and the application may be carrying more than this protocol.

## Consequences

**Positive**

- **The wire format can change without touching stored data.** A future binary format is
  a wire-version bump, not a re-digest and not a migration.
- **The protocol is diagnosable.** A session can be replayed with `jq`, read in a log,
  or captured by a packet analyser. That is worth a great deal in a system with this
  many failure modes — the purge quiesce, the staleness rule, merge decisions,
  visibility profiles.
- **No new dependency in the core.** The core already depends on `serde`. Compression is
  a dependency of the transport crate, which is optional by construction.
- **Rows are positional, so the row payload is compact.** Repeating column names on
  every row is the single largest source of JSON overhead, and the columns are already
  declared once in the handshake.
- **The digest is the anchor of the row.** A peer that cannot recompute the digest
  still has one, and a peer that can recomputes it and checks.

**Negative**

- **JSON is larger on the wire than a binary format.** Measured on the shapes this
  protocol actually carries: a digest-mode page of 2 000 `(key, digest)` pairs is 128 kB
  as JSON against 84 kB binary — 52 % larger. A row-mode page of 2 000 typical rows is
  163 kB as JSON against 143 kB binary — 14 % larger.
- **Under zstd level 1 the gap is 1–4 %.** The same digest-mode page compresses to
  67.6 kB as JSON against 64.9 kB binary; the same row-mode page to 44.6 kB against
  42.8 kB. Compression is what makes the JSON choice nearly free, which is why the
  recommendation is to compress rather than to avoid JSON.
- **The tagged value form is more verbose than a bare JSON value.** A `REAL` costs
  `{"$f":…}` and a `BLOB` costs `{"$b":…}`. In exchange, no receiver has to guess a
  storage class.
- **A transport that compresses must decompress symmetrically.** A peer that
  compresses and a peer that does not produce a parse failure that looks like
  corruption. The rule has to be written down, and it is.

**Neutral**

- The wire version and the codec version stay separate. `merkle-lww/1` versions the
  protocol; `codec_version` versions the canonical encoding. Changing the wire format
  does not change `codec_version`, and changing the encoding does not change the wire
  version. Both are carried in the handshake, and both are compared at handshake.
- `schema_hash` is unchanged. It never covered the wire format.

## Alternatives considered

**A binary format from the start.** Rejected. It buys 1–4 % on the wire once the
transport compresses, and it costs the diagnostic surface that this protocol needs
most, plus a field-tagging discipline that is easy to get subtly wrong. A hand-rolled
binary format without explicit field numbers is a permanent compatibility trap; one
with them is a schema, a generator, and a version negotiation — all of which JSON plus
a version field already gives.

**CBOR.** Rejected for now, and the most likely choice if the wire format is ever
revised. It is binary, self-describing, extensible, and it has a native byte string, so
the BLOB wrapper would not be needed. It was not chosen because it buys little while
the transport is not compressing, and because it costs the diagnostic surface. It is
recorded here as the successor, not as an option.

**Binary behind a cargo feature.** Rejected. A feature that changes the bytes on the
wire turns every deployment into a wire-format variant, and the interoperability matrix
becomes a test matrix. A wire version is the right mechanism: it is negotiated at
handshake, and a mismatch is a fatal, explicable error.

**Let the crate do the compression.** Rejected. The core is synchronous and has no
runtime dependency ([../architecture.md](../architecture.md) § "Threading model"), and
compression is a property of the bytes on the wire. Putting it in the core would make
the core care about the transport, and would make `max_message_bytes` ambiguous — it
would have to mean one thing before compression and another after.

**Make compression mandatory.** Rejected. A transport that cannot compress — a
constrained device, a proxy that rewrites bodies, a message queue — must still be able
to speak the protocol.

**Keep the row as an object keyed by column name.** Rejected: it repeats the column
names on every row, and the columns are already declared once in the handshake.

**Keep the digest out of `row` mode.** Rejected: a peer with a narrower projection
cannot recompute it, so it would have no digest at all.

## Related

- [ADR-0016](0016-visibility-profiles-mapped-to-peer-roles.md) — the projection, and why
  the digest is received verbatim by a narrow peer
- [ADR-0011](0011-crate-provides-the-exchange.md) — the crate owns the exchange, the
  application owns the transport
- [../protocol.md](../protocol.md) § "Message encoding" — the normative description
- [../architecture.md](../architecture.md) § "The crate graph" — which crate owns what
