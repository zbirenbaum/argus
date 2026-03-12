// Rust guideline compliant 2026-02-21
//! Request and response types for the supervisor REST API.
//!
//! All types derive `Serialize` and `Deserialize` for JSON wire format.

use serde::{Deserialize, Serialize};

use crate::events::ApprovalDecision;

/// Response body for `POST /agent/pause`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PauseResponse {
    /// Always `"paused"`.
    pub status: String,
    /// Processes that were stopped.
    pub stopped_processes: Vec<ProcessInfo>,
}

/// Response body for `POST /agent/resume`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeResponse {
    /// Always `"running"`.
    pub status: String,
    /// Number of processes that were resumed.
    pub resumed_count: u32,
}

/// Response body for `GET /agent/status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    /// One of `"running"` or `"paused"`.
    pub status: String,
    /// Unique agent identifier.
    pub agent_id: String,
    /// Seconds since the supervisor started.
    pub uptime_seconds: f64,
    /// Total events emitted so far.
    pub event_count: u64,
    /// Currently tracked processes.
    pub processes: Vec<ProcessInfo>,
}

/// Abbreviated process info for API responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessInfo {
    /// OS process ID.
    pub pid: u32,
    /// Executable path or name.
    pub binary: String,
    /// Process state as seen by the supervisor.
    pub state: String,
}

/// Response body for `GET /approvals/pending`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingApprovalsResponse {
    /// List of actions awaiting a decision.
    pub pending: Vec<PendingAction>,
}

/// A single action awaiting approval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingAction {
    /// Unique identifier for this pending action.
    pub action_id: String,
    /// PID of the blocked tracee.
    pub pid: u32,
    /// Executable name of the blocked process.
    pub process: String,
    /// Syscall category that triggered the rule.
    pub syscall: String,
    /// Resolved filesystem path, if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// ISO 8601 timestamp when the action was blocked.
    pub timestamp: String,
    /// Description of the rule that matched.
    pub rule_matched: String,
}

/// Response body for `POST /approvals/{action_id}/approve`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApproveResponse {
    /// The action that was approved.
    pub action_id: String,
    /// Always `"approved"`.
    pub result: String,
    /// PID that was unblocked.
    pub pid: u32,
}

/// Response body for `POST /approvals/{action_id}/deny`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DenyResponse {
    /// The action that was denied.
    pub action_id: String,
    /// Always `"denied"`.
    pub result: String,
    /// PID that was unblocked with `EPERM`.
    pub pid: u32,
    /// The errno that was injected.
    pub injected_errno: String,
}

/// Response body for `POST /rules`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RulesAppliedResponse {
    /// Always `true` on success.
    pub applied: bool,
    /// Total rules in the new rule set.
    pub rule_count: usize,
}

/// Health check response for `GET /health`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    /// Always `"ok"`.
    pub status: String,
    /// Agent identifier.
    pub agent_id: String,
    /// Total events emitted.
    pub event_count: u64,
}

/// Request body for `POST /restore`.
#[derive(Debug, Clone, Deserialize)]
pub struct RestoreRequest {
    /// Wall-clock timestamp to restore to.
    #[serde(default)]
    pub timestamp: Option<String>,
    /// Event sequence number to restore to.
    #[serde(default)]
    pub seq: Option<u64>,
    /// Restore mode: `"full"` or `"selective"`.
    pub mode: String,
    /// Target directory for full restore output.
    #[serde(default)]
    pub target: Option<String>,
    /// Single path for selective restore.
    #[serde(default)]
    pub path: Option<String>,
    /// Multiple paths for selective restore.
    #[serde(default)]
    pub paths: Option<Vec<String>>,
}

/// Response body for `POST /restore`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreResponse {
    /// Sequence number of the restored snapshot.
    pub restored_to_seq: u64,
    /// Wall-clock timestamp of the restored snapshot.
    pub restored_to_ts: String,
    /// Root tree hash of the restored snapshot.
    pub tree_hash: String,
    /// Number of files written.
    pub files_restored: u64,
    /// Total bytes written.
    pub bytes_restored: u64,
}

/// Response body for `GET /tree`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeSnapshotResponse {
    /// Current root tree hash.
    pub tree_hash: String,
    /// Current event sequence number.
    pub seq: u64,
    /// Number of tracked files.
    pub file_count: usize,
    /// All tracked files with their content hashes.
    pub files: Vec<TreeFileEntry>,
}

/// Single file entry in a tree snapshot response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeFileEntry {
    /// Relative path within the workspace.
    pub path: String,
    /// Content hash (SHA-256).
    pub hash: String,
}

/// Internal pending approval stored in shared state.
#[derive(Debug)]
pub struct PendingApprovalEntry {
    /// Unique identifier for this action.
    pub action_id: String,
    /// PID of the blocked tracee.
    pub pid: u32,
    /// Executable name.
    pub process: String,
    /// Syscall category string.
    pub syscall: String,
    /// Resolved path, if any.
    pub path: Option<String>,
    /// When this action was blocked.
    pub timestamp: String,
    /// Human-readable rule description.
    pub rule_matched: String,
    /// Channel to deliver the decision to the tracer thread.
    pub decision_tx: Option<tokio::sync::oneshot::Sender<ApprovalDecision>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_response_roundtrip() {
        let resp = PauseResponse {
            status: "paused".into(),
            stopped_processes: vec![ProcessInfo {
                pid: 42,
                binary: "/usr/bin/python".into(),
                state: "stopped".into(),
            }],
        };
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: PauseResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.status, "paused");
        assert_eq!(parsed.stopped_processes.len(), 1);
    }

    #[test]
    fn resume_response_roundtrip() {
        let resp = ResumeResponse {
            status: "running".into(),
            resumed_count: 3,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: ResumeResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.resumed_count, 3);
    }

    #[test]
    fn status_response_roundtrip() {
        let resp = StatusResponse {
            status: "running".into(),
            agent_id: "agent-1".into(),
            uptime_seconds: 42.5,
            event_count: 100,
            processes: vec![],
        };
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: StatusResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.agent_id, "agent-1");
    }

    #[test]
    fn pending_action_optional_path() {
        let action = PendingAction {
            action_id: "a1".into(),
            pid: 10,
            process: "python".into(),
            syscall: "exec".into(),
            path: None,
            timestamp: "2026-01-01T00:00:00Z".into(),
            rule_matched: "exec rule".into(),
        };
        let json = serde_json::to_string(&action).unwrap();
        assert!(!json.contains("path"));
    }

    #[test]
    fn health_response_roundtrip() {
        let resp = HealthResponse {
            status: "ok".into(),
            agent_id: "a1".into(),
            event_count: 0,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: HealthResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.status, "ok");
    }
}
