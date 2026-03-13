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
    Blocked(control::Blocked),
    RulesUpdated(control::RulesUpdated),

    // Snapshot
    InitialFile(snapshot::InitialFile),
    InitialState(snapshot::InitialState),
    Checkpoint(snapshot::Checkpoint),
    MmapWarning(snapshot::MmapWarning),
}

impl EventPayload {
    /// Extract the tree_hash if this event carries one.
    pub fn tree_hash(&self) -> Option<&str> {
        match self {
            EventPayload::Write(e) => e.tree_hash.as_deref(),
            EventPayload::Rename(e) => e.tree_hash.as_deref(),
            EventPayload::Unlink(e) => e.tree_hash.as_deref(),
            EventPayload::Mkdir(e) => e.tree_hash.as_deref(),
            EventPayload::Rmdir(e) => e.tree_hash.as_deref(),
            EventPayload::Truncate(e) => e.tree_hash.as_deref(),
            EventPayload::Link(e) => e.tree_hash.as_deref(),
            EventPayload::Symlink(e) => e.tree_hash.as_deref(),
            EventPayload::InitialState(e) => e.tree_hash.as_deref(),
            EventPayload::Checkpoint(e) => e.tree_hash.as_deref(),
            _ => None,
        }
    }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vclock: Option<HashMap<String, u64>>,
    #[serde(flatten)]
    pub payload: EventPayload,
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

    /// Returns the current sequence value without incrementing.
    pub fn current(&self) -> u64 {
        self.next.load(Ordering::Relaxed)
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

impl EventPayload {
    /// Returns the serde discriminator tag for this variant.
    pub fn event_type_tag(&self) -> &'static str {
        match self {
            Self::Exec(_) => "exec",
            Self::Fork(_) => "fork",
            Self::Exit(_) => "exit",
            Self::Read(_) => "read",
            Self::Write(_) => "write",
            Self::Rename(_) => "rename",
            Self::Unlink(_) => "unlink",
            Self::Mkdir(_) => "mkdir",
            Self::Rmdir(_) => "rmdir",
            Self::Chmod(_) => "chmod",
            Self::Truncate(_) => "truncate",
            Self::Link(_) => "link",
            Self::Symlink(_) => "symlink",
            Self::Stdio(_) => "stdio",
            Self::PipeCreate(_) => "pipe_create",
            Self::PipeData(_) => "pipe_data",
            Self::PipeClose(_) => "pipe_close",
            Self::PtyCreate(_) => "pty_create",
            Self::PtyData(_) => "pty_data",
            Self::FdRedirect(_) => "fd_redirect",
            Self::Socket(_) => "socket",
            Self::Connect(_) => "connect",
            Self::Accept(_) => "accept",
            Self::TlsKeys(_) => "tls_keys",
            Self::HttpRequest(_) => "http_request",
            Self::HttpResponse(_) => "http_response",
            Self::AgentStart(_) => "agent_start",
            Self::AgentPause(_) => "agent_pause",
            Self::AgentResume(_) => "agent_resume",
            Self::PendingApproval(_) => "pending_approval",
            Self::ApprovalGranted(_) => "approval_granted",
            Self::ApprovalDenied(_) => "approval_denied",
            Self::Blocked(_) => "blocked",
            Self::RulesUpdated(_) => "rules_updated",
            Self::InitialFile(_) => "initial_file",
            Self::InitialState(_) => "initial_state",
            Self::Checkpoint(_) => "checkpoint",
            Self::MmapWarning(_) => "mmap_warning",
        }
    }

    /// Returns the primary PID associated with this event, if any.
    pub fn pid(&self) -> Option<u32> {
        match self {
            Self::Exec(e) => Some(e.pid),
            Self::Fork(f) => Some(f.parent_pid),
            Self::Exit(e) => Some(e.pid),
            Self::Read(r) => Some(r.pid),
            Self::Write(w) => Some(w.pid),
            Self::Rename(r) => Some(r.pid),
            Self::Unlink(u) => Some(u.pid),
            Self::Mkdir(m) => Some(m.pid),
            Self::Rmdir(r) => Some(r.pid),
            Self::Chmod(c) => Some(c.pid),
            Self::Truncate(t) => Some(t.pid),
            Self::Link(l) => Some(l.pid),
            Self::Symlink(s) => Some(s.pid),
            Self::Stdio(s) => Some(s.pid),
            Self::PipeCreate(p) => Some(p.pid),
            Self::PipeData(p) => Some(p.pid),
            Self::PipeClose(p) => Some(p.pid),
            Self::PtyCreate(p) => Some(p.pid),
            Self::PtyData(p) => Some(p.pid),
            Self::FdRedirect(f) => Some(f.pid),
            Self::Socket(s) => Some(s.pid),
            Self::Connect(c) => Some(c.pid),
            Self::Accept(a) => Some(a.pid),
            Self::TlsKeys(t) => Some(t.pid),
            Self::HttpRequest(r) => Some(r.pid),
            Self::HttpResponse(r) => Some(r.pid),
            Self::PendingApproval(p) => Some(p.pid),
            Self::ApprovalGranted(a) => Some(a.pid),
            Self::ApprovalDenied(a) => Some(a.pid),
            Self::Blocked(b) => Some(b.pid),
            Self::MmapWarning(m) => Some(m.pid),
            Self::InitialFile(f) => Some(f.pid),
            Self::AgentStart(_)
            | Self::AgentPause(_)
            | Self::AgentResume(_)
            | Self::RulesUpdated(_)
            | Self::InitialState(_)
            | Self::Checkpoint(_) => None,
        }
    }

    /// Returns true if this event mutates the filesystem state.
    ///
    /// Used by TreeStage to decide whether to update the Merkle tree.
    pub fn is_mutating(&self) -> bool {
        matches!(
            self,
            EventPayload::Write(_)
                | EventPayload::Rename(_)
                | EventPayload::Unlink(_)
                | EventPayload::Mkdir(_)
                | EventPayload::Rmdir(_)
                | EventPayload::Chmod(_)
                | EventPayload::Truncate(_)
                | EventPayload::Link(_)
                | EventPayload::Symlink(_)
                | EventPayload::InitialFile(_)
        )
    }

    /// Returns filesystem paths referenced by this event.
    ///
    /// Most events return zero or one path. `Rename` returns both
    /// the old and new paths.
    pub fn paths(&self) -> Vec<&str> {
        match self {
            Self::Read(r) => vec![&r.path],
            Self::Write(w) => vec![&w.path],
            Self::Rename(r) => vec![&r.old_path, &r.new_path],
            Self::Unlink(u) => vec![&u.path],
            Self::Mkdir(m) => vec![&m.path],
            Self::Rmdir(r) => vec![&r.path],
            Self::Chmod(c) => vec![&c.path],
            Self::Truncate(t) => vec![&t.path],
            Self::Link(l) => vec![&l.target, &l.link_path],
            Self::Symlink(s) => vec![&s.target, &s.link_path],
            Self::MmapWarning(m) => vec![&m.path],
            Self::InitialFile(f) => vec![&f.path],
            Self::Blocked(b) => b.path.as_deref().into_iter().collect(),
            _ => vec![],
        }
    }
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
            vclock: None,
            payload,
        }
    }
}

#[cfg(test)]
#[path = "envelope_tests.rs"]
mod tests;
