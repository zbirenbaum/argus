// Rust guideline compliant 2026-02-21
//! Policies for combining multiple approver verdicts.

use serde::{Deserialize, Serialize};

use super::request::ApprovalRequest;
use super::verdict::Verdict;
use super::DynApprover;

/// How to combine verdicts from multiple approvers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPolicy {
    /// First approver to return a successful verdict wins.
    ///
    /// Approvers that return errors are skipped. If all fail,
    /// the syscall is denied.
    #[default]
    FirstResponse,

    /// All approvers must allow. A single deny or error → deny.
    Unanimous,

    /// Any single allow is sufficient. All must deny to deny.
    AnyAllow,
}

/// Fan-out to all approvers and combine per the policy.
pub(super) fn evaluate(
    approvers: &[DynApprover],
    policy: &ApprovalPolicy,
    request: &ApprovalRequest,
) -> Verdict {
    match policy {
        ApprovalPolicy::FirstResponse => evaluate_first_response(approvers, request),
        ApprovalPolicy::Unanimous => evaluate_unanimous(approvers, request),
        ApprovalPolicy::AnyAllow => evaluate_any_allow(approvers, request),
    }
}

/// Return the first successful verdict. Skip errors.
fn evaluate_first_response(
    approvers: &[DynApprover],
    request: &ApprovalRequest,
) -> Verdict {
    for approver in approvers {
        match approver.judge(request) {
            Ok(verdict) => return verdict,
            Err(err) => {
                tracing::warn!(
                    approver = approver.name(),
                    error = %err,
                    action_id = %request.action_id,
                    "approver failed, trying next"
                );
            }
        }
    }
    Verdict::deny_no_reason("system:all-approvers-failed")
}

/// All must allow. First deny or error short-circuits.
fn evaluate_unanimous(
    approvers: &[DynApprover],
    request: &ApprovalRequest,
) -> Verdict {
    let mut last_allow = None;

    for approver in approvers {
        match approver.judge(request) {
            Ok(verdict) if verdict.is_allow() => {
                last_allow = Some(verdict);
            }
            Ok(verdict) => return verdict,
            Err(err) => {
                tracing::warn!(
                    approver = approver.name(),
                    error = %err,
                    action_id = %request.action_id,
                    "approver failed, treating as deny for unanimous policy"
                );
                return Verdict::deny(
                    format!("approver '{}' failed: {err}", approver.name()),
                    "system:approver-error",
                );
            }
        }
    }

    last_allow.unwrap_or_else(|| Verdict::deny_no_reason("system:no-approvers"))
}

/// Any single allow is sufficient. All must deny (or error) to deny.
fn evaluate_any_allow(
    approvers: &[DynApprover],
    request: &ApprovalRequest,
) -> Verdict {
    let mut last_deny = None;

    for approver in approvers {
        match approver.judge(request) {
            Ok(verdict) if verdict.is_allow() => return verdict,
            Ok(verdict) => {
                last_deny = Some(verdict);
            }
            Err(err) => {
                tracing::warn!(
                    approver = approver.name(),
                    error = %err,
                    action_id = %request.action_id,
                    "approver failed, continuing for any-allow policy"
                );
            }
        }
    }

    last_deny.unwrap_or_else(|| Verdict::deny_no_reason("system:no-approvers"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_serde_round_trip() {
        for policy in [
            ApprovalPolicy::FirstResponse,
            ApprovalPolicy::Unanimous,
            ApprovalPolicy::AnyAllow,
        ] {
            let json = serde_json::to_string(&policy).unwrap();
            let back: ApprovalPolicy = serde_json::from_str(&json).unwrap();
            assert_eq!(policy, back);
        }
    }

    #[test]
    fn default_policy_is_first_response() {
        assert_eq!(ApprovalPolicy::default(), ApprovalPolicy::FirstResponse);
    }
}
