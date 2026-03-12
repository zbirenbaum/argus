// Rust guideline compliant 2026-02-21
//! File metadata syscall handlers (rename, unlink, mkdir, chmod, etc.).
//!
//! Each handler parses syscall arguments at entry, stores them in the
//! unified pending map, and resumes with `ptrace::syscall` for exit
//! capture. Events are only emitted at exit if the kernel returned
//! success (non-negative return value).

use anyhow::Result;
use nix::unistd::Pid;
use tracing::event;
use tracing::Level;

use crate::tracer::memory;
use crate::tracer::pending::PendingSyscall;
use crate::tracer::regs::{self, UserRegs};
use crate::tracer::syscall_nr::*;
use crate::tracer::trace_loop::TracerLoop;

/// Handles rename/renameat/renameat2 at syscall entry.
///
/// Parses path arguments, stores them as `PendingSyscall::Rename`,
/// and resumes with `ptrace::syscall` for exit capture.
pub fn handle_rename(
    tracer: &mut TracerLoop,
    pid: Pid,
    nr: u64,
    r: &UserRegs,
) -> Result<bool> {
    let pid_u32 = pid.as_raw() as u32;
    let (old_path, new_path) = match nr {
        SYS_RENAME => (
            memory::read_c_string(pid, regs::arg1(r))?,
            memory::read_c_string(pid, regs::arg2(r))?,
        ),
        SYS_RENAMEAT | SYS_RENAMEAT2 => {
            let old_dirfd = regs::arg1(r) as i32;
            let new_dirfd = regs::arg3(r) as i32;
            let old = memory::read_path_at(pid, old_dirfd, regs::arg2(r))?;
            let new = memory::read_path_at(pid, new_dirfd, regs::arg4(r))?;
            (
                old.to_string_lossy().into_owned(),
                new.to_string_lossy().into_owned(),
            )
        }
        _ => return Ok(false),
    };

    tracer.pending.insert(pid_u32, PendingSyscall::Rename {
        pid: pid_u32,
        old_path,
        new_path,
    });
    nix::sys::ptrace::syscall(pid, None)?;
    Ok(true)
}

/// Handles unlink/unlinkat at syscall entry.
pub fn handle_unlink(
    tracer: &mut TracerLoop,
    pid: Pid,
    nr: u64,
    r: &UserRegs,
) -> Result<bool> {
    let pid_u32 = pid.as_raw() as u32;
    let path = match nr {
        SYS_UNLINK => memory::read_c_string(pid, regs::arg1(r))?,
        SYS_UNLINKAT => {
            let dirfd = regs::arg1(r) as i32;
            memory::read_path_at(pid, dirfd, regs::arg2(r))?
                .to_string_lossy()
                .into_owned()
        }
        _ => return Ok(false),
    };

    tracer.pending.insert(pid_u32, PendingSyscall::Unlink {
        pid: pid_u32,
        path,
    });
    nix::sys::ptrace::syscall(pid, None)?;
    Ok(true)
}

/// Handles mkdir/mkdirat at syscall entry.
pub fn handle_mkdir(
    tracer: &mut TracerLoop,
    pid: Pid,
    nr: u64,
    r: &UserRegs,
) -> Result<bool> {
    let pid_u32 = pid.as_raw() as u32;
    let path = match nr {
        SYS_MKDIR => memory::read_c_string(pid, regs::arg1(r))?,
        SYS_MKDIRAT => {
            let dirfd = regs::arg1(r) as i32;
            memory::read_path_at(pid, dirfd, regs::arg2(r))?
                .to_string_lossy()
                .into_owned()
        }
        _ => return Ok(false),
    };

    tracer.pending.insert(pid_u32, PendingSyscall::Mkdir {
        pid: pid_u32,
        path,
    });
    nix::sys::ptrace::syscall(pid, None)?;
    Ok(true)
}

/// Handles rmdir at syscall entry.
pub fn handle_rmdir(
    tracer: &mut TracerLoop,
    pid: Pid,
    r: &UserRegs,
) -> Result<bool> {
    let pid_u32 = pid.as_raw() as u32;
    let path = memory::read_c_string(pid, regs::arg1(r))?;

    tracer.pending.insert(pid_u32, PendingSyscall::Rmdir {
        pid: pid_u32,
        path,
    });
    nix::sys::ptrace::syscall(pid, None)?;
    Ok(true)
}

/// Handles chmod/fchmod/fchmodat at syscall entry.
pub fn handle_chmod(
    tracer: &mut TracerLoop,
    pid: Pid,
    nr: u64,
    r: &UserRegs,
) -> Result<bool> {
    let pid_u32 = pid.as_raw() as u32;
    let (path, new_mode) = match nr {
        SYS_CHMOD => (
            memory::read_c_string(pid, regs::arg1(r))?,
            regs::arg2(r) as u32,
        ),
        SYS_FCHMODAT => {
            let dirfd = regs::arg1(r) as i32;
            let p = memory::read_path_at(pid, dirfd, regs::arg2(r))?;
            (p.to_string_lossy().into_owned(), regs::arg3(r) as u32)
        }
        SYS_FCHMOD => {
            let fd = regs::arg1(r) as i32;
            let link = format!("/proc/{}/fd/{fd}", pid.as_raw());
            let p = std::fs::read_link(&link).unwrap_or_default();
            (p.to_string_lossy().into_owned(), regs::arg2(r) as u32)
        }
        _ => return Ok(false),
    };

    tracer.pending.insert(pid_u32, PendingSyscall::Chmod {
        pid: pid_u32,
        path,
        new_mode,
    });
    nix::sys::ptrace::syscall(pid, None)?;
    Ok(true)
}

