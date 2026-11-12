//! The configuration types.
//!
//! The crate does not impose a config format on its consumers. What it
//! provides is a `serde`-derived struct with sane defaults, the ability to
//! override any field from whatever config system the consumer already uses,
//! and **no opinion** about where the config comes from. See
//! [ADR-0007](../../../doc/adr/0007-config-is-a-library-type.md).
//!
//! Everything here is `serde`-derived with a `Default`, and every default is
//! written out by hand rather than derived — a derived default would give a
//! zero retention and a zero message budget, which are not "sane" but are
//! merely "zero".

use crate::error::Error;
use crate::merge::MergePolicyFn;
use crate::visibility::VisibilityHash;
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

/// A node's role in the deployment.
///
/// The role decides what a peer may see, what it may write, and which
/// credential it presents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// A master. Holds the master credential, serves children, and talks to
    /// the other masters.
    Master,
    /// A child. Holds the child credential, and has exactly one upstream
    /// master.
    Child,
    /// A read-only client. Sees a projection, and writes nothing.
    Client,
}

impl Default for Role {
    /// The **least** privileged role.
    ///
    /// A default that handed out write authority would be a default that
    /// handed out authority the operator did not ask for.
    fn default() -> Self {
        Role::Client
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Role::Master => "master",
            Role::Child => "child",
            Role::Client => "client",
        })
    }
}

/// A column's declared type — its SQLite affinity.
///
/// This is the column's affinity, **not** a guarantee about the value:
/// SQLite is dynamically typed, so a `text` column can hold an integer. The
/// type is carried on the wire so a receiver can decode a row's positional
/// values, and it is part of `schema_hash`, because the digest's input depends
/// on the storage class of every value.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum ColumnType {
    /// `NULL`.
    Null,
    /// `INTEGER`.
    Integer,
    /// `REAL`.
    Real,
    /// `TEXT`. The default — SQLite's own default affinity for a column with
    /// no declared type.
    #[default]
    Text,
    /// `BLOB`.
    Blob,
}

/// Whether a column is visible by default.
///
/// A `Public` column is visible to every profile **unless that profile
/// narrows it**. A `Restricted` column is visible only to `trusted` profiles,
/// and is never transmitted to a client.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    /// Visible to every profile, unless a profile narrows it. The default.
    #[default]
    Public,
    /// Visible only to trusted profiles.
    Restricted,
}

/// Whether a table's rows are updated in place or only ever inserted.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum TableKind {
    /// Rows are keyed and updated in place. The default.
    #[default]
    Keyed,
    /// Rows are only ever inserted. No updates, no deletes.
    ///
    /// A convenience: it removes `status` and `purge_on` from the table, since
    /// there is nothing to delete, and it removes the soft-delete machinery
    /// from the trigger set.
    AppendOnly,
}

/// How two versions of one **column** combine.
///
/// Note the distinction: [`MergeMode`] decides how a column's values combine,
/// and [`MergePolicy`] decides how two versions of one **row** combine.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum MergeMode {
    /// Last write wins on the whole value. The default.
    #[default]
    LastWriteWins,
    /// The values are sets; concurrent edits union.
    ///
    /// **Additive only** — a removal never propagates, because the removed
    /// element comes back on the next union. This is only correct for
    /// append-only sets.
    SetUnion,
}

/// Which trigger dialect the crate installs.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum WriteMode {
    /// UDF triggers. The crate can enforce the write-time invariants. The
    /// default.
    #[default]
    Enforced,
    /// Pure-SQL triggers. The invariants that need the authorizer cannot be
    /// enforced against a foreign connection.
    Compatible,
}

/// How a table's identifiers are allocated from per-site ranges.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllocationSpec {
    /// The column holding the globally unique identifier.
    pub id_column: String,
    /// The column naming the site that owns the identifier. The crate manages it.
    pub site_column: String,
    /// The registry table holding the site ranges. Must be in the same database
    /// as the table — triggers cannot reference an attached database.
    pub registry_table: String,
}

