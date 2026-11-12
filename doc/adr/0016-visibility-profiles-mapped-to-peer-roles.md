# ADR-0016: Visibility profiles mapped to peer roles

- **Status:** accepted
- **Date:** 2026-11
- **Deciders:** project maintainers

## Context

The visibility model has two classes: **trusted** (masters and children), which sees
everything, and **client** (read-only), which sees the public columns and the tables
marked `client_visible`. A column is either `Public` or `Restricted`, and a table is
either visible to clients or not.

That is enough for "a secret that clients must not see". It is not enough for a
deployment that says:

- masters exchange a credential column with each other, but children never see it;
- a large payload column is withheld from read-only clients;
- one table is excluded for children and another for clients;
- one particular client — a TUI, say — gets a narrower view than the other clients.

Each of those is a rule of the form *"this class of peer may see these tables and these
columns"*, and the current model can express none of them without bolting a special
case onto every one of them.

The requirements that make this hard:

- The projection must be **declarative**, because it has to be compared between peers
  and stored in a hash.
- It must **not** affect the digest's input. The digest is always computed over the
  table's **full replicated column set**, never over the columns a particular peer may
  see, so two peers that see different columns still agree on the digest for the same
  logical row, and a child and a master holding the same row converge.
- A peer that **cannot** see every replicated column of a table therefore **cannot**
  compute that table's digest. It receives the digest verbatim and never recomputes it,
  the same treatment a client gets for not holding the seed. This does not affect the
  tree: `row_hash` is a function of the digest, not of the payload.
- A read-only client necessarily has a **narrower registration** than a master. If the
  registration is what is hashed, a client can never match a master's hash, and the
  existing "client schema mismatch is a warning, trusted peer mismatch is fatal" rule
  becomes incoherent.

## Decision

### 1. A visibility profile is a named projection

```rust
pub struct VisibilityProfile {
    pub name: String,
    /// Trusted profiles may see `Restricted` columns.
    pub trusted: bool,
    pub tables: Vec<ProfileTable>,
}

pub struct ProfileTable {
    pub table: String,
    pub visible: bool,
    pub columns: Vec<String>,
}
```

A profile names the tables it includes and, for each, the columns it may see.

### 2. Profiles are mapped to roles, and a peer may override the mapping

```rust
SyncConfig {
    profiles: Vec<VisibilityProfile>,
    role_profile: HashMap<Role, String>,
    peers: Vec<Peer { …, profile: Option<String> }>,
}
```

The role mapping is the default; `Peer.profile` overrides it for one peer.

### 3. Two hashes, with different jobs

| Hash | Covers | Compared between |
|---|---|---|
| `schema_hash` | exactly the inputs to the row digest | trusted peers — fatal |
| `visibility_hash` | the profile definitions | trusted peers — fatal |

They are separate because they answer different questions. `schema_hash` says "can we
compute the same digest for the same row?". `visibility_hash` says "do we project that
row the same way?". Folding them together would force every client to fail the digest
check, because a client's registration is necessarily narrower.

### 4. A client receives the resolved projection

A client names its profile in the handshake. The server resolves it to a projection —
the table → column list — and sends it. The client applies that projection and syncs
the intersection of it with its own registration. A client does **not** compare either
hash: it cannot compute `schema_hash`, because it does not hold the restricted columns,
and it does not need to.

A client that knows the profile definition may compare the projection against its own
expectation and log a warning on a difference. It never fails the session.

### 5. The column classification is the default projection

`Visibility::Public` means "visible to every profile". `Visibility::Restricted` means
"visible only to `trusted` profiles". A profile may narrow that — it may hide a public
column from one role — but it may never expose a restricted column to an untrusted
profile.

### 6. `client_visible` is retired

The per-table `visible` flag in the profile replaces it. There is one mechanism, so the
two cannot disagree.

### 7. Write authority follows visibility

A peer may write a table only if its profile exposes every replicated column of it: a
peer that cannot see a column cannot compute the row's digest.

## Consequences

**Positive**

- **The awkward cases become declarations.** "Masters see this column, children do
  not" is a profile, not a code path.
- **The digest is untouched.** The profile never enters the digest input, so a child
  and a master holding the same row still agree on its hash.
- **One mechanism.** The per-table flag and the per-column classification cannot drift
  apart, because they are the same thing.
- **A divergence is detectable.** Two trusted peers with different profiles are
  refused at handshake, before any data moves.

**Negative**

- **A profile is deployment-wide state.** It must be identical across the deployment,
  which is one more thing to provision — and the crate checks it, so a mistake is loud
  rather than silent.
- **A client cannot verify what it is missing.** It receives the projection and
  applies it; it cannot compute the hash of a schema it was never given. That is
  inherent to being a client.
- **Writing code must check two things.** Visibility *and* write authority, because
  "may see" and "may write" are separate permissions.

**Neutral**

- The `Visibility` enum stays, as the default projection rather than the mechanism.
- The client intersection rule is unchanged: a client that names something the server
  does not have gets the intersection and a warning.

## Alternatives considered

**Keep two classes and add a third.** Rejected: the third class would need its own
flags, and the fourth would need another. The cases are per-peer-role projections, not
a small set of classes.

**Encode the projection in `schema_hash`.** Rejected: a client's registration is
narrower by construction, so it could never compute the hash. The two hashes have to be
separate.

**Let each peer state its own projection, and trust it.** Rejected: the projection is
what decides what a peer may see. A peer that states its own would be stating its own
permissions.

**Make the profile part of the row.** Rejected: a per-row visibility flag would have to
enter the digest, and two peers holding the same row under different profiles would
then compute different digests.

**Keep `client_visible` alongside the profiles.** Rejected: two mechanisms for one
decision, and no way to tell which one won.

## Related

- [../data-model.md](../data-model.md) — visibility profiles and the two hashes
- [../security.md](../security.md) — the enforcement points
- [../protocol.md](../protocol.md) — the projection in the handshake
- [ADR-0015](0015-per-table-merge-policies-and-write-authority.md) — write authority
