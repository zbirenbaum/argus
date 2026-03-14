// Rust guideline compliant 2026-02-21
//! Lock-free bridge between the API server and the tracer thread.
//!
//! The tracer loop runs synchronously on a dedicated OS thread while the
//! API server runs on the tokio runtime. This module provides the
//! thread-safe bridge between them using only atomic and lock-free
//! primitives — no `Mutex` anywhere.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use arc_swap::ArcSwap;
use compact_str::CompactString;
use dashmap::DashMap;
use parking_lot::Mutex as ParkingMutex;
use tokio::sync::broadcast;

use crate::api::types::PendingApprovalEntry;
use crate::cas::Cas;
use crate::config::RuleSet;
use crate::events::{ApprovalDecision, Event, EventPayload, SequenceGenerator};
use crate::pipeline::EmitResult;
use crate::pipeline::RecordBus;
use crate::pipeline::overflow::OverflowQueue;
use crate::pipeline::record::Record;
use crate::pipeline::stall::StallState;
use crate::snapshot::MerkleTree;

/// Broadcast channel capacity for API event subscribers.
///
/// 256 is large enough to buffer bursts without back-pressure on the
/// trace loop. Lagging receivers silently skip missed events.
const EVENT_CHANNEL_CAPACITY: usize = 256;

/// Bridge between the ptrace loop and the API server.
///
/// Most fields are immutable after construction or use lock-free primitives.
/// The stall field uses a `parking_lot::Mutex` because its writes are brief
/// and never held across await points.
pub struct Bridge {
    agent_id: CompactString,
    started_at: Instant,
    paused: Arc<AtomicBool>,
    rules: Arc<ArcSwap<RuleSet>>,
    pending_approvals: DashMap<String, PendingApprovalEntry>,
    seq_gen: SequenceGenerator,
    event_tx: broadcast::Sender<Event>,
    /// Latest Merkle tree snapshot, swapped on every mutating event.
    tree: ArcSwap<MerkleTree>,
    /// CAS backend for content reads and restore operations.
    cas: Arc<dyn Cas>,
    /// Maps event seq → tree_hash for point-in-time restore lookups.
    tree_hashes: DashMap<u64, String>,
    /// Pipeline bus for emitting API-originated events to all sinks.
    bus: RecordBus,
    /// Overflow queue for API-path events that fail required sinks.
    overflow: Option<Arc<OverflowQueue>>,
    /// Current sink stall state, if any required sinks are failing.
    stall: ParkingMutex<Option<StallState>>,
}

impl std::fmt::Debug for Bridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bridge")
            .field("agent_id", &self.agent_id)
            .field("paused", &self.paused.load(Ordering::Relaxed))
            .field("pending_approvals", &self.pending_approvals.len())
            .field("tree_hashes", &self.tree_hashes.len())
            .finish_non_exhaustive()
    }
}

impl Bridge {
    /// Creates a new bridge with the given CAS backend.
    pub fn new(agent_id: CompactString, cas: Arc<dyn Cas>, bus: RecordBus) -> Self {
        Self::with_overflow(agent_id, cas, bus, None)
    }

    /// Creates a new bridge with an optional overflow queue.
    pub(crate) fn with_overflow(
        agent_id: CompactString,
        cas: Arc<dyn Cas>,
        bus: RecordBus,
        overflow: Option<Arc<OverflowQueue>>,
    ) -> Self {
        let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self {
            agent_id,
            started_at: Instant::now(),
            paused: Arc::new(AtomicBool::new(false)),
            rules: Arc::new(ArcSwap::from_pointee(RuleSet::default())),
            pending_approvals: DashMap::new(),
            seq_gen: SequenceGenerator::default(),
            event_tx,
            tree: ArcSwap::from_pointee(MerkleTree::new()),
            cas,
            tree_hashes: DashMap::new(),
            bus,
            overflow,
            stall: ParkingMutex::new(None),
        }
    }

    /// Atomically swap the latest tree snapshot.
    ///
    /// Called by the pipeline runner at batch-size cadence.
    pub fn store_tree(&self, tree: MerkleTree) {
        self.tree.store(Arc::new(tree));
    }

    /// Load the latest tree snapshot.
    pub fn load_tree(&self) -> arc_swap::Guard<Arc<MerkleTree>> {
        self.tree.load()
    }

    /// Record the tree hash for a given event sequence number.
    pub fn insert_tree_hash(&self, seq: u64, tree_hash: String) {
        self.tree_hashes.insert(seq, tree_hash);
    }

    /// Look up the tree hash recorded at a given event sequence.
    pub fn get_tree_hash(&self, seq: u64) -> Option<String> {
        self.tree_hashes.get(&seq).map(|v| v.clone())
    }

    /// Reference to the CAS backend.
    pub fn cas(&self) -> &Arc<dyn Cas> {
        &self.cas
    }

