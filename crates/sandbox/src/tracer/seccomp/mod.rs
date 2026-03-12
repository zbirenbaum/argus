//! Seccomp-BPF filter for x86_64 syscall interception.
//!
//! Installs a BPF program that selectively traps syscalls via
//! `SECCOMP_RET_TRACE` for ptrace interception, blocks io_uring with
//! `SECCOMP_RET_ERRNO`, and allows everything else at native speed.

use anyhow::{Context, Result};

use crate::tracer::seccomp::bpf::{build_filter_program, SyscallAction};

mod bpf;
mod syscalls;

pub use syscalls::{BLOCKED_SYSCALLS, TRACED_SYSCALLS};

/// Installs the seccomp-BPF filter for ptrace syscall tracing.
///
/// Must be called in the child process after `fork()` and before `exec()`.
/// Sets `PR_SET_NO_NEW_PRIVS` first (required by seccomp), then loads the
/// BPF program that routes syscalls to ptrace stops, errno returns, or
/// native execution.
///
/// # Errors
///
/// Returns an error if `prctl` calls fail (e.g., missing `CAP_SYS_ADMIN`
/// or kernel seccomp support).
pub fn install_seccomp_filter() -> Result<()> {
    // Required before seccomp filter installation on unprivileged processes.
    let ret = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
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

    let prog = libc::sock_fprog {
        len: program.len() as u16,
        // SAFETY: kernel does not mutate the BPF filter array
        filter: program.as_ptr() as *mut libc::sock_filter,
    };

    // SAFETY: `prog` points to a valid BPF program that lives for the
    // duration of this call. `prctl` with `PR_SET_SECCOMP` copies the
    // program into the kernel, so no lifetime issues after return.
    let ret = unsafe {
        libc::prctl(
            libc::PR_SET_SECCOMP,
            libc::SECCOMP_MODE_FILTER,
            &prog as *const libc::sock_fprog,
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

/// Returns `true` if the given syscall number will be trapped by the filter.
pub fn is_trapped(syscall_nr: u64) -> bool {
    TRACED_SYSCALLS.contains(&syscall_nr) || BLOCKED_SYSCALLS.contains(&syscall_nr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trapped_syscalls_count() {
        assert_eq!(TRACED_SYSCALLS.len(), 61, "expected 61 traced syscalls");
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
        // getpid (39) is not in either list
        assert!(!is_trapped(39), "getpid should not be trapped");
        // getuid (102)
        assert!(!is_trapped(102), "getuid should not be trapped");
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
        // Fork a child, attach ptrace, install filter, and let it exit.
        // SECCOMP_RET_TRACE without a ptracer kills the process, so the
        // parent must act as a minimal tracer.
        //
        // SAFETY: fork/ptrace/waitpid in a controlled test.
        unsafe {
            let pid = libc::fork();
            assert!(pid >= 0, "fork failed");

            if pid == 0 {
                // Let parent attach before we install the filter.
                libc::ptrace(libc::PTRACE_TRACEME, 0, 0, 0);
                libc::raise(libc::SIGSTOP);
                install_seccomp_filter().expect("filter install failed");
                libc::_exit(0);
            }

            // Wait for child's initial SIGSTOP.
            let mut status: libc::c_int = 0;
            libc::waitpid(pid, &mut status, 0);

            // Resume and handle seccomp stops until child exits.
            libc::ptrace(libc::PTRACE_CONT, pid, 0, 0);
            loop {
                let ret = libc::waitpid(pid, &mut status, 0);
                if ret < 0 {
                    let err = std::io::Error::last_os_error();
                    // ECHILD means no more children (possible with threads).
                    if err.raw_os_error() == Some(libc::ECHILD) {
                        break;
                    }
                    panic!("waitpid failed: {err}");
                }
                if libc::WIFEXITED(status) {
                    assert_eq!(libc::WEXITSTATUS(status), 0);
                    break;
                }
                // Continue past any ptrace stops (seccomp or signal).
                libc::ptrace(libc::PTRACE_CONT, pid, 0, 0);
            }
        }
    }
}
