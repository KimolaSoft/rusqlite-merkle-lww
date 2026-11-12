# Implementation plan

The work is split **by crate**, not by phase. There is one plan per crate, and each
one is narrow enough that the whole of it can be held in mind at once — which is the
point, because the phases ran long and the focus drifted.

| Plan | Crate | Depends on |
|---|---|---|
| [model.md](plan/model.md) | `merkle-lww-model` | `serde` |
| [protocol.md](plan/protocol.md) | `merkle-lww-protocol` | `model` |
| [core.md](plan/core.md) | `merkle-lww-core` | `model`, `protocol`, `rusqlite` |
| [axum.md](plan/axum.md) | `merkle-lww-axum` | `core`, `protocol`, `model`, `tokio` |
| [cli.md](plan/cli.md) | `merkle-lww-cli` | `axum`, `core` |
| [demo.md](plan/demo.md) | `merkle-lww-demo` | `core`, `protocol`, `model` |

This file holds what does not belong to any one crate: the dependency order, the
cross-cutting work, the open questions and the risks.

See [ADR-0024](doc/adr/0024-the-workspace-is-split-into-six-crates.md) for why the
workspace is split this way, and
[ADR-0023](doc/adr/0023-json-wire-format-and-transport-compression.md) for the wire
format.

## Principles

- **Enforce in SQLite, not in the API.** The invariants live in the database, so a
  write that bypasses the crate's API still cannot break them.
  ([ADR-0002](doc/adr/0002-enforce-in-sqlite-not-in-the-api.md))
- **The crate owns the exchange; the application owns the transport.**
  ([ADR-0011](doc/adr/0011-crate-provides-the-exchange.md))
- **The digest is always computed and keyed.**
  ([ADR-0004](doc/adr/0004-digest-is-always-computed-and-keyed.md))
- **Bucket boundaries are absolute and epoch-anchored.**
  ([ADR-0003](doc/adr/0003-absolute-bucket-boundaries.md))
- **An ADR that turns out to be wrong is superseded, not edited.**

## Dependencies

The dependency edges point one way, and the build order follows them:

```
model ──► protocol ──► core ──► axum ──► cli
                    │         │
                    └─────────┴──────► demo
```

```
merkle-lww-model      the vocabulary: config, codec, digest, hashes, merge, time
      │
      ▼
merkle-lww-protocol   the wire contract: messages, JSON shapes, handshake rules
      │
      ▼
merkle-lww-core       the engine over SQLite: store, tree, diff, session, lifecycle
      │
      ├──────────────► merkle-lww-demo       the conformance harness
      ▼
merkle-lww-axum       the transport: REST, WebSocket, progress, compression
      │
      ▼
merkle-lww-cli        the binary
```

**The order the crates are built in is `model`, `protocol`, `core`, then `demo` and
`axum` in parallel, then `cli`.** `demo` needs `core` to be useful, but it is small
enough that its transport and clock can be written against the `protocol` traits while
`core` is still landing.

## The crate graph

| Crate | Holds | Why it is separate |
|---|---|---|
| `merkle-lww-model` | the vocabulary: config, the canonical encoding, the digest, the schema and visibility hashes, the merge rules, time, errors | Pure — no SQLite, no runtime. The compatibility surface is testable without a database and portable to another language; the extension point carries no engine weight |
| `merkle-lww-protocol` | the wire contract: the message types, the JSON shapes, the tagged value form, the handshake validation rules, the engine traits, the `Transport` trait, the wire version | The wire version and the codec version have different lifecycles. The rules are pure functions over a gathered `LocalView`, so every rejection case is a test with no fixture |
| `merkle-lww-core` | the engine: store, triggers, authorizer, log, tree, diff, bulk apply, session, paging, lifecycle, cache, registry, allocator, directory, the session journal | Everything that touches SQLite, in one crate — because they share `Db` in nearly every signature |
| `merkle-lww-axum` | the transport: REST, WebSocket, progress, compression | Where the async boundary is, and where `tokio` lives |
| `merkle-lww-cli` | the binary: `serve`, `pull`, `push`, `inspect`, `tree`, `verify` | A binary is a different kind of artifact |
| `merkle-lww-demo` | the in-memory transport, the scripted clock, the two-node fixture, the topology and scenario fixtures | Makes the interface provable and the failure modes deterministic. A crate, so the examples are compiled and run |

