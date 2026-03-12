// Rust guideline compliant 2026-02-21
//! Event envelope with sequence generation and timestamps.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use super::control;
use super::file;
use super::io;
use super::network;
use super::process;
use super::snapshot;

/// Tagged union of all supervisor event payloads.
///
/// Serializes with a `"type"` discriminator field using snake_case names,
/// so each variant flattens its fields alongside `"type": "variant_name"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventPayload {
    // Process
    Exec(process::Exec),
    Fork(process::Fork),
    Exit(process::Exit),

    // File content
    Read(file::Read),
    Write(file::Write),

    // File metadata
    Rename(file::Rename),
    Unlink(file::Unlink),
    Mkdir(file::Mkdir),
    Rmdir(file::Rmdir),
    Chmod(file::Chmod),
    Truncate(file::Truncate),
    Link(file::Link),
    Symlink(file::Symlink),

    // Stdio / pipe / PTY
    Stdio(io::Stdio),
    PipeCreate(io::PipeCreate),
    PipeData(io::PipeData),
    PipeClose(io::PipeClose),
    PtyCreate(io::PtyCreate),
    PtyData(io::PtyData),
    FdRedirect(io::FdRedirect),

    // Network
    Socket(network::Socket),
    Connect(network::Connect),
    Accept(network::Accept),
    TlsKeys(network::TlsKeys),
    HttpRequest(network::HttpRequest),
    HttpResponse(network::HttpResponse),

    // Control
    AgentStart(control::AgentStart),
    AgentPause(control::AgentPause),
    AgentResume(control::AgentResume),
    PendingApproval(control::PendingApproval),
    ApprovalGranted(control::ApprovalGranted),
    ApprovalDenied(control::ApprovalDenied),

    // Snapshot
    InitialState(snapshot::InitialState),
    Checkpoint(snapshot::Checkpoint),
    MmapWarning(snapshot::MmapWarning),
}

/// Immutable event record emitted by the supervisor.
///
/// Contains dual timestamps for local ordering and cross-agent correlation,
/// a monotonic sequence number, and the event payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub seq: u64,
    pub ts_monotonic: u64,
    pub ts_wall: String,
    pub agent_id: String,
    #[serde(flatten)]
    pub payload: EventPayload,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vclock: Option<HashMap<String, u64>>,
}

/// Thread-safe monotonic sequence number generator.
///
/// Uses `AtomicU64` with `Relaxed` ordering since the sequence is only
/// required to be unique and monotonic within a single agent, and the
/// atomic increment itself guarantees that.
#[derive(Debug)]
pub struct SequenceGenerator {
    next: AtomicU64,
}

impl SequenceGenerator {
    /// Creates a generator starting at the given value.
    pub fn new(start: u64) -> Self {
        Self {
            next: AtomicU64::new(start),
        }
    }

    /// Returns the next sequence number, incrementing atomically.
    pub fn next_seq(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }
}

impl Default for SequenceGenerator {
    fn default() -> Self {
        Self::new(0)
    }
}

/// Returns a `(ts_monotonic, ts_wall)` timestamp pair.
///
/// `ts_monotonic` comes from `CLOCK_MONOTONIC` via `std::time::Instant`
/// mapped to nanoseconds since an arbitrary epoch. On Linux the actual
/// `CLOCK_MONOTONIC_RAW` will be used in the tracer; this portable
/// implementation is sufficient for tests and non-ptrace paths.
///
/// `ts_wall` is RFC 3339 with nanosecond precision from `chrono::Utc`.
pub fn timestamp_pair() -> (u64, String) {
    // Monotonic nanos — uses libc for CLOCK_MONOTONIC_RAW when available
    let mono = monotonic_nanos();
    let wall = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
    (mono, wall)
}

/// Reads `CLOCK_MONOTONIC` nanoseconds via `std::time::Instant`.
///
/// On the actual Linux target the tracer will use `CLOCK_MONOTONIC_RAW`
/// directly via libc; this keeps the events crate portable for tests.
fn monotonic_nanos() -> u64 {
    use std::time::Instant;

    // Lazy-initialized epoch so values are relative to process start.
    use std::sync::OnceLock;
    static EPOCH: OnceLock<Instant> = OnceLock::new();

    let epoch = EPOCH.get_or_init(Instant::now);
    let elapsed = epoch.elapsed();
    elapsed.as_nanos() as u64
}

