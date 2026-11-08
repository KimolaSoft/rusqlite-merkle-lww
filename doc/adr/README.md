# Architecture decision records

One file per significant decision, in the form:

```
Status · Date · Deciders
Context · Decision · Consequences · Alternatives considered · Related
```

These record the *why* behind the design. When asking why the system works the way it
does — why the tree uses absolute bucket boundaries rather than rolling ones, or why sync
invariants are enforced inside SQLite rather than by wrapping SQL — the answer is here.

| ADR | Decision | Status |
|---|---|---|
| [0001](0001-last-update-is-the-ordering-key.md) | `last_update` is the ordering key; no version key or HLC | accepted |
| [0002](0002-enforce-in-sqlite-not-in-the-api.md) | Enforce invariants in SQLite, not in a query wrapper | accepted |
| [0003](0003-absolute-bucket-boundaries.md) | Absolute epoch-anchored bucket boundaries, not rolling tiers | accepted |
| [0004](0004-digest-is-always-computed-and-keyed.md) | The digest is always computed, and keyed | accepted |
| [0005](0005-soft-deletes-with-a-shared-purge-schedule.md) | Soft deletes with a shared purge schedule | accepted |
| [0006](0006-site-namespaced-keys.md) | Site-namespaced keys, with `GlobalId` as the default for shared datasets | accepted |
| [0007](0007-config-is-a-library-type.md) | Config is a serde struct, not a file format | accepted |
| [0008](0008-two-peer-identity-layers.md) | Peer identity is a stable id plus a separate credential | accepted |
| [0009](0009-progress-has-no-eta.md) | Progress is X-of-Y; no ETA in the protocol | accepted |
| [0010](0010-no-orm-query-wrapper.md) | No ORM; the crate owns the connection, not the query language | accepted |
| [0011](0011-crate-provides-the-exchange.md) | The crate provides the exchange; the application provides the transport | accepted |
| [0012](0012-sync-key-is-the-applications-key.md) | The sync key is the application's key when it is globally unique and stable | accepted |
| [0013](0013-site-registry-and-range-allocation.md) | A site registry in the main database; a row names the site that owns its identifier | accepted |

## Status values

| Status | Meaning |
|---|---|
| **proposed** | Under discussion |
| **accepted** | In force |
| **superseded by ADR-NNNN** | Replaced; the replacement is named |
| **deprecated** | No longer relevant, but not replaced |

## Adding one

A new ADR is warranted when a decision is:

- **hard to reverse** — changing it means re-digesting, re-bucketing, or re-seeding;
- **not obvious** — a competent engineer would reasonably choose differently;
- **consequential** — it constrains other parts of the design.

Style is deliberately not a criterion. A decision that is merely a preference belongs in
the design documents, not here.

An ADR is immutable once accepted. To change a decision, write a new ADR that supersedes
it, and edit the old one's status line to point at the replacement. The reasoning at the
time of the original decision is preserved, because understanding why it was made is
what makes it possible to tell whether it still applies.
