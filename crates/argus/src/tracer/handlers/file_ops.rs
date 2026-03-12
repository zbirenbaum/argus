//! File open/close/dup/fcntl syscall handlers.

use anyhow::Result;
use nix::unistd::Pid;

use crate::state::FdTarget;
use crate::tracer::memory;
use crate::tracer::regs::{self, UserRegs};
use crate::tracer::syscall_nr::*;
use crate::tracer::trace_loop::TracerLoop;

/// `O_CLOEXEC` flag — close fd on exec. Value per linux/fcntl.h.
const O_CLOEXEC: i32 = 0x80000;
/// `F_SETFD` — set fd flags. Value per linux/fcntl.h.
const F_SETFD: i32 = 2;
/// `FD_CLOEXEC` — close-on-exec fd flag. Value per linux/fcntl.h.
const FD_CLOEXEC: i32 = 1;

/// Handles open/openat/openat2/creat by recording a pending open.
///
/// The fd number is only available after the syscall completes, so
/// this saves the path and flags at entry. The handler in
/// `handle_seccomp_stop` resumes the tracee with `ptrace::syscall`;
/// at exit, `complete_pending_open` reads the return value and
/// inserts the fd into the process fd table.
///
/// Returns `true` if the tracee was resumed (caller should not
/// call `ptrace::cont`).
pub fn handle_open(
    tracer: &mut TracerLoop,
    pid: Pid,
    nr: u64,
    r: &UserRegs,
) -> Result<bool> {
    let (path, flags) = match nr {
        SYS_OPEN => {
            let p = memory::read_c_string(pid, regs::arg1(r))?;
            let f = regs::arg2(r) as i32;
            (p, f)
        }
        SYS_CREAT => {
            let p = memory::read_c_string(pid, regs::arg1(r))?;
            (p, 0)
        }
        SYS_OPENAT | SYS_OPENAT2 => {
            let dirfd = regs::arg1(r) as i32;
            let p = memory::read_path_at(pid, dirfd, regs::arg2(r))?
                .to_string_lossy()
                .into_owned();
            let f = regs::arg3(r) as i32;
            (p, f)
        }
        _ => return Ok(false),
    };

    let pid_u32 = pid.as_raw() as u32;

    use crate::tracer::trace_loop::PendingOpen;
    tracer.pending_opens.insert(pid_u32, PendingOpen { path, flags });

    nix::sys::ptrace::syscall(pid, None)?;
    Ok(true)
}

/// Handles close by removing the fd from the process fd table.
pub fn handle_close(
    tracer: &mut TracerLoop,
    pid: Pid,
    r: &UserRegs,
) -> Result<()> {
    let fd = regs::arg1(r) as i32;
    let pid_u32 = pid.as_raw() as u32;

    let Some(proc_state) = tracer.process_tree.get_process_mut(pid_u32) else {
        return Ok(());
    };

    if let Some(target) = proc_state.fds.remove(fd) {
        if let FdTarget::Pipe { inode, .. } = &target {
            tracer.pipe_registry.on_close(pid_u32, fd, *inode);
        }
    }

    Ok(())
}

/// Handles dup/dup2/dup3 by cloning the fd table entry.
pub fn handle_dup(
    tracer: &mut TracerLoop,
    pid: Pid,
    nr: u64,
    r: &UserRegs,
) -> Result<()> {
    let old_fd = regs::arg1(r) as i32;
    let new_fd = match nr {
        // dup() return value is the new fd; need post-syscall.
        SYS_DUP => return Ok(()),
        SYS_DUP2 | SYS_DUP3 => regs::arg2(r) as i32,
        _ => return Ok(()),
    };

    let pid_u32 = pid.as_raw() as u32;
    let Some(proc_state) = tracer.process_tree.get_process_mut(pid_u32) else {
        return Ok(());
    };

    proc_state.fds.dup(old_fd, new_fd);

    if nr == SYS_DUP3 {
        let flags = regs::arg3(r) as i32;
        if flags & O_CLOEXEC != 0 {
            proc_state.fds.set_cloexec(new_fd);
        }
    }

    tracer
        .pipe_registry
        .on_dup(pid_u32, old_fd, new_fd, &proc_state.fds);

    Ok(())
}

/// Handles fcntl for F_SETFD (cloexec flag management).
pub fn handle_fcntl(
    tracer: &mut TracerLoop,
    pid: Pid,
    r: &UserRegs,
) -> Result<()> {
    let fd = regs::arg1(r) as i32;
    let cmd = regs::arg2(r) as i32;
    let pid_u32 = pid.as_raw() as u32;

    let Some(proc_state) = tracer.process_tree.get_process_mut(pid_u32) else {
        return Ok(());
    };

    if cmd == F_SETFD {
        let arg = regs::arg3(r) as i32;
        if arg & FD_CLOEXEC != 0 {
            proc_state.fds.set_cloexec(fd);
        } else {
            proc_state.fds.clear_cloexec(fd);
        }
    }

    Ok(())
}
