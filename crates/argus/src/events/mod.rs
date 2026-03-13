//! Event types and envelope for the Argus supervisor.
//!
//! Every filesystem, process, network, and control action captured by the
//! ptrace supervisor is recorded as an [`Event`] containing a tagged
//! [`EventPayload`]. Events carry dual timestamps for local ordering
//! (`ts_monotonic`) and cross-agent correlation (`ts_wall`), plus an
//! optional vector clock for causal ordering.
//!
//! The [`SequenceGenerator`] provides thread-safe monotonic sequence
//! numbers, and [`timestamp_pair`] produces the dual-timestamp fields.

pub mod control;
pub mod envelope;
pub mod file;
pub mod io;
pub mod network;
pub mod process;
pub mod snapshot;

#[doc(inline)]
pub use control::ApprovalDecision;
#[doc(inline)]
pub use envelope::{Event, EventPayload, SequenceGenerator};
pub(crate) use envelope::timestamp_pair;
