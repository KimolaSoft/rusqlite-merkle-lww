# ADR-0018: Bounded messages, paged transfer, and durable client sessions

- **Status:** accepted
- **Date:** 2026-11
- **Deciders:** project maintainers

## Context

The wire protocol had no bounds on the size of a message or the length of a session.

The ROWS phase sends the rows of the differing buckets. A bulk import into one hour
produces one hour's worth of rows in one message — which for a table with a large
payload column is tens or hundreds of megabytes. There is no partial answer: a client
either receives the whole thing or fails, and a transport with a body-size limit
rejects the request outright.

The same applies to the pull-only flow. The protocol says "the server sends the full
diff proactively", which is a full diff of an entire dataset — the largest thing the
protocol can possibly send, in one message, to a read-only client on a link that may be
slow.

Sessions compound it. A session is held in memory with a five-minute TTL, and there is
no limit on how many round trips it may make. A peer that keeps a session open can hold
a connection indefinitely.

And a read-only client on an unreliable link has nowhere to put what it has received.
If it crashes halfway through a large transfer, it starts again — which for a client
whose job is to hold a copy of a large dataset is not a recoverable situation.

## Decision

### 1. A message is bounded

`max_message_bytes` (default 4 MiB) is a soft budget the crate enforces on its own
output. The sender stops adding rows when the encoded message would exceed it and
returns a cursor instead:

```jsonc
{ "rows": [ … ],
  "next_cursor": { "tbl": "device", "bucket_id": 29681, "level": "hour",
                   "last_key": "…", "as_of_us": … },
  "done": false }
```

The budget is a soft one because the transport may impose a smaller limit of its own;
the effective limit is the smaller of the two.

The SUMMARY hash list is bounded the same way. A truncated hash list is always a
**prefix** of the full one, ordered by `(level, bucket_id)`.

### 2. The cursor is deterministic and resumable

The cursor names the bucket, the last key sent within it, and the session's `as_of_us`.
Rows within a bucket are ordered by their canonical key bytes, so both sides agree on
the order without exchanging it.

Re-issuing a ROWS with a cursor continues exactly where the last page stopped.
Re-sending a page is harmless — the merge is idempotent.

### 3. A session is bounded, and the bound is retryable

`max_rounds_per_session` (default 1000) ends a session with `SESSION_LIMIT`. That is a
**retryable** outcome, not a failure: the caller re-opens the session with the last
cursor and continues. The bound exists to stop a session running forever and to give
the transport a place to yield — not to fail a legitimate large transfer.

### 4. The pull-only flow uses the same machinery

The server's proactive push is paged with the same budget and the same cursors, and it
respects the same table ordering. A pull-only client therefore has the same bounds as
a bidirectional peer.

### 5. A client can resume after a restart

A client persists its session in a node-local `_sync_session` table, and stages the
pages it receives before applying them:

```sql
CREATE TABLE _sync_session (
    session_id TEXT PRIMARY KEY,
    peer_id    TEXT NOT NULL,
    profile    TEXT NOT NULL,
    state      TEXT NOT NULL,
    cursor     BLOB,
    as_of_us   INTEGER,
    created_us INTEGER NOT NULL,
    updated_us INTEGER NOT NULL
);
```

The cursor advances only after the applying transaction commits, so a crash between
"received" and "applied" loses nothing.

The five-minute TTL becomes an **idle** TTL. A session with a persisted cursor survives
a restart: the client re-opens it by id and asks for the next page.

### 6. Ordering

Tables are synced in ascending `sync_priority` — core tables before large history
tables — and within a table the buckets are walked **newest-first**, so the data a user
is most likely to read lands in the database before the history nobody is waiting for.

## Consequences

**Positive**

- **No message is unbounded.** The largest thing on the wire is one page, and the page
  size is a configuration value.
- **A large transfer is resumable.** A client that crashes resumes; a session that hits
  the round limit continues; a dropped connection is re-issued from a cursor.
- **The pull-only flow is no longer special.** One paging mechanism, one set of bounds.
- **Useful data arrives first.** The ordering means a client catching up after an
  outage is usable long before it is complete.

**Negative**

- **More round trips.** A large transfer takes more messages, each smaller. This is the
  cost of the bound.
- **The client keeps state on disk.** `_sync_session` and the staged pages are durable
  state that must be cleaned up, and a client that never finishes a transfer keeps
  its staged rows.
- **The cursor is protocol state.** It has to be preserved across restarts, and a
  cursor from a different `as_of_us` is not usable.

**Neutral**

- The message format is otherwise unchanged; paging is an extension of the ROWS
  request and response.
- Idempotency is unchanged: applying the same row twice is still a no-op.

## Alternatives considered

**Leave messages unbounded and let the transport deal with it.** Rejected: the
transport is the application's, and its limits are not the crate's to know. A crate
that only works with transports configured a particular way is not transport-agnostic.

**Use a fixed row count per page.** Rejected: a row count says nothing about bytes. A
page of ten rows with a megabyte payload each is worse than a page of a thousand small
rows. The budget is in bytes, and the row count follows from it.

**Make the round limit fatal.** Rejected: a legitimate transfer can exceed it. A
retryable limit with a cursor lets the caller continue; a fatal one would fail the
transfer for a reason that has nothing to do with correctness.

**Drop the session on restart.** Rejected: that is the case this decision exists for.
A read-only client whose job is to hold a large dataset cannot restart its transfer
from scratch every time it restarts.

**Order tables oldest-first.** Rejected: the newest data is what a user is waiting
for. History is what nobody is waiting for.

## Related

- [../protocol.md](../protocol.md) — paging, cursors, durable sessions
- [../data-model.md](../data-model.md) — the `_sync_session` table
- [../bulk-apply.md](../bulk-apply.md) — applying a page on the receiving side
- [../operations.md](../operations.md) — what `SESSION_LIMIT` means in practice
