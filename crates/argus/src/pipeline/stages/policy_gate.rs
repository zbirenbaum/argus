// Rust guideline compliant 2026-02-21
//! Unified policy enforcement stage.
//!
//! Replaces the separate `CheckRulesStage` + `ApprovalStage` with a single
//! gate that evaluates the hot-swappable ruleset, handles block/ask-user
//! verdicts, and returns a stream-compatible [`PolicyOutcome`].
//!
//! Pause-before-action matches go through the judge chain
//! ([`crate::approver`]): the agent is frozen, the chain renders a
//! verdict, and `Allow`/`Deny` decide the syscall outright. An
//! escalation — including the default of having no judges configured —
//! falls through to the human approval API, which is the backstop.

use std::sync::Arc;

use arc_swap::ArcSwap;
use tracing::{Level, event};

use crate::api::routes::submit_pending_approval;
use crate::api::state::SharedState;
use crate::approver::{ApprovalRequest, Approvers, Verdict};
use crate::config::{MatchKind, RuleDecision, RuleSet};
use crate::events::{ApprovalDecision, EventPayload};
use crate::events::control;
use crate::pipeline::classified::{ClassifiedEvent, Classification};
use crate::pipeline::ptrace_thread::PtraceHandle;

/// Result of evaluating a classified event against the policy ruleset.
#[derive(Debug)]
pub enum PolicyOutcome {
    /// No rule matched or the human approved — continue processing.
    Approved(ClassifiedEvent),
    /// Block rule matched — EPERM injected, event filtered from stream.
    Blocked {
        pid: u32,
        syscall: String,
        path: Option<String>,
        reason: String,
    },
}

/// Unified policy enforcement gate for the pipeline.
///
/// Evaluates each classified event against the dynamic ruleset and
/// handles block and ask-user verdicts. The ruleset is hot-swappable
/// via `ArcSwap` so rules can be updated at runtime through the API.
pub struct PolicyGate {
    handle: PtraceHandle,
    rules: Arc<ArcSwap<RuleSet>>,
    shared: SharedState,
    approvers: Approvers,
}

impl PolicyGate {
    /// Create a new policy gate with no automated judges.
    ///
    /// Every pause-before-action match escalates straight to the human
    /// approval API.
    pub fn new(
        handle: PtraceHandle,
        rules: Arc<ArcSwap<RuleSet>>,
        shared: SharedState,
    ) -> Self {
        Self::with_approvers(handle, rules, shared, Approvers::new())
    }

    /// Create a policy gate backed by an escalation chain of judges.
    ///
    /// The chain is consulted on every pause-before-action match. A
    /// terminal verdict decides the syscall outright; an escalation (or
    /// an empty chain) falls through to the human approval API, which is
    /// the terminal backstop.
    pub fn with_approvers(
        handle: PtraceHandle,
        rules: Arc<ArcSwap<RuleSet>>,
        shared: SharedState,
        approvers: Approvers,
    ) -> Self {
        Self { handle, rules, shared, approvers }
    }

    /// Evaluate a classified event against the policy ruleset.
    ///
    /// Returns `Approved` when the event should continue through the
    /// pipeline, or `Blocked` when EPERM was injected and the event
    /// should be recorded as blocked. A pause-before-action match
    /// freezes the whole agent, consults the judge chain, and — if no
    /// judge reaches a terminal verdict — waits for an operator. The
    /// tracee is not resumed for the entire duration.
    pub async fn evaluate(&self, event: ClassifiedEvent) -> PolicyOutcome {
        let rules = self.rules.load();
        let (kind, path, binary, dest) = extract_context(&event.classification);

        let Some(kind) = kind else {
            return PolicyOutcome::Approved(event);
        };

        match rules.evaluate(kind, path.as_deref(), binary.as_deref(), dest.as_deref()) {
            RuleDecision::Allow => PolicyOutcome::Approved(event),

            RuleDecision::Block { rule_index } => {
                let reason = rules.block_rule_description(rule_index);
                self.handle.inject_error(event.pid, libc::EPERM);

                let pid_raw = event.pid.as_raw() as u32;
                let syscall = event.syscall_name();
                let evt_path = event.primary_path();

                event!(
                    name: "pipeline.policy.blocked",
                    Level::INFO,
                    pid = pid_raw,
                    rule = reason.as_str(),
                    "policy gate blocked syscall",
                );

                PolicyOutcome::Blocked {
                    pid: pid_raw,
                    syscall,
                    path: evt_path,
                    reason,
                }
            }

            RuleDecision::Pause { rule_index } => {
                let reason = rules.pause_rule_description(rule_index);
                self.hold_for_decision(event, reason).await
            }
        }
    }

