//! Every configuration type round-trips through serde.
//!
//! A `Default` value serialises and deserialises to itself. This is what makes
//! a config file round-trippable: a deployment can read its config, write it
//! back, and get the same deployment — and a tool that rewrites the config
//! does not silently drop a field.
//!
//! It is also a check that the types do not grow a field that cannot be
//! serialised, which is the way a config type rots.

use merkle_lww_model::config::MergePolicy;
use merkle_lww_model::merge::{MergeContext, MergeDecision, MergePolicyFn, RejectReason};
use merkle_lww_model::value::Value;
use merkle_lww_model::{
    AllocationSpec, CacheConfig, ColumnSpec, ColumnType, KeyStrategy, MergeMode, Peer, PeerAuth,
    ProfileTable, Role, SecretString, SyncConfig, TableKind, TableSpec, Uuid, Visibility,
    VisibilityHash, VisibilityProfile, WriteAuthority, WriteMode,
};
use std::collections::HashMap;

/// Assert that a value survives a trip through JSON unchanged.
fn assert_round_trips<T>(value: &T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let json = serde_json::to_string(value).expect("serialises");
    let back: T = serde_json::from_str(&json).expect("deserialises");
    assert_eq!(&back, value, "round trip changed the value: {json}");
}

#[test]
fn the_configuration_round_trips() {
    assert_round_trips(&SyncConfig::default());
}

#[test]
fn every_configuration_type_round_trips() {
    assert_round_trips(&Role::default());
    assert_round_trips(&ColumnType::default());
    assert_round_trips(&Visibility::default());
    assert_round_trips(&TableKind::default());
    assert_round_trips(&MergeMode::default());
    assert_round_trips(&WriteMode::default());
    assert_round_trips(&KeyStrategy::default());
    assert_round_trips(&WriteAuthority::default());
    assert_round_trips(&MergePolicy::default());
    assert_round_trips(&SecretString::default());
    assert_round_trips(&Uuid::default());
    assert_round_trips(&ColumnSpec::default());
    assert_round_trips(&TableSpec::default());
    assert_round_trips(&AllocationSpec::default());
    assert_round_trips(&Peer::default());
    assert_round_trips(&PeerAuth::default());
    assert_round_trips(&CacheConfig::default());
    assert_round_trips(&VisibilityProfile::default());
    assert_round_trips(&ProfileTable::default());
    assert_round_trips(&VisibilityHash::default());
}

#[test]
fn every_variant_of_every_enumeration_round_trips() {
    for role in [Role::Master, Role::Child, Role::Client] {
        assert_round_trips(&role);
    }
    for column_type in [
        ColumnType::Null,
        ColumnType::Integer,
        ColumnType::Real,
        ColumnType::Text,
        ColumnType::Blob,
    ] {
        assert_round_trips(&column_type);
    }
    for visibility in [Visibility::Public, Visibility::Restricted] {
        assert_round_trips(&visibility);
    }
    for kind in [TableKind::Keyed, TableKind::AppendOnly] {
        assert_round_trips(&kind);
    }
    for mode in [MergeMode::LastWriteWins, MergeMode::SetUnion] {
        assert_round_trips(&mode);
    }
    for write_mode in [WriteMode::Enforced, WriteMode::Compatible] {
        assert_round_trips(&write_mode);
    }
    for authority in [
        WriteAuthority::Any,
        WriteAuthority::MasterOnly,
        WriteAuthority::OwnerOnly,
        WriteAuthority::Roles(vec![Role::Master, Role::Child]),
    ] {
        assert_round_trips(&authority);
    }
    for strategy in [
        KeyStrategy::GlobalId {
            key_columns: vec!["id".into()],
        },
        KeyStrategy::Derived {
            identity_columns: vec!["email".into()],
            prefix: Some("user".into()),
            namespace: Some(Uuid::parse("6f1e0c2a-1b3d-4e5f-8a9b-0c1d2e3f4a5b").unwrap()),
        },
        KeyStrategy::Derived {
            identity_columns: vec!["email".into()],
            prefix: None,
            namespace: None,
        },
        KeyStrategy::SiteNamespaced {
            site_column: "site".into(),
            key_columns: vec!["id".into()],
        },
        KeyStrategy::Surrogate {
            column: "sync_id".into(),
        },
        KeyStrategy::Rowid,
    ] {
        assert_round_trips(&strategy);
    }
}

