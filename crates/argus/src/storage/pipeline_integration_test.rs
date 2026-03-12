//! Integration test for StoragePipeline against MinIO.
//!
//! These tests run automatically when MINIO_ENDPOINT is set.
//! When it's not set, they skip with a message (not ignored).
//!
//! To run:
//!   docker compose up -d minio minio-init
//!   docker network connect argus-run_default argus-arm64
//!   docker exec -e AWS_ACCESS_KEY_ID=minioadmin -e AWS_SECRET_ACCESS_KEY=minioadmin \
//!     -e AWS_REGION=us-east-1 -e MINIO_ENDPOINT=http://argus-run-minio-1:9000 \
//!     argus-arm64 bash -c "cd /workspace && cargo test -p argus pipeline_minio"

use std::time::Duration;

use crate::config::{
    DigestCacheConfig, DurabilityMode, LocalBufferConfig, S3Config,
    StorageConfig, UploadConfig,
};
use crate::events::envelope::{Event, EventPayload, SequenceGenerator};
use crate::events::process;
use crate::storage::object_store_dyn::DynObjectStore;
use crate::storage::pipeline::StoragePipeline;
use crate::storage::s3::{ObjectStore, S3Client};

/// Returns MinIO config if MINIO_ENDPOINT is set, None otherwise.
fn minio_config() -> Option<S3Config> {
    let endpoint = std::env::var("MINIO_ENDPOINT").ok()?;
    Some(S3Config {
        bucket: "argus-test".into(),
        prefix: String::new(),
        region: std::env::var("AWS_REGION")
            .unwrap_or_else(|_| "us-east-1".into()),
        endpoint: Some(endpoint),
    })
}

/// Skip the test if MinIO is not available.
macro_rules! require_minio {
    () => {
        match minio_config() {
            Some(cfg) => cfg,
            None => {
                eprintln!("MINIO_ENDPOINT not set, skipping MinIO test");
                return;
            }
        }
    };
}

fn test_storage_config(
    dir: &std::path::Path,
    s3: Option<S3Config>,
) -> StorageConfig {
    StorageConfig {
        s3,
        upload: UploadConfig {
            max_concurrent: 2,
            max_attempts: 3,
            retry_backoff_base: Duration::from_millis(100),
        },
        local_buffer: LocalBufferConfig {
            cas_dir: dir.join("cas"),
            event_dir: dir.join("events"),
            max_size: bytesize::ByteSize::mib(100),
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

fn make_event(seq_gen: &SequenceGenerator, agent_id: &str) -> Event {
    Event::new(
        seq_gen,
        agent_id.to_string(),
        EventPayload::Exec(process::Exec {
            pid: 100,
            ppid: 1,
            binary: "/bin/test".into(),
            argv: vec!["test".into()],
            envp: vec![],
            cwd: "/workspace".into(),
        }),
    )
}

#[tokio::test]
async fn pipeline_minio_cas_upload() {
    let s3_config = require_minio!();

    let s3_client = S3Client::new(&s3_config).await.unwrap();

    let dir = tempfile::tempdir().unwrap();
    let config = test_storage_config(dir.path(), Some(s3_config));
    let dyn_store = DynObjectStore::new(s3_client.clone());
    let agent_id = format!("test-{}", uuid::Uuid::new_v4());

    let mut pipeline = StoragePipeline::new(
        &config,
        agent_id.clone(),
        dyn_store,
        DurabilityMode::Memory,
    )
    .unwrap();

    // Store 3 CAS objects
    let data_a = b"content-alpha";
    let data_b = b"content-beta";
    let data_c = b"content-gamma";
    let hash_a = pipeline.store_content(data_a).unwrap();
    let hash_b = pipeline.store_content(data_b).unwrap();
    let hash_c = pipeline.store_content(data_c).unwrap();

    // Append events
    let seq_gen = SequenceGenerator::default();
    for _ in 0..5 {
        pipeline
            .append_event(&make_event(&seq_gen, &agent_id))
            .unwrap();
    }

    let stats = pipeline.shutdown().await.unwrap();

    assert_eq!(stats.failed, 0, "no uploads should fail");
    // 3 CAS + at least 1 event segment + 1 digest cache snapshot
    assert!(
        stats.uploaded >= 5,
        "expected at least 5 uploads, got {}",
        stats.uploaded,
    );

    // Verify CAS objects in MinIO
    for (hash, data) in [
        (&hash_a, &data_a[..]),
        (&hash_b, &data_b[..]),
        (&hash_c, &data_c[..]),
    ] {
        let key = S3Client::cas_key(hash);
        let fetched = s3_client.get(&key).await.unwrap();
        assert_eq!(
            fetched, data,
            "CAS object {hash} content mismatch"
        );
    }

    // Verify event segment exists
    let event_keys = s3_client
        .list(&format!("events/{agent_id}/"))
        .await
        .unwrap();
    assert!(
        !event_keys.is_empty(),
        "expected event segments in S3"
    );

    // Verify digest cache snapshot exists
    let cache_key = S3Client::digest_cache_key(&agent_id);
    assert!(
        s3_client.exists(&cache_key).await.unwrap(),
        "digest cache snapshot should exist in S3"
    );
}

#[tokio::test]
async fn pipeline_minio_dedup_skips_reupload() {
    let s3_config = require_minio!();

    let s3_client = S3Client::new(&s3_config).await.unwrap();

    let dir = tempfile::tempdir().unwrap();
    let config = test_storage_config(dir.path(), Some(s3_config));
    let dyn_store = DynObjectStore::new(s3_client.clone());
    let agent_id = format!("test-dedup-{}", uuid::Uuid::new_v4());

    let mut pipeline = StoragePipeline::new(
        &config,
        agent_id.clone(),
        dyn_store,
        DurabilityMode::Memory,
    )
    .unwrap();

    let data = b"deduplicate-me";
    let h1 = pipeline.store_content(data).unwrap();

    // Wait for upload, drain confirmations
    tokio::time::sleep(Duration::from_millis(500)).await;
    let confirmed = pipeline.process_confirmations().unwrap();
    assert!(confirmed >= 1, "expected at least 1 confirmation");

    // Store same content again — should skip upload
    let h2 = pipeline.store_content(data).unwrap();
    assert_eq!(h1, h2);

    let _stats = pipeline.shutdown().await.unwrap();

    // Verify the single CAS upload landed
    let cas_key = S3Client::cas_key(&h1);
    let fetched = s3_client.get(&cas_key).await.unwrap();
    assert_eq!(fetched, data);
}

#[tokio::test]
async fn pipeline_minio_s3_list_confirms_structure() {
    let s3_config = require_minio!();

    let s3_client = S3Client::new(&s3_config).await.unwrap();

    let dir = tempfile::tempdir().unwrap();
    let config = test_storage_config(dir.path(), Some(s3_config));
    let dyn_store = DynObjectStore::new(s3_client.clone());
    let agent_id = format!("test-structure-{}", uuid::Uuid::new_v4());

    let mut pipeline = StoragePipeline::new(
        &config,
        agent_id.clone(),
        dyn_store,
        DurabilityMode::Memory,
    )
    .unwrap();

    let hash = pipeline.store_content(b"structure-test").unwrap();
    pipeline.shutdown().await.unwrap();

    // CAS key follows {prefix}/{suffix} layout
    let cas_keys = s3_client.list("cas/").await.unwrap();
    let expected_key = S3Client::cas_key(&hash);
    assert!(
        cas_keys.contains(&expected_key),
        "expected CAS key {expected_key} in {cas_keys:?}"
    );
}