    /// Hold the agent while a pause-before-action verdict is decided.
    ///
    /// The tracee that made the syscall is already stopped at its
    /// syscall entry, but its siblings are not: without an explicit
    /// freeze they keep running — and could complete the very action
    /// being judged — while the decision is outstanding. Freezing first
    /// makes "the agent is stopped" true for every traced process.
    async fn hold_for_decision(
        &self,
        event: ClassifiedEvent,
        reason: String,
    ) -> PolicyOutcome {
        let pid_raw = event.pid.as_raw() as u32;
        let syscall = event.syscall_name();
        let evt_path = event.primary_path();

        let stopped = self.handle.freeze().await;
        event!(
            name: "pipeline.policy.frozen",
            Level::INFO,
            pid = pid_raw,
            rule = reason.as_str(),
            stopped.count = stopped.len(),
            "agent frozen pending a verdict",
        );

        // One identifier for the whole decision: the judges see it, and
        // if they escalate the operator sees the same one.
        let action_id = uuid::Uuid::new_v4().to_string();
        let request = ApprovalRequest {
            action_id: action_id.clone(),
            pid: pid_raw,
            process: format!("pid:{pid_raw}"),
            syscall: syscall.clone(),
            path: evt_path.clone(),
            binary: None,
            destination: None,
            rule_description: reason.clone(),
        };

        let decision = match self.judge(request).await {
            Verdict::Allow { approver, .. } => {
                self.shared.emit(EventPayload::ApprovalGranted(control::ApprovalGranted {
                    pid: pid_raw,
                    rule_name: reason.clone(),
                    approver,
                }));
                ApprovalDecision::Approve
            }
            Verdict::Deny { approver, .. } => {
                self.shared.emit(EventPayload::ApprovalDenied(control::ApprovalDenied {
                    pid: pid_raw,
                    rule_name: reason.clone(),
                    approver,
                }));
                ApprovalDecision::Deny
            }
            // No judge reached a terminal verdict: the human API is the
            // backstop, and the agent stays frozen until it answers.
            Verdict::Escalate { .. } => {
                self.await_human(action_id, pid_raw, &syscall, &evt_path, &reason).await
            }
        };

        if decision == ApprovalDecision::Deny {
            self.handle.inject_error(event.pid, libc::EPERM);
            return PolicyOutcome::Blocked {
                pid: pid_raw,
                syscall,
                path: evt_path,
                reason,
            };
        }

        PolicyOutcome::Approved(event)
    }

    /// Walk the judge chain off the async runtime.
    ///
    /// [`Approvers::judge`] is sync by design — implementations block on
    /// HTTP calls and the like — so it runs on a blocking thread. The
    /// tracee stays stopped throughout either way: nothing resumes it
    /// until this function returns.
    async fn judge(&self, request: ApprovalRequest) -> Verdict {
        if self.approvers.is_empty() {
            return Verdict::escalate("no judges configured", "system");
        }

        let approvers = self.approvers.clone();
        match tokio::task::spawn_blocking(move || approvers.judge_or_escalate(&request)).await {
            Ok(verdict) => verdict,
            Err(e) => {
                event!(
                    name: "pipeline.policy.judge_panicked",
                    Level::ERROR,
                    error.message = %e,
                    "judge chain panicked, escalating to the human backstop",
                );
                Verdict::escalate("judge chain panicked", "system")
            }
        }
    }