    /// Emits an event to all broadcast subscribers.
    ///
    /// Zero-cost when no receivers are connected — `send` silently drops
    /// the event with no allocation. On required-sink failure the record
    /// is buffered in the overflow queue when one is configured.
    pub fn emit(&self, payload: EventPayload) {
        let evt = Event::new(&self.seq_gen, self.agent_id.clone(), payload);
        let record = Record::Event(evt.clone());
        if let EmitResult::RequiredFailed(failures) = self.bus.emit(record.clone()) {
            if let Some(ref overflow) = self.overflow {
                overflow.push(&record);
            }
            for (sink_name, err) in &failures {
                tracing::event!(
                    name: "pipeline.emit.required_sink_failed",
                    tracing::Level::ERROR,
                    sink.name = sink_name.as_str(),
                    error.message = %err,
                    "required sink failed on API path, buffered in overflow queue",
                );
            }
        }
        let _ = self.event_tx.send(evt);
    }

    /// Broadcast a pipeline event to WebSocket subscribers.
    ///
    /// Unlike `emit`, this does not create a new event or write to the bus.
    /// Used by the pipeline output stage to fan events to WebSocket clients.
    pub fn broadcast(&self, event: &Event) {
        let _ = self.event_tx.send(event.clone());
    }

    /// Subscribes to the event broadcast channel.
    pub fn subscribe_events(&self) -> broadcast::Receiver<Event> {
        self.event_tx.subscribe()
    }

