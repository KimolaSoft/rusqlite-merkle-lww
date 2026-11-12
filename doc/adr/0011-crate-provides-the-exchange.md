# ADR-0011: The crate provides the exchange; the application provides the transport

- **Status:** accepted
- **Date:** 2026-11
- **Deciders:** project maintainers

## Context

The crate replicates data between nodes. Something has to carry the messages between
them: HTTP, WebSocket, or something else.

The requirement was:

> I don't think we care about tls ourselves specifically, we'd also want both REST and WS
> but -- the application can handle that and just make a call to the crate to do the data
> exchange. But it should be in the doc guidance for implementation to make sure people
> know they should use it.

Three things are being asked for:

1. The crate does not terminate TLS, and does not own the network layer.
2. Both REST and WebSocket should be available.
3. The application calls into the crate to perform the exchange — and the documentation
   should tell implementers to do so, rather than leaving them to reimplement the diff.

The third point matters more than it looks. A sync protocol implemented naively — compare
row sets, transfer differences — is *almost* right, and the ways it is wrong are subtle:
an off-by-one in the comparison range, a missing tie-break, a cursor advanced before the
data is durable. Each of those produces divergence rather than an error, and divergence is
expensive to diagnose.

## Decision

**The crate owns the exchange. The application owns the transport.**

The core exposes the exchange as a transport-agnostic operation:

```rust
let report = db.perform_exchange(peer, transport)?;
```

The core is **synchronous**; an async caller moves the call off the reactor thread with
`spawn_blocking`.

The `transport` is anything that can carry request and response messages. The crate does
not know or care whether that is an HTTP body, a WebSocket frame, or a message queue.

The application supplies:

- TLS termination;
- authentication;
- routing, middleware, session management.

The crate supplies:

- the diff, the merge, the LWW resolution;
- the message type and the wire format;
- the session state machine and progress accounting;
- the `_sync_log` and the Merkle tree.

### Both REST and WebSocket are provided

`merkle-lww-axum` provides both, because writing a correct session handler is work an
application should not have to repeat, and getting it subtly wrong produces divergence
rather than an error.

An application that already has a session layer, or that wants to expose sync over
something else, calls the core directly instead. The axum crate is a convenience, not a
requirement.

### The guidance is written down

[implementation.md](../implementation.md) exists specifically to tell application
developers to use the crate's exchange rather than reimplementing it, and to document the
integration mistakes that cause divergence rather than errors.

## Consequences

**Positive**

- **No TLS dependency.** `rustls` or `native-tls` does not appear in the crate's tree.
  Every deployment already has a TLS story, and this one is not in the business of adding
  a second.
- **Usable from any web framework.** The core does not depend on axum. An application on
  actix, rocket, warp, or a bespoke server can mount the exchange.
- **The application keeps its own architecture.** No imposed routing, no imposed session
  model, no imposed middleware.
- **The exchange logic cannot be reimplemented by accident** — or rather, if the
  application uses the crate, it cannot be. That is the point of the guidance document.

**Negative**

- **The crate's own REST and WebSocket implementations may not fit every application.**
  An application with unusual requirements will call the core and handle the session
  itself.
- **Two session implementations to keep correct** — the crate's and the application's, if
  the latter is written. The crate's is the reference; the application's is the risk.
- **The core API has to be genuinely transport-agnostic**, which rules out designs where
  the session logic knows about HTTP. This constrains the internal shape of the session
  state machine: it must be expressible as "given this request, produce this response".

**Neutral**

- The crate's message type is the wire format. An application using a different framework
  still speaks the crate's protocol, so peers are interoperable regardless of what the
  application looks like.

## Alternatives considered

**The crate terminates TLS.** Rejected: it adds a heavy dependency for something every
deployment already does, and it prevents the crate from being used behind a proxy or a
service mesh without a double-termination configuration.

**The crate provides only the core, with no transport at all.** Rejected: then every
application writes its own session handler, and the ways to get that subtly wrong produce
divergence rather than errors. Providing a reference implementation is worth more than
the freedom not to.

**The crate provides only REST, and applications layer WebSocket on top.** Rejected: the
WebSocket path is not REST plus a long poll — it has server-initiated notifications and
progress frames, which need their own handling.

**The application reimplements the exchange.** Rejected explicitly. This is the failure
mode the crate exists to prevent.

## Related

- [../architecture.md](../architecture.md) § "Transport"
- [../implementation.md](../implementation.md) — the guidance for application developers
- [../protocol.md](../protocol.md) — the message set
- [ADR-0008](0008-two-peer-identity-layers.md) — peer identity and credentials
