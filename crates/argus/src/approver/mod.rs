// Rust guideline compliant 2026-02-21
//! Pluggable approval interface for syscall interception.
//!
//! When a pause-before-action rule matches, the supervisor needs a
//! decision: allow or deny the syscall. The [`Approver`] trait
//! abstracts _who_ makes that decision — an LLM judge, a human via
//! push notification, an email recipient, or the existing REST API.
//!
//! Multiple approvers can be composed via [`Approvers`], which
//! evaluates them according to an [`ApprovalPolicy`].
//!
//! # Architecture
//!
//! ```text
//! ptrace loop → RuleSet::evaluate() → Pause match
//!   → Approvers::judge(request)
//!     → fan-out to [ApiApprover, LlmApprover, WebhookApprover, ...]
//!     → ApprovalPolicy combines verdicts
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

mod policy;
mod request;
mod verdict;

pub use policy::ApprovalPolicy;
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
/// (network failure, timeout, etc.). The [`ApprovalPolicy`] decides
/// how to handle inconclusive approvers.
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

/// Collection of approvers evaluated according to a policy.
///
/// This is the main entry point the supervisor uses. Configure it
/// with one or more [`DynApprover`]s and an [`ApprovalPolicy`], then
/// call [`Approvers::judge`] on each paused syscall.
#[derive(Debug, Clone)]
pub struct Approvers {
    approvers: Vec<DynApprover>,
    policy: ApprovalPolicy,
}

impl Approvers {
    /// Create a new approver collection with the given policy.
    pub fn new(policy: ApprovalPolicy) -> Self {
        Self {
            approvers: Vec::new(),
            policy,
        }
    }

    /// Add an approver to the collection.
    pub fn add(&mut self, approver: DynApprover) {
        self.approvers.push(approver);
    }

    /// Number of configured approvers.
    pub fn len(&self) -> usize {
        self.approvers.len()
    }

    /// Whether no approvers are configured.
    pub fn is_empty(&self) -> bool {
        self.approvers.is_empty()
    }

    /// Evaluate all approvers and combine verdicts per the policy.
    ///
    /// If no approvers are configured, returns `Verdict::allow` so
    /// the supervisor does not block indefinitely.
    pub fn judge(&self, request: &ApprovalRequest) -> Verdict {
        if self.approvers.is_empty() {
            return Verdict::allow("no approvers configured", "system");
        }

        policy::evaluate(&self.approvers, &self.policy, request)
    }
}

impl Default for Approvers {
    /// Default: no approvers, first-response policy.
    fn default() -> Self {
        Self::new(ApprovalPolicy::FirstResponse)
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
    fn empty_approvers_allows() {
        let approvers = Approvers::default();
        let v = approvers.judge(&test_request());
        assert!(v.is_allow());
    }

    #[test]
    fn single_allow_approver() {
        let mut approvers = Approvers::new(ApprovalPolicy::FirstResponse);
        approvers.add(DynApprover::new(AllowAll));
        let v = approvers.judge(&test_request());
        assert!(v.is_allow());
        assert_eq!(v.approver(), "allow-all");
    }

    #[test]
    fn single_deny_approver() {
        let mut approvers = Approvers::new(ApprovalPolicy::FirstResponse);
        approvers.add(DynApprover::new(DenyAll));
        let v = approvers.judge(&test_request());
        assert!(v.is_deny());
        assert_eq!(v.reason(), Some("too dangerous"));
    }

    #[test]
    fn first_response_takes_first_ok() {
        let mut approvers = Approvers::new(ApprovalPolicy::FirstResponse);
        approvers.add(DynApprover::new(AllowAll));
        approvers.add(DynApprover::new(DenyAll));
        let v = approvers.judge(&test_request());
        assert!(v.is_allow());
    }

    #[test]
    fn unanimous_requires_all_allow() {
        let mut approvers = Approvers::new(ApprovalPolicy::Unanimous);
        approvers.add(DynApprover::new(AllowAll));
        approvers.add(DynApprover::new(DenyAll));
        let v = approvers.judge(&test_request());
        assert!(v.is_deny());
    }

    #[test]
    fn unanimous_all_allow() {
        let mut approvers = Approvers::new(ApprovalPolicy::Unanimous);
        approvers.add(DynApprover::new(AllowAll));
        let v = approvers.judge(&test_request());
        assert!(v.is_allow());
    }

    #[test]
    fn any_allow_passes_with_one() {
        let mut approvers = Approvers::new(ApprovalPolicy::AnyAllow);
        approvers.add(DynApprover::new(DenyAll));
        approvers.add(DynApprover::new(AllowAll));
        let v = approvers.judge(&test_request());
        assert!(v.is_allow());
    }

    #[test]
    fn any_allow_denies_when_none_allow() {
        let mut approvers = Approvers::new(ApprovalPolicy::AnyAllow);
        approvers.add(DynApprover::new(DenyAll));
        let v = approvers.judge(&test_request());
        assert!(v.is_deny());
    }

    #[test]
    fn failing_approver_skipped_first_response() {
        let mut approvers = Approvers::new(ApprovalPolicy::FirstResponse);
        approvers.add(DynApprover::new(Failing));
        approvers.add(DynApprover::new(AllowAll));
        let v = approvers.judge(&test_request());
        assert!(v.is_allow());
    }

    #[test]
    fn all_failing_defaults_to_deny() {
        let mut approvers = Approvers::new(ApprovalPolicy::FirstResponse);
        approvers.add(DynApprover::new(Failing));
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
    fn approvers_len_tracks_additions() {
        let mut a = Approvers::default();
        assert!(a.is_empty());
        a.add(DynApprover::new(AllowAll));
        assert_eq!(a.len(), 1);
    }
}
