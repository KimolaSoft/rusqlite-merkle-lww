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
    node_id:  Uuid::new_v4(),
    site:     "eu-west".into(),
    epoch_us: 1735689600000000,      // shared across the deployment
    role:     Role::Master,
    ..Default::default()
};
```

`epoch_us` and `digest_seed` must be identical across every trusted peer. See
[config.md](config.md).

### 2. Describe your tables

```rust
let spec = TableSpec {
    name: "device".into(),
    kind: TableKind::Keyed,
    key: KeyStrategy::SiteNamespaced {
        site_column: "sync_ns".into(),
        key_columns: vec!["id".into()],
    },
    columns: vec![
        ColumnSpec { name: "payload".into(), replicated: true,  visibility: Visibility::Public },
        ColumnSpec { name: "secret".into(), replicated: true,  visibility: Visibility::Restricted },
    ],
    client_visible: true,
};
```

### 3. Open the store

```rust
let db = Db::open("app.sqlite", config, vec![spec])?;
db.migrate()?;                 // create or adopt the managed tables
```

### 4. Use the connection

```rust
let conn = db.conn()?;                        // pooled, instrumented
conn.execute("INSERT INTO device(sync_ns, id, payload) VALUES (?1, ?2, ?3)",
             params!["eu-west", "abc", "v1"])?;
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
UPDATE device SET payload = 'v2' WHERE sync_ns = ?1 AND id = ?2;

-- Only when importing historical data, where the timestamp is the point:
UPDATE device SET payload = 'v2', last_update = '2024-01-01T00:00:00.000000Z'
 WHERE sync_ns = ?1 AND id = ?2;
```

### Do not write the managed columns

`status` and `purge_on` are crate-managed. Use `Db::delete()` for a delete:

```rust
db.delete("device", &key)?;      // sets status and purge_on
```

A raw `DELETE` is converted to a soft delete by a trigger, and a raw write to `status` is
rejected by the authorizer on instrumented connections.

### Do not write to `_sync_*`

These are internal. The authorizer rejects writes on instrumented connections, and the
log guard rejects them on any connection.

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
    tx.upsert("device", &key, values)?;
    tx.upsert("reading", &reading_key, readings)?;
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
