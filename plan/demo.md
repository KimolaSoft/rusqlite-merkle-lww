# Plan — `merkle-lww-demo`

**Crate.** `crates/merkle-lww-demo` · depends on `merkle-lww-core`,
`merkle-lww-protocol`, `merkle-lww-model`.

**Goal.** The conformance harness — the answer to "how do we know the interface is
right?"

This crate exists to make the interface **provable** and **testable**:

- **provable** — the wire contract is pinned by bytes, not by prose, so a second
  implementation in another language can be checked against it, and a change to the
  wire format is a visible diff rather than a silent one;
- **testable** — the failure modes that matter are reachable deterministically, with
  no ports, no network and no sleeping.

**It is a crate rather than a directory of integration tests** so that the examples are
**compiled and run** by CI. A worked example that is not executed is documentation that
has already drifted.

## Contents

| Module | Holds |
|---|---|
| `transport.rs` | the in-memory `Transport` |
| `clock.rs` | the scripted clock |
| `fixture.rs` | the two-node fixture |
| `vectors.rs` | the topology and scenario fixtures |

---

## D1 — The in-memory transport and the scripted clock

### Deliverables

- `transport.rs`: an in-memory implementation of the transport trait — a channel between
  two peers in one process.
- `clock.rs`: a clock the test controls, so time advances when the test says so.

### Notes

- **The transport is the reason the convergence test is possible.** The core's
  exchange is transport-agnostic by design, and this is where that pays: several `Db`
  instances, each with its own database, exchanging through an in-memory transport, in
  one process.
- **The scripted clock replaces sleeps with time advancement.** The failure modes in
  this design are all about *time*: a purge instant that arrives, a quiesce window
  that opens and closes, a staleness horizon that is crossed. Every one of them is
  otherwise a `sleep`, and a `sleep` is a flaky test.
- **The clock is single-threaded.** Two peers driven by one clock advance in lockstep,
  so the order of events is deterministic rather than merely likely.
- **The transport can be made to misbehave deliberately**: drop a message, reorder two,
  or stall a peer past an instant. The failure paths are the ones worth testing, and
  they are otherwise the ones hardest to reach.

### Exit criteria

- Two peers exchange a full session in one process, with no ports and no network.
- **A dropped, reordered and stalled message are each exercised**, and the session
  either recovers or fails with the documented outcome.
- **Advancing the clock by 31 days crosses the staleness horizon**, and the next
  session is refused with `STALE_PEER` — with no wall-clock time passing.
- The suite runs in under a second, and is not flaky across a hundred consecutive
  runs.

---

## D2 — The two-node fixture

### Deliverables

- `fixture.rs`: two `Db`s over in-memory SQLite, exchanged through the in-memory
  transport, with the scripted clock and a shared configuration.

### Notes

- **The fixture is the convergence suite's subject.** It is what makes Suite 1
  runnable for two and for three nodes.
- The fixture owns the **seeded identifiers** and the **fixed epoch**, so a test does
  not depend on the wall clock or on randomness that is not seeded.
- The fixture can produce the specific states the lifecycle tests need: a peer that
  has been offline across a purge instant, a peer that is stale, a table with
  tombstones due and a table without.

### Exit criteria

- The fixture brings two nodes to a converged state and back, repeatedly, with no
  flakiness.
- The fixture can be driven into each lifecycle state — offline across a purge,
  stale, quiescing — without the test having to construct those states by hand.
- **The fixture is the only place in the workspace that knows how to stand up a node**,
  so a change to the setup is a change in one file.

---

## D3 — The topologies, the scenarios and the worked example

### Deliverables

- `vectors.rs`: the **topology** and **scenario** fixtures — the multi-node topologies
  for the convergence test, and the named lifecycle scenarios.
- The **wire vectors are consumed, not published**: `merkle-lww-protocol` publishes
  `wire_vectors.json`, because the protocol crate is what defines the wire. This crate
  asserts against them, so a change to a message shape fails here as well as there.
- A worked example that runs two nodes end to end and prints what happened.

### Notes

- **The vectors pin the wire contract by bytes.** A message shape is a contract
  between two independently-written implementations, and prose is a poor way to state
  one. The vectors are JSON, so another language can consume them, and they are
  byte-exact, so a difference is a difference.
- **The worked example is compiled and run.** It is the first thing a new reader
  should be able to read, and the reason `doc/examples.md` can point at running code
  rather than at a description of code.
- **A change to a message shape is a visible diff in the vectors.** That is the
  property that makes the wire format safe to evolve: the review sees the bytes.
- **The topologies and the scenarios belong here.** They are the harness's: the
  topologies say how many nodes and in what shape, and the scenarios say what the
  clock should do.

### Exit criteria

- **Every message type has a vector** in `merkle-lww-protocol`, and the vectors are
  stable — a change to one is a diff a reviewer can see.
- **The vectors are consumable by another language**: JSON, with the bytes spelled
  exactly, and with no Rust-specific encoding anywhere in them.
- **This crate asserts against the wire vectors**, so a message-shape change fails the
  harness too. The vectors are not a snapshot that is regenerated on demand — a
  failure means the wire changed, and that is a decision to be made deliberately.
- **The worked example runs in CI** and its output is asserted, so it cannot rot.
- The topology and scenario fixtures are published, and every suite that uses them
  names the fixture it ran against.

---

## Crate-level exit criteria

- The crate is **not published**. It is a test and example harness, and it must not
  become a place where product code accumulates.
- `cargo clippy` is clean; `cargo fmt --check` is clean.
- The crate's dependency set is exactly `core`, `protocol` and `model` — it does not
  depend on `axum`, so the harness runs without a runtime.

**Test suites:** the wire-vector assertions, the two-node fixture, the scripted-clock
scenarios.
