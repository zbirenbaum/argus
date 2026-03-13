// Rust guideline compliant 2026-02-21
//! Pipeline runner: wires all stages and drives the event processing loop.
//!
//! [`PipelineRunner`] owns every pipeline stage and the [`PtraceStream`]
//! source. It runs the main processing loop until the traced process exits,
//! forwarding each stop through classify → rules → approval → capture →
//! tree → stamp → bus.

use futures::StreamExt;

use crate::pipeline::{PtraceStream, RawStopRecorder, RecordBus};

use crate::pipeline::stages::{
    ApprovalStage, CaptureStage, CheckRulesStage, ClassifyStage, StampStage, TreeStage,
};
use crate::pipeline::classified::Classification;
use crate::pipeline::directive::PipelineDirective;

/// Owns all pipeline stages and drives the main processing loop.
///
/// Construct via field initialization, then call [`PipelineRunner::run`].
/// The runner blocks until [`PtraceStream`] signals that the traced process
/// has exited and yields no more stops.
pub struct PipelineRunner {
    /// Source of raw ptrace stops.
    pub ptrace: PtraceStream,
    /// Classifies stops into semantic operations.
    pub classify: ClassifyStage,
    /// Evaluates block and pause-before rules.
    pub rules: CheckRulesStage,
    /// Sends pause-before actions to the operator for approval.
    pub approvals: ApprovalStage,
    /// Reads content from tracee memory and CAS.
    pub capture: CaptureStage,
    /// Applies Merkle tree updates and produces tree hashes.
    pub tree: TreeStage,
    /// Assigns sequence numbers, timestamps, and agent ID.
    pub stamp: StampStage,
    /// Fans completed events out to all registered sinks.
    pub bus: RecordBus,
    /// Optional raw-stop recorder for debugging and replay.
    pub recorder: Option<RawStopRecorder>,
}

impl PipelineRunner {
    /// Run the pipeline until the traced process exits.
    ///
    /// Consumes `self`; the caller should proceed with shutdown after this
    /// returns.
    pub async fn run(mut self) {
        while let Some(stop) = self.ptrace.next().await {
            if let Some(ref mut rec) = self.recorder {
                rec.record(&stop);
            }

            let classified = self.classify.classify(stop).await;

            // Passthrough stops need no further processing; resume immediately
            // to minimize latency on the hot path.
            if matches!(classified.classification, Classification::Passthrough) {
                self.ptrace.directive(PipelineDirective::Resume {
                    pid: classified.pid,
                });
                continue;
            }

            if let Some(rule_match) = self.rules.check_block(&classified) {
                self.ptrace.directive(PipelineDirective::InjectError {
                    pid: classified.pid,
                    // EPERM is the standard error for policy-blocked operations;
                    // matches what seccomp SECCOMP_RET_ERRNO would return.
                    errno: libc::EPERM,
                });
                let blocked = self.stamp.stamp_blocked(
                    classified.pid.as_raw() as u32,
                    classified.syscall_name(),
                    classified.primary_path(),
                    rule_match.description.clone(),
                );
                self.bus.emit(crate::pipeline::Record::Event(blocked));
                continue;
            }

            if self.rules.needs_approval(&classified)
                && !self.approvals.process(&classified)
            {
                // ApprovalStage already sent InjectError for denied requests.
                continue;
            }

            let captured = self.capture.capture(classified).await;
            self.ptrace.directive(PipelineDirective::Resume {
                pid: captured.pid,
            });

            let tree_hash = self.tree.update(&captured);
            if let Some(event) = self.stamp.stamp(captured, tree_hash) {
                self.bus.emit(crate::pipeline::Record::Event(event));
            }
        }
    }
}
