// Rust guideline compliant 2026-02-21

//! Async upload pool with retry and backoff.
//!
//! [`UploadPool`] manages a fixed number of tokio worker tasks that
//! consume [`UploadJob`] items from an mpsc channel, retry on
//! transient failures with exponential backoff, and track aggregate
//! statistics via atomic counters in [`UploadStats`].
//!
//! Upload confirmations are sent to an optional callback channel so
//! that the digest cache and local buffer eviction can react to
//! completed uploads.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::event;

use crate::cas::ContentHash;
use crate::config::UploadConfig;

use super::object_store_dyn::DynObjectStore;
use super::s3::S3Client;

/// A unit of work submitted to the upload pool.
#[derive(Debug, Clone)]
pub enum UploadJob {
    /// Content-addressable blob.
    CasObject {
        /// Content hash used as the key.
        hash: ContentHash,
        /// Raw content bytes.
        data: Vec<u8>,
    },
    /// Event log segment.
    EventSegment {
        /// Agent that produced this segment.
        agent_id: String,
        /// Monotonic segment sequence number.
        seq: u64,
        /// JSONL-encoded event data.
        data: Vec<u8>,
    },
    /// Merkle tree checkpoint.
    Checkpoint {
        /// Agent that owns this checkpoint.
        agent_id: String,
        /// Checkpoint sequence number.
        seq: u64,
        /// Serialized checkpoint data.
        data: Vec<u8>,
    },
    /// Digest cache snapshot for fast recovery.
    DigestCacheSnapshot {
        /// Agent that owns this cache.
        agent_id: String,
        /// Serialized cache data.
        data: Vec<u8>,
    },
}

impl UploadJob {
    fn s3_key(&self) -> String {
        match self {
            Self::CasObject { hash, .. } => S3Client::cas_key(hash),
            Self::EventSegment { agent_id, seq, .. } => {
                S3Client::event_segment_key(agent_id, *seq)
            }
            Self::Checkpoint { agent_id, seq, .. } => {
                S3Client::checkpoint_key(agent_id, *seq)
            }
            Self::DigestCacheSnapshot { agent_id, .. } => {
                S3Client::digest_cache_key(agent_id)
            }
        }
    }

    fn data(&self) -> &[u8] {
        match self {
            Self::CasObject { data, .. }
            | Self::EventSegment { data, .. }
            | Self::Checkpoint { data, .. }
            | Self::DigestCacheSnapshot { data, .. } => data,
        }
    }

    fn into_data(self) -> Vec<u8> {
        match self {
            Self::CasObject { data, .. }
            | Self::EventSegment { data, .. }
            | Self::Checkpoint { data, .. }
            | Self::DigestCacheSnapshot { data, .. } => data,
        }
    }
}

/// Aggregate upload statistics tracked with atomic counters.
///
/// Safe to read from any thread at any time. Counters are
/// monotonically increasing (except `pending` which fluctuates).
#[derive(Debug, Default)]
pub struct UploadStats {
    /// Jobs waiting or in-flight.
    pub pending: AtomicU64,
    /// Successfully uploaded jobs.
    pub uploaded: AtomicU64,
    /// Jobs that exhausted all retries.
    pub failed: AtomicU64,
    /// Total bytes successfully uploaded.
    pub bytes_uploaded: AtomicU64,
}

/// Snapshot of upload stats for reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UploadStatsSnapshot {
    /// Jobs waiting or in-flight.
    pub pending: u64,
    /// Successfully uploaded jobs.
    pub uploaded: u64,
    /// Jobs that exhausted all retries.
    pub failed: u64,
    /// Total bytes successfully uploaded.
    pub bytes_uploaded: u64,
}

impl UploadStats {
    /// Take a consistent-ish snapshot of all counters.
    pub fn snapshot(&self) -> UploadStatsSnapshot {
        UploadStatsSnapshot {
            pending: self.pending.load(Ordering::Relaxed),
            uploaded: self.uploaded.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
            bytes_uploaded: self.bytes_uploaded.load(Ordering::Relaxed),
        }
    }
}

/// Confirmation sent after a successful upload.
#[derive(Debug, Clone)]
pub struct UploadConfirmation {
    /// The S3 key that was uploaded.
    pub key: String,
    /// Number of bytes uploaded.
    pub bytes: u64,
}

