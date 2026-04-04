// Rust guideline compliant 2026-02-21
//! Pipeline runner: pure stream combinator chain.
//!
//! The ptrace event stream is processed through two composed stages:
//!
//! 1. **Core pipeline** (`unfold`): ptrace → record → pause → classify →
//!    policy → capture → tree → stamp. Yields `Event` values.
//!
//! 2. **Output pipeline** (`fold`): redact → outputs → bus (with retry).
//!    Consumes events, threads output state by value.
//!
//! No `Arc<Mutex<>>` anywhere. `unfold` owns all core state; `fold`
//! owns all output state. Natural backpressure: if the output stage
//! blocks (retry loop), `unfold` stops polling the ptrace stream.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use futures::StreamExt;

use tracing::event;
use tracing::Level;

use crate::api::state::SharedState;
use crate::cas::ContentHash;
use crate::pipeline::{EmitResult, PtraceStream, RawStopRecorder, RecordBus, Record};
use crate::pipeline::outputs::OutputList;
use crate::pipeline::stall::StallState;
use crate::pipeline::stages::{CaptureStage, ClassifyStage, PolicyGate, StampStage, TreeStage};
use crate::pipeline::stages::policy_gate::PolicyOutcome;
use crate::pipeline::stages::redact::RedactStage;

/// Core pipeline state threaded through `unfold`.
struct CoreState {
    ptrace: PtraceStream,
    classify: ClassifyStage,
    policy_gate: PolicyGate,
    capture: CaptureStage,
    tree: TreeStage,
    stamp: StampStage,
    recorder: Option<RawStopRecorder>,
    paused: Arc<AtomicBool>,
    shared: SharedState,
    /// Tracks time since last tree mutation for idle-timeout finalization.
    last_tree_mutation: Option<Instant>,
    /// How long to wait with no mutations before flushing the tree.
    tree_idle_flush: Duration,
    /// Sequence number of the most recently stamped event.
    last_seq: u64,
    /// Mutations since last CAS persist.
    dirty_since_persist: u64,
    /// How many mutations between CAS persists.
    persist_batch_size: u64,
    /// CAS root hash from the most recent persist — what MerkleTree::load expects.
    last_cas_hash: Option<ContentHash>,
    /// When the last browsable snapshot was recorded.
    last_snapshot_time: Instant,
    /// Tree mutations since the last browsable snapshot.
    changes_since_snapshot: u64,
    /// Time between automatic browsable snapshots (0 = disabled).
    snapshot_interval: Duration,
    /// Mutations between automatic browsable snapshots (0 = disabled).
    snapshot_change_threshold: u64,
}

