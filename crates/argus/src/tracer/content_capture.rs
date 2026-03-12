//! Captures written data from tracee memory and stores it in CAS.
//!
//! Called by syscall handlers on write/read operations. Reads the
//! buffer from tracee address space via `process_vm_readv`, hashes
//! the content, and stores it in the content-addressable store.

use anyhow::{Result, bail};
use nix::unistd::Pid;
use tracing::{Level, event};

use crate::cas::{Cas, LocalCas, ContentHash};

use super::memory;

/// Largest single buffer we will read from a tracee in one call.
///
/// 16 MiB balances memory pressure against the overhead of multiple
/// `process_vm_readv` calls for very large writes.
const MAX_SINGLE_READ: usize = 16 * 1024 * 1024;

/// Reads a write buffer from tracee memory and stores it in CAS.
///
/// Returns the content hash if the buffer was successfully read and
/// stored. Returns `None` for zero-length writes or null addresses.
///
/// # Errors
///
/// Returns an error if reading tracee memory or CAS storage fails.
pub fn capture_write_buffer(
    cas: &LocalCas,
    pid: Pid,
    buf_addr: u64,
    len: u64,
) -> Result<Option<ContentHash>> {
    if len == 0 || buf_addr == 0 {
        return Ok(None);
    }

    let data = read_tracee_buffer(pid, buf_addr, len)?;
    let hash = cas.put(&data)?;

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
    cas: &LocalCas,
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

    let hash = cas.put(&data)?;

    event!(
        name: "content.capture.iovec",
        Level::TRACE,
        pid = pid.as_raw(),
        content.hash = hash.as_str(),
        content.size = data.len(),
        iov_cnt = iov_cnt,
        "captured iovec buffer: {{content.size}} bytes -> {{content.hash}}",
    );

    Ok(Some(hash))
}

/// Captures a buffer and returns the hash as a `String`, or `None`.
///
/// Convenience wrapper that logs failures as warnings rather than
/// propagating errors. Syscall handling should not abort on capture
/// failure since the tracee must always be resumed.
pub fn try_capture_flat(
    cas: &LocalCas,
    pid: Pid,
    buf_addr: u64,
    len: u64,
) -> Option<String> {
    match capture_write_buffer(cas, pid, buf_addr, len) {
        Ok(Some(hash)) => Some(hash.to_string()),
        Ok(None) => None,
        Err(e) => {
            event!(
                name: "content.capture.flat.failed",
                Level::WARN,
                pid = pid.as_raw(),
                error.message = %e,
                "flat buffer capture failed for pid {{pid}}: {{error.message}}",
            );
            None
        }
    }
}

/// Captures an iovec buffer and returns the hash as a `String`, or `None`.
///
/// Same error-swallowing behavior as [`try_capture_flat`].
pub fn try_capture_iovec(
    cas: &LocalCas,
    pid: Pid,
    iov_addr: u64,
    iov_cnt: u64,
) -> Option<String> {
    match capture_iovec_buffer(cas, pid, iov_addr, iov_cnt) {
        Ok(Some(hash)) => Some(hash.to_string()),
        Ok(None) => None,
        Err(e) => {
            event!(
                name: "content.capture.iovec.failed",
                Level::WARN,
                pid = pid.as_raw(),
                error.message = %e,
                "iovec capture failed for pid {{pid}}: {{error.message}}",
            );
            None
        }
    }
}

/// Reads a contiguous buffer from tracee memory, chunking large reads.
fn read_tracee_buffer(
    pid: Pid,
    addr: u64,
    len: u64,
) -> Result<Vec<u8>> {
    let total = usize::try_from(len)?;

    if total <= MAX_SINGLE_READ {
        return memory::read_bytes(pid, addr, total);
    }

    let mut result = Vec::with_capacity(total);
    let mut offset: usize = 0;

    while offset < total {
        let chunk_size = (total - offset).min(MAX_SINGLE_READ);
        let chunk_addr = addr + offset as u64;
        let chunk = memory::read_bytes(pid, chunk_addr, chunk_size)?;
        let read_len = chunk.len();
        result.extend_from_slice(&chunk);
        offset += read_len;

        // Short read means the tracee's mapping ended.
        if read_len < chunk_size {
            break;
        }
    }

    Ok(result)
}

/// Size of a `struct iovec` on 64-bit Linux (two pointers: base + len).
const IOVEC_SIZE: usize = 16;

/// Linux `UIO_MAXIOV` — maximum number of iovec entries per call.
const MAX_IOV_COUNT: u64 = 1024;

/// Reads a scatter/gather iovec array from tracee memory.
///
/// Reads the iovec struct array first, then reads each individual
/// buffer and concatenates them.
fn read_tracee_iovec(
    pid: Pid,
    iov_addr: u64,
    iov_cnt: u64,
) -> Result<Vec<u8>> {
    if iov_cnt > MAX_IOV_COUNT {
        bail!(
            "iov_cnt {iov_cnt} exceeds Linux UIO_MAXIOV ({MAX_IOV_COUNT})"
        );
    }
    let cnt = iov_cnt as usize;
    let iov_bytes = memory::read_bytes(
        pid,
        iov_addr,
        cnt * IOVEC_SIZE,
    )?;

    let mut result = Vec::new();

    for i in 0..cnt {
        let base_offset = i * IOVEC_SIZE;

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
#[path = "content_capture_tests.rs"]
mod tests;
