# The wire protocol

JSON on the wire, wire version `merkle-lww/1`. The same message set is used over REST
and WebSocket.

**Compression is not part of the protocol.** A transport may apply a content encoding —
`merkle-lww-axum` enables zstd level 1 by default — but the encoding is negotiated at
the transport layer, and both peers must agree. See
[ADR-0023](adr/0023-json-wire-format-and-transport-compression.md).

## Roles in a session

| Local role | Remote role | Direction |
|---|---|---|
| Child | Master | push local writes, pull master data |
| Master | Child | pull child writes, push master data |
| Master | Master | bidirectional |
| Master | Client | push only; the client pulls |

"Pull-only" is a role flag, not a separate protocol. In a bidirectional session the
upload and download phases are interleaved in one ROWS step — each side sends its
differing rows and receives the peer's — so there is no separate push API.

## Message encoding

This section is normative. It defines how a value, a row and a message are represented.

### Values

SQLite is dynamically typed, and the digest distinguishes `INTEGER` from `REAL` and
`TEXT` from `BLOB`. The wire must distinguish them too:

| Type | On the wire |
|---|---|
| NULL | `null` |
| INTEGER | a JSON number in **integral syntax** — no fraction, no exponent |
| REAL | `{"$f": <JSON number>}` |
| TEXT | a JSON string |
| BLOB | `{"$b": "<base64>"}` — RFC 4648, standard alphabet, padded |

A plain JSON number with a fraction or an exponent is an **error**. A producer that
means `REAL` must write `{"$f": …}`. The asymmetry is deliberate: integers are the
common case and stay plain, while the two cases JSON cannot express unambiguously are
wrapped.

### Rows

A row's values are a **positional array**, in the order given by the **projection** for
that table — not an object keyed by column name. The columns are declared once, in the
handshake, with their names and types.

```
mode = "digest":  "rows": [ [ <key>, <digest> ], … ]
mode = "row":     "rows": [ [ <key>, <digest>, [ <value>, … ] ], … ]
```

- `<key>` is the canonical key, base64.
- `<digest>` is the 16-byte digest, big-endian, base64.
- `<value>` is a value in the form above.

The **digest is present in both modes**. It is required in `row` mode because a peer
whose projection is narrower than the table's replicated column set cannot recompute
it, and because a peer that *can* recompute it should, as a check.

A peer with the full projection recomputes the digest from the received values and
compares it with the one it was sent. A peer with a narrower projection accepts the
digest verbatim and never recomputes it. See
[ADR-0016](adr/0016-visibility-profiles-mapped-to-peer-roles.md).

### The handshake declares the columns

```
"tables": [ { "name": "device",
              "kind": "keyed",
              "key": { "strategy": "global_id", "key_columns": ["id"] },
              "columns": [ { "name": "payload", "type": "text",
                             "replicated": true, "visibility": "public" },
                           { "name": "secret",  "type": "blob",
                             "replicated": true, "visibility": "restricted" } ],
              "write_authority": "any" } ]
```

The **projection** is the ordered list of columns a session actually carries, and the
row array follows it:

```
"projection": [ { "table": "device",
                   "columns": [ { "name": "payload", "type": "text" } ] } ]
```

The projection's **order is the row array's order**, and it carries each column's
**type** — so a projection is self-sufficient for decoding a row, and a client that
does not have the table registered can still apply one. A peer decodes a row by
walking the projection, not the table's full column list — which is what lets a narrow
peer decode a row it could not have computed a digest for.

### Unknown fields are ignored

A receiver ignores a field it does not know. That is what makes the format extensible
without a version bump for every addition.

### The byte budget

`max_message_bytes` is measured on the **JSON message**, before any transport-level
content encoding. The budget bounds the crate's page-assembly work and its staged-page
writes, so it is measured on the message the crate produces. Compressed bytes are
always fewer, so the wire is never the binding constraint.

## Handshake

