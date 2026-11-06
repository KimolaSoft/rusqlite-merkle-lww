# Documentation index

Design documentation for `rusqlite-merkle-lww`. These documents specify the system
before implementation begins; each one is intended to be read on its own, with the
architecture decision records ([`adr/`](adr/)) carrying the rationale for the choices
made along the way.

## The documents

### [implementation.md](implementation.md)

**Start here if you are building an application on this crate.** What the crate
provides, what it expects from you, how to wire it in, and the integration mistakes that
cause divergence rather than errors.

### [architecture.md](architecture.md)

The shape of the system: roles (master / child / client), the peer topology, the crate
layout, the threading model, the division of transport responsibility, and how the
pieces fit together.

### [data-model.md](data-model.md)

What is stored. The managed columns every synced table carries, the four key strategies,
the visibility model for restricted columns, the internal `_sync_*` tables, and the
canonical encoding used for hashing.

### [bulk-apply.md](bulk-apply.md)

How many rows are applied at once. The three bulk paths, SQLite's 32,766 bound-parameter
limit and how the chunk size is derived, and how the last-write-wins filter is expressed
in the upsert itself.

### [sqlite-interface.md](sqlite-interface.md)

How the crate interacts with SQLite. The trigger set, the authorizer rules, the two
trigger dialects (enforced and compatible) and the trade-off between them, and DDL
management. Includes the verified behaviour of each SQLite mechanism used.

### [merkle.md](merkle.md)

The tree. Bucket definitions, the hash construction at each level, the comparison-depth
policy, the in-memory snapshot, and how the tree is rebuilt and maintained.

### [protocol.md](protocol.md)

The wire protocol. Handshake and validation, the session phases, message formats, the
progress model, and the rules for idempotency and overlap.

### [lifecycle.md](lifecycle.md)

What happens to deleted data. Soft deletes, the shared purge schedule and its quiesce
window, the resurrection invariant and the staleness rule that enforces it, and the
replication argument in full.

### [security.md](security.md)

The trust model. Peer identity and credentials, TLS termination, the restricted-column
model, and why the digest is keyed.

### [config.md](config.md)

Every configuration field, which values must agree across peers and which may differ,
and how the config is supplied by the consuming application.

### [testing.md](testing.md)

The test strategy, the suite that matters most (convergence), the enforcement suite, and
the acceptance criteria each implementation phase is measured against.

### [operations.md](operations.md)

Running the thing: deployment shapes, the CLI, metrics, and how to diagnose the failure
modes the design creates.

## Architecture decision records

[`adr/`](adr/) holds one file per significant decision, in the form:

```
Status, Context, Decision, Consequences, Alternatives considered
```

These are the records to read when asking *why* the system works the way it does — for
example why the tree uses absolute bucket boundaries rather than rolling ones, or why
sync invariants are enforced inside SQLite rather than by wrapping SQL.

| ADR | Decision |
|---|---|
| [0001](adr/0001-last-update-is-the-ordering-key.md) | `last_update` is the ordering key; no version key or HLC |
| [0002](adr/0002-enforce-in-sqlite-not-in-the-api.md) | Enforce invariants in SQLite, not in a query wrapper |
| [0003](adr/0003-absolute-bucket-boundaries.md) | Absolute epoch-anchored bucket boundaries, not rolling tiers |
| [0004](adr/0004-digest-is-always-computed-and-keyed.md) | The digest is always computed, and keyed |
| [0005](adr/0005-soft-deletes-with-a-shared-purge-schedule.md) | Soft deletes purged on a globally scheduled instant |
| [0006](adr/0006-site-namespaced-keys.md) | Site-namespaced keys, with `GlobalId` as the default for shared datasets |
| [0007](adr/0007-config-is-a-library-type.md) | Config is a serde struct, not a file format |
| [0008](adr/0008-two-peer-identity-layers.md) | Peer identity is a stable id plus a separate credential |
| [0009](adr/0009-progress-has-no-eta.md) | Progress is X-of-Y; no ETA in the protocol |
| [0010](adr/0010-no-orm-query-wrapper.md) | No ORM; the crate owns the connection, not the query language |

## Conventions used throughout

- **`MUST` / `MUST NOT`** — a requirement. Violating it is a bug, not a style choice.
- **`SHOULD` / `SHOULD NOT`** — a strong recommendation. Deviating needs a reason.
- **`MAY`** — optional.
- **"the peer"** — any node this node talks to, regardless of role.
- **"the node"** — this process, as a peer.
- **"`last_update`"** — the RFC 3339 text form, e.g. `2026-11-24T17:24:03.302238Z`.
- **"`hlc_us`"** — the same instant as an integer, microseconds since the Unix epoch.

## Terminology

| Term | Meaning |
|---|---|
| **bucket** | A time range in the Merkle tree. Three levels: hour, cycle (24h), month (30 cycles) |
| **depth** | The level at which two trees are first compared (§ merkle.md) |
| **digest** | xxh3-128 over a row's replicated content, keyed with the deployment seed |
| **row hash** | The value folded into a bucket hash; covers key, timestamp, status, digest |
| **managed table** | A table the crate syncs, and for which it owns columns, triggers and DDL |
| **managed column** | A column the crate owns: `sync_ns`, `last_update`, `status`, `purge_on` |
| **dialect** | Which trigger flavour is in use: *enforced* (Rust UDF) or *compatible* (pure SQL) |
| **quiesce window** | The interval around a purge instant during which sync is refused |
| **purge instant** | The globally scheduled moment at which tombstones are removed |
| **absence** | A key not present in `_sync_log` — either never existed, or purged |
| **staleness horizon** | How long a peer may be offline before it must re-seed rather than sync incrementally |
| **site** | A deployment location. Part of the key under the `SiteNamespaced` strategy; an ordinary column under `GlobalId` |
| **digest seed** | The deployment secret the digest is keyed with; shared by trusted peers, absent from clients |

## Status of this documentation

These documents describe an intended design. Where the design was validated against real
SQLite behaviour rather than assumed, the verification is recorded in
[sqlite-interface.md](sqlite-interface.md) § "Verified SQLite behaviour" and summarised in
the relevant ADRs. Everything else is reasoned but not yet exercised by code.

Open questions are tracked in [../PLAN.md](../PLAN.md) § "Open questions".
