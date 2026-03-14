// Rust guideline compliant 2026-02-21
//! Unified policy enforcement stage.
//!
//! Replaces the separate `CheckRulesStage` + `ApprovalStage` with a single
//! gate that evaluates the hot-swappable ruleset, handles block/ask-user
//! verdicts, and returns a stream-compatible [`PolicyOutcome`].
//!
//! The `Analyze` variant is defined for future analyzer-service integration
//! but is not yet wired to an HTTP client.

use std::sync::Arc;

use arc_swap::ArcSwap;
use tracing::{Level, event};

use crate::api::routes::submit_pending_approval;
use crate::api::state::SharedState;
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
}

impl PolicyGate {
    /// Create a new policy gate.
    pub fn new(
        handle: PtraceHandle,
        rules: Arc<ArcSwap<RuleSet>>,
        shared: SharedState,
    ) -> Self {
        Self { handle, rules, shared }
    }

    /// Evaluate a classified event against the policy ruleset.
    ///
    /// Returns `Approved` when the event should continue through the
    /// pipeline, or `Blocked` when EPERM was injected and the event
    /// should be recorded as blocked. For ask-user verdicts the method
    /// blocks (async) until the human decision arrives via the API.
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
                let pid_raw = event.pid.as_raw() as u32;
                let syscall = event.syscall_name();
                let evt_path = event.primary_path();

                event!(
                    name: "pipeline.policy.ask_user",
                    Level::INFO,
                    pid = pid_raw,
                    rule = reason.as_str(),
                    "policy gate waiting for human decision",
                );

                self.shared.emit(EventPayload::PendingApproval(
                    control::PendingApproval {
                        pid: pid_raw,
                        syscall: syscall.clone(),
                        path: evt_path.clone(),
                        binary: None,
                        rule_name: reason.clone(),
                    },
                ));

                let (_action_id, rx) = submit_pending_approval(
                    &self.shared,
                    pid_raw,
                    format!("pid:{pid_raw}"),
                    syscall.clone(),
                    evt_path.clone(),
                    reason.clone(),
                );

                let decision = rx.await.unwrap_or(ApprovalDecision::Deny);

                if decision == ApprovalDecision::Deny {
                    self.handle.inject_error(event.pid, libc::EPERM);
                    PolicyOutcome::Blocked {
                        pid: pid_raw,
                        syscall,
                        path: evt_path,
                        reason,
                    }
                } else {
                    PolicyOutcome::Approved(event)
                }
            }
        }
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
        let (tx, rx) = mpsc::unbounded_channel();
        let handle = PtraceHandle::from_sender(tx);
        let rules_swap = Arc::new(ArcSwap::new(Arc::new(rules)));
        let shared = test_shared();
        (PolicyGate::new(handle, rules_swap, shared), rx)
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
        let directive = rx.try_recv().unwrap();
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

        let directive = rx.try_recv().unwrap();
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