```jsonc
{
  "protocol": "merkle-lww/1",
  "node_id": "…",              // 128-bit hex
  "site": "eu-west",
  "role": "master",            // master | child | client
  "profile": "master",         // the visibility profile this session runs under
  "epoch_us": 1735689600000000,
  "as_of_us": 1792953600000000,
  "last_purge_instant": 1792617600000000,
  "retention_us": 2592000000000,
  "purge_period_us": 86400000000,
  "purge_offset_us": 0,
  "last_sync_us": 1792950000000000,
  "registry": { "enabled": true },   // false when the deployment brings its own ids
  "tables": [ { "name": "device",
                "kind": "keyed",
                "key": { "strategy": "global_id",
                         "key_columns": ["id"] },
                "columns": [ { "name": "payload", "type": "text",
                               "replicated": true,
                               "visibility": "public" },
                             { "name": "secret", "type": "blob",
                               "replicated": true,
                               "visibility": "restricted" } ],
                "write_authority": "any" },
              { "name": "sites",
                "kind": "keyed",
                "key": { "strategy": "global_id",
                         "key_columns": ["site_id"] },
                "columns": [ { "name": "uid_start", "type": "integer",
                               "replicated": true,
                               "visibility": "public" },
                             { "name": "uid_end", "type": "integer",
                               "replicated": true,
                               "visibility": "public" } ],
                "write_authority": "master_only" } ],
  "schema_hash": "…",
  "visibility_hash": "…",
  "codec_version": 1,
  "seed_verifier": "…",       // a verifier over the digest seed — never the seed
  "credential": "…",           // the credential the peer presents for its role
  "directory": [ { "node_id": "…", "role": "master" },
                 { "node_id": "…", "role": "child",
                   "parent_id": "…" } ]
}
```

The `sites` registry is listed like any other table, but it is **conditional**: it is
exchanged first in every session **when the deployment uses the crate's allocator**,
and a node that has never synced receives it as part of the join bundle rather than by
syncing. When `registry.enabled` is false there is no registry, no `site` column, and
no REGISTRY phase. See § "REGISTRY".

### The projection

A session runs under exactly one **visibility profile** — a named set of tables and
columns ([data-model.md](data-model.md) § "Visibility profiles"). The profile is
resolved during the handshake, and the resolved **projection** is sent to the peer:

```jsonc
"projection": [ { "table": "device",
                   "columns": [ { "name": "payload", "type": "text" } ] } ]
```

The projection's **order is the row array's order** — see § "Message encoding" — and it
carries each column's type, so a projection is self-sufficient for decoding a row.

For a trusted peer the projection is the whole registered schema and the two sides
compare `visibility_hash`. For a client session the server sends the projection for the
client's profile and the client applies it — a client never sees a column outside its
profile, and it never sees a table the profile excludes.

### The directory in the handshake

Every master sends the UUID directory it holds, and every master takes the **union**. That
is how provisioning a child makes it known to the whole mesh without an operator editing
each config file.

**A child never receives the directory.** It sends only its own entry, and learns only
its upstream master's UUID. A child that learned another child's UUID could present
`(that UUID, the child string)` and be accepted as that child — the child string is
shared by all children, so the UUID is the only thing distinguishing them. The directory
is the child security boundary, and the handshake is where it is withheld.

A client sends no directory and receives none.

### The seed verifier

**The digest seed is never sent.** It is a secret, and a peer that has it can compute
every digest. But the handshake has to be able to detect that two peers hold
*different* seeds, because different seeds mean different digests for the same row and
the trees will never converge.

The handshake carries a **verifier** instead: a keyed derivation of the seed that two
holders of the same seed compute identically and that does not disclose the seed. Two
verifiers that differ mean two seeds that differ, and the session is refused with
`DIGEST_SEED_MISMATCH`. A verifier that *matches* proves nothing on its own — it is a
consistency check, not an authentication — which is why it is only ever compared
between **trusted peers**, and why a client is never asked for one.

**The credential is presented in the handshake**, and it is the role string: the master
string, or the child string. A child holds only the child string, and a master holds
both. It is checked against the role the peer claims, and a peer that cannot verify for
its claimed role is refused with `BAD_CREDENTIAL`.

**The authenticated identity is not on the wire.** It comes from the transport — the
application tells the crate who it authenticated — and the crate binds it to the
`node_id` the peer claimed. A peer whose claimed `node_id` is not the identity the
transport authenticated is refused with `IDENTITY_MISMATCH`. See
[ADR-0020](adr/0020-application-authenticated-peer-identity-and-optional-per-node-credentials.md).

### Validation

