//! The merge extension point.
//!
//! A **merge policy** decides how two versions of one row combine. For most
//! tables the answer is the default — last write wins on
//! `(last_update, digest)` — and the crate provides it. An application that
//! needs something else implements [`MergePolicyFn`] and registers it per
//! table.
//!
//! Two rules govern a custom policy, and they are not advice:
//!
//! 1. **A custom policy must be pure and convergent.** The same
//!    `(incoming, local)` pair must produce the same decision on every peer.
//!    A policy that consults the wall clock or a node-local counter is a
//!    divergence generator, and the crate cannot detect it.
//! 2. **Batch repair runs on the sender.** A repair applied on the receiving
//!    side changes the stored bytes, and therefore the digest — so two peers
//!    would compute different digests for the same logical row.
//!
//! This is the one extension point that can break the core guarantee, which
//! is why the rules are stated here rather than left to the policy author.
//!
//! **Status: the shapes only.** The rules — the default LWW comparison, the
//! `Undecided` fallback, and the write-authority evaluation — are the work of
//! stage M4. See [plan/model.md](../../../plan/model.md).

use crate::config::{Role, Uuid};
use crate::error::Error;
use crate::value::Value;
use serde::{Deserialize, Serialize};

/// A row, as the merge rules see it.
///
/// The values are the row's **replicated** columns, in registration order —
/// the same order the digest concatenates them in. Local columns are not
/// here: they are excluded from the digest, and a merge decision must not
/// depend on them, or two peers holding the same entity with different local
/// values would decide differently.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Row {
    /// The canonical key bytes.
    pub key: Vec<u8>,
    /// The row's digest.
    pub digest: [u8; 16],
    /// The row's timestamp, in Unix microseconds.
    pub last_update: i64,
    /// The replicated column values, in registration order.
    pub values: Vec<Value>,
}

/// A row that arrived from a peer.
///
/// The same shape as a [`Row`], and deliberately so: what distinguishes an
/// incoming row from a stored one is which side of the comparison it is on,
/// not what it contains. The aliases are here so that a policy's signature
/// says which is which.
pub type IncomingRow = Row;

/// A row as it is stored locally.
pub type StoredRow = Row;

/// Why a row was refused.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RejectReason {
    /// The row's origin may not write that table.
    WriteNotPermitted,
    /// The row's identifier is outside its owner's range.
    RangeViolation,
    /// Anything else, with a reason.
    Other(String),
}

impl std::fmt::Display for RejectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RejectReason::WriteNotPermitted => f.write_str("the origin may not write this table"),
            RejectReason::RangeViolation => f.write_str("the identifier is outside its range"),
            RejectReason::Other(reason) => f.write_str(reason),
        }
    }
}

/// What a merge policy decided.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MergeDecision {
    /// Take the incoming row wholesale.
    TakeIncoming,
    /// Keep the local row.
    KeepLocal,
    /// A row synthesised from both — a field-wise merge, say.
    Merged(Row),
    /// The policy cannot decide this pair, and the default LWW plus digest
    /// tie-break applies.
    ///
    /// This is the escape hatch for a policy that meets a case it does not
    /// understand. Without it, a policy author has to invent a decision for
    /// every case, and inventing one is how a custom policy diverges.
    ///
    /// The fallback covers a policy that **cannot decide**. It does not cover
    /// one that decides wrongly: a non-convergent policy returns a definite
    /// decision, so the tie-break never fires and the crate cannot detect it.
    Undecided,
    /// Refuse the row, with a reason.
    Reject(RejectReason),
}

/// What a merge policy is told about the comparison it is making.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeContext<'a> {
    /// The table the row belongs to.
    pub table: &'a str,
    /// The node that **wrote** the row, not the node that sent it.
    pub origin_node: Uuid,
    /// The role of that node.
    pub origin_role: Role,
    /// The site of that node.
    pub origin_site: &'a str,
    /// This node's role.
    pub local_role: Role,
    /// This node's site.
    pub local_site: &'a str,
}

/// The extension point an application implements.
///
/// The trait lives in `merkle-lww-model` rather than in the engine so that
/// implementing it does not imply a SQLite dependency: a merge policy is a
/// pure function over two rows, and it should be as easy to write — and as
/// easy to test — as any other pure function.
pub trait MergePolicyFn: Send + Sync {
    /// Decide what to do with an incoming row.
    ///
    /// `local` is `None` when there is no local row to compare against.
    fn merge(
        &self,
        ctx: &MergeContext<'_>,
        incoming: &IncomingRow,
        local: Option<&StoredRow>,
    ) -> MergeDecision;

    /// Repair a batch before it is replicated. Runs on the **sender**.
    ///
    /// The default does nothing, which is the right answer for every policy
    /// that does not need it.
    fn prepare(&self, _ctx: &MergeContext<'_>, _batch: &mut Vec<Row>) -> Result<(), Error> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_decision_round_trips() {
        let decisions = [
            MergeDecision::TakeIncoming,
            MergeDecision::KeepLocal,
            MergeDecision::Undecided,
            MergeDecision::Reject(RejectReason::WriteNotPermitted),
        ];
        for decision in decisions {
            let json = serde_json::to_string(&decision).unwrap();
            let back: MergeDecision = serde_json::from_str(&json).unwrap();
            assert_eq!(back, decision);
        }
    }
}
