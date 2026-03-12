// Rust guideline compliant 2026-02-21
//! Architecture-abstracted register access for ptrace.
//!
//! The production target is x86_64, but development/CI may use aarch64
//! (e.g., Apple Silicon Docker). This module provides uniform access to
//! syscall arguments regardless of architecture.

use libc::user_regs_struct;

/// Returns the syscall number from registers.
pub fn syscall_nr(regs: &user_regs_struct) -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        regs.orig_rax
    }
    #[cfg(target_arch = "aarch64")]
    {
        regs.regs[8]
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        compile_error!("unsupported architecture");
    }
}

/// Returns the syscall return value.
pub fn syscall_ret(regs: &user_regs_struct) -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        regs.rax
    }
    #[cfg(target_arch = "aarch64")]
    {
        regs.regs[0]
    }
}

/// Returns syscall argument 1 (rdi on x86_64, x0 on aarch64).
pub fn arg1(regs: &user_regs_struct) -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        regs.rdi
    }
    #[cfg(target_arch = "aarch64")]
    {
        regs.regs[0]
    }
}

/// Returns syscall argument 2 (rsi on x86_64, x1 on aarch64).
pub fn arg2(regs: &user_regs_struct) -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        regs.rsi
    }
    #[cfg(target_arch = "aarch64")]
    {
        regs.regs[1]
    }
}

/// Returns syscall argument 3 (rdx on x86_64, x2 on aarch64).
pub fn arg3(regs: &user_regs_struct) -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        regs.rdx
    }
    #[cfg(target_arch = "aarch64")]
    {
        regs.regs[2]
    }
}

/// Returns syscall argument 4 (r10 on x86_64, x3 on aarch64).
pub fn arg4(regs: &user_regs_struct) -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        regs.r10
    }
    #[cfg(target_arch = "aarch64")]
    {
        regs.regs[3]
    }
}

/// Returns syscall argument 5 (r8 on x86_64, x4 on aarch64).
pub fn arg5(regs: &user_regs_struct) -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        regs.r8
    }
    #[cfg(target_arch = "aarch64")]
    {
        regs.regs[4]
    }
}
