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

#[doc(inline)]
pub use path_index::PathIndex;
#[doc(inline)]
pub use pid_index::{PidIndex, ProcessInfo};
#[doc(inline)]
pub use query::{QueryEngine, QueryFilter, QueryResult};
#[doc(inline)]
pub use type_index::TypeIndex;

// Rust guideline compliant 2026-02-21
