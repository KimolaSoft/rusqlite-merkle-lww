# Implementation guidance

This document is for **application developers** building on `rusqlite-merkle-lww`. It
covers how to wire the crate into an application, and — importantly — what the crate
expects the application to handle.

The other documents describe the design. This one describes the integration.

---

## The division of responsibility

The crate provides the **data exchange**. The application provides everything around it.

| Concern | Who |
|---|---|
| Diffing, merging, LWW resolution | **crate** |
| The wire message format | **crate** |
| Session state, progress accounting | **crate** |
| The `_sync_log` and the Merkle tree | **crate** |
| Stamping `last_update`, maintaining the log | **crate** (via triggers) |
| REST and WebSocket endpoints | crate provides them; application may use them |
| TLS termination | **application** |
| Peer authentication | **application** verifies; crate checks the credential it is given |
| Routing, middleware, session cookies | **application** |
| Which tables to sync, and how | **application** (via `TableSpec`) |
| Clock synchronisation | **application** (NTP/chrony) |

**The crate does not do TLS.** Terminate it wherever you already do — an ingress
controller, a service mesh, a reverse proxy, or `axum-server` with rustls in your own
binary. The crate has no TLS dependency and adding one would be a decision for you, not
for it.

---

## The recommended integration

### 1. Build the config

```rust
use merkle_lww_core::SyncConfig;

let config = SyncConfig {
    node_id:  issued_uuid,            // issued by a master, not generated here
    site:     "eu-west".into(),
    epoch_us: 1735689600000000,      // shared across the deployment
    role:     Role::Master,
    credential: Some(master_string),  // what this node presents
    digest_seed: digest_seed,         // shared by every node that computes digests
    ..Default::default()
};
```

Three secrets, and they are not interchangeable:

| Secret | Who has it | What it does |
|---|---|---|
| **master string** | every master | authenticates master ↔ master |
| **child string** | every child; every master | authenticates child ↔ master |
| **digest seed** | every master and every child | keys the hash — **not** a credential |

The digest seed must reach **children** as well as masters: a child writes rows, and every
write produces a digest. Clients get none of the three.

**None of them is written to the database.** The crate holds them in memory and
implements no rotation — that is your layer. See [security.md](security.md).

`epoch_us` must be identical across every trusted peer, and so must `digest_seed`. See
[config.md](config.md).

### 2. Describe your tables

```rust
let spec = TableSpec {
    name: "users".into(),
    kind: TableKind::Keyed,
    key: KeyStrategy::GlobalId {
        // Your own column, already globally unique and never reused.
        key_columns: vec!["userid".into()],
    },
    columns: vec![
        ColumnSpec { name: "userid".into(),  replicated: true, visibility: Visibility::Public,
                     local: false, merge: MergeMode::LastWriteWins },
        ColumnSpec { name: "site".into(),    replicated: true, visibility: Visibility::Public,
                     local: false, merge: MergeMode::LastWriteWins },
        ColumnSpec { name: "username".into(),replicated: true, visibility: Visibility::Public,
                     local: false, merge: MergeMode::LastWriteWins },
        ColumnSpec { name: "email".into(),   replicated: true, visibility: Visibility::Public,
                     local: false, merge: MergeMode::LastWriteWins },
        ColumnSpec { name: "secret".into(),  replicated: true, visibility: Visibility::Restricted,
                     local: false, merge: MergeMode::LastWriteWins },
    ],
    client_visible: true,
    allocation: Some(AllocationSpec {
        id_column:      "userid".into(),
        site_column:    "site".into(),
        registry_table: "sites".into(),
    }),
};
```

**If your key is already globally unique and stable, that is the sync key** — the crate
adds no key column, and `ON CONFLICT (userid)` is the conflict target.

That means `userid` must be globally unique across the whole deployment and never reused.
The usual way to get that is a per-site range:

```
site 1 → 10000 … 19999
site 2 → 20000 … 29999
```

**The ranges live in a `sites` registry table**, which you create in the same database as
your data:

```sql
CREATE TABLE sites (
    site_id     TEXT    NOT NULL,      -- immutable, globally unique — THE SYNC KEY
    site_name   TEXT    NOT NULL,      -- mutable display name
    uid_start   INTEGER NOT NULL,
    uid_end     INTEGER NOT NULL,
    city        TEXT,
    country     TEXT,
    last_update TEXT    NOT NULL,
    status      TEXT    NOT NULL,
    purge_on    TEXT,
    PRIMARY KEY (site_id),
    CHECK (uid_start <= uid_end)
);
```

It must be in the **same database** as your data — SQLite forbids a trigger from
referencing an object in an attached database, and the range check is a trigger.

**Your table needs a `site` column**, naming the site that owns the identifier. The crate
fills it; you never write it:

