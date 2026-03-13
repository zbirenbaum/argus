// Rust guideline compliant 2026-02-21
//! Pipeline runner: wires all stages and drives the event processing loop.
//!
//! [`PipelineRunner`] owns every pipeline stage and the [`PtraceStream`]
//! source. It runs the main processing loop until the traced process exits,
//! forwarding each stop through classify → rules → approval → capture →
//! tree → stamp → redact → outputs (user-facing) + bus (internal sinks).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::StreamExt;

use tracing::event;
use tracing::Level;

use crate::api::routes::submit_pending_approval;
use crate::api::state::SharedState;
use crate::events::{ApprovalDecision, EventPayload};
use crate::events::control;
use crate::pipeline::{PtraceStream, RawStopRecorder, RecordBus};
use crate::pipeline::classified::Classification;
use crate::pipeline::directive::PipelineDirective;
use crate::pipeline::outputs::OutputList;
use crate::pipeline::raw_stop::StopType;
use crate::pipeline::stages::{
    ApprovalStage, CaptureStage, CheckRulesStage, ClassifyStage, StampStage, TreeStage,
};
use crate::pipeline::stages::check_rules::RuleAction;
use crate::pipeline::stages::redact::RedactStage;

/// Owns all pipeline stages and drives the main processing loop.
///
/// Construct via [`PipelineRunner::new`], then call [`PipelineRunner::run`].
/// The runner blocks until [`PtraceStream`] signals that the traced process
/// has exited and yields no more stops.
pub struct PipelineRunner {
    ptrace: PtraceStream,
    classify: ClassifyStage,
    rules: CheckRulesStage,
    approvals: ApprovalStage,
    capture: CaptureStage,
    tree: TreeStage,
    stamp: StampStage,
    bus: RecordBus,
    outputs: OutputList,
    redact: RedactStage,
    recorder: Option<RawStopRecorder>,
    paused: Arc<AtomicBool>,
    shared: SharedState,
}

