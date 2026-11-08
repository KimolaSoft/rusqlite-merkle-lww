# Security

## The trust model

The system replicates data between machines that belong to the same deployment. The
threat model is:

- **Trusted**: masters and children. They hold the full dataset, including restricted
  columns. They authenticate to each other.
- **Untrusted**: read-only clients. They hold a subset — restricted columns are never
  transmitted to them — and they cannot write.

The design assumes the deployment's operators are not adversaries to each other. It does
not assume the network is friendly, and it does not assume a client is honest about what
it asks for.

## Peer identity

A peer needs a **stable identity** that means the same thing across restarts,
redeployments, and TLS termination. Three layers, each answering a different question.

| Layer | Question it answers | Value |
|---|---|---|
| **Identity** | *who is this?* | a UUID, issued by a master |
| **Directory** | *who exists, and may I address them?* | every node's UUID, masters only |
| **Credential** | *may I talk to them?* | a role-level secret string |
| **Digest seed** | *what key do I hash with?* | a third secret, not a credential |

### Layer 1 — the identity

```rust
pub struct Peer {
    pub node_id:   Uuid,             // stable; issued by a master
    pub name:      String,           // for logs and the UI
    pub role:      Role,
    pub endpoint:  String,           // https://…
    pub auth:      PeerAuth,
}
```

A node's `node_id` is **issued by a master** — minted when a master provisions a child,
and by a master for itself at first start. It is persisted in `_sync_meta` and presented
at handshake.

Issuing it is what makes the UUID meaningful within the deployment: a master vouches for
it and can revoke it.

### Layer 2 — the directory

Every master knows **every node's UUID** — its own, its children's, and other masters'
children.

The directory is **propagated master to master** at handshake: each master sends the
directory it holds, and each takes the union. Provisioning a child therefore makes it
known to the whole mesh, with no operator editing every config file.

**Children are not given the directory.** A child knows its own UUID and its upstream
master's. A client knows none.

The directory is control-plane state, stored in `_sync_directory`, and is **not
replicated** — not in `_sync_log`, not in the Merkle tree, not in any application table.

### Layer 3 — the credential

A `node_id` alone is a **claim**, not a credential. The directory is the trust boundary:
a node whose UUID is not in the directory will not be talked to, regardless of what it
asserts.

```rust
pub enum PeerAuth {
    /// A role-level shared secret, compared in constant time.
    RoleSecret { role: Role, secret: SecretString },

    /// Client certificate subject/SAN, checked against the presented certificate.
    ClientCert { subject: String },

    /// Trust the transport: TLS is terminated elsewhere and mTLS is already verified.
    TransportOnly,
}
```

Two strings exist:

| String | Held by | Authenticates |
|---|---|---|
| **master string** | every master | master ↔ master |
| **child string** | every child, and every master | child ↔ master |

`SecretString` is a wrapper that does not implement `Debug` as its contents, so secrets do
not leak into logs or error messages. That is the whole reason to have it.

Because the credentials are **role-level**, every master is cryptographically identical to
every other master. The deployment gets **authorization and revocation**, but not
**attribution** — nothing can prove which master wrote a given row. Per-node credentials
or signing keys would be needed for that.

### The directory is the child security boundary

All children share the child string, so **the UUID is the only thing distinguishing one
child from another**. A child that learned another child's UUID could present
`(that UUID, the child string)` and be accepted as that child.

This is exactly why the directory is withheld from children, and why it is control plane
rather than replicated data: putting it in the dataset would put it in the hands of every
child.

### The check

At handshake, the node verifies three things:

1. the presented `node_id` is in its directory — otherwise `UNKNOWN_PEER`;
2. the credential verifies for the role the peer claims — otherwise `BAD_CREDENTIAL`;
3. the deployment state matches — epoch, digest seed, schema hash, codec version.

The crate compares a token itself; certificate validation is the transport's job.

### Mutual authentication with one secret

A child must know it is talking to a real master, without holding the master string. It
does not need to: the master proves itself by HMAC-ing the child's challenge with the
**child string**, which the child also holds.

