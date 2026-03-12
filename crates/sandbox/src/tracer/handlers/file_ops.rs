// Rust guideline compliant 2026-02-21
//! File open/close/dup and metadata syscall handlers.

use anyhow::Result;
use libc::user_regs_struct;
use nix::unistd::Pid;
use tracing::event;
use tracing::Level;

use crate::events::EventPayload;
use crate::events::file as ef;
use crate::state::FdTarget;
use crate::tracer::memory;
use crate::tracer::regs;
use crate::tracer::syscall_nr::*;
use crate::tracer::trace_loop::TracerLoop;

/// Handles open/openat/openat2/creat by recording the fd-to-path mapping.
pub fn handle_open(
    _tracer: &mut TracerLoop,
    pid: Pid,
    nr: u64,
    r: &user_regs_struct,
) -> Result<()> {
    let path = match nr {
        SYS_OPEN | SYS_CREAT => memory::read_c_string(pid, regs::arg1(r))?,
        SYS_OPENAT | SYS_OPENAT2 => {
            let dirfd = regs::arg1(r) as i32;
            memory::read_path_at(pid, dirfd, regs::arg2(r))?
                .to_string_lossy()
                .into_owned()
        }
        _ => return Ok(()),
    };

    let pid_u32 = pid.as_raw() as u32;
    event!(
        name: "tracer.open.path",
        Level::DEBUG,
        pid = pid_u32,
        file.path = %path,
        "open path recorded for pid {{pid}}: {{file.path}}",
    );

    // Phase 1 limitation: seccomp stops happen on syscall entry before
    // the kernel assigns an fd. Read/write handlers resolve fd targets
    // from /proc/{pid}/fd/{fd} as a fallback.
    Ok(())
}

/// Handles close by removing the fd from the process fd table.
pub fn handle_close(
    tracer: &mut TracerLoop,
    pid: Pid,
    r: &user_regs_struct,
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
    r: &user_regs_struct,
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
        if flags & libc::O_CLOEXEC != 0 {
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
    r: &user_regs_struct,
) -> Result<()> {
    let fd = regs::arg1(r) as i32;
    let cmd = regs::arg2(r) as i32;
    let pid_u32 = pid.as_raw() as u32;

    let Some(proc_state) = tracer.process_tree.get_process_mut(pid_u32) else {
        return Ok(());
    };

    if cmd == libc::F_SETFD {
        let arg = regs::arg3(r) as i32;
        if arg & libc::FD_CLOEXEC != 0 {
            proc_state.fds.set_cloexec(fd);
        } else {
            proc_state.fds.clear_cloexec(fd);
        }
    }

    Ok(())
}

/// Handles rename/renameat/renameat2.
pub fn handle_rename(
    tracer: &mut TracerLoop,
    pid: Pid,
    nr: u64,
    r: &user_regs_struct,
) -> Result<()> {
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
        _ => return Ok(()),
    };

    tracer.emit(EventPayload::Rename(ef::Rename {
        pid: pid_u32,
        old_path,
        new_path,
        tree_hash: None,
    }));
    Ok(())
}

/// Handles unlink/unlinkat.
pub fn handle_unlink(
    tracer: &mut TracerLoop,
    pid: Pid,
    nr: u64,
    r: &user_regs_struct,
) -> Result<()> {
    let pid_u32 = pid.as_raw() as u32;
    let path = match nr {
        SYS_UNLINK => memory::read_c_string(pid, regs::arg1(r))?,
        SYS_UNLINKAT => {
            let dirfd = regs::arg1(r) as i32;
            memory::read_path_at(pid, dirfd, regs::arg2(r))?
                .to_string_lossy()
                .into_owned()
        }
        _ => return Ok(()),
    };

    tracer.emit(EventPayload::Unlink(ef::Unlink {
        pid: pid_u32,
        path,
        content_hash: None,
        tree_hash: None,
    }));
    Ok(())
}

/// Handles mkdir/mkdirat.
pub fn handle_mkdir(
    tracer: &mut TracerLoop,
    pid: Pid,
    nr: u64,
    r: &user_regs_struct,
) -> Result<()> {
    let pid_u32 = pid.as_raw() as u32;
    let path = match nr {
        SYS_MKDIR => memory::read_c_string(pid, regs::arg1(r))?,
        SYS_MKDIRAT => {
            let dirfd = regs::arg1(r) as i32;
            memory::read_path_at(pid, dirfd, regs::arg2(r))?
                .to_string_lossy()
                .into_owned()
        }
        _ => return Ok(()),
    };

    tracer.emit(EventPayload::Mkdir(ef::Mkdir {
        pid: pid_u32,
        path,
        tree_hash: None,
    }));
    Ok(())
}

/// Handles rmdir.
pub fn handle_rmdir(
    tracer: &mut TracerLoop,
    pid: Pid,
    r: &user_regs_struct,
) -> Result<()> {
    let pid_u32 = pid.as_raw() as u32;
    let path = memory::read_c_string(pid, regs::arg1(r))?;

    tracer.emit(EventPayload::Rmdir(ef::Rmdir {
        pid: pid_u32,
        path,
        tree_hash: None,
    }));
    Ok(())
}

/// Handles chmod/fchmod/fchmodat.
pub fn handle_chmod(
    tracer: &mut TracerLoop,
    pid: Pid,
    nr: u64,
    r: &user_regs_struct,
) -> Result<()> {
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
        _ => return Ok(()),
    };

    tracer.emit(EventPayload::Chmod(ef::Chmod {
        pid: pid_u32,
        path,
        old_mode: 0, // Phase 1: not captured pre-call.
        new_mode,
    }));
    Ok(())
}

/// Handles truncate/ftruncate.
pub fn handle_truncate(
    tracer: &mut TracerLoop,
    pid: Pid,
    nr: u64,
    r: &user_regs_struct,
) -> Result<()> {
    let pid_u32 = pid.as_raw() as u32;
    let (path, new_size) = match nr {
        SYS_TRUNCATE => (memory::read_c_string(pid, regs::arg1(r))?, regs::arg2(r)),
        SYS_FTRUNCATE => {
            let fd = regs::arg1(r) as i32;
            let link = format!("/proc/{}/fd/{fd}", pid.as_raw());
            let p = std::fs::read_link(&link).unwrap_or_default();
            (p.to_string_lossy().into_owned(), regs::arg2(r))
        }
        _ => return Ok(()),
    };

    tracer.emit(EventPayload::Truncate(ef::Truncate {
        pid: pid_u32,
        path,
        old_size: 0,
        new_size,
        before_hash: None,
        after_hash: None,
        tree_hash: None,
    }));
    Ok(())
}

/// Handles link/linkat.
pub fn handle_link(
    tracer: &mut TracerLoop,
    pid: Pid,
    nr: u64,
    r: &user_regs_struct,
) -> Result<()> {
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
        _ => return Ok(()),
    };

    tracer.emit(EventPayload::Link(ef::Link {
        pid: pid_u32,
        target,
        link_path,
        tree_hash: None,
    }));
    Ok(())
}

/// Handles symlink/symlinkat.
pub fn handle_symlink(
    tracer: &mut TracerLoop,
    pid: Pid,
    nr: u64,
    r: &user_regs_struct,
) -> Result<()> {
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
        _ => return Ok(()),
    };

    tracer.emit(EventPayload::Symlink(ef::Symlink {
        pid: pid_u32,
        target,
        link_path,
        tree_hash: None,
    }));
    Ok(())
}
