//! Syscall dispatch and handler functions for seccomp-ptrace stops.
//!
//! Each handler reads arguments from the tracee's registers, updates
//! in-memory state, and emits the corresponding event. Phase 1 does
//! not capture file content -- hashes are `None`.

mod file_ops;
mod io_ops;
mod metadata_ops;
mod net_ops;

use anyhow::Result;
use nix::unistd::Pid;
use tracing::event;
use tracing::Level;

use super::syscall_nr::*;
use super::trace_loop::TracerLoop;

/// Pause-before-action stub for P2 integration.
///
/// Always returns `Allow`. The hook point is wired up in the
/// pause-resume-api phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseAction {
    Allow,
}

fn check_pause_rules(_pid: Pid, _syscall_nr: u64) -> PauseAction {
    PauseAction::Allow
}

/// Dispatches a seccomp stop to the appropriate handler.
///
/// Reads the syscall number from registers and delegates to
/// category-specific handler functions.
///
/// # Errors
///
/// Returns an error if register reads or handler logic fails.
pub fn handle_seccomp_stop(tracer: &mut TracerLoop, pid: Pid) -> Result<()> {
    let regs = super::regs::get_regs(pid)?;
    let nr = super::regs::syscall_nr(&regs);

    let _action = check_pause_rules(pid, nr);

    match nr {
        // File open/close/dup
        SYS_OPEN | SYS_OPENAT | SYS_OPENAT2 | SYS_CREAT => {
            file_ops::handle_open(tracer, pid, nr, &regs)?;
        }
        SYS_CLOSE => {
            file_ops::handle_close(tracer, pid, &regs)?;
        }
        SYS_DUP | SYS_DUP2 | SYS_DUP3 => {
            file_ops::handle_dup(tracer, pid, nr, &regs)?;
        }
        SYS_FCNTL => {
            file_ops::handle_fcntl(tracer, pid, &regs)?;
        }

        // Seek
        SYS_LSEEK => {
            io_ops::handle_lseek(tracer, pid, &regs)?;
        }

        // Read/write
        SYS_READ | SYS_PREAD64 | SYS_READV | SYS_PREADV => {
            io_ops::handle_read(tracer, pid, &regs)?;
        }
        SYS_WRITE | SYS_PWRITE64 | SYS_WRITEV | SYS_PWRITEV => {
            io_ops::handle_write(tracer, pid, &regs)?;
        }

        // File metadata
        SYS_RENAME | SYS_RENAMEAT | SYS_RENAMEAT2 => {
            metadata_ops::handle_rename(tracer, pid, nr, &regs)?;
        }
        SYS_UNLINK | SYS_UNLINKAT => {
            metadata_ops::handle_unlink(tracer, pid, nr, &regs)?;
        }
        SYS_MKDIR | SYS_MKDIRAT => {
            metadata_ops::handle_mkdir(tracer, pid, nr, &regs)?;
        }
        SYS_RMDIR => {
            metadata_ops::handle_rmdir(tracer, pid, &regs)?;
        }
        SYS_CHMOD | SYS_FCHMOD | SYS_FCHMODAT => {
            metadata_ops::handle_chmod(tracer, pid, nr, &regs)?;
        }
        SYS_CHOWN | SYS_FCHOWN | SYS_FCHOWNAT => {
            metadata_ops::handle_chown(tracer, pid, nr, &regs)?;
        }
        SYS_TRUNCATE | SYS_FTRUNCATE => {
            metadata_ops::handle_truncate(tracer, pid, nr, &regs)?;
        }
        SYS_LINK | SYS_LINKAT => {
            metadata_ops::handle_link(tracer, pid, nr, &regs)?;
        }
        SYS_SYMLINK | SYS_SYMLINKAT => {
            metadata_ops::handle_symlink(tracer, pid, nr, &regs)?;
        }
        SYS_READLINK | SYS_READLINKAT => {
            // Readlink is informational; no event needed in phase 1.
        }

        // Pipe/PTY
        SYS_PIPE | SYS_PIPE2 => {
            io_ops::handle_pipe(tracer, pid, &regs)?;
        }
        SYS_IOCTL => {
            io_ops::handle_ioctl(tracer, pid, &regs)?;
        }

        // Network
        SYS_SOCKET => {
            net_ops::handle_socket(tracer, pid, &regs)?;
        }
        SYS_CONNECT => {
            net_ops::handle_connect(tracer, pid, &regs)?;
        }
        SYS_ACCEPT | SYS_ACCEPT4 => {
            net_ops::handle_accept(tracer, pid, &regs)?;
        }
        SYS_BIND | SYS_LISTEN | SYS_SENDTO | SYS_SENDMSG
        | SYS_RECVFROM | SYS_RECVMSG => {
            // Phase 1: tracked via fd table but no events emitted.
        }

        // Process lifecycle handled via PTRACE_EVENT, not seccomp.
        SYS_FORK | SYS_VFORK | SYS_CLONE | SYS_CLONE3
        | SYS_EXECVE | SYS_EXECVEAT | SYS_EXIT | SYS_EXIT_GROUP => {}

        other => {
            event!(
                name: "tracer.syscall.unhandled",
                Level::TRACE,
                pid = pid.as_raw(),
                syscall_nr = other,
                "unhandled seccomp stop for syscall {{syscall_nr}} in pid {{pid}}",
            );
        }
    }

    Ok(())
}
