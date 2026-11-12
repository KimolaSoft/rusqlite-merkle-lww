# Plan — `merkle-lww-axum`

**Crate.** `crates/merkle-lww-axum` · depends on `merkle-lww-core`,
`merkle-lww-protocol`, `merkle-lww-model`, `tokio` and `axum`.

**Goal.** The transport: the protocol over HTTP and WebSocket, as an optional
convenience over a transport-agnostic core.

**This is where the async boundary is**, and where `tokio` lives. The core is
synchronous, and every database call from async context goes through `spawn_blocking`.

**This is also where compression lives.** The wire format is JSON; the transport may
apply a content encoding. See
[ADR-0023](../doc/adr/0023-json-wire-format-and-transport-compression.md).

## Contents

| Module | Holds |
|---|---|
| `rest.rs` | the REST endpoints |
| `ws.rs` | the WebSocket session handling |
| `progress.rs` | the progress frames and the progress endpoints |
| `compression.rs` | the zstd content encoding |

---

## A1 — REST

### Deliverables

- The REST endpoints from [protocol.md](../doc/protocol.md) § "REST":
  `POST /v1/sync/hello`, `/summary`, `/detail`, `/rows`, `/commit`.
- The session registry: opaque `session_id`, in memory, five-minute idle TTL.
- The handlers, built on `core`'s transport-agnostic exchange.
- **The non-fatal refusals**, carried over the wire with their documented code:
  `COLUMN_NOT_PERMITTED` — the column is dropped from the projection, the request is
  logged, and the session continues; `TABLE_NOT_PERMITTED`; `WRITE_NOT_PERMITTED`;
  `ROW_PURGED`; `SESSION_LIMIT`. A refusal that ends the session is a different thing
  from a refusal that does not, and the transport has to be able to express both.

### Notes

- **The handlers do not know the protocol.** They serialise a message, call the
  exchange, and serialise the answer. The session logic is in `core`; the handler is
  a translation layer.
- **Sessions are addressed by an opaque `session_id`**, held in memory with a
  five-minute TTL. A single `POST /v1/sync` with a `step` discriminator is an
  acceptable simplification if the route count matters more than clarity.
- **The crate does not terminate TLS.** The application does, usually at a proxy or
  ingress, and handles authentication. The application's authenticated identity is
  passed in and bound to the claimed node id.
- **Progress is X of Y**, and `Y` is always known before the phase starts.

### Exit criteria

- Suite 15 (transport integration) passes for REST.
- **Every handshake rejection case returns the documented error code** over the wire,
  not just in the pure function.
- **The pull-only client flow works**: the server pushes the diff, the client applies
  it — **in bounded pages**, with the same budget as a bidirectional session.
- A session that is idle past the TTL is refused, and a session with a persisted cursor
  survives it.
- **No database call happens on the reactor thread** — asserted by a test that runs the
  handlers under a runtime configured to panic on a blocking call.

**Test suites:** 12, 15, 17.

---

## A2 — WebSocket and progress

### Deliverables

- The WebSocket handler: `GET /v1/sync/ws`.
- `progress.rs`: the progress frames, throttled to 250 ms.
- The connection **is** the session; `session_id` is allowed but not required.
- Heartbeat every 30 s; drop the connection after two missed heartbeats.

### Notes

- **The server pushes `{"type":"changed","buckets":[…]}`** when its snapshot advances.
  A pull-only client uses this as a trigger to open a session.
- **Progress frames flow on the same socket**, and are only sent when `completed`
  advances — a progress stream that emits on every row is a progress stream nobody
  reads.
- The snapshot is an immutable `Arc<SyncSnapshot>` behind an `ArcSwap`; readers call
  `arc.load_full()` — a refcount bump, no lock, no contention.

### Exit criteria

- Suite 12 (progress) passes: every phase's `total` is known **before** the phase runs,
  `completed` never exceeds `total`, and the denominator changes correctly between
  phases.
