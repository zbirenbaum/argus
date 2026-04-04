// Rust guideline compliant 2026-02-21
//! Tracee memory access helpers for reading data from traced processes.
//!
//! Uses `process_vm_readv` for efficient cross-process memory reads,
//! falling back to `ptrace::read` for small reads when needed.

use std::io::{IoSlice, IoSliceMut};

use anyhow::{Context, Result, bail};
use nix::sys::uio::{RemoteIoVec, process_vm_readv, process_vm_writev};
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

/// Writes bytes into tracee memory via `process_vm_writev`.
///
/// Used by transparent proxy mode to overwrite a `sockaddr` in the
/// tracee's address space before the kernel processes `connect()`.
///
/// # Errors
///
/// Returns an error if the syscall fails (invalid address, dead process).
pub fn write_bytes(pid: Pid, addr: u64, data: &[u8]) -> Result<()> {
    if data.is_empty() {
        return Ok(());
    }

    let remote = RemoteIoVec {
        base: addr as usize,
        len: data.len(),
    };

    process_vm_writev(
        pid,
        &[IoSlice::new(data)],
        &[remote],
    )
    .with_context(|| format!("process_vm_writev failed for pid {pid} at 0x{addr:x}"))?;

    Ok(())
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
    fn write_bytes_zero_length_is_noop() {
        let result = write_bytes(Pid::from_raw(1), 0x1000, &[]);
        assert!(result.is_ok());
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
}
