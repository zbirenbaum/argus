use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use tokio::sync::Mutex;

use crate::cas::ContentHash;
use crate::config::{
    DigestCacheConfig, DurabilityMode, LocalBufferConfig, StorageConfig,
    UploadConfig,
};
use crate::events::envelope::{Event, EventPayload, SequenceGenerator};
use crate::events::process;
use crate::storage::object_store_dyn::DynObjectStore;
use crate::storage::s3::ObjectStore;

use super::StoragePipeline;

/// Mock object store for pipeline tests.
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

    async fn get_stored(&self, key: &str) -> Option<Vec<u8>> {
        self.stored
            .lock()
            .await
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }
}

impl ObjectStore for MockStore {
    async fn put(
        &self,
        key: &str,
        data: Vec<u8>,
    ) -> anyhow::Result<()> {
        let remaining = self.fail_count.load(Ordering::Relaxed);
        if remaining > 0 {
            self.fail_count.fetch_sub(1, Ordering::Relaxed);
            anyhow::bail!("simulated failure");
        }
        self.stored.lock().await.push((key.to_owned(), data));
        Ok(())
    }

    async fn get(&self, key: &str) -> anyhow::Result<Vec<u8>> {
        self.stored
            .lock()
            .await
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .ok_or_else(|| anyhow::anyhow!("not found: {key}"))
    }

    async fn exists(&self, key: &str) -> anyhow::Result<bool> {
        Ok(self.stored.lock().await.iter().any(|(k, _)| k == key))
    }

    async fn list(
        &self,
        prefix: &str,
    ) -> anyhow::Result<Vec<String>> {
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

fn test_config(dir: &std::path::Path) -> StorageConfig {
    StorageConfig {
        s3: None,
        upload: UploadConfig {
            max_concurrent: 2,
            max_attempts: 3,
            retry_backoff_base: Duration::from_millis(1),
        },
        local_buffer: LocalBufferConfig {
            cas_dir: dir.join("cas"),
            event_dir: dir.join("events"),
            max_size: bytesize::ByteSize::mib(10),
            min_retention: Duration::from_secs(0),
        },
        digest_cache: DigestCacheConfig {
            path: dir.join("digest-cache.bin"),
            ttl: Duration::from_secs(3600),
            snapshot_interval: Duration::from_secs(600),
            rebuild_on_start: false,
        },
    }
}

fn make_event(seq_gen: &SequenceGenerator) -> Event {
    Event::new(
        seq_gen,
        "test-agent".to_string(),
        EventPayload::Exec(process::Exec {
            pid: 42,
            ppid: 1,
            binary: "/bin/echo".into(),
            argv: vec!["echo".into(), "hello".into()],
            envp: vec![],
            cwd: "/workspace".into(),
        }),
    )
}

#[tokio::test]
async fn store_content_uploads_to_s3() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());
    let store = Arc::new(MockStore::new(0));
    let dyn_store = DynObjectStore::new(Arc::clone(&store));

    let mut pipeline = StoragePipeline::new(
        &config,
        "test-agent".into(),
        dyn_store,
        DurabilityMode::Memory,
    )
    .unwrap();

    let data = b"hello world";
    let hash = pipeline.store_content(data).unwrap();

    // Content should be in local CAS
    let read_back = pipeline.read_content(&hash).unwrap();
    assert_eq!(read_back, data);

    // Shutdown to drain uploads
    pipeline.shutdown().await.unwrap();

    // Verify CAS object uploaded
    let cas_key = format!("cas/{}/{}", hash.prefix(), hash.suffix());
    let uploaded = store.get_stored(&cas_key).await;
    assert!(uploaded.is_some());
    assert_eq!(uploaded.unwrap(), data);
}

#[tokio::test]
async fn duplicate_content_skips_upload() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());
    let store = Arc::new(MockStore::new(0));
    let dyn_store = DynObjectStore::new(Arc::clone(&store));

    let mut pipeline = StoragePipeline::new(
        &config,
        "test-agent".into(),
        dyn_store,
        DurabilityMode::Memory,
    )
    .unwrap();

    let data = b"duplicate content";
    let h1 = pipeline.store_content(data).unwrap();

    // Wait for upload confirmation and process it
    tokio::time::sleep(Duration::from_millis(50)).await;
    pipeline.process_confirmations().unwrap();

    // Second store should skip upload (digest cache hit)
    let h2 = pipeline.store_content(data).unwrap();
    assert_eq!(h1, h2);

    let stats = pipeline.shutdown().await.unwrap();
    // 1 CAS upload (dedup skipped the second) + 1 digest cache snapshot.
    // No event segment because we didn't append any events.
    assert_eq!(stats.uploaded, 2, "expected 2 uploads (1 CAS + digest snapshot)");
}

