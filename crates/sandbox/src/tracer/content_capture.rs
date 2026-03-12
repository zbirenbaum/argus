// Rust guideline compliant 2026-02-21
//! Captures written data from tracee memory and stores it in CAS.
//!
//! Called by syscall handlers on write/read operations. Reads the
//! buffer from tracee address space via `process_vm_readv`, hashes
//! the content, and stores it in the content-addressable store.
//! Uses the digest cache to skip re-hashing content already known
//! to exist in remote storage.

use std::sync::Arc;

use anyhow::Result;
use nix::unistd::Pid;
use tracing::event;
use tracing::Level;

use crate::cas::{CasStore, ContentHash};

use super::memory;

/// Largest single buffer we will read from a tracee in one call.
///
/// Buffers exceeding this are read in chunks to avoid excessive
/// memory allocation. 16 MiB balances memory pressure against the
/// overhead of multiple `process_vm_readv` calls.
const MAX_SINGLE_READ: usize = 16 * 1024 * 1024;

/// Reads a write buffer from tracee memory and stores it in CAS.
///
/// Returns the content hash if the buffer was successfully read and
/// stored. Returns `None` for zero-length writes (no content to hash).
///
/// # Errors
///
/// Returns an error if reading tracee memory or CAS storage fails.
pub fn capture_write_buffer(
    cas: &Arc<CasStore>,
    pid: Pid,
    buf_addr: u64,
    len: u64,
) -> Result<Option<ContentHash>> {
    if len == 0 || buf_addr == 0 {
        return Ok(None);
    }

    let data = read_tracee_buffer(pid, buf_addr, len)?;
    let hash = cas.store(&data)?;

    event!(
        name: "content.capture.write",
        Level::TRACE,
        pid = pid.as_raw(),
        content.hash = hash.as_str(),
        content.size = data.len(),
        "captured write buffer: {{content.size}} bytes -> {{content.hash}}",
    );

    Ok(Some(hash))
}

/// Reads an iovec buffer array from tracee memory and stores in CAS.
///
/// For `writev`/`readv` syscalls, the buffer is scattered across
/// multiple iovec entries. This reads all entries, concatenates them,
/// and stores the combined content.
///
/// # Errors
///
/// Returns an error if reading tracee memory or CAS storage fails.
pub fn capture_iovec_buffer(
    cas: &Arc<CasStore>,
    pid: Pid,
    iov_addr: u64,
    iov_cnt: u64,
) -> Result<Option<ContentHash>> {
    if iov_cnt == 0 || iov_addr == 0 {
        return Ok(None);
    }

    let data = read_tracee_iovec(pid, iov_addr, iov_cnt)?;
    if data.is_empty() {
        return Ok(None);
    }

    let hash = cas.store(&data)?;

    event!(
        name: "content.capture.iovec",
        Level::TRACE,
        pid = pid.as_raw(),
        content.hash = hash.as_str(),
        content.size = data.len(),
        iov_cnt = iov_cnt,
        "captured iovec buffer: {{content.size}} bytes ({{iov_cnt}} entries) -> {{content.hash}}",
    );

    Ok(Some(hash))
}

/// Reads a contiguous buffer from tracee memory, chunking large reads.
fn read_tracee_buffer(pid: Pid, addr: u64, len: u64) -> Result<Vec<u8>> {
    let total = len as usize;

    if total <= MAX_SINGLE_READ {
        return memory::read_bytes(pid, addr, total);
    }

    let mut result = Vec::with_capacity(total);
    let mut offset: usize = 0;

    while offset < total {
        let chunk_size = (total - offset).min(MAX_SINGLE_READ);
        let chunk_addr = addr + offset as u64;
        let chunk = memory::read_bytes(pid, chunk_addr, chunk_size)?;
        result.extend_from_slice(&chunk);
        offset += chunk.len();

        // Short read means the tracee's mapping ended.
        if chunk.len() < chunk_size {
            break;
        }
    }

    Ok(result)
}

/// Size of a `struct iovec` on x86_64/aarch64 Linux (two pointers).
const IOVEC_SIZE: usize = std::mem::size_of::<libc::iovec>();

/// Reads a scatter/gather iovec array from tracee memory.
///
/// Reads the iovec struct array first, then reads each individual
/// buffer and concatenates them.
fn read_tracee_iovec(
    pid: Pid,
    iov_addr: u64,
    iov_cnt: u64,
) -> Result<Vec<u8>> {
    let cnt = iov_cnt as usize;
    let iov_bytes = memory::read_bytes(pid, iov_addr, cnt * IOVEC_SIZE)?;

    let mut result = Vec::new();

    for i in 0..cnt {
        let base_offset = i * IOVEC_SIZE;

        // Bounds check: ensure we have enough bytes for this entry.
        if base_offset + IOVEC_SIZE > iov_bytes.len() {
            break;
        }

        let iov_base = u64::from_ne_bytes(
            iov_bytes[base_offset..base_offset + 8]
                .try_into()
                .expect("slice is exactly 8 bytes"),
        );
        let iov_len = u64::from_ne_bytes(
            iov_bytes[base_offset + 8..base_offset + 16]
                .try_into()
                .expect("slice is exactly 8 bytes"),
        );

        if iov_len == 0 || iov_base == 0 {
            continue;
        }

        let chunk = read_tracee_buffer(pid, iov_base, iov_len)?;
        result.extend_from_slice(&chunk);
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_length_write_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(
            CasStore::new(dir.path().join("cas")).expect("CasStore::new"),
        );
        let result =
            capture_write_buffer(&store, Pid::from_raw(1), 0x1000, 0);
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn null_addr_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(
            CasStore::new(dir.path().join("cas")).expect("CasStore::new"),
        );
        let result = capture_write_buffer(&store, Pid::from_raw(1), 0, 100);
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn zero_iovec_count_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(
            CasStore::new(dir.path().join("cas")).expect("CasStore::new"),
        );
        let result =
            capture_iovec_buffer(&store, Pid::from_raw(1), 0x1000, 0);
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn null_iovec_addr_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(
            CasStore::new(dir.path().join("cas")).expect("CasStore::new"),
        );
        let result = capture_iovec_buffer(&store, Pid::from_raw(1), 0, 5);
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn max_single_read_constant_is_reasonable() {
        assert_eq!(MAX_SINGLE_READ, 16 * 1024 * 1024);
    }

    #[test]
    fn iovec_size_matches_platform() {
        assert_eq!(IOVEC_SIZE, 16);
    }
}