```
child  → master:  HELLO { uuid, nonce }
master → child:   PROOF = HMAC(child_string, nonce ‖ child_uuid)
child:            verify PROOF against its own child_string
```

### Secrets are not stored

The digest seed and the role strings are supplied by the application as configuration.
The crate holds them in memory for the life of the process and **writes none of them to
the database** — including the digest seed, which an earlier revision stored in
`_sync_meta`.

The database is replicated, inspected, backed up and shipped around. It is the wrong place
for the material that authenticates a peer. The consequence is that the crate has **no
rotation logic**; rotation is an application concern, and an application that wants
managed rotation can put a crate in front of this one.

See [adr/0014-peer-credentials-and-the-uuid-directory.md](adr/0014-peer-credentials-and-the-uuid-directory.md).

## TLS

**The crate does not terminate TLS.** This is deliberate, and it keeps a TLS stack out of
the dependency list entirely.

The application terminates TLS wherever it already does — an ingress controller, a service
mesh, a reverse proxy, or `axum-server` with rustls in the consuming application. The
crate then authenticates the peer with whatever credential the deployment already has.

This is part of the broader division: **the crate provides the data exchange; the
application provides the transport.** See
[implementation.md](implementation.md) and [architecture.md](architecture.md) §
"Transport".

If the application does terminate TLS itself, `PeerAuth::ClientCert` is the natural fit:
the client certificate's subject or SAN is checked against the configured peer.

What the crate must do either way:

- **Never log the credential.** `SecretString` handles the token case; certificate
  subjects are logged, but certificate *contents* are not.
- **Never expose it in an error message.** An authentication failure says "peer not
  authorised", not "expected token abc, got def".

### What TLS does not cover

TLS protects data in transit. It does not decide *who may see which column*. That is the
visibility model, below.

## Restricted columns

```rust
pub enum Visibility {
    /// Sent to any peer, including read-only clients.
    Public,
    /// Stored and synced between trusted peers only. Never transmitted to a client.
    Restricted,
}
```

| Peer class | Public columns | Restricted columns | May write |
|---|---|---|---|
| **Trusted** (master, child) | yes | yes | yes |
| **Client** (read-only) | no¹ | **no** | no |

¹ Only columns marked `replicated = true` are sent at all. A column that is not
replicated is local-only and is not part of the sync payload.

### Enforcement points

The restricted-column rule must hold at every point where data leaves the node:

1. **Row payload.** The server filters outgoing row payloads by peer class. Restricted
   values are never serialised for a client — not in the row payload, not in an error
   message, not in a `SELECT`.
2. **Table list.** Tables with `client_visible = false` are excluded from client sessions
   entirely, so their existence and shape are not advertised.
3. **Handshake.** A client that requests a `Restricted` column is rejected with
   `COLUMN_NOT_PERMITTED`.
4. **The digest seed.** Never sent to a client. Without it, a client cannot compute or
   verify any digest.
5. **The read cache.** The cache holds full rows including restricted columns, but it is
   local to the node. A restricted column is filtered at the *serialisation* boundary
   when serving a client, regardless of whether the row came from the cache or the
   database. The cache is therefore safe to share between peer classes, as long as
   filtering happens at the boundary — which it must anyway, because the wire format is
   the only place the distinction exists.

Point 5 is worth stating explicitly because it is the one that is easy to get wrong: the
filter belongs at serialisation, not at storage.

### Why the digest is keyed

Restricted columns may hold low-entropy values — a boolean, a status enum. An unkeyed
digest would let a client confirm a guess: submit a candidate value, observe the
resulting bucket hash change.

So the digest uses **seeded xxh3** with a deployment secret:

- shared by all trusted peers — they must agree, or every row hash differs;
- absent from clients — they cannot confirm a guess, and cannot recompute a digest.

The client receives the digest verbatim and folds it into the row hash. It never
recomputes it, and does not need to.

The consequence is that the digest seed is **deployment-wide state**, and a mismatch
between trusted peers is fatal at handshake (`DIGEST_SEED_MISMATCH`).

