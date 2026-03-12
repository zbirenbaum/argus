// Rust guideline compliant 2026-02-21
//! Escalation chain evaluation for the approver pipeline.
//!
//! Walks the approver list in order. First non-`Escalate` verdict
//! wins. Errors are treated as escalations (logged, then continue).
//! If every approver escalates, falls through to a system deny.

use super::request::ApprovalRequest;
use super::verdict::Verdict;
use super::DynApprover;

/// Walk the escalation chain and return the first terminal verdict.
///
/// The chain is evaluated sequentially — order matters. Typical
/// configuration puts automated judges (LLM) first and the human
/// API endpoint last as the final backstop.
///
/// Errors from an approver are treated as implicit escalations: the
/// failure is logged and the next approver is consulted.
pub(super) fn walk_chain(
    chain: &[DynApprover],
    request: &ApprovalRequest,
) -> Verdict {
    for approver in chain {
        let verdict = match approver.judge(request) {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(
                    approver = approver.name(),
                    error = %err,
                    action_id = %request.action_id,
                    "approver failed, escalating to next"
                );
                continue;
            }
        };

        match &verdict {
            Verdict::Escalate { reason, .. } => {
                tracing::info!(
                    approver = approver.name(),
                    reason = reason.as_deref().unwrap_or("none"),
                    action_id = %request.action_id,
                    "approver escalated, continuing chain"
                );
            }
            _ => return verdict,
        }
    }

    Verdict::deny_no_reason("system:chain-exhausted")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approver::Approver;

    struct TerminalAllow;
    impl Approver for TerminalAllow {
        fn judge(&self, _req: &ApprovalRequest) -> anyhow::Result<Verdict> {
            Ok(Verdict::allow("approved", "terminal-allow"))
        }
        fn name(&self) -> &str { "terminal-allow" }
    }

    struct Escalator;
    impl Approver for Escalator {
        fn judge(&self, _req: &ApprovalRequest) -> anyhow::Result<Verdict> {
            Ok(Verdict::escalate("low confidence", "escalator"))
        }
        fn name(&self) -> &str { "escalator" }
    }

    struct TerminalDeny;
    impl Approver for TerminalDeny {
        fn judge(&self, _req: &ApprovalRequest) -> anyhow::Result<Verdict> {
            Ok(Verdict::deny("blocked", "terminal-deny"))
        }
        fn name(&self) -> &str { "terminal-deny" }
    }

    struct Failing;
    impl Approver for Failing {
        fn judge(&self, _req: &ApprovalRequest) -> anyhow::Result<Verdict> {
            anyhow::bail!("connection refused")
        }
        fn name(&self) -> &str { "failing" }
    }

    fn req() -> ApprovalRequest {
        ApprovalRequest {
            action_id: "t1".into(),
            pid: 1,
            process: "sh".into(),
            syscall: "unlink".into(),
            path: Some("/workspace/x".into()),
            binary: None,
            destination: None,
            rule_description: "test rule".into(),
        }
    }

    #[test]
    fn terminal_verdict_stops_chain() {
        let chain = vec![
            DynApprover::new(TerminalAllow),
            DynApprover::new(TerminalDeny),
        ];
        let v = walk_chain(&chain, &req());
        assert!(v.is_allow());
        assert_eq!(v.approver(), "terminal-allow");
    }

    #[test]
    fn escalation_continues_to_next() {
        let chain = vec![
            DynApprover::new(Escalator),
            DynApprover::new(TerminalAllow),
        ];
        let v = walk_chain(&chain, &req());
        assert!(v.is_allow());
        assert_eq!(v.approver(), "terminal-allow");
    }

    #[test]
    fn all_escalate_falls_through_to_deny() {
        let chain = vec![
            DynApprover::new(Escalator),
            DynApprover::new(Escalator),
        ];
        let v = walk_chain(&chain, &req());
        assert!(v.is_deny());
        assert_eq!(v.approver(), "system:chain-exhausted");
    }

    #[test]
    fn error_treated_as_escalation() {
        let chain = vec![
            DynApprover::new(Failing),
            DynApprover::new(TerminalAllow),
        ];
        let v = walk_chain(&chain, &req());
        assert!(v.is_allow());
    }

    #[test]
    fn all_errors_falls_through_to_deny() {
        let chain = vec![DynApprover::new(Failing)];
        let v = walk_chain(&chain, &req());
        assert!(v.is_deny());
    }

    #[test]
    fn empty_chain_denies() {
        let v = walk_chain(&[], &req());
        assert!(v.is_deny());
    }

    #[test]
    fn deny_stops_chain() {
        let chain = vec![
            DynApprover::new(TerminalDeny),
            DynApprover::new(TerminalAllow),
        ];
        let v = walk_chain(&chain, &req());
        assert!(v.is_deny());
        assert_eq!(v.approver(), "terminal-deny");
    }

    #[test]
    fn mixed_escalate_error_then_terminal() {
        let chain = vec![
            DynApprover::new(Escalator),
            DynApprover::new(Failing),
            DynApprover::new(TerminalDeny),
        ];
        let v = walk_chain(&chain, &req());
        assert!(v.is_deny());
        assert_eq!(v.approver(), "terminal-deny");
    }
}
