//! Trait defining the content-addressable storage contract.
//!
//! All CAS implementations (local filesystem, in-memory, S3) implement
//! this trait so storage consumers are provider-agnostic.

use anyhow::Result;

use super::hash::ContentHash;

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

// Rust guideline compliant 2026-02-21