#[tokio::test]
async fn append_events_and_rotate_segment() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = test_config(dir.path());
    // Tiny segment to force rotation
    config.local_buffer.event_dir = dir.path().join("events");

    let store = Arc::new(MockStore::new(0));
    let dyn_store = DynObjectStore::new(Arc::clone(&store));

    let mut pipeline = StoragePipeline::new(
        &config,
        "test-agent".into(),
        dyn_store,
        DurabilityMode::Memory,
    )
    .unwrap();

    let seq_gen = SequenceGenerator::default();

    // Append several events
    for _ in 0..5 {
        let event = make_event(&seq_gen);
        pipeline.append_event(&event).unwrap();
    }

    pipeline.shutdown().await.unwrap();

    // Event segment should have been uploaded (finalize submits it)
    let keys = store.stored_keys().await;
    let segment_keys: Vec<_> = keys
        .iter()
        .filter(|k| k.starts_with("events/"))
        .collect();
    assert!(
        !segment_keys.is_empty(),
        "expected at least one event segment uploaded"
    );
}

#[tokio::test]
async fn process_confirmations_updates_digest_cache() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());
    let store = Arc::new(MockStore::new(0));
    let dyn_store = DynObjectStore::new(Arc::clone(&store));

    let mut pipeline = StoragePipeline::new(
        &config,
        "test-agent".into(),
        dyn_store,
        DurabilityMode::Memory,
    )
    .unwrap();

    assert_eq!(pipeline.digest_cache_len(), 0);

    pipeline.store_content(b"content-a").unwrap();
    pipeline.store_content(b"content-b").unwrap();

    // Give upload pool time to process
    tokio::time::sleep(Duration::from_millis(100)).await;

    let confirmed = pipeline.process_confirmations().unwrap();
    assert!(confirmed >= 2, "expected at least 2 confirmations, got {confirmed}");
    assert!(pipeline.digest_cache_len() >= 2);
}

#[tokio::test]
async fn save_digest_cache_uploads_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());
    let store = Arc::new(MockStore::new(0));
    let dyn_store = DynObjectStore::new(Arc::clone(&store));

    let mut pipeline = StoragePipeline::new(
        &config,
        "test-agent".into(),
        dyn_store,
        DurabilityMode::Memory,
    )
    .unwrap();

    pipeline.store_content(b"something").unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    pipeline.process_confirmations().unwrap();

    pipeline.save_digest_cache().unwrap();
    pipeline.shutdown().await.unwrap();

    let keys = store.stored_keys().await;
    let cache_keys: Vec<_> = keys
        .iter()
        .filter(|k| k.contains("digest-cache"))
        .collect();
    assert!(
        !cache_keys.is_empty(),
        "expected digest cache snapshot uploaded"
    );
}

#[tokio::test]
async fn shutdown_reports_stats() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());
    let store = Arc::new(MockStore::new(0));
    let dyn_store = DynObjectStore::new(Arc::clone(&store));

    let mut pipeline = StoragePipeline::new(
        &config,
        "test-agent".into(),
        dyn_store,
        DurabilityMode::Memory,
    )
    .unwrap();

    pipeline.store_content(b"stats-test").unwrap();

    let seq_gen = SequenceGenerator::default();
    pipeline.append_event(&make_event(&seq_gen)).unwrap();

    let stats = pipeline.shutdown().await.unwrap();

    // CAS blob + event segment + digest cache snapshot
    assert!(
        stats.uploaded >= 2,
        "expected at least 2 uploads, got {}",
        stats.uploaded,
    );
    assert_eq!(stats.failed, 0);
}

#[test]
fn extract_cas_hash_parses_valid_key() {
    let hash = ContentHash::from_data(b"test");
    let key = format!("cas/{}/{}", hash.prefix(), hash.suffix());
    let extracted = super::extract_cas_hash(&key);
    assert_eq!(extracted, Some(hash));
}

#[test]
fn extract_cas_hash_returns_none_for_non_cas() {
    assert!(super::extract_cas_hash("events/a1/0.jsonl").is_none());
    assert!(super::extract_cas_hash("meta/a1/digest-cache-latest.bin").is_none());
    assert!(super::extract_cas_hash("cas/x/short").is_none());
}