/// How a table's key is derived.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "strategy", rename_all = "snake_case")]
pub enum KeyStrategy {
    /// The application's own column, already globally unique and stable.
    /// The default.
    GlobalId {
        /// The key columns, in order.
        key_columns: Vec<String>,
    },
    /// A deterministic id derived by the crate from identity columns.
    Derived {
        /// Columns whose values form the derivation input.
        identity_columns: Vec<String>,
        /// Optional prefix distinguishing entity kinds within a table.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefix: Option<String>,
        /// The UUIDv5 namespace the derivation runs in.
        ///
        /// `None` means the crate derives it, per table, from the table name
        /// and `epoch_us`. That is stable **within a deployment** — every peer
        /// computes the same namespace — and differs **between** deployments,
        /// because the epoch differs. Set it explicitly if the ids must be
        /// stable across deployments too.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<Uuid>,
    },
    /// Owner-namespaced: the row's site is part of the key.
    SiteNamespaced {
        /// The column naming the owning site.
        site_column: String,
        /// The key columns, in order.
        key_columns: Vec<String>,
    },
    /// A random surrogate. Never merges across sites.
    Surrogate {
        /// The column the surrogate is written to.
        column: String,
    },
    /// SQLite's implicit rowid.
    Rowid,
}

impl Default for KeyStrategy {
    fn default() -> Self {
        KeyStrategy::GlobalId {
            key_columns: Vec::new(),
        }
    }
}

/// Who may write a table's rows.
///
/// Evaluated against the row's **origin** — the node that wrote it — and not
/// against the node that forwarded it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum WriteAuthority {
    /// Anyone. The default.
    #[default]
    Any,
    /// Only a master.
    MasterOnly,
    /// Only the site that owns the row.
    OwnerOnly,
    /// Only the listed roles.
    Roles(Vec<Role>),
}

impl WriteAuthority {
    /// The stable string for the non-list variants, or `None` for
    /// [`WriteAuthority::Roles`].
    pub fn as_str(&self) -> Option<&'static str> {
        match self {
            WriteAuthority::Any => Some("any"),
            WriteAuthority::MasterOnly => Some("master_only"),
            WriteAuthority::OwnerOnly => Some("owner_only"),
            WriteAuthority::Roles(_) => None,
        }
    }
}

impl Serialize for WriteAuthority {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            WriteAuthority::Any | WriteAuthority::MasterOnly | WriteAuthority::OwnerOnly => {
                serializer.serialize_str(self.as_str().expect("a unit variant has a name"))
            }
            WriteAuthority::Roles(roles) => roles.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for WriteAuthority {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct AuthorityVisitor;

        impl<'de> Visitor<'de> for AuthorityVisitor {
            type Value = WriteAuthority;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("\"any\", \"master_only\", \"owner_only\", or a list of roles")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                match value {
                    "any" => Ok(WriteAuthority::Any),
                    "master_only" => Ok(WriteAuthority::MasterOnly),
                    "owner_only" => Ok(WriteAuthority::OwnerOnly),
                    other => Err(de::Error::unknown_variant(
                        other,
                        &["any", "master_only", "owner_only"],
                    )),
                }
            }

            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut roles = Vec::new();
                while let Some(role) = seq.next_element::<Role>()? {
                    roles.push(role);
                }
                Ok(WriteAuthority::Roles(roles))
            }
        }

        deserializer.deserialize_any(AuthorityVisitor)
    }
}

/// How two versions of one **row** combine.
///
/// The default is last write wins, because it is a **total order**: given the
/// same pair of rows, every peer reaches the same conclusion, with no further
/// conditions. That property is what the whole convergence argument rests on.
#[derive(Clone, Default)]
pub enum MergePolicy {
    /// Last write wins on `(last_update, digest)`, per row. The default.
    #[default]
    LastWriteWins,
    /// The application decides. See [`MergePolicyFn`].
    Custom(Arc<dyn MergePolicyFn>),
}

impl MergePolicy {
    /// Whether this is the default rule.
    pub fn is_last_write_wins(&self) -> bool {
        matches!(self, MergePolicy::LastWriteWins)
    }
}

/// Two custom policies are equal only when they are the **same** policy.
///
/// A trait object cannot be compared by value — there is no way to ask two
/// `dyn MergePolicyFn` whether they behave alike, and pretending they are
/// equal because they are both custom would make a config comparison lie.
/// Identity is the only honest answer available.
impl PartialEq for MergePolicy {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (MergePolicy::LastWriteWins, MergePolicy::LastWriteWins) => true,
            (MergePolicy::Custom(a), MergePolicy::Custom(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl Eq for MergePolicy {}

impl fmt::Debug for MergePolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MergePolicy::LastWriteWins => f.write_str("LastWriteWins"),
            // The policy is code, not data: there is nothing useful to print.
            MergePolicy::Custom(_) => f.write_str("Custom(<application-defined>)"),
        }
    }
}

