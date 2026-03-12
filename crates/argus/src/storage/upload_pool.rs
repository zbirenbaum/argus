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
use bytes::Bytes;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::event;

use crate::config::UploadConfig;

use super::object_store_dyn::DynObjectStore;
use super::upload_job::UploadJob;

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
    /// Jobs that exhausted all attempts.
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
    /// Jobs that exhausted all attempts.
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
            let max_attempts = config.max_attempts;
            let backoff_base = config.retry_backoff_base;
            let confirm_tx = confirm_tx.clone();

            let handle = tokio::spawn(async move {
                worker_loop(
                    worker_id, rx, store, stats,
                    max_attempts, backoff_base, confirm_tx,
                )
                .await;
            });
            workers.push(handle);
        }

        Self { tx, stats, workers, confirm_rx }
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
    max_attempts: u32,
    backoff_base: Duration,
    confirm_tx: mpsc::Sender<UploadConfirmation>,
) {
    loop {
        let job = {
            let mut rx = rx.lock().await;
            rx.recv().await
        };
        let Some(job) = job else { break };

        let key = job.s3_key();
        let byte_count = job.data().len() as u64;
        let data = Bytes::from(job.into_data());

        let success = upload_with_retry(
            &store, &key, data, max_attempts, backoff_base, worker_id,
        )
        .await;

        stats.pending.fetch_sub(1, Ordering::Relaxed);

        if success {
            stats.uploaded.fetch_add(1, Ordering::Relaxed);
            stats.bytes_uploaded.fetch_add(byte_count, Ordering::Relaxed);
            if confirm_tx
                .send(UploadConfirmation { key, bytes: byte_count })
                .await
                .is_err()
            {
                event!(
                    name: "upload.confirm_send.failed",
                    tracing::Level::WARN,
                    upload.worker_id = worker_id,
                    "confirmation receiver dropped, upload confirmed but not tracked"
                );
            }
        } else {
            stats.failed.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Apply randomized jitter to a delay: `delay * rand(0.5..1.5)`.
fn jittered(delay: Duration) -> Duration {
    let jitter_factor = 0.5 + fastrand::f64();
    delay.mul_f64(jitter_factor)
}

async fn upload_with_retry(
    store: &DynObjectStore,
    key: &str,
    data: Bytes,
    max_attempts: u32,
    backoff_base: Duration,
    worker_id: u32,
) -> bool {
    for attempt in 0..max_attempts {
        match store.put(key, data.to_vec()).await {
            Ok(()) => return true,
            Err(err) => {
                if attempt + 1 < max_attempts {
                    let delay = jittered(
                        backoff_base * 2u32.saturating_pow(attempt),
                    );
                    event!(
                        name: "upload.retry",
                        tracing::Level::WARN,
                        upload.key = key,
                        upload.attempt = attempt + 1,
                        upload.max_attempts = max_attempts,
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
                        upload.attempts = max_attempts,
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
#[path = "upload_pool_tests.rs"]
mod tests;
