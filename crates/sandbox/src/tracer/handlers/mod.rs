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
use nix::sys::ptrace;
use nix::unistd::Pid;
use tracing::event;
use tracing::Level;

use crate::state::FdTarget;

use super::memory;
use super::regs;
use super::syscall_nr::*;
use super::trace_loop::{CaptureKind, PendingCapture, TracerLoop, hash_file_content};

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
/// Returns `true` if the tracee was already resumed via
/// `ptrace::syscall` (for file-mutating ops that need exit capture).
/// Returns `false` when the caller must call `ptrace::cont`.
///
/// # Errors
///
/// Returns an error if register reads or handler logic fails.
pub fn handle_seccomp_stop(tracer: &mut TracerLoop, pid: Pid) -> Result<bool> {
    let r = regs::get_regs(pid)?;
    let nr = regs::syscall_nr(&r);

    let _action = check_pause_rules(pid, nr);

    match nr {
        // File open/close/dup — capture O_TRUNC as a mutation.
        SYS_OPEN | SYS_OPENAT | SYS_OPENAT2 | SYS_CREAT => {
            if try_start_open_trunc_capture(tracer, pid, nr, &r)? {
                return Ok(true);
            }
            file_ops::handle_open(tracer, pid, nr, &r)?;
        }
        SYS_CLOSE => {
            file_ops::handle_close(tracer, pid, &r)?;
        }
        SYS_DUP | SYS_DUP2 | SYS_DUP3 => {
            file_ops::handle_dup(tracer, pid, nr, &r)?;
        }
        SYS_FCNTL => {
            file_ops::handle_fcntl(tracer, pid, &r)?;
        }

        // Seek
        SYS_LSEEK => {
            io_ops::handle_lseek(tracer, pid, &r)?;
        }

        // Read
        SYS_READ | SYS_PREAD64 | SYS_READV | SYS_PREADV => {
            io_ops::handle_read(tracer, pid, &r)?;
        }

        // Write — capture before/after hashes for file targets.
        SYS_WRITE | SYS_PWRITE64 | SYS_WRITEV | SYS_PWRITEV => {
            if try_start_write_capture(tracer, pid, &r)? {
                return Ok(true);
            }
            io_ops::handle_write(tracer, pid, &r)?;
        }

        // File metadata
        SYS_RENAME | SYS_RENAMEAT | SYS_RENAMEAT2 => {
            metadata_ops::handle_rename(tracer, pid, nr, &r)?;
        }
        SYS_UNLINK | SYS_UNLINKAT => {
            metadata_ops::handle_unlink(tracer, pid, nr, &r)?;
        }
        SYS_MKDIR | SYS_MKDIRAT => {
            metadata_ops::handle_mkdir(tracer, pid, nr, &r)?;
        }
        SYS_RMDIR => {
            metadata_ops::handle_rmdir(tracer, pid, &r)?;
        }
        SYS_CHMOD | SYS_FCHMOD | SYS_FCHMODAT => {
            metadata_ops::handle_chmod(tracer, pid, nr, &r)?;
        }
        SYS_CHOWN | SYS_FCHOWN | SYS_FCHOWNAT => {
            metadata_ops::handle_chown(tracer, pid, nr, &r)?;
        }
        SYS_TRUNCATE | SYS_FTRUNCATE => {
            metadata_ops::handle_truncate(tracer, pid, nr, &r)?;
        }
        SYS_LINK | SYS_LINKAT => {
            metadata_ops::handle_link(tracer, pid, nr, &r)?;
        }
        SYS_SYMLINK | SYS_SYMLINKAT => {
            metadata_ops::handle_symlink(tracer, pid, nr, &r)?;
        }
        SYS_READLINK | SYS_READLINKAT => {}

        // Pipe/PTY
        SYS_PIPE | SYS_PIPE2 => {
            io_ops::handle_pipe(tracer, pid, &r)?;
        }
        SYS_IOCTL => {
            io_ops::handle_ioctl(tracer, pid, &r)?;
        }

        // Network
        SYS_SOCKET => {
            net_ops::handle_socket(tracer, pid, &r)?;
        }
        SYS_CONNECT => {
            net_ops::handle_connect(tracer, pid, &r)?;
        }
        SYS_ACCEPT | SYS_ACCEPT4 => {
            net_ops::handle_accept(tracer, pid, &r)?;
        }
        SYS_BIND | SYS_LISTEN | SYS_SENDTO | SYS_SENDMSG
        | SYS_RECVFROM | SYS_RECVMSG => {}

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

    Ok(false)
}

/// Starts a write capture for file targets.
///
/// Hashes the file before the write, stores a [`PendingCapture`],
/// and resumes with `ptrace::syscall` to catch the exit stop.
/// Returns `true` if capture was started (tracee already resumed).
fn try_start_write_capture(
    tracer: &mut TracerLoop,
    pid: Pid,
    r: &regs::UserRegs,
) -> Result<bool> {
    let fd = regs::arg1(r) as i32;
    let size = regs::arg3(r);
    let pid_u32 = pid.as_raw() as u32;

    let target = io_ops::resolve_fd_target(tracer, pid_u32, fd);
    let path = match target {
        FdTarget::File { ref path } => path.to_string_lossy().into_owned(),
        _ => return Ok(false),
    };

    let before_hash = hash_file_content(&tracer.cas, &path);

    tracer.pending_captures.insert(pid_u32, PendingCapture {
        before_hash,
        path,
        pid: pid_u32,
        kind: CaptureKind::Write { fd, size },
    });

    ptrace::syscall(pid, None)?;
    Ok(true)
}

/// `O_TRUNC` flag — truncate file on open.
const O_TRUNC: u64 = 0x200;
/// `O_WRONLY` flag.
const O_WRONLY: u64 = 0x1;
/// `O_RDWR` flag.
const O_RDWR: u64 = 0x2;

/// Captures open(O_TRUNC) as a file mutation so the hash chain
/// includes truncations between writes.
fn try_start_open_trunc_capture(
    tracer: &mut TracerLoop,
    pid: Pid,
    nr: u64,
    r: &regs::UserRegs,
) -> Result<bool> {
    let flags = match nr {
        SYS_OPEN => regs::arg2(r),
        SYS_OPENAT => regs::arg3(r),
        SYS_CREAT => return Ok(false),
        _ => return Ok(false),
    };

    if flags & O_TRUNC == 0 {
        return Ok(false);
    }
    // Only capture if the open is for writing.
    if flags & O_WRONLY == 0 && flags & O_RDWR == 0 {
        return Ok(false);
    }

    let path = resolve_open_path(pid, nr, r)?;
    let before_hash = hash_file_content(&tracer.cas, &path);

    // Skip capture if file doesn't exist yet (nothing to truncate).
    if before_hash.is_none() {
        return Ok(false);
    }

    let pid_u32 = pid.as_raw() as u32;
    tracer.pending_captures.insert(pid_u32, PendingCapture {
        before_hash,
        path,
        pid: pid_u32,
        kind: CaptureKind::OpenTrunc,
    });

    ptrace::syscall(pid, None)?;
    Ok(true)
}

/// Reads the path argument from an open/openat syscall.
fn resolve_open_path(
    pid: Pid,
    nr: u64,
    r: &regs::UserRegs,
) -> Result<String> {
    match nr {
        SYS_OPEN => memory::read_c_string(pid, regs::arg1(r)),
        SYS_OPENAT => {
            let dirfd = regs::arg1(r) as i32;
            let p = memory::read_path_at(pid, dirfd, regs::arg2(r))?;
            Ok(p.to_string_lossy().into_owned())
        }
        _ => Ok(String::new()),
    }
}
