// Rust guideline compliant 2026-02-21
//! Per-syscall classification handlers for the classify stage.
//!
//! Each handler matches a specific syscall number and returns the
//! corresponding `Classification`. Errors and unrecognized syscalls
//! fall through to `Classification::Passthrough`.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;

use nix::unistd::Pid;
use tracing::event;
use tracing::Level;

use crate::state::fd_table::{FdTarget, PipeEnd};
use crate::pipeline::classified::{Classification, PipeDirection, StdioType};
use crate::pipeline::raw_stop::SyscallArgs;

use super::classify::{ClassifyStage, PendingEntry};
use super::sockaddr::{encode_sockaddr, is_tls_port, parse_sockaddr};

/// Fallback address when the real address cannot be read from tracee memory.
const UNSPECIFIED_ADDR: SocketAddr =
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0));

/// Dispatch a syscall entry stop to the appropriate handler.
pub async fn handle_entry(
    stage: &ClassifyStage,
    pid: Pid,
    nr: u64,
    args: SyscallArgs,
) -> Classification {
    // Syscall numbers are architecture-dependent — use libc constants.
    #[cfg(target_arch = "aarch64")]
    {
        handle_entry_aarch64(stage, pid, nr, args).await
    }
    #[cfg(target_arch = "x86_64")]
    {
        handle_entry_x86_64(stage, pid, nr, args).await
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    {
        Classification::Passthrough
    }
}

// ─── aarch64 ──────────────────────────────────────────────────────────────

#[cfg(target_arch = "aarch64")]
async fn handle_entry_aarch64(
    stage: &ClassifyStage,
    pid: Pid,
    nr: u64,
    args: SyscallArgs,
) -> Classification {
    match nr as i64 {
        libc::SYS_openat => handle_openat(stage, pid, args).await,
        libc::SYS_close => handle_close(stage, pid, args),
        libc::SYS_read => handle_read(stage, pid, args),
        libc::SYS_write => handle_write(stage, pid, args),
        libc::SYS_renameat | libc::SYS_renameat2 => handle_renameat(stage, pid, args).await,
        libc::SYS_unlinkat => handle_unlinkat(stage, pid, args).await,
        libc::SYS_mkdirat => handle_mkdirat(stage, pid, args).await,
        libc::SYS_pipe2 => handle_pipe(stage, pid, args),
        libc::SYS_dup => handle_dup(stage, pid, args, false),
        libc::SYS_dup3 => handle_dup(stage, pid, args, true),
        libc::SYS_socket => handle_socket(stage, pid, args),
        libc::SYS_connect => handle_connect(stage, pid, args).await,
        libc::SYS_accept | libc::SYS_accept4 => handle_accept(stage, pid, args).await,
        libc::SYS_fchmodat => handle_fchmodat(stage, pid, args).await,
        libc::SYS_truncate => handle_truncate(stage, pid, args).await,
        libc::SYS_ftruncate => handle_ftruncate(stage, pid, args),
        libc::SYS_linkat => handle_linkat(stage, pid, args).await,
        libc::SYS_symlinkat => handle_symlinkat(stage, pid, args).await,
        _ => Classification::Passthrough,
    }
}

// ─── x86_64 ───────────────────────────────────────────────────────────────

#[cfg(target_arch = "x86_64")]
async fn handle_entry_x86_64(
    stage: &ClassifyStage,
    pid: Pid,
    nr: u64,
    args: SyscallArgs,
) -> Classification {
    match nr as i64 {
        libc::SYS_openat | libc::SYS_open => handle_openat(stage, pid, args).await,
        libc::SYS_close => handle_close(stage, pid, args),
        libc::SYS_read => handle_read(stage, pid, args),
        libc::SYS_write => handle_write(stage, pid, args),
        libc::SYS_rename => handle_rename_2arg(stage, pid, args).await,
        libc::SYS_renameat | libc::SYS_renameat2 => handle_renameat(stage, pid, args).await,
        libc::SYS_unlink => handle_unlink_1arg(stage, pid, args).await,
        libc::SYS_unlinkat => handle_unlinkat(stage, pid, args).await,
        libc::SYS_mkdir => handle_mkdir_1arg(stage, pid, args).await,
        libc::SYS_mkdirat => handle_mkdirat(stage, pid, args).await,
        libc::SYS_rmdir => handle_rmdir_1arg(stage, pid, args).await,
        libc::SYS_pipe | libc::SYS_pipe2 => handle_pipe(stage, pid, args),
        libc::SYS_dup => handle_dup(stage, pid, args, false),
        libc::SYS_dup2 | libc::SYS_dup3 => handle_dup(stage, pid, args, true),
        libc::SYS_socket => handle_socket(stage, pid, args),
        libc::SYS_connect => handle_connect(stage, pid, args).await,
        libc::SYS_accept | libc::SYS_accept4 => handle_accept(stage, pid, args).await,
        libc::SYS_chmod => handle_chmod_1arg(stage, pid, args).await,
        libc::SYS_fchmodat => handle_fchmodat(stage, pid, args).await,
        libc::SYS_truncate => handle_truncate(stage, pid, args).await,
        libc::SYS_ftruncate => handle_ftruncate(stage, pid, args),
        libc::SYS_link => handle_link_2arg(stage, pid, args).await,
        libc::SYS_linkat => handle_linkat(stage, pid, args).await,
        libc::SYS_symlink => handle_symlink_2arg(stage, pid, args).await,
        libc::SYS_symlinkat => handle_symlinkat(stage, pid, args).await,
        _ => Classification::Passthrough,
    }
}

// ─── Handlers ─────────────────────────────────────────────────────────────

async fn handle_openat(
    stage: &ClassifyStage,
    pid: Pid,
    args: SyscallArgs,
) -> Classification {
    // openat(dirfd, pathname, flags, mode)
    // arg0=dirfd, arg1=pathname_ptr, arg2=flags, arg3=mode
    let path = stage.handle.read_string(pid, args.arg1 as usize, 4096).await;
    let path = match path {
        Ok(p) => PathBuf::from(p),
        Err(e) => {
            event!(name: "classify.openat.error", Level::WARN, error.message = %e, "failed to read path");
            return Classification::Passthrough;
        }
    };
    let flags = args.arg2 as i32;
    let mode = args.arg3 as u32;
    // Defer to exit — we need the return value (new fd) to populate
    // the fd table. The exit handler updates state and returns Passthrough.
    stage.pending.insert(pid, PendingEntry::Openat { path, flags, mode });
    Classification::Passthrough
}

fn handle_close(stage: &ClassifyStage, pid: Pid, args: SyscallArgs) -> Classification {
    let fd = args.arg0 as i32;
    if let Some(mut table) = stage.fd_tables.get_mut(&pid) {
        table.remove(fd);
    }
    Classification::FileClose { fd }
}

fn handle_read(stage: &ClassifyStage, pid: Pid, args: SyscallArgs) -> Classification {
    let fd = args.arg0 as i32;
    let buf_addr = args.arg1 as usize;
    let len = args.arg2 as usize;

    let target = stage.fd_tables.get(&pid).and_then(|t| t.get(fd).cloned());
    match target {
        Some(FdTarget::File { path }) => {
            Classification::FileRead { path, fd, buf_addr, len }
        }
        Some(FdTarget::Pipe { inode, direction: PipeEnd::Read }) => {
            Classification::PipeData { inode, direction: PipeDirection::Read, buf_addr, len }
        }
        _ => {
            let stdio = fd_to_stdio(fd);
            if let Some(subtype) = stdio {
                Classification::Stdio { subtype, pipe_inode: None, buf_addr, len }
            } else {
                Classification::Passthrough
            }
        }
    }
}

fn handle_write(stage: &ClassifyStage, pid: Pid, args: SyscallArgs) -> Classification {
    let fd = args.arg0 as i32;
    let buf_addr = args.arg1 as usize;
    let len = args.arg2 as usize;

    let target = stage.fd_tables.get(&pid).and_then(|t| t.get(fd).cloned());
    match target {
        Some(FdTarget::File { path }) => {
            Classification::FileWrite { path, fd, buf_addr, len }
        }
        Some(FdTarget::Pipe { inode, direction: PipeEnd::Write }) => {
            Classification::PipeData { inode, direction: PipeDirection::Write, buf_addr, len }
        }
        _ => {
            let stdio = fd_to_stdio(fd);
            if let Some(subtype) = stdio {
                Classification::Stdio { subtype, pipe_inode: None, buf_addr, len }
            } else {
                Classification::Passthrough
            }
        }
    }
}

async fn handle_renameat(
    stage: &ClassifyStage,
    pid: Pid,
    args: SyscallArgs,
) -> Classification {
    // renameat(olddirfd, oldpath, newdirfd, newpath)
    let old = stage.handle.read_string(pid, args.arg1 as usize, 4096).await;
    let new = stage.handle.read_string(pid, args.arg3 as usize, 4096).await;
    match (old, new) {
        (Ok(o), Ok(n)) => Classification::FileRename {
            old_path: PathBuf::from(o),
            new_path: PathBuf::from(n),
        },
        _ => Classification::Passthrough,
    }
}

#[cfg(target_arch = "x86_64")]
async fn handle_rename_2arg(
    stage: &ClassifyStage,
    pid: Pid,
    args: SyscallArgs,
) -> Classification {
    let old = stage.handle.read_string(pid, args.arg0 as usize, 4096).await;
    let new = stage.handle.read_string(pid, args.arg1 as usize, 4096).await;
    match (old, new) {
        (Ok(o), Ok(n)) => Classification::FileRename {
            old_path: PathBuf::from(o),
            new_path: PathBuf::from(n),
        },
        _ => Classification::Passthrough,
    }
}

async fn handle_unlinkat(
    stage: &ClassifyStage,
    pid: Pid,
    args: SyscallArgs,
) -> Classification {
    let path = stage.handle.read_string(pid, args.arg1 as usize, 4096).await;
    match path {
        Ok(p) => Classification::FileUnlink { path: PathBuf::from(p) },
        Err(_) => Classification::Passthrough,
    }
}

#[cfg(target_arch = "x86_64")]
async fn handle_unlink_1arg(
    stage: &ClassifyStage,
    pid: Pid,
    args: SyscallArgs,
) -> Classification {
    let path = stage.handle.read_string(pid, args.arg0 as usize, 4096).await;
    match path {
        Ok(p) => Classification::FileUnlink { path: PathBuf::from(p) },
        Err(_) => Classification::Passthrough,
    }
}

async fn handle_mkdirat(
    stage: &ClassifyStage,
    pid: Pid,
    args: SyscallArgs,
) -> Classification {
    let path = stage.handle.read_string(pid, args.arg1 as usize, 4096).await;
    match path {
        Ok(p) => Classification::FileMkdir { path: PathBuf::from(p) },
        Err(_) => Classification::Passthrough,
    }
}

#[cfg(target_arch = "x86_64")]
async fn handle_mkdir_1arg(
    stage: &ClassifyStage,
    pid: Pid,
    args: SyscallArgs,
) -> Classification {
    let path = stage.handle.read_string(pid, args.arg0 as usize, 4096).await;
    match path {
        Ok(p) => Classification::FileMkdir { path: PathBuf::from(p) },
        Err(_) => Classification::Passthrough,
    }
}

#[cfg(target_arch = "x86_64")]
async fn handle_rmdir_1arg(
    stage: &ClassifyStage,
    pid: Pid,
    args: SyscallArgs,
) -> Classification {
    let path = stage.handle.read_string(pid, args.arg0 as usize, 4096).await;
    match path {
        Ok(p) => Classification::FileRmdir { path: PathBuf::from(p) },
        Err(_) => Classification::Passthrough,
    }
}

fn handle_pipe(
    stage: &ClassifyStage,
    pid: Pid,
    args: SyscallArgs,
) -> Classification {
    // pipe2(pipefd, flags) — arg0 is the address of int[2] in tracee memory.
    // Defer to exit — we need the kernel to fill the array before reading.
    let pipe_array_addr = args.arg0 as usize;
    stage.pending.insert(pid, PendingEntry::Pipe { pipe_array_addr });
    Classification::Passthrough
}

fn handle_dup(
    stage: &ClassifyStage,
    pid: Pid,
    args: SyscallArgs,
    has_newfd: bool,
) -> Classification {
    let old_fd = args.arg0 as i32;
    // dup() has no second arg (kernel chooses fd); dup2/dup3 specify new_fd.
    let new_fd = if has_newfd { Some(args.arg1 as i32) } else { None };
    // Defer to exit — we need the return value to confirm success and
    // (for dup) to learn the kernel-chosen fd number.
    stage.pending.insert(pid, PendingEntry::Dup { old_fd, new_fd });
    Classification::Passthrough
}

fn handle_socket(
    stage: &ClassifyStage,
    pid: Pid,
    args: SyscallArgs,
) -> Classification {
    let domain = args.arg0 as i32;
    let sock_type = args.arg1 as i32;
    // Defer to exit — the return value is the new fd number.
    stage.pending.insert(pid, PendingEntry::Socket { domain, sock_type });
    Classification::Passthrough
}

async fn handle_connect(
    stage: &ClassifyStage,
    pid: Pid,
    args: SyscallArgs,
) -> Classification {
    let fd = args.arg0 as i32;
    let sockaddr_addr = args.arg1 as usize;
    let sockaddr_len = args.arg2 as usize;

    let sockaddr_bytes = stage.handle.read_memory(pid, sockaddr_addr, sockaddr_len).await
        .unwrap_or_default();
    let original_dest = parse_sockaddr(&sockaddr_bytes);

    if stage.transparent_mode
        && let Some(addr) = &original_dest
            && is_tls_port(addr) && !addr.ip().is_loopback() {
                let proxy_bytes = encode_sockaddr(stage.proxy_addr);
                let _ = stage.handle.write_memory(pid, sockaddr_addr, proxy_bytes).await;
            }

    let addr = original_dest.unwrap_or_else(|| UNSPECIFIED_ADDR);
    Classification::NetConnect { fd, addr }
}

async fn handle_accept(
    stage: &ClassifyStage,
    pid: Pid,
    args: SyscallArgs,
) -> Classification {
    let fd = args.arg0 as i32;
    let sockaddr_addr = args.arg1 as usize;
    let sockaddr_len_addr = args.arg2 as usize;

    // Read the peer address length first, then the address itself.
    let len = if sockaddr_len_addr != 0 {
        stage.handle.read_memory(pid, sockaddr_len_addr, 4).await
            .ok()
            .and_then(|b| b.try_into().ok().map(u32::from_ne_bytes))
            .unwrap_or(16) as usize
    } else {
        16
    };

    let peer = if sockaddr_addr != 0 {
        stage.handle.read_memory(pid, sockaddr_addr, len).await
            .ok()
            .and_then(|b| parse_sockaddr(&b))
            .unwrap_or_else(|| UNSPECIFIED_ADDR)
    } else {
        UNSPECIFIED_ADDR
    };

    Classification::NetAccept { fd, peer }
}

async fn handle_fchmodat(
    stage: &ClassifyStage,
    pid: Pid,
    args: SyscallArgs,
) -> Classification {
    let path = stage.handle.read_string(pid, args.arg1 as usize, 4096).await;
    let mode = args.arg2 as u32;
    match path {
        Ok(p) => Classification::FileChmod { path: PathBuf::from(p), mode },
        Err(_) => Classification::Passthrough,
    }
}

#[cfg(target_arch = "x86_64")]
async fn handle_chmod_1arg(
    stage: &ClassifyStage,
    pid: Pid,
    args: SyscallArgs,
) -> Classification {
    let path = stage.handle.read_string(pid, args.arg0 as usize, 4096).await;
    let mode = args.arg1 as u32;
    match path {
        Ok(p) => Classification::FileChmod { path: PathBuf::from(p), mode },
        Err(_) => Classification::Passthrough,
    }
}

async fn handle_truncate(
    stage: &ClassifyStage,
    pid: Pid,
    args: SyscallArgs,
) -> Classification {
    let path = stage.handle.read_string(pid, args.arg0 as usize, 4096).await;
    let len = args.arg1;
    match path {
        Ok(p) => Classification::FileTruncate { path: PathBuf::from(p), len },
        Err(_) => Classification::Passthrough,
    }
}

fn handle_ftruncate(stage: &ClassifyStage, pid: Pid, args: SyscallArgs) -> Classification {
    let fd = args.arg0 as i32;
    let len = args.arg1;
    let path = stage.fd_tables.get(&pid)
        .and_then(|t| t.get(fd).cloned())
        .and_then(|target| match target {
            FdTarget::File { path } => Some(path),
            _ => None,
        });
    match path {
        Some(p) => Classification::FileTruncate { path: p, len },
        None => Classification::Passthrough,
    }
}

async fn handle_linkat(
    stage: &ClassifyStage,
    pid: Pid,
    args: SyscallArgs,
) -> Classification {
    // linkat(olddirfd, oldpath, newdirfd, newpath, flags)
    let target = stage.handle.read_string(pid, args.arg1 as usize, 4096).await;
    let link_path = stage.handle.read_string(pid, args.arg3 as usize, 4096).await;
    match (target, link_path) {
        (Ok(t), Ok(l)) => Classification::FileLink {
            target: PathBuf::from(t),
            link_path: PathBuf::from(l),
        },
        _ => Classification::Passthrough,
    }
}

#[cfg(target_arch = "x86_64")]
async fn handle_link_2arg(
    stage: &ClassifyStage,
    pid: Pid,
    args: SyscallArgs,
) -> Classification {
    let target = stage.handle.read_string(pid, args.arg0 as usize, 4096).await;
    let link_path = stage.handle.read_string(pid, args.arg1 as usize, 4096).await;
    match (target, link_path) {
        (Ok(t), Ok(l)) => Classification::FileLink {
            target: PathBuf::from(t),
            link_path: PathBuf::from(l),
        },
        _ => Classification::Passthrough,
    }
}

async fn handle_symlinkat(
    stage: &ClassifyStage,
    pid: Pid,
    args: SyscallArgs,
) -> Classification {
    // symlinkat(target, newdirfd, linkpath)
    let target = stage.handle.read_string(pid, args.arg0 as usize, 4096).await;
    let link_path = stage.handle.read_string(pid, args.arg2 as usize, 4096).await;
    match (target, link_path) {
        (Ok(t), Ok(l)) => Classification::FileSymlink {
            target: PathBuf::from(t),
            link_path: PathBuf::from(l),
        },
        _ => Classification::Passthrough,
    }
}

#[cfg(target_arch = "x86_64")]
async fn handle_symlink_2arg(
    stage: &ClassifyStage,
    pid: Pid,
    args: SyscallArgs,
) -> Classification {
    let target = stage.handle.read_string(pid, args.arg0 as usize, 4096).await;
    let link_path = stage.handle.read_string(pid, args.arg1 as usize, 4096).await;
    match (target, link_path) {
        (Ok(t), Ok(l)) => Classification::FileSymlink {
            target: PathBuf::from(t),
            link_path: PathBuf::from(l),
        },
        _ => Classification::Passthrough,
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────

/// Map stdin/stdout/stderr fd numbers to `StdioType`.
fn fd_to_stdio(fd: i32) -> Option<StdioType> {
    match fd {
        0 => Some(StdioType::Stdin),
        1 => Some(StdioType::Stdout),
        2 => Some(StdioType::Stderr),
        _ => None,
    }
}
