//! In-memory content-addressable store.
//!
//! Backed by a `RwLock<HashMap>` so tests can exercise CAS-dependent
//! code without touching the filesystem. Also usable as a hot cache
//! tier in front of disk or remote storage.

use std::collections::HashMap;
use std::sync::RwLock;

use anyhow::{Result, anyhow};

use super::hash::ContentHash;
use super::stats::BackendStats;
use super::traits::{Cas, CasBackend};

/// In-memory CAS backed by a locked hash map.
///
/// Thread-safe via `RwLock`. Suitable for tests and as a hot cache
/// tier in [`TieredCas`](super::tiered::TieredCas) composition.
#[derive(Debug, Default)]
pub struct MemoryCas {
    store: RwLock<HashMap<ContentHash, Vec<u8>>>,
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
            .read()
            .expect("MemoryCas lock poisoned")
            .get(hash)
            .cloned()
            .ok_or_else(|| anyhow!("not found: {hash}"))
    }

    fn put(&self, content: &[u8]) -> Result<ContentHash> {
        let hash = ContentHash::from_data(content);
        self.store
            .write()
            .expect("MemoryCas lock poisoned")
            .insert(hash.clone(), content.to_vec());
        Ok(hash)
    }

    fn exists(&self, hash: &ContentHash) -> Result<bool> {
        Ok(self
            .store
            .read()
            .expect("MemoryCas lock poisoned")
            .contains_key(hash))
    }
}

impl CasBackend for MemoryCas {
    fn delete(&self, hash: &ContentHash) -> Result<()> {
        self.store
            .write()
            .expect("MemoryCas lock poisoned")
            .remove(hash)
            .ok_or_else(|| anyhow!("not found: {hash}"))?;
        Ok(())
    }

    fn stats(&self) -> BackendStats {
        let map = self.store.read().expect("MemoryCas lock poisoned");
        BackendStats {
            object_count: map.len() as u64,
            total_bytes: map.values().map(|v| v.len() as u64).sum(),
        }
    }
}

// Rust guideline compliant 2026-02-21
