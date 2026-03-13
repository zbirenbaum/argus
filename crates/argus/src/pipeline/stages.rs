// Rust guideline compliant 2026-02-21
//! Pipeline processing stages.
//!
//! Each stage takes a stop from the previous stage, performs one
//! focused transformation, and passes it on. Stages are constructed in
//! `main.rs` and wired together by [`PipelineRunner`].
//!
//! All types in this module are placeholders; parallel agents provide
//! the concrete implementations. The names and signatures are fixed so
//! `main.rs` and `runner.rs` compile against this scaffold.

use std::sync::Arc;

use arc_swap::ArcSwap;

use crate::api::state::SharedState;
use crate::cas::LocalCas;
use crate::config::{ProxyMode, RuleSet};
use crate::events::Event;
use crate::pipeline::classified::{CapturedStop, ClassifiedStop, RawStop};
use crate::pipeline::RecordBus;

/// Classifies raw ptrace stops into semantic operations.
///
/// Placeholder — the real implementation is provided by the classify-stage
/// agent.
// TODO: replace with real `ClassifyStage` once classify-stage agent merges.
pub struct ClassifyStage;

impl ClassifyStage {
    /// Construct the classify stage.
    pub fn new(
        _shared: SharedState,
        _proxy_mode: ProxyMode,
        _mitm_port: u16,
    ) -> Self {
        Self
    }

    /// Classify a raw stop.
    pub async fn classify(&self, stop: RawStop) -> ClassifiedStop {
        ClassifiedStop {
            pid: stop.pid,
            classification: crate::pipeline::classified::Classification::Passthrough,
        }
    }
}

/// Evaluates block and pause-before rules against classified stops.
///
/// Placeholder — the real implementation is provided by the rules-stage agent.
// TODO: replace with real `CheckRulesStage` once rules-stage agent merges.
pub struct CheckRulesStage;

impl CheckRulesStage {
    /// Construct the rules check stage from the shared rules handle.
    pub fn new(_rules: Arc<ArcSwap<RuleSet>>) -> Self {
        Self
    }

    /// Returns `Some(rule)` if the stop should be blocked.
    pub fn check_block(&self, _stop: &ClassifiedStop) -> Option<String> {
        None
    }

    /// Returns `true` if the stop requires operator approval.
    pub fn needs_approval(&self, _stop: &ClassifiedStop) -> bool {
        false
    }
}

/// Forwards pause-before stops to the operator and waits for a decision.
///
/// Placeholder — the real implementation is provided by the approval-stage
/// agent.
// TODO: replace with real `ApprovalStage` once approval-stage agent merges.
pub struct ApprovalStage;

impl ApprovalStage {
    /// Construct the approval stage.
    pub fn new(_shared: SharedState) -> Self {
        Self
    }

    /// Returns `true` if the operation was approved, `false` if denied.
    ///
    /// On denial the stage is responsible for sending `InjectError` to the
    /// ptrace loop before returning.
    pub async fn process(&self, _stop: &ClassifiedStop) -> bool {
        true
    }
}

/// Reads content from tracee memory and stores it in the CAS.
///
/// Placeholder — the real implementation is provided by the capture-stage
/// agent.
// TODO: replace with real `CaptureStage` once capture-stage agent merges.
pub struct CaptureStage;

impl CaptureStage {
    /// Construct the capture stage.
    pub fn new(_cas: Arc<LocalCas>, _bus: RecordBus) -> Self {
        Self
    }

    /// Capture content for the stop and return a `CapturedStop`.
    pub async fn capture(&self, stop: ClassifiedStop) -> CapturedStop {
        CapturedStop { pid: stop.pid }
    }
}

/// Updates the Merkle tree and produces a tree hash for each mutating stop.
///
/// Placeholder — the real implementation is provided by the tree-stage agent.
// TODO: replace with real `TreeStage` once tree-stage agent merges.
pub struct TreeStage;

impl TreeStage {
    /// Construct the tree stage.
    pub fn new(_shared: SharedState) -> Self {
        Self
    }

    /// Update the Merkle tree and return the new root hash.
    pub fn update(&self, _stop: &CapturedStop) -> String {
        String::new()
    }
}

/// Assigns sequence numbers, timestamps, and agent ID to completed stops.
///
/// Placeholder — the real implementation is provided by the stamp-stage agent.
// TODO: replace with real `StampStage` once stamp-stage agent merges.
pub struct StampStage {
    seq_gen: crate::events::SequenceGenerator,
    agent_id: String,
}

impl StampStage {
    /// Construct the stamp stage.
    pub fn new(seq_gen: crate::events::SequenceGenerator, agent_id: String) -> Self {
        Self { seq_gen, agent_id }
    }

    /// Produce a blocked event for a rule-denied stop.
    pub fn stamp_blocked(&self, stop: &ClassifiedStop) -> Event {
        use crate::events::{EventPayload, SequenceGenerator};
        use crate::events::process::Exit;
        // Placeholder event — real implementation emits a BlockedSyscall event.
        Event::new(
            &self.seq_gen,
            self.agent_id.clone(),
            EventPayload::Exit(Exit {
                pid: stop.pid.as_raw() as u32,
                exit_code: 0,
                signal: None,
            }),
        )
    }

    /// Stamp a captured stop into a completed event.
    pub fn stamp(&self, stop: CapturedStop, _tree_hash: String) -> Event {
        use crate::events::{EventPayload, SequenceGenerator};
        use crate::events::process::Exit;
        // Placeholder — real implementation emits the appropriate event type.
        Event::new(
            &self.seq_gen,
            self.agent_id.clone(),
            EventPayload::Exit(Exit {
                pid: stop.pid.as_raw() as u32,
                exit_code: 0,
                signal: None,
            }),
        )
    }
}
