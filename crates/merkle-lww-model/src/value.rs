//! The five SQLite storage classes, as one Rust type.
//!
//! SQLite is dynamically typed, and the digest **distinguishes** `INTEGER` from
//! `REAL` and `TEXT` from `BLOB`. Anything that has to tell those apart — the
//! canonical encoding, the row digest, a merge policy — needs one type that
//! carries the distinction rather than four that do not.
//!
//! The **wire** form of a value is a different representation of the same
//! distinction, and it belongs to `merkle-lww-protocol`: a JSON `null`, a
//! number in integral syntax, `{"$f": …}` for a real, a JSON string, and
//! `{"$b": …}` for a blob. See [protocol.md](../../../doc/protocol.md)
//! § "Message encoding".

use crate::config::ColumnType;
use serde::{Deserialize, Serialize};

/// A value in one of SQLite's five storage classes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Value {
    /// `NULL`.
    Null,
    /// `INTEGER` — a signed 64-bit integer.
    Integer(i64),
    /// `REAL` — a 64-bit float.
    Real(f64),
    /// `TEXT`.
    Text(String),
    /// `BLOB` — an uninterpreted byte string.
    Blob(Vec<u8>),
}

impl Value {
    /// The storage class of this value.
    pub const fn column_type(&self) -> ColumnType {
        match self {
            Value::Null => ColumnType::Null,
            Value::Integer(_) => ColumnType::Integer,
            Value::Real(_) => ColumnType::Real,
            Value::Text(_) => ColumnType::Text,
            Value::Blob(_) => ColumnType::Blob,
        }
    }

    /// Whether this value is `NULL`.
    pub const fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// The integer, if this is one.
    pub const fn as_integer(&self) -> Option<i64> {
        match self {
            Value::Integer(v) => Some(*v),
            _ => None,
        }
    }

    /// The float, if this is one.
    pub const fn as_real(&self) -> Option<f64> {
        match self {
            Value::Real(v) => Some(*v),
            _ => None,
        }
    }

    /// The text, if this is one.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::Text(v) => Some(v),
            _ => None,
        }
    }

    /// The bytes, if this is a blob.
    pub fn as_blob(&self) -> Option<&[u8]> {
        match self {
            Value::Blob(v) => Some(v),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_column_type_is_the_storage_class() {
        assert_eq!(Value::Null.column_type(), ColumnType::Null);
        assert_eq!(Value::Integer(1).column_type(), ColumnType::Integer);
        assert_eq!(Value::Real(1.0).column_type(), ColumnType::Real);
        assert_eq!(Value::Text("x".into()).column_type(), ColumnType::Text);
        assert_eq!(Value::Blob(vec![]).column_type(), ColumnType::Blob);
    }

    #[test]
    fn an_integral_real_is_still_a_real() {
        // SQLite is dynamically typed, and the digest distinguishes the
        // storage classes. A `REAL` holding 5.0 is not an integer.
        let v = Value::Real(5.0);
        assert_eq!(v.column_type(), ColumnType::Real);
        assert_eq!(v.as_integer(), None);
        assert_eq!(v.as_real(), Some(5.0));
    }
}