/// Async upload pool with configurable concurrency and retry.
#[derive(Debug)]
pub struct UploadPool {
    tx: mpsc::Sender<UploadJob>,
    stats: Arc<UploadStats>,
    workers: Vec<JoinHandle<()>>,
    confirm_rx: mpsc::Receiver<UploadConfirmation>,
}

impl UploadPool {
    /// Create and start the upload pool.
    ///
    /// Spawns `config.max_concurrent` worker tasks that consume jobs
    /// from a shared channel.
    pub fn new(
        store: DynObjectStore,
        config: &UploadConfig,
        channel_capacity: usize,
    ) -> Self {
        let (tx, rx) = mpsc::channel::<UploadJob>(channel_capacity);
        let rx = Arc::new(tokio::sync::Mutex::new(rx));
        let stats = Arc::new(UploadStats::default());
        let (confirm_tx, confirm_rx) = mpsc::channel(channel_capacity);

        let mut workers = Vec::with_capacity(config.max_concurrent as usize);

        for worker_id in 0..config.max_concurrent {
            let rx = Arc::clone(&rx);
            let store = store.clone();
            let stats = Arc::clone(&stats);
            let retry_max = config.retry_max;
            let backoff_base = config.retry_backoff_base;
            let confirm_tx = confirm_tx.clone();

            let handle = tokio::spawn(async move {
                worker_loop(
                    worker_id,
                    rx,
                    store,
                    stats,
                    retry_max,
                    backoff_base,
                    confirm_tx,
                )
                .await;
            });
            workers.push(handle);
        }

        Self {
            tx,
            stats,
            workers,
            confirm_rx,
        }
    }

    /// Submit a job for async upload.
    ///
    /// Returns immediately. The job will be picked up by a worker task.
    ///
    /// # Errors
    ///
    /// Returns an error if the pool has been shut down.
    pub fn submit(&self, job: UploadJob) -> Result<()> {
        self.stats.pending.fetch_add(1, Ordering::Relaxed);
        self.tx
            .try_send(job)
            .map_err(|e| {
                self.stats.pending.fetch_sub(1, Ordering::Relaxed);
                anyhow::anyhow!("upload pool channel error: {e}")
            })
    }

    /// Read-only access to the stats counters.
    pub fn stats(&self) -> &UploadStats {
        &self.stats
    }

    /// Mutable access to the confirmation receiver.
    ///
    /// Callers can poll this to learn about completed uploads for
    /// local buffer eviction and digest cache bookkeeping.
    pub fn confirmations(&mut self) -> &mut mpsc::Receiver<UploadConfirmation> {
        &mut self.confirm_rx
    }

    /// Drain the queue and wait for all workers to finish.
    ///
    /// Drops the sender so workers exit after processing remaining
    /// items. Returns the shared stats handle so callers can inspect
    /// final counters.
    ///
    /// # Errors
    ///
    /// Returns an error if any worker panicked.
    pub async fn shutdown(self) -> Result<Arc<UploadStats>> {
        drop(self.tx);

        for (i, handle) in self.workers.into_iter().enumerate() {
            handle
                .await
                .with_context(|| format!("upload worker {i} panicked"))?;
        }
        Ok(self.stats)
    }
}

async fn worker_loop(
    worker_id: u32,
    rx: Arc<tokio::sync::Mutex<mpsc::Receiver<UploadJob>>>,
    store: DynObjectStore,
    stats: Arc<UploadStats>,
    retry_max: u32,
    backoff_base: Duration,
    confirm_tx: mpsc::Sender<UploadConfirmation>,
) {
    loop {
        let job = {
            let mut rx = rx.lock().await;
            rx.recv().await
        };

        let Some(job) = job else {
            break;
        };

        let key = job.s3_key();
        let byte_count = job.data().len() as u64;
        let data = job.into_data();

        let success = upload_with_retry(
            &store,
            &key,
            data,
            retry_max,
            backoff_base,
            worker_id,
        )
        .await;

        stats.pending.fetch_sub(1, Ordering::Relaxed);

        if success {
            stats.uploaded.fetch_add(1, Ordering::Relaxed);
            stats
                .bytes_uploaded
                .fetch_add(byte_count, Ordering::Relaxed);
            let _ = confirm_tx
                .send(UploadConfirmation {
                    key,
                    bytes: byte_count,
                })
                .await;
        } else {
            stats.failed.fetch_add(1, Ordering::Relaxed);
        }
    }
}