#[test]
fn a_populated_configuration_round_trips() {
    let config = SyncConfig {
        epoch_us: 1_735_689_600_000_000,
        digest_seed: [7; 32],
        node_id: Uuid::parse("6f1e0c2a-1b3d-4e5f-8a9b-0c1d2e3f4a5b").unwrap(),
        site: "eu-west".into(),
        role: Role::Master,
        credential: Some(SecretString::new("the master string")),
        peers: vec![Peer {
            node_id: Uuid::parse("11111111-2222-3333-4444-555555555555").unwrap(),
            name: "tokyo".into(),
            role: Role::Master,
            endpoint: "https://tokyo.example".into(),
            auth: PeerAuth::RoleSecret {
                role: Role::Master,
                secret: SecretString::new("shared"),
            },
            profile: Some("master".into()),
        }],
        profiles: vec![VisibilityProfile {
            name: "master".into(),
            trusted: true,
            tables: vec![ProfileTable {
                table: "device".into(),
                visible: true,
                columns: vec!["payload".into()],
            }],
        }],
        tables: vec![TableSpec {
            name: "device".into(),
            kind: TableKind::Keyed,
            key: KeyStrategy::GlobalId {
                key_columns: vec!["id".into()],
            },
            columns: vec![ColumnSpec {
                name: "payload".into(),
                r#type: ColumnType::Text,
                replicated: true,
                visibility: Visibility::Public,
                local: false,
                merge: MergeMode::LastWriteWins,
            }],
            merge: MergePolicy::LastWriteWins,
            write_authority: WriteAuthority::Any,
            sync_priority: 0,
            allocation: None,
        }],
        role_profile: HashMap::from([(Role::Master, "master".to_string())]),
        ..Default::default()
    };

    assert_round_trips(&config);
}

#[test]
fn a_value_round_trips() {
    for value in [
        Value::Null,
        Value::Integer(-1),
        Value::Real(1.5),
        Value::Text("hello".into()),
        Value::Blob(vec![0, 1, 2, 255]),
    ] {
        assert_round_trips(&value);
    }
}

#[test]
fn a_merge_decision_round_trips() {
    let row = merkle_lww_model::Row {
        key: vec![1, 2, 3],
        digest: [9; 16],
        last_update: 1_735_689_600_000_000,
        values: vec![Value::Integer(42)],
    };
    for decision in [
        MergeDecision::TakeIncoming,
        MergeDecision::KeepLocal,
        MergeDecision::Undecided,
        MergeDecision::Merged(row.clone()),
        MergeDecision::Reject(RejectReason::WriteNotPermitted),
        MergeDecision::Reject(RejectReason::Other("because".into())),
    ] {
        assert_round_trips(&decision);
    }
}

/// A policy that exists only to prove the extension point does not drag a
/// SQLite dependency in behind it.
struct AlwaysTake;

impl MergePolicyFn for AlwaysTake {
    fn merge(
        &self,
        _ctx: &MergeContext<'_>,
        _incoming: &merkle_lww_model::Row,
        _local: Option<&merkle_lww_model::Row>,
    ) -> MergeDecision {
        MergeDecision::TakeIncoming
    }
}

#[test]
fn a_custom_policy_is_named_but_not_serialised() {
    let policy = MergePolicy::Custom(std::sync::Arc::new(AlwaysTake));
    let json = serde_json::to_string(&policy).unwrap();
    assert_eq!(json, r#""Custom""#);
    assert!(serde_json::from_str::<MergePolicy>(&json).is_err());
}

/// The serialised shapes are the wire shapes.
///
/// `protocol.md` § "Message encoding" fixes them, and the handshake declares
/// the columns in them. A change to any of these strings is a change to the
/// wire contract, and the fixture diff is where it should show up.
#[test]
fn the_serialised_shapes_are_the_documented_wire_shapes() {
    assert_eq!(
        serde_json::to_string(&KeyStrategy::GlobalId {
            key_columns: vec!["id".into()],
        })
        .unwrap(),
        r#"{"strategy":"global_id","key_columns":["id"]}"#
    );
    assert_eq!(
        serde_json::to_string(&KeyStrategy::Rowid).unwrap(),
        r#"{"strategy":"rowid"}"#
    );
    assert_eq!(serde_json::to_string(&Role::Master).unwrap(), r#""master""#);
    assert_eq!(
        serde_json::to_string(&ColumnType::Blob).unwrap(),
        r#""blob""#
    );
    assert_eq!(
        serde_json::to_string(&Visibility::Restricted).unwrap(),
        r#""restricted""#
    );
    assert_eq!(
        serde_json::to_string(&TableKind::Keyed).unwrap(),
        r#""keyed""#
    );
    assert_eq!(
        serde_json::to_string(&WriteAuthority::Any).unwrap(),
        r#""any""#
    );
    assert_eq!(
        serde_json::to_string(&Uuid::nil()).unwrap(),
        r#""00000000-0000-0000-0000-000000000000""#
    );
}
