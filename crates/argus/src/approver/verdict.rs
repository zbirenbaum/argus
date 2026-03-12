// Rust guideline compliant 2026-02-21
//! Approval verdict returned by each approver.

use serde::{Deserialize, Serialize};

/// Decision rendered by an approver.
///
/// Three outcomes: allow the syscall, deny it (inject EPERM), or
/// escalate to the next approver in the chain. If every approver
/// escalates, the last approver in the chain must produce a terminal
/// verdict (the human API endpoint blocks until a decision arrives).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "lowercase")]
pub enum Verdict {
    /// Allow the syscall to proceed.
    Allow {
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        approver: String,
    },

    /// Deny the syscall — inject EPERM.
    Deny {
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        approver: String,
    },

    /// Pass the decision to the next approver in the chain.
    ///
    /// Typical use: an LLM judge whose confidence is below threshold.
    Escalate {
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        approver: String,
    },
}

impl Verdict {
    /// Construct an allow verdict.
    pub fn allow(reason: impl Into<String>, approver: impl Into<String>) -> Self {
        Self::Allow {
            reason: Some(reason.into()),
            approver: approver.into(),
        }
    }

    /// Construct a deny verdict.
    pub fn deny(reason: impl Into<String>, approver: impl Into<String>) -> Self {
        Self::Deny {
            reason: Some(reason.into()),
            approver: approver.into(),
        }
    }

    /// Construct a deny verdict with no reason.
    pub fn deny_no_reason(approver: impl Into<String>) -> Self {
        Self::Deny {
            reason: None,
            approver: approver.into(),
        }
    }

    /// Construct an escalation verdict.
    pub fn escalate(reason: impl Into<String>, approver: impl Into<String>) -> Self {
        Self::Escalate {
            reason: Some(reason.into()),
            approver: approver.into(),
        }
    }

    /// Whether this verdict allows the syscall.
    pub fn is_allow(&self) -> bool {
        matches!(self, Self::Allow { .. })
    }

    /// Whether this verdict denies the syscall.
    pub fn is_deny(&self) -> bool {
        matches!(self, Self::Deny { .. })
    }

    /// Whether this verdict escalates to the next approver.
    pub fn is_escalate(&self) -> bool {
        matches!(self, Self::Escalate { .. })
    }

    /// Whether this is a terminal verdict (not an escalation).
    pub fn is_terminal(&self) -> bool {
        !self.is_escalate()
    }

    /// Optional explanation for the decision.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Allow { reason, .. }
            | Self::Deny { reason, .. }
            | Self::Escalate { reason, .. } => reason.as_deref(),
        }
    }

    /// Identity of the approver that rendered this verdict.
    pub fn approver(&self) -> &str {
        match self {
            Self::Allow { approver, .. }
            | Self::Deny { approver, .. }
            | Self::Escalate { approver, .. } => approver,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_verdict() {
        let v = Verdict::allow("safe operation", "llm-judge");
        assert!(v.is_allow());
        assert!(!v.is_deny());
        assert!(!v.is_escalate());
        assert!(v.is_terminal());
        assert_eq!(v.reason(), Some("safe operation"));
        assert_eq!(v.approver(), "llm-judge");
    }

    #[test]
    fn deny_verdict() {
        let v = Verdict::deny("deleting production data", "human");
        assert!(v.is_deny());
        assert!(v.is_terminal());
        assert_eq!(v.reason(), Some("deleting production data"));
    }

    #[test]
    fn deny_no_reason() {
        let v = Verdict::deny_no_reason("system");
        assert!(v.is_deny());
        assert_eq!(v.reason(), None);
    }

    #[test]
    fn escalate_verdict() {
        let v = Verdict::escalate("confidence 0.6 < threshold 0.8", "llm-judge");
        assert!(v.is_escalate());
        assert!(!v.is_terminal());
        assert_eq!(v.reason(), Some("confidence 0.6 < threshold 0.8"));
        assert_eq!(v.approver(), "llm-judge");
    }

    #[test]
    fn serde_round_trip_all_variants() {
        let cases = [
            Verdict::allow("ok", "test"),
            Verdict::deny("bad", "test"),
            Verdict::escalate("unsure", "test"),
            Verdict::deny_no_reason("system"),
        ];
        for v in &cases {
            let json = serde_json::to_string(v).unwrap();
            let back: Verdict = serde_json::from_str(&json).unwrap();
            assert_eq!(*v, back, "round-trip failed for: {json}");
        }
    }

    #[test]
    fn serde_uses_decision_tag() {
        let v = Verdict::escalate("unsure", "llm");
        let json = serde_json::to_string(&v).unwrap();
        assert!(json.contains("\"decision\":\"escalate\""), "got: {json}");
    }
}
