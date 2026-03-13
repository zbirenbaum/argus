// Rust guideline compliant 2026-02-21
//! Pipeline stages — composable transforms over the ptrace event stream.

pub mod approvals;
pub mod capture;
pub mod check_rules;
pub mod classify;
pub mod sockaddr;
pub mod stamp;
pub mod syscall_handlers;
pub mod tree;

pub use approvals::ApprovalStage;
pub use capture::CaptureStage;
pub use check_rules::CheckRulesStage;
pub use classify::ClassifyStage;
pub use stamp::StampStage;
pub use tree::TreeStage;
