// Rust guideline compliant 2026-02-21
//! End-to-end pipeline integration tests (validation tests 1–7, 9, 11, 13).
//!
//! Each test drives `PipelineRunner::run()` with canned stops and verifies
//! the events captured by `MemorySink`.

use std::time::Duration;

use nix::unistd::Pid;
use tokio::time::timeout;

use crate::config::RuleSet;
use crate::events::EventPayload;
use crate::pipeline::mock_ptrace::MockPtraceThread;
use crate::pipeline::raw_stop::{RawSyscallStop, StopType};

use super::harness::{build_harness, entry, exit_stop, nr};

// ── Test 1 — Basic process tracing ───────────────────────────────────────────

/// Feed exec + exit stops; verify `Exec` and `Exit` events appear in the sink.
#[tokio::test]
async fn test1_exec_and_exit_events_produced() {
    let pid = 1000;
    let stops = vec![
        RawSyscallStop {
            pid: Pid::from_raw(pid),
            stop_type: StopType::Exec { pid: Pid::from_raw(pid) },
        },
        RawSyscallStop {
            pid: Pid::from_raw(pid),
            stop_type: StopType::Exit { pid: Pid::from_raw(pid), exit_code: 0 },
        },
    ];
    let h = build_harness(MockPtraceThread::new(), stops, RuleSet::default());
    h.runner.run().await;
    let events = h.sink.events();
    assert!(events.iter().any(|e| matches!(e.payload, EventPayload::Exec(_))), "Exec event required");
    assert!(events.iter().any(|e| matches!(e.payload, EventPayload::Exit(_))), "Exit event required");
}

// ── Test 2 — Stdio capture ────────────────────────────────────────────────────

/// Write to fd 1 (stdout) with canned buffer; verify `Stdio` with `stdout`.
#[tokio::test]
async fn test2_stdout_write_produces_stdio_event() {
    let pid = 1001;
    let buf_addr: u64 = 0x1000;
    let data = b"hello stdout\n";
    let mut mock = MockPtraceThread::new();
    mock.add_memory(Pid::from_raw(pid), buf_addr as usize, data.to_vec());

    let stops = vec![entry(pid, nr::WRITE, [1, buf_addr, data.len() as u64, 0, 0, 0])];
    let h = build_harness(mock, stops, RuleSet::default());
    h.runner.run().await;

    let events = h.sink.events();
    let stdio_evs: Vec<_> = events.iter().filter(|e| matches!(e.payload, EventPayload::Stdio(_))).collect();
    assert!(!stdio_evs.is_empty(), "Stdio event required");
    match &stdio_evs[0].payload {
        EventPayload::Stdio(s) => assert_eq!(s.subtype, crate::events::io::StdioSubtype::Stdout),
        _ => panic!("expected Stdio payload"),
    }
}

// ── Test 3 — File write + unlink ──────────────────────────────────────────────

/// Register fd via openat entry/exit, write, then unlink. Verify Write + Unlink.
#[tokio::test]
async fn test3_file_write_and_unlink_events() {
    let pid = 1002;
    let path_addr: u64 = 0x2000;
    let buf_addr: u64 = 0x3000;
    let path_bytes = b"/workspace/test.txt\0";
    let write_data = b"some content";
    let fd: i64 = 5;

    let mut mock = MockPtraceThread::new();
    mock.add_memory(Pid::from_raw(pid), path_addr as usize, path_bytes.to_vec());
    mock.add_memory(Pid::from_raw(pid), buf_addr as usize, write_data.to_vec());

    let stops = vec![
        entry(pid, nr::OPENAT, [libc::AT_FDCWD as u64, path_addr, libc::O_WRONLY as u64, 0o644, 0, 0]),
        exit_stop(pid, nr::OPENAT, fd),
        entry(pid, nr::WRITE, [fd as u64, buf_addr, write_data.len() as u64, 0, 0, 0]),
        entry(pid, nr::UNLINKAT, [libc::AT_FDCWD as u64, path_addr, 0, 0, 0, 0]),
    ];
    let h = build_harness(mock, stops, RuleSet::default());
    h.runner.run().await;

    let events = h.sink.events();
    assert!(events.iter().any(|e| matches!(e.payload, EventPayload::Write(_))), "Write event required");
    assert!(events.iter().any(|e| matches!(e.payload, EventPayload::Unlink(_))), "Unlink event required");
}

