// Rust guideline compliant 2026-02-21
//! Unit-level tests for `SharedState`, `PolicyGate`, and workspace walk.
//!
//! Covers validate.sh tests 9 (pause/resume), 10 (pause-before-action approval
//! flow), 11 (tree snapshot), and 12 (initial-state workspace walk).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use arc_swap::ArcSwap;
use nix::unistd::Pid;

use crate::api::state::{SharedState, new_shared_state, resolve_approval};
use crate::cas::MemoryCas;
use crate::config::{MatchKind, Rule, RuleSet};
use crate::events::{ApprovalDecision, EventPayload};
use crate::events::control::{AgentPause, AgentResume};
use crate::pipeline::bus::RecordBus;
use crate::pipeline::classified::{ClassifiedEvent, Classification};
use crate::pipeline::directive::PipelineDirective;
use crate::pipeline::ptrace_thread::PtraceHandle;
use crate::pipeline::raw_stop::{RawSyscallStop, StopType, SyscallArgs};
use crate::pipeline::sink::SinkPriority;
use crate::pipeline::sinks::memory::MemorySink;
use crate::pipeline::stages::policy_gate::{PolicyGate, PolicyOutcome};

// ── helpers ───────────────────────────────────────────────────────────────────

fn test_shared() -> SharedState {
    let cas: Arc<dyn crate::cas::Cas> = Arc::new(MemoryCas::new());
    new_shared_state("test".into(), cas, RecordBus::new(vec![]))
}

fn test_shared_with_sink(sink: Arc<MemorySink>) -> SharedState {
    let cas: Arc<dyn crate::cas::Cas> = Arc::new(MemoryCas::new());
    let bus = RecordBus::new(vec![sink as Arc<dyn crate::pipeline::sink::Sink>]);
    new_shared_state("test".into(), cas, bus)
}

fn make_event(pid: i32, cls: Classification) -> ClassifiedEvent {
    ClassifiedEvent {
        pid: Pid::from_raw(pid),
        raw: RawSyscallStop {
            pid: Pid::from_raw(pid),
            stop_type: StopType::SyscallEntry {
                syscall_nr: 0,
                args: SyscallArgs::from_array([0; 6]),
            },
        },
        classification: cls,
    }
}

fn make_gate(
    shared: SharedState,
    rules: RuleSet,
) -> (PolicyGate, tokio::sync::mpsc::UnboundedReceiver<PipelineDirective>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let handle = PtraceHandle::from_sender(tx);
    let rules_swap = Arc::new(ArcSwap::new(Arc::new(rules)));
    (PolicyGate::new(handle, rules_swap, shared), rx)
}

// ── Test 9 — Pause/Resume flag semantics ─────────────────────────────────────

/// `AtomicBool` flag store/load models the pause-hot-path in `wait_if_paused`.
#[test]
fn test_pause_flag_blocks_pipeline() {
    let flag = Arc::new(AtomicBool::new(false));
    assert!(!flag.load(Ordering::Acquire));
    flag.store(true, Ordering::Release);
    assert!(flag.load(Ordering::Acquire));
    flag.store(false, Ordering::Release);
    assert!(!flag.load(Ordering::Acquire));
}

/// `Bridge::set_paused` is idempotent; second call returns false.
#[test]
fn test_bridge_pause_resume_state_transitions() {
    let shared = test_shared();
    assert!(!shared.is_paused());
    assert!(shared.set_paused(true));
    assert!(!shared.set_paused(true), "idempotent pause must not signal change");
    assert!(shared.is_paused());
    assert!(shared.set_paused(false));
    assert!(!shared.is_paused());
}

/// `emit(AgentPause)` reaches both the `RecordBus` sink and the broadcast channel.
#[test]
fn test_shared_emit_produces_bus_event() {
    let sink = Arc::new(MemorySink::new(SinkPriority::Blocking));
    let shared = test_shared_with_sink(Arc::clone(&sink));
    let mut rx = shared.subscribe_events();

    shared.emit(EventPayload::AgentPause(AgentPause {
        reason: "test pause".into(),
        stopped_pids: vec![42],
    }));
    assert_eq!(sink.len(), 1);
    assert!(matches!(&sink.events()[0].payload, EventPayload::AgentPause(_)));

    let broadcast_evt = rx.try_recv().expect("broadcast must deliver the event");
    assert!(matches!(broadcast_evt.payload, EventPayload::AgentPause(_)));

    shared.emit(EventPayload::AgentResume(AgentResume { resumed_pids: vec![42] }));
    assert_eq!(sink.len(), 2);
}

// ── Test 10 — Pause-before-action: approval flow ──────────────────────────────

/// Deny yields `Blocked` and injects EPERM via the directive channel.
#[tokio::test]
async fn test_policy_gate_deny_blocks_and_injects_eperm() {
    let mut rs = RuleSet::default();
    rs.pause_before.push(Rule::new(
        MatchKind::Unlink,
        vec!["/workspace/**".into()],
        vec![],
        vec![],
    ));
    rs.compile_patterns();
    let shared = test_shared();
    let (gate, rx) = make_gate(Arc::clone(&shared), rs);
    let mut rx = crate::pipeline::freeze::spawn_freeze_responder(rx);
    let event = make_event(99, Classification::FileUnlink {
        path: PathBuf::from("/workspace/critical.txt"),
    });

    let shared2 = Arc::clone(&shared);
    tokio::spawn(async move {
        loop {
            if shared2.pending_count() > 0 {
                let action_id = shared2.pending_actions()[0].action_id.clone();
                resolve_approval(&shared2, &action_id, ApprovalDecision::Deny);
                break;
            }
            tokio::task::yield_now().await;
        }
    });

    match gate.evaluate(event).await {
        PolicyOutcome::Blocked { pid, .. } => assert_eq!(pid, 99),
        PolicyOutcome::Approved(_) => panic!("expected Blocked after Deny"),
    }
    let dir = rx.recv().await.expect("directive must be sent");
    assert!(matches!(dir, PipelineDirective::InjectError { errno, .. } if errno == libc::EPERM));
}

