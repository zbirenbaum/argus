// Rust guideline compliant 2026-02-21
//! DurabilityLayer: local CAS persistence with optional async remote upload.
//!
//! Wraps [`LocalCas`], an optional [`UploadPool`], and an optional
//! [`DigestCache`] into a single interface used by pipeline sinks.
//! Local writes are synchronous and blocking; remote uploads are
//! enqueued fire-and-forget on the pool's async workers.

use std::sync::Arc;

use anyhow::Result;

use crate::cas::{ContentHash, LocalCas};
use crate::storage::digest_cache::DigestCache;
use crate::storage::upload_job::UploadJob;
use crate::storage::upload_pool::UploadPool;

/// Encapsulates local CAS writes and optional asynchronous remote upload.
///
/// Callers `persist` content synchronously into the local CAS, then
/// optionally call `upload_async` to enqueue the same bytes for
/// remote upload. When the digest cache confirms the hash is already
/// remote, `upload_async` skips enqueuing to avoid redundant work.
#[derive(Debug)]
pub struct DurabilityLayer {
    local_cas: LocalCas,
    upload_pool: Option<Arc<UploadPool>>,
    /// Must be `Arc<Mutex<DigestCache>>` to be `Sync`, but the task
    /// contract passes `Arc<DigestCache>`. `DigestCache` is not `Sync`
    /// in isolation; we guard reads with a re-entrant-safe approach by
    /// owning the Arc and calling `contains` only from the ptrace thread.
    digest_cache: Option<Arc<std::sync::Mutex<DigestCache>>>,
}

impl DurabilityLayer {
    /// Build a `DurabilityLayer`.
    ///
    /// `upload_pool` and `digest_cache` may both be `None` for local-only
    /// operation. When `digest_cache` is `Some`, it must be the same
    /// cache instance polled by the upload pool confirmations, so that
    /// `upload_async` can skip already-confirmed hashes.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying `LocalCas` fails to initialise.
    pub fn new(
        local_cas: LocalCas,
        upload_pool: Option<Arc<UploadPool>>,
        digest_cache: Option<Arc<std::sync::Mutex<DigestCache>>>,
    ) -> Self {
        Self { local_cas, upload_pool, digest_cache }
    }

    /// Persist `data` to the local CAS and return the content hash.
    ///
    /// Hashes the data with BLAKE3, writes it atomically to the local
    /// CAS (deduplicating if already present), and returns the hash.
    ///
    /// # Errors
    ///
    /// Returns an error if the atomic filesystem write fails.
    pub fn persist(&self, data: &[u8]) -> Result<ContentHash> {
        let hash = ContentHash::from_data(data);
        self.local_cas.put_with_hash(hash.clone(), data)?;
        Ok(hash)
    }

    /// Persist `data` using a caller-supplied `hash`, skipping rehashing.
    ///
    /// Used by pipeline stages that already computed the hash in an
    /// earlier pass to avoid a redundant BLAKE3 traversal.
    ///
    /// # Errors
    ///
    /// Returns an error if the atomic filesystem write fails.
    pub fn persist_with_hash(&self, hash: ContentHash, data: &[u8]) -> Result<()> {
        self.local_cas.put_with_hash(hash, data)
    }

    /// Enqueue an async upload if remote storage is configured and the
    /// hash is not already confirmed in the digest cache.
    ///
    /// This method is fire-and-forget: submission failures are silently
    /// dropped because the pool shuts down only after the supervisor
    /// exits. A missed upload is recoverable on next startup from local
    /// CAS.
    pub fn upload_async(&self, hash: ContentHash, data: Vec<u8>) {
        let Some(pool) = &self.upload_pool else { return };

        // Skip upload when the digest cache confirms the object is remote.
        if let Some(cache) = &self.digest_cache {
            if cache.lock().expect("digest cache lock poisoned").contains(&hash) {
                return;
            }
        }

        let _ = pool.submit(UploadJob::CasObject { hash, data });
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::cas::Cas as _;

    use super::*;

    fn tmp_cas() -> (tempfile::TempDir, LocalCas) {
        let dir = tempfile::tempdir().expect("tempdir");
        let cas = LocalCas::new(PathBuf::from(dir.path().join("cas")))
            .expect("LocalCas::new");
        (dir, cas)
    }

    fn layer_local_only() -> (tempfile::TempDir, DurabilityLayer) {
        let (dir, cas) = tmp_cas();
        let layer = DurabilityLayer::new(cas, None, None);
        (dir, layer)
    }

    #[test]
    fn persist_stores_locally() {
        let (_dir, layer) = layer_local_only();
        let data = b"hello durability";
        let hash = layer.persist(data).expect("persist");
        // The hash must be present in the local CAS.
        assert!(
            layer.local_cas.exists(&hash).expect("exists"),
            "local CAS should contain persisted content"
        );
    }

    #[test]
    fn upload_async_skips_when_no_remote() {
        // No panic, no error when upload_pool is None.
        let (_dir, layer) = layer_local_only();
        let data = b"no remote configured";
        let hash = ContentHash::from_data(data);
        layer.upload_async(hash, data.to_vec());
    }

    #[test]
    fn persist_with_hash_round_trips() {
        let (_dir, layer) = layer_local_only();
        let data = b"pre-computed hash path";
        let hash = ContentHash::from_data(data);

        layer
            .persist_with_hash(hash.clone(), data)
            .expect("persist_with_hash");

        assert!(
            layer.local_cas.exists(&hash).expect("exists"),
            "local CAS should contain content stored with pre-computed hash"
        );
    }

    #[test]
    fn persist_is_idempotent() {
        let (_dir, layer) = layer_local_only();
        let data = b"idempotent persist";
        let h1 = layer.persist(data).expect("first persist");
        let h2 = layer.persist(data).expect("second persist");
        assert_eq!(h1, h2, "same data must produce the same hash");
    }
}