// ── Test 4 — Pipe topology ────────────────────────────────────────────────

/// Create a pipe via pipe2 entry/exit, then write to the write-end fd.
/// Verify `PipeCreate` and `PipeData` events appear.
#[tokio::test]
async fn test4_pipe_topology() {
    let pid = 1003;
    let pipe_array_addr: u64 = 0xA000;
    let buf_addr: u64 = 0xB000;

    let mut mock = MockPtraceThread::new();
    // pipe2() writes [read_fd=10, write_fd=11] at pipe_array_addr.
    let pipe_fds: [i32; 2] = [10, 11];
    let mut pipe_bytes = Vec::with_capacity(8);
    pipe_bytes.extend_from_slice(&pipe_fds[0].to_ne_bytes());
    pipe_bytes.extend_from_slice(&pipe_fds[1].to_ne_bytes());
    mock.add_memory(Pid::from_raw(pid), pipe_array_addr as usize, pipe_bytes);
    mock.add_memory(Pid::from_raw(pid), buf_addr as usize, b"pipe data\n".to_vec());

    let stops = vec![
        // pipe2 entry: arg0 = pipe_array_addr
        entry(pid, nr::PIPE2, [pipe_array_addr, 0, 0, 0, 0, 0]),
        // pipe2 exit: success (return 0)
        exit_stop(pid, nr::PIPE2, 0),
        // Write to the pipe's write-end fd=11
        entry(pid, nr::WRITE, [11, buf_addr, 10, 0, 0, 0]),
    ];
    let h = build_harness(mock, stops, RuleSet::default());
    h.runner.run().await;

    let events = h.sink.events();
    assert!(
        events.iter().any(|e| matches!(e.payload, EventPayload::PipeCreate(_))),
        "PipeCreate event required"
    );
    assert!(
        events.iter().any(|e| matches!(e.payload, EventPayload::PipeData(_))),
        "PipeData event required"
    );
}

// ── Test 5 — Subprocess tree ──────────────────────────────────────────────────

/// Fork → child exec → child exit → parent exit produces Fork/Exec/Exit events.
#[tokio::test]
async fn test5_subprocess_fork_exec_exit() {
    let parent = 2000;
    let child = 2001;
    let stops = vec![
        RawSyscallStop {
            pid: Pid::from_raw(parent),
            stop_type: StopType::Fork {
                parent: Pid::from_raw(parent),
                child: Pid::from_raw(child),
            },
        },
        RawSyscallStop { pid: Pid::from_raw(child), stop_type: StopType::Exec { pid: Pid::from_raw(child) } },
        RawSyscallStop { pid: Pid::from_raw(child), stop_type: StopType::Exit { pid: Pid::from_raw(child), exit_code: 0 } },
        RawSyscallStop { pid: Pid::from_raw(parent), stop_type: StopType::Exit { pid: Pid::from_raw(parent), exit_code: 0 } },
    ];
    let h = build_harness(MockPtraceThread::new(), stops, RuleSet::default());
    h.runner.run().await;

    let events = h.sink.events();
    assert!(events.iter().any(|e| {
        if let EventPayload::Fork(f) = &e.payload { f.parent_pid == parent as u32 && f.child_pid == child as u32 } else { false }
    }), "Fork with correct PIDs required");
    assert!(events.iter().any(|e| matches!(e.payload, EventPayload::Exec(_))));
    let exits: Vec<_> = events.iter().filter(|e| matches!(e.payload, EventPayload::Exit(_))).collect();
    assert_eq!(exits.len(), 2, "two Exit events expected");
}

// ── Test 6 — Escape test (self-created tool) ─────────────────────────────

