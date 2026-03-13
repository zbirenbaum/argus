//! Path, PID, and type indexes with a combined query engine.
//!
//! Each index is an in-memory [`BTreeMap`] backed by append-only disk files.
//! Indexes are updated synchronously on each event write and rebuilt from
//! the event log segments on restart. They are local-only and never
//! archived to S3.
//!
//! The [`QueryEngine`] accepts filter parameters (path, pid, event type,
//! time range, sequence range, limit) and intersects results from the
//! individual indexes.

pub(crate) mod path_index;
pub(crate) mod pid_index;
pub(crate) mod query;
pub(crate) mod type_index;

/// Sequence number paired with the event type tag.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct IndexEntry {
    /// Monotonic event sequence number.
    pub(crate) seq: u64,
    /// Serde discriminator tag (e.g. `"write"`, `"unlink"`).
    pub(crate) event_type: String,
}

pub(crate) use path_index::PathIndex;
pub(crate) use pid_index::PidIndex;
pub(crate) use type_index::TypeIndex;

// Rust guideline compliant 2026-02-21