impl CoreState {
    async fn wait_if_paused(&self) {
        while self.paused.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Persist the tree to CAS and store in SharedState.
    fn flush_tree(&mut self) {
        if self.tree.file_count() == 0 {
            return;
        }
        if let Ok(cas_hash) = self.tree.persist() {
            self.last_cas_hash = Some(cas_hash);
            self.shared.insert_tree_hash(self.last_seq, cas_hash.to_string());
            self.shared.store_tree(self.tree.snapshot());
            self.dirty_since_persist = 0;
            event!(
                name: "pipeline.tree.flushed",
                Level::DEBUG,
                cas_hash = %cas_hash,
                last_seq = self.last_seq,
                "tree flushed to CAS and shared state",
            );
        }
    }

    /// Check if idle-timeout has elapsed and flush if so.
    fn maybe_idle_flush(&mut self) {
        if let Some(last) = self.last_tree_mutation {
            if last.elapsed() >= self.tree_idle_flush {
                self.flush_tree();
                self.last_tree_mutation = None;
            }
        }
    }

    /// Record a browsable snapshot if a time or change threshold is met.
    fn maybe_take_snapshot(&mut self) {
        if self.changes_since_snapshot == 0 {
            return;
        }

        let time_trigger = self.snapshot_interval > Duration::ZERO
            && self.last_snapshot_time.elapsed() >= self.snapshot_interval;
        let change_trigger = self.snapshot_change_threshold > 0
            && self.changes_since_snapshot >= self.snapshot_change_threshold;

        if !time_trigger && !change_trigger {
            return;
        }

        // Ensure tree is persisted to CAS before recording the snapshot.
        if self.dirty_since_persist > 0 {
            self.flush_tree();
        }

        if let Some(ref cas_hash) = self.last_cas_hash {
            let entry = crate::api::types::SnapshotEntry {
                seq: self.last_seq,
                ts_wall: chrono::Utc::now().to_rfc3339(),
                tree_hash: cas_hash.to_string(),
                file_count: self.tree.file_count(),
            };
            event!(
                name: "pipeline.snapshot.recorded",
                Level::INFO,
                snapshot.seq = entry.seq,
                snapshot.file_count = entry.file_count,
                "browsable snapshot recorded at seq={{snapshot.seq}}",
            );
            self.shared.push_snapshot(entry);
            self.changes_since_snapshot = 0;
            self.last_snapshot_time = Instant::now();
        }
    }
}

/// Output pipeline state threaded through `fold`.
struct OutputState {
    redact: RedactStage,
    outputs: OutputList,
    bus: RecordBus,
    shared: SharedState,
}

impl OutputState {
    /// Emit with exponential-backoff retry for required sinks.
    ///
    /// The tracee stays frozen for the entire retry duration because
    /// `unfold` won't produce the next event until `fold` returns.
    async fn emit_required(&self, record: Record) {
        let mut backoff = Duration::from_secs(1);
        const MAX_BACKOFF: Duration = Duration::from_secs(60);
        let mut retry_count: u32 = 0;
        let stall_start = Instant::now();

        loop {
            match self.bus.emit(record.clone()) {
                EmitResult::Ok => {
                    if retry_count > 0 {
                        self.shared.clear_stall();
                        event!(
                            name: "pipeline.ptrace.stall_recovered",
                            Level::INFO,
                            retry_count,
                            stall_duration_ms = stall_start.elapsed().as_millis() as u64,
                            "required sinks recovered, resuming tracee",
                        );
                    }
                    break;
                }
                EmitResult::RequiredFailed(failures) => {
                    retry_count += 1;
                    let sink_names: Vec<String> =
                        failures.iter().map(|(n, _)| n.clone()).collect();
                    self.shared.set_stall(StallState {
                        failed_sinks: sink_names.clone(),
                        since: stall_start,
                        retry_count,
                    });
                    event!(
                        name: "pipeline.ptrace.sink_stall",
                        Level::WARN,
                        ?sink_names,
                        retry_count,
                        backoff_ms = backoff.as_millis() as u64,
                        "required sinks failed, tracee frozen, retrying",
                    );
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
        }
    }
}

/// Owns all pipeline stages and drives the stream combinator chain.
///
/// Construct via [`PipelineRunner::new`], then call [`PipelineRunner::run`].
pub struct PipelineRunner {
    ptrace: PtraceStream,
    classify: ClassifyStage,
    policy_gate: PolicyGate,
    capture: CaptureStage,
    tree: TreeStage,
    stamp: StampStage,
    bus: RecordBus,
    outputs: OutputList,
    redact: RedactStage,
    recorder: Option<RawStopRecorder>,
    paused: Arc<AtomicBool>,
    shared: SharedState,
    persist_batch_size: u64,
    snapshot_interval: Duration,
    snapshot_change_threshold: u64,
}

/// Default idle timeout before tree flush — 5 seconds with no mutations.
const TREE_IDLE_FLUSH: Duration = Duration::from_secs(5);

impl PipelineRunner {
    /// Construct a new pipeline runner with the given stages and bus.
    #[expect(clippy::too_many_arguments, reason = "pipeline wiring requires many components")]
    pub(crate) fn new(
        ptrace: PtraceStream,
        classify: ClassifyStage,
        policy_gate: PolicyGate,
        capture: CaptureStage,
        tree: TreeStage,
        stamp: StampStage,
        bus: RecordBus,
        outputs: OutputList,
        redact: RedactStage,
        recorder: Option<RawStopRecorder>,
        paused: Arc<AtomicBool>,
        shared: SharedState,
        persist_batch_size: u64,
        snapshot_interval: Duration,
        snapshot_change_threshold: u64,
    ) -> Self {
        Self {
            ptrace, classify, policy_gate, capture,
            tree, stamp, bus, outputs, redact, recorder, paused, shared,
            persist_batch_size, snapshot_interval, snapshot_change_threshold,
        }
    }

    /// Run the pipeline until the traced process exits.
    ///
    /// Consumes `self`. The pipeline is two composed stream stages:
    ///
    /// - `unfold` threads `CoreState` through each iteration, yielding
    ///   `Event` values. When the ptrace stream ends, `CoreState` is
    ///   returned via a `oneshot` channel so the shutdown path can
    ///   finalize the tree.
    ///
    /// - `fold` threads `OutputState` through each event, applying
    ///   redact → outputs → bus with retry. Natural backpressure:
    ///   when `fold` blocks on retry, `unfold` stops polling ptrace.
    pub async fn run(self) {
        event!(name: "pipeline.ptrace.started", Level::INFO, "ptrace pipeline running");

        // Channel to recover CoreState after the stream ends so we can
        // finalize the tree. unfold drops its state when it returns None,
        // so we send it out just before.
        let (core_tx, core_rx) = tokio::sync::oneshot::channel::<CoreState>();

        let core = CoreState {
            ptrace: self.ptrace,
            classify: self.classify,
            policy_gate: self.policy_gate,
            capture: self.capture,
            tree: self.tree,
            stamp: self.stamp,
            recorder: self.recorder,
            paused: self.paused,
            shared: self.shared.clone(),
            last_tree_mutation: None,
            tree_idle_flush: TREE_IDLE_FLUSH,
            last_seq: 0,
            dirty_since_persist: 0,
            persist_batch_size: self.persist_batch_size,
            last_cas_hash: None,
            last_snapshot_time: Instant::now(),
            changes_since_snapshot: 0,
            snapshot_interval: self.snapshot_interval,
            snapshot_change_threshold: self.snapshot_change_threshold,
        };

        // Wrap the oneshot in Option so the closure can take() it once.
        let core_tx = Some(core_tx);

        // ── Core pipeline: unfold produces Event stream ──
        let events = futures::stream::unfold((core, core_tx), |(mut s, mut tx)| async move {
            loop {
                // Compute the minimum timeout across all pending timers:
                // idle flush (5s after last mutation) and snapshot interval.
                let mut timeout = Duration::MAX;
                if let Some(last) = s.last_tree_mutation {
                    timeout = timeout.min(s.tree_idle_flush.saturating_sub(last.elapsed()));
                }
                if s.changes_since_snapshot > 0 && s.snapshot_interval > Duration::ZERO {
                    timeout = timeout.min(
                        s.snapshot_interval.saturating_sub(s.last_snapshot_time.elapsed()),
                    );
                }

                let stop = if timeout < Duration::MAX {
                    match tokio::time::timeout(timeout, s.ptrace.next()).await {
                        Ok(Some(stop)) => stop,
                        Ok(None) => {
                            if let Some(tx) = tx.take() { let _ = tx.send(s); }
                            return None;
                        }
                        Err(_) => {
                            // Timer fired — run both checks; each is a no-op
                            // when its own condition is not met.
                            s.maybe_idle_flush();
                            s.maybe_take_snapshot();
                            continue;
                        }
                    }
                } else {
                    match s.ptrace.next().await {
                        Some(stop) => stop,
                        None => {
                            if let Some(tx) = tx.take() { let _ = tx.send(s); }
                            return None;
                        }
                    }
                };

                if let Some(ref mut rec) = s.recorder {
                    rec.record(&stop);
                }

                s.wait_if_paused().await;

                let Some(classified) = s.classify.process(stop).await else {
                    continue;
                };

                match s.policy_gate.evaluate(classified).await {
                    PolicyOutcome::Blocked { pid, syscall, path, reason } => {
                        let evt = s.stamp.stamp_blocked(pid, syscall, path, reason);
                        return Some((evt, (s, tx)));
                    }
                    PolicyOutcome::Approved(ce) => {
                        let captured = s.capture.process(ce).await;
                        let (captured, tree_hash) = s.tree.process(captured);

                        // Track mutation time for idle-flush of the snapshot.
                        if tree_hash.is_some() {
                            s.last_tree_mutation = Some(Instant::now());
                            s.changes_since_snapshot += 1;
                        }

                        // Persist to CAS + update SharedState at batch
                        // boundaries. persist() stores TreeObjects to CAS
                        // so restore can load them. snapshot() clones the
                        // BTreeMap — O(file_count), batched to amortize.
                        s.dirty_since_persist += u64::from(tree_hash.is_some());
                        if s.dirty_since_persist >= s.persist_batch_size {
                            if let Ok(cas_hash) = s.tree.persist() {
                                s.shared.store_tree(s.tree.snapshot());
                                // CAS hash is what MerkleTree::load expects.
                                s.last_cas_hash = Some(cas_hash);
                            }
                            s.dirty_since_persist = 0;
                            s.last_tree_mutation = None;
                        }

                        // Check if change-count threshold triggers a snapshot.
                        s.maybe_take_snapshot();

                        if let Some(evt) = s.stamp.stamp(captured, tree_hash) {
                            s.last_seq = evt.seq;
                            // Map seq to the CAS root hash so /restore
                            // can load the tree via MerkleTree::load.
                            if let Some(ref cas_hash) = s.last_cas_hash {
                                s.shared.insert_tree_hash(evt.seq, cas_hash.to_string());
                            }
                            return Some((evt, (s, tx)));
                        }
                        continue;
                    }
                }
            }
        });

        // ── Output pipeline: fold consumes Event stream ──
        let output = OutputState {
            redact: self.redact,
            outputs: self.outputs,
            bus: self.bus,
            shared: self.shared,
        };

        let mut out = events
            .fold(output, |mut out, mut evt| async move {
                event!(
                    name: "pipeline.ptrace.emitted",
                    Level::DEBUG,
                    event.seq = evt.seq,
                    event.type_ = evt.payload.event_type_tag(),
                    "event emitted to outputs and bus",
                );
                out.redact.redact(&mut evt);
                out.outputs.emit(&evt);
                out.shared.broadcast(&evt);
                out.emit_required(Record::Event(evt)).await;
                out
            })
            .await;

        // ── Shutdown: finalize tree and flush ──
        if let Ok(mut core) = core_rx.await {
            core.flush_tree();
        }

        event!(name: "pipeline.ptrace.stopped", Level::INFO, "ptrace pipeline finished, shutting down outputs and bus");
        let _ = out.outputs.shutdown();
        out.bus.shutdown_all();
    }
}
