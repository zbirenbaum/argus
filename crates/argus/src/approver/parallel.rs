// Rust guideline compliant 2026-02-21
//! Composite approver that fans out to multiple approvers in parallel.
//!
//! Sits in a single slot of the escalation chain. Spawns OS threads
//! (the ptrace loop is sync), collects verdicts, and combines them
//! per a [`ParallelPolicy`].

use std::thread;

use serde::{Deserialize, Serialize};

use super::request::ApprovalRequest;
use super::verdict::Verdict;
use super::{Approver, DynApprover};

/// How to combine verdicts from parallel approvers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParallelPolicy {
    /// First terminal verdict wins. Remaining threads abandoned.
    FirstResponse,

    /// Collect `min_approvals` terminal verdicts. If all agree, that
    /// verdict wins. If they disagree, escalate. Most secure — requires
    /// agreement, not just a majority.
    #[default]
    Consensus,

    /// All must allow. One deny → deny. One error/escalate → escalate.
    Unanimous,

    /// Majority of terminal verdicts wins. Ties → escalate.
    /// With one model, majority-of-one works naturally.
    Majority,
}

/// Approver that fans out to multiple inner approvers concurrently.
///
/// Implements [`Approver`] so it can sit in any slot of the
/// escalation chain. Uses OS threads since the trait is sync.
pub struct ParallelApprover {
    name: String,
    approvers: Vec<DynApprover>,
    policy: ParallelPolicy,
    /// Minimum terminal verdicts required before deciding.
    ///
    /// Only used by [`ParallelPolicy::Consensus`]. If fewer than
    /// `min_approvals` terminal verdicts are collected, the group
    /// escalates. Defaults to the total number of inner approvers.
    min_approvals: Option<usize>,
}

impl ParallelApprover {
    /// Create a parallel approver with the given policy.
    pub fn new(
        name: impl Into<String>,
        policy: ParallelPolicy,
    ) -> Self {
        Self {
            name: name.into(),
            approvers: Vec::new(),
            policy,
            min_approvals: None,
        }
    }

    /// Set the minimum terminal verdicts required for consensus.
    ///
    /// Clamped to `[1, approvers.len()]` at evaluation time.
    pub fn with_min_approvals(mut self, n: usize) -> Self {
        self.min_approvals = Some(n);
        self
    }

    /// Add an inner approver to the parallel group.
    pub fn push(&mut self, approver: DynApprover) {
        self.approvers.push(approver);
    }

    /// Effective min_approvals: explicit value or total count.
    fn effective_min(&self) -> usize {
        let total = self.approvers.len();
        self.min_approvals
            .map(|n| n.clamp(1, total))
            .unwrap_or(total)
    }
}

impl Approver for ParallelApprover {
    fn judge(&self, request: &ApprovalRequest) -> anyhow::Result<Verdict> {
        if self.approvers.is_empty() {
            return Ok(Verdict::escalate(
                "no inner approvers configured",
                &self.name,
            ));
        }

        let verdicts = fan_out(&self.approvers, request);
        Ok(combine(&verdicts, self.policy, self.effective_min(), &self.name))
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// Collected result from a single parallel approver.
enum Outcome {
    Ok(Verdict),
    Err(String),
}

/// Spawn a thread per approver, collect all results.
fn fan_out(approvers: &[DynApprover], request: &ApprovalRequest) -> Vec<Outcome> {
    thread::scope(|s| {
        let handles: Vec<_> = approvers
            .iter()
            .map(|approver| {
                s.spawn(|| match approver.judge(request) {
                    Ok(v) => Outcome::Ok(v),
                    Err(e) => Outcome::Err(format!("{}: {e}", approver.name())),
                })
            })
            .collect();

        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|_| {
                Outcome::Err("thread panicked".into())
            }))
            .collect()
    })
}

/// Combine outcomes per the policy.
fn combine(
    outcomes: &[Outcome],
    policy: ParallelPolicy,
    min_approvals: usize,
    group_name: &str,
) -> Verdict {
    match policy {
        ParallelPolicy::FirstResponse => combine_first(outcomes, group_name),
        ParallelPolicy::Consensus => combine_consensus(outcomes, min_approvals, group_name),
        ParallelPolicy::Unanimous => combine_unanimous(outcomes, group_name),
        ParallelPolicy::Majority => combine_majority(outcomes, group_name),
    }
}

