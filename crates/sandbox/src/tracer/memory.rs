// Rust guideline compliant 2026-02-21
//! Tracee memory access helpers for reading data from traced processes.
//!
//! Uses `process_vm_readv` for efficient cross-process memory reads,
//! falling back to `ptrace::read` for small reads when needed.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use nix::unistd::Pid;

/// Maximum path length we will read from a tracee.
const MAX_PATH_LEN: usize = 4096;

/// Reads a null-terminated C string from tracee memory.
///
/// Uses `process_vm_readv` to read up to `MAX_PATH_LEN` bytes, then
/// truncates at the first null byte.
///
/// # Errors
///
/// Returns an error if the memory read fails (invalid address, dead process).
pub fn read_c_string(pid: Pid, addr: u64) -> Result<String> {
    if addr == 0 {
        bail!("null pointer passed to read_c_string");
    }
    let bytes = read_bytes(pid, addr, MAX_PATH_LEN)?;
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned().pipe_ok()
}

/// Reads arbitrary bytes from tracee memory via `process_vm_readv`.
///
/// # Errors
///
/// Returns an error if the syscall fails.
pub fn read_bytes(pid: Pid, addr: u64, len: usize) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; len];
    let local_iov = libc::iovec {
        iov_base: buf.as_mut_ptr().cast(),
        iov_len: len,
    };
    let remote_iov = libc::iovec {
        iov_base: addr as *mut libc::c_void,
        iov_len: len,
    };

    // SAFETY: `local_iov` points to `buf` which is valid for `len` bytes,
    // `remote_iov` references tracee address space, and `pid` identifies
    // a process we are tracing via ptrace.
    let n = unsafe {
        libc::process_vm_readv(
            pid.as_raw(),
            &local_iov,
            1,
            &remote_iov,
            1,
            0,
        )
    };
    if n < 0 {
        return Err(std::io::Error::last_os_error())
            .context(format!("process_vm_readv failed for pid {pid} at 0x{addr:x}"));
    }

    buf.truncate(n as usize);
    Ok(buf)
}

/// Resolves a path for `*at()` syscalls, handling `AT_FDCWD`.
///
/// If `dirfd` is `AT_FDCWD`, the path is resolved relative to the
/// process cwd (read from `/proc/{pid}/cwd`). If the path is absolute,
/// `dirfd` is ignored per kernel semantics.
///
/// # Errors
///
/// Returns an error if the path cannot be read from tracee memory.
pub fn read_path_at(pid: Pid, dirfd: i32, path_addr: u64) -> Result<PathBuf> {
    let raw_path = read_c_string(pid, path_addr)?;
    let path = Path::new(&raw_path);

    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }

    if dirfd == libc::AT_FDCWD {
        let cwd = read_proc_cwd(pid)?;
        return Ok(cwd.join(path));
    }

    // dirfd points to an open directory. Read from /proc/{pid}/fd/{dirfd}.
    let link = format!("/proc/{}/fd/{}", pid.as_raw(), dirfd);
    let dir = std::fs::read_link(&link)
        .with_context(|| format!("readlink {link}"))?;
    Ok(dir.join(path))
}

/// Reads `/proc/{pid}/cwd` to determine the current working directory.
fn read_proc_cwd(pid: Pid) -> Result<PathBuf> {
    let link = format!("/proc/{}/cwd", pid.as_raw());
    std::fs::read_link(&link)
        .with_context(|| format!("readlink {link}"))
}

/// Reads `/proc/{pid}/exe` to determine the executable path.
///
/// # Errors
///
/// Returns an error if `/proc/{pid}/exe` cannot be read.
pub fn read_proc_exe(pid: Pid) -> Result<PathBuf> {
    let link = format!("/proc/{}/exe", pid.as_raw());
    std::fs::read_link(&link)
        .with_context(|| format!("readlink {link}"))
}

/// Reads `/proc/{pid}/cmdline` to get argv.
///
/// # Errors
///
/// Returns an error if the file cannot be read.
pub fn read_proc_cmdline(pid: Pid) -> Result<Vec<String>> {
    let path = format!("/proc/{}/cmdline", pid.as_raw());
    let data = std::fs::read(&path)
        .with_context(|| format!("read {path}"))?;
    let args: Vec<String> = data
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| OsStr::from_bytes(s).to_string_lossy().into_owned())
        .collect();
    Ok(args)
}

/// Helper trait to convert a value into `Result::Ok`.
trait PipeOk: Sized {
    fn pipe_ok(self) -> Result<Self> {
        Ok(self)
    }
}

impl<T> PipeOk for T {}