| Check | Failure |
|---|---|
| `epoch_us` differs | `EPOCH_MISMATCH` — fatal; the trees are not comparable |
| `protocol` differs | `PROTOCOL_UNSUPPORTED` |
| `codec_version` differs | `CODEC_MISMATCH` — fatal; the encoding changed, so every digest changed |
| `seed_verifier` differs (trusted peers) | `DIGEST_SEED_MISMATCH` — fatal; every row hash differs. The seed itself is never sent — see § "The seed verifier" |
| `schema_hash` differs (trusted peers) | `SCHEMA_MISMATCH` — fatal; the digest inputs differ |
| `visibility_hash` differs (trusted peers) | `VISIBILITY_MISMATCH` — fatal; the profiles differ, so the peers project the data differently |
| `node_id` not in the directory | `UNKNOWN_PEER` |
| `node_id` differs from the application's authenticated identity | `IDENTITY_MISMATCH` — fatal; the peer is not who the transport says it is |
| credential does not verify for the claimed role | `BAD_CREDENTIAL` |
| **clock skew** beyond `max_skew` | `CLOCK_SKEW` — **the peer is refused**; the two nodes cannot agree on what time it is, and every timestamp they exchange would be suspect |
| `as_of_us` differs by more than `as_of_tolerance` | **not a failure**: the server's wins; the client adopts it and reports that it did. This is the session's *comparison depth*, not a clock reading — the two sides are choosing where to compare, not disagreeing about when it is |
| `last_sync_us` — peer is older than the staleness horizon | `STALE_PEER` — the peer must re-seed, and the session is **pull-only** (see [lifecycle.md](lifecycle.md)) |
| `retention_us`, `purge_period_us` or `purge_offset_us` differs | `PURGE_SCHEDULE_MISMATCH` — fatal; the peers would purge different sets at different moments |
| `last_purge_instant` — peer is newer than ours | **not a failure**: the local node catches up by purging locally, then syncs |
| `last_purge_instant` — peer is older than ours | **not a failure**: the peer catches up on its side; the local node says so in the handshake |
| peer requests a column outside its profile | `COLUMN_NOT_PERMITTED` — **the request is refused, not the session.** The column is dropped from the projection and the request is logged; the session continues |
| unknown `tables`/`columns`, **client** session | sync the intersection, log a warning |
| unknown `tables`/`columns`, **trusted peer** | `SCHEMA_MISMATCH` — fatal |

The last two rows are not in conflict, and the distinction matters. A read-only client
legitimately sees a narrower schema — only the profile's projection — so the
intersection is the right behaviour. A **trusted peer** that disagrees about the
replicated column set would compute different digests for the same logical row, and the
trees would never converge. That is not a warning; it is fatal. The `schema_hash` is
what makes the difference detectable.

**`COLUMN_NOT_PERMITTED` is not fatal.** A peer asking for a column it is not allowed
to see is a mistake, not an attack: the projection is the server's to decide, and the
request is refused by dropping the column and logging it. The session continues, because
there is a well-defined projection to carry on with. `EPOCH_MISMATCH`,
`SCHEMA_MISMATCH`, `VISIBILITY_MISMATCH`, `DIGEST_SEED_MISMATCH` and
`PURGE_SCHEDULE_MISMATCH` are fatal precisely because there is **no** well-defined way
to continue: the two sides would compute different digests for the same row, or purge
different sets.

**Two different tolerances, and they are not the same knob.** `max_skew` is about the
*clock*: if two nodes disagree about what time it is by more than the tolerance, the
peer is refused with `CLOCK_SKEW`, because every timestamp either side stamps would be
suspect. `as_of_tolerance` is about the *comparison depth*: it decides how deep the two
sides compare, and the server simply wins. Adopting the server's depth is correct and
costs nothing; refusing a peer whose clock is off is necessary, because there is no
negotiation that fixes it.

`as_of_us` is the single timestamp driving the comparison-depth policy and the "what
changed since" computation. In a client→server session the server supplies it, so both
sides choose identical depths.

### Two different failure modes, two different meanings

**Missing a purge is not staleness.** A peer whose `last_purge_instant` is behind ours
has missed one or more purge instants; it catches up by purging locally, and the
session proceeds. This is a routine, recoverable condition.

**Being offline beyond the retention window is staleness.** A peer whose
`last_sync_us` is older than `now - retention` may still hold a live row whose
tombstone has been purged, and nothing can order those two rows. It must re-seed. The
check is mesh-wide, not per-link — see [lifecycle.md](lifecycle.md) § "The staleness
rule".

**Staleness is symmetric.** The rule refuses a stale peer as a *destination*, and also
as a *source*: a session with a stale peer is pull-only, and the stale peer's pushes
are refused with `STALE_PEER`. A peer that was away longer than the tombstone evidence
lasts cannot tell a row that was deleted and purged while it was away from one that was
merely old, so its rows are not accepted.

### The authenticated identity

The application authenticates the peer — with TLS, mTLS, a JWT, whatever it already
uses — and tells the crate who it is:

```rust
SessionContext {
    authenticated_peer:   Some(node_id),   // the identity the transport established
    authenticated_subject: Some("spiffe://…"),
}
```

When `authenticated_peer` is set, the `node_id` in the HELLO **must** equal it, or the
session is refused with `IDENTITY_MISMATCH`. This closes the gap that role-level
credentials leave open: without it, any master could claim any other master's UUID.
The crate does not authenticate anybody — it binds the identity the application already
established to the node id the peer claims. See
[security.md](security.md) § "The authenticated identity".

## Session phases

