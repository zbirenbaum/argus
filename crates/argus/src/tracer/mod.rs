// Rust guideline compliant 2026-02-21
//! Ptrace utilities: seccomp BPF, memory reading, register access, and
//! syscall number constants.
//!
//! The trace loop itself has moved to `argus::pipeline`; this module
//! retains the low-level building blocks used by pipeline stages.

pub mod memory;
pub mod pending;
pub mod regs;
pub mod seccomp;
pub mod syscall_nr;