```sql
CREATE TABLE users (
    userid      INTEGER PRIMARY KEY,
    site        TEXT    NOT NULL DEFAULT (local_site()),
    username    TEXT    NOT NULL,
    email       TEXT    NOT NULL,
    last_update TEXT    NOT NULL DEFAULT (sync_stamp()),
    status      TEXT    NOT NULL DEFAULT 'live',
    purge_on    TEXT
);
```

Note `site` names the **owner** of the identifier, not the site that last wrote the row.
When another site edits one of your users, `site` stays yours and only `last_update`
moves.

> If the table already exists, `ALTER TABLE … ADD COLUMN` cannot use a non-constant
> default, so `DEFAULT (local_site())` is only available in `CREATE TABLE`. For an
> existing table, ask the crate for the `AFTER INSERT` backfill trigger and backfill the
> existing rows.

**If your key is only locally unique** — an integer sequence, or a mutable one like an
email — use `KeyStrategy::Derived` instead, and the crate manages a derived column.

**Mark local columns.** A column whose value is meaningful only on the site that wrote it
— a local counter, a site-specific flag — should be `local: true`. A remote write never
overwrites it, and it is excluded from the digest so that peers can still converge.

For a table whose rows are genuinely per-site, use `KeyStrategy::SiteNamespaced` instead.

### 3. Open the store

```rust
let db = Db::open("app.sqlite", config, vec![spec])?;
db.migrate()?;                 // create or adopt the managed tables
```

### 4. Use the connection

```rust
let conn = db.conn()?;                        // pooled, instrumented
conn.execute("INSERT INTO users(userid, username, email) VALUES (?1, ?2, ?3)",
             params![42, "alice", "alice@example.com"])?;
```

**This is the important part.** The connection must come from `Db::conn()`, or from a
connection built with the crate's factory. A connection you open yourself will not have
the triggers' functions registered, and writes to managed tables will fail with
`no such function: sync_stamp`.

That failure is deliberate — it is the enforced dialect working as designed. But it
means:

- **If you use `rusqlite` directly**, take connections from `Db::conn()`.
- **If you use `diesel`, `sqlx`, or the `sqlite3` CLI**, the enforced dialect cannot work,
  because those open their own connections. Use the compatible dialect, or keep those
  tools away from the managed tables.

### 5. Expose the exchange

If you want the crate's REST and WebSocket endpoints, mount them:

```rust
let app = Router::new()
    .merge(merkle_lww_axum::routes(&db))
    .layer(AuthenticationLayer::new(&peers))    // yours
    .layer(TlsLayer);                           // yours, or a proxy in front
```

If you already have a session layer, or want to expose sync differently, call the core
directly:

```rust
// The application owns the transport; the crate owns the exchange.
async fn handle(req: SyncRequest) -> SyncResponse {
    let report = db.perform_exchange(peer, transport).await?;
    SyncResponse::from(report)
}
```

The `transport` is anything that can carry request and response messages. The crate does
not care whether it is an HTTP body, a WebSocket frame, or a message queue.

---

## What to get right

### Use the crate's connection for managed tables

Covered above, but it is the single most common integration mistake.

### Let the crate stamp `last_update`

Do not write `last_update` yourself unless you are importing historical data. The
trigger stamps it, and an explicitly-supplied value is respected — so an accidental
write of `last_update` to a stale value will silently lose that row's later updates.

To import, write the value you want. To update, omit the column:

```sql
-- Good: the crate stamps this.
UPDATE users SET username = 'alice2' WHERE email = ?1;

-- Only when importing historical data, where the timestamp is the point:
UPDATE users SET username = 'alice2', last_update = '2024-01-01T00:00:00.000000Z'
 WHERE email = ?1;
```

### Do not write the managed columns

`status` and `purge_on` are crate-managed. Use `Db::delete()` for a delete:

```rust
db.delete("users", &key)?;       // sets status and purge_on
```

A raw `DELETE` is converted to a soft delete by a trigger, and a raw write to `status` is
rejected by the authorizer on instrumented connections.

If the crate manages a key column (`Derived`, `SiteNamespaced` or `Surrogate`),
writing it directly breaks the identity of the row — the crate derives it and would
overwrite your value on the next write.

**Do not write the `site` column either.** It names the site that *owns* the identifier,
and the engine fills it with `local_site()`. Writing it by hand is how a row ends up
claiming an owner whose range does not contain its identifier — the range trigger will
reject the write, which is the correct outcome, but it is easier not to create the
problem.

### Do not move a site's range

`uid_start` and `uid_end` are immutable. A site is renamed; its range is not moved.

Moving a range would invalidate every identifier already minted under it, and there is no
support in this design for re-allocating a site's identifiers. If a site needs a bigger
range, give it a second one — or retire it and create a new site.

Assign ranges centrally. Two nodes creating sites concurrently can each accept a range
that overlaps the other's, because each check passes locally and the merge happens
afterwards. The crate's `sites_no_overlap` trigger stops a malformed allocation entering
the table, but it cannot make two independent decisions agree.

### Do not reuse a globally unique identifier

If your key is globally unique — a `userid` from a per-site range — an identifier must
**never be reissued**, even after a crash. A gap is harmless; a reuse is corruption.