/// A child exec + file write in workspace: both events must be captured.
#[tokio::test]
async fn test6_self_created_tool_captured() {
    let parent = 2100;
    let child = 2101;
    let path_addr: u64 = 0xC000;
    let buf_addr: u64 = 0xD000;
    let fd: i64 = 4;

    let mut mock = MockPtraceThread::new();
    mock.add_memory(Pid::from_raw(child), path_addr as usize, b"/workspace/tool-output.txt\0".to_vec());
    mock.add_memory(Pid::from_raw(child), buf_addr as usize, b"tool output data".to_vec());

    let stops = vec![
        // Parent forks child.
        RawSyscallStop {
            pid: Pid::from_raw(parent),
            stop_type: StopType::Fork { parent: Pid::from_raw(parent), child: Pid::from_raw(child) },
        },
        // Child execs python3.
        RawSyscallStop { pid: Pid::from_raw(child), stop_type: StopType::Exec { pid: Pid::from_raw(child) } },
        // Child opens file.
        entry(child, nr::OPENAT, [libc::AT_FDCWD as u64, path_addr, libc::O_WRONLY as u64, 0o644, 0, 0]),
        exit_stop(child, nr::OPENAT, fd),
        // Child writes.
        entry(child, nr::WRITE, [fd as u64, buf_addr, 16, 0, 0, 0]),
        // Child exits.
        RawSyscallStop { pid: Pid::from_raw(child), stop_type: StopType::Exit { pid: Pid::from_raw(child), exit_code: 0 } },
        // Parent exits.
        RawSyscallStop { pid: Pid::from_raw(parent), stop_type: StopType::Exit { pid: Pid::from_raw(parent), exit_code: 0 } },
    ];
    let h = build_harness(mock, stops, RuleSet::default());
    h.runner.run().await;

    let events = h.sink.events();
    assert!(events.iter().any(|e| matches!(e.payload, EventPayload::Exec(_))), "child exec must be captured");
    assert!(
        events.iter().any(|e| {
            if let EventPayload::Write(w) = &e.payload { w.path.contains("tool-output") } else { false }
        }),
        "child write to tool-output.txt must be captured"
    );
}

// ── Test 7 — Write hash chain integrity ──────────────────────────────────────

/// Sequential writes from two PIDs to the same path both have `after_hash` set.
#[tokio::test]
async fn test7_write_hash_chain_integrity() {
    let pid_a = 3000;
    let pid_b = 3001;
    let path_addr: u64 = 0x5000;
    let buf_a: u64 = 0x6000;
    let buf_b: u64 = 0x7000;
    let path_bytes = b"/workspace/shared.txt\0";
    let fd: i64 = 7;

    let mut mock = MockPtraceThread::new();
    mock.add_memory(Pid::from_raw(pid_a), path_addr as usize, path_bytes.to_vec());
    mock.add_memory(Pid::from_raw(pid_b), path_addr as usize, path_bytes.to_vec());
    mock.add_memory(Pid::from_raw(pid_a), buf_a as usize, b"first write".to_vec());
    mock.add_memory(Pid::from_raw(pid_b), buf_b as usize, b"second write".to_vec());

    let stops = vec![
        entry(pid_a, nr::OPENAT, [libc::AT_FDCWD as u64, path_addr, libc::O_WRONLY as u64, 0o644, 0, 0]),
        exit_stop(pid_a, nr::OPENAT, fd),
        entry(pid_b, nr::OPENAT, [libc::AT_FDCWD as u64, path_addr, libc::O_WRONLY as u64, 0o644, 0, 0]),
        exit_stop(pid_b, nr::OPENAT, fd),
        entry(pid_a, nr::WRITE, [fd as u64, buf_a, 11, 0, 0, 0]),
        entry(pid_b, nr::WRITE, [fd as u64, buf_b, 12, 0, 0, 0]),
    ];
    let h = build_harness(mock, stops, RuleSet::default());
    h.runner.run().await;

    let events = h.sink.events();
    let writes: Vec<_> = events.iter().filter_map(|e| {
        if let EventPayload::Write(w) = &e.payload { Some(w) } else { None }
    }).collect();
    assert_eq!(writes.len(), 2, "two Write events required");
    for w in &writes {
        assert!(w.after_hash.is_some(), "after_hash must be set");
    }
}

