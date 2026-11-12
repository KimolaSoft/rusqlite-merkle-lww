# Plan — `merkle-lww-cli`

**Crate.** `crates/merkle-lww-cli` · a binary · depends on `merkle-lww-axum` and
`merkle-lww-core`.

**Goal.** Operational tooling.

The `inspect`, `tree` and `verify` subcommands are **not optional extras** — Merkle
bugs are undebuggable without them. See
[operations.md](../doc/operations.md).

## Contents

| Module | Holds |
|---|---|
| `main.rs` | the subcommand dispatch: `serve`, `pull`, `push`, `inspect`, `tree`, `verify` |

---

## X1 — `inspect`, `tree` and `verify`

### Deliverables

- `inspect`: the local state — the registered tables, the log's shape, the tree's
  shape, the last purge instant, the peer list, and **the contents of a bucket**: which
  keys, which `last_update`, which digest.
- `tree`: the bucket tree, at a given level, with the hash of each bucket.
- `verify`: the divergence report.

### Notes

- **`verify` reports and does not repair.** A tool that repairs silently is a tool
  that hides the bug it was called to find. Repair is a deliberate act, and it is the
  re-seed.
- `inspect` and `tree` are the tools that make a divergence diagnosable. They must
  work on a database the crate did not open — read-only, and without taking the
  writer lock.
- The output is **text and JSON**, and the JSON is stable enough to be diffed between
  runs. A tree dump that changes shape between versions is not diffable, and a diff
  that is not possible is not a diagnostic.

### Exit criteria

- `verify` reports a divergence introduced by an out-of-band edit, and names the
  **rows** whose stored digest disagrees with the tree.
- `verify` reports **nothing** on a converged pair of databases — a tool that always
  finds something is not a diagnostic.
- `tree` output at two levels is consistent: the parent hash is the fold of its
  children's.
- `inspect` and `tree` work on a database opened read-only by another process.

---

## X2 — `serve`, `pull` and `push`

### Deliverables

- `serve`: the axum server, bound to an address, with the TLS and the authentication
  left to the deployment in front of it.
- `pull` and `push`: the client side of a session, over HTTP or WebSocket.

### Notes

- **The CLI terminates no TLS.** It expects TLS to be terminated in front of it, or the
  operator to have accepted the risk explicitly.
- **`pull` and `push` are thin.** They open a session and drive it; the session logic
  is in `core`. A CLI that reimplements the protocol is a second implementation, and
  the point of the protocol crate is that there is one.
- The client side is resumable: a `pull` interrupted and re-run continues from the
  durable session's cursor rather than starting again.

### Exit criteria

- `serve` starts a server that a `pull` client can complete a session against.
- An interrupted `pull` resumes from its cursor.
- The CLI's error output names the protocol error code, so a failure is diagnosable
  against the error table.
- **No secret appears in any log line or error message** — the `SecretString` audit
  covers the CLI's output too.

---

## Crate-level exit criteria

- `cargo clippy` is clean; `cargo fmt --check` is clean.
- The CLI's `--help` output names every subcommand and its options.
- The CLI is covered by the `SecretString` audit: no credential in any log line or
  error message.
- **Suite 10 (visibility) passes for the CLI's output**: no restricted value appears in
  any line the CLI writes.

**Test suites:** the operational tooling tests; Suite 10 as it applies to the CLI's
output.
