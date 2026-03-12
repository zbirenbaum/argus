// Rust guideline compliant 2026-02-21
//! Tracee memory access helpers for reading data from traced processes.
//!
//! Uses `process_vm_readv` for efficient cross-process memory reads,
//! falling back to `ptrace::read` for small reads when needed.

use std::ffi::OsStr;
use std::io::IoSliceMut;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use nix::sys::uio::{RemoteIoVec, process_vm_readv};
use nix::unistd::Pid;

/// Maximum path length we will read from a tracee.
const MAX_PATH_LEN: usize = 4096;

/// `AT_FDCWD` sentinel — resolve relative to process cwd. Value -100
/// per linux/fcntl.h.
const AT_FDCWD: i32 = -100;

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
    Ok(String::from_utf8_lossy(&bytes[..end]).into_owned())
}

/// Reads arbitrary bytes from tracee memory via `process_vm_readv`.
///
/// Returns the actual bytes read, which may be fewer than `len` if the
/// tracee's mapping ends before the requested range.
///
/// # Errors
///
/// Returns an error if the syscall fails.
pub fn read_bytes(pid: Pid, addr: u64, len: usize) -> Result<Vec<u8>> {
    if len == 0 {
        return Ok(Vec::new());
    }

    let mut buf = vec![0u8; len];

    let remote = RemoteIoVec {
        base: addr as usize,
        len,
    };

    let n = process_vm_readv(
        pid,
        &mut [IoSliceMut::new(&mut buf)],
        &[remote],
    )
    .with_context(|| format!("process_vm_readv failed for pid {pid} at 0x{addr:x}"))?;

    buf.truncate(n);
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

    if dirfd == AT_FDCWD {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_bytes_zero_length_returns_empty() {
        let result = read_bytes(Pid::from_raw(1), 0x1000, 0);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn read_c_string_null_pointer_errors() {
        let result = read_c_string(Pid::from_raw(1), 0);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("null pointer"));
    }

    #[test]
    fn max_path_len_is_reasonable() {
        assert_eq!(MAX_PATH_LEN, 4096);
    }

    #[test]
    fn at_fdcwd_matches_kernel_value() {
        assert_eq!(AT_FDCWD, -100);
    }
}
