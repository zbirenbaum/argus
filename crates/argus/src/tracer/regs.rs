// Rust guideline compliant 2026-02-21
//! Architecture-abstracted register access for ptrace.
//!
//! The production target is x86_64, but development/CI may use aarch64
//! (e.g., Apple Silicon Docker). This module provides uniform access to
//! syscall arguments regardless of architecture.
//!
//! Register structs mirror the kernel `user_regs_struct` layout to
//! avoid a direct dependency on libc for this low-level type.

/// x86_64 register set as returned by `PTRACE_GETREGS`.
///
/// Layout matches linux/user_regs_struct for x86_64 exactly.
#[cfg(target_arch = "x86_64")]
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct UserRegs {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub rbp: u64,
    pub rbx: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rax: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub orig_rax: u64,
    pub rip: u64,
    pub cs: u64,
    pub eflags: u64,
    pub rsp: u64,
    pub ss: u64,
    pub fs_base: u64,
    pub gs_base: u64,
    pub ds: u64,
    pub es: u64,
    pub fs: u64,
    pub gs: u64,
}

/// aarch64 register set as returned by `PTRACE_GETREGSET`.
///
/// Layout matches linux/user_pt_regs for aarch64.
#[cfg(target_arch = "aarch64")]
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct UserRegs {
    pub regs: [u64; 31],
    pub sp: u64,
    pub pc: u64,
    pub pstate: u64,
}

/// Returns the syscall number from registers.
pub fn syscall_nr(regs: &UserRegs) -> u64 {
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
pub fn syscall_ret(regs: &UserRegs) -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        regs.rax
    }
    #[cfg(target_arch = "aarch64")]
    {
        regs.regs[0]
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        compile_error!("unsupported architecture");
    }
}

/// Returns syscall argument 1 (rdi on x86_64, x0 on aarch64).
pub fn arg1(regs: &UserRegs) -> u64 {
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
pub fn arg2(regs: &UserRegs) -> u64 {
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
pub fn arg3(regs: &UserRegs) -> u64 {
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
pub fn arg4(regs: &UserRegs) -> u64 {
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
pub fn arg5(regs: &UserRegs) -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        regs.r8
    }
    #[cfg(target_arch = "aarch64")]
    {
        regs.regs[4]
    }
}

/// Cancels a syscall by invalidating the syscall number.
///
/// Sets `orig_rax` to -1 on x86_64 (or `x8` to -1 on aarch64)
/// so the kernel skips execution and returns -ENOSYS. Caller must
/// follow up at syscall exit to set the desired errno.
pub fn cancel_syscall(regs: &mut UserRegs) {
    #[cfg(target_arch = "x86_64")]
    {
        regs.orig_rax = (-1_i64) as u64;
    }
    #[cfg(target_arch = "aarch64")]
    {
        regs.regs[8] = (-1_i64) as u64;
    }
}

/// Reads the syscall return value from the register set.
pub fn ret_val(regs: &UserRegs) -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        regs.rax
    }
    #[cfg(target_arch = "aarch64")]
    {
        regs.regs[0]
    }
}

/// Sets the syscall return value in the register set.
pub fn set_ret(regs: &mut UserRegs, val: u64) {
    #[cfg(target_arch = "x86_64")]
    {
        regs.rax = val;
    }
    #[cfg(target_arch = "aarch64")]
    {
        regs.regs[0] = val;
    }
}

/// Writes the tracee's register set via ptrace.
///
/// Uses `PTRACE_SETREGS` on x86_64 and `PTRACE_SETREGSET` with
/// `NT_PRSTATUS` on aarch64.
///
/// # Errors
///
/// Returns an error if the ptrace call fails.
pub fn set_regs(pid: nix::unistd::Pid, regs: &UserRegs) -> anyhow::Result<()> {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: UserRegs is #[repr(C)] with identical layout to
        // libc::user_regs_struct — same fields in same order.
        let raw: libc::user_regs_struct = unsafe { std::mem::transmute(*regs) };
        nix::sys::ptrace::setregs(pid, raw)?;
        Ok(())
    }
    #[cfg(target_arch = "aarch64")]
    {
        use anyhow::Context;
        const NT_PRSTATUS: libc::c_int = 1;
        let iov = libc::iovec {
            iov_base: (regs as *const UserRegs).cast_mut().cast(),
            iov_len: std::mem::size_of::<UserRegs>(),
        };
        // SAFETY: iov points to a valid UserRegs with correct size.
        let ret = unsafe {
            libc::ptrace(
                libc::PTRACE_SETREGSET,
                pid.as_raw() as libc::c_uint,
                NT_PRSTATUS,
                std::ptr::addr_of!(iov),
            )
        };
        if ret == -1 {
            return Err(std::io::Error::last_os_error())
                .context("PTRACE_SETREGSET(NT_PRSTATUS) failed");
        }
        Ok(())
    }
}

/// Reads the tracee's register set via ptrace.
///
/// Uses `PTRACE_GETREGS` on x86_64 and `PTRACE_GETREGSET` with
/// `NT_PRSTATUS` on aarch64.
///
/// # Errors
///
/// Returns an error if the ptrace call fails.
pub fn get_regs(pid: nix::unistd::Pid) -> anyhow::Result<UserRegs> {
    #[cfg(target_arch = "x86_64")]
    {
        let raw = nix::sys::ptrace::getregs(pid)?;
        // SAFETY: UserRegs is #[repr(C)] with identical layout to
        // libc::user_regs_struct — same fields in same order.
        Ok(unsafe { std::mem::transmute(raw) })
    }
    #[cfg(target_arch = "aarch64")]
    {
        use anyhow::Context;
        const NT_PRSTATUS: libc::c_int = 1;
        let mut regs = UserRegs::default();
        let mut iov = libc::iovec {
            iov_base: std::ptr::addr_of_mut!(regs).cast(),
            iov_len: std::mem::size_of::<UserRegs>(),
        };
        // SAFETY: iov points to a valid UserRegs with correct size.
        let ret = unsafe {
            libc::ptrace(
                libc::PTRACE_GETREGSET,
                pid.as_raw() as libc::c_uint,
                NT_PRSTATUS,
                std::ptr::addr_of_mut!(iov),
            )
        };
        if ret == -1 {
            return Err(std::io::Error::last_os_error())
                .context("PTRACE_GETREGSET(NT_PRSTATUS) failed");
        }
        Ok(regs)
    }
}
