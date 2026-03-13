// Rust guideline compliant 2026-02-21
//! Approval stage: routes classified events through the approver chain.
//!
//! The `Approvers` type is synchronous by design (the ptrace loop blocks
//! a tracee at syscall entry). This stage is therefore also sync.

use uuid::Uuid;

use crate::approver::{ApprovalRequest, Approvers};
use crate::pipeline::classified::{ClassifiedEvent, Classification};

/// Stage that consults the configured approver chain.
pub struct ApprovalStage {
    pub approvers: Approvers,
}

impl ApprovalStage {
    /// Create a new approval stage wrapping the given approver chain.
    pub fn new(approvers: Approvers) -> Self {
        Self { approvers }
    }

    /// Return `true` if the event is approved (allowed), `false` if denied.
    ///
    /// A fresh action ID is generated for each call so the approver log
    /// can correlate the decision back to the event.
    pub fn process(&self, event: &ClassifiedEvent) -> bool {
        let request = build_request(event);
        self.approvers.judge(&request).is_allow()
    }
}

/// Construct an [`ApprovalRequest`] from a classified event.
fn build_request(event: &ClassifiedEvent) -> ApprovalRequest {
    let pid = event.pid.as_raw() as u32;
    let (syscall, path, binary, destination) = classify_to_request_fields(&event.classification);

    ApprovalRequest {
        action_id: Uuid::new_v4().to_string(),
        pid,
        process: format!("pid:{pid}"),
        syscall,
        path,
        binary,
        destination,
        rule_description: String::new(),
    }
}

/// Extract the relevant ApprovalRequest fields from a classification.
fn classify_to_request_fields(
    c: &Classification,
) -> (String, Option<String>, Option<String>, Option<String>) {
    match c {
        Classification::FileWrite { path, .. } => {
            ("write".into(), Some(path.to_string_lossy().into()), None, None)
        }
        Classification::FileRead { path, .. } => {
            ("read".into(), Some(path.to_string_lossy().into()), None, None)
        }
        Classification::FileUnlink { path } => {
            ("unlink".into(), Some(path.to_string_lossy().into()), None, None)
        }
        Classification::FileRename { old_path, .. } => {
            ("rename".into(), Some(old_path.to_string_lossy().into()), None, None)
        }
        Classification::FileChmod { path, .. } => {
            ("chmod".into(), Some(path.to_string_lossy().into()), None, None)
        }
        Classification::FileTruncate { path, .. } => {
            ("truncate".into(), Some(path.to_string_lossy().into()), None, None)
        }
        Classification::ProcessExec { binary, .. } => {
            let name = binary.to_string_lossy().into_owned();
            ("exec".into(), None, Some(name), None)
        }
        Classification::NetConnect { addr, .. } => {
            ("connect".into(), None, None, Some(addr.to_string()))
        }
        _ => ("passthrough".into(), None, None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use nix::unistd::Pid;
    use crate::approver::{Approver, DynApprover, Verdict};
    use crate::pipeline::raw_stop::{RawSyscallStop, StopType, SyscallArgs};

    struct AllowAll;
    impl Approver for AllowAll {
        fn judge(&self, _req: &ApprovalRequest) -> anyhow::Result<Verdict> {
            Ok(Verdict::allow("ok", "test"))
        }
        fn name(&self) -> &str { "allow-all" }
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

    #[test]
    fn allow_all_approver_approves() {
        let mut approvers = Approvers::new();
        approvers.push(DynApprover::new(AllowAll));
        let stage = ApprovalStage::new(approvers);
        let event = make_event(Classification::FileUnlink {
            path: PathBuf::from("/workspace/test.txt"),
        });
        assert!(stage.process(&event));
    }

    #[test]
    fn empty_chain_allows() {
        let stage = ApprovalStage::new(Approvers::new());
        let event = make_event(Classification::Passthrough);
        assert!(stage.process(&event));
    }
}
