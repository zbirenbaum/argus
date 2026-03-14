// Rust guideline compliant 2026-02-21
//! Content capture stage: reads tracee memory and hashes file content.
//!
//! For file writes the capture stage reads the pre-write file state (before
//! hash) and the write buffer content (after hash). For reads and stdio it
//! reads the buf_addr memory directly. Large blobs are split into chunks.
//!
//! Bytes up to `max_inline_bytes` are cloned into the `data` field of the
//! returned [`CapturedContent`] so downstream enrichment stages can embed
//! them directly into event records without a CAS round-trip.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use dashmap::DashMap;
use nix::unistd::Pid;
use tokio::sync::Mutex;
use tracing::event;
use tracing::Level;
use crate::cas::ContentHash;
use crate::pipeline::capture_policy::{CaptureLevel, CapturePolicy};
use crate::pipeline::captured::{CapturedContent, CapturedEvent};
use crate::pipeline::classified::{ClassifiedEvent, Classification};
use crate::pipeline::durability::DurabilityLayer;
use crate::pipeline::ptrace_thread::PtraceHandle;

/// Minimum blob size (bytes) before chunked emission is used.
///
/// Blobs under this threshold are stored as a single Content record.
/// Larger blobs are split into CHUNK_SIZE pieces with a Manifest.
const CHUNK_THRESHOLD: usize = 256 * 1024;

/// Maximum bytes per chunk in a chunked upload.
const CHUNK_SIZE: usize = 4 * 1024 * 1024;

/// Stage that captures content from traced syscalls.
pub struct CaptureStage {
    handle: PtraceHandle,
    durability: DurabilityLayer,
    policy: CapturePolicy,
    /// Per-path mutex that serializes concurrent writes to the same file.
    ///
    /// Wrapped in `Arc` so we can clone the handle and release the DashMap
    /// shard lock before awaiting the inner mutex (avoids holding a shard
    /// lock across an async suspension point).
    write_locks: DashMap<PathBuf, Arc<Mutex<()>>>,
    /// Tracked file content hashes shared with `ClassifyStage`.
    ///
    /// Used instead of filesystem reads for `before_hash` to avoid
    /// races between resuming a previous write and reading the file
    /// for the next write's `before_hash`.
    file_state: Arc<DashMap<PathBuf, ContentHash>>,
    /// Maximum bytes to retain as inline data in [`CapturedContent`].
    ///
    /// Data larger than this cap is still hashed and stored in CAS but
    /// the `data` field is set to `None` to keep event records small.
    max_inline_bytes: usize,
}

impl CaptureStage {
    /// Create a new capture stage.
    pub fn new(
        handle: PtraceHandle,
        durability: DurabilityLayer,
        policy: CapturePolicy,
        file_state: Arc<DashMap<PathBuf, ContentHash>>,
        max_inline_bytes: usize,
    ) -> Self {
        Self {
            handle,
            durability,
            policy,
            write_locks: DashMap::new(),
            file_state,
            max_inline_bytes,
        }
    }

    /// Capture content for a classified event and return the enriched result.
    pub async fn capture(&self, event: ClassifiedEvent) -> CapturedEvent {
        let pid = event.pid;
        let cls_name = event.syscall_name();
        event!(
            name: "pipeline.capture.start",
            Level::DEBUG,
            pid = pid.as_raw(),
            classification = cls_name.as_str(),
            "starting content capture",
        );
        let content = match &event.classification {
            Classification::FileWrite { path, buf_addr, len, .. } => {
                self.capture_write(pid, path, *buf_addr, *len).await
            }
            Classification::FileRead { path, buf_addr, len, .. } => {
                self.capture_read(pid, path, *buf_addr, *len).await
            }
            Classification::FileUnlink { path } => {
                self.capture_delete(path).await
            }
            Classification::FileTruncate { path, len } => {
                self.capture_truncate(path, *len).await
            }
            Classification::Stdio { buf_addr, len, .. }
            | Classification::PipeData { buf_addr, len, .. }
            | Classification::PtyData { buf_addr, len, .. } => {
                self.capture_stream(pid, *buf_addr, *len).await
            }
            _ => CapturedContent::None,
        };

        CapturedEvent { pid, classification: event.classification, content }
    }

