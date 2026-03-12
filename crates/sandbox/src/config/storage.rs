//! Storage configuration for S3, local buffers, and digest cache.
//!
//! Maps the `storage:` section of the supervisor YAML config.
//! All durations use `humantime-serde` for human-readable values like `5m`
//! and sizes use `bytesize` for values like `2GB`.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::bail;
use serde::{Deserialize, Serialize};

/// Aggregate storage configuration.
///
/// Groups S3, upload tuning, local buffer, and digest cache settings.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StorageConfig {
    /// S3-compatible object store. `None` means local-only mode.
    pub s3: Option<S3Config>,

    /// Async upload pool tuning.
    #[serde(default)]
    pub upload: UploadConfig,

    /// Local CAS and event buffer paths and limits.
    #[serde(default)]
    pub local_buffer: LocalBufferConfig,

    /// Digest cache persistence settings.
    #[serde(default)]
    pub digest_cache: DigestCacheConfig,
}

impl StorageConfig {
    /// Validate storage-specific invariants.
    ///
    /// # Errors
    ///
    /// Returns an error if S3 bucket is configured but empty, or
    /// upload concurrency is zero.
    pub fn validate(&self) -> anyhow::Result<()> {
        if let Some(s3) = &self.s3 {
            if s3.bucket.is_empty() {
                bail!("s3.bucket must not be empty when s3 is configured");
            }
            if s3.region.is_empty() {
                bail!("s3.region must not be empty when s3 is configured");
            }
        }
        if self.upload.max_concurrent == 0 {
            bail!("upload.max_concurrent must be at least 1");
        }
        Ok(())
    }
}

/// S3-compatible object store connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct S3Config {
    /// Bucket name.
    pub bucket: String,

    /// Key prefix, typically `agents/{agent_id}/`.
    #[serde(default)]
    pub prefix: String,

    /// AWS region or equivalent.
    pub region: String,

    /// Custom endpoint for S3-compatible stores (MinIO, LocalStack).
    pub endpoint: Option<String>,
}

/// Upload pool tuning knobs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadConfig {
    /// Maximum concurrent S3 uploads.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: u32,

    /// Total number of attempts per upload (1 initial + N-1 retries).
    #[serde(default = "default_max_attempts", alias = "retry_max")]
    pub max_attempts: u32,

    /// Base delay for exponential backoff between retries.
    #[serde(
        default = "default_retry_backoff_base",
        with = "humantime_serde"
    )]
    pub retry_backoff_base: Duration,
}

impl Default for UploadConfig {
    fn default() -> Self {
        Self {
            max_concurrent: default_max_concurrent(),
            max_attempts: default_max_attempts(),
            retry_backoff_base: default_retry_backoff_base(),
        }
    }
}

/// Local buffer paths and eviction limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalBufferConfig {
    /// Directory for content-addressable blobs.
    #[serde(default = "default_cas_dir")]
    pub cas_dir: PathBuf,

    /// Directory for event log segments.
    #[serde(default = "default_event_dir")]
    pub event_dir: PathBuf,

    /// Maximum total size before LRU eviction of uploaded content.
    #[serde(default = "default_max_size")]
    pub max_size: bytesize::ByteSize,

    /// Minimum time to keep content locally after upload confirmation.
    #[serde(
        default = "default_min_retention",
        with = "humantime_serde"
    )]
    pub min_retention: Duration,
}

impl Default for LocalBufferConfig {
    fn default() -> Self {
        Self {
            cas_dir: default_cas_dir(),
            event_dir: default_event_dir(),
            max_size: default_max_size(),
            min_retention: default_min_retention(),
        }
    }
}

/// Digest cache persistence and TTL settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigestCacheConfig {
    /// Path to the on-disk digest cache binary file.
    #[serde(default = "default_digest_cache_path")]
    pub path: PathBuf,

    /// Time-to-live for cached digest entries before re-verification.
    #[serde(default = "default_digest_ttl", with = "humantime_serde")]
    pub ttl: Duration,

    /// How often to upload a cache snapshot to S3.
    #[serde(
        default = "default_snapshot_interval",
        with = "humantime_serde"
    )]
    pub snapshot_interval: Duration,

    /// Whether to rebuild the cache from S3 on startup if the
    /// local file is missing.
    #[serde(default = "default_rebuild_on_start")]
    pub rebuild_on_start: bool,
}

impl Default for DigestCacheConfig {
    fn default() -> Self {
        Self {
            path: default_digest_cache_path(),
            ttl: default_digest_ttl(),
            snapshot_interval: default_snapshot_interval(),
            rebuild_on_start: default_rebuild_on_start(),
        }
    }
}

// --- Default value functions ---

fn default_max_concurrent() -> u32 {
    4
}

fn default_max_attempts() -> u32 {
    5
}

/// 1 second base for exponential backoff.
fn default_retry_backoff_base() -> Duration {
    Duration::from_secs(1)
}

fn default_cas_dir() -> PathBuf {
    PathBuf::from("/data/cas")
}

fn default_event_dir() -> PathBuf {
    PathBuf::from("/data/events")
}

/// 2 GiB local buffer cap before eviction kicks in.
fn default_max_size() -> bytesize::ByteSize {
    bytesize::ByteSize::gib(2)
}

/// Keep uploaded content locally for at least 5 minutes.
fn default_min_retention() -> Duration {
    Duration::from_secs(300)
}

fn default_digest_cache_path() -> PathBuf {
    PathBuf::from("/data/digest-cache.bin")
}

/// Re-verify cached hashes after 7 days.
fn default_digest_ttl() -> Duration {
    Duration::from_secs(7 * 24 * 3600)
}

/// Upload a cache snapshot every 10 minutes.
fn default_snapshot_interval() -> Duration {
    Duration::from_secs(600)
}

fn default_rebuild_on_start() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_defaults() {
        let cfg = StorageConfig::default();
        assert!(cfg.s3.is_none());
        assert_eq!(cfg.upload.max_concurrent, 4);
        assert_eq!(cfg.upload.max_attempts, 5);
        assert_eq!(cfg.local_buffer.cas_dir, PathBuf::from("/data/cas"));
        assert!(cfg.digest_cache.rebuild_on_start);
    }

    #[test]
    fn s3_config_round_trip() {
        let s3 = S3Config {
            bucket: "test-bucket".into(),
            prefix: "agents/a1/".into(),
            region: "us-east-1".into(),
            endpoint: Some("http://localhost:9000".into()),
        };
        let yaml = serde_yaml::to_string(&s3).unwrap();
        let parsed: S3Config = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.bucket, "test-bucket");
        assert_eq!(parsed.endpoint.unwrap(), "http://localhost:9000");
    }

    #[test]
    fn validate_rejects_empty_bucket() {
        let cfg = StorageConfig {
            s3: Some(S3Config {
                bucket: String::new(),
                prefix: String::new(),
                region: "us-west-2".into(),
                endpoint: None,
            }),
            ..StorageConfig::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("bucket"));
    }

    #[test]
    fn validate_rejects_zero_concurrency() {
        let cfg = StorageConfig {
            upload: UploadConfig {
                max_concurrent: 0,
                ..UploadConfig::default()
            },
            ..StorageConfig::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("max_concurrent"));
    }

    #[test]
    fn validate_passes_without_s3() {
        let cfg = StorageConfig::default();
        cfg.validate().unwrap();
    }
}
