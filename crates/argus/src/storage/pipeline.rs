//! Unified storage pipeline: CAS + event log + S3 upload + digest cache.
//!
//! [`StoragePipeline`] composes the individual storage components into
//! a single write path. Content is stored locally and enqueued for
//! async S3 upload. Upload confirmations update the digest cache and
//! local buffer for continuous pruning.
//!
//! `process_confirmations()` is non-blocking and must be called from
//! the supervisor's main loop (after each ptrace stop handler returns)
//! so the local buffer is pruned continuously during long-running agents.

use std::path::PathBuf;

use anyhow::{Context, Result};
use tracing::event;

use crate::cas::{Cas, LocalCas, ContentHash};
use crate::config::{DurabilityMode, StorageConfig};
use crate::events::Event;
use crate::storage::digest_cache::DigestCache;
use crate::storage::event_log::EventLog;
use crate::storage::local_buffer::LocalBuffer;
use crate::storage::object_store_dyn::DynObjectStore;
use crate::storage::upload_job::UploadJob;
use crate::storage::upload_pool::{UploadPool, UploadStatsSnapshot};

/// Unified storage pipeline tying CAS, event log, upload pool,
/// digest cache, and local buffer together.
pub struct StoragePipeline {
    cas: LocalCas,
    event_log: EventLog,
    upload_pool: UploadPool,
    digest_cache: DigestCache,
    local_buffer: LocalBuffer,
    agent_id: String,
    cas_dir: PathBuf,
}

impl StoragePipeline {
    /// Construct a pipeline from config, wiring all components.
    ///
    /// `store` is the S3-compatible backend (real or mock). Pass a
    /// `DynObjectStore` wrapping an `S3Client` for production, or a
    /// mock for tests.
    pub fn new(
        config: &StorageConfig,
        agent_id: String,
        store: DynObjectStore,
        durability: DurabilityMode,
    ) -> Result<Self> {
        let cas_dir = config.local_buffer.cas_dir.clone();
        let cas = LocalCas::new(cas_dir.clone())
            .context("create CAS store")?;

        let event_log = EventLog::new(
            agent_id.clone(),
            config.local_buffer.event_dir.clone(),
            durability,
        )
        .context("create event log")?;

        let upload_pool = UploadPool::new(
            store,
            &config.upload,
            256,
        );

        let digest_cache = DigestCache::load_or_default(
            &config.digest_cache.path,
            config.digest_cache.ttl,
        );

        let local_buffer = LocalBuffer::new(
            config.local_buffer.max_size.as_u64(),
        );

        Ok(Self {
            cas,
            event_log,
            upload_pool,
            digest_cache,
            local_buffer,
            agent_id,
            cas_dir,
        })
    }

    /// Hash and store content in the local CAS, enqueue S3 upload
    /// if not already known remotely.
    pub fn store_content(&mut self, data: &[u8]) -> Result<ContentHash> {
        let hash = self.cas.put(data)
            .context("CAS put")?;

        let local_path = self.cas.object_path(&hash);
        self.local_buffer.track(local_path, data.len() as u64);

        if !self.digest_cache.contains(&hash) {
            let job = UploadJob::CasObject {
                hash: hash.clone(),
                data: data.to_vec(),
            };
            self.upload_pool.submit(job)
                .context("submit CAS upload")?;
        }

        Ok(hash)
    }

    /// Append an event to the log. Segment rotation and upload
    /// happen automatically when the size threshold is reached.
    pub fn append_event(&mut self, event: &Event) -> Result<()> {
        self.event_log.append(event, Some(&self.upload_pool))
    }