// ── Test 7b — Write interleaving (hash chain correctness) ────────────────

/// Multiple writes from different PIDs to the same path. Each write's
/// `after_hash` must differ from the previous (unique content → unique hash).
#[tokio::test]
async fn test7b_write_interleaving_hash_chain() {
    let pids = [3100, 3101, 3102, 3103];
    let path_addr: u64 = 0xE000;
    let path_bytes = b"/workspace/shared.txt\0";
    let fd: i64 = 7;

    let mut mock = MockPtraceThread::new();
    for &pid in &pids {
        mock.add_memory(Pid::from_raw(pid), path_addr as usize, path_bytes.to_vec());
    }

    let mut stops = Vec::new();
    // Each PID opens the file then writes unique content.
    for (i, &pid) in pids.iter().enumerate() {
        let buf_addr = 0xF000 + (i as u64 * 0x100);
        let content = format!("writer {i} data\n");
        mock.add_memory(Pid::from_raw(pid), buf_addr as usize, content.as_bytes().to_vec());

        stops.push(entry(pid, nr::OPENAT, [libc::AT_FDCWD as u64, path_addr, libc::O_WRONLY as u64, 0o644, 0, 0]));
        stops.push(exit_stop(pid, nr::OPENAT, fd));
        stops.push(entry(pid, nr::WRITE, [fd as u64, buf_addr, content.len() as u64, 0, 0, 0]));
    }

    let h = build_harness(mock, stops, RuleSet::default());
    h.runner.run().await;

    let events = h.sink.events();
    let writes: Vec<_> = events.iter().filter_map(|e| {
        if let EventPayload::Write(w) = &e.payload { Some(w) } else { None }
    }).collect();
    assert_eq!(writes.len(), pids.len(), "one Write event per PID");

    // All must have after_hash set.
    for w in &writes {
        assert!(w.after_hash.is_some(), "after_hash must be set for every write");
    }
    // Each write has unique content → unique hash.
    let hashes: std::collections::HashSet<_> = writes.iter().map(|w| w.after_hash.as_ref().unwrap()).collect();
    assert_eq!(hashes.len(), writes.len(), "all after_hash values must be unique (unique content)");
}

// ── Test 10 — Pause-before-action via full runner ────────────────────────

/// PolicyGate with a pause_before unlink rule. The unlink stop triggers
/// a pending approval. A concurrent task denies it. The runner must
/// produce a `Blocked` event and not hang.
#[tokio::test]
async fn test10_pause_before_action_deny_blocks() {
    use crate::api::state::resolve_approval;
    use crate::events::ApprovalDecision;
    use crate::config::{MatchKind, Rule};

    let pid = 4100;
    let path_addr: u64 = 0x10000;

    let mut mock = MockPtraceThread::new();
    mock.add_memory(Pid::from_raw(pid), path_addr as usize, b"/workspace/critical.txt\0".to_vec());

    let stops = vec![
        entry(pid, nr::UNLINKAT, [libc::AT_FDCWD as u64, path_addr, 0, 0, 0, 0]),
    ];

    let mut rules = RuleSet::default();
    rules.pause_before.push(Rule::new(
        MatchKind::Unlink,
        vec!["/workspace/**".into()],
        vec![],
        vec![],
    ));
    rules.compile_patterns();

    let h = build_harness(mock, stops, rules);
    let shared = h.shared.clone();

    // Deny the approval from a concurrent task.
    tokio::spawn(async move {
        loop {
            if shared.pending_count() > 0 {
                let actions = shared.pending_actions();
                let id = actions[0].action_id.clone();
                resolve_approval(&shared, &id, ApprovalDecision::Deny);
                break;
            }
            tokio::task::yield_now().await;
        }
    });

    timeout(Duration::from_secs(5), h.runner.run())
        .await
        .expect("runner must not hang on denied approval");

    let events = h.sink.events();
    assert!(
        events.iter().any(|e| matches!(e.payload, EventPayload::Blocked(_))),
        "Blocked event must be produced when approval is denied"
    );
}

