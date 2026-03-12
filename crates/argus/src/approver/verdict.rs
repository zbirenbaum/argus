// Rust guideline compliant 2026-02-21
//! Approval verdict returned by each approver.

use serde::{Deserialize, Serialize};

/// Decision rendered by an approver.
///
/// Carries the allow/deny decision, an optional human-readable reason,
/// and the identity of the approver that made the decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    decision: Decision,
    reason: Option<String>,
    approver: String,
}

/// Binary allow/deny outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Allow,
    Deny,
}

impl Verdict {
    /// Construct an allow verdict.
    pub fn allow(reason: impl Into<String>, approver: impl Into<String>) -> Self {
        Self {
            decision: Decision::Allow,
            reason: Some(reason.into()),
            approver: approver.into(),
        }
    }

    /// Construct a deny verdict.
    pub fn deny(reason: impl Into<String>, approver: impl Into<String>) -> Self {
        Self {
            decision: Decision::Deny,
            reason: Some(reason.into()),
            approver: approver.into(),
        }
    }

    /// Construct a deny verdict with no reason (e.g. all approvers failed).
    pub fn deny_no_reason(approver: impl Into<String>) -> Self {
        Self {
            decision: Decision::Deny,
            reason: None,
            approver: approver.into(),
        }
    }

    /// Whether this verdict allows the syscall.
    pub fn is_allow(&self) -> bool {
        self.decision == Decision::Allow
    }

    /// Whether this verdict denies the syscall.
    pub fn is_deny(&self) -> bool {
        self.decision == Decision::Deny
    }

    /// The decision enum value.
    pub fn decision(&self) -> Decision {
        self.decision
    }

    /// Optional explanation for the decision.
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    /// Identity of the approver that rendered this verdict.
    pub fn approver(&self) -> &str {
        &self.approver
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
        assert_eq!(v.reason(), Some("safe operation"));
        assert_eq!(v.approver(), "llm-judge");
    }

    #[test]
    fn deny_verdict() {
        let v = Verdict::deny("deleting production data", "human");
        assert!(v.is_deny());
        assert_eq!(v.reason(), Some("deleting production data"));
    }

    #[test]
    fn deny_no_reason() {
        let v = Verdict::deny_no_reason("system");
        assert!(v.is_deny());
        assert_eq!(v.reason(), None);
    }

    #[test]
    fn serde_round_trip() {
        let v = Verdict::allow("ok", "test");
        let json = serde_json::to_string(&v).unwrap();
        let back: Verdict = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn decision_variants() {
        assert_eq!(Verdict::allow("", "").decision(), Decision::Allow);
        assert_eq!(Verdict::deny("", "").decision(), Decision::Deny);
    }
}