    /// Whether the agent is currently paused.
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Acquire)
    }

    /// Sets the paused flag. Returns `true` if the state changed.
    pub fn set_paused(&self, paused: bool) -> bool {
        self.paused
            .compare_exchange(!paused, paused, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Shared handle to the pause flag for the pipeline runner.
    pub fn pause_flag(&self) -> Arc<AtomicBool> {
        self.paused.clone()
    }

    /// The configured agent identifier.
    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    /// Seconds since the supervisor started.
    pub fn uptime_seconds(&self) -> f64 {
        self.started_at.elapsed().as_secs_f64()
    }

    /// Current event sequence number.
    pub fn event_seq(&self) -> u64 {
        self.seq_gen.current()
    }

    /// Inserts a pending approval entry.
    pub fn insert_pending(&self, entry: PendingApprovalEntry) {
        self.pending_approvals
            .insert(entry.action_id.clone(), entry);
    }

    /// Removes and returns a pending approval by action ID.
    pub fn remove_pending(&self, action_id: &str) -> Option<PendingApprovalEntry> {
        self.pending_approvals.remove(action_id).map(|(_, v)| v)
    }

    /// Snapshot of all pending approval entries.
    pub fn pending_actions(&self) -> Vec<PendingApprovalEntry> {
        self.pending_approvals
            .iter()
            .map(|entry| PendingApprovalEntry {
                action_id: entry.action_id.clone(),
                pid: entry.pid,
                process: entry.process.clone(),
                syscall: entry.syscall.clone(),
                path: entry.path.clone(),
                timestamp: entry.timestamp.clone(),
                rule_matched: entry.rule_matched.clone(),
                decision_tx: None,
            })
            .collect()
    }

    /// Number of pending approvals.
    pub fn pending_count(&self) -> usize {
        self.pending_approvals.len()
    }

    /// Returns a cloned handle to the `ArcSwap<RuleSet>` for lock-free reads.
    ///
    /// The tracer thread calls `.load()` on each syscall stop.
    /// The API thread calls `.store()` to swap atomically.
    pub fn rules_handle(&self) -> Arc<ArcSwap<RuleSet>> {
        Arc::clone(&self.rules)
    }

    /// Load the current rule set snapshot.
    pub fn load_rules(&self) -> arc_swap::Guard<Arc<RuleSet>> {
        self.rules.load()
    }

    /// Atomically replace the active rule set.
    pub fn store_rules(&self, new_rules: RuleSet) {
        self.rules.store(Arc::new(new_rules));
    }

    /// Records a sink stall condition, replacing any prior stall state.
    pub fn set_stall(&self, state: StallState) {
        *self.stall.lock() = Some(state);
    }

    /// Clears the stall state once all required sinks have recovered.
    pub fn clear_stall(&self) {
        *self.stall.lock() = None;
    }

    /// Returns a snapshot of the current stall state, if any.
    pub fn stall_state(&self) -> Option<StallState> {
        self.stall.lock().clone()
    }
}

/// Thread-safe handle to the bridge.
pub type SharedState = Arc<Bridge>;

/// Creates a new shared bridge handle.
pub fn new_shared_state(agent_id: CompactString, cas: Arc<dyn Cas>, bus: RecordBus) -> SharedState {
    Arc::new(Bridge::new(agent_id, cas, bus))
}

/// Creates a new shared bridge handle with an overflow queue.
pub(crate) fn new_shared_state_with_overflow(
    agent_id: CompactString,
    cas: Arc<dyn Cas>,
    bus: RecordBus,
    overflow: Option<Arc<OverflowQueue>>,
) -> SharedState {
    Arc::new(Bridge::with_overflow(agent_id, cas, bus, overflow))
}

/// Delivers an approval decision through the oneshot channel.
///
/// Removes the entry from state, sends the decision, and returns the
/// entry metadata. Returns `None` if the action ID is not found.
pub fn resolve_approval(
    state: &SharedState,
    action_id: &str,
    decision: ApprovalDecision,
) -> Option<PendingApprovalEntry> {
    let mut entry = state.remove_pending(action_id)?;

    if let Some(tx) = entry.decision_tx.take() {
        let _ = tx.send(decision);
    }

    Some(entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cas::MemoryCas;

    fn test_cas() -> Arc<dyn Cas> {
        Arc::new(MemoryCas::new())
    }

    fn test_bus() -> RecordBus {
        RecordBus::new(vec![])
    }

    #[test]
    fn new_state_is_running() {
        let bridge = Bridge::new("test".into(), test_cas(), test_bus());
        assert!(!bridge.is_paused());
        assert_eq!(bridge.agent_id(), "test");
    }

    #[test]
    fn set_paused_returns_whether_changed() {
        let bridge = Bridge::new("test".into(), test_cas(), test_bus());
        assert!(bridge.set_paused(true));
        assert!(!bridge.set_paused(true));
        assert!(bridge.set_paused(false));
        assert!(!bridge.set_paused(false));
    }

    #[test]
    fn insert_and_remove_pending() {
        let bridge = Bridge::new("test".into(), test_cas(), test_bus());
        let entry = PendingApprovalEntry {
            action_id: "a1".into(),
            pid: 42,
            process: "python".into(),
            syscall: "unlink".into(),
            path: Some("/workspace/foo.txt".into()),
            timestamp: "2026-01-01T00:00:00Z".into(),
            rule_matched: "unlink /workspace/**".into(),
            decision_tx: None,
        };
        bridge.insert_pending(entry);
        assert_eq!(bridge.pending_count(), 1);
        assert_eq!(bridge.pending_actions().len(), 1);

        let removed = bridge.remove_pending("a1").unwrap();
        assert_eq!(removed.pid, 42);
        assert_eq!(bridge.pending_count(), 0);
    }

    #[test]
    fn remove_nonexistent_returns_none() {
        let bridge = Bridge::new("test".into(), test_cas(), test_bus());
        assert!(bridge.remove_pending("nope").is_none());
    }

    #[test]
    fn resolve_approval_delivers_decision() {
        let shared = new_shared_state("test".into(), test_cas(), test_bus());
        let (tx, mut rx) = tokio::sync::oneshot::channel();

        shared.insert_pending(PendingApprovalEntry {
            action_id: "a1".into(),
            pid: 10,
            process: "bash".into(),
            syscall: "exec".into(),
            path: None,
            timestamp: "2026-01-01T00:00:00Z".into(),
            rule_matched: "exec rule".into(),
            decision_tx: Some(tx),
        });

        let entry = resolve_approval(&shared, "a1", ApprovalDecision::Approve).unwrap();
        assert_eq!(entry.pid, 10);

        let decision = rx.try_recv().unwrap();
        assert_eq!(decision, ApprovalDecision::Approve);
    }

    #[test]
    fn resolve_nonexistent_returns_none() {
        let shared = new_shared_state("test".into(), test_cas(), test_bus());
        assert!(resolve_approval(&shared, "nope", ApprovalDecision::Deny).is_none());
    }

    #[test]
    fn uptime_is_positive() {
        let bridge = Bridge::new("test".into(), test_cas(), test_bus());
        assert!(bridge.uptime_seconds() >= 0.0);
    }

    #[test]
    fn shared_state_across_threads() {
        let shared = new_shared_state("threaded".into(), test_cas(), test_bus());
        let handle = {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || {
                shared.set_paused(true);
            })
        };
        handle.join().unwrap();
        assert!(shared.is_paused());
    }

    #[test]
    fn emit_with_no_subscribers_does_not_panic() {
        let bridge = Bridge::new("test".into(), test_cas(), test_bus());
        bridge.emit(EventPayload::AgentPause(crate::events::control::AgentPause {
            reason: "test".into(),
            stopped_pids: Vec::new(),
        }));
    }

    #[test]
    fn subscribe_receives_events() {
        let bridge = Bridge::new("test".into(), test_cas(), test_bus());
        let mut rx = bridge.subscribe_events();
        bridge.emit(EventPayload::AgentPause(crate::events::control::AgentPause {
            reason: "test".into(),
            stopped_pids: Vec::new(),
        }));
        let evt = rx.try_recv().unwrap();
        assert_eq!(&*evt.agent_id, "test");
    }

    #[test]
    fn store_and_load_tree() {
        let bridge = Bridge::new("test".into(), test_cas(), test_bus());
        let mut tree = crate::snapshot::MerkleTree::new();
        tree.update(
            std::path::PathBuf::from("a.txt"),
            crate::cas::ContentHash::from_data(b"hello"),
        );
        bridge.store_tree(tree);

        let loaded = bridge.load_tree();
        assert_eq!(loaded.file_count(), 1);
    }

    #[test]
    fn insert_and_get_tree_hash() {
        let bridge = Bridge::new("test".into(), test_cas(), test_bus());
        bridge.insert_tree_hash(42, "abc123".into());
        assert_eq!(bridge.get_tree_hash(42), Some("abc123".into()));
        assert_eq!(bridge.get_tree_hash(99), None);
    }
}