impl Serialize for MergePolicy {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // A custom merge policy is **code, not configuration**. It is named on
        // the wire and in a config file by its kind, and it cannot be
        // reconstructed from one — an application that wants a custom policy
        // installs it in code.
        serializer.serialize_str(match self {
            MergePolicy::LastWriteWins => "LastWriteWins",
            MergePolicy::Custom(_) => "Custom",
        })
    }
}

impl<'de> Deserialize<'de> for MergePolicy {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        match name.as_str() {
            "LastWriteWins" => Ok(MergePolicy::LastWriteWins),
            "Custom" => Err(de::Error::custom(
                "a custom merge policy is code, not configuration: it cannot be \
                 deserialised, and an application installs it in code",
            )),
            other => Err(de::Error::unknown_variant(
                other,
                &["LastWriteWins", "Custom"],
            )),
        }
    }
}

/// A column of a replicated table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnSpec {
    /// The column's name.
    pub name: String,
    /// The column's declared type — its affinity.
    pub r#type: ColumnType,
    /// Whether the column participates in sync and in the digest.
    ///
    /// A column that is not replicated is local-only: it is not synced, not
    /// hashed, and not part of the digest.
    pub replicated: bool,
    /// The column's default visibility.
    pub visibility: Visibility,
    /// Whether the column is meaningful only on the writing site.
    ///
    /// A local column **is** replicated, but is never overwritten by a remote
    /// write, and is **excluded from the digest** — otherwise two peers
    /// holding the same entity with different local values would compute
    /// different digests and never converge.
    pub local: bool,
    /// How two versions of this column combine.
    pub merge: MergeMode,
}

impl Default for ColumnSpec {
    fn default() -> Self {
        ColumnSpec {
            name: String::new(),
            r#type: ColumnType::default(),
            replicated: true,
            visibility: Visibility::default(),
            local: false,
            merge: MergeMode::default(),
        }
    }
}

/// A replicated table.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableSpec {
    /// The table's name.
    pub name: String,
    /// Whether rows are updated in place or only inserted.
    pub kind: TableKind,
    /// How the table's key is derived.
    pub key: KeyStrategy,
    /// The columns, **in registration order**.
    ///
    /// The order is load-bearing: the digest concatenates the replicated
    /// values in registration order, so two nodes that register the same
    /// columns in a different order compute different digests for identical
    /// rows. The order is part of `schema_hash`.
    pub columns: Vec<ColumnSpec>,
    /// How two versions of a row combine.
    pub merge: MergePolicy,
    /// Who may write this table's rows.
    pub write_authority: WriteAuthority,
    /// Lower syncs first.
    pub sync_priority: u8,
    /// Present when the identifiers are allocated from per-site ranges.
    /// Absent in a deployment that brings its own identifier scheme.
    pub allocation: Option<AllocationSpec>,
}

/// How a peer authenticates.
///
/// This is local configuration, and it is the trust boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PeerAuth {
    /// A role-level shared secret, compared in constant time.
    RoleSecret {
        /// The role the secret belongs to.
        role: Role,
        /// The secret itself.
        secret: SecretString,
    },
    /// A per-node secret. Each node has its own; the holder learns *which*
    /// node authenticated, which role-level credentials cannot tell them.
    PerNodeSecret {
        /// The node the secret belongs to.
        node_id: Uuid,
        /// The secret itself.
        secret: SecretString,
    },
    /// Client certificate subject/SAN, checked against the presented
    /// certificate.
    ClientCert {
        /// The expected subject or SAN.
        subject: String,
    },
    /// Trust the transport: TLS is terminated elsewhere and mTLS is already
    /// verified.
    TransportOnly,
}

impl Default for PeerAuth {
    /// The weakest requirement.
    fn default() -> Self {
        PeerAuth::TransportOnly
    }
}

/// A peer this node talks to, or serves.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Peer {
    /// The peer's stable identifier, issued by a master.
    pub node_id: Uuid,
    /// A name for logs and the user interface.
    pub name: String,
    /// The peer's role.
    pub role: Role,
    /// Where to reach it.
    pub endpoint: String,
    /// How it authenticates.
    pub auth: PeerAuth,
    /// Overrides the role→profile mapping for this peer. `None` uses the
    /// mapping.
    pub profile: Option<String>,
}