    async fn capture_write(
        &self,
        pid: Pid,
        path: &Path,
        buf_addr: usize,
        len: usize,
    ) -> CapturedContent {
        let level = self.policy.level(path, pid.as_raw() as u32, len);
        event!(
            name: "pipeline.capture.write",
            Level::DEBUG,
            pid = pid.as_raw(),
            path = %path.display(),
            len,
            "capturing write content",
        );
        if level == CaptureLevel::Ignore {
            return CapturedContent::None;
        }

        // Acquire per-path lock to serialize concurrent writes for hash
        // chain correctness. Clone the Arc to release the DashMap shard lock
        // before awaiting — holding a shard lock across .await is a deadlock risk.
        let lock = self.write_locks
            .entry(path.to_path_buf())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _guard = lock.lock().await;

        // Use tracked in-memory hash instead of racy filesystem read.
        let before_hash = self.file_state.get(path).map(|h| *h);

        let (after_hash, inline_data) = if level == CaptureLevel::Full {
            match self.handle.read_memory(pid, buf_addr, len).await {
                Ok(d) => {
                    self.policy.record_bytes(pid.as_raw() as u32, d.len());
                    let inline = inline_slice(&d, self.max_inline_bytes);
                    let hash = emit_content(&self.durability, d);
                    (Some(hash), inline)
                }
                Err(_) => (None, None),
            }
        } else {
            (None, None)
        };

        // Update tracked state so the next write sees this write's hash.
        if let Some(hash) = after_hash {
            self.file_state.insert(path.to_path_buf(), hash);
        }

        CapturedContent::FileWrite { before_hash, after_hash, data: inline_data, size: len }
    }

    async fn capture_read(
        &self,
        pid: Pid,
        path: &Path,
        buf_addr: usize,
        len: usize,
    ) -> CapturedContent {
        let level = self.policy.level(path, pid.as_raw() as u32, len);
        if level == CaptureLevel::Ignore {
            return CapturedContent::None;
        }
        if level == CaptureLevel::MetadataOnly {
            return CapturedContent::FileRead { content_hash: None, data: None, size: len };
        }

        let (content_hash, inline_data) = match self.handle.read_memory(pid, buf_addr, len).await {
            Ok(d) => {
                self.policy.record_bytes(pid.as_raw() as u32, d.len());
                let inline = inline_slice(&d, self.max_inline_bytes);
                let hash = emit_content(&self.durability, d);
                (Some(hash), inline)
            }
            Err(_) => (None, None),
        };

        CapturedContent::FileRead { content_hash, data: inline_data, size: len }
    }

    async fn capture_delete(&self, path: &Path) -> CapturedContent {
        let (content_hash, inline_data) = match self.handle.read_file(path.to_path_buf()).await {
            Ok(d) => {
                let inline = inline_slice(&d, self.max_inline_bytes);
                let hash = emit_content(&self.durability, d);
                (Some(hash), inline)
            }
            Err(_) => (None, None),
        };
        CapturedContent::FileDelete { content_hash, data: inline_data }
    }

    async fn capture_truncate(&self, path: &Path, new_len: u64) -> CapturedContent {
        // Read before state from disk once. The syscall has not yet executed,
        // so the file still holds its pre-truncation content. Derive the
        // after-truncation state by slicing the same buffer.
        let before_bytes = match self.handle.read_file(path.to_path_buf()).await {
            Ok(d) => d,
            Err(_) => {
                return CapturedContent::FileTruncate {
                    before_hash: None,
                    after_hash: None,
                    before_data: None,
                    after_data: None,
                };
            }
        };

        let before_inline = inline_slice(&before_bytes, self.max_inline_bytes);
        let before_hash = emit_content(&self.durability, before_bytes.clone());

        let truncated_len = (new_len as usize).min(before_bytes.len());
        let truncated = &before_bytes[..truncated_len];
        let after_inline = inline_slice(truncated, self.max_inline_bytes);
        let after_hash = emit_content(&self.durability, truncated.to_vec());

        CapturedContent::FileTruncate {
            before_hash: Some(before_hash),
            after_hash: Some(after_hash),
            before_data: before_inline,
            after_data: after_inline,
        }
    }

