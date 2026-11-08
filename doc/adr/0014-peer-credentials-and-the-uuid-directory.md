# ADR-0014: Role-level credentials, a master-issued UUID directory, and a separate digest seed

- **Status:** accepted
- **Date:** 2026-11
- **Deciders:** project maintainers

## Context

The deployment has three roles — masters, children and read-only clients — and the
maintainer described the intended shape:

> Each node has some uuid used in communications, masters can make them up and assign the
> uuids. There should also be a 'secret' which users will provide the master(s) so they
> can remain sync'd to one another as a sort of extra requirement to ensure the masters
> agree. One string unknown to us, but assigned to a config value which we'd have at
> runtime. Child nodes would have a similar but different string. All children would have
> the same child string but each would only know their own uuid. All masters would know the
> master string and the child string and all uuids.

And the data placement:

> All data is expected to live on all masters. All data not specific to child sync security
> is also expected to live on all children. All data not tagged as non-client is expected to
> replicate to DB-bearing clients.

Two requirements accompany this:

- **Key rotation must be easy.** "Allowing the users to readily, by config file change,
  invalidate a node by performing key rotation."
- **Secrets belong in the application layer.** "I think it doesn't belong in the DB, it
  belongs in the application layer."

[ADR-0008](0008-two-peer-identity-layers.md) established two layers — a `node_id` identity
and a credential — but left the credential's granularity open, and did not account for the
digest seed.

## Decision

### 1. Identity is a master-issued UUID

Every node has a UUID, and it is **issued by a master**. A master mints a UUID when it
provisions a child; masters mint their own at first start.

The node persists its own UUID and presents it at handshake. It is the address used in
communications, and the key under which the node's cursor, progress and membership are
recorded.

This differs from the previous design, in which a node generated and persisted its own
UUID. Issuing it makes the UUID meaningful within the deployment — it belongs to a master
that can vouch for it and revoke it.

### 2. The UUID directory is master-only control state

Every master knows **every node's UUID**: its own, its children's, and those of other
masters' children.

The directory is **propagated master to master** at handshake, not configured by hand. Each
master sends the directory it holds; each takes the union. A master that provisions a child
therefore makes that child known to the whole mesh, with no operator editing five config
files.

**Children are not given the directory.** A child knows its own UUID and its upstream
master's, and nothing else. A client knows no UUIDs at all.

The directory is **not replicated data**. It is not in the Merkle tree, not in
`_sync_log`, and not in any application table. It lives in node-local state, because it is
control plane, not data plane.

### 3. Credentials are role-level, not per-node

Two strings:

| String | Held by | Authenticates |
|---|---|---|
| **master string** | every master | master ↔ master |
| **child string** | every child, and every master | child ↔ master |

A session is authenticated by the credential of the **role** the connecting node claims.
The claimed UUID must additionally be in the directory.

The consequence is that **the UUID is the only thing distinguishing one child from
another** — they all present the same child string. This is why the directory is
withheld from children: a child that learned another child's UUID could present
`(that UUID, the child string)` and be accepted as that child. The directory *is* the
child security boundary.

### 4. The digest seed is a third value, and not a credential

The digest is keyed, so that a client cannot confirm a guess at a `Restricted` value by
submitting a candidate and observing the bucket hash change. The key for that hash is the
**digest seed**.

It is **not** a credential: it grants no access, and it authenticates nothing. It
answers a different question — *"what key do I hash with?"* — and the two credential
strings answer *"should I talk to this node?"*.

It is nevertheless secret material, and it must reach **every node that computes a
digest**. That includes children: a child writes rows, and every write produces a digest
in `_sync_log` and a leaf in its Merkle tree. So the digest seed cannot be the master
string — children deliberately do not have it, or a child could impersonate a master.

Three provisioning options were considered:

| | Values to distribute | Cost |
|---|---|---|
| **(A)** digest seed = the child string | two | rotating the child string **re-digests the entire deployment** — every stored digest and every tree leaf changes, so it is a full rebuild |
| **(B)** digest seed = its own value | three | one more value to distribute, but it rotates independently |
| **(C)** digest seed derived from the master string, children given the derived blob | three | the same coupling as (A) — rotating the master string rebuilds everything |

**Option (B).** The maintainer's stated goal is that key rotation be cheap and routine.
Under (A) and (C), rotating a credential costs a full dataset rebuild; under (B), it
does not. Rotating the *digest seed* is always a full rebuild — that is inherent to a
keyed hash, not a design choice — which is precisely why it should be its own value.

| Node | master string | child string | digest seed |
|---|---|---|---|
| master | ✓ | ✓ | ✓ |
| child | ✗ | ✓ | ✓ |
| client | ✗ | ✗ | ✗ |

### 5. The crate persists no secret material