impl Default for Peer {
    fn default() -> Self {
        Peer {
            node_id: Uuid::nil(),
            name: String::new(),
            role: Role::default(),
            endpoint: String::new(),
            auth: PeerAuth::default(),
            profile: None,
        }
    }
}

/// The read-through cache's limits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheConfig {
    /// The eligibility rule: rows whose `last_update` is within this window
    /// are eligible for caching. These are the rows an application actually
    /// re-reads.
    pub window: Duration,
    /// The byte bound. This is the one that matters — a cache of "the last
    /// 24 h of changes" naturally bounds itself by time, but a single key
    /// updated every second would otherwise grow without limit.
    pub max_bytes: usize,
    /// The entry bound.
    pub max_entries: usize,
    /// Evicts entries not read recently, so the working set does not
    /// accumulate stale rows.
    pub time_to_idle: Option<Duration>,
}

impl Default for CacheConfig {
    fn default() -> Self {
        CacheConfig {
            window: Duration::from_secs(24 * 3600),
            max_bytes: 256 * 1024 * 1024,
            max_entries: 200_000,
            time_to_idle: Some(Duration::from_secs(600)),
        }
    }
}

/// The crate's configuration.
///
/// The values in the first group **must agree across peers**: a mismatch
/// between two peers is a correctness bug rather than a policy difference,
/// and it is fatal at handshake. The rest is local policy.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SyncConfig {
    // ---- must agree across peers ----
    /// The bucket epoch. An arbitrary instant whose only job is to make bucket
    /// boundaries agree. **It cannot change** — changing it re-buckets every
    /// row and invalidates every stored hash.
    pub epoch_us: i64,
    /// The digest seed. The digest is keyed, and the seed never leaves the
    /// process.
    pub digest_seed: [u8; 32],
    /// How long a tombstone lives.
    pub retention: Duration,
    /// The interval between consecutive purge instants.
    pub purge_period: Duration,
    /// The offset within `purge_period` at which the purge instant falls.
    pub purge_offset_us: i64,
    /// Derived from `profiles`.
    pub visibility_hash: VisibilityHash,

    // ---- local identity ----
    /// This node's identifier, issued by a master.
    pub node_id: Uuid,
    /// This node's site name.
    pub site: String,
    /// This node's role.
    pub role: Role,

    // ---- credentials ----
    /// The role string for this node's role. A master holds both; a child
    /// holds only the child string. Absent for a client.
    pub credential: Option<SecretString>,

    // ---- visibility ----
    /// The named projections.
    pub profiles: Vec<crate::visibility::VisibilityProfile>,
    /// The role→profile mapping.
    pub role_profile: HashMap<Role, String>,
    /// The peers this node serves or talks to.
    pub peers: Vec<Peer>,

    // ---- local policy ----
    /// Ranges newer than this compare at hour depth.
    pub hot_window: Duration,
    /// Ranges newer than this compare at cycle depth; older compare at month
    /// depth.
    pub warm_window: Duration,
    /// The window around each purge instant during which the tables being
    /// purged refuse sync.
    pub purge_quiesce: Duration,
    /// Refuse a peer whose clock is more than this far out, with `CLOCK_SKEW`.
    pub max_skew: Duration,
    /// How far the two sides' chosen comparison depths may differ before the
    /// client reports adopting the server's.
    pub as_of_tolerance: Duration,
    /// The soft budget one protocol message may occupy, measured on the JSON
    /// message before compression.
    pub max_message_bytes: usize,
    /// Round trips before a session ends with a retryable `SESSION_LIMIT`.
    pub max_rounds_per_session: u32,
    /// A tripwire on the number of buckets in the tree. Zero disables it.
    pub max_tree_buckets: usize,
    /// Which trigger dialect is in use.
    pub write_mode: WriteMode,
    /// Whether to record attribution in `_sync_audit`.
    pub audit: bool,
    /// The read-through cache's limits.
    pub cache: CacheConfig,
    /// The replicated tables.
    pub tables: Vec<TableSpec>,
}