// ── Test 9 — Pause/Resume via runner ─────────────────────────────────────────

/// Pause flag set before run, cleared after 100 ms — runner completes within 2 s.
#[tokio::test]
async fn test9_pause_then_resume_does_not_hang() {
    let pid = 4000;
    let stops = vec![
        RawSyscallStop { pid: Pid::from_raw(pid), stop_type: StopType::Exec { pid: Pid::from_raw(pid) } },
        RawSyscallStop { pid: Pid::from_raw(pid), stop_type: StopType::Exit { pid: Pid::from_raw(pid), exit_code: 0 } },
    ];
    let h = build_harness(MockPtraceThread::new(), stops, RuleSet::default());
    let shared = h.shared.clone();
    shared.set_paused(true);

    let run_fut = h.runner.run();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        shared.set_paused(false);
    });

    timeout(Duration::from_secs(2), run_fut)
        .await
        .expect("runner must complete within timeout after resume");
    assert!(!h.sink.is_empty(), "events must be produced after resume");
}

// ── Test 11 — Tree snapshot after write ──────────────────────────────────────

/// After a file write the tree snapshot in `SharedState` must have file_count ≥ 1.
#[tokio::test]
async fn test11_tree_snapshot_reflects_writes() {
    let pid = 5000;
    let path_addr: u64 = 0x8000;
    let buf_addr: u64 = 0x9000;
    let path_bytes = b"/workspace/snap.txt\0";
    let content = b"snapshot content";
    let fd: i64 = 9;

    let mut mock = MockPtraceThread::new();
    mock.add_memory(Pid::from_raw(pid), path_addr as usize, path_bytes.to_vec());
    mock.add_memory(Pid::from_raw(pid), buf_addr as usize, content.to_vec());

    let stops = vec![
        entry(pid, nr::OPENAT, [libc::AT_FDCWD as u64, path_addr, libc::O_WRONLY as u64, 0o644, 0, 0]),
        exit_stop(pid, nr::OPENAT, fd),
        entry(pid, nr::WRITE, [fd as u64, buf_addr, content.len() as u64, 0, 0, 0]),
        RawSyscallStop { pid: Pid::from_raw(pid), stop_type: StopType::Exit { pid: Pid::from_raw(pid), exit_code: 0 } },
    ];
    let h = build_harness(mock, stops, RuleSet::default());
    let shared = h.shared.clone();
    h.runner.run().await;

    assert!(shared.load_tree().file_count() >= 1, "tree must reflect the write");
}

// ── Test 13 — Child process reaping ──────────────────────────────────────────

/// Fork 5 children, each exec + exit. All 6 Exit events produced; no hang.
#[tokio::test]
async fn test13_all_children_reaped_without_hang() {
    let parent = 6000i32;
    let children: [i32; 5] = [6001, 6002, 6003, 6004, 6005];
    let mut stops = Vec::new();
    for &child in &children {
        stops.push(RawSyscallStop {
            pid: Pid::from_raw(parent),
            stop_type: StopType::Fork { parent: Pid::from_raw(parent), child: Pid::from_raw(child) },
        });
        stops.push(RawSyscallStop { pid: Pid::from_raw(child), stop_type: StopType::Exec { pid: Pid::from_raw(child) } });
        stops.push(RawSyscallStop { pid: Pid::from_raw(child), stop_type: StopType::Exit { pid: Pid::from_raw(child), exit_code: 0 } });
    }
    stops.push(RawSyscallStop { pid: Pid::from_raw(parent), stop_type: StopType::Exit { pid: Pid::from_raw(parent), exit_code: 0 } });

    let h = build_harness(MockPtraceThread::new(), stops, RuleSet::default());
    timeout(Duration::from_secs(5), h.runner.run())
        .await
        .expect("runner must not hang");

    let exits: Vec<_> = h.sink.events().into_iter().filter(|e| matches!(e.payload, EventPayload::Exit(_))).collect();
    assert_eq!(exits.len(), 6, "5 children + 1 parent = 6 Exit events");
}
