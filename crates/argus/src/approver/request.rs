// Rust guideline compliant 2026-02-21
//! Approval request passed to each approver.

use serde::{Deserialize, Serialize};

/// Context for a paused syscall awaiting approval.
///
/// Contains everything an approver needs to make a decision. Fields
/// mirror the information available at the ptrace syscall-entry stop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    /// Unique identifier for this pending action.
    pub action_id: String,

    /// PID of the blocked tracee.
    pub pid: u32,

    /// Executable name of the blocked process.
    pub process: String,

    /// Syscall category that triggered the rule (e.g. "unlink", "exec").
    pub syscall: String,

    /// Resolved filesystem path, if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,

    /// Binary being executed, if this is an exec syscall.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,

    /// Network destination, if this is a connect syscall.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,

    /// Human-readable description of the rule that matched.
    pub rule_description: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_json() {
        let req = ApprovalRequest {
            action_id: "abc-123".into(),
            pid: 42,
            process: "python".into(),
            syscall: "unlink".into(),
            path: Some("/workspace/data.csv".into()),
            binary: None,
            destination: None,
            rule_description: "unlink /workspace/**".into(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("\"binary\""));
        assert!(!json.contains("\"destination\""));
        let back: ApprovalRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.pid, 42);
        assert_eq!(back.path.as_deref(), Some("/workspace/data.csv"));
    }

    #[test]
    fn all_fields_present() {
        let req = ApprovalRequest {
            action_id: "xyz".into(),
            pid: 1,
            process: "bash".into(),
            syscall: "exec".into(),
            path: None,
            binary: Some("rm".into()),
            destination: None,
            rule_description: "exec rm".into(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"binary\""));
    }
}