impl Default for SyncConfig {
    fn default() -> Self {
        SyncConfig {
            // 2025-01-01T00:00:00Z. A conventional choice, because it makes
            // the boundaries line up with the calendar — but any fixed value
            // works, and it must never change.
            epoch_us: 1_735_689_600_000_000,
            // A default seed is not a secret. It exists so that a config
            // round-trips; a deployment sets its own, and a deployment that
            // leaves this in place has published its digests to anyone who
            // asks.
            digest_seed: [0; 32],
            retention: Duration::from_secs(30 * 24 * 3600),
            purge_period: Duration::from_secs(24 * 3600),
            purge_offset_us: 0,
            visibility_hash: VisibilityHash::default(),
            node_id: Uuid::nil(),
            site: String::new(),
            role: Role::default(),
            credential: None,
            profiles: Vec::new(),
            role_profile: HashMap::new(),
            peers: Vec::new(),
            hot_window: Duration::from_secs(24 * 3600),
            warm_window: Duration::from_secs(90 * 24 * 3600),
            purge_quiesce: Duration::from_secs(60),
            max_skew: Duration::from_secs(5),
            as_of_tolerance: Duration::from_secs(1),
            max_message_bytes: 4 * 1024 * 1024,
            max_rounds_per_session: 1000,
            max_tree_buckets: 0,
            write_mode: WriteMode::default(),
            audit: false,
            cache: CacheConfig::default(),
            tables: Vec::new(),
        }
    }
}

/// A secret.
///
/// The `Debug` implementation prints a redaction marker and **nothing** of
/// the secret, and the equality check compares in constant time — a peer
/// credential is compared against a presented value, and a comparison that
/// leaks its length or its first differing byte is a comparison an attacker
/// can grind.
///
/// The crate persists no secret material. The digest seed, the role strings and
/// the peer list are configuration, held in memory for the life of the process.
#[derive(Clone, Default, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretString(String);

impl SecretString {
    /// Wrap a secret.
    pub fn new(secret: impl Into<String>) -> Self {
        SecretString(secret.into())
    }

    /// The secret's contents.
    ///
    /// Named so that every call site is a visible decision to expose it.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }

    /// Whether the secret is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretString(<redacted>)")
    }
}

impl PartialEq for SecretString {
    /// Compares in constant time, and does not leak the length through an
    /// early return.
    fn eq(&self, other: &Self) -> bool {
        let a = self.0.as_bytes();
        let b = other.0.as_bytes();
        if a.len() != b.len() {
            return false;
        }
        let mut difference = 0_u8;
        for (x, y) in a.iter().zip(b.iter()) {
            difference |= x ^ y;
        }
        difference == 0
    }
}

/// A 128-bit identifier.
///
/// The crate defines its own rather than depending on a UUID crate, because
/// the compatibility surface is meant to depend on `serde` and nothing else,
/// and because the byte layout is part of the contract: it is what appears in
/// `_sync_meta`, what is compared in the directory, and what goes on the wire.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Uuid([u8; 16]);

impl Uuid {
    /// The all-zero UUID.
    pub const fn nil() -> Self {
        Uuid([0; 16])
    }

    /// Whether this is the all-zero UUID.
    ///
    /// A nil `node_id` means "not provisioned", which is a different thing
    /// from "provisioned as some specific node".
    pub const fn is_nil(&self) -> bool {
        let mut i = 0;
        while i < 16 {
            if self.0[i] != 0 {
                return false;
            }
            i += 1;
        }
        true
    }

    /// The raw bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// Build a UUID from its raw bytes.
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Uuid(bytes)
    }

    /// Parse a UUID.
    ///
    /// Accepts the hyphenated form (`6f1e0c2a-…`) and the bare 32-digit
    /// hexadecimal form. The hyphenated form is the canonical one; the bare
    /// form is accepted because it is what some tooling emits.
    pub fn parse(input: &str) -> Result<Self, Error> {
        let bad = || Error::Uuid(input.to_string());

        let hex: String = match input.len() {
            32 => input.to_string(),
            36 => {
                let b = input.as_bytes();
                if b[8] != b'-' || b[13] != b'-' || b[18] != b'-' || b[23] != b'-' {
                    return Err(bad());
                }
                let mut hex = String::with_capacity(32);
                hex.push_str(&input[0..8]);
                hex.push_str(&input[9..13]);
                hex.push_str(&input[14..18]);
                hex.push_str(&input[19..23]);
                hex.push_str(&input[24..36]);
                hex
            }
            _ => return Err(bad()),
        };

        if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(bad());
        }

        let mut bytes = [0_u8; 16];
        for (index, chunk) in hex.as_bytes().chunks(2).enumerate() {
            let text = std::str::from_utf8(chunk).map_err(|_| bad())?;
            bytes[index] = u8::from_str_radix(text, 16).map_err(|_| bad())?;
        }
        Ok(Uuid(bytes))
    }
}

impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = &self.0;
        write!(
            f,
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-\
             {:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            b[0],
            b[1],
            b[2],
            b[3],
            b[4],
            b[5],
            b[6],
            b[7],
            b[8],
            b[9],
            b[10],
            b[11],
            b[12],
            b[13],
            b[14],
            b[15]
        )
    }
}

impl fmt::Debug for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::str::FromStr for Uuid {
    type Err = Error;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Uuid::parse(input)
    }
}

impl Serialize for Uuid {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Uuid {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Uuid::parse(&text).map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_uuid_round_trips_through_its_text_form() {
        let text = "6f1e0c2a-1b3d-4e5f-8a9b-0c1d2e3f4a5b";
        let uuid = Uuid::parse(text).unwrap();
        assert_eq!(uuid.to_string(), text);
        assert_eq!(
            Uuid::parse(&uuid.to_string().replace('-', "")).unwrap(),
            uuid
        );
    }

    #[test]
    fn a_malformed_uuid_is_rejected() {
        for bad in [
            "",
            "not-a-uuid",
            "6f1e0c2a-1b3d-4e5f-8a9b-0c1d2e3f4a5",
            "6f1e0c2a-1b3d-4e5f-8a9b-0c1d2e3f4a5zz",
            "6f1e0c2a_1b3d-4e5f-8a9b-0c1d2e3f4a5b",
            "6f1e0c2a-1b3d-4e5f-8a9b-0c1d2e3f4a5b-",
        ] {
            assert!(Uuid::parse(bad).is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn a_nil_uuid_is_recognised() {
        assert!(Uuid::nil().is_nil());
        assert!(!Uuid::parse("6f1e0c2a-1b3d-4e5f-8a9b-0c1d2e3f4a5b")
            .unwrap()
            .is_nil());
    }

    #[test]
    fn a_secret_is_not_printed() {
        let secret = SecretString::new("hunter2-the-master-string");
        let printed = format!("{secret:?}");
        assert!(!printed.contains("hunter2"), "{printed} leaked the secret");
        assert!(!printed.contains("master"), "{printed} leaked the secret");
    }

    #[test]
    fn a_secret_compares_equal_to_itself() {
        let a = SecretString::new("abc");
        let b = SecretString::new("abc");
        let c = SecretString::new("abd");
        let d = SecretString::new("ab");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
    }

    #[test]
    fn the_key_strategy_serialises_to_the_documented_shape() {
        let strategy = KeyStrategy::GlobalId {
            key_columns: vec!["id".into()],
        };
        let json = serde_json::to_string(&strategy).unwrap();
        assert_eq!(json, r#"{"strategy":"global_id","key_columns":["id"]}"#);
    }

    #[test]
    fn the_write_authority_serialises_to_the_documented_shape() {
        assert_eq!(
            serde_json::to_string(&WriteAuthority::Any).unwrap(),
            r#""any""#
        );
        assert_eq!(
            serde_json::to_string(&WriteAuthority::MasterOnly).unwrap(),
            r#""master_only""#
        );
        assert_eq!(
            serde_json::to_string(&WriteAuthority::Roles(vec![Role::Master])).unwrap(),
            r#"["master"]"#
        );
    }

    #[test]
    fn the_write_authority_round_trips() {
        for authority in [
            WriteAuthority::Any,
            WriteAuthority::MasterOnly,
            WriteAuthority::OwnerOnly,
            WriteAuthority::Roles(vec![Role::Master, Role::Child]),
        ] {
            let json = serde_json::to_string(&authority).unwrap();
            let back: WriteAuthority = serde_json::from_str(&json).unwrap();
            assert_eq!(back, authority);
        }
    }

    #[test]
    fn a_custom_merge_policy_cannot_be_deserialised() {
        // It is code, not configuration.
        assert!(serde_json::from_str::<MergePolicy>(r#""Custom""#).is_err());
        assert_eq!(
            serde_json::from_str::<MergePolicy>(r#""LastWriteWins""#).unwrap(),
            MergePolicy::LastWriteWins
        );
    }
}
