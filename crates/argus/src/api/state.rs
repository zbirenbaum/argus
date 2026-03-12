// Rust guideline compliant 2026-02-21
//! Shared state between the API server and the tracer thread.
//!
//! The tracer loop runs synchronously on a dedicated OS thread while the
//! API server runs on the tokio runtime. This module provides the
//! thread-safe bridge between them using `Arc<Mutex<_>>`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use arc_swap::ArcSwap;
use tokio::sync::mpsc;

use crate::api::types::PendingApprovalEntry;
use crate::config::RuleSet;
use crate::events::{ApprovalDecision, Event, EventPayload, SequenceGenerator};

/// Shared supervisor state for API and tracer threads.
///
/// Wrap in `Arc<Mutex<_>>` for thread-safe sharing. The mutex is held
/// only briefly during state reads and writes, so contention is minimal.
#[derive(Debug)]
pub struct SupervisorState {
    paused: bool,
    agent_id: String,
    started_at: Instant,
    seq_gen: SequenceGenerator,
    pending_approvals: HashMap<String, PendingApprovalEntry>,
    event_tx: Option<mpsc::UnboundedSender<Event>>,
    rules: Arc<ArcSwap<RuleSet>>,
}

impl SupervisorState {
    /// Creates state for a new supervisor session.
    pub fn new(agent_id: String) -> Self {
        Self {
            paused: false,
            agent_id,
            started_at: Instant::now(),
            seq_gen: SequenceGenerator::default(),
            pending_approvals: HashMap::new(),
            event_tx: None,
            rules: Arc::new(ArcSwap::from_pointee(RuleSet::default())),
        }
    }

    /// Creates state with an event sender for structured event emission.
    pub fn with_event_tx(agent_id: String, event_tx: mpsc::UnboundedSender<Event>) -> Self {
        Self {
            paused: false,
            agent_id,
            started_at: Instant::now(),
            seq_gen: SequenceGenerator::default(),
            pending_approvals: HashMap::new(),
            event_tx: Some(event_tx),
            rules: Arc::new(ArcSwap::from_pointee(RuleSet::default())),
        }
    }

    /// Emits an event through the event channel if configured.
    ///
    /// Best-effort: silently drops the event if the receiver is closed.
    pub fn emit(&self, payload: EventPayload) {
        if let Some(tx) = &self.event_tx {
            let evt = Event::new(&self.seq_gen, self.agent_id.clone(), payload);
            let _ = tx.send(evt);
        }
    }

    /// Whether the agent is currently paused.
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Sets the paused flag. Returns `true` if the state changed.
    pub fn set_paused(&mut self, paused: bool) -> bool {
        if self.paused == paused {
            return false;
        }
        self.paused = paused;
        true
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
        self.seq_gen.next_seq()
    }

    /// Inserts a pending approval entry.
    pub fn insert_pending(&mut self, entry: PendingApprovalEntry) {
        self.pending_approvals
            .insert(entry.action_id.clone(), entry);
    }

    /// Removes and returns a pending approval by action ID.
    pub fn remove_pending(&mut self, action_id: &str) -> Option<PendingApprovalEntry> {
        self.pending_approvals.remove(action_id)
    }

    /// Snapshot of all pending approval entries.
    pub fn pending_actions(&self) -> Vec<&PendingApprovalEntry> {
        self.pending_approvals.values().collect()
    }

    /// Number of pending approvals.
    pub fn pending_count(&self) -> usize {
        self.pending_approvals.len()
    }

    /// Returns a handle to the `ArcSwap<RuleSet>` for lock-free reads.
    ///
    /// The tracer thread calls `rules_handle().load()` on each syscall
    /// stop. The API thread calls `rules_handle().store()` to swap
    /// atomically.
    pub fn rules_handle(&self) -> &Arc<ArcSwap<RuleSet>> {
        &self.rules
    }

    /// Load the current rule set snapshot.
    pub fn load_rules(&self) -> arc_swap::Guard<Arc<RuleSet>> {
        self.rules.load()
    }

    /// Atomically replace the active rule set.
    pub fn store_rules(&self, new_rules: RuleSet) {
        self.rules.store(Arc::new(new_rules));
    }
}

/// Thread-safe handle to supervisor state.
pub type SharedState = Arc<Mutex<SupervisorState>>;

/// Creates a new shared state handle.
pub fn new_shared_state(agent_id: String) -> SharedState {
    Arc::new(Mutex::new(SupervisorState::new(agent_id)))
}

/// Creates a new shared state handle with an event sender.
pub fn new_shared_state_with_events(
    agent_id: String,
    event_tx: mpsc::UnboundedSender<Event>,
) -> SharedState {
    Arc::new(Mutex::new(SupervisorState::with_event_tx(
        agent_id, event_tx,
    )))
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
    let mut guard = state.lock().expect("state lock poisoned");
    let mut entry = guard.remove_pending(action_id)?;

    if let Some(tx) = entry.decision_tx.take() {
        let _ = tx.send(decision);
    }

    Some(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_state_is_running() {
        let state = SupervisorState::new("test".into());
        assert!(!state.is_paused());
        assert_eq!(state.agent_id(), "test");
    }

    #[test]
    fn set_paused_returns_whether_changed() {
        let mut state = SupervisorState::new("test".into());
        assert!(state.set_paused(true));
        assert!(!state.set_paused(true));
        assert!(state.set_paused(false));
        assert!(!state.set_paused(false));
    }

    #[test]
    fn insert_and_remove_pending() {
        let mut state = SupervisorState::new("test".into());
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
        state.insert_pending(entry);
        assert_eq!(state.pending_count(), 1);
        assert_eq!(state.pending_actions().len(), 1);

        let removed = state.remove_pending("a1").unwrap();
        assert_eq!(removed.pid, 42);
        assert_eq!(state.pending_count(), 0);
    }

    #[test]
    fn remove_nonexistent_returns_none() {
        let mut state = SupervisorState::new("test".into());
        assert!(state.remove_pending("nope").is_none());
    }

    #[test]
    fn resolve_approval_delivers_decision() {
        let shared = new_shared_state("test".into());
        let (tx, mut rx) = tokio::sync::oneshot::channel();

        {
            let mut guard = shared.lock().unwrap();
            guard.insert_pending(PendingApprovalEntry {
                action_id: "a1".into(),
                pid: 10,
                process: "bash".into(),
                syscall: "exec".into(),
                path: None,
                timestamp: "2026-01-01T00:00:00Z".into(),
                rule_matched: "exec rule".into(),
                decision_tx: Some(tx),
            });
        }

        let entry = resolve_approval(&shared, "a1", ApprovalDecision::Approve).unwrap();
        assert_eq!(entry.pid, 10);

        let decision = rx.try_recv().unwrap();
        assert_eq!(decision, ApprovalDecision::Approve);
    }

    #[test]
    fn resolve_nonexistent_returns_none() {
        let shared = new_shared_state("test".into());
        assert!(resolve_approval(&shared, "nope", ApprovalDecision::Deny).is_none());
    }

    #[test]
    fn uptime_is_positive() {
        let state = SupervisorState::new("test".into());
        assert!(state.uptime_seconds() >= 0.0);
    }

    #[test]
    fn shared_state_across_threads() {
        let shared = new_shared_state("threaded".into());
        let handle = {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || {
                let mut guard = shared.lock().unwrap();
                guard.set_paused(true);
            })
        };
        handle.join().unwrap();
        let guard = shared.lock().unwrap();
        assert!(guard.is_paused());
    }
}
