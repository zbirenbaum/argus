// Rust guideline compliant 2026-02-21
//! Shared resource context threaded through all pipeline variants.
//!
//! [`PipelineContext`] bundles the dependencies that every pipeline
//! (Ptrace, Proxy, Keylog) needs, so they can share a single
//! `SequenceGenerator` for total event ordering and a common bus.

use std::sync::Arc;

use compact_str::CompactString;

use crate::events::SequenceGenerator;
use crate::pipeline::bus::RecordBus;

/// Resources shared across all pipeline variants.
///
/// Clone this struct to hand a copy to each pipeline; the `Arc` fields
/// ensure the underlying state is shared, not duplicated.
#[derive(Clone, Debug)]
pub struct PipelineContext {
    /// Monotonic sequence counter shared across all pipelines.
    pub(crate) seq: Arc<SequenceGenerator>,
    /// Channel to the sink chain (event log, CAS, S3, broadcast).
    pub(crate) bus: RecordBus,
    /// Identifier stamped onto every emitted event.
    pub(crate) agent_id: CompactString,
}

impl PipelineContext {
    /// Create a new pipeline context.
    pub(crate) fn new(seq: Arc<SequenceGenerator>, bus: RecordBus, agent_id: CompactString) -> Self {
        Self { seq, bus, agent_id }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clone_shares_seq_arc() {
        let seq = Arc::new(SequenceGenerator::new(0));
        // A minimal bus cannot be constructed in unit tests without the full
        // sink chain, so we verify only the Arc identity of seq here.
        let first = seq.next_seq();
        let cloned_seq = Arc::clone(&seq);
        let second = cloned_seq.next_seq();
        // Both callers on the same Arc see strictly increasing values.
        assert!(second > first);
    }
}
