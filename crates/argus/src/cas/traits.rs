//! Trait defining the content-addressable storage contract.
//!
//! [`Cas`] is the read/write interface every backend implements.
//! [`CasBackend`] extends it with eviction and stats for backends
//! that participate in tiered caching via [`TieredCas`](super::cached::TieredCas).

use anyhow::Result;

use super::hash::ContentHash;
use super::stats::BackendStats;

/// Content-addressable storage backend.
///
/// Implementations must be safe to share across threads. Content is
/// keyed by its SHA-256 digest — identical content always produces
/// the same key.
pub trait Cas: Send + Sync {
    /// Retrieve content by its hash.
    ///
    /// # Errors
    ///
    /// Returns an error if the object does not exist or cannot be read.
    fn get(&self, hash: &ContentHash) -> Result<Vec<u8>>;

    /// Hash and store content, returning the content hash.
    ///
    /// Storing identical content twice is idempotent.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    fn put(&self, content: &[u8]) -> Result<ContentHash>;

    /// Check whether an object exists in the store.
    ///
    /// # Errors
    ///
    /// Returns an error if the existence check itself fails.
    fn exists(&self, hash: &ContentHash) -> Result<bool>;
}

impl<T: Cas> Cas for std::sync::Arc<T> {
    fn get(&self, hash: &ContentHash) -> Result<Vec<u8>> {
        (**self).get(hash)
    }

    fn put(&self, content: &[u8]) -> Result<ContentHash> {
        (**self).put(content)
    }

    fn exists(&self, hash: &ContentHash) -> Result<bool> {
        (**self).exists(hash)
    }
}

/// CAS with eviction and observability.
///
/// Extends [`Cas`] with `delete` for eviction and `stats` for
/// monitoring. Backends that participate in [`TieredCas`](super::cached::TieredCas)
/// composition must implement this trait.
pub trait CasBackend: Cas {
    /// Remove an object from the store.
    ///
    /// # Errors
    ///
    /// Returns an error if the object does not exist or removal fails.
    fn delete(&self, hash: &ContentHash) -> Result<()>;

    /// Point-in-time storage statistics.
    fn stats(&self) -> BackendStats;
}

impl<T: CasBackend> CasBackend for std::sync::Arc<T> {
    fn delete(&self, hash: &ContentHash) -> Result<()> {
        (**self).delete(hash)
    }

    fn stats(&self) -> BackendStats {
        (**self).stats()
    }
}

// Rust guideline compliant 2026-02-21