impl Event {
    /// Constructs an event with auto-filled seq and timestamps.
    pub fn new(
        seq_gen: &SequenceGenerator,
        agent_id: String,
        payload: EventPayload,
    ) -> Self {
        let (ts_monotonic, ts_wall) = timestamp_pair();
        Self {
            seq: seq_gen.next_seq(),
            ts_monotonic,
            ts_wall,
            agent_id,
            payload,
            vclock: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seq_generator_increments() {
        let seq_gen = SequenceGenerator::new(0);
        assert_eq!(seq_gen.next_seq(), 0);
        assert_eq!(seq_gen.next_seq(), 1);
        assert_eq!(seq_gen.next_seq(), 2);
    }

    #[test]
    fn seq_generator_custom_start() {
        let seq_gen = SequenceGenerator::new(100);
        assert_eq!(seq_gen.next_seq(), 100);
        assert_eq!(seq_gen.next_seq(), 101);
    }

    #[test]
    fn event_auto_fills_fields() {
        let seq_gen = SequenceGenerator::default();
        let payload = EventPayload::Fork(process::Fork {
            parent_pid: 1,
            child_pid: 2,
        });
        let event = Event::new(&seq_gen, "test-agent".into(), payload);

        assert_eq!(event.seq, 0);
        assert_eq!(event.agent_id, "test-agent");
        assert!(event.ts_wall.contains('T'));
        assert!(event.vclock.is_none());
    }

    #[test]
    fn event_sequential_seq() {
        let seq_gen = SequenceGenerator::default();
        let mk = || {
            EventPayload::Exit(process::Exit {
                pid: 1,
                exit_code: 0,
                signal: None,
            })
        };
        let e1 = Event::new(&seq_gen, "a".into(), mk());
        let e2 = Event::new(&seq_gen, "a".into(), mk());
        let e3 = Event::new(&seq_gen, "a".into(), mk());
        assert_eq!(e1.seq, 0);
        assert_eq!(e2.seq, 1);
        assert_eq!(e3.seq, 2);
    }

    #[test]
    fn timestamp_monotonicity() {
        let (t1, _) = timestamp_pair();
        let (t2, _) = timestamp_pair();
        // t2 should be >= t1 (could be equal if called very fast)
        assert!(t2 >= t1);
    }

    #[test]
    fn timestamp_wall_is_rfc3339() {
        let (_, wall) = timestamp_pair();
        // Must parse back as a valid DateTime
        chrono::DateTime::parse_from_rfc3339(&wall)
            .expect("ts_wall must be valid RFC 3339");
    }

    #[test]
    fn event_json_has_type_field() {
        let seq_gen = SequenceGenerator::default();
        let event = Event::new(
            &seq_gen,
            "test".into(),
            EventPayload::Fork(process::Fork {
                parent_pid: 1,
                child_pid: 2,
            }),
        );
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"fork\""));
        assert!(json.contains("\"parent_pid\":1"));
        assert!(json.contains("\"child_pid\":2"));
        assert!(json.contains("\"seq\":0"));
        assert!(json.contains("\"agent_id\":\"test\""));
    }

    #[test]
    fn event_round_trip_write() {
        let seq_gen = SequenceGenerator::default();
        let event = Event::new(
            &seq_gen,
            "agent-1".into(),
            EventPayload::Write(file::Write {
                pid: 42,
                path: "/workspace/output.csv".into(),
                fd: 3,
                offset: 0,
                size: 4096,
                before_hash: Some("ab12".into()),
                after_hash: Some("cd34".into()),
                tree_hash: Some("ef56".into()),
            }),
        );
        let json = serde_json::to_string(&event).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(event.seq, back.seq);
        assert_eq!(event.agent_id, back.agent_id);
        assert_eq!(event.payload, back.payload);
    }

