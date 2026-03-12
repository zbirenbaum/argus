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
