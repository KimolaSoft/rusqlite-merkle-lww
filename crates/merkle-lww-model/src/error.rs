//! The error codes, and the crate's error type.
//!
//! The codes are the crate's public contract with the wire. A code is a stable
//! string; the numeric value that may sit behind it is an implementation
//! detail, and the numbering is not part of the contract.
//!
//! The three groups are the three groups in [protocol.md](../doc/protocol.md)
//! § "Error codes": a **fatal** code ends the session, a **non-fatal** code
//! lets the session continue with a caveat, and a **local** code is the
//! crate's own failure and is never sent to a peer.

use std::fmt;

/// How bad a failure is.
///
/// The distinction is not cosmetic. A fatal code means there is **no**
/// well-defined way to continue — the two sides would compute different
/// digests for the same logical row, or purge different sets. A non-fatal code
/// means the session continues, with something dropped or refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// The session cannot proceed.
    Fatal,
    /// The session continues with a caveat.
    NonFatal,
    /// The crate's own failure. Never sent to a peer.
    Local,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Severity::Fatal => "fatal",
            Severity::NonFatal => "non-fatal",
            Severity::Local => "local",
        })
    }
}

/// A stable, wire-visible failure code.
///
/// Every code the protocol can report is here, and the `as_str` of each is the
/// exact string that appears on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ErrorCode {
    // ---- fatal: the session cannot proceed ----
    /// The peer speaks a different protocol version.
    ProtocolUnsupported,
    /// The canonical encoding differs.
    CodecMismatch,
    /// The bucket epoch differs.
    EpochMismatch,
    /// The digest seed differs.
    DigestSeedMismatch,
    /// The digest inputs differ.
    SchemaMismatch,
    /// The visibility profiles differ.
    VisibilityMismatch,
    /// The presented UUID is not in the directory.
    UnknownPeer,
    /// The claimed UUID is not the identity the transport authenticated.
    IdentityMismatch,
    /// The credential does not verify for the claimed role.
    BadCredential,
    /// The peer is older than the staleness horizon.
    StalePeer,
    /// The purge schedule differs.
    PurgeScheduleMismatch,
    /// The peer's clock is more than `max_skew` away.
    ClockSkew,

    // ---- non-fatal: the session continues with a caveat ----
    /// A peer asked for a column outside its profile.
    ColumnNotPermitted,
    /// A peer asked for a table its profile excludes.
    TableNotPermitted,
    /// A row's origin may not write that table.
    WriteNotPermitted,
    /// A tombstone the receiver has already purged.
    RowPurged,
    /// The tables being purged are quiescing.
    PurgeInProgress,
    /// The session hit `max_rounds_per_session`.
    SessionLimit,
    /// A peer named a table this node does not sync.
    UnknownTable,
    /// A peer named a column this node does not have.
    UnknownColumn,
    /// A row's identifier is outside its owner's range.
    RangeViolation,
    /// Two rows in one batch carry the same key.
    KeyConflict,
    /// A progress report is older than the cursor.
    ProgressStale,

    // ---- local: the crate's own failures ----
    /// The allocator handed out an identifier it had already used.
    IdentifierReused,
    /// A site's range overlaps another site's.
    OverlappingRanges,
    /// An attempt to move a site's range.
    RangeImmutable,
    /// A stored digest does not match a recomputed one.
    DigestMismatch,
}