/// First terminal verdict wins. Errors and escalations skipped.
fn combine_first(outcomes: &[Outcome], group_name: &str) -> Verdict {
    for outcome in outcomes {
        if let Outcome::Ok(v) = outcome
            && v.is_terminal() {
                return v.clone();
            }
    }
    Verdict::escalate("all inner approvers escalated or failed", group_name)
}

/// Collect terminal verdicts. If >= min_approvals agree, that wins.
/// If they disagree or too few responded, escalate.
fn combine_consensus(
    outcomes: &[Outcome],
    min_approvals: usize,
    group_name: &str,
) -> Verdict {
    let mut allows = 0u32;
    let mut denies = 0u32;

    for outcome in outcomes {
        match outcome {
            Outcome::Ok(v) if v.is_allow() => allows += 1,
            Outcome::Ok(v) if v.is_deny() => denies += 1,
            _ => {} // escalations, errors don't count as terminal
        }
    }

    let terminal = allows + denies;
    if terminal < min_approvals as u32 {
        return Verdict::escalate(
            format!(
                "insufficient terminal verdicts ({terminal} < {min_approvals} required)"
            ),
            group_name,
        );
    }

    // All terminal verdicts must agree for consensus.
    if denies == 0 {
        Verdict::allow(
            format!("consensus: {allows} allow, 0 deny"),
            group_name,
        )
    } else if allows == 0 {
        Verdict::deny(
            format!("consensus: 0 allow, {denies} deny"),
            group_name,
        )
    } else {
        Verdict::escalate(
            format!("no consensus ({allows} allow, {denies} deny)"),
            group_name,
        )
    }
}

/// All must allow. One deny → deny. One error/escalate → escalate.
fn combine_unanimous(outcomes: &[Outcome], group_name: &str) -> Verdict {
    let mut saw_allow = false;

    for outcome in outcomes {
        match outcome {
            Outcome::Ok(v) if v.is_deny() => return v.clone(),
            Outcome::Ok(v) if v.is_allow() => saw_allow = true,
            Outcome::Ok(_) => {
                return Verdict::escalate(
                    "inner approver escalated under unanimous policy",
                    group_name,
                );
            }
            Outcome::Err(e) => {
                return Verdict::escalate(
                    format!("inner approver failed: {e}"),
                    group_name,
                );
            }
        }
    }

    if saw_allow {
        Verdict::allow("all inner approvers allowed", group_name)
    } else {
        Verdict::escalate("no verdicts", group_name)
    }
}