The `alloc` feature stays a **feature** of `merkle-lww-core`, not a crate: the allocator
needs to install triggers and read the registry table inside the same database as
everything else, and the feature already achieves what a boundary would.

## Cross-cutting work

### Documentation

The documents in [doc/](doc/) are written before implementation and updated as the
implementation reveals the design to be wrong. An ADR that turns out to be wrong is
superseded, not edited.

Each crate's plan has a documentation obligation in its exit criteria, because a
crate boundary is also a documentation boundary: a reader who wants to know how to
compute a digest should not have to read the transport's code to find out.

### Fixtures

| Fixture | Created in | Used by |
|---|---|---|
| `golden_vectors.json` | [model](plan/model.md) M2 | Suite 14 |
| `schema_hash_vectors.json` | [model](plan/model.md) M3 | the `schema_hash` checks |
| `visibility_hash_vectors.json` | [model](plan/model.md) M3 | the `visibility_hash` checks |
| `wire_vectors.json` | [protocol](plan/protocol.md) P1 | the wire contract, cross-implementation |
| `topologies.json` | [demo](plan/demo.md) D3 | Suite 1 |
| `scenarios.json` | [demo](plan/demo.md) D3 | Suites 5, 6 |

**The crate that defines a format publishes its vectors.** The digest, the schema hash
and the visibility hash are defined by `merkle-lww-model`; the wire is defined by
`merkle-lww-protocol`; the topologies and the lifecycle scenarios are the harness's,
and belong to `merkle-lww-demo`.

Every fixture is JSON, so it can be shared with an implementation in another language,
which is the point of specifying the encodings precisely.

### Tooling

- `cargo fmt`, `cargo clippy` with `-D warnings`.
- `cargo nextest` for the test runner, once the suite is large enough to warrant it.
- `cargo-insta` for snapshot tests, where a snapshot is the right tool. The published
  vectors are **not** insta snapshots: they are checked-in fixtures, and a change to one
  is a diff a reviewer sees rather than a snapshot that is regenerated on demand.
- CI on Linux and macOS, with the SQLite behaviour assertions run against both the
  bundled and the system SQLite where the system build is new enough.
- **`cargo publish --dry-run` for the published crates**, and a check that
  `merkle-lww-demo` is not published.

---

## Open questions

These are unresolved, and the ones marked **blocking** hold up a specific crate.