    #[test]
    fn event_vclock_serialization() {
        let seq_gen = SequenceGenerator::default();
        let mut event = Event::new(
            &seq_gen,
            "a".into(),
            EventPayload::Fork(process::Fork {
                parent_pid: 1,
                child_pid: 2,
            }),
        );

        // Without vclock — field omitted
        let json = serde_json::to_string(&event).unwrap();
        assert!(!json.contains("vclock"));

        // With vclock — field present
        let mut vc = HashMap::new();
        vc.insert("a".into(), 5);
        vc.insert("b".into(), 3);
        event.vclock = Some(vc);
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("vclock"));
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(event.vclock, back.vclock);
    }

    #[test]
    fn all_variants_round_trip() {
        let seq_gen = SequenceGenerator::default();
        let agent = "rt".to_string();

        let payloads: Vec<EventPayload> = vec![
            EventPayload::Exec(process::Exec {
                pid: 1, ppid: 0, binary: "/bin/sh".into(),
                argv: vec![], envp: vec![], cwd: "/".into(),
            }),
            EventPayload::Fork(process::Fork { parent_pid: 1, child_pid: 2 }),
            EventPayload::Exit(process::Exit { pid: 1, exit_code: 0, signal: None }),
            EventPayload::Read(file::Read {
                pid: 1, path: "/f".into(), fd: 3, offset: 0, size: 10,
                content_hash: None,
            }),
            EventPayload::Write(file::Write {
                pid: 1, path: "/f".into(), fd: 3, offset: 0, size: 10,
                before_hash: None, after_hash: None, tree_hash: None,
            }),
            EventPayload::Rename(file::Rename {
                pid: 1, old_path: "/a".into(), new_path: "/b".into(), tree_hash: None,
            }),
            EventPayload::Unlink(file::Unlink {
                pid: 1, path: "/f".into(), content_hash: None, tree_hash: None,
            }),
            EventPayload::Mkdir(file::Mkdir { pid: 1, path: "/d".into(), tree_hash: None }),
            EventPayload::Rmdir(file::Rmdir { pid: 1, path: "/d".into(), tree_hash: None }),
            EventPayload::Chmod(file::Chmod {
                pid: 1, path: "/f".into(), old_mode: 0o644, new_mode: 0o755,
            }),
            EventPayload::Truncate(file::Truncate {
                pid: 1, path: "/f".into(), old_size: 100, new_size: 0,
                before_hash: None, after_hash: None, tree_hash: None,
            }),
            EventPayload::Link(file::Link {
                pid: 1, target: "/a".into(), link_path: "/b".into(), tree_hash: None,
            }),
            EventPayload::Symlink(file::Symlink {
                pid: 1, target: "/a".into(), link_path: "/s".into(), tree_hash: None,
            }),
            EventPayload::Stdio(io::Stdio {
                pid: 1, subtype: io::StdioSubtype::Stdout,
                content_hash: None, size: 5, pipe_inode: None,
                dest_pid: None, source_pid: None,
            }),
            EventPayload::PipeCreate(io::PipeCreate {
                pid: 1, inode: 99, read_fd: 3, write_fd: 4,
            }),
            EventPayload::PipeData(io::PipeData {
                pid: 1, inode: 99, direction: io::PipeDirection::Write,
                content_hash: None, size: 10, dest_pids: vec![],
            }),
            EventPayload::PipeClose(io::PipeClose {
                pid: 1, inode: 99, direction: io::PipeDirection::Read,
            }),
            EventPayload::PtyCreate(io::PtyCreate {
                pid: 1, master_fd: 5, slave_path: "/dev/pts/0".into(),
            }),
            EventPayload::PtyData(io::PtyData {
                pid: 1, subtype: io::PtySubtype::MasterRead,
                content_hash: None, size: 8, slave_path: "/dev/pts/0".into(),
            }),
            EventPayload::FdRedirect(io::FdRedirect {
                pid: 1, fd: 1, target: io::FdTarget {
                    target_type: "file".into(), inode: None,
                    path: Some("/tmp/out".into()), direction: None,
                },
            }),
            EventPayload::Socket(network::Socket {
                pid: 1, domain: "AF_INET".into(), sock_type: "SOCK_STREAM".into(), fd: 5,
            }),
            EventPayload::Connect(network::Connect {
                pid: 1, fd: 5, remote_addr: "1.2.3.4".into(), remote_port: 80,
            }),
            EventPayload::Accept(network::Accept {
                pid: 1, fd: 6, peer_addr: "5.6.7.8".into(), peer_port: 9999,
            }),
            EventPayload::TlsKeys(network::TlsKeys {
                pid: 1, fd: 5, sni: None, keylog_line_hash: None,
            }),
            EventPayload::HttpRequest(network::HttpRequest {
                pid: 1, method: "GET".into(), url: "/".into(),
                headers_hash: None, body_hash: None, status: None,
            }),
            EventPayload::HttpResponse(network::HttpResponse {
                pid: 1, status: 200, headers_hash: None, body_hash: None,
            }),
            EventPayload::AgentStart(control::AgentStart {
                agent_id: "a".into(), config_summary: "s".into(),
                node: None, pod: None,
            }),
            EventPayload::AgentPause(control::AgentPause {
                reason: "r".into(), stopped_pids: vec![1],
            }),
            EventPayload::AgentResume(control::AgentResume { resumed_pids: vec![1] }),
            EventPayload::PendingApproval(control::PendingApproval {
                pid: 1, syscall: "execve".into(), path: None,
                binary: Some("/bin/rm".into()), rule_name: "no_rm".into(),
            }),
            EventPayload::ApprovalGranted(control::ApprovalGranted {
                pid: 1, rule_name: "no_rm".into(), approver: "admin".into(),
            }),
            EventPayload::ApprovalDenied(control::ApprovalDenied {
                pid: 1, rule_name: "no_rm".into(), approver: "admin".into(),
            }),
            EventPayload::InitialState(snapshot::InitialState {
                tree_hash: None, file_count: 10, total_size: 1024,
            }),
            EventPayload::Checkpoint(snapshot::Checkpoint {
                seq: 50, tree_hash: None,
                checkpoint_s3_key: "ck/a/50.bin".into(),
            }),
            EventPayload::MmapWarning(snapshot::MmapWarning {
                pid: 1, path: "/f".into(), fd: 3, prot: 3, flags: 1,
            }),
        ];

        for payload in payloads {
            let event = Event::new(&seq_gen, agent.clone(), payload);
            let json = serde_json::to_string(&event).unwrap();
            let back: Event = serde_json::from_str(&json).unwrap();
            assert_eq!(event.seq, back.seq);
            assert_eq!(event.agent_id, back.agent_id);
            assert_eq!(event.payload, back.payload);
        }
    }
}
