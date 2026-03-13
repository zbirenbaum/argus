// Rust guideline compliant 2026-02-21
//! Directives sent from pipeline stages back to the ptrace loop.
//!
//! After processing a stop, a stage sends a [`PipelineDirective`] to the
//! ptrace loop via [`PtraceStream::directive`]. The loop applies it before
//! returning the tracee to user-space execution.

use nix::unistd::Pid;

/// An instruction from a pipeline stage to the ptrace loop.
///
/// Placeholder — the real variants will include all ptrace operations once
/// the ptrace-stream agent lands.
// TODO: replace with real `PipelineDirective` once ptrace-stream agent merges.
#[derive(Debug, Clone)]
pub enum PipelineDirective {
    /// Resume the tracee with `PTRACE_CONT`.
    Resume {
        /// The PID to resume.
        pid: Pid,
    },
    /// Resume the tracee but inject `errno` as the syscall return value.
    InjectError {
        /// The PID to resume.
        pid: Pid,
        /// The errno value to inject (e.g. `libc::EPERM`).
        errno: i32,
    },
}
