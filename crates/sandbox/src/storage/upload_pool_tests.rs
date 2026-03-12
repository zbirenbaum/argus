use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use tokio::sync::Mutex;

use super::*;
use crate::cas::ContentHash;
use crate::storage::s3::{ObjectStore, S3Client};
use crate::storage::upload_job::UploadJob;

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
        max_attempts: 3,
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

#[test]
fn jittered_delay_within_bounds() {
    let base = Duration::from_millis(100);
    for _ in 0..100 {
        let result = super::jittered(base);
        let ms = result.as_millis();
        assert!(ms >= 50, "jittered delay {ms}ms below 50ms floor");
        assert!(ms <= 150, "jittered delay {ms}ms above 150ms ceiling");
    }
}