| # | Question | Impact | Blocking |
|---|---|---|---|
| 1 | **Default write dialect.** Enforced (UDF triggers) or compatible (pure-SQL staging)? | Determines whether the application can use `Db::conn()` at all | [core](plan/core.md) C1 |
| 2 | ~~Does the crate terminate TLS?~~ | **Resolved: no.** The crate has no TLS dependency. The application terminates TLS and handles authentication; the crate provides the exchange and the peer check. See [doc/implementation.md](doc/implementation.md) | — |
| 3 | ~~`GlobalId` or `SiteNamespaced` as the default key strategy?~~ | **Resolved: `GlobalId` default.** The sites share one dataset, so the same user is the same user. `SiteNamespaced` remains available per table for genuinely per-site data. See [ADR-0006](doc/adr/0006-site-namespaced-keys.md) | — |
| 4 | **How is the digest seed distributed?** Environment variable, mounted secret, or KMS. The crate holds it in memory and **never writes it to the database**; it implements no secret store and no rotation. See Q16 and [ADR-0020](doc/adr/0020-application-authenticated-peer-identity-and-optional-per-node-credentials.md) | Deployment; not implementation | — |
| 5 | ~~Wire format. JSON, or binary from the start?~~ | **Resolved: JSON**, wire version `merkle-lww/1`. Rows are positional arrays over the projection's column list. **Compression is not part of the protocol** — it is a transport-level content encoding, and `merkle-lww-axum` enables zstd level 1 by default. The wire format is outside `schema_hash` and outside the digest, so changing it re-digests nothing. See [ADR-0023](doc/adr/0023-json-wire-format-and-transport-compression.md) | — |
| 6 | ~~Is the purge period daily, or aligned to the retention window?~~ | **Resolved: daily.** The purge instant is every 24 h, at a deployment-wide offset within the period. The cadence only decides how soon a tombstone is collected; with a 30-day retention a tombstone lives 30–31 days. It does not interact with the purge floor, which applies to tombstones only. See [ADR-0022](doc/adr/0022-one-purge-instant-per-window.md) and [doc/lifecycle.md](doc/lifecycle.md) | — |
| 7 | **Does the compatible dialect's drainer compute digests for the whole table or only for staged rows?** Only staged rows, presumably, but a row written and deleted within one interval is lost — acceptable, since it was never visible to a peer. | Drain semantics | [core](plan/core.md) C1 |
| 8 | **Should `AppendOnly` tables exist?** They remove the delete machinery from the trigger set, at the cost of a second code path. | Scope of the trigger set | [core](plan/core.md) C1 |
| 9 | ~~Should the crate manage the `sync_id` column, or use the application's own key?~~ | **Resolved: the application's own key.** The application's `userid` is already globally unique and never reused, so it *is* the sync key and no extra column is needed. `Derived` covers applications whose key is local or mutable. See [ADR-0012](doc/adr/0012-sync-key-is-the-applications-key.md) | — |
| 10 | **How is the derivation namespace chosen?** A per-table constant from the table name and `epoch_us` keeps ids stable within a deployment but differs between deployments. | Id stability | [model](plan/model.md) M2 |
| 11 | ~~Should the crate own the id allocator?~~ | **Resolved: it may, and it is optional.** The crate offers the allocator and the range registry behind the `alloc` feature; a deployment that brings its own identifier scheme has neither. Owning it makes the "never reused" property a guarantee rather than a convention. See [ADR-0019](doc/adr/0019-the-allocation-registry-is-optional.md) | — |
| 12 | **Should the crate offer a membership-table helper?** Materialising a JSON array into a child table removes a recurring source of merge bugs. | API scope | [cli](plan/cli.md) X1 |
| 13 | ~~Who assigns the site ranges, and how?~~ | **Resolved: the deployment does, and the crate enforces locally.** Central assignment plus local enforcement; the crate cannot prevent two nodes from concurrently creating sites with overlapping ranges, because each check passes locally and the merge happens afterwards. Where the crate's own allocator is used, it is the allocator that assigns the ranges. It is a deployment invariant. See [ADR-0019](doc/adr/0019-the-allocation-registry-is-optional.md) | — |
| 14 | **Is the `site` column's owner id an opaque id or a readable slug?** A slug is nicer to read and awkward to keep immutable; an opaque id is the reverse. The application owns the `sites` schema, so this is the application's choice — but the crate must not assume a slug is stable. Only applies where the allocation scheme is in use. | Schema ownership | [core](plan/core.md) C1 |
| 15 | ~~How is the peer credential structured?~~ | **Resolved.** Master-issued UUIDs; a UUID directory propagated master-to-master; role-level master and child strings; the digest seed as a third, independently-rotated value. Per-node revocation is by removing a UUID from the directory. The application's authenticated identity is bound to the claimed node id, and per-node credentials and local attribution are available as options. See [ADR-0020](doc/adr/0020-application-authenticated-peer-identity-and-optional-per-node-credentials.md) | — |
| 16 | ~~Where does the digest seed live?~~ | **Resolved.** It is configuration, held in memory, and **never written to the database**. The crate implements no rotation. | — |
| 17 | ~~How do two peers merge a row when LWW is the wrong rule?~~ | **Resolved.** A per-table merge policy, with two requirements: it must be pure and convergent, and batch repair runs on the sender. See [ADR-0015](doc/adr/0015-per-table-merge-policies-and-write-authority.md) | — |
| 18 | ~~How is a row's write authority decided?~~ | **Resolved.** Per table, against the **origin** of the row — the node that wrote it — which the wire now carries. See [ADR-0015](doc/adr/0015-per-table-merge-policies-and-write-authority.md) | — |
| 19 | ~~How are visibility classes beyond trusted/client expressed?~~ | **Resolved.** Named visibility profiles mapped to roles, overridable per peer, hashed separately from the schema. See [ADR-0016](doc/adr/0016-visibility-profiles-mapped-to-peer-roles.md) | — |
| 20 | ~~What does a re-seed do with the stale peer's local writes?~~ | **Resolved: it discards them.** A re-seed is a **wipe** — the stale peer purges locally, discards its synced state, and pulls the peer's dataset. It never pushes: a stale peer cannot tell a row that was deleted and purged while it was away from one that was merely old, so offering them would resurrect them. This makes the resurrection guarantee unconditional. See [ADR-0021](doc/adr/0021-re-seed-is-a-wipe.md) | — |
| 21 | ~~How are large transfers bounded?~~ | **Resolved.** A byte budget per message, a round limit per session, resumable cursors, and a durable client session. See [ADR-0018](doc/adr/0018-bounded-messages-paged-transfer-and-durable-client-sessions.md) | — |
| 22 | ~~Must an application use the crate's allocator?~~ | **Resolved: no.** The registry, the `site` column, the range trigger and the REGISTRY phase are all optional. See [ADR-0019](doc/adr/0019-the-allocation-registry-is-optional.md) | — |
| 23 | ~~Can an application keep its own write path?~~ | **Resolved.** `Db::register_on(&Connection)` installs the UDFs and the authorizer on a caller-owned connection. See [doc/sqlite-interface.md](doc/sqlite-interface.md) | — |
| 24 | ~~Is the purge instant fixed, and is `purge_on` per row?~~ | **Resolved.** `purge_on` is computed from the delete's window — the interval between two consecutive purge instants — so every tombstone in a window shares one deadline, and the tombstone lives at least `retention`. The instant carries a deployment-wide `purge_offset_us`, so a deployment can choose when in the period the purge falls. See [ADR-0022](doc/adr/0022-one-purge-instant-per-window.md) | — |
| 25 | ~~What does a visibility profile do to the digest?~~ | **Resolved: nothing.** The digest's input is always the full replicated column set. A peer that cannot see a column receives the digest verbatim and never recomputes it, and the tree is unaffected because `row_hash` is a function of the digest. See [ADR-0016](doc/adr/0016-visibility-profiles-mapped-to-peer-roles.md) | — |
| 26 | **Is the JSON the public contract, or the Rust API?** The golden wire vectors imply the JSON is frozen and the Rust types are a serialisation of it. If the Rust types are the contract, the JSON can change freely and the vectors are only a convenience. The two readings give different answers to "may I add a field to `Hello`?" — and to whether a Rust consumer and a non-Rust consumer are held to the same shape. | The wire format's evolution policy | [protocol](plan/protocol.md) P1 |
| 27 | **Should `merkle-lww-model` be published separately, or re-exported through `merkle-lww-core`?** An application that configures the crate and implements a merge policy needs the types either way. Re-exporting keeps the dependency list at one crate; publishing separately makes the compatibility surface a first-class artifact that another language's implementation can be checked against. | The public API shape | [model](plan/model.md) M1 |

