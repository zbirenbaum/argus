//! In-memory content-addressable store for testing.
//!
//! Backed by a `RwLock<HashMap>` so tests can exercise CAS-dependent
//! code without touching the filesystem.

use std::collections::HashMap;
use std::sync::RwLock;

use anyhow::{Result, anyhow};

use super::hash::ContentHash;
use super::traits::Cas;

/// In-memory CAS backed by a locked hash map.
///
/// Thread-safe via `RwLock`. Intended for unit tests only — not
/// optimized for production workloads.
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

// Rust guideline compliant 2026-02-21
