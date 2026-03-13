// Rust guideline compliant 2026-02-21
//! Rule-checking stage: evaluates block and pause-before-action rules.
//!
//! Block rules cause the pipeline to inject EPERM and skip downstream
//! stages. Pause-before-action rules route to the approval stage first.

use std::sync::Arc;

use arc_swap::ArcSwap;

use crate::config::{MatchKind, RuleDecision, RuleSet};
use crate::pipeline::classified::{ClassifiedEvent, Classification};

/// Outcome of a rule check.
pub enum RuleAction {
    /// Inject EPERM immediately; no approval needed.
    Block,
    /// Hold for operator approval before continuing.
    Pause,
}

/// A matched rule with a human-readable description.
pub struct RuleMatch {
    pub description: String,
    pub action: RuleAction,
}

/// Stage that evaluates the active rule set against each classified event.
pub struct CheckRulesStage {
    pub rules: Arc<ArcSwap<RuleSet>>,
}

impl CheckRulesStage {
    /// Create a new stage backed by a hot-swappable rule set.
    pub fn new(rules: Arc<ArcSwap<RuleSet>>) -> Self {
        Self { rules }
    }

    /// Return a block or pause match if any rule fires, or `None` to allow.
    pub fn check_block(&self, event: &ClassifiedEvent) -> Option<RuleMatch> {
        let rules = self.rules.load();
        let (kind, path, binary, dest) = extract_context(&event.classification);

        match rules.evaluate(kind?, path.as_deref(), binary.as_deref(), dest.as_deref()) {
            RuleDecision::Block { rule_index } => Some(RuleMatch {
                description: rules.block_rule_description(rule_index),
                action: RuleAction::Block,
            }),
            RuleDecision::Pause { rule_index } => Some(RuleMatch {
                description: rules.pause_rule_description(rule_index),
                action: RuleAction::Pause,
            }),
            RuleDecision::Allow => None,
        }
    }

    /// Return `true` if a pause-before-action rule matches this event.
    pub fn needs_approval(&self, event: &ClassifiedEvent) -> bool {
        let rules = self.rules.load();
        let (kind, path, binary, dest) = extract_context(&event.classification);
        let Some(kind) = kind else { return false };
        matches!(
            rules.evaluate(kind, path.as_deref(), binary.as_deref(), dest.as_deref()),
            RuleDecision::Pause { .. }
        )
    }
}

/// Extract rule-matching context from a classification.
///
/// Returns `(match_kind, path, binary, destination)`. Returns `None`
/// for the match_kind on events that have no corresponding rule category.
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
    use arc_swap::ArcSwap;
    use crate::config::{MatchKind, Rule, RuleSet};
    use crate::pipeline::raw_stop::{RawSyscallStop, StopType, SyscallArgs};
    use nix::unistd::Pid;

    fn make_event(cls: Classification) -> ClassifiedEvent {
        ClassifiedEvent {
            pid: Pid::from_raw(1),
            raw: RawSyscallStop {
                pid: Pid::from_raw(1),
                stop_type: StopType::SyscallEntry {
                    syscall_nr: 0,
                    args: SyscallArgs::from_array([0; 6]),
                },
            },
            classification: cls,
        }
    }

    #[test]
    fn block_rule_matches_unlink() {
        let mut rs = RuleSet::default();
        rs.block.push(Rule::new(
            MatchKind::Unlink,
            vec!["/workspace/**".into()],
            vec![],
            vec![],
        ));
        rs.compile_patterns();

        let stage = CheckRulesStage::new(Arc::new(ArcSwap::new(Arc::new(rs))));
        let event = make_event(Classification::FileUnlink {
            path: PathBuf::from("/workspace/file.txt"),
        });
        let m = stage.check_block(&event);
        assert!(m.is_some());
        assert!(matches!(m.unwrap().action, RuleAction::Block));
    }

    #[test]
    fn no_match_returns_none() {
        let stage = CheckRulesStage::new(Arc::new(ArcSwap::new(Arc::new(RuleSet::default()))));
        let event = make_event(Classification::Passthrough);
        assert!(stage.check_block(&event).is_none());
    }
}
