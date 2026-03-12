//! S3-compatible object store client and trait.
//!
//! Provides [`ObjectStore`] as a trait for testability, and [`S3Client`]
//! as the production implementation backed by `aws-sdk-s3`. Key paths
//! follow the layout defined in the project architecture doc:
//! `{prefix}cas/{hash[0:2]}/{hash[2:]}` for CAS objects, etc.

use std::sync::Arc;

use anyhow::{Context, Result};
use aws_sdk_s3::primitives::ByteStream;

use crate::cas::ContentHash;
use crate::config::S3Config;

/// Async object storage abstraction.
///
/// Enables swapping the real S3 backend for a mock in tests.
/// All keys are relative to the configured bucket/prefix.
///
/// Implementors must be `Send + Sync` so that the upload pool can
/// share them across tokio tasks.
pub trait ObjectStore: Send + Sync + 'static {
    /// Upload bytes to the given key.
    ///
    /// # Errors
    ///
    /// Returns an error if the upload fails (network, permissions, etc).
    fn put(&self, key: &str, data: Vec<u8>) -> impl Future<Output = Result<()>> + Send;

    /// Download bytes from the given key.
    ///
    /// # Errors
    ///
    /// Returns an error if the key does not exist or download fails.
    fn get(&self, key: &str) -> impl Future<Output = Result<Vec<u8>>> + Send;

    /// Check whether a key exists.
    ///
    /// # Errors
    ///
    /// Returns an error on transient failures.
    fn exists(&self, key: &str) -> impl Future<Output = Result<bool>> + Send;

    /// List all keys under a prefix, paginating automatically.
    ///
    /// # Errors
    ///
    /// Returns an error on transient failures.
    fn list(&self, prefix: &str) -> impl Future<Output = Result<Vec<String>>> + Send;
}

/// S3-compatible object store client.
///
/// Wraps the AWS SDK client and applies the configured bucket and
/// key prefix to all operations.
#[derive(Debug, Clone)]
pub struct S3Client {
    inner: aws_sdk_s3::Client,
    bucket: String,
    prefix: String,
}

impl S3Client {
    /// Create a new client from [`S3Config`].
    ///
    /// Configures the AWS SDK with region and optional custom endpoint
    /// (for MinIO, LocalStack, etc).
    ///
    /// # Errors
    ///
    /// Returns an error if the AWS SDK configuration fails.
    pub async fn new(config: &S3Config) -> Result<Self> {
        let http_client = aws_smithy_http_client::Builder::new()
            .tls_provider(aws_smithy_http_client::tls::Provider::Rustls(
                aws_smithy_http_client::tls::rustls_provider::CryptoMode::Ring,
            ))
            .build_https();

        let mut sdk_config = aws_config::from_env()
            .http_client(http_client.clone())
            .region(aws_sdk_s3::config::Region::new(config.region.clone()));

        if let Some(endpoint) = &config.endpoint {
            sdk_config = sdk_config.endpoint_url(endpoint);
        }

        let sdk_config = sdk_config.load().await;

        // Force path-style when a custom endpoint is set, since virtual-hosted
        // style does not work with most S3-compatible stores.
        let s3_config = aws_sdk_s3::config::Builder::from(&sdk_config)
            .force_path_style(config.endpoint.is_some())
            .http_client(http_client)
            .build();

        let inner = aws_sdk_s3::Client::from_conf(s3_config);

        Ok(Self {
            inner,
            bucket: config.bucket.clone(),
            prefix: config.prefix.clone(),
        })
    }

    fn full_key(&self, key: &str) -> String {
        format!("{}{}", self.prefix, key)
    }

    /// Build the CAS key path for a content hash.
    pub fn cas_key(hash: &ContentHash) -> String {
        format!(
            "cas/{}/{}/{}",
            hash.algorithm_dir(),
            hash.prefix(),
            hash.suffix()
        )
    }

