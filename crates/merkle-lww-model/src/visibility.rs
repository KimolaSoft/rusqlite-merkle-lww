//! The visibility profiles.
//!
//! A **visibility profile** is a named set of tables and columns, mapped to a
//! role and overridable per peer. It is the authoritative projection; the
//! `Public`/`Restricted` classification on a column is only the default.
//!
//! The profile definitions are hashed separately from the schema, into
//! `visibility_hash`. The separation is deliberate: `schema_hash` decides
//! whether two nodes compute the same digest for the same logical row, and
//! `visibility_hash` decides whether they project that row the same way.
//! Folding them together would force a read-only client — whose registration
//! is necessarily narrower — to fail the digest check.
//!
//! **Status: the types only.** The projection resolver and the hash itself are
//! the work of stage M3. See [plan/model.md](../../../plan/model.md).

use crate::error::Error;
use serde::{Deserialize, Serialize};
use std::fmt;

/// One table's visibility within a profile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileTable {
    /// The table's name.
    pub table: String,
    /// Whether the table is visible at all. The table is excluded entirely
    /// when this is false.
    pub visible: bool,
    /// The columns this profile may see.
    pub columns: Vec<String>,
}

impl Default for ProfileTable {
    fn default() -> Self {
        ProfileTable {
            table: String::new(),
            visible: true,
            columns: Vec::new(),
        }
    }
}

/// A named projection.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisibilityProfile {
    /// The profile's name.
    pub name: String,
    /// Whether the profile may see `Restricted` columns.
    pub trusted: bool,
    /// The profile's tables.
    pub tables: Vec<ProfileTable>,
}

/// A 128-bit hash of the visibility profile definitions.
///
/// The same shape as the row digest, and for the same reason: it is compared
/// as an unsigned 128-bit integer, not as bytes, so that two peers cannot
/// disagree about the comparison.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct VisibilityHash([u8; 16]);

impl VisibilityHash {
    /// The all-zero hash — the default, and the hash of no profiles.
    pub const fn zero() -> Self {
        VisibilityHash([0; 16])
    }

    /// The raw bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// Build a hash from its raw bytes.
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        VisibilityHash(bytes)
    }

    /// Parse the hexadecimal form.
    pub fn parse(input: &str) -> Result<Self, Error> {
        if input.len() != 32 || !input.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::Uuid(format!(
                "not a 32-digit hexadecimal hash: {input:?}"
            )));
        }
        let mut bytes = [0_u8; 16];
        for (index, chunk) in input.as_bytes().chunks(2).enumerate() {
            let text = std::str::from_utf8(chunk)
                .map_err(|_| Error::Uuid(format!("not a hexadecimal hash: {input:?}")))?;
            bytes[index] = u8::from_str_radix(text, 16)
                .map_err(|_| Error::Uuid(format!("not a hexadecimal hash: {input:?}")))?;
        }
        Ok(VisibilityHash(bytes))
    }
}

impl fmt::Display for VisibilityHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for VisibilityHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VisibilityHash({self})")
    }
}

impl Serialize for VisibilityHash {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for VisibilityHash {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        VisibilityHash::parse(&text).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hash_round_trips_through_its_text_form() {
        let hash = VisibilityHash::from_bytes([
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ]);
        let text = hash.to_string();
        assert_eq!(text.len(), 32);
        assert_eq!(VisibilityHash::parse(&text).unwrap(), hash);
    }

    #[test]
    fn a_malformed_hash_is_rejected() {
        for bad in ["", "abc", &"f".repeat(31), &"f".repeat(33), &"g".repeat(32)] {
            assert!(
                VisibilityHash::parse(bad).is_err(),
                "{bad:?} should not parse"
            );
        }
    }
}
