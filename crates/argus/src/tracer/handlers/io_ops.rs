//! Read, write, pipe, and PTY syscall handlers.

use anyhow::Result;
use nix::unistd::Pid;
use tracing::event;
use tracing::Level;

use crate::events::EventPayload;
use crate::events::file as ef;
use crate::events::io as eio;
use crate::state::{FdTarget, PipeEnd};
use crate::tracer::regs::{self, UserRegs};
use crate::tracer::trace_loop::TracerLoop;

/// Handles read/pread64/readv/preadv.
///
/// For file-backed fds, records a pending read and returns `true` so
/// the caller resumes with `ptrace::syscall`. At exit, the buffer is
/// read from tracee memory, hashed, and stored in CAS.
///
/// For pipes, PTYs, stdin: emits immediately (content capture deferred
/// to a later phase) and returns `false`.
pub fn handle_read(
    tracer: &mut TracerLoop,
    pid: Pid,
    r: &UserRegs,
) -> Result<bool> {
    let fd = regs::arg1(r) as i32;
    let buf_addr = regs::arg2(r);
    let size = regs::arg3(r);
    let pid_u32 = pid.as_raw() as u32;

    let target = resolve_fd_target(tracer, pid_u32, fd);

    // Stdin is only classified as Stdio when fd 0 is backed by a
    // pipe or PTY, not when redirected to a regular file.
    if fd == 0 && matches!(target, FdTarget::Pipe { .. } | FdTarget::Pty { .. }) {
        use crate::tracer::trace_loop::PendingRead;
        tracer.pending_reads.insert(pid_u32, PendingRead {
            pid: pid_u32,
            fd,
            path: String::new(),
            buf_addr,
            count: size,
        });
        nix::sys::ptrace::syscall(pid, None)?;
        return Ok(true);
    }

    match target {
        FdTarget::File { path } => {
            let path_str = path.to_string_lossy().into_owned();
            use crate::tracer::trace_loop::PendingRead;
            tracer.pending_reads.insert(pid_u32, PendingRead {
                pid: pid_u32,
                fd,
                path: path_str,
                buf_addr,
                count: size,
            });
            nix::sys::ptrace::syscall(pid, None)?;
            Ok(true)
        }
        FdTarget::Pipe { inode, .. } => {
            tracer.emit(EventPayload::PipeData(eio::PipeData {
                pid: pid_u32,
                inode,
                direction: eio::PipeDirection::Read,
                content_hash: None,
                size,
                dest_pids: vec![],
            }));
            Ok(false)
        }
        FdTarget::Pty { peer_path, .. } => {
            tracer.emit(EventPayload::PtyData(eio::PtyData {
                pid: pid_u32,
                subtype: eio::PtySubtype::MasterRead,
                content_hash: None,
                size,
                slave_path: peer_path.to_string_lossy().into_owned(),
            }));
            Ok(false)
        }
        FdTarget::DevNull | FdTarget::Socket { .. } | FdTarget::Unknown => Ok(false),
    }
}

/// Handles write/pwrite64/writev/pwritev for non-file targets.
///
/// File writes are handled by `try_start_write_capture` which captures
/// before/after hashes. This handles the fallthrough: stdio, pipes,
/// PTYs. Captures content from tracee memory at the entry point
/// (write buffers are valid at entry since the caller provides them).
pub fn handle_write(
    tracer: &mut TracerLoop,
    pid: Pid,
    r: &UserRegs,
) -> Result<()> {
    let fd = regs::arg1(r) as i32;
    let buf_addr = regs::arg2(r);
    let size = regs::arg3(r);
    let pid_u32 = pid.as_raw() as u32;

    let target = resolve_fd_target(tracer, pid_u32, fd);

    // Capture write buffer content from tracee memory. For writes,
    // the buffer is valid at entry (caller provides it).
    let content_hash = super::super::content_capture::try_capture_flat(
        &tracer.cas, pid, buf_addr, size,
    );

    // Only classify fd 1/2 as Stdio when the underlying target is a
    // pipe or PTY. When stdout/stderr is redirected to a file, emit
    // a normal Write event instead.
    if (fd == 1 || fd == 2)
        && matches!(target, FdTarget::Pipe { .. } | FdTarget::Pty { .. })
    {
        let subtype = if fd == 1 {
            eio::StdioSubtype::Stdout
        } else {
            eio::StdioSubtype::Stderr
        };
        tracer.emit(EventPayload::Stdio(eio::Stdio {
            pid: pid_u32,
            subtype,
            content_hash,
            size,
            pipe_inode: match &target {
                FdTarget::Pipe { inode, .. } => Some(*inode),
                _ => None,
            },
            dest_pid: None,
            source_pid: None,
        }));
        return Ok(());
    }

    match target {
        FdTarget::File { path } => {
            let tree_hash = tracer.tree_root();
            tracer.emit(EventPayload::Write(ef::Write {
                pid: pid_u32,
                path: path.to_string_lossy().into_owned(),
                fd,
                offset: 0,
                size,
                before_hash: None,
                after_hash: None,
                tree_hash,
            }));
        }
        FdTarget::Pipe { inode, .. } => {
            tracer.emit(EventPayload::PipeData(eio::PipeData {
                pid: pid_u32,
                inode,
                direction: eio::PipeDirection::Write,
                content_hash,
                size,
                dest_pids: vec![],
            }));
        }
        FdTarget::Pty { peer_path, .. } => {
            tracer.emit(EventPayload::PtyData(eio::PtyData {
                pid: pid_u32,
                subtype: eio::PtySubtype::SlaveWrite,
                content_hash,
                size,
                slave_path: peer_path.to_string_lossy().into_owned(),
            }));
        }
        FdTarget::DevNull | FdTarget::Socket { .. } | FdTarget::Unknown => {}
    }

    Ok(())
}