    /// Build the event segment key path.
    pub fn event_segment_key(agent_id: &str, segment_seq: u64) -> String {
        format!("events/{agent_id}/{segment_seq}.jsonl")
    }

    /// Build the checkpoint key path.
    pub fn checkpoint_key(agent_id: &str, seq: u64) -> String {
        format!("checkpoints/{agent_id}/{seq}.bin")
    }

    /// Build the digest cache snapshot key path.
    pub fn digest_cache_key(agent_id: &str) -> String {
        format!("meta/{agent_id}/digest-cache-latest.bin")
    }
}

impl ObjectStore for S3Client {
    async fn put(&self, key: &str, data: Vec<u8>) -> Result<()> {
        let full_key = self.full_key(key);
        self.inner
            .put_object()
            .bucket(&self.bucket)
            .key(&full_key)
            .body(ByteStream::from(data))
            .send()
            .await
            .with_context(|| format!("S3 PUT failed for key {full_key}"))?;
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>> {
        let full_key = self.full_key(key);
        let resp = self
            .inner
            .get_object()
            .bucket(&self.bucket)
            .key(&full_key)
            .send()
            .await
            .with_context(|| format!("S3 GET failed for key {full_key}"))?;

        let bytes = resp
            .body
            .collect()
            .await
            .with_context(|| format!("failed reading body for {full_key}"))?
            .into_bytes()
            .to_vec();

        Ok(bytes)
    }

    async fn exists(&self, key: &str) -> Result<bool> {
        let full_key = self.full_key(key);
        let result = self
            .inner
            .head_object()
            .bucket(&self.bucket)
            .key(&full_key)
            .send()
            .await;

        match result {
            Ok(_) => Ok(true),
            Err(err) => {
                if err.as_service_error().is_some_and(|e| e.is_not_found()) {
                    Ok(false)
                } else {
                    Err(err)
                        .with_context(|| format!("S3 HEAD failed for key {full_key}"))
                }
            }
        }
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let full_prefix = self.full_key(prefix);
        let mut keys = Vec::new();
        let mut continuation_token: Option<String> = None;

        loop {
            let mut req = self
                .inner
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&full_prefix);

            if let Some(token) = continuation_token.take() {
                req = req.continuation_token(token);
            }

            let resp = req
                .send()
                .await
                .with_context(|| {
                    format!("S3 LIST failed for prefix {full_prefix}")
                })?;

            if let Some(contents) = resp.contents {
                for obj in contents {
                    if let Some(key) = obj.key {
                        keys.push(key);
                    }
                }
            }

            if resp.is_truncated == Some(true) {
                continuation_token = resp.next_continuation_token;
            } else {
                break;
            }
        }

        Ok(keys)
    }
}

impl<T: ObjectStore> ObjectStore for Arc<T> {
    async fn put(&self, key: &str, data: Vec<u8>) -> Result<()> {
        (**self).put(key, data).await
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>> {
        (**self).get(key).await
    }

    async fn exists(&self, key: &str) -> Result<bool> {
        (**self).exists(key).await
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>> {
        (**self).list(prefix).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cas_key_uses_hash_prefix_suffix() {
        let hash = ContentHash::from_data(b"hello");
        let key = S3Client::cas_key(&hash);
        assert_eq!(
            key,
            format!("cas/{}/{}/{}", hash.algorithm_dir(), hash.prefix(), hash.suffix())
        );
    }

    #[test]
    fn event_segment_key_format() {
        let key = S3Client::event_segment_key("agent-1", 42);
        assert_eq!(key, "events/agent-1/42.jsonl");
    }

    #[test]
    fn checkpoint_key_format() {
        let key = S3Client::checkpoint_key("agent-1", 7);
        assert_eq!(key, "checkpoints/agent-1/7.bin");
    }

    #[test]
    fn digest_cache_key_format() {
        let key = S3Client::digest_cache_key("agent-1");
        assert_eq!(key, "meta/agent-1/digest-cache-latest.bin");
    }
}
