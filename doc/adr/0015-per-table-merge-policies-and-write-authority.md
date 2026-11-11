# ADR-0015: Per-table merge policies and write authority

- **Status:** accepted
- **Date:** 2026-11
- **Deciders:** project maintainers

## Context

The merge rule in this design is last-write-wins on `(last_update, digest)`, applied to
the whole row. It is a good rule, and it is the right default. But a real schema
usually contains a few tables where it is the wrong rule, and a deployment where "every
peer may write every table" is the wrong policy.

The cases that came up:

- **A lease table.** Two sites allocate the same lease. The *first* allocation should
  win, not the last write — and LWW gives the last write.
- **An epoch-ordered ownership field.** A row carries an ownership epoch. LWW picks the
  winner of the row, and then the ownership field has to be re-asserted from whichever
  side had the higher epoch, because LWW may have picked the older epoch's row.
- **Batch repair before apply.** A partial unique index means an incoming batch may
  need a column rewritten before it can be inserted. The repair has to happen before
  the upsert.
- **Master-authoritative policy tables.** Some tables are the master's to write. A child
  that pushes a row for one must be refused.

The design already had two extension points, and neither was enough:

- `MergeMode` on a **column** decides how two values of one column combine. It cannot
  express "the first allocation wins", because that is a rule about *rows*.
- The **role hierarchy** decides who may talk to whom. It says nothing about who may
  write a given table.

A third problem sits underneath both. Write authority has to be evaluated against the
node that **wrote** a row, not the node that forwarded it: a master's rows travel
master → master → child, and the child's authority must be judged against the master
that wrote them. Today the wire carries no origin, so the receiving node cannot make
that judgement at all.

## Decision

### 1. A table may declare a merge policy

```rust
pub enum MergePolicy {
    LastWriteWins,
    Custom(Arc<dyn MergePolicyFn>),
}

pub trait MergePolicyFn: Send + Sync {
    fn merge(&self, ctx: &MergeContext<'_>, incoming: &IncomingRow,
             local: Option<&StoredRow>) -> MergeDecision;

    /// Optional: repair a batch before it is replicated. Runs on the **sender**.
    fn prepare(&self, ctx: &MergeContext<'_>, batch: &mut Vec<Row>) -> Result<()>;
}
```

`LastWriteWins` remains the default and covers most tables.

### 2. Two requirements on a custom policy

**The policy must be pure and convergent.** The same `(incoming, local)` pair must
produce the same decision on every peer. A policy may not consult the wall clock, a
node-local counter, or anything else that differs between peers.

The reason is not fastidiousness. LWW's convergence comes from being a **total
order**: two peers holding two versions of a row compute the same answer because the
order is the same everywhere. A custom policy replaces the order, so it has to supply
an equivalent property or it has replaced convergence with hope. The crate cannot check
this — a policy that consults the clock looks exactly like one that does not — so it is
a stated requirement on the application.

The digest tie-break survives as the fallback: a policy that returns "undecided" falls
through to LWW on `(last_update, digest)`.

**Batch repair runs on the sender.** `prepare` is called before the rows are
replicated. A repair applied on receipt changes the stored bytes, and therefore the
digest — so the two peers would compute different digests for the same logical row, and
the trees would never converge. A receiver-side repair is safe only if it is a
deterministic function of the replicated row set that every peer computes identically,
which in practice means doing it before the rows are sent.

### 3. A table may declare a write authority

```rust
pub enum WriteAuthority {
    Any,
    MasterOnly,
    OwnerOnly,
    Roles(Vec<Role>),
}
```

The check is made when the row is **applied**, against the **origin** of the row — the
node that wrote it — and not against the node that forwarded it. A row whose origin
lacks authority for the table is refused with `WRITE_NOT_PERMITTED`, and the table is
dropped from the session.

### 4. The wire carries the origin

Every ROWS batch carries the origin of the rows it contains:

```jsonc
"origin": { "node_id": "…", "role": "child", "site": "eu-west" }
```

This is a protocol change, and it is unavoidable: without the origin, the receiving
node cannot evaluate authority for a row that arrived through a third node.

### 5. Write authority and visibility are related

A peer may write a table only if its profile exposes **every** replicated column of
that table. A peer that cannot see a column cannot compute the row's digest, and a row
it cannot digest is a row it must not write. The crate checks both.

## Consequences

**Positive**

- **The cases that motivated it are expressible.** First-allocation-wins, epoch
  re-assertion, batch repair and master-only tables are all ordinary code in the
  application, not new machinery in the crate.
- **The default is untouched.** A deployment with no custom policy behaves exactly as
  before.
- **Authority follows the writer.** A child's write refused by a master is refused on
  every master, because every master evaluates the same origin against the same rule.

**Negative**

- **The crate cannot police convergence.** A non-convergent policy produces silent,
  permanent divergence, and the crate has no way to detect it. The requirement is
  documented; it is not enforced.
- **The origin is new wire state.** It must be carried, stored, and preserved through
  forwarding. A node that forwards rows it did not write must not overwrite the origin
  with its own.
- **A custom policy is a public API.** Changing its shape later breaks applications
  that implemented it.

**Neutral**

- The digest tie-break is unchanged, and remains the fallback for "undecided".
- The digest input does not change. A merge policy decides *which* row wins; it does
  not change *what* the digest covers.

## Alternatives considered

**Make the merge rule configurable per column only.** Rejected: the cases that
motivated this are rules about rows, not about columns. A first-allocation rule cannot
be expressed as a column merge mode.

**Let the application implement the whole merge.** Rejected: the crate's merge is
where the convergence argument lives. Handing it to the application hands over the
argument too.

**Let a peer write every table it can see.** Rejected: "can see" and "may write" are
different permissions. A client sees a projection and writes nothing; a child may see
a table and still not be allowed to write it.

**Evaluate authority against the immediate sender.** Rejected: it would make the rule
depend on the topology rather than on the data. A master's row would be acceptable
when it arrived directly and unacceptable when it arrived via another master — the same
row, two verdicts.

**Allow receiver-side batch repair.** Rejected: it changes the stored bytes and
therefore the digest, which is divergence by construction.

## Related

- [../data-model.md](../data-model.md) — merge policies, write authority
- [../protocol.md](../protocol.md) — the origin in the ROWS batch
- [../bulk-apply.md](../bulk-apply.md) — why the upsert-level LWW filter is not universal
- [ADR-0001](0001-last-update-is-the-ordering-key.md) — the default ordering
