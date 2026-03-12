// Rust guideline compliant 2026-02-21
//! Agent control event payloads.

use serde::{Deserialize, Serialize};

/// Agent supervisor started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentStart {
    /// Duplicates envelope `agent_id` for self-contained event payloads.
    #[serde(rename = "start_agent_id")]
    pub agent_id: String,
    pub config_summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pod: Option<String>,
}

/// Agent was paused by operator or rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentPause {
    pub reason: String,
    pub stopped_pids: Vec<u32>,
}

/// Agent was resumed after a pause.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentResume {
    pub resumed_pids: Vec<u32>,
}

/// A syscall is awaiting approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingApproval {
    pub pid: u32,
    pub syscall: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
    pub rule_name: String,
}

/// A pending approval was granted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalGranted {
    pub pid: u32,
    pub rule_name: String,
    pub approver: String,
}

/// A pending approval was denied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalDenied {
    pub pid: u32,
    pub rule_name: String,
    pub approver: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_start_round_trip() {
        let s = AgentStart {
            agent_id: "researcher-abc".into(),
            config_summary: "default".into(),
            node: Some("node-1".into()),
            pod: Some("argus-xyz".into()),
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: AgentStart = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn pause_resume_round_trip() {
        let p = AgentPause {
            reason: "user request".into(),
            stopped_pids: vec![10, 11, 12],
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: AgentPause = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);

        let r = AgentResume {
            resumed_pids: vec![10, 11, 12],
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: AgentResume = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn approval_flow_round_trip() {
        let pending = PendingApproval {
            pid: 42,
            syscall: "execve".into(),
            path: None,
            binary: Some("/usr/bin/rm".into()),
            rule_name: "no_rm".into(),
        };
        let json = serde_json::to_string(&pending).unwrap();
        assert!(!json.contains("\"path\""));
        let back: PendingApproval = serde_json::from_str(&json).unwrap();
        assert_eq!(pending, back);

        let granted = ApprovalGranted {
            pid: 42,
            rule_name: "no_rm".into(),
            approver: "admin@example.com".into(),
        };
        let json = serde_json::to_string(&granted).unwrap();
        let back: ApprovalGranted = serde_json::from_str(&json).unwrap();
        assert_eq!(granted, back);

        let denied = ApprovalDenied {
            pid: 42,
            rule_name: "no_rm".into(),
            approver: "admin@example.com".into(),
        };
        let json = serde_json::to_string(&denied).unwrap();
        let back: ApprovalDenied = serde_json::from_str(&json).unwrap();
        assert_eq!(denied, back);
    }
}