/// Handles truncate/ftruncate at syscall entry.
pub fn handle_truncate(
    tracer: &mut TracerLoop,
    pid: Pid,
    nr: u64,
    r: &UserRegs,
) -> Result<bool> {
    let pid_u32 = pid.as_raw() as u32;
    let (path, new_size) = match nr {
        SYS_TRUNCATE => (memory::read_c_string(pid, regs::arg1(r))?, regs::arg2(r)),
        SYS_FTRUNCATE => {
            let fd = regs::arg1(r) as i32;
            let link = format!("/proc/{}/fd/{fd}", pid.as_raw());
            let p = std::fs::read_link(&link).unwrap_or_default();
            (p.to_string_lossy().into_owned(), regs::arg2(r))
        }
        _ => return Ok(false),
    };

    tracer.pending.insert(pid_u32, PendingSyscall::Truncate {
        pid: pid_u32,
        path,
        new_size,
    });
    nix::sys::ptrace::syscall(pid, None)?;
    Ok(true)
}

/// Handles link/linkat at syscall entry.
pub fn handle_link(
    tracer: &mut TracerLoop,
    pid: Pid,
    nr: u64,
    r: &UserRegs,
) -> Result<bool> {
    let pid_u32 = pid.as_raw() as u32;
    let (target, link_path) = match nr {
        SYS_LINK => (
            memory::read_c_string(pid, regs::arg1(r))?,
            memory::read_c_string(pid, regs::arg2(r))?,
        ),
        SYS_LINKAT => {
            let old_dirfd = regs::arg1(r) as i32;
            let new_dirfd = regs::arg3(r) as i32;
            let t = memory::read_path_at(pid, old_dirfd, regs::arg2(r))?;
            let l = memory::read_path_at(pid, new_dirfd, regs::arg4(r))?;
            (
                t.to_string_lossy().into_owned(),
                l.to_string_lossy().into_owned(),
            )
        }
        _ => return Ok(false),
    };

    tracer.pending.insert(pid_u32, PendingSyscall::Link {
        pid: pid_u32,
        target,
        link_path,
    });
    nix::sys::ptrace::syscall(pid, None)?;
    Ok(true)
}

/// Handles chown/fchown/fchownat.
///
/// Log-only — no event type exists for chown. Does not use
/// entry/exit pattern since there is no event to validate.
pub fn handle_chown(
    _tracer: &mut TracerLoop,
    pid: Pid,
    nr: u64,
    r: &UserRegs,
) -> Result<()> {
    let pid_u32 = pid.as_raw() as u32;
    let path = match nr {
        SYS_CHOWN => memory::read_c_string(pid, regs::arg1(r))?,
        SYS_FCHOWNAT => {
            let dirfd = regs::arg1(r) as i32;
            memory::read_path_at(pid, dirfd, regs::arg2(r))?
                .to_string_lossy()
                .into_owned()
        }
        SYS_FCHOWN => {
            let fd = regs::arg1(r) as i32;
            let link = format!("/proc/{}/fd/{fd}", pid.as_raw());
            std::fs::read_link(&link)
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        }
        _ => return Ok(()),
    };

    event!(
        name: "tracer.chown",
        Level::DEBUG,
        pid = pid_u32,
        file.path = %path,
        "chown on {{file.path}} for pid {{pid}}",
    );

    Ok(())
}

/// Handles symlink/symlinkat at syscall entry.
pub fn handle_symlink(
    tracer: &mut TracerLoop,
    pid: Pid,
    nr: u64,
    r: &UserRegs,
) -> Result<bool> {
    let pid_u32 = pid.as_raw() as u32;
    let (target, link_path) = match nr {
        SYS_SYMLINK => (
            memory::read_c_string(pid, regs::arg1(r))?,
            memory::read_c_string(pid, regs::arg2(r))?,
        ),
        SYS_SYMLINKAT => {
            let t = memory::read_c_string(pid, regs::arg1(r))?;
            let dirfd = regs::arg2(r) as i32;
            let l = memory::read_path_at(pid, dirfd, regs::arg3(r))?;
            (t, l.to_string_lossy().into_owned())
        }
        _ => return Ok(false),
    };

    tracer.pending.insert(pid_u32, PendingSyscall::Symlink {
        pid: pid_u32,
        target,
        link_path,
    });
    nix::sys::ptrace::syscall(pid, None)?;
    Ok(true)
}
