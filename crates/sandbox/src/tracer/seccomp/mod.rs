// Rust guideline compliant 2026-02-21
//! Seccomp-BPF filter for x86_64 syscall interception.
//!
//! Installs a BPF program that selectively traps syscalls via
//! `SECCOMP_RET_TRACE` for ptrace interception, blocks io_uring with
//! `SECCOMP_RET_ERRNO`, and allows everything else at native speed.

use anyhow::{Context, Result};

use crate::tracer::seccomp::bpf::{SockFilter, SyscallAction, build_filter_program};

mod bpf;
mod syscalls;

pub use syscalls::{BLOCKED_SYSCALLS, TRACED_SYSCALLS};

/// Installs the seccomp-BPF filter for ptrace syscall tracing.
///
/// Must be called in the child process after `fork()` and before
/// `exec()`. Sets `PR_SET_NO_NEW_PRIVS` first (required by seccomp),
/// then loads the BPF program that routes syscalls to ptrace stops,
/// errno returns, or native execution.
///
/// # Errors
///
/// Returns an error if `prctl` calls fail (e.g., missing
/// `CAP_SYS_ADMIN` or kernel seccomp support).
pub fn install_seccomp_filter() -> Result<()> {
    // SAFETY: prctl with PR_SET_NO_NEW_PRIVS is safe and idempotent.
    let ret = unsafe {
        nix::libc::prctl(nix::libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0)
    };
    if ret != 0 {
        return Err(std::io::Error::last_os_error())
            .context("prctl(PR_SET_NO_NEW_PRIVS) failed");
    }

    let traced: Vec<SyscallAction> = TRACED_SYSCALLS
        .iter()
        .map(|&nr| SyscallAction::Trace(nr))
        .collect();

    let blocked: Vec<SyscallAction> = BLOCKED_SYSCALLS
        .iter()
        .map(|&nr| SyscallAction::Errno(nr))
        .collect();

    let mut actions = traced;
    actions.extend(blocked);

    let program = build_filter_program(&actions);

    // Cast our SockFilter array to libc::sock_filter — identical
    // #[repr(C)] layout: {u16, u8, u8, u32}.
    let filter_ptr = program.as_ptr().cast::<nix::libc::sock_filter>();

    let prog = nix::libc::sock_fprog {
        len: program.len() as u16,
        filter: filter_ptr as *mut nix::libc::sock_filter,
    };

    // SAFETY: `prog` points to a valid BPF program that lives for the
    // duration of this call. prctl copies the program into the kernel.
    let ret = unsafe {
        nix::libc::prctl(
            nix::libc::PR_SET_SECCOMP,
            nix::libc::SECCOMP_MODE_FILTER,
            &prog as *const nix::libc::sock_fprog,
        )
    };
    if ret != 0 {
        return Err(std::io::Error::last_os_error())
            .context("prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER) failed");
    }

    Ok(())
}

/// Returns all syscall numbers that generate ptrace stops.
pub fn trapped_syscalls() -> &'static [u64] {
    TRACED_SYSCALLS
}

/// Returns `true` if the given syscall number will be trapped.
pub fn is_trapped(syscall_nr: u64) -> bool {
    TRACED_SYSCALLS.contains(&syscall_nr)
        || BLOCKED_SYSCALLS.contains(&syscall_nr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trapped_syscalls_count() {
        #[cfg(target_arch = "x86_64")]
        assert_eq!(TRACED_SYSCALLS.len(), 61, "expected 61 traced syscalls on x86_64");
        #[cfg(target_arch = "aarch64")]
        assert_eq!(TRACED_SYSCALLS.len(), 46, "expected 46 traced syscalls on aarch64");
        assert_eq!(BLOCKED_SYSCALLS.len(), 3, "expected 3 blocked syscalls");
    }

    #[test]
    fn is_trapped_returns_true_for_traced() {
        for &nr in TRACED_SYSCALLS {
            assert!(is_trapped(nr), "syscall {nr} should be trapped");
        }
    }

    #[test]
    fn is_trapped_returns_true_for_blocked() {
        for &nr in BLOCKED_SYSCALLS {
            assert!(is_trapped(nr), "blocked syscall {nr} should be trapped");
        }
    }

    #[test]
    fn is_trapped_returns_false_for_untraced() {
        // getpid: 39 on x86_64, 172 on aarch64
        #[cfg(target_arch = "x86_64")]
        {
            assert!(!is_trapped(39), "getpid should not be trapped");
            assert!(!is_trapped(102), "getuid should not be trapped");
        }
        #[cfg(target_arch = "aarch64")]
        {
            assert!(!is_trapped(172), "getpid should not be trapped");
            assert!(!is_trapped(174), "getuid should not be trapped");
        }
    }

    #[test]
    fn trapped_syscalls_fn_returns_traced_list() {
        assert_eq!(trapped_syscalls().len(), TRACED_SYSCALLS.len());
        assert!(std::ptr::eq(trapped_syscalls(), TRACED_SYSCALLS));
    }

    #[test]
    fn bpf_program_is_well_formed() {
        let traced: Vec<SyscallAction> = TRACED_SYSCALLS
            .iter()
            .map(|&nr| SyscallAction::Trace(nr))
            .collect();
        let blocked: Vec<SyscallAction> = BLOCKED_SYSCALLS
            .iter()
            .map(|&nr| SyscallAction::Errno(nr))
            .collect();

        let mut actions = traced;
        actions.extend(blocked);
        let program = build_filter_program(&actions);

        assert!(!program.is_empty(), "BPF program must not be empty");
        assert!(
            program.len() <= 4096,
            "BPF program exceeds max instruction limit"
        );
    }

    #[test]
    fn no_duplicate_syscall_numbers() {
        let mut all: Vec<u64> = TRACED_SYSCALLS.to_vec();
        all.extend_from_slice(BLOCKED_SYSCALLS);
        all.sort_unstable();
        let before = all.len();
        all.dedup();
        assert_eq!(
            before,
            all.len(),
            "duplicate syscall numbers found in traced/blocked lists"
        );
    }

    #[test]
    #[ignore]
    fn install_filter_succeeds_on_linux() {
        use nix::sys::ptrace;
        use nix::sys::signal::Signal;
        use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
        use nix::unistd::ForkResult;

        // SAFETY: fork/ptrace/waitpid in a controlled test.
        unsafe {
            match nix::unistd::fork().expect("fork failed") {
                ForkResult::Child => {
                    ptrace::traceme().expect("traceme failed");
                    nix::sys::signal::raise(Signal::SIGSTOP)
                        .expect("raise failed");
                    install_seccomp_filter()
                        .expect("filter install failed");
                    std::process::exit(0);
                }
                ForkResult::Parent { child } => {
                    waitpid(child, None).expect("waitpid failed");
                    ptrace::cont(child, None).expect("cont failed");

                    loop {
                        match waitpid(
                            child,
                            Some(WaitPidFlag::__WALL),
                        ) {
                            Ok(WaitStatus::Exited(_, code)) => {
                                assert_eq!(code, 0);
                                break;
                            }
                            Ok(WaitStatus::Stopped(pid, _))
                            | Ok(WaitStatus::PtraceEvent(
                                pid,
                                _,
                                _,
                            )) => {
                                ptrace::cont(pid, None)
                                    .expect("cont failed");
                            }
                            Err(nix::errno::Errno::ECHILD) => break,
                            Err(e) => panic!("waitpid failed: {e}"),
                            _ => {}
                        }
                    }
                }
            }
        }
    }
}
