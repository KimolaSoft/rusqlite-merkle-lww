# ADR-0020: Application-authenticated peer identity and optional per-node credentials

- **Status:** accepted
- **Date:** 2026-11
- **Deciders:** project maintainers

- **Supersedes:** [ADR-0014](0014-peer-credentials-and-the-uuid-directory.md)

## Context

[ADR-0014](0014-peer-credentials-and-the-uuid-directory.md) settled on **role-level**
credentials: one master string shared by every master, one child string shared by every
child. It recorded the consequence plainly:

> Because the credentials are role-level, every master is cryptographically identical to
> every other master. A master can present another master's UUID and be accepted.
>
> The deployment therefore gets **authorization and revocation**, but not
> **attribution**.

The directory closes part of that gap. A node's UUID must be in the directory, and
removing a UUID revokes exactly that node. But the directory says only that a UUID
**exists** — it cannot say that *this connection* is that node. Any master that knows
another master's UUID can present it and be accepted as that master, and the session
proceeds. The UUID is a claim, and the credential cannot corroborate it.

Two requirements follow from that:

1. **The application's authenticated identity must reach the session.** The
   application already authenticates its peers — with TLS, mTLS, a JWT, a service mesh
   identity — and it knows who it is talking to. The crate throws that information away
   and asks the peer to assert who it is.
2. **Attribution is wanted.** "Which node wrote this row?" is a question a deployment
   with several sites and several operators will ask.

The cost of per-node credentials, as ADR-0014 recorded, is an N×M secret distribution:
every master holding every other master's secret, and a rotation touching every peer's
configuration. That cost is real, and it is why role-level credentials were chosen. It
is also not a reason to discard the identity the application has already established.

## Decision

### 1. The crate binds the application's identity to the claimed node id

```rust
SessionContext {
    authenticated_peer:   Some(node_id),     // who the transport established
    authenticated_subject: Some("spiffe://…"), // for logs and the audit record
}
```

When `authenticated_peer` is set, the `node_id` in the HELLO **must** equal it, or the
session is refused with `IDENTITY_MISMATCH`.

This is not authentication — the crate still authenticates nobody. It is a **binding**:
the application authenticates, the crate checks that the peer is not claiming to be
somebody else. It closes the impersonation gap without changing the credential model
at all, and it costs one field.

### 2. Per-node credentials are available, and optional

```rust
pub enum PeerAuth {
    RoleSecret { role: Role, secret: SecretString },
    PerNodeSecret { node_id: Uuid, secret: SecretString },
    ClientCert { subject: String },
    TransportOnly,
}
```

Role-level strings remain the default. A deployment that needs attribution can issue a
per-node secret to a peer; the crate then knows *which* node authenticated, which
role-level credentials cannot tell it.

The two modes coexist. A deployment may use role-level strings for most peers and a
per-node secret for the ones it cares about.

### 3. Attribution is recorded locally, in an optional table

```sql
CREATE TABLE _sync_audit (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    tbl        TEXT    NOT NULL,
    k          BLOB    NOT NULL,
    peer_id    TEXT    NOT NULL,   -- the node that wrote the row
    applied_us INTEGER NOT NULL
);
```

The table is **node-local, not replicated, and not in the digest**, and it is **off by
default**. It records what *this node* knows: the peer that wrote each row it applied.

This is the honest limit of attribution in a replicated system. A node can only vouch
for the rows it applied itself. Making attribution replicated would mean putting the
writer's identity into the row, and therefore into the digest — and two peers holding
the same logical row under different attribution would then compute different digests
and never converge.

### 4. The origin travels with the row

Every ROWS batch carries the origin of the rows it contains, so that write authority
can be evaluated against the node that wrote them. See
[ADR-0015](0015-per-table-merge-policies-and-write-authority.md).

## Consequences

**Positive**

- **The impersonation gap closes** for any deployment whose application authenticates
  its peers — which is every deployment that terminates TLS.
- **Attribution is available** without changing the default credential model.
- **No new secret to distribute** in the common case. The identity is already there.
- **The audit cannot affect convergence.** It is off the digest and off the wire.

**Negative**

- **The application must pass the identity.** A deployment that does not authenticate
  its peers gets the old behaviour: a UUID is a claim, and role-level credentials
  cannot corroborate it.
- **Per-node secrets are still N×M.** The opt-in mode has the distribution cost
  ADR-0014 recorded. It is opt-in for that reason.
- **The audit is a local view.** Each node has its own record, and they will not
  necessarily agree about a row neither of them applied.

**Neutral**

- Role-level credentials, the master string and the child string are unchanged.
- The directory is unchanged: it still answers "who exists", and revocation by
  removing a UUID still works.

## Alternatives considered

**Make per-node credentials the only mode.** Rejected: it forces an N×M secret
distribution on every deployment, including the ones that do not need attribution, and
it makes key rotation — which ADR-0014 made a stated goal — expensive.

**Add a writer id to the row.** Rejected: it would enter the digest, and two peers
holding the same logical row written by different peers would compute different
digests. That is the divergence this whole design avoids.

**Have the crate authenticate the peer itself.** Rejected: the crate has no TLS
dependency and no opinion about the application's auth stack. It would duplicate what
the application already does, worse.

**Rely on the directory alone.** Rejected: it says a UUID exists, not that this
connection is that node.

**Make the audit table replicated.** Rejected: see above — attribution in the row
means divergence.

## Related

- [../security.md](../security.md) — the authenticated identity, attribution
- [../protocol.md](../protocol.md) — the handshake and the origin
- [../config.md](../config.md) — the credential modes and the audit flag
- [ADR-0014](0014-peer-credentials-and-the-uuid-directory.md) — the role-level
  credential decision this ADR supersedes