/// Majority of terminal verdicts wins. Ties → escalate.
fn combine_majority(outcomes: &[Outcome], group_name: &str) -> Verdict {
    let mut allows = 0u32;
    let mut denies = 0u32;

    for outcome in outcomes {
        match outcome {
            Outcome::Ok(v) if v.is_allow() => allows += 1,
            Outcome::Ok(v) if v.is_deny() => denies += 1,
            _ => {}
        }
    }

    if allows > denies {
        Verdict::allow(
            format!("majority allowed ({allows} allow, {denies} deny)"),
            group_name,
        )
    } else if denies > allows {
        Verdict::deny(
            format!("majority denied ({allows} allow, {denies} deny)"),
            group_name,
        )
    } else {
        Verdict::escalate(
            format!("no majority ({allows} allow, {denies} deny)"),
            group_name,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedApprover {
        verdict: Verdict,
        name: &'static str,
    }

    impl Approver for FixedApprover {
        fn judge(&self, _req: &ApprovalRequest) -> anyhow::Result<Verdict> {
            Ok(self.verdict.clone())
        }
        fn name(&self) -> &str {
            self.name
        }
    }

    struct FailingApprover;
    impl Approver for FailingApprover {
        fn judge(&self, _req: &ApprovalRequest) -> anyhow::Result<Verdict> {
            anyhow::bail!("timeout")
        }
        fn name(&self) -> &str {
            "failing"
        }
    }

    fn allow_approver(name: &'static str) -> DynApprover {
        DynApprover::new(FixedApprover {
            verdict: Verdict::allow("ok", name),
            name,
        })
    }

    fn deny_approver(name: &'static str) -> DynApprover {
        DynApprover::new(FixedApprover {
            verdict: Verdict::deny("bad", name),
            name,
        })
    }

    fn escalate_approver(name: &'static str) -> DynApprover {
        DynApprover::new(FixedApprover {
            verdict: Verdict::escalate("unsure", name),
            name,
        })
    }

    fn req() -> ApprovalRequest {
        ApprovalRequest {
            action_id: "p1".into(),
            pid: 1,
            process: "sh".into(),
            syscall: "unlink".into(),
            path: Some("/workspace/x".into()),
            binary: None,
            destination: None,
            rule_description: "test".into(),
        }
    }

    // --- FirstResponse ---

    #[test]
    fn first_response_takes_first_terminal() {
        let mut pa = ParallelApprover::new("group", ParallelPolicy::FirstResponse);
        pa.push(allow_approver("a"));
        pa.push(deny_approver("b"));
        let v = pa.judge(&req()).unwrap();
        assert!(v.is_terminal());
    }

    #[test]
    fn first_response_skips_escalations() {
        let mut pa = ParallelApprover::new("group", ParallelPolicy::FirstResponse);
        pa.push(escalate_approver("a"));
        pa.push(allow_approver("b"));
        let v = pa.judge(&req()).unwrap();
        assert!(v.is_allow());
    }

    #[test]
    fn first_response_all_escalate() {
        let mut pa = ParallelApprover::new("group", ParallelPolicy::FirstResponse);
        pa.push(escalate_approver("a"));
        let v = pa.judge(&req()).unwrap();
        assert!(v.is_escalate());
    }

    #[test]
    fn first_response_errors_skipped() {
        let mut pa = ParallelApprover::new("group", ParallelPolicy::FirstResponse);
        pa.push(DynApprover::new(FailingApprover));
        pa.push(allow_approver("b"));
        let v = pa.judge(&req()).unwrap();
        assert!(v.is_allow());
    }

    // --- Consensus ---

    #[test]
    fn consensus_all_allow() {
        let mut pa = ParallelApprover::new("group", ParallelPolicy::Consensus);
        pa.push(allow_approver("a"));
        pa.push(allow_approver("b"));
        let v = pa.judge(&req()).unwrap();
        assert!(v.is_allow());
        assert_eq!(v.approver(), "group");
    }

    #[test]
    fn consensus_all_deny() {
        let mut pa = ParallelApprover::new("group", ParallelPolicy::Consensus);
        pa.push(deny_approver("a"));
        pa.push(deny_approver("b"));
        let v = pa.judge(&req()).unwrap();
        assert!(v.is_deny());
    }

    #[test]
    fn consensus_disagreement_escalates() {
        let mut pa = ParallelApprover::new("group", ParallelPolicy::Consensus);
        pa.push(allow_approver("a"));
        pa.push(deny_approver("b"));
        let v = pa.judge(&req()).unwrap();
        assert!(v.is_escalate());
    }

    #[test]
    fn consensus_min_approvals_met() {
        let mut pa = ParallelApprover::new("group", ParallelPolicy::Consensus)
            .with_min_approvals(2);
        pa.push(allow_approver("a"));
        pa.push(allow_approver("b"));
        pa.push(escalate_approver("c"));
        let v = pa.judge(&req()).unwrap();
        assert!(v.is_allow());
    }

    #[test]
    fn consensus_min_approvals_not_met() {
        let mut pa = ParallelApprover::new("group", ParallelPolicy::Consensus)
            .with_min_approvals(2);
        pa.push(allow_approver("a"));
        pa.push(escalate_approver("b"));
        pa.push(escalate_approver("c"));
        let v = pa.judge(&req()).unwrap();
        assert!(v.is_escalate());
        assert!(v.reason().unwrap().contains("insufficient"));
    }

    #[test]
    fn consensus_min_approvals_default_is_total() {
        // 3 approvers, 1 escalates → only 2 terminal → < 3 required → escalate
        let mut pa = ParallelApprover::new("group", ParallelPolicy::Consensus);
        pa.push(allow_approver("a"));
        pa.push(allow_approver("b"));
        pa.push(escalate_approver("c"));
        let v = pa.judge(&req()).unwrap();
        assert!(v.is_escalate());
    }

    #[test]
    fn consensus_errors_dont_count() {
        let mut pa = ParallelApprover::new("group", ParallelPolicy::Consensus)
            .with_min_approvals(2);
        pa.push(allow_approver("a"));
        pa.push(allow_approver("b"));
        pa.push(DynApprover::new(FailingApprover));
        let v = pa.judge(&req()).unwrap();
        assert!(v.is_allow());
    }

    // --- Unanimous ---

    #[test]
    fn unanimous_all_allow() {
        let mut pa = ParallelApprover::new("group", ParallelPolicy::Unanimous);
        pa.push(allow_approver("a"));
        pa.push(allow_approver("b"));
        let v = pa.judge(&req()).unwrap();
        assert!(v.is_allow());
        assert_eq!(v.approver(), "group");
    }

    #[test]
    fn unanimous_one_deny() {
        let mut pa = ParallelApprover::new("group", ParallelPolicy::Unanimous);
        pa.push(allow_approver("a"));
        pa.push(deny_approver("b"));
        let v = pa.judge(&req()).unwrap();
        assert!(v.is_deny());
    }

    #[test]
    fn unanimous_error_escalates() {
        let mut pa = ParallelApprover::new("group", ParallelPolicy::Unanimous);
        pa.push(allow_approver("a"));
        pa.push(DynApprover::new(FailingApprover));
        let v = pa.judge(&req()).unwrap();
        assert!(v.is_escalate());
    }

    // --- Majority ---

    #[test]
    fn majority_allows_win() {
        let mut pa = ParallelApprover::new("group", ParallelPolicy::Majority);
        pa.push(allow_approver("a"));
        pa.push(allow_approver("b"));
        pa.push(deny_approver("c"));
        let v = pa.judge(&req()).unwrap();
        assert!(v.is_allow());
    }

    #[test]
    fn majority_denies_win() {
        let mut pa = ParallelApprover::new("group", ParallelPolicy::Majority);
        pa.push(deny_approver("a"));
        pa.push(deny_approver("b"));
        pa.push(allow_approver("c"));
        let v = pa.judge(&req()).unwrap();
        assert!(v.is_deny());
    }

    #[test]
    fn majority_tie_escalates() {
        let mut pa = ParallelApprover::new("group", ParallelPolicy::Majority);
        pa.push(allow_approver("a"));
        pa.push(deny_approver("b"));
        let v = pa.judge(&req()).unwrap();
        assert!(v.is_escalate());
    }

    #[test]
    fn majority_escalations_dont_count() {
        let mut pa = ParallelApprover::new("group", ParallelPolicy::Majority);
        pa.push(escalate_approver("a"));
        pa.push(escalate_approver("b"));
        pa.push(allow_approver("c"));
        let v = pa.judge(&req()).unwrap();
        assert!(v.is_allow());
    }

    // --- Edge cases ---

    #[test]
    fn empty_parallel_escalates() {
        let pa = ParallelApprover::new("empty", ParallelPolicy::Consensus);
        let v = pa.judge(&req()).unwrap();
        assert!(v.is_escalate());
    }

    #[test]
    fn parallel_name() {
        let pa = ParallelApprover::new("llm-panel", ParallelPolicy::Consensus);
        assert_eq!(pa.name(), "llm-panel");
    }

    #[test]
    fn default_policy_is_consensus() {
        assert_eq!(ParallelPolicy::default(), ParallelPolicy::Consensus);
    }

    #[test]
    fn majority_single_model_works() {
        let mut pa = ParallelApprover::new("solo", ParallelPolicy::Majority);
        pa.push(allow_approver("a"));
        let v = pa.judge(&req()).unwrap();
        assert!(v.is_allow());
    }

    #[test]
    fn min_approvals_clamped_to_bounds() {
        // min_approvals = 100, but only 2 approvers → clamped to 2
        let mut pa = ParallelApprover::new("group", ParallelPolicy::Consensus)
            .with_min_approvals(100);
        pa.push(allow_approver("a"));
        pa.push(allow_approver("b"));
        let v = pa.judge(&req()).unwrap();
        assert!(v.is_allow());
    }

    #[test]
    fn min_approvals_zero_clamped_to_one() {
        let mut pa = ParallelApprover::new("group", ParallelPolicy::Consensus)
            .with_min_approvals(0);
        pa.push(escalate_approver("a"));
        // min clamped to 1, no terminal verdicts → insufficient
        let v = pa.judge(&req()).unwrap();
        assert!(v.is_escalate());
    }
}