Question 1 changes the shape of the code. The rest can be settled during the crate they
affect.

### Question 1, elaborated — the write dialect

The question is not "does the application use `rusqlite`?" It is:

> **Does the application open connections the crate did not instrument?**

That determines whether the crate's write-time invariants can be enforced at all.

**How the invariant is enforced.** Every write to a managed table must (a) stamp
`last_update` and (b) update `_sync_log`. Doing that in Rust means the trigger calls a
Rust function — the digest is xxh3, which SQLite does not have.

- **Enforced.** The trigger calls `sync_stamp()` and `sync_digest()`, registered on the
  connection by the crate. The work happens inside the write transaction, so the log is
  never stale. On a connection without the functions registered, the write **fails** with
  `no such function: sync_stamp`.
- **Compatible.** The trigger stages the row into a table using pure SQL, and a Rust
  drainer computes the digest afterwards. Works on any connection. Costs up to one drain
  interval of latency, and millisecond rather than microsecond timestamp resolution.

**What it means in practice.**

| Application | Enforced | Compatible |
|---|---|---|
| Uses `rusqlite`, takes connections from `Db::conn()` | **yes, recommended** | works, with latency |
| Uses `rusqlite`, opens its own connections | no — needs the connection factory | works |
| Uses `diesel` | no — diesel opens its own connections | works |
| Uses `sqlx` | no | works |
| Uses the `sqlite3` CLI | no | works, but only for reads |

