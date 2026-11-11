# ADR-0019: The allocation registry is optional

- **Status:** accepted
- **Date:** 2026-11
- **Deciders:** project maintainers

- **Supersedes:** [ADR-0013](0013-site-registry-and-range-allocation.md)

## Context

[ADR-0013](0013-site-registry-and-range-allocation.md) decided that a site registry
lives in the main database, is replicated, and is exchanged before any data in every
session. It also decided that a table with allocated identifiers carries a `site`
column naming the site that owns the identifier, and that the range check is a
trigger.

Those decisions are sound **for a deployment that uses the crate's allocation
scheme**. The problem is that the scheme was treated as a precondition rather than as
an option.

An application that already has an identifier scheme — a central id service, a
per-team leased range of its own, a database sequence with a site prefix — cannot use
the crate's allocator, and should not have to. But as the design stood, such an
application would have had to:

- create a `sites` registry table it does not use, with ranges it does not own;
- add a `site` column to every allocated table, maintained by the crate, when the
  application already knows the owner;
- accept a REGISTRY phase in every session, exchanging a table nobody reads;
- take a dependency on the allocator's "never reused" guarantee, which is about the
  crate's ranges and not about theirs.

The registry was also in the **critical path of every session**, which made an
optional feature behave like a mandatory one. The REGISTRY phase ran unconditionally,
before any data, in every session, on every node.

## Decision

**The allocation scheme is optional. The registry, the `site` column, the range
trigger, the allocator and the REGISTRY phase are all used only when a table asks for
them.**

### 1. A table opts in

```rust
pub struct TableSpec {
    …
    /// Present when the identifiers are allocated from per-site ranges.
    /// Absent in a deployment that brings its own identifier scheme.
    pub allocation: Option<AllocationSpec>,
}
```

When no table declares an `AllocationSpec`:

- there is no `sites` registry table;
- there is no `site` column on any table;
- there is no range trigger, and no `local_site()` UDF;
- there is no REGISTRY phase in the protocol.

### 2. The allocator is a feature, not a dependency

The allocation machinery lives behind an optional `alloc` feature. The sync core does
not depend on it: the core's types, the store, the tree, the diff engine and the
protocol all work without it.

### 3. The handshake says whether the registry is in use

```jsonc
"registry": { "enabled": true }
```

The REGISTRY phase runs only when it is enabled, and the join bundle includes the
registry only when it is enabled.

### 4. Everything else in ADR-0013 stands

When the crate's scheme **is** in use, the registry still lives in the **main
database** — SQLite forbids a trigger from referencing an object in an attached
database, and the range check is a trigger. The range is still immutable. The `site`
column still names the id owner, not the writer. The registry is still replicated,
still exchanged before data, and still part of the join bundle.

## Consequences

**Positive**

- **An existing id scheme is a first-class option.** The application declares its
  tables, syncs them, and never hears about ranges.
- **The core is smaller.** The allocator, the registry triggers and the `site` column
  are not in the default build.
- **The REGISTRY phase stops being overhead.** A deployment without the registry does
  not exchange a table nobody reads, before every session.
- **Nothing changes for a deployment that uses the scheme.** The behaviour is the
  same, with one extra field in the handshake.

**Negative**

- **Two configurations to test.** The registry-enabled path and the
  registry-disabled path are both real, and both need coverage.
- **The handshake has a mode.** A node must handle a peer that has the registry
  enabled and one that does not — which in practice means they disagree about the
  deployment, and the crate treats that as a mismatch to report rather than to
  reconcile.

**Neutral**

- The registry's role in the protocol is unchanged where it exists.
- `AllocationSpec` was already an `Option` in the table spec; what changed is that
  "no allocation anywhere" is now a supported deployment rather than a degenerate one.

## Alternatives considered

**Keep the registry mandatory and let the application populate it.** Rejected: an
application with its own id scheme would have to invent ranges it does not use, and
maintain a table whose values are meaningless. The maintenance cost is real, and it is
the kind of cost that gets paid wrong.

**Make the registry optional per session rather than per deployment.** Rejected: the
registry is deployment-wide state. Half a deployment with a registry is a deployment
where some peers validate identifiers and others do not.

**Drop the registry entirely and require applications to validate identifiers
themselves.** Rejected: this is the same loss as the compatible dialect's gap — the
check would move out of SQLite and into application code, where it can be bypassed by
one raw `conn.execute()`. The value of the registry is that the check is a trigger.

**Keep the allocator in the core but make the registry table optional.** Rejected: the
allocator is the part that needs the registry. Splitting them would leave the
allocator with nothing to allocate from.

## Related

- [../data-model.md](../data-model.md) — the optional `AllocationSpec`
- [../protocol.md](../protocol.md) — the conditional REGISTRY phase
- [../implementation.md](../implementation.md) — bringing your own identifiers
- [ADR-0013](0013-site-registry-and-range-allocation.md) — the registry decision
  itself, which this ADR supersedes