The digest seed, the role strings and the peer list are supplied by the application as
configuration. The crate holds them in memory for the life of the process and **does not
write them to the database**.

This reverses an earlier decision to store `digest_seed` in `_sync_meta`. The database is
the thing being replicated, inspected, backed up and shipped around; it is the wrong place
for the material that authenticates a peer.

The consequence is that the crate has **no rotation logic at all**. Rotation is an
application concern, and an application that wants managed rotation can put a crate in
front of this one.

### 6. Revocation comes in two granularities

| Action | Effect |
|---|---|
| **Remove a UUID from the directory** | invalidates exactly one node, immediately, with no secret rotation |
| **Rotate the master string** | invalidates every master at once |
| **Rotate the child string** | invalidates every child at once |
| **Rotate the digest seed** | invalidates every digest — a full rebuild of every tree |

The first is what makes "invalidate a node by a config file change" cheap in the common
case. The role strings cover the cases where a whole class must go at once.

### 7. Mutual authentication with one secret

A child needs to know it is talking to a real master, without holding the master string.
It does not need to: a master proves itself by HMAC-ing the child's challenge with the
**child string**, which the child also holds.

```
child  → master:  HELLO { uuid, nonce }
master → child:   PROOF = HMAC(child_string, nonce ‖ child_uuid)
child:            verify PROOF against its own child_string
```

Both sides prove group membership from a single shared secret, and the child never learns
the master string.

### 8. Attribution is not available

Role-level credentials mean every master is cryptographically identical to every other
master. A master can present another master's UUID and be accepted.

The deployment therefore gets **authorization and revocation**, but not **attribution**:
nothing in the protocol can prove *which* master wrote a given row. If audit attribution
is ever required, it needs per-node credentials or per-node signing keys — a change to
§ 3, not to the rest of the design.

## Consequences

**Positive**

- **Per-node revocation is cheap.** Removing a UUID from the directory invalidates one
  node, with no secret rotation and no fleet-wide re-provisioning.
- **Credential rotation does not rebuild anything.** Under (B), rotating the master or
  child string leaves every digest and every tree intact.
- **The directory stays correct without operator effort.** Masters merge it at handshake,
  so provisioning a child makes it known to the whole mesh immediately.
- **Children are isolated from each other.** They cannot address one another, because they
  do not know each other's UUIDs.
- **Mutual authentication works with one secret per role**, and children never hold the
  master string.
- **No secret material is written to the database.**

**Negative**

- **Masters are mutually impersonable.** No attribution.
- **The directory must be kept confidential from children**, and it is the only thing
  distinguishing them. A leak of the directory to a child compromises that child's
  identity.
- **Every node needs three-ish values** — masters three, children two — and getting the
  right one to the right node is an operational responsibility.
- **Losing the digest seed means a full rebuild** of that node's tree and re-synchronisation
  of its dataset. It should be backed up like any other critical secret.
- **The directory is a merge, not a consensus.** Two masters that provision conflicting
  UUIDs produce a union, and the directory holds both. A UUID is unique by construction —
  masters mint them — so this is a misconfiguration rather than a race.

**Neutral**

- The directory grows monotonically as nodes are provisioned. It is not garbage-collected
  by the crate; the operator removes a node's UUID when decommissioning it.
- Children hold the full dataset, so a child is a near-complete replica. The hierarchy
  controls **connectivity and write authority, not storage**.

## Alternatives considered

**Per-node credentials.** Each node has its own secret, so revocation and attribution are
per-node. Rejected: it requires an N×M secret distribution — every master holding every
other master's secret — and a rotation touches every peer's configuration. The
UUID-list revocation achieves the common case without that cost, at the price of
attribution.

**Deriving the digest seed from a role string (options A and C).** Fewer values to
distribute. Rejected: it couples credential rotation to a full dataset rebuild, which
contradicts the stated goal that rotation be routine.

**The digest seed *is* the master string.** Fewer values still. Rejected: children must
compute digests, and handing them the master string would let a child impersonate a
master.

**Self-assigned UUIDs.** Each node generates its own. Rejected: the issuer is what makes
a UUID revocable and meaningful. A node could otherwise claim any identity it liked.

**Storing secrets in the database.** Convenient — the crate would find them on start-up.
Rejected: the database is replicated, inspected and backed up; it is the wrong place for
the material that authenticates a peer, and the maintainer put rotation in the application
layer explicitly.

**Putting the directory in the replicated data.** Every node would then know every UUID.
Rejected: it would put the child identity boundary inside the dataset, which is replicated
to children — the one place it must not reach.

## Related

- [ADR-0008](0008-two-peer-identity-layers.md) — the identity/credential split
- [../security.md](../security.md) — the trust model
- [../config.md](../config.md) — where the values live
- [../architecture.md](../architecture.md) — roles, topology, data placement
- [../protocol.md](../protocol.md) — the handshake