    /// Drain pending upload confirmations, updating digest cache
    /// and local buffer. Non-blocking — returns immediately if no
    /// confirmations are ready.
    ///
    /// Call this from the supervisor's main loop after each ptrace
    /// stop handler returns to keep the local buffer pruned during
    /// long-running agents.
    pub fn process_confirmations(&mut self) -> Result<usize> {
        let mut count = 0;
        while let Ok(confirm) = self.upload_pool.confirmations().try_recv() {
            if let Some(hash) = extract_cas_hash(&confirm.key) {
                self.digest_cache.insert(hash, confirm.bytes);
            }
            self.local_buffer.confirm_upload(&confirm.key);
            count += 1;
        }

        if count > 0 {
            let pruned = self.local_buffer.prune()?;
            if pruned > 0 {
                event!(
                    name: "pipeline.buffer.pruned",
                    tracing::Level::DEBUG,
                    pipeline.pruned_files = pruned,
                    pipeline.confirmations = count,
                    "pruned {pruned} files after {count} confirmations"
                );
            }
        }

        Ok(count)
    }

    /// Persist the digest cache to disk and enqueue an S3 snapshot.
    pub fn save_digest_cache(&mut self) -> Result<()> {
        self.digest_cache.save_to_disk()
            .context("save digest cache to disk")?;

        let data = std::fs::read(self.digest_cache_path())
            .context("read digest cache for upload")?;

        let job = UploadJob::DigestCacheSnapshot {
            agent_id: self.agent_id.clone(),
            data,
        };
        self.upload_pool.submit(job)
            .context("submit digest cache snapshot upload")?;

        Ok(())
    }

    /// Flush the event log to disk without rotating.
    pub fn flush(&mut self) -> Result<()> {
        self.event_log.flush()
    }

    /// Finalize all storage: flush event log, save digest cache,
    /// drain the upload pool.
    pub async fn shutdown(mut self) -> Result<UploadStatsSnapshot> {
        self.event_log.finalize(Some(&self.upload_pool))
            .context("finalize event log")?;

        self.save_digest_cache()
            .context("save digest cache on shutdown")?;

        let stats = self.upload_pool.shutdown().await
            .context("shutdown upload pool")?;

        let snap = stats.snapshot();
        event!(
            name: "pipeline.shutdown",
            tracing::Level::INFO,
            pipeline.uploaded = snap.uploaded,
            pipeline.failed = snap.failed,
            pipeline.bytes = snap.bytes_uploaded,
            "storage pipeline shut down"
        );

        Ok(snap)
    }

    /// Read-only access to upload stats.
    pub fn upload_stats(&self) -> UploadStatsSnapshot {
        self.upload_pool.stats().snapshot()
    }

    /// Number of entries in the digest cache.
    pub fn digest_cache_len(&self) -> usize {
        self.digest_cache.len()
    }

    /// Total bytes tracked by the local buffer.
    pub fn local_buffer_bytes(&self) -> u64 {
        self.local_buffer.total_bytes()
    }

    /// Read content from the local CAS by hash.
    pub fn read_content(&self, hash: &ContentHash) -> Result<Vec<u8>> {
        self.cas.get(hash)
    }

    /// Current event log segment sequence number.
    pub fn current_segment_seq(&self) -> u64 {
        self.event_log.current_segment_seq()
    }

    fn digest_cache_path(&self) -> PathBuf {
        self.cas_dir
            .parent()
            .unwrap_or(&self.cas_dir)
            .join("digest-cache.bin")
    }
}

/// Extract a `ContentHash` from a CAS S3 key like `cas/blake3/ab/cdef...`.
fn extract_cas_hash(key: &str) -> Option<ContentHash> {
    let rest = key.strip_prefix("cas/")?;
    let (algorithm, remainder) = rest.split_once('/')?;
    let (hex_prefix, hex_suffix) = remainder.split_once('/')?;
    if hex_prefix.len() != 2 {
        return None;
    }
    let full = format!("{algorithm}:{hex_prefix}{hex_suffix}");
    ContentHash::try_from(full).ok()
}

#[cfg(test)]
#[path = "pipeline_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "pipeline_integration_test.rs"]
mod integration_tests;
