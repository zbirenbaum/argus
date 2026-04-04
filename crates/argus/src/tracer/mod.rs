// Rust guideline compliant 2026-02-21
//! Ptrace utilities: seccomp BPF, memory reading, register access, and
//! syscall number constants.
//!
//! The trace loop itself has moved to `argus::pipeline`; this module
//! retains the low-level building blocks used by pipeline stages.

pub(crate) mod memory;
pub(crate) mod regs;
// seccomp stays pub: supervisor's startup.rs calls install_seccomp_filter directly.
pub mod seccomp;