```
1. HELLO     client → server
2. REGISTRY  exchange the site registry        (only when the allocator is in use)
3. SUMMARY   exchange bucket hashes for a range
4. DETAIL    exchange child hashes for the buckets that differ
5. ROWS      exchange the differing rows, in pages
6. COMMIT    advance the cursor
```

### REGISTRY

The site registry is exchanged **before** any data, and applied before any data is
accepted — **when the deployment uses the crate's allocator**.

The reason is the range check. A node cannot validate an identifier it has no range for,
so the registry must be current before a row arrives. It is also the reason the registry is
part of the join bundle: a node that has never synced has no ranges at all, and cannot
decide whether a row's identifier belongs to a site it has not heard of.

The phase is a normal table sync — the same SUMMARY/DETAIL/ROWS machinery, over the
`sites` table — run first, and it runs at the start of every session, even when the
registry itself has not changed. A session that omits it would accept rows whose owners
it does not know, which the range trigger defers rather than rejects.

**The phase is conditional.** A deployment that brings its own identifier scheme sets
`registry.enabled = false` in the handshake; there is then no registry table, no `site`
column, no range check, and no REGISTRY phase. The allocation machinery is an optional
feature of the crate, not a precondition for syncing. See
[adr/0019](adr/0019-the-allocation-registry-is-optional.md).

For a site created *during* a session, the registry row may reach the peer after the
site's data rows do. The range trigger fires only when the registry knows the row's
owner, so such a row is accepted provisionally and re-validated when the registry catches
up.

### SUMMARY

```jsonc
// request
{ "session_id": "…", "tbl": "device",
  "from_us": …, "to_us": …, "level": "hour|cycle|month",
  "hashes": [ { "bucket_id": 29681, "level": "hour",
                "hash": "…", "count": 42 } ] }

// response
{ "session_id": "…",
  "hashes": [ { "bucket_id": 29681, "level": "hour",
                "hash": "…", "count": 42 } ],
  "differing": [ { "bucket_id": 29681, "level": "hour" } ],
  "total_buckets": 8766 }
```

Each side sends **the hashes it holds for the range**, and each computes the symmetric
difference from the two hash lists. The differing set is symmetric, so both sides learn
the same set; the `differing` list in the response is the responder's own computation,
carried as a cross-check.

A hash list is ordered by `(level, bucket_id)`, so the two sides can be compared without
a sort at the receiver, and a truncated list (see § "Paging and message budgets") is
always a prefix of the full one.

`total_buckets` is the number of buckets in the range at the requested depth, exactly
computable from the time range and the depth. It is the progress denominator for the
comparison phase (§ "Progress").

### DETAIL

```jsonc
// request
{ "session_id": "…", "tbl": "device",
  "buckets": [ { "id": 1237, "level": "cycle" } ] }

// response
{ "children": [ { "id": 29681, "level": "hour", "hash": "…", "count": 42 } ] }
```

Only differing buckets are expanded, one level per round trip. The `count` is the row
count, which becomes the denominator for the transfer phase.

### ROWS

Where bandwidth is won or lost. Two modes:

**`mode = "digest"` (default).** For each differing hour, each side sends a compact list
of `(key, digest)`. From the two lists both sides compute the exact differing key set,
then exchange only those rows' contents.

**`mode = "row"` (simple, more bytes).** Each side sends all rows for the differing
hours; the peer merges by LWW. Simpler and idempotent, but re-transmits unchanged rows in
a busy hour.

Both modes are idempotent and safe to retry.

Deleted rows are transmitted with their full payload and their `status`/`purge_on`, in
the same bucket as everything else — no separate tombstone stream, no extra round trip.

Every ROWS batch carries the **origin** of the rows it contains:

```jsonc
{ "session_id": "…", "tbl": "device", "mode": "digest",
  "origin": { "node_id": "…", "role": "child", "site": "eu-west" },
  "cursor": { "tbl": "device", "bucket_id": 29681, "level": "hour",
              "last_key": "…", "as_of_us": … },
  "rows": [ [ "<base64 key>", "<base64 digest>" ], … ] }
```

and in `row` mode, the values follow the digest:

```jsonc
{ "session_id": "…", "tbl": "device", "mode": "row",
  "origin": { "node_id": "…", "role": "child", "site": "eu-west" },
  "rows": [ [ "<base64 key>", "<base64 digest>",
              [ "…", 1735689600000000, { "$b": "…" } ] ], … ] }
```

The values are positional, in the projection's order for that table — see
§ "Message encoding". The origin is **the node that wrote the rows**, not the node
sending them. A master's rows travel master → master → child, and the child's write
authority must still be evaluated against the master that wrote them. See
[data-model.md](data-model.md) § "Write authority".