    /// Publish a pending approval and block until an operator decides.
    async fn await_human(
        &self,
        action_id: String,
        pid: u32,
        syscall: &str,
        path: &Option<String>,
        reason: &str,
    ) -> ApprovalDecision {
        event!(
            name: "pipeline.policy.ask_user",
            Level::INFO,
            pid = pid,
            rule = reason,
            "policy gate waiting for human decision",
        );

        self.shared.emit(EventPayload::PendingApproval(
            control::PendingApproval {
                pid,
                syscall: syscall.to_owned(),
                path: path.clone(),
                binary: None,
                rule_name: reason.to_owned(),
            },
        ));

        let (_action_id, rx) = submit_pending_approval(
            &self.shared,
            action_id,
            pid,
            format!("pid:{pid}"),
            syscall.to_owned(),
            path.clone(),
            reason.to_owned(),
        );

        rx.await.unwrap_or(ApprovalDecision::Deny)
    }
}

/// Extract rule-matching context from a classification.
fn extract_context(
    c: &Classification,
) -> (Option<MatchKind>, Option<String>, Option<String>, Option<String>) {
    match c {
        Classification::FileWrite { path, .. } | Classification::FileTruncate { path, .. } => {
            (Some(MatchKind::Write), Some(path.to_string_lossy().into()), None, None)
        }
        Classification::FileRead { path, .. } => {
            (Some(MatchKind::Read), Some(path.to_string_lossy().into()), None, None)
        }
        Classification::FileUnlink { path } => {
            (Some(MatchKind::Unlink), Some(path.to_string_lossy().into()), None, None)
        }
        Classification::FileRename { old_path, .. } => {
            (Some(MatchKind::Rename), Some(old_path.to_string_lossy().into()), None, None)
        }
        Classification::FileChmod { path, .. } => {
            (Some(MatchKind::Chmod), Some(path.to_string_lossy().into()), None, None)
        }
        Classification::ProcessExec { binary, .. } => {
            let name = binary.file_name().map(|n| n.to_string_lossy().into());
            (Some(MatchKind::Exec), None, name, None)
        }
        Classification::NetConnect { addr, .. } => {
            let dest = Some(addr.to_string());
            (Some(MatchKind::Connect), None, None, dest)
        }
        _ => (None, None, None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    use nix::unistd::Pid;
    use tokio::sync::mpsc;

    use crate::cas::MemoryCas;
    use crate::config::{Rule, RuleSet};
    use crate::pipeline::bus::RecordBus;
    use crate::pipeline::directive::PipelineDirective;
    use crate::pipeline::ptrace_thread::PtraceHandle;
    use crate::pipeline::raw_stop::{RawSyscallStop, StopType, SyscallArgs};

    fn test_shared() -> SharedState {
        let cas: Arc<dyn crate::cas::Cas> = Arc::new(MemoryCas::new());
        crate::api::state::new_shared_state("test".into(), cas, RecordBus::new(vec![]))
    }

    fn make_event(cls: Classification) -> ClassifiedEvent {
        ClassifiedEvent {
            pid: Pid::from_raw(42),
            raw: RawSyscallStop {
                pid: Pid::from_raw(42),
                stop_type: StopType::SyscallEntry {
                    syscall_nr: 0,
                    args: SyscallArgs::from_array([0; 6]),
                },
            },
            classification: cls,
        }
    }

    fn make_gate(rules: RuleSet) -> (PolicyGate, mpsc::UnboundedReceiver<PipelineDirective>) {
        make_gate_with(rules, Approvers::new())
    }

    /// Build a gate whose freeze requests are answered automatically.
    ///
    /// The returned receiver carries every directive except `Freeze`, so
    /// assertions read the same as before the freeze step existed.
    fn make_gate_with(
        rules: RuleSet,
        approvers: Approvers,
    ) -> (PolicyGate, mpsc::UnboundedReceiver<PipelineDirective>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let handle = PtraceHandle::from_sender(tx);
        let rules_swap = Arc::new(ArcSwap::new(Arc::new(rules)));
        let shared = test_shared();
        let gate = PolicyGate::with_approvers(handle, rules_swap, shared, approvers);
        (gate, crate::pipeline::freeze::spawn_freeze_responder(rx))
    }

    /// Ruleset with a single pause-before-action rule on unlink.
    fn pause_on_unlink() -> RuleSet {
        let mut rs = RuleSet::default();
        rs.pause_before.push(Rule::new(
            MatchKind::Unlink,
            vec!["/workspace/**".into()],
            vec![],
            vec![],
        ));
        rs.compile_patterns();
        rs
    }

    /// Judge with a fixed verdict, standing in for an LLM or webhook.
    struct FixedJudge {
        verdict: Verdict,
        name: &'static str,
    }

    impl crate::approver::Approver for FixedJudge {
        fn judge(&self, _req: &ApprovalRequest) -> anyhow::Result<Verdict> {
            Ok(self.verdict.clone())
        }
        fn name(&self) -> &str {
            self.name
        }
    }

    fn chain_of(verdict: Verdict, name: &'static str) -> Approvers {
        let mut approvers = Approvers::new();
        approvers.push(crate::approver::DynApprover::new(FixedJudge { verdict, name }));
        approvers
    }

    fn unlink_event() -> ClassifiedEvent {
        make_event(Classification::FileUnlink {
            path: PathBuf::from("/workspace/critical.txt"),
        })
    }

    #[tokio::test]
    async fn no_rules_approves() {
        let (gate, _rx) = make_gate(RuleSet::default());
        let event = make_event(Classification::FileWrite {
            path: PathBuf::from("/workspace/test.txt"),
            fd: 3,
            buf_addr: 0,
            len: 10,
        });
        let outcome = gate.evaluate(event).await;
        assert!(matches!(outcome, PolicyOutcome::Approved(_)));
    }

    #[tokio::test]
    async fn block_rule_injects_eperm() {
        let mut rs = RuleSet::default();
        rs.block.push(Rule::new(
            MatchKind::Unlink,
            vec!["/workspace/**".into()],
            vec![],
            vec![],
        ));
        rs.compile_patterns();

        let (gate, mut rx) = make_gate(rs);
        let event = make_event(Classification::FileUnlink {
            path: PathBuf::from("/workspace/secret.txt"),
        });
        let outcome = gate.evaluate(event).await;

        match outcome {
            PolicyOutcome::Blocked { pid, reason, .. } => {
                assert_eq!(pid, 42);
                assert!(!reason.is_empty());
            }
            PolicyOutcome::Approved(_) => panic!("expected Blocked"),
        }

        // Verify InjectError directive was sent.
        let directive = rx.recv().await.unwrap();
        assert!(matches!(directive, PipelineDirective::InjectError { errno, .. } if errno == libc::EPERM));
    }

    #[tokio::test]
    async fn passthrough_always_approved() {
        let mut rs = RuleSet::default();
        rs.block.push(Rule::new(
            MatchKind::Unlink,
            vec!["/workspace/**".into()],
            vec![],
            vec![],
        ));
        rs.compile_patterns();

        let (gate, _rx) = make_gate(rs);
        let event = make_event(Classification::Passthrough);
        let outcome = gate.evaluate(event).await;
        assert!(matches!(outcome, PolicyOutcome::Approved(_)));
    }

    #[tokio::test]
    async fn pause_rule_denied_blocks() {
        let mut rs = RuleSet::default();
        rs.pause_before.push(Rule::new(
            MatchKind::Write,
            vec!["/workspace/**".into()],
            vec![],
            vec![],
        ));
        rs.compile_patterns();

        let (gate, mut rx) = make_gate(rs);
        let event = make_event(Classification::FileWrite {
            path: PathBuf::from("/workspace/danger.txt"),
            fd: 3,
            buf_addr: 0,
            len: 5,
        });

        // Resolve the approval as Deny from another task.
        let shared = Arc::clone(&gate.shared);
        tokio::spawn(async move {
            // Wait for the pending approval to appear.
            loop {
                if shared.pending_count() > 0 {
                    let actions = shared.pending_actions();
                    let action_id = &actions[0].action_id;
                    crate::api::state::resolve_approval(&shared, action_id, ApprovalDecision::Deny);
                    break;
                }
                tokio::task::yield_now().await;
            }
        });

        let outcome = gate.evaluate(event).await;
        assert!(matches!(outcome, PolicyOutcome::Blocked { .. }));

        let directive = rx.recv().await.unwrap();
        assert!(matches!(directive, PipelineDirective::InjectError { .. }));
    }

    #[tokio::test]
    async fn pause_rule_approved_passes() {
        let mut rs = RuleSet::default();
        rs.pause_before.push(Rule::new(
            MatchKind::Write,
            vec!["/workspace/**".into()],
            vec![],
            vec![],
        ));
        rs.compile_patterns();

        let (gate, _rx) = make_gate(rs);
        let event = make_event(Classification::FileWrite {
            path: PathBuf::from("/workspace/ok.txt"),
            fd: 3,
            buf_addr: 0,
            len: 5,
        });

        let shared = Arc::clone(&gate.shared);
        tokio::spawn(async move {
            loop {
                if shared.pending_count() > 0 {
                    let actions = shared.pending_actions();
                    let action_id = &actions[0].action_id;
                    crate::api::state::resolve_approval(&shared, action_id, ApprovalDecision::Approve);
                    break;
                }
                tokio::task::yield_now().await;
            }
        });

        let outcome = gate.evaluate(event).await;
        assert!(matches!(outcome, PolicyOutcome::Approved(_)));
    }

    // ── Judge verdicts drive the freeze ──────────────────────────────

    #[tokio::test]
    async fn judge_deny_blocks_and_injects_eperm() {
        let (gate, mut rx) = make_gate_with(
            pause_on_unlink(),
            chain_of(Verdict::deny("destructive", "llm"), "llm"),
        );

        let outcome = gate.evaluate(unlink_event()).await;
        assert!(
            matches!(outcome, PolicyOutcome::Blocked { .. }),
            "a rejected action must not reach the kernel",
        );

        let directive = rx.recv().await.expect("denied syscall must be answered");
        assert!(
            matches!(directive, PipelineDirective::InjectError { errno, .. } if errno == libc::EPERM),
            "spec requires EPERM for a denied syscall, got {directive:?}",
        );
        let extra = tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await;
        assert!(extra.is_err(), "no further directives after the denial");
    }

    #[tokio::test]
    async fn judge_deny_never_resumes_before_the_verdict() {
        // A judge that blocks for a while stands in for an LLM call: the
        // tracee must stay stopped for its whole duration, which means
        // no directive may be sent until the verdict lands.
        struct SlowDeny;
        impl crate::approver::Approver for SlowDeny {
            fn judge(&self, _req: &ApprovalRequest) -> anyhow::Result<Verdict> {
                std::thread::sleep(std::time::Duration::from_millis(300));
                Ok(Verdict::deny("too risky", "slow-judge"))
            }
            fn name(&self) -> &str {
                "slow-judge"
            }
        }

        let mut approvers = Approvers::new();
        approvers.push(crate::approver::DynApprover::new(SlowDeny));
        let (gate, mut rx) = make_gate_with(pause_on_unlink(), approvers);

        let evaluate = tokio::spawn(async move { gate.evaluate(unlink_event()).await });

        // Mid-judgement: nothing has been sent that would let the tracee run.
        let early = tokio::time::timeout(std::time::Duration::from_millis(150), rx.recv()).await;
        assert!(early.is_err(), "tracee resumed before the judge decided");

        assert!(matches!(evaluate.await.unwrap(), PolicyOutcome::Blocked { .. }));
        assert!(matches!(
            rx.recv().await,
            Some(PipelineDirective::InjectError { errno, .. }) if errno == libc::EPERM
        ));
    }

    #[tokio::test]
    async fn judge_allow_passes_through_without_asking_a_human() {
        let (gate, mut rx) = make_gate_with(
            pause_on_unlink(),
            chain_of(Verdict::allow("scratch file", "llm"), "llm"),
        );

        let outcome = gate.evaluate(unlink_event()).await;
        assert!(matches!(outcome, PolicyOutcome::Approved(_)));
        assert_eq!(gate.shared.pending_count(), 0, "allow must not queue an approval");

        let idle = tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await;
        assert!(idle.is_err(), "an approved syscall is resumed downstream, not here");
    }

    #[tokio::test]
    async fn judge_escalation_holds_the_tracee_until_a_human_answers() {
        let (gate, mut rx) = make_gate_with(
            pause_on_unlink(),
            chain_of(Verdict::escalate("low confidence", "llm"), "llm"),
        );
        let shared = Arc::clone(&gate.shared);

        let evaluate = tokio::spawn(async move { gate.evaluate(unlink_event()).await });

        // The action is queued for a human and the tracee stays stopped.
        let mut waited = 0;
        while shared.pending_count() == 0 && waited < 200 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            waited += 1;
        }
        assert_eq!(shared.pending_count(), 1, "escalation must reach the human backstop");

        let idle = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await;
        assert!(idle.is_err(), "tracee resumed while an approval was still pending");

        let action_id = shared.pending_actions()[0].action_id.clone();
        crate::api::state::resolve_approval(&shared, &action_id, ApprovalDecision::Deny);

        assert!(matches!(evaluate.await.unwrap(), PolicyOutcome::Blocked { .. }));
        assert!(matches!(
            rx.recv().await,
            Some(PipelineDirective::InjectError { errno, .. }) if errno == libc::EPERM
        ));
    }

    #[tokio::test]
    async fn judge_chain_escalates_to_the_second_judge() {
        let mut approvers = Approvers::new();
        approvers.push(crate::approver::DynApprover::new(FixedJudge {
            verdict: Verdict::escalate("unsure", "llm"),
            name: "llm",
        }));
        approvers.push(crate::approver::DynApprover::new(FixedJudge {
            verdict: Verdict::deny("policy says no", "webhook"),
            name: "webhook",
        }));

        let (gate, _rx) = make_gate_with(pause_on_unlink(), approvers);
        assert!(matches!(
            gate.evaluate(unlink_event()).await,
            PolicyOutcome::Blocked { .. }
        ));
        assert_eq!(gate.shared.pending_count(), 0, "a terminal verdict skips the human");
    }

    #[tokio::test]
    async fn hot_swap_rules() {
        let (gate, _rx) = make_gate(RuleSet::default());

        // Initially no rules — approved.
        let event = make_event(Classification::FileUnlink {
            path: PathBuf::from("/workspace/file.txt"),
        });
        assert!(matches!(gate.evaluate(event).await, PolicyOutcome::Approved(_)));

        // Hot-swap in a block rule.
        let mut rs = RuleSet::default();
        rs.block.push(Rule::new(
            MatchKind::Unlink,
            vec!["/workspace/**".into()],
            vec![],
            vec![],
        ));
        rs.compile_patterns();
        gate.rules.store(Arc::new(rs));

        // Now the same event is blocked.
        let event = make_event(Classification::FileUnlink {
            path: PathBuf::from("/workspace/file.txt"),
        });
        assert!(matches!(gate.evaluate(event).await, PolicyOutcome::Blocked { .. }));
    }
}
