# ADR-0007: Config is a library type, not a file format

- **Status:** accepted
- **Date:** 2026-11
- **Deciders:** project maintainers

## Context

The crate needs configuration: an epoch, a digest seed, retention, the peer list, table
specifications.

There are two ways to provide it:

- **The crate ships a config format.** A TOML or YAML schema, a loader, an environment
  variable convention.
- **The crate ships a config *type*.** A `serde`-derived struct with defaults, which the
  consumer populates from whatever it already uses.

The requirement was:

> some of the 'config' options just need to be a part of however users will implement the
> crate, it will need some kind of config, some kind of table create

The observation behind it is that the consumer already has a config system — every
application does — and a library that ships its own format either duplicates that or
forces a migration.

## Decision

**Ship a `serde`-derived struct with a `Default` implementation. Do not ship a config
file format.**

```rust
let config = SyncConfig {
    epoch_us: 1735689600000000,
    ..Default::default()
};
```

The consumer wires it into `figment`, `config`, environment variables, or hand-written
constants. A `from_toml`/`from_env` convenience is available behind an **optional
feature**, so it never becomes a reason to adopt anything.

## Consequences

**Positive**

- **No dependency on a config library.** The crate's dependency list stays small, and
  the consumer does not get two config systems.
- **No forced migration.** An application with an existing config file adds one struct
  literal; it does not restructure its configuration.
- **Defaults are discoverable.** `SyncConfig::default()` in the documentation tells a
  consumer what every field does, without reading a format specification.
- **Still serialisable.** Because the struct is `serde`-derived, a consumer that *does*
  want a file can derive or write the serialisation itself.

**Negative**

- **The crate cannot offer a "just works" configuration story.** A consumer wanting
  zero-config has to write the wiring. This is a real cost for the simplest possible
  onboarding.
- **Every consumer writes the same loader.** Some duplication across the ecosystem.
- **The optional `from_toml` feature is a maintenance burden** — a second path into the
  same struct, which can drift from the first.

**Neutral**

- Environment variable naming is not standardised by the crate. The optional feature
  provides one convention; consumers using their own system are on their own.

## Alternatives considered

**Ship a TOML schema and loader as the primary interface.** Rejected: it forces a
config format on consumers who have one, and it is the reason many libraries end up
with two config systems in one application.

**Ship a builder API instead of a struct.** Rejected: a builder is ergonomic for
optional settings but verbose for the common case, and it does not compose with serde.
A struct with `Default` covers both — `..Default::default()` in a struct literal is
concise, and every field can still be overridden.

**No configuration surface at all — hardcoded constants.** Rejected: `epoch_us`,
`digest_seed` and the peer list *cannot* be hardcoded, because they must agree across
the deployment.

**Environment variables only.** Rejected: unsuitable for complex nested structures like
the table specifications.

## Related

- [../config.md](../config.md) — the full field reference
- [../architecture.md](../architecture.md) — the crate dependency list