The cursor in a request is the same shape as the `next_cursor` a response returns. It
carries the `as_of_us` it was issued at, because a cursor from a different comparison
depth is not usable — the session would be comparing against a different snapshot.

### Paging and message budgets

**A ROWS message is bounded.** The sender stops adding rows when the encoded message
would exceed `max_message_bytes` (default 4 MiB) and returns a cursor instead:

```jsonc
// response
{ "session_id": "…",
  "origin": { "node_id": "…", "role": "child", "site": "eu-west" },
  "rows": [ … ],
  "next_cursor": { "tbl": "device", "bucket_id": 29681, "level": "hour",
                   "last_key": "…", "as_of_us": … },
  "done": false }
```

The response carries the origin too, because the response is where the rows are: a
receiver evaluates write authority against the rows it is about to apply, and it cannot
do that without knowing who wrote them.

The cursor is **deterministic and resumable**: it names the bucket, the last key sent
within it, and the `as_of_us` the session is running at. Rows within a bucket are
ordered by their canonical key bytes, so both sides agree on the order without
exchanging it, and a re-issued ROWS with the same cursor continues exactly where the
last page stopped. Re-sending a page is harmless — the merge is idempotent.

The same machinery bounds the SUMMARY hash list: a range with more buckets than fit in
one message is truncated, and the truncated list is always a **prefix** of the full one,
ordered by `(level, bucket_id)`.

**A session is bounded too.** After `max_rounds_per_session` (default 1000) round trips
the session ends with `SESSION_LIMIT` — a **retryable** outcome, not a failure. The
caller re-opens the session with the last cursor and continues. This is what stops a
pathological peer from holding a session open indefinitely, and what gives the transport
a natural place to yield.

### COMMIT

```jsonc
{ "session_id": "…" }
→ { "as_of_us": …, "synced_us": … }
```

Writes the peer cursor in `_sync_cursors` and, on the server side, triggers a snapshot
publication.

## Applying a row

The merge rule is LWW on `(last_update, digest)`:

```
apply(incoming, local):
    if incoming.last_update >  local.last_update  -> take incoming
    if incoming.last_update <  local.last_update  -> keep local
    if equal:
        if incoming.digest > local.digest          -> take incoming
        else                                       -> keep local
```

The tie-break fires on an **exact** `last_update` collision. That is not a theoretical
edge case: our timestamps are microsecond-resolution, but the clock underneath may be
coarser, and two nodes genuinely writing at the same instant is routine. When it
happens, the two nodes wrote **different data** — this is a real conflict, not a
duplicate write.

The digest covers the payload, so the two rows have different digests, and the comparison
resolves them. The digest is 16 bytes, big-endian on the wire, and compared as an
**unsigned 128-bit integer**:

```
incoming.digest > local.digest   →   incoming_u128 > local_u128
```

Comparing the raw bytes in memory order would let two peers disagree on a tie, and each
would keep a different row — permanent divergence, invisible until it caused one.

Both peers hold the same digest for the same row, so they reach the same conclusion.

Because the digest is keyed, only trusted peers can compute it. A client never performs
the tie-break — it applies what the server sends.

### Local columns are kept

When taking the incoming row, columns marked `local` are **not** taken. The remote's value
for a local sequence number belongs to a different sequence and is meaningless here:

```
apply(incoming, local):
    winner = the row chosen by the LWW rule above
    result = winner, but with every `local` column taken from `local`
```

This is what keeps a merged row's local `userid` intact when a remote copy arrives, and
it is why local columns are excluded from the digest — see
[data-model.md](data-model.md) § "Local columns".

### Set-valued columns

A column whose merge mode is `SetUnion` is merged as a union rather than replaced. The
default is whole-value replacement, under which a concurrent edit to a JSON array loses
one side's change. See [data-model.md](data-model.md) § "Set-valued columns".

### Per-table merge policies

The rule above is the **default**, and it is the default for a reason: LWW on
`(last_update, digest)` is a total order, and a total order is what makes two peers
converge on the same row.

Some schemas need a different rule — first-allocation-wins on a lease table, an
epoch-ordered ownership field re-asserted after LWW, a batch repair before apply to
satisfy a partial unique index. Those are expressed as a **per-table merge policy**:

```rust
pub trait MergePolicyFn: Send + Sync {
    fn merge(&self, ctx: &MergeContext<'_>, incoming: &IncomingRow,
             local: Option<&StoredRow>) -> MergeDecision;

    /// Optional sender-side batch repair, before the rows are replicated.
    fn prepare(&self, ctx: &MergeContext<'_>, batch: &mut Vec<Row>) -> Result<()>;
}
```

Two rules keep a custom policy from breaking convergence, and both are requirements
rather than advice:

1. **The policy must be pure and convergent.** The same `(incoming, local)` pair must
   produce the same decision on every peer. It may not consult the wall clock, a
   node-local counter, or anything else that differs between peers. A policy that
   decides "the newer wall-clock wins" is the default rule; a policy that decides "the
   node that has seen the row more times wins" is a divergence generator.
2. **Batch repair runs on the sender.** A repair applied on the receiving side changes
   the stored bytes, and therefore the digest — so the two peers would compute different
   digests for the same logical row and never converge. Repair that is applied on
   receipt is only safe if it is a deterministic function of the replicated row set
   that every peer computes identically, which in practice means doing it before the
   rows are sent.

The digest tie-break remains the fallback: a policy that returns `Undecided` falls
through to LWW on `(last_update, digest)`. The fallback covers a policy that **cannot
decide**; it does not cover one that decides wrongly. A non-convergent policy returns a
definite decision, so the tie-break never fires and the crate cannot detect it.

See [data-model.md](data-model.md) § "Merge policies" and
[adr/0015](adr/0015-per-table-merge-policies-and-write-authority.md).

### Write authority

Not every table is writable by every role. A master-authoritative policy table accepts
writes from masters and refuses them from children; a site-owned table accepts them
only from its owner.

The check is made **when the row is applied**, against the **origin** of the row — the
node that wrote it, which the ROWS batch carries — and not against the node that
happened to forward it. A row whose origin lacks authority for the table is refused
with `WRITE_NOT_PERMITTED`, and the table is dropped from the session.

A peer may write a table only if its profile exposes every replicated column of that
table: a peer that cannot see a column cannot compute the row's digest, and a row it
cannot digest is a row it must not write.

A row is applied to the user table and to `_sync_log` in one transaction, and the tree is
updated incrementally.

## The purge quiesce window

Around each purge instant, a node refuses sessions that touch the tables it is about to
purge:

```
503 PURGE_IN_PROGRESS
{ "tables": ["device"], "until_us": … }
```

**The window is per table, not global.** Before the instant, the node computes the set
of tables with tombstones due:

```
due = SELECT DISTINCT tbl FROM _sync_log
      WHERE purge_on_us IS NOT NULL AND purge_on_us <= T
```

In steady state this selects exactly the rows with `purge_on_us = T`, because the
earlier deadlines were purged at their own instants. The `<=` is what lets a node that
**missed** an instant catch up: its backlog counts as due, and is purged rather than
orphaned at a deadline that will never come round again. The same predicate is used for
the purge itself.

- If `due` is empty, the purge is a no-op and the node **does not quiesce at all**.
- If `due` is not empty, only those tables refuse sync; a session about another table
  proceeds normally.

That is the difference between a daily fleet-wide sync outage and a daily outage of the
one table that happens to have tombstones due. The `_sync_log.purge_on_us` column exists
so this check is an indexed existence query rather than a scan of the user tables.

A session refused for a quiescing table may retry after the window closes; nothing is
lost.

## Progress

Every phase reports **X of Y**, with `Y` always known before the phase starts. That is
what makes the number meaningful rather than a spinner.

```rust
pub enum Phase {
    Comparing { range_from: i64, range_to: i64 },
    Expanding,
    Transferring,
    Applying,
    Done,
    Failed,
}

pub enum Unit { Buckets, Rows }

pub struct SyncProgress {
    pub session_id:  Uuid,
    pub peer:        Uuid,
    pub phase:       Phase,
    pub completed:   u64,
    pub total:       u64,           // always known
    pub unit:        Unit,
    pub bytes:       u64,
    pub started_at:  Instant,
    pub updated_at:  Instant,
}
```

| Phase | Unit | Y is |
|---|---|---|
| Comparing | buckets | total buckets in the requested range |
| Expanding | buckets | differing buckets from the comparison phase |
| Transferring | rows | sum of row counts over the differing leaf buckets |
| Applying | rows | rows received |

The denominators are known beforehand: the bucket count in a range is computable from the
epoch and the range; the differing-bucket count comes from the comparison phase; the row
count comes from the bucket metadata carried in the DETAIL response.

### No ETA

There is deliberately no `eta_seconds` in the protocol. An estimate computed from a
moving denominator is misleading, and a misleading ETA is worse than none: it makes
people wait when they should give up.

What is exact is the **rate** and the **byte count**. A consumer that wants an estimate
can derive one from `completed` and `elapsed` and present it as approximate — a display
concern, not a protocol one. See [ADR-0009](adr/0009-progress-has-no-eta.md).

### Where progress is surfaced

```
GET /v1/sync/sessions/{id}/progress    → SyncProgress for one session
GET /v1/sync/progress                  → every active session, grouped by peer
GET /v1/sync/peers/{peer}/progress     → this node's view of one peer
```