    /// Stream-compatible capture with internal tracee resume.
    ///
    /// Captures content then resumes the tracee so downstream stages
    /// do not need access to the ptrace handle.
    pub async fn process(&self, event: ClassifiedEvent) -> CapturedEvent {
        let captured = self.capture(event).await;
        self.handle.resume(captured.pid, false, None);
        captured
    }

    async fn capture_stream(
        &self,
        pid: Pid,
        buf_addr: usize,
        len: usize,
    ) -> CapturedContent {
        let (content_hash, inline_data) = match self.handle.read_memory(pid, buf_addr, len).await {
            Ok(d) => {
                self.policy.record_bytes(pid.as_raw() as u32, d.len());
                let inline = inline_slice(&d, self.max_inline_bytes);
                let hash = emit_content(&self.durability, d);
                (Some(hash), inline)
            }
            Err(_) => (None, None),
        };
        CapturedContent::StreamData { content_hash, data: inline_data, size: len }
    }
}

/// Return `Some(data.clone())` when `data.len() <= cap`, otherwise `None`.
///
/// Callers set `cap` from [`CaptureStage::max_inline_bytes`].
fn inline_slice(data: &[u8], cap: usize) -> Option<Vec<u8>> {
    if data.len() <= cap { Some(data.to_vec()) } else { None }
}

/// Persist one or more content blobs via DurabilityLayer, returning the root hash.
///
/// Data under `CHUNK_THRESHOLD` is stored as a single object. Larger data is
/// split into `CHUNK_SIZE` pieces persisted individually, with a manifest object
/// whose hash is computed from the concatenated chunk hash strings. The manifest
/// hash is returned so callers always have a single stable content identifier.
fn emit_content(durability: &DurabilityLayer, data: Vec<u8>) -> ContentHash {
    if data.len() < CHUNK_THRESHOLD {
        let hash = ContentHash::from_data(&data);
        let _ = durability.persist_with_hash(hash.clone(), &data);
        durability.upload_async(hash.clone(), data);
        return hash;
    }

    let mut chunk_hashes = Vec::new();
    for chunk in data.chunks(CHUNK_SIZE) {
        let ch = ContentHash::from_data(chunk);
        let _ = durability.persist_with_hash(ch.clone(), chunk);
        durability.upload_async(ch.clone(), chunk.to_vec());
        chunk_hashes.push(ch);
    }

    // Manifest hash is derived from the concatenated chunk hash strings so it
    // is deterministic and content-addressable independent of upload order.
    let manifest_input = {
        use std::fmt::Write as _;
        let mut buf = String::new();
        for (i, h) in chunk_hashes.iter().enumerate() {
            if i > 0 { buf.push('\n'); }
            let _ = write!(buf, "{h}");
        }
        buf
    };
    let manifest_hash = ContentHash::from_data(manifest_input.as_bytes());
    let manifest_data = serde_json::to_vec(&chunk_hashes).unwrap_or_default();
    let _ = durability.persist_with_hash(manifest_hash.clone(), &manifest_data);
    durability.upload_async(manifest_hash.clone(), manifest_data);
    manifest_hash
}

#[cfg(test)]
mod tests {
    use super::inline_slice;

    #[test]
    fn inline_slice_under_cap_returns_some() {
        let data = vec![1u8, 2, 3, 4, 5];
        let result = inline_slice(&data, 10);
        assert_eq!(result, Some(data));
    }

    #[test]
    fn inline_slice_exactly_at_cap_returns_some() {
        let data = vec![0u8; 16];
        let result = inline_slice(&data, 16);
        assert_eq!(result, Some(data));
    }

    #[test]
    fn inline_slice_over_cap_returns_none() {
        let data = vec![0u8; 17];
        let result = inline_slice(&data, 16);
        assert_eq!(result, None);
    }

    #[test]
    fn inline_slice_empty_data_returns_some() {
        let result = inline_slice(&[], 0);
        assert_eq!(result, Some(vec![]));
    }

    #[test]
    fn inline_slice_zero_cap_with_data_returns_none() {
        let data = vec![1u8];
        let result = inline_slice(&data, 0);
        assert_eq!(result, None);
    }
}