/// Handles pipe/pipe2.
///
/// On syscall entry the kernel has not yet filled the pipefd array,
/// so we cannot read the fds. Phase 1: log only.
pub fn handle_pipe(
    _tracer: &mut TracerLoop,
    pid: Pid,
    _r: &UserRegs,
) -> Result<()> {
    let pid_u32 = pid.as_raw() as u32;

    event!(
        name: "tracer.pipe.create",
        Level::DEBUG,
        pid = pid_u32,
        "pipe syscall detected for pid {{pid}}; post-syscall fd capture not yet implemented",
    );

    Ok(())
}

/// Handles lseek.
///
/// Phase 1: logged for debugging but no event emitted. Offset
/// tracking will be added in a later phase.
pub fn handle_lseek(
    _tracer: &mut TracerLoop,
    pid: Pid,
    r: &UserRegs,
) -> Result<()> {
    let fd = regs::arg1(r) as i32;
    let pid_u32 = pid.as_raw() as u32;

    event!(
        name: "tracer.lseek",
        Level::TRACE,
        pid = pid_u32,
        fd = fd,
        "lseek on fd {{fd}} for pid {{pid}}",
    );

    Ok(())
}

/// Handles ioctl — checks for PTY-related ioctls.
pub fn handle_ioctl(
    _tracer: &mut TracerLoop,
    pid: Pid,
    r: &UserRegs,
) -> Result<()> {
    let fd = regs::arg1(r) as i32;
    let request = regs::arg2(r);
    let pid_u32 = pid.as_raw() as u32;

    // TIOCGPTN: get PTY number.
    const TIOCGPTN: u64 = 0x8004_5430;
    // TIOCSPTLCK: lock/unlock PTY.
    const TIOCSPTLCK: u64 = 0x4004_5431;

    match request {
        TIOCGPTN => {
            event!(
                name: "tracer.pty.getnr",
                Level::DEBUG,
                pid = pid_u32,
                fd = fd,
                "TIOCGPTN on fd {{fd}} for pid {{pid}}",
            );
        }
        TIOCSPTLCK => {
            event!(
                name: "tracer.pty.lock",
                Level::DEBUG,
                pid = pid_u32,
                fd = fd,
                "TIOCSPTLCK on fd {{fd}} for pid {{pid}}",
            );
        }
        _ => {}
    }

    Ok(())
}

/// Resolves an fd to its target using the process fd table, falling
/// back to `/proc/{pid}/fd/{fd}` if the table has no entry.
pub(crate) fn resolve_fd_target(tracer: &TracerLoop, pid_u32: u32, fd: i32) -> FdTarget {
    if let Some(proc_state) = tracer.process_tree.get_process(pid_u32) {
        if let Some(target) = proc_state.fds.get(fd) {
            return target.clone();
        }
    }

    // Fallback: read from procfs.
    let link = format!("/proc/{pid_u32}/fd/{fd}");
    match std::fs::read_link(&link) {
        Ok(path) => {
            let path_str = path.to_string_lossy();
            if path_str.starts_with("pipe:[") {
                let inode = path_str
                    .trim_start_matches("pipe:[")
                    .trim_end_matches(']')
                    .parse::<u64>()
                    .unwrap_or(0);
                FdTarget::Pipe {
                    inode,
                    direction: PipeEnd::Read,
                }
            } else if path_str == "/dev/null" {
                FdTarget::DevNull
            } else if path_str.starts_with("socket:[") {
                FdTarget::Socket {
                    domain: 0,
                    addr: None,
                }
            } else {
                FdTarget::File { path }
            }
        }
        Err(_) => FdTarget::Unknown,
    }
}