Over WebSocket, the server pushes a progress frame when the value changes materially —
at most once per 250 ms, and only when `completed` advances:

```jsonc
{ "type": "progress", "session_id": "…", "phase": "transferring",
  "completed": 3800, "total": 12400, "unit": "rows",
  "bytes": 4194304 }
```

### Bulk seeding

A peer that has fallen out of the retention window must be re-seeded wholesale. That is
the case a human actually waits on, so it gets its own progress path: "row N of M", where
M is the table's row count from `SELECT count(*)` at the start.

A re-seed is a **wipe** — the stale peer discards its synced state and pulls the peer's
dataset. It does not push: its unsynced writes are discarded, because a peer that was
away longer than the tombstone evidence lasts cannot tell a row that was deleted and
purged while it was away from one that was merely old. See
[lifecycle.md](lifecycle.md) § "What a re-seed does".

### Persistence

Per-peer progress is written to `_sync_cursors.progress`, so a restarted node can show
"last synced with master B: 62% (interrupted)" rather than nothing at all.

## Idempotency and overlap

- Sync is **idempotent**. Applying the same row twice is a no-op under LWW.
- Overlapping sessions between the same pair are fine. Cursors advance monotonically.
- A session is resumable by re-issuing SUMMARY with the same range.
- A page is resumable by re-issuing ROWS with the cursor the last page returned.
- A session that fails mid-ROWS leaves the cursor at its previous value; the next session
  redoes the work. There is no partial-commit state.

## Durable client sessions

A read-only client on an unreliable link needs more than an in-memory session with a
five-minute TTL: it needs to survive a restart and pick up where it left off.

The client persists its session state in a local, **node-local** table:

```sql
CREATE TABLE _sync_session (
    session_id   TEXT PRIMARY KEY,
    peer_id      TEXT NOT NULL,
    profile      TEXT NOT NULL,
    state        TEXT NOT NULL,     -- active | paused | complete
    cursor       BLOB,              -- the last applied page cursor
    as_of_us     INTEGER,
    created_us   INTEGER NOT NULL,
    updated_us   INTEGER NOT NULL
);
```

Received pages are staged before they are applied, so a crash between "received" and
"applied" loses nothing: the staged rows are applied on restart, and the cursor advances
only after the transaction commits.

A session with a persisted cursor **survives a restart** — the TTL applies to *idle*
sessions, not to ones with work outstanding. A restarted client re-opens the session by
id and asks for the next page.

### Ordering

A client that is catching up after a long outage wants the important tables first:

```rust
pub struct TableSpec {
    …
    /// Lower numbers sync first. Core tables before large history tables.
    pub sync_priority: u8,
}
```

Tables are synced in ascending priority, and within a table the buckets are walked
**newest-first**, so the data a user is most likely to read lands in the database
before the history nobody is waiting for. The same ordering applies to the server's
proactive push in a pull-only session.

## WebSocket

```
GET /v1/sync/ws   (upgrade)
```

- The server pushes `{"type":"changed","buckets":[…]}` when its snapshot advances. A
  pull-only client uses this as a trigger to open a session.
- Progress frames flow on the same socket.
- The connection *is* the session; `session_id` is allowed but not required.
- Heartbeat every 30 s; drop the connection after two missed heartbeats.

## REST

```
POST /v1/sync/hello
POST /v1/sync/summary
POST /v1/sync/detail
POST /v1/sync/rows
POST /v1/sync/commit
```

Sessions are addressed by an opaque `session_id`, held in memory with a 5-minute TTL. A
single `POST /v1/sync` with a `step` discriminator is an acceptable simplification if the
route count matters more than clarity.

A client that needs to survive a restart keeps its session in `_sync_session` and re-opens
it by id — see § "Durable client sessions".

## Pull-only clients

A client that only ever receives can use a simplified flow: `HELLO` → the server sends
the full diff proactively → the client applies → `COMMIT`. The client does not compute
symmetric differences, and the server drives the progress report directly.

The proactive flow uses the **same page machinery** as the normal one: the server sends
one bounded page at a time, with a cursor, and the client acknowledges each page before
the next is sent. A pull-only client therefore has the same byte budget, the same
resumability, and the same table ordering as a bidirectional peer — which matters,
because a full diff into one hour of data is exactly the case that used to produce one
enormous message.

## Error codes

Every failure the protocol can report, in one place. The wire carries the code as a
stable string; the numeric value is an implementation detail.

### Fatal — the session cannot proceed

