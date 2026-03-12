//! In-memory content-addressable store.
//!
//! Backed by `DashMap` for lock-free concurrent access. Usable in tests
//! and as a hot cache tier in front of disk or remote storage.

use anyhow::{Result, anyhow};
use dashmap::DashMap;

use super::hash::ContentHash;
use super::stats::BackendStats;
use super::traits::{Cas, CasBackend};

/// In-memory CAS backed by a concurrent hash map.
///
/// Thread-safe without explicit locking. Suitable for tests and as a
/// hot cache tier in [`TieredCas`](super::tiered::TieredCas) composition.
#[derive(Debug, Default)]
pub struct MemoryCas {
    store: DashMap<ContentHash, Vec<u8>>,
}

impl MemoryCas {
    /// Create an empty in-memory store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl Cas for MemoryCas {
    fn get(&self, hash: &ContentHash) -> Result<Vec<u8>> {
        self.store
            .get(hash)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| anyhow!("not found: {hash}"))
    }

    fn put(&self, content: &[u8]) -> Result<ContentHash> {
        let hash = ContentHash::from_data(content);
        self.store.insert(hash.clone(), content.to_vec());
        Ok(hash)
    }

    fn exists(&self, hash: &ContentHash) -> Result<bool> {
        Ok(self.store.contains_key(hash))
    }
}

impl CasBackend for MemoryCas {
    fn delete(&self, hash: &ContentHash) -> Result<()> {
        self.store
            .remove(hash)
            .ok_or_else(|| anyhow!("not found: {hash}"))?;
        Ok(())
    }

    fn stats(&self) -> BackendStats {
        let mut total_bytes = 0u64;
        let mut count = 0u64;
        for entry in &self.store {
            count += 1;
            total_bytes += entry.value().len() as u64;
        }
        BackendStats {
            object_count: count,
            total_bytes,
        }
    }
}

// Rust guideline compliant 2026-02-21