impl PipelineRunner {
    /// Construct a new pipeline runner with the given stages and bus.
    ///
    /// Called by `SupervisorRuntime::into_pipeline`; not part of the
    /// public API.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        ptrace: PtraceStream,
        classify: ClassifyStage,
        rules: CheckRulesStage,
        approvals: ApprovalStage,
        capture: CaptureStage,
        tree: TreeStage,
        stamp: StampStage,
        bus: RecordBus,
        outputs: OutputList,
        redact: RedactStage,
        recorder: Option<RawStopRecorder>,
        paused: Arc<AtomicBool>,
        shared: SharedState,
    ) -> Self {
        Self {
            ptrace, classify, rules, approvals, capture,
            tree, stamp, bus, outputs, redact, recorder, paused, shared,
        }
    }

    /// Run the pipeline until the traced process exits.
    ///
    /// Consumes `self`; the caller should proceed with shutdown after this
    /// returns.
    pub async fn run(mut self) {
        event!(name: "pipeline.ptrace.started", Level::INFO, "ptrace pipeline running");
        while let Some(stop) = self.ptrace.next().await {
            if let Some(ref mut rec) = self.recorder {
                rec.record(&stop);
            }

            // While paused, don't deliver any directives — the tracee
            // stays frozen because the ptrace thread is waiting for a
            // Resume/InjectError that we withhold.
            self.wait_if_paused().await;

            let classified = self.classify.classify(stop).await;
            let pid_raw = classified.pid.as_raw();
            let cls_name = classified.syscall_name();
            event!(
                name: "pipeline.ptrace.classified",
                Level::DEBUG,
                pid = pid_raw,
                classification = cls_name.as_str(),
                "classified syscall stop",
            );

            // Passthrough stops need no further processing; resume immediately
            // to minimize latency on the hot path. Use ptrace::syscall only
            // when a pending entry exists (openat, dup, socket, pipe) so the
            // exit stop is delivered for fd-table correlation. Otherwise use
            // ptrace::cont to avoid per-syscall overhead on all threads.
            if matches!(classified.classification, Classification::Passthrough) {
                let trace_exit = self.classify.pending.contains_key(&classified.pid);

                // Re-inject pending signals so the tracee actually receives
                // them (e.g. SIGCHLD for child-process notification).
                let signal = match &classified.raw.stop_type {
                    StopType::Signal { signal, .. } => {
                        nix::sys::signal::Signal::try_from(*signal).ok()
                    }
                    _ => None,
                };

                event!(
                    name: "pipeline.ptrace.passthrough",
                    Level::TRACE,
                    pid = pid_raw,
                    ?signal,
                    "passthrough, resuming immediately",
                );
                self.ptrace.directive(PipelineDirective::Resume {
                    pid: classified.pid,
                    trace_exit,
                    signal,
                });
                continue;
            }

            if let Some(rule_match) = self.rules.check_block(&classified) {
                match rule_match.action {
                    RuleAction::Block => {
                        self.ptrace.directive(PipelineDirective::InjectError {
                            pid: classified.pid,
                            errno: libc::EPERM,
                        });
                        let mut blocked = self.stamp.stamp_blocked(
                            classified.pid.as_raw() as u32,
                            classified.syscall_name(),
                            classified.primary_path(),
                            // description is the last use of rule_match; move instead of clone
                            rule_match.description,
                        );
                        self.redact.redact(&mut blocked);
                        self.outputs.emit(&blocked);
                        self.bus.emit(crate::pipeline::Record::Event(blocked));
                        continue;
                    }
                    RuleAction::Pause => {
                        let pid_raw = classified.pid.as_raw() as u32;
                        let syscall = classified.syscall_name();
                        let path = classified.primary_path();

                        // Emit the WebSocket notification first (clones needed here
                        // because submit_pending_approval also needs these values).
                        self.shared.emit(EventPayload::PendingApproval(
                            control::PendingApproval {
                                pid: pid_raw,
                                syscall: syscall.clone(),
                                path: path.clone(),
                                binary: None,
                                rule_name: rule_match.description.clone(),
                            },
                        ));

                        // Move syscall, path, and description into the last consumer
                        // to avoid three extra allocations on every pause-before-action.
                        let (_action_id, rx) = submit_pending_approval(
                            &self.shared,
                            pid_raw,
                            format!("pid:{pid_raw}"),
                            syscall,
                            path,
                            rule_match.description,
                        );

                        // Block until the API delivers a decision.
                        // The API handler emits ApprovalGranted/ApprovalDenied
                        // events, so the runner only needs to act on the verdict.
                        let decision = rx.await.unwrap_or(ApprovalDecision::Deny);

                        if decision == ApprovalDecision::Deny {
                            self.ptrace.directive(PipelineDirective::InjectError {
                                pid: classified.pid,
                                errno: libc::EPERM,
                            });
                            continue;
                        }
                        // Approved — fall through to capture/tree/stamp.
                    }
                }
            }

            let captured = self.capture.capture(classified).await;
            let has_content = !matches!(captured.content, crate::pipeline::captured::CapturedContent::None);
            event!(
                name: "pipeline.ptrace.captured",
                Level::DEBUG,
                pid = captured.pid.as_raw(),
                has_content,
                "content capture complete",
            );
            self.ptrace.directive(PipelineDirective::Resume {
                pid: captured.pid,
                trace_exit: false,
                signal: None,
            });

            let tree_hash = self.tree.update(&captured);
            let has_tree_hash = tree_hash.is_some();
            event!(
                name: "pipeline.ptrace.tree_updated",
                Level::DEBUG,
                pid = captured.pid.as_raw(),
                has_tree_hash,
                "tree stage complete",
            );

            // Sync tree snapshot to SharedState and persist to CAS so
            // the /tree and /restore endpoints can serve it.
            let cas_tree_hash = {
                let snapshot = self.tree.tree().lock().unwrap();
                // store() borrows snapshot; store_tree() needs an owned Arc, so one
                // deep clone is unavoidable here given that TreeStage holds Mutex<MerkleTree>
                // rather than Arc<Mutex<MerkleTree>>. Wrap in Arc immediately to make
                // the ownership intent explicit.
                self.shared.store_tree(Arc::new(snapshot.clone()));
                snapshot.store(self.shared.cas().as_ref()).ok()
            };

            if let Some(mut evt) = self.stamp.stamp(captured, tree_hash) {
                event!(
                    name: "pipeline.ptrace.emitted",
                    Level::DEBUG,
                    event.seq = evt.seq,
                    event.type_ = evt.payload.event_type_tag(),
                    "event emitted to bus and outputs",
                );
                // Use the CAS storage hash (not root_hash) for restore
                // lookups since MerkleTree::load expects the CAS hash.
                if let Some(ref th) = cas_tree_hash {
                    self.shared.insert_tree_hash(evt.seq, th.to_string());
                }
                // Apply redaction before delivering to user-facing outputs.
                self.redact.redact(&mut evt);
                // Enriched user-facing output (stdout, file, etc.).
                self.outputs.emit(&evt);
                // Internal sinks (event log, index, broadcast).
                self.bus.emit(crate::pipeline::Record::Event(evt));
            }
        }

        event!(name: "pipeline.ptrace.stopped", Level::INFO, "ptrace pipeline finished, shutting down outputs and bus");
        // Flush enriched outputs first, then internal sinks. The runtime hands
        // ownership of both to the runner via into_pipeline, so shutdown must
        // happen here rather than in the caller.
        let _ = self.outputs.shutdown();
        self.bus.shutdown_all();
    }

    /// Spin-wait while the pause flag is set, yielding to the runtime.
    async fn wait_if_paused(&self) {
        while self.paused.load(Ordering::Acquire) {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
}
