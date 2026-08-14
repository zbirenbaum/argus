// Rust guideline compliant 2026-02-21
//! Pluggable approval interface for syscall interception.
//!
//! When a pause-before-action rule matches, the supervisor needs a
//! decision: allow, deny, or escalate the syscall. The [`Approver`]
//! trait abstracts _who_ makes that decision — an LLM judge, a human
//! via push notification, an email recipient, or the existing REST API.
//!
//! # Escalation chain
//!
//! Approvers are evaluated in order. Each returns [`Verdict::Allow`],
//! [`Verdict::Deny`], or [`Verdict::Escalate`]. The first non-escalate
//! verdict wins. Errors are treated as implicit escalations.
//!
//! ```text
//! ptrace loop → RuleSet::evaluate() → Pause match
//!   → Approvers::judge(request)
//!     → LlmApprover: confident → Allow/Deny (done)
//!     → LlmApprover: unsure   → Escalate
//!       → ApiApprover: blocks until human decides → Allow/Deny
//!   → Verdict::Allow → resume tracee
//!   → Verdict::Deny  → inject EPERM
//! ```
//!
//! # Sync design
//!
//! The trait is intentionally sync because the ptrace loop runs on a
//! dedicated OS thread holding a tracee frozen at syscall entry.
//! Implementations that need async I/O (webhooks, LLM APIs) handle
//! the blocking internally (e.g. `tokio::runtime::Handle::block_on`).

mod parallel;
mod policy;
mod request;
mod verdict;

pub use parallel::{ParallelApprover, ParallelPolicy};
pub use request::ApprovalRequest;
pub use verdict::Verdict;

use std::fmt;
use std::sync::Arc;

/// Decides whether a paused syscall should proceed or be denied.
///
/// The method is sync because the ptrace loop is sync.
/// Implementations that need async I/O (HTTP calls to an LLM, push
/// notifications, email, etc.) block internally.
///
/// # Errors
///
/// Returning an error means the approver could not reach a decision
/// (network failure, timeout, etc.). The escalation chain treats
/// errors as implicit escalations to the next approver.
pub trait Approver: Send + Sync {
    /// Render a judgment on a pending syscall.
    fn judge(&self, request: &ApprovalRequest) -> anyhow::Result<Verdict>;

    /// Human-readable name for logging and event attribution.
    fn name(&self) -> &str;
}

/// Runtime-polymorphic approver for heterogeneous collections.
///
/// Wraps `Arc<dyn Approver>` so the concrete type doesn't leak smart
/// pointers into the public API (M-AVOID-WRAPPERS).
#[derive(Clone)]
pub struct DynApprover(Arc<dyn Approver>);

impl DynApprover {
    /// Wrap any [`Approver`] for dynamic dispatch.
    pub fn new<T: Approver + 'static>(approver: T) -> Self {
        Self(Arc::new(approver))
    }

    /// Render a judgment through the wrapped approver.
    pub fn judge(&self, request: &ApprovalRequest) -> anyhow::Result<Verdict> {
        self.0.judge(request)
    }

    /// Name of the underlying approver.
    pub fn name(&self) -> &str {
        self.0.name()
    }
}

impl fmt::Debug for DynApprover {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("DynApprover").field(&self.name()).finish()
    }
}

/// Ordered escalation chain of approvers.
///
/// This is the main entry point the supervisor uses. Configure it
/// with one or more [`DynApprover`]s ordered from automated (LLM,
/// webhook) to human (API endpoint). Call [`Approvers::judge`] on
/// each paused syscall.
///
/// The last approver should be a terminal backstop (e.g. the human
/// API endpoint) that never escalates.
#[derive(Debug, Clone, Default)]
pub struct Approvers {
    chain: Vec<DynApprover>,
}

impl Approvers {
    /// Create an empty escalation chain.
    pub fn new() -> Self {
        Self { chain: Vec::new() }
    }

    /// Append an approver to the end of the escalation chain.
    pub fn push(&mut self, approver: DynApprover) {
        self.chain.push(approver);
    }

    /// Number of approvers in the chain.
    pub fn len(&self) -> usize {
        self.chain.len()
    }

    /// Whether the chain is empty.
    pub fn is_empty(&self) -> bool {
        self.chain.is_empty()
    }

    /// Walk the escalation chain and return the first terminal verdict.
    ///
    /// If no approvers are configured, returns `Verdict::Allow` so
    /// the supervisor does not block indefinitely.
    pub fn judge(&self, request: &ApprovalRequest) -> Verdict {
        if self.chain.is_empty() {
            return Verdict::allow("no approvers configured", "system");
        }

        policy::walk_chain(&self.chain, request)
    }

