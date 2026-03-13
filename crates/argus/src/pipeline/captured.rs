// Rust guideline compliant 2026-02-21
//! Captured event after content has been read and hashed.

use nix::unistd::Pid;

use crate::cas::ContentHash;

use super::classified::Classification;

/// Content captured by the capture stage for a single event.
#[derive(Debug, Clone)]
pub enum CapturedContent {
    /// No content capture was warranted (passthrough, metadata-only, etc.).
    None,

    /// A write captured the file content before and after.
    FileWrite {
        /// Hash of the file content before the syscall executed.
        before_hash: Option<ContentHash>,
        /// Hash of the data being written (from tracee memory buffer).
        after_hash: Option<ContentHash>,
        size: usize,
    },

    /// A read captured the file content.
    FileRead {
        content_hash: Option<ContentHash>,
        size: usize,
    },

    /// Bytes flowing through stdio, pipe, or PTY.
    StreamData {
        content_hash: Option<ContentHash>,
        size: usize,
    },

    /// File deleted — pre-deletion hash recorded.
    FileDelete {
        content_hash: Option<ContentHash>,
    },
}

/// A classified event enriched with captured content hashes.
#[derive(Debug)]
pub struct CapturedEvent {
    pub pid: Pid,
    pub classification: Classification,
    pub content: CapturedContent,
}