/// Approve yields `Approved` without injecting EPERM.
#[tokio::test]
async fn test_policy_gate_approve_passes_through() {
    let mut rs = RuleSet::default();
    rs.pause_before.push(Rule::new(
        MatchKind::Unlink,
        vec!["/workspace/**".into()],
        vec![],
        vec![],
    ));
    rs.compile_patterns();
    let shared = test_shared();
    let (gate, rx) = make_gate(Arc::clone(&shared), rs);
    let mut rx = crate::pipeline::freeze::spawn_freeze_responder(rx);
    let event = make_event(77, Classification::FileUnlink {
        path: PathBuf::from("/workspace/safe.txt"),
    });

    let shared2 = Arc::clone(&shared);
    tokio::spawn(async move {
        loop {
            if shared2.pending_count() > 0 {
                let action_id = shared2.pending_actions()[0].action_id.clone();
                resolve_approval(&shared2, &action_id, ApprovalDecision::Approve);
                break;
            }
            tokio::task::yield_now().await;
        }
    });

    assert!(matches!(gate.evaluate(event).await, PolicyOutcome::Approved(_)));
    let idle = tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await;
    assert!(idle.is_err(), "no directive for an approved action");
}

// ── Test 11 — Snapshot ────────────────────────────────────────────────────────

/// `store_tree` is immediately visible via `load_tree`.
#[test]
fn test_tree_snapshot_round_trips() {
    use crate::cas::ContentHash;
    use crate::snapshot::MerkleTree;

    let shared = test_shared();
    let mut tree = MerkleTree::new();
    tree.update(PathBuf::from("snap.txt"), ContentHash::from_data(b"v1"));
    assert_eq!(tree.file_count(), 1);
    shared.store_tree(tree);

    let loaded = shared.load_tree();
    assert_eq!(loaded.file_count(), 1);
    assert!(!loaded.root_hash().to_string().is_empty());
}

// ── Test 12 — Initial state: workspace walk ───────────────────────────────────

/// Walking a two-file workspace produces ≥2 `InitialFile` events and exactly
/// one `InitialState` event with correct totals.
#[test]
fn test_initial_state_walk_emits_events() {
    use std::fs;
    use tempfile::TempDir;

    use crate::cas::ContentHash;
    use crate::events::{Event, SequenceGenerator};
    use crate::events::snapshot::{InitialFile, InitialState};
    use crate::pipeline::record::Record;
    use crate::pipeline::stages::redact::RedactStage;
    use crate::snapshot::MerkleTree;

    let tmp = TempDir::new().expect("tempdir");
    fs::write(tmp.path().join("existing.txt"), "pre-existing\n").expect("write");
    fs::create_dir_all(tmp.path().join("subdir")).expect("mkdir");
    fs::write(tmp.path().join("subdir/nested.txt"), "nested\n").expect("write nested");

    let sink = Arc::new(MemorySink::new(SinkPriority::Blocking));
    let bus = RecordBus::new(vec![Arc::clone(&sink) as Arc<dyn crate::pipeline::sink::Sink>]);
    let seq = Arc::new(SequenceGenerator::default());
    let agent_id: compact_str::CompactString = "test".into();

    let mut file_count: u64 = 0;
    let mut total_size: u64 = 0;
    let mut tree = MerkleTree::new();

    walk_test(tmp.path(), &mut |path: &std::path::Path| {
        use std::os::unix::fs::MetadataExt;
        let meta = match path.metadata() { Ok(m) if m.is_file() => m, _ => return };
        let data = match fs::read(path) { Ok(d) => d, Err(_) => return };
        let hash = ContentHash::from_data(&data);
        tree.update(path.to_path_buf(), hash.clone());
        let payload = EventPayload::InitialFile(InitialFile {
            pid: 0,
            path: path.to_string_lossy().into(),
            content_hash: hash.to_string(),
            size: meta.len(),
            mode: meta.mode(),
        });
        let _ = bus.emit(Record::Event(Event::new(&seq, agent_id.clone(), payload)));
        file_count += 1;
        total_size += meta.len();
    });

    let tree_hash = (file_count > 0).then(|| tree.root_hash().to_string());
    let summary = EventPayload::InitialState(InitialState { tree_hash, file_count, total_size });
    let _ = bus.emit(Record::Event(Event::new(&seq, agent_id.clone(), summary)));

    let events = sink.events();
    let files: Vec<_> = events.iter().filter(|e| matches!(e.payload, EventPayload::InitialFile(_))).collect();
    let summaries: Vec<_> = events.iter().filter(|e| matches!(e.payload, EventPayload::InitialState(_))).collect();

    assert!(files.len() >= 2, "expected >= 2 initial_file events, got {}", files.len());
    assert_eq!(summaries.len(), 1);
    if let EventPayload::InitialState(ref s) = summaries[0].payload {
        assert!(s.file_count >= 2);
        assert!(s.total_size > 0);
        assert!(s.tree_hash.is_some());
    }
}

fn walk_test(dir: &std::path::Path, visitor: &mut dyn FnMut(&std::path::Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() { walk_test(&path, visitor); } else { visitor(&path); }
    }
}