    /// Walk the chain, reporting exhaustion as an escalation.
    ///
    /// [`Approvers::judge`] denies when every approver escalates, which
    /// is the right default for a chain that ends in a terminal
    /// backstop. The supervisor's backstop is the human approval API,
    /// which lives outside this chain, so it needs to tell "the judges
    /// decided to reject" apart from "the judges want a human" — this
    /// method keeps that distinction.
    pub fn judge_or_escalate(&self, request: &ApprovalRequest) -> Verdict {
        if self.chain.is_empty() {
            return Verdict::escalate("no approvers configured", "system");
        }

        match policy::walk_chain(&self.chain, request) {
            Verdict::Deny { approver, .. } if approver == "system:chain-exhausted" => {
                Verdict::escalate("all approvers escalated", "system:chain-exhausted")
            }
            verdict => verdict,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AllowAll;
    impl Approver for AllowAll {
        fn judge(&self, _req: &ApprovalRequest) -> anyhow::Result<Verdict> {
            Ok(Verdict::allow("looks fine", "allow-all"))
        }
        fn name(&self) -> &str {
            "allow-all"
        }
    }

    struct DenyAll;
    impl Approver for DenyAll {
        fn judge(&self, _req: &ApprovalRequest) -> anyhow::Result<Verdict> {
            Ok(Verdict::deny("too dangerous", "deny-all"))
        }
        fn name(&self) -> &str {
            "deny-all"
        }
    }

    struct EscalateAll;
    impl Approver for EscalateAll {
        fn judge(&self, _req: &ApprovalRequest) -> anyhow::Result<Verdict> {
            Ok(Verdict::escalate("confidence 0.5 < 0.8", "llm"))
        }
        fn name(&self) -> &str {
            "escalate-all"
        }
    }

    struct Failing;
    impl Approver for Failing {
        fn judge(&self, _req: &ApprovalRequest) -> anyhow::Result<Verdict> {
            anyhow::bail!("network timeout")
        }
        fn name(&self) -> &str {
            "failing"
        }
    }

    fn test_request() -> ApprovalRequest {
        ApprovalRequest {
            action_id: "test-123".into(),
            pid: 42,
            process: "python".into(),
            syscall: "unlink".into(),
            path: Some("/workspace/important.txt".into()),
            binary: None,
            destination: None,
            rule_description: "unlink /workspace/**".into(),
        }
    }

    #[test]
    fn empty_chain_allows() {
        let approvers = Approvers::default();
        let v = approvers.judge(&test_request());
        assert!(v.is_allow());
    }

    #[test]
    fn single_allow() {
        let mut approvers = Approvers::new();
        approvers.push(DynApprover::new(AllowAll));
        let v = approvers.judge(&test_request());
        assert!(v.is_allow());
        assert_eq!(v.approver(), "allow-all");
    }

    #[test]
    fn single_deny() {
        let mut approvers = Approvers::new();
        approvers.push(DynApprover::new(DenyAll));
        let v = approvers.judge(&test_request());
        assert!(v.is_deny());
        assert_eq!(v.reason(), Some("too dangerous"));
    }

    #[test]
    fn escalate_to_human() {
        let mut approvers = Approvers::new();
        approvers.push(DynApprover::new(EscalateAll));
        approvers.push(DynApprover::new(AllowAll));
        let v = approvers.judge(&test_request());
        assert!(v.is_allow());
        assert_eq!(v.approver(), "allow-all");
    }

    #[test]
    fn escalate_to_deny() {
        let mut approvers = Approvers::new();
        approvers.push(DynApprover::new(EscalateAll));
        approvers.push(DynApprover::new(DenyAll));
        let v = approvers.judge(&test_request());
        assert!(v.is_deny());
        assert_eq!(v.approver(), "deny-all");
    }

    #[test]
    fn all_escalate_falls_through() {
        let mut approvers = Approvers::new();
        approvers.push(DynApprover::new(EscalateAll));
        approvers.push(DynApprover::new(EscalateAll));
        let v = approvers.judge(&test_request());
        assert!(v.is_deny());
        assert_eq!(v.approver(), "system:chain-exhausted");
    }

    #[test]
    fn first_terminal_wins() {
        let mut approvers = Approvers::new();
        approvers.push(DynApprover::new(AllowAll));
        approvers.push(DynApprover::new(DenyAll));
        let v = approvers.judge(&test_request());
        assert!(v.is_allow());
    }

    #[test]
    fn error_escalates_to_next() {
        let mut approvers = Approvers::new();
        approvers.push(DynApprover::new(Failing));
        approvers.push(DynApprover::new(AllowAll));
        let v = approvers.judge(&test_request());
        assert!(v.is_allow());
    }

    #[test]
    fn all_errors_denies() {
        let mut approvers = Approvers::new();
        approvers.push(DynApprover::new(Failing));
        let v = approvers.judge(&test_request());
        assert!(v.is_deny());
    }

    #[test]
    fn dyn_approver_debug_shows_name() {
        let a = DynApprover::new(AllowAll);
        let debug = format!("{a:?}");
        assert!(debug.contains("allow-all"));
    }

    #[test]
    fn chain_len_tracks_pushes() {
        let mut a = Approvers::default();
        assert!(a.is_empty());
        a.push(DynApprover::new(AllowAll));
        assert_eq!(a.len(), 1);
    }

    #[test]
    fn mixed_escalate_error_then_terminal() {
        let mut approvers = Approvers::new();
        approvers.push(DynApprover::new(EscalateAll));
        approvers.push(DynApprover::new(Failing));
        approvers.push(DynApprover::new(DenyAll));
        let v = approvers.judge(&test_request());
        assert!(v.is_deny());
        assert_eq!(v.approver(), "deny-all");
    }
}
