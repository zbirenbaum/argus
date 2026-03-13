// Rust guideline compliant 2026-02-21
//! Content capture stage: reads tracee memory and hashes file content.
//!
//! For file writes the capture stage reads the pre-write file state (before
//! hash) and the write buffer content (after hash). For reads and stdio it
//! reads the buf_addr memory directly. Large blobs are split into chunks.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use dashmap::DashMap;
use nix::unistd::Pid;
use tokio::sync::Mutex;
use crate::cas::ContentHash;
use crate::pipeline::bus::RecordBus;
use crate::pipeline::capture_policy::{CaptureLevel, CapturePolicy};
use crate::pipeline::captured::{CapturedContent, CapturedEvent};
use crate::pipeline::classified::{ClassifiedEvent, Classification};
use crate::pipeline::ptrace_thread::PtraceHandle;
use crate::pipeline::record::Record;

/// Minimum blob size (bytes) before chunked emission is used.
///
/// Blobs under this threshold are stored as a single Content record.
/// Larger blobs are split into CHUNK_SIZE pieces with a Manifest.
const CHUNK_THRESHOLD: usize = 256 * 1024;

/// Maximum bytes per chunk in a chunked upload.
const CHUNK_SIZE: usize = 4 * 1024 * 1024;

/// Stage that captures content from traced syscalls.
pub struct CaptureStage {
    pub handle: PtraceHandle,
    pub bus: RecordBus,
    pub policy: CapturePolicy,
    /// Per-path mutex that serializes concurrent writes to the same file.
    pub write_locks: DashMap<PathBuf, Mutex<()>>,
    /// Tracked file content hashes shared with `ClassifyStage`.
    ///
    /// Used instead of filesystem reads for `before_hash` to avoid
    /// races between resuming a previous write and reading the file
    /// for the next write's `before_hash`.
    pub file_state: Arc<DashMap<PathBuf, ContentHash>>,
}

impl CaptureStage {
    /// Create a new capture stage.
    pub fn new(
        handle: PtraceHandle,
        bus: RecordBus,
        policy: CapturePolicy,
        file_state: Arc<DashMap<PathBuf, ContentHash>>,
    ) -> Self {
        Self {
            handle,
            bus,
            policy,
            write_locks: DashMap::new(),
            file_state,
        }
    }

    /// Capture content for a classified event and return the enriched result.
    pub async fn capture(&self, event: ClassifiedEvent) -> CapturedEvent {
        let pid = event.pid;
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
        if level == CaptureLevel::Ignore {
            return CapturedContent::None;
        }

        // Acquire per-path lock to serialize concurrent writes for hash
        // chain correctness. Hold through before_hash read → after_hash emit.
        let lock = self.write_locks.entry(path.to_path_buf()).or_insert_with(|| Mutex::new(()));
        let _guard = lock.lock().await;

        // Use tracked in-memory hash instead of racy filesystem read.
        let before_hash = self.file_state.get(path).map(|h| h.clone());

        let after_hash = if level == CaptureLevel::Full {
            self.handle.read_memory(pid, buf_addr, len).await.ok().map(|d| {
                self.policy.record_bytes(pid.as_raw() as u32, d.len());
                emit_content(&self.bus, d)
            })
        } else {
            None
        };

        // Update tracked state so the next write sees this write's hash.
        if let Some(ref hash) = after_hash {
            self.file_state.insert(path.to_path_buf(), hash.clone());
        }

        CapturedContent::FileWrite { before_hash, after_hash, size: len }
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
            return CapturedContent::FileRead { content_hash: None, size: len };
        }

        let content_hash = self.handle.read_memory(pid, buf_addr, len).await.ok().map(|d| {
            self.policy.record_bytes(pid.as_raw() as u32, d.len());
            emit_content(&self.bus, d)
        });

        CapturedContent::FileRead { content_hash, size: len }
    }

    async fn capture_delete(&self, path: &Path) -> CapturedContent {
        let content_hash = self.handle.read_file(path.to_path_buf()).await.ok().map(|d| hash_and_emit(&self.bus, d));
        CapturedContent::FileDelete { content_hash }
    }

    async fn capture_stream(
        &self,
        pid: Pid,
        buf_addr: usize,
        len: usize,
    ) -> CapturedContent {
        let content_hash = self.handle.read_memory(pid, buf_addr, len).await.ok().map(|d| {
            self.policy.record_bytes(pid.as_raw() as u32, d.len());
            emit_content(&self.bus, d)
        });
        CapturedContent::StreamData { content_hash, size: len }
    }
}

/// Emit one or more `Content` records for `data`, returning the root hash.
///
/// Data under `CHUNK_THRESHOLD` emits a single Content record.
/// Larger data is chunked into `CHUNK_SIZE` pieces and a Manifest is emitted.
fn emit_content(bus: &RecordBus, data: Vec<u8>) -> ContentHash {
    if data.len() < CHUNK_THRESHOLD {
        let hash = ContentHash::from_data(&data);
        bus.emit(Record::Content { hash: hash.clone(), data });
        return hash;
    }

    let mut chunk_hashes = Vec::new();
    for chunk in data.chunks(CHUNK_SIZE) {
        let ch = ContentHash::from_data(chunk);
        bus.emit(Record::Content { hash: ch.clone(), data: chunk.to_vec() });
        chunk_hashes.push(ch);
    }

    // Manifest hash is derived from the concatenated chunk hash strings.
    let manifest_input: String = chunk_hashes.iter().map(|h| h.as_str()).collect::<Vec<_>>().join("\n");
    let manifest_hash = ContentHash::from_data(manifest_input.as_bytes());
    bus.emit(Record::Manifest { hash: manifest_hash.clone(), chunks: chunk_hashes });
    manifest_hash
}

/// Hash and emit content, returning the hash without policy accounting.
fn hash_and_emit(bus: &RecordBus, data: Vec<u8>) -> ContentHash {
    emit_content(bus, data)
}
