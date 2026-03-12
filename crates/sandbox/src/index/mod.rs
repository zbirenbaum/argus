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

pub mod path_index;
pub mod pid_index;
pub mod query;
pub mod type_index;

/// Sequence number paired with the event type tag.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct IndexEntry {
    /// Monotonic event sequence number.
    pub seq: u64,
    /// Serde discriminator tag (e.g. `"write"`, `"unlink"`).
    pub event_type: String,
}

#[doc(inline)]
pub use path_index::PathIndex;
#[doc(inline)]
pub use pid_index::{PidIndex, ProcessInfo};
#[doc(inline)]
pub use query::{QueryEngine, QueryFilter, QueryResult};
#[doc(inline)]
pub use type_index::TypeIndex;

// Rust guideline compliant 2026-02-21