| Code | Meaning | Typical cause |
|---|---|---|
| `PROTOCOL_UNSUPPORTED` | The peer speaks a different protocol version | Version skew |
| `CODEC_MISMATCH` | The canonical encoding differs | A crate upgrade on one side |
| `EPOCH_MISMATCH` | The bucket epoch differs | The deployment was provisioned inconsistently |
| `DIGEST_SEED_MISMATCH` | The digest seed differs | The wrong seed, or a rotation that did not reach everyone |
| `SCHEMA_MISMATCH` | The digest inputs differ | A column was added, removed or reordered on one side |
| `VISIBILITY_MISMATCH` | The visibility profiles differ | The two sides project the data differently |
| `UNKNOWN_PEER` | The presented UUID is not in the directory | Not provisioned, or revoked |
| `IDENTITY_MISMATCH` | The claimed UUID is not the identity the transport authenticated | A peer claiming another node's UUID |
| `BAD_CREDENTIAL` | The credential does not verify for the claimed role | Wrong secret, or a rotated one |
| `STALE_PEER` | The peer is older than the staleness horizon | The peer has been offline longer than the retention window and must re-seed; the session is pull-only in both directions |
| `PURGE_SCHEDULE_MISMATCH` | The purge schedule differs — `retention_us`, `purge_period_us` or `purge_offset_us` | The deployment was provisioned inconsistently; the peers would purge different sets |
| `CLOCK_SKEW` | The peer's clock is more than `max_skew` away | One node's clock drifted, or NTP is not running; every timestamp either side stamps would be suspect |

Every one of these is a **correctness** failure, not a policy difference: continuing
would mean comparing trees that cannot be compared, or accepting writes from a node that
should not be writing.

**`CLOCK_SKEW` is a different kind of failure from the rest of this table.** The others
say the two sides disagree about a *shared fact* — the epoch, the encoding, the schema.
There is no negotiation that fixes that; the deployment must be made consistent.
`CLOCK_SKEW` says the two sides disagree about *the time*, and the fix is operational:
get the clocks right. It is fatal for the session and not for the deployment, which is
why it is worth distinguishing from `EPOCH_MISMATCH` in an alert.

**Missing a purge instant is not in this table.** A peer whose `last_purge_instant` is
behind ours catches up by purging locally and the session continues. Only the retention
horizon produces `STALE_PEER`.

### Non-fatal — the session continues with a caveat

| Code | Meaning | Handling |
|---|---|---|
| `COLUMN_NOT_PERMITTED` | A peer asked for a column outside its profile | The column is dropped; the request is logged |
| `TABLE_NOT_PERMITTED` | A peer asked for a table its profile excludes | The table is dropped |
| `WRITE_NOT_PERMITTED` | A row's origin may not write that table | The row is refused; the table is dropped from the session |
| `ROW_PURGED` | A tombstone the receiver has already purged | The row is refused; the sender purges it at the next instant |
| `PURGE_IN_PROGRESS` | The tables being purged are quiescing | The session is refused for those tables; retry after the window |
| `SESSION_LIMIT` | The session hit `max_rounds_per_session` | **Retryable**: re-open with the last cursor and continue |
| `UNKNOWN_TABLE` | A peer named a table this node does not sync | Client sessions: sync the intersection. Trusted peers: `SCHEMA_MISMATCH` |
| `UNKNOWN_COLUMN` | A peer named a column this node does not have | As above |
| `RANGE_VIOLATION` | A row's identifier is outside its owner's range | The row is refused; the write is logged |
| `KEY_CONFLICT` | Two rows in one batch carry the same key | The batch is refused; it is a bug in the sender |
| `PROGRESS_STALE` | A progress report is older than the cursor | Ignored |

### Local — the crate's own failures

| Code | Meaning |
|---|---|
| `IDENTIFIER_REUSED` | The allocator handed out an identifier it had already used |
| `OVERLAPPING_RANGES` | A site's range overlaps another site's |
| `RANGE_IMMUTABLE` | An attempt to move a site's range |
| `DIGEST_MISMATCH` | A stored digest does not match a recomputed one — a raw write bypassed the trigger |

## Related documents

- [merkle.md](merkle.md) — what is being compared
- [lifecycle.md](lifecycle.md) — the staleness rule and the purge handshake
- [data-model.md](data-model.md) — merge policies, write authority and visibility profiles
- [security.md](security.md) — handshake validation as part of the trust model
- [architecture.md](architecture.md) — which crate owns which part of this protocol
- [adr/0009](adr/0009-progress-has-no-eta.md) ·
  [adr/0015](adr/0015-per-table-merge-policies-and-write-authority.md) ·
  [adr/0018](adr/0018-bounded-messages-paged-transfer-and-durable-client-sessions.md) ·
  [adr/0023](adr/0023-json-wire-format-and-transport-compression.md) ·
  [adr/0024](adr/0024-the-workspace-is-split-into-six-crates.md)
