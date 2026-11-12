# ADR-0008: Two peer identity layers

- **Status:** partially superseded by [ADR-0014](0014-peer-credentials-and-the-uuid-directory.md), [ADR-0020](0020-application-authenticated-peer-identity-and-optional-per-node-credentials.md)
- **Date:** 2026-11
- **Deciders:** project maintainers

> **Partially superseded.** The **two-layer model** this record decides — a stable node id
> plus a separate credential — **stands**. Two details have moved on:
>
> - The `PeerAuth` enum below is superseded. The current enum, which adds a per-node
>   secret alongside the role-level one, is in
>   [ADR-0020](0020-application-authenticated-peer-identity-and-optional-per-node-credentials.md).
> - The amendment below is carried forward by
>   [ADR-0020](0020-application-authenticated-peer-identity-and-optional-per-node-credentials.md),
>   which supersedes ADR-0014 and adds the application's authenticated identity as the
>   binding of a claimed node id.

## Context

The deployment has masters, children and clients that need to identify each other
reliably. The requirement was:

> Need some kind of 'constant key' uuid whatever to denote valid peers, assumed ssl
> probably with tls.

Two properties are needed, and they are distinct:

1. **Identity** — a stable name for a peer that survives restarts, redeployments, and
   TLS termination.
2. **Credential** — proof that the peer presenting that identity is entitled to it.

A `node_id` UUID provides the first but not the second. It is a *claim*, and a claim
that is accepted without verification is just an authorisation bypass: anyone who can
reach the endpoint can present any UUID.

## Decision

**Two layers, both required.**

### Layer 1 — identity

```rust
pub struct Peer {
    pub node_id:   Uuid,     // stable, persisted in _sync_meta
    pub name:      String,
    pub role:      Role,
    pub endpoint:  String,
    pub auth:      PeerAuth,
}
```

`node_id` is generated once and persisted. At handshake, the node checks the presented
`node_id` against its **configured peer list**. An unknown identity is rejected with
`UNKNOWN_PEER`.

The peer list is the trust boundary, and it is deployment-local configuration.

### Layer 2 — credential

```rust
pub enum PeerAuth {
    Token(SecretString),
    ClientCert { subject: String },
    TransportOnly,
}
```

`SecretString` does not implement `Debug` as its contents, so tokens do not leak into
logs or error messages.

### TLS is an assumption, not a feature

**The crate does not terminate TLS.** TLS terminates in front of it — an ingress
controller, a service mesh, a reverse proxy, or `axum-server` with rustls in the
consuming application.

The crate authenticates the peer with whatever credential the deployment already has.

See [ADR-0011](0011-crate-provides-the-exchange.md) for the full division of
responsibility between the crate and the application.

## Amendment

[ADR-0014](0014-peer-credentials-and-the-uuid-directory.md) extends this decision
(it has since been superseded by
[ADR-0020](0020-application-authenticated-peer-identity-and-optional-per-node-credentials.md),
which carries these points forward):

- the `node_id` is **issued by a master**, not self-generated;
- a **UUID directory** sits between identity and credential — every node's UUID, merged
  across masters at handshake, and withheld from children;
- the credential is **role-level** (a master string and a child string), not per-node;
- the **digest seed** is a third value, separate from both credential strings and rotated
  independently.

## Consequences

**Positive**

- **No TLS stack in the dependency list.** The crate stays free of `rustls` or
  `native-tls`, which is one of the heaviest dependencies a Rust project can carry, and
  one that every deployment already has.
- **The trust boundary is explicit.** A node talks only to peers it has been configured
  with. This is a list a human can read and audit.
- **Identity survives infrastructure changes.** `node_id` is in the database, not
  derived from a certificate or an IP address, so it survives TLS termination,
  redeployment, and a change of address.
- **The credential is pluggable.** A deployment with mTLS uses `ClientCert`; one with a
  service mesh uses `TransportOnly`; one with nothing but a shared secret uses `Token`.
- **Secrets cannot leak into logs.** `SecretString` and the deliberate absence of
  credential detail from error messages.

**Negative**

- **Two mechanisms must both be correct.** A peer with a valid credential but an unknown
  `node_id` is rejected; a peer with a known `node_id` but no valid credential is
  rejected. Both checks must be wired up.
- **A misconfigured peer list is a silent availability problem.** If the master does not
  know the child's `node_id`, the connection fails — loudly, but with a configuration
  error rather than a protocol error, which is harder to diagnose.
- **The credential check is not the crate's job in the `TransportOnly` and `ClientCert`
  cases.** The crate checks a token itself; for certificates it relies on the transport
  or the proxy. If that verification is misconfigured, the crate cannot tell.

**Neutral**

- `node_id` is exchanged in the handshake and checked. The `UNKNOWN_PEER` error does not
  distinguish "not configured" from "not yet configured on this node", which is
  deliberate — the distinction would leak the peer list to an unauthenticated party.

## Alternatives considered

**A shared secret only, no per-peer identity.** Rejected: every peer is
indistinguishable, so a credential leak compromises the whole deployment, and there is
no way to tell which peer is misbehaving.

**Certificate fingerprints as the identity.** Rejected: identity would change when a
certificate is renewed, and it would couple the sync protocol's notion of a peer to the
transport's.

**Trust-on-first-use.** Rejected: it makes the first connection authoritative, which is
exactly the wrong property for a system that replicates deletions.

**A peer directory service.** Rejected: it adds a dependency and an availability
requirement to a system whose whole point is that peers can come and go. A static,
deployment-local list is simpler and more robust.

**The crate terminates TLS.** Supported, but outside the crate: the consuming
application can run `axum-server` with rustls and use `PeerAuth::ClientCert`. The
decision is only that the *crate* does not take on that dependency.

## Related

- [../security.md](../security.md) — the full trust model
- [../protocol.md](../protocol.md) — handshake validation
- [../config.md](../config.md) — the peer list
- [ADR-0011](0011-crate-provides-the-exchange.md) — the crate/application division