### The site registry is deployment-wide state too

The `sites` registry — the table mapping each site to its identifier range — is the same
kind of state as the seed and the epoch. It is not secret, but it is shared, and a node
that lacks it cannot validate the identifiers it receives.

It travels in the **join bundle**, alongside the epoch and the seed, and is exchanged
before any data in every session. A peer presenting a different registry is a deployment
error, and the range check will reject the affected rows.

The registry is also the one place where the deployment's *structure* is visible: which
sites exist, and which identifiers belong to which of them. A client that should not see
that can mark the `sites` table `client_visible = false`.

### A client that cannot write

A client holds projections, not full rows. If it could write, it could not recompute the
digest of a row it holds, and the tree would be corrupted.

The design makes this structural rather than advisory:

- the authorizer denies writes to `_sync_*` for everyone but the crate;
- the client role has no write path in the protocol at all;
- a trusted peer that lacks restricted columns cannot write to that table — it could not
  recompute the digest — and the crate enforces this with a clear error rather than
  silent corruption.

## The digest seed

| Property | Requirement |
|---|---|
| Shared across trusted peers | Yes — a mismatch makes every row hash differ |
| **Reached by children too** | Yes — a child writes rows, and every write produces a digest |
| Absent from clients | Yes — otherwise restricted values are confirmable |
| Rotation | Not supported in v1. Rotation requires a full re-digest, which means a full re-seed. |
| Storage | The consumer's responsibility — env var, mounted secret, KMS |

**The crate does not persist it.** It is supplied as configuration, held in memory for the
life of the process, and never written to the database. An earlier revision stored it in
`_sync_meta`; that was reversed.

It is a **third value**, separate from the two credential strings, and it is rotated
independently of them. Deriving it from a credential would couple a routine credential
rotation to a full dataset rebuild — every stored digest and every tree leaf would
change. See [adr/0014-peer-credentials-and-the-uuid-directory.md](adr/0014-peer-credentials-and-the-uuid-directory.md)
§ 4.

### The three secrets side by side

| | Master string | Child string | Digest seed |
|---|---|---|---|
| **Purpose** | authenticate master ↔ master | authenticate child ↔ master | key the hash |
| **A credential?** | yes | yes | **no** — it authenticates nothing |
| **Masters have it** | yes | yes | yes |
| **Children have it** | no | yes | yes |
| **Clients have it** | no | no | no |
| **Rotation cost** | re-provision the masters | re-provision the children | **full rebuild of every tree** |

The digest seed cannot be the master string: children need it to digest their own writes,
and children deliberately do not have the master string — that would let a child
impersonate a master.

## Row-level integrity

The digest covers the replicated content, so two peers holding different bytes under the
same timestamp is detectable. The `verify` command reports such rows.

It does not repair them. An edit that did not advance `last_update` cannot be ordered
correctly, because the information needed to order it was never recorded. Repairing it
would mean guessing.

## What the crate does not do

- **It does not encrypt data at rest.** That is the database's business, and SQLite's
  own mechanisms or the filesystem's.
- **It does not implement authentication.** It checks a credential it is given; producing
  and validating credentials is the deployment's business.
- **It does not implement authorisation within the application.** Only the
  public/restricted distinction is modelled.
- **It does not defend against a malicious trusted peer.** A trusted peer that lies about
  row contents will be believed, because it is trusted. The digest makes accidental
  divergence visible; it does not make a hostile peer safe.

That last point is a real limitation and worth stating plainly: the restricted-column
model protects against *over-broad reads*, not against a compromised master.

## Related documents

- [data-model.md](data-model.md) — the visibility model and the digest
- [sqlite-interface.md](sqlite-interface.md) — the authorizer as part of enforcement
- [protocol.md](protocol.md) — handshake validation
- [config.md](config.md) — where the seed and peer list live
- [adr/0004](adr/0004-digest-is-always-computed-and-keyed.md) ·
  [adr/0008](adr/0008-two-peer-identity-layers.md)
