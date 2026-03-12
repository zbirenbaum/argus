//! Remote content-addressable store backed by object storage.
//!
//! Wraps a [`DynObjectStore`] (S3, GCS, Azure, MinIO, etc.) with CAS
//! semantics: keys are derived from the content hash using the layout
//! `cas/{algorithm}/{digest[0:2]}/{digest[2:]}`.

use anyhow::{Context, Result};

use crate::storage::object_store_dyn::DynObjectStore;

use super::hash::ContentHash;
use super::stats::BackendStats;
use super::traits::{Cas, CasBackend};

/// CAS backed by remote object storage.
///
/// The specific provider (S3, GCS, Azure) is a configuration detail
/// hidden behind [`DynObjectStore`]. This type adds hash-addressed
/// put/get semantics on top.
#[derive(Debug, Clone)]
pub struct RemoteCas {
    backend: DynObjectStore,
}

impl RemoteCas {
    /// Wrap an object store backend with CAS semantics.
    pub fn new(backend: DynObjectStore) -> Self {
        Self { backend }
    }

    /// Build the object key for a content hash.
    fn object_key(hash: &ContentHash) -> String {
        format!(
            "cas/{}/{}/{}",
            hash.algorithm_dir(),
            hash.prefix(),
            hash.suffix()
        )
    }
}

impl Cas for RemoteCas {
    fn get(&self, hash: &ContentHash) -> Result<Vec<u8>> {
        let key = Self::object_key(hash);
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(self.backend.get(&key))
                .with_context(|| format!("remote CAS get {hash}"))
        })
    }

    fn put(&self, content: &[u8]) -> Result<ContentHash> {
        let hash = ContentHash::from_data(content);
        let key = Self::object_key(&hash);
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(self.backend.put(&key, content.to_vec()))
                .with_context(|| format!("remote CAS put {hash}"))?;
            Ok(hash)
        })
    }

    fn exists(&self, hash: &ContentHash) -> Result<bool> {
        let key = Self::object_key(hash);
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(self.backend.exists(&key))
                .with_context(|| format!("remote CAS exists {hash}"))
        })
    }
}

impl CasBackend for RemoteCas {
    fn delete(&self, hash: &ContentHash) -> Result<()> {
        // Remote stores rely on lifecycle rules for eviction.
        // Explicit delete is a no-op — the object remains until
        // the bucket policy expires it.
        let _ = hash;
        Ok(())
    }

    fn stats(&self) -> BackendStats {
        // Remote stats require listing, which is expensive.
        // Return zeroes; monitoring uses S3 bucket metrics.
        BackendStats::default()
    }
}

// Rust guideline compliant 2026-02-21