impl ErrorCode {
    /// Every code, in the order the tables in `protocol.md` list them.
    pub const ALL: &'static [ErrorCode] = &[
        ErrorCode::ProtocolUnsupported,
        ErrorCode::CodecMismatch,
        ErrorCode::EpochMismatch,
        ErrorCode::DigestSeedMismatch,
        ErrorCode::SchemaMismatch,
        ErrorCode::VisibilityMismatch,
        ErrorCode::UnknownPeer,
        ErrorCode::IdentityMismatch,
        ErrorCode::BadCredential,
        ErrorCode::StalePeer,
        ErrorCode::PurgeScheduleMismatch,
        ErrorCode::ClockSkew,
        ErrorCode::ColumnNotPermitted,
        ErrorCode::TableNotPermitted,
        ErrorCode::WriteNotPermitted,
        ErrorCode::RowPurged,
        ErrorCode::PurgeInProgress,
        ErrorCode::SessionLimit,
        ErrorCode::UnknownTable,
        ErrorCode::UnknownColumn,
        ErrorCode::RangeViolation,
        ErrorCode::KeyConflict,
        ErrorCode::ProgressStale,
        ErrorCode::IdentifierReused,
        ErrorCode::OverlappingRanges,
        ErrorCode::RangeImmutable,
        ErrorCode::DigestMismatch,
    ];

    /// The stable string for this code — the exact text that goes on the wire.
    pub const fn as_str(self) -> &'static str {
        match self {
            ErrorCode::ProtocolUnsupported => "PROTOCOL_UNSUPPORTED",
            ErrorCode::CodecMismatch => "CODEC_MISMATCH",
            ErrorCode::EpochMismatch => "EPOCH_MISMATCH",
            ErrorCode::DigestSeedMismatch => "DIGEST_SEED_MISMATCH",
            ErrorCode::SchemaMismatch => "SCHEMA_MISMATCH",
            ErrorCode::VisibilityMismatch => "VISIBILITY_MISMATCH",
            ErrorCode::UnknownPeer => "UNKNOWN_PEER",
            ErrorCode::IdentityMismatch => "IDENTITY_MISMATCH",
            ErrorCode::BadCredential => "BAD_CREDENTIAL",
            ErrorCode::StalePeer => "STALE_PEER",
            ErrorCode::PurgeScheduleMismatch => "PURGE_SCHEDULE_MISMATCH",
            ErrorCode::ClockSkew => "CLOCK_SKEW",
            ErrorCode::ColumnNotPermitted => "COLUMN_NOT_PERMITTED",
            ErrorCode::TableNotPermitted => "TABLE_NOT_PERMITTED",
            ErrorCode::WriteNotPermitted => "WRITE_NOT_PERMITTED",
            ErrorCode::RowPurged => "ROW_PURGED",
            ErrorCode::PurgeInProgress => "PURGE_IN_PROGRESS",
            ErrorCode::SessionLimit => "SESSION_LIMIT",
            ErrorCode::UnknownTable => "UNKNOWN_TABLE",
            ErrorCode::UnknownColumn => "UNKNOWN_COLUMN",
            ErrorCode::RangeViolation => "RANGE_VIOLATION",
            ErrorCode::KeyConflict => "KEY_CONFLICT",
            ErrorCode::ProgressStale => "PROGRESS_STALE",
            ErrorCode::IdentifierReused => "IDENTIFIER_REUSED",
            ErrorCode::OverlappingRanges => "OVERLAPPING_RANGES",
            ErrorCode::RangeImmutable => "RANGE_IMMUTABLE",
            ErrorCode::DigestMismatch => "DIGEST_MISMATCH",
        }
    }

    /// How bad this failure is.
    pub const fn severity(self) -> Severity {
        match self {
            ErrorCode::ProtocolUnsupported
            | ErrorCode::CodecMismatch
            | ErrorCode::EpochMismatch
            | ErrorCode::DigestSeedMismatch
            | ErrorCode::SchemaMismatch
            | ErrorCode::VisibilityMismatch
            | ErrorCode::UnknownPeer
            | ErrorCode::IdentityMismatch
            | ErrorCode::BadCredential
            | ErrorCode::StalePeer
            | ErrorCode::PurgeScheduleMismatch
            | ErrorCode::ClockSkew => Severity::Fatal,

            ErrorCode::ColumnNotPermitted
            | ErrorCode::TableNotPermitted
            | ErrorCode::WriteNotPermitted
            | ErrorCode::RowPurged
            | ErrorCode::PurgeInProgress
            | ErrorCode::SessionLimit
            | ErrorCode::UnknownTable
            | ErrorCode::UnknownColumn
            | ErrorCode::RangeViolation
            | ErrorCode::KeyConflict
            | ErrorCode::ProgressStale => Severity::NonFatal,

            ErrorCode::IdentifierReused
            | ErrorCode::OverlappingRanges
            | ErrorCode::RangeImmutable
            | ErrorCode::DigestMismatch => Severity::Local,
        }
    }

    /// Whether this code ends the session.
    pub const fn is_fatal(self) -> bool {
        matches!(self.severity(), Severity::Fatal)
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The crate's error type.
///
/// A [`Error`] is either a protocol failure — which carries the stable
/// [`ErrorCode`] that goes on the wire — or a local fault in something the
/// crate was handed: a malformed timestamp, a malformed UUID, a configuration
/// value that cannot be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// A failure the protocol reports to a peer, identified by its wire code.
    Protocol(ErrorCode),
    /// A configuration value cannot be used.
    Config(String),
    /// A timestamp is not a valid RFC 3339 instant.
    Timestamp(String),
    /// A UUID is not well formed.
    Uuid(String),
}

impl Error {
    /// The wire code, if this is a protocol failure.
    pub fn code(&self) -> Option<ErrorCode> {
        match self {
            Error::Protocol(code) => Some(*code),
            _ => None,
        }
    }

    /// Whether this failure ends the session.
    ///
    /// Only a protocol failure has a severity; a local fault is the crate's own
    /// problem and is never reported to a peer.
    pub fn is_fatal(&self) -> bool {
        self.code().is_some_and(ErrorCode::is_fatal)
    }
}

impl From<ErrorCode> for Error {
    fn from(code: ErrorCode) -> Self {
        Error::Protocol(code)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Protocol(code) => write!(f, "{code}"),
            Error::Config(reason) => write!(f, "invalid configuration: {reason}"),
            Error::Timestamp(input) => write!(f, "not a valid RFC 3339 timestamp: {input:?}"),
            Error::Uuid(input) => write!(f, "not a valid UUID: {input:?}"),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_has_a_distinct_stable_string() {
        let mut seen = std::collections::BTreeSet::new();
        for code in ErrorCode::ALL {
            assert!(
                seen.insert(code.as_str()),
                "duplicate wire string for {code:?}"
            );
            assert_eq!(code.as_str(), code.to_string());
        }
        assert_eq!(seen.len(), ErrorCode::ALL.len());
    }

    #[test]
    fn every_code_is_an_upper_snake_case_identifier() {
        for code in ErrorCode::ALL {
            let text = code.as_str();
            assert!(!text.is_empty());
            assert!(
                text.chars()
                    .all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit()),
                "{text} is not upper snake case"
            );
        }
    }

    #[test]
    fn the_codes_are_grouped_as_the_documentation_groups_them() {
        let fatal = ErrorCode::ALL
            .iter()
            .filter(|c| c.severity() == Severity::Fatal)
            .count();
        let non_fatal = ErrorCode::ALL
            .iter()
            .filter(|c| c.severity() == Severity::NonFatal)
            .count();
        let local = ErrorCode::ALL
            .iter()
            .filter(|c| c.severity() == Severity::Local)
            .count();

        // The three tables in protocol.md § "Error codes".
        assert_eq!(fatal, 12);
        assert_eq!(non_fatal, 11);
        assert_eq!(local, 4);
        assert_eq!(fatal + non_fatal + local, ErrorCode::ALL.len());
    }
}
