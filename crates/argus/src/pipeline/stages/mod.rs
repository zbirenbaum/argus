// Rust guideline compliant 2026-02-21
//! Pipeline stages — composable transforms over the ptrace event stream.

pub(crate) mod approvals;
pub(crate) mod capture;
pub(crate) mod check_rules;
pub(crate) mod classify;
pub(crate) mod policy_gate;
pub(crate) mod redact;
pub(crate) mod sockaddr;
pub(crate) mod stamp;
pub(crate) mod syscall_handlers;
pub(crate) mod tree;

pub(crate) use capture::CaptureStage;
pub(crate) use classify::ClassifyStage;
pub(crate) use policy_gate::PolicyGate;
pub(crate) use stamp::StampStage;
pub(crate) use tree::TreeStage;