- **A WS progress frame is emitted at the configured cadence**, and not more often.
- A real client against a real server completes a session; the connection is the
  session.
- The connection drops after two missed heartbeats.

**Test suites:** 12, 15.

---

## A3 — Compression

### Deliverables

- **zstd level 1 by default**, on the REST path, with a configuration switch to disable
  it.
- The `Content-Encoding` handling.
- The WebSocket path uses whatever the transport negotiates.

### Notes

- **Compression is not part of the protocol.** It is a transport-level content
  encoding, and both peers must agree. A transport that cannot compress — a
  constrained device, a proxy that rewrites bodies, a message queue — must still be
  able to speak the protocol.
- **zstd level 1, not level 9.** Measured on the shapes this protocol carries, level 9
  buys 2.3 % over level 1 at roughly three times the time.
- **The REST path gets zstd; the WebSocket path does not.** The deployed WebSocket
  compression extension is `permessage-deflate`, which is deflate, not zstd.
- **`max_message_bytes` is measured on the JSON message**, before compression. The
  budget bounds the crate's page-assembly work and its staged-page writes, so it is
  measured on the message the crate produces. Compressed bytes are always fewer, so
  the wire is never the binding constraint.
- **A transport must not mix attacker-controlled values and secret values in one
  compressed stream.** The compression side channels need both in the same stream and
  a measurable compressed length. This protocol's threat model does not obviously
  expose it — a client session's projection excludes restricted columns, and a trusted
  peer already holds them — but the transport is the application's, and the application
  may be carrying more than this protocol.

### Exit criteria

- A compressed REST session is byte-identical after decompression to an uncompressed
  one, and both converge to the same state.
- **Compression can be turned off** and the session still works.
- A peer that compresses and a peer that does not produce a clear error, not a silent
  misparse.
- The wire-bytes half of Suite 10 passes: no restricted value appears anywhere in the
  wire bytes.

**Test suites:** 10, 15.

---

## A4 — Metrics and the health endpoint

### Deliverables

- The metrics from [operations.md](../doc/operations.md) § "Metrics" — the counters and
  gauges a Prometheus-style scrape reads.
- `GET /v1/health`, returning per-peer state: last successful sync, whether it is stale,
  the current session and its progress, and whether the node is inside a purge quiesce
  window.

### Notes

- **The crate owns the counters; the transport owns the endpoint.** The engine knows
  what it compared, sent, applied and purged; the transport is what a scraper can
  reach. The metrics are therefore produced in `merkle-lww-core` and exposed over
  HTTP here.
- **The diff ratio is the metric that matters.** `sync_buckets_differing_total` over
  `sync_buckets_compared_total` is near zero in a healthy deployment, and a ratio that
  climbs steadily means peers are diverging.
- **`sync_peer_stale` is the one to alert on.** A peer creeping toward the staleness
  horizon is about to require a re-seed, and a re-seed discards its unsynced writes.
- **The health endpoint reads the same state the engine holds.** It does not poll the
  peers, and it does not open a session.
- `clock_offset_estimate_us` — the node's estimate of its peers' clock offsets — is
  reported alongside, because a skew is the cause of a `CLOCK_SKEW` refusal and an
  operator needs to see which peer is off.

### Exit criteria

- Every metric in [operations.md](../doc/operations.md) § "Metrics" is exposed, and the
  names match the table exactly.
- The health endpoint returns the per-peer state without opening a session, and without
  touching a peer.
- A metric counter is monotonic and a gauge is not — a counter that goes down is a bug,
  and the test says so.

---

## Crate-level exit criteria

- The crate builds, and `cargo clippy` is clean with no warnings.
- **The core has no dependency on this crate**, and the exchange works over a non-HTTP
  transport.
- The crate does **not** terminate TLS, and says so in its documentation.
- Documentation is updated to match the implementation.

**Test suites:** 10, 12, 15, 17, 20, 22.