Allocate durably, then use:

```
allocate (durable, own transaction) → use
```

not the other way round. If the insert is rolled back, the identifier is simply skipped.

A decommissioned site's range is retired with it, and is never reissued — exactly like a
retired identifier.

### Do not rely on a local column surviving a merge

A column marked `local: true` — a local sequence, a local counter — keeps its **local**
value when a remote copy arrives. That is the point: the remote's value comes from a
different sequence and means nothing here.

The corollary is that such a column should not carry a `UNIQUE` constraint, because the
constraint holds within one site but not across the merged dataset. A remote row whose
local sequence collides with a different local row will fail to insert.

### Do not expect a JSON array column to merge as a set

### Do not write to `_sync_*`

These are internal. The authorizer rejects writes on instrumented connections, and the
log guard rejects them on any connection.

### Do not leak the UUID directory to a child

The directory — every node's UUID — is the child security boundary. Every child shares
the child string, so the UUID is the only thing distinguishing one child from another: a
child that learned another child's UUID could present it and be accepted as that child.

The crate sends a child only its own entry. If you build your own transport, do the
same.

### Do not put secrets in the database

The crate writes none of them, and neither should you. The database is replicated,
inspected, backed up and shipped around; it is the wrong place for the material that
authenticates a peer. Keep the digest seed and the role strings in the environment, a
mounted secret, or a KMS — and rotate them by rotating the configuration.

Rotation of the digest seed is the expensive one: it re-digests every row, so it is a full
rebuild of every tree. The role strings rotate cheaply.

### Do not expect a JSON array column to merge as a set

A column holding a JSON array is merged as a **value** under last-write-wins, so two sites
editing the same array concurrently lose one side's change:

```
site A: ["admin", "dev"]   site B: ["admin", "ops"]   →  ["admin", "ops"]
```

If membership needs to merge correctly, normalise it into its own table — one row per
member, each with its own last-write-wins and its own deletion. That is the same table a
`groups` schema wants anyway.

`MergeMode::SetUnion` is available, but it is additive only: a removal never propagates,
because the removed element comes back on the next union.

### Keep clocks synchronised

NTP or chrony, agreeing within ±1 second. This is a hard requirement, not a
recommendation — see [operations.md](operations.md).

### Do not change `epoch_us` after data exists

It re-buckets every row and invalidates every stored hash. There is no migration path.

---

## Common questions

### Can I use a different web framework?

Yes. The core does not depend on axum. `merkle-lww-axum` is a convenience, not a
requirement. Implement `Transport` for your framework and call `perform_exchange`.

### Can I use WebSocket only, or REST only?

Yes, either. The two are independent surfaces over the same session logic. WebSocket
adds the `changed` notification, so a client knows when to open a session; REST is
polled.

### Can I expose sync on an existing server?

Yes. Mount the routes, or call the core. Both work alongside an existing application.

### What if my application already has tables?

Use `Db::adopt_table(&spec)`. The crate adds its managed columns and installs the
triggers. Note that a table's primary key cannot be changed after the fact — see
[sqlite-interface.md](sqlite-interface.md) § "SQLite constraints".

### Can I sync only some tables?

Yes — the ones you list in `TableSpec`. Tables not listed are not managed and are not
touched.

### What if two of my tables should share a transaction?

`Db::transaction()` gives you one, and the managed operations take `&mut Transaction`:

```rust
db.transaction(|tx| {
    tx.upsert("users", &key, values)?;
    tx.upsert("readings", &reading_key, readings)?;
    Ok(())
})?;
```

The `last_update` stamping and the log update happen inside the transaction, so a
rollback undoes both. The Merkle tree is updated after the transaction commits.

### How do I monitor a sync?

`SyncReport` gives the counts. `SyncProgress` gives the live state, and the crate
publishes progress over its transports. See [protocol.md](protocol.md) § "Progress".

### What happens if my peer goes away mid-sync?

The session ends and the cursor stays where it was. The next session redoes the work.
There is no partial-commit state — see [protocol.md](protocol.md) § "Idempotency and
overlap".

---

## Checklist before going live

- [ ] All nodes share the same `epoch_us` and `digest_seed`.
- [ ] All nodes are within ±1 second of each other.
- [ ] `retention` and `purge_period` match across peers.
- [ ] The application uses `Db::conn()` (or the connection factory) for managed tables.
- [ ] TLS is terminated somewhere.
- [ ] Peers are in the configured peer list.
- [ ] A `verify` run reports no digest divergence.
- [ ] A test restore of one node completes and re-converges.
- [ ] The purge quiesce window does not overlap a deployment.

---

## Related documents

- [config.md](config.md) — configuration reference
- [sqlite-interface.md](sqlite-interface.md) — what the crate installs on the connection
- [protocol.md](protocol.md) — the wire format
- [operations.md](operations.md) — deployment and troubleshooting
- [security.md](security.md) — the trust model