async fn upload_with_retry(
    store: &DynObjectStore,
    key: &str,
    data: Vec<u8>,
    retry_max: u32,
    backoff_base: Duration,
    worker_id: u32,
) -> bool {
    for attempt in 0..retry_max {
        match store.put(key, data.clone()).await {
            Ok(()) => return true,
            Err(err) => {
                if attempt + 1 < retry_max {
                    let delay = backoff_base * 2u32.saturating_pow(attempt);
                    event!(
                        name: "upload.retry",
                        tracing::Level::WARN,
                        upload.key = key,
                        upload.attempt = attempt + 1,
                        upload.max_retries = retry_max,
                        upload.worker_id = worker_id,
                        error.message = %err,
                        "upload failed, retrying in {delay:?}: {{upload.key}}"
                    );
                    tokio::time::sleep(delay).await;
                } else {
                    event!(
                        name: "upload.exhausted",
                        tracing::Level::ERROR,
                        upload.key = key,
                        upload.attempts = retry_max,
                        upload.worker_id = worker_id,
                        error.message = %err,
                        "upload failed after {{upload.attempts}} attempts: {{upload.key}}"
                    );
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU32;

    use tokio::sync::Mutex;

    use super::*;
    use crate::storage::s3::ObjectStore;

    /// Mock object store that can be configured to fail N times.
    #[derive(Debug)]
    struct MockStore {
        fail_count: AtomicU32,
        stored: Mutex<Vec<(String, Vec<u8>)>>,
    }

    impl MockStore {
        fn new(fail_count: u32) -> Self {
            Self {
                fail_count: AtomicU32::new(fail_count),
                stored: Mutex::new(Vec::new()),
            }
        }

        async fn stored_keys(&self) -> Vec<String> {
            self.stored
                .lock()
                .await
                .iter()
                .map(|(k, _)| k.clone())
                .collect()
        }
    }

    impl ObjectStore for MockStore {
        async fn put(&self, key: &str, data: Vec<u8>) -> Result<()> {
            let remaining = self.fail_count.load(Ordering::Relaxed);
            if remaining > 0 {
                self.fail_count.fetch_sub(1, Ordering::Relaxed);
                anyhow::bail!("simulated failure");
            }
            self.stored
                .lock()
                .await
                .push((key.to_owned(), data));
            Ok(())
        }

        async fn get(&self, key: &str) -> Result<Vec<u8>> {
            let stored = self.stored.lock().await;
            stored
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .ok_or_else(|| anyhow::anyhow!("not found: {key}"))
        }

        async fn exists(&self, key: &str) -> Result<bool> {
            Ok(self.stored.lock().await.iter().any(|(k, _)| k == key))
        }

        async fn list(&self, prefix: &str) -> Result<Vec<String>> {
            Ok(self
                .stored
                .lock()
                .await
                .iter()
                .filter(|(k, _)| k.starts_with(prefix))
                .map(|(k, _)| k.clone())
                .collect())
        }
    }

    fn test_config() -> UploadConfig {
        UploadConfig {
            max_concurrent: 2,
            retry_max: 3,
            // Very short backoff for tests.
            retry_backoff_base: Duration::from_millis(1),
        }
    }

    #[tokio::test]
    async fn submit_and_process_jobs() {
        let store = Arc::new(MockStore::new(0));
        let dyn_store = DynObjectStore::new(Arc::clone(&store));
        let config = test_config();
        let pool = UploadPool::new(dyn_store, &config, 64);

        pool.submit(UploadJob::CasObject {
            hash: ContentHash::from_data(b"hello"),
            data: b"hello".to_vec(),
        })
        .unwrap();

        pool.submit(UploadJob::EventSegment {
            agent_id: "a1".into(),
            seq: 1,
            data: b"event data".to_vec(),
        })
        .unwrap();

        pool.shutdown().await.unwrap();

        let keys = store.stored_keys().await;
        assert_eq!(keys.len(), 2);
    }

    #[tokio::test]
    async fn stats_track_uploads() {
        let store = Arc::new(MockStore::new(0));
        let dyn_store = DynObjectStore::new(Arc::clone(&store));
        let config = test_config();
        let pool = UploadPool::new(dyn_store, &config, 64);

        pool.submit(UploadJob::Checkpoint {
            agent_id: "a1".into(),
            seq: 0,
            data: vec![1, 2, 3],
        })
        .unwrap();

        let stats = pool.shutdown().await.unwrap();

        let snap = stats.snapshot();
        assert_eq!(snap.uploaded, 1);
        assert_eq!(snap.failed, 0);
        assert_eq!(snap.pending, 0);
        assert_eq!(snap.bytes_uploaded, 3);
    }

    #[tokio::test]
    async fn retry_then_succeed() {
        // Fail twice, succeed on third attempt.
        let store = Arc::new(MockStore::new(2));
        let dyn_store = DynObjectStore::new(Arc::clone(&store));
        let config = test_config();
        let pool = UploadPool::new(dyn_store, &config, 64);

        pool.submit(UploadJob::CasObject {
            hash: ContentHash::from_data(b"retry-me"),
            data: b"retry-me".to_vec(),
        })
        .unwrap();

        let stats = pool.shutdown().await.unwrap();

        let snap = stats.snapshot();
        assert_eq!(snap.uploaded, 1);
        assert_eq!(snap.failed, 0);
    }

    #[tokio::test]
    async fn exhaust_retries_marks_failed() {
        // Fail more times than retry_max.
        let store = Arc::new(MockStore::new(100));
        let dyn_store = DynObjectStore::new(Arc::clone(&store));
        let config = test_config();
        let pool = UploadPool::new(dyn_store, &config, 64);

        pool.submit(UploadJob::CasObject {
            hash: ContentHash::from_data(b"doomed"),
            data: b"doomed".to_vec(),
        })
        .unwrap();

        let stats = pool.shutdown().await.unwrap();

        let snap = stats.snapshot();
        assert_eq!(snap.uploaded, 0);
        assert_eq!(snap.failed, 1);
        assert_eq!(snap.bytes_uploaded, 0);
    }

    #[tokio::test]
    async fn shutdown_drains_queue() {
        let store = Arc::new(MockStore::new(0));
        let dyn_store = DynObjectStore::new(Arc::clone(&store));
        let config = test_config();
        let pool = UploadPool::new(dyn_store, &config, 64);

        for i in 0..10u64 {
            pool.submit(UploadJob::EventSegment {
                agent_id: "a1".into(),
                seq: i,
                data: vec![i as u8],
            })
            .unwrap();
        }

        pool.shutdown().await.unwrap();

        let keys = store.stored_keys().await;
        assert_eq!(keys.len(), 10);
    }

    #[tokio::test]
    async fn confirmations_received() {
        let store = Arc::new(MockStore::new(0));
        let dyn_store = DynObjectStore::new(Arc::clone(&store));
        let config = test_config();
        let mut pool = UploadPool::new(dyn_store, &config, 64);

        pool.submit(UploadJob::DigestCacheSnapshot {
            agent_id: "a1".into(),
            data: b"cache-data".to_vec(),
        })
        .unwrap();

        // Grab confirmation before shutdown drops the channel.
        let confirm = pool
            .confirmations()
            .recv()
            .await
            .expect("should receive confirmation");

        assert_eq!(confirm.key, "meta/a1/digest-cache-latest.bin");
        assert_eq!(confirm.bytes, 10);

        pool.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn upload_job_key_construction() {
        let hash = ContentHash::from_data(b"test");
        let job = UploadJob::CasObject {
            hash: hash.clone(),
            data: vec![],
        };
        assert_eq!(job.s3_key(), S3Client::cas_key(&hash));

        let job = UploadJob::EventSegment {
            agent_id: "x".into(),
            seq: 5,
            data: vec![],
        };
        assert_eq!(job.s3_key(), "events/x/5.jsonl");

        let job = UploadJob::Checkpoint {
            agent_id: "x".into(),
            seq: 3,
            data: vec![],
        };
        assert_eq!(job.s3_key(), "checkpoints/x/3.bin");

        let job = UploadJob::DigestCacheSnapshot {
            agent_id: "x".into(),
            data: vec![],
        };
        assert_eq!(job.s3_key(), "meta/x/digest-cache-latest.bin");
    }
}