**The compatibility gap, verified.** The authorizer is per-connection, so in compatible
mode a foreign connection has no denial rules. It can write `_sync_log` and hard-delete
managed rows. Two pure-SQL guards close the important cases: a `BEFORE DELETE` trigger
converts a hard delete into a soft delete, and a guard trigger rejects writes to the
internal log. The remaining gap is that a foreign connection can write `status` or
`purge_on` directly. That is caught and propagated, but the tombstone deadline may be
inconsistent. See [doc/sqlite-interface.md](doc/sqlite-interface.md) § "Compatible mode and
foreign connections".

**Recommendation.** Default to **enforced**, and document that the application must use
`Db::conn()` or the crate's connection factory. The failure mode for a misconfigured
application is a loud error at the first write, not silent divergence. If the target
application genuinely cannot be changed, compatible mode is the fallback and the
documented gaps apply.

**The question to answer:** *can the application be changed to take its connections from
the crate?* If yes, enforced. If no, compatible, and the gap is accepted knowingly.

### Question 3, elaborated — the key strategy

> **When two sites create what should be the same logical entity, do they become one row
> or two?**

**Resolved: `GlobalId` as the default.** The sites share one dataset, so the same user
is the same user everywhere, and two sites updating that user should converge on one row.

The consequence that must be handled explicitly: **`GlobalId` only merges if both sites
derive the same id.** A randomly generated globally-unique id (UUIDv7, ULID, Snowflake)
does not merge anything — it merely avoids collisions. For "the same user is the same
user" to hold, the id must be:

- derived deterministically from a natural key — `uuidv5(namespace, "user:alice")`; or
- assigned by something upstream that already agrees across sites.

The crate should offer a `Derived` variant that computes a deterministic UUIDv5 from
the table's identity columns, so an application can opt into merging without changing
its schema. `Surrogate` remains available for tables that genuinely must not merge: it
is a random UUIDv7 and never merges across sites.

`SiteNamespaced` stays available per table for data that is genuinely per-site — where
merging two records would be actively harmful.

See [doc/data-model.md](doc/data-model.md) § "Key strategies" and
[ADR-0006](doc/adr/0006-site-namespaced-keys.md).

---

## Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| A SQLite version changes behaviour the design relies on | Low | The behaviour assertions are preserved as tests; they fail loudly rather than subtly |
| The digest seed leaks to a client | Low | The seed is never sent; a client cannot recompute a digest; Suite 10 asserts it |
| A deployment mixes enforced and compatible nodes | Medium | The two dialects produce comparable timestamps and identical digests, so mixing is safe; the drain latency is the only difference |
| The digest or encoding changes after data exists | Low | Both are versioned in `_sync_meta`; the crate refuses to start with a changed version unless told to re-digest |
| Clock skew beyond the documented `max_skew` (5 s) | Medium | A skew check at handshake; peers believed to be skewed are refused |
| The month tier's memory grows unexpectedly | Low | Bounded by activity, not age; `max_tree_buckets` is a tripwire |
| A bulk apply exceeds SQLite's 32,766 bound-parameter limit | Medium | The chunk size is derived from the column count, and the JSON path uses a single parameter regardless of batch size. A 50,000-row batch is covered by a test |
| The application generates random ids where deterministic ones were needed, so rows never merge | Medium | Documented prominently in the key-strategy section and ADR-0006; `verify` can report entities that exist at more than one site with the same natural key |
| A site uses `GlobalId` on data that is genuinely per-site, and two records merge | Low | `SiteNamespaced` is a per-table option; the failure is data loss, so the default should be reviewed when the deployment shape changes |
| A custom merge policy is not convergent, and two peers diverge silently | Medium | The requirement is documented as a hard rule and in ADR-0015; the convergence suite runs with a custom policy in the loop. **The crate cannot detect this** — it is the one extension point that can break the core guarantee. The `Undecided` fallback covers a policy that *cannot decide*, not one that decides wrongly: a non-convergent policy returns a definite decision, so the tie-break never fires |
| A re-seed discards a peer's unsynced local writes | Low | The re-seed is a wipe by design: a peer disconnected for longer than `retention` is not a valid instance, and the tombstone evidence cannot outlive that outage anyway. The loss is confined to a peer beyond the staleness horizon, and Suite 21 asserts the behaviour explicitly |
| A stale peer pushes a row that was deleted and purged while it was away | Low | Staleness is symmetric: a stale peer's pushes are refused with `STALE_PEER`, and it re-seeds by pulling. The purge floor remains the last line of defence for tombstones |
| A page cursor is stale or from a different `as_of_us`, and a transfer resumes at the wrong place | Low | The cursor is derived from deterministic ordering, re-sending a page is idempotent, and a mismatched cursor is rejected rather than applied |
| A visibility profile diverges between peers, and they project the data differently | Low | `visibility_hash` is compared at handshake and is fatal between trusted peers |
| A client restarts mid-transfer and restarts from the beginning | Low | The session journal is durable, pages are staged before they are applied, and the cursor advances only after the commit |
| A deployment sets `retention` shorter than its longest tolerable outage | Low | The staleness rule and the resurrection guarantee both depend on `retention` exceeding the disconnection window. The relationship is stated in `doc/lifecycle.md` § "The constant that makes it true" and in ADR-0021, and the operational requirement is part of the configuration documentation |
| **The crate boundaries turn out to be wrong, and code has to be moved across them** | Medium | The split was made before any code existed, which is the cheapest moment to make it. The boundaries are also documented with their rationale, so a boundary that turns out to be wrong can be argued against rather than guessed at |
| **A published crate's API is changed by accident** | Medium | Every type that crosses a crate boundary is published surface. Semver is enforced, the compatibility surface (`merkle-lww-model`) is versioned by `codec_version` independently of the crates' own versions, and the wire contract is versioned by `merkle-lww/1` |
| **A peer running an older wire version cannot be upgraded in step** | Low | The wire version is carried in the handshake and a mismatch is refused with `PROTOCOL_UNSUPPORTED`. The wire format is outside `schema_hash` and outside the digest, so a version bump re-digests nothing and needs no migration |
| **The wire format's JSON is larger than it needs to be** | Low | Measured: 14 % larger than a binary framing for row mode, 52 % for digest mode — but **1–4 % once the transport compresses**, which is why compression is the recommendation rather than a binary format. The positional row shape removes the largest source of overhead |
| **A transport compresses and its peer does not** | Low | The rule is written down: compression is a transport-level content encoding and both peers must agree. The failure is a parse error, not a silent misparse, and it is asserted in the axum plan |
