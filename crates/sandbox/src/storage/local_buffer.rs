//! Bounded LRU cache for local CAS objects and event segments.
//!
//! [`LocalBuffer`] tracks files written to the local data directory
//! and evicts the oldest upload-confirmed entries when total size
//! exceeds the configured limit.

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use tracing::event;

/// Entry tracked by the local buffer for eviction decisions.
#[derive(Debug, Clone)]
pub struct BufferEntry {
    /// Absolute path to the local file.
    pub path: PathBuf,
    /// Size of the file in bytes.
    pub size: u64,
    /// When the file was first tracked.
    pub created: Instant,
    /// Set to `true` once S3 upload is confirmed.
    pub upload_confirmed: bool,
}

/// Bounded LRU cache managing local data directory space.
///
/// Tracks CAS objects and event segments on disk. Evicts only
/// upload-confirmed entries, oldest first, when total tracked
/// size exceeds `max_bytes`.
#[derive(Debug)]
pub struct LocalBuffer {
    entries: VecDeque<BufferEntry>,
    total_bytes: u64,
    max_bytes: u64,
}

impl LocalBuffer {
    /// Create a buffer with the given capacity limit.
    pub fn new(max_bytes: u64) -> Self {
        Self {
            entries: VecDeque::new(),
            total_bytes: 0,
            max_bytes,
        }
    }

    /// Register a file with the buffer for tracking.
    pub fn track(&mut self, path: PathBuf, size: u64) {
        self.total_bytes += size;
        self.entries.push_back(BufferEntry {
            path,
            size,
            created: Instant::now(),
            upload_confirmed: false,
        });
    }

    /// Mark all entries matching `key_suffix` as upload-confirmed.
    ///
    /// The `key_suffix` is matched against the file name component
    /// of each entry's path.
    pub fn confirm_upload(&mut self, key_suffix: &str) {
        for entry in &mut self.entries {
            let matches = entry
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|name| key_suffix.ends_with(name));
            if matches {
                entry.upload_confirmed = true;
            }
        }
    }

    /// Evict oldest confirmed entries until total size is under limit.
    ///
    /// Returns the number of files deleted.
    ///
    /// # Errors
    ///
    /// Returns an error if a file deletion fails. Partially-completed
    /// evictions are not rolled back.
    pub fn prune(&mut self) -> Result<usize> {
        if self.total_bytes <= self.max_bytes {
            return Ok(0);
        }

        let mut evicted = 0;
        let mut kept = VecDeque::with_capacity(self.entries.len());

        while let Some(entry) = self.entries.pop_front() {
            if entry.upload_confirmed
                && self.total_bytes > self.max_bytes
            {
                delete_file(&entry.path)?;
                self.total_bytes -= entry.size;
                evicted += 1;

                event!(
                    name: "local_buffer.evict",
                    tracing::Level::DEBUG,
                    file.path = %entry.path.display(),
                    file.size = entry.size,
                    "evicted confirmed file {{file.path}} ({{file.size}} bytes)"
                );
            } else {
                kept.push_back(entry);
            }
        }

        self.entries = kept;
        Ok(evicted)
    }

    /// Total bytes currently tracked.
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// Number of entries currently tracked.
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Maximum allowed bytes before eviction.
    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }
}

/// Delete a file, ignoring "not found" (already cleaned up).
fn delete_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| {
            format!("failed to delete {}", path.display())
        }),
    }
}

#[cfg(test)]
#[path = "local_buffer_tests.rs"]
mod tests;

// Rust guideline compliant 2026-02-21
