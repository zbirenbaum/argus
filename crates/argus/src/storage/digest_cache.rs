//! Tracks content hashes known to exist in remote storage (S3).
//!
//! Avoids redundant uploads by maintaining a TTL-bounded set of
//! hashes that have been confirmed present in the remote CAS.
//! The cache is periodically persisted to disk using bincode and
//! reloaded on startup.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::cas::ContentHash;

/// Matches the spec's recommended remote-object TTL before re-verification.
const DEFAULT_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Record of a single content hash confirmed in remote storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigestEntry {
    pub size_bytes: u64,
    pub uploaded_at: SystemTime,
    pub ttl: Duration,
}

impl DigestEntry {
    fn is_expired(&self, now: SystemTime) -> bool {
        now.duration_since(self.uploaded_at)
            .map(|elapsed| elapsed >= self.ttl)
            .unwrap_or(true)
    }
}

/// Aggregate statistics for the digest cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestCacheStats {
    pub total_entries: usize,
    pub total_bytes: u64,
    pub expired_entries: usize,
}

/// In-memory set of content hashes known to exist remotely.
///
/// Entries expire after a configurable TTL, forcing re-verification
/// with the remote store. The cache serializes to disk via bincode
/// for fast startup recovery.
///
/// # Thread Safety
///
/// This type is **not internally synchronized**. Callers sharing a
/// `DigestCache` across threads or async tasks must wrap it in
/// `Arc<Mutex<DigestCache>>` (or equivalent).
#[derive(Debug, Serialize, Deserialize)]
pub struct DigestCache {
    known_remote: HashMap<ContentHash, DigestEntry>,
    cache_file: PathBuf,
}

impl DigestCache {
    /// Create an empty cache that will persist to `cache_file`.
    pub fn new(cache_file: PathBuf) -> Self {
        Self {
            known_remote: HashMap::new(),
            cache_file,
        }
    }

    /// Check if `hash` is present and not expired.
    pub fn contains(&self, hash: &ContentHash) -> bool {
        self.known_remote
            .get(hash)
            .is_some_and(|entry| !entry.is_expired(SystemTime::now()))
    }

    /// Record a hash as present remotely with the default TTL.
    pub fn insert(&mut self, hash: ContentHash, size_bytes: u64) {
        self.insert_with_ttl(hash, size_bytes, DEFAULT_TTL);
    }

    /// Record a hash as present remotely with a custom TTL.
    pub fn insert_with_ttl(
        &mut self,
        hash: ContentHash,
        size_bytes: u64,
        ttl: Duration,
    ) {
        self.known_remote.insert(
            hash,
            DigestEntry {
                size_bytes,
                uploaded_at: SystemTime::now(),
                ttl,
            },
        );
    }

    /// Explicitly remove a hash from the cache.
    pub fn remove(&mut self, hash: &ContentHash) {
        self.known_remote.remove(hash);
    }

    /// Remove all expired entries, returning how many were pruned.
    pub fn prune_expired(&mut self) -> usize {
        let now = SystemTime::now();
        let before = self.known_remote.len();
        self.known_remote.retain(|_, entry| !entry.is_expired(now));
        before - self.known_remote.len()
    }

    /// Number of entries (including expired but not yet pruned).
    pub fn len(&self) -> usize {
        self.known_remote.len()
    }

    /// Whether the cache contains zero entries.
    pub fn is_empty(&self) -> bool {
        self.known_remote.is_empty()
    }

    /// Compute aggregate statistics over the cache.
    pub fn stats(&self) -> DigestCacheStats {
        let now = SystemTime::now();
        let mut total_bytes: u64 = 0;
        let mut expired_entries: usize = 0;

        for entry in self.known_remote.values() {
            total_bytes = total_bytes.saturating_add(entry.size_bytes);
            if entry.is_expired(now) {
                expired_entries += 1;
            }
        }

        DigestCacheStats {
            total_entries: self.known_remote.len(),
            total_bytes,
            expired_entries,
        }
    }

    /// Serialize the cache to its configured disk path.
    ///
    /// Uses an atomic write pattern (write to temp, then rename) to
    /// prevent corruption if the process is interrupted mid-write.
    pub fn save_to_disk(&self) -> Result<()> {
        let data = bincode::serialize(self)
            .context("serialize digest cache")?;

        if let Some(parent) = self.cache_file.parent() {
            std::fs::create_dir_all(parent)
                .context("create digest cache directory")?;
        }

        let tmp_path = self.cache_file.with_extension("bin.tmp");
        std::fs::write(&tmp_path, &data)
            .context("write digest cache temp file")?;
        std::fs::rename(&tmp_path, &self.cache_file)
            .context("rename digest cache into place")?;

        Ok(())
    }

    /// Load a previously persisted cache from disk.
    ///
    /// Returns an error if the file exists but is corrupt. Callers
    /// that want to fall back to an empty cache on missing files
    /// should check for `io::ErrorKind::NotFound` themselves.
    pub fn load_from_disk(cache_file: &Path) -> Result<Self> {
        let data = std::fs::read(cache_file)
            .context("read digest cache file")?;
        let mut cache: Self = bincode::deserialize(&data)
            .context("deserialize digest cache")?;
        cache.cache_file = cache_file.to_path_buf();
        Ok(cache)
    }

    /// Load a cache from disk, falling back to an empty cache on any error.
    ///
    /// Convenience wrapper around [`load_from_disk`](Self::load_from_disk)
    /// that returns a fresh cache when the file is missing or corrupt.
    pub fn load_or_default(cache_file: &Path, ttl: Duration) -> Self {
        let _ = ttl; // reserved for future per-cache default TTL
        Self::load_from_disk(cache_file)
            .unwrap_or_else(|_| Self::new(cache_file.to_path_buf()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_cache_path() -> (PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("digest-cache.bin");
        (path, dir)
    }

    fn make_hash(data: &[u8]) -> ContentHash {
        ContentHash::from_data(data)
    }

    #[test]
    fn contains_returns_false_for_unknown_hash() {
        let (path, _dir) = temp_cache_path();
        let cache = DigestCache::new(path);
        assert!(!cache.contains(&make_hash(b"unknown")));
    }

    #[test]
    fn insert_then_contains_returns_true() {
        let (path, _dir) = temp_cache_path();
        let mut cache = DigestCache::new(path);
        let h = make_hash(b"known");
        cache.insert(h.clone(), 42);
        assert!(cache.contains(&h));
    }

    #[test]
    fn remove_then_contains_returns_false() {
        let (path, _dir) = temp_cache_path();
        let mut cache = DigestCache::new(path);
        let h = make_hash(b"removeme");
        cache.insert(h.clone(), 10);
        cache.remove(&h);
        assert!(!cache.contains(&h));
    }

    #[test]
    fn expired_entry_is_not_contained() {
        let (path, _dir) = temp_cache_path();
        let mut cache = DigestCache::new(path);
        let h = make_hash(b"expiring");
        cache.insert_with_ttl(h.clone(), 5, Duration::ZERO);
        assert!(!cache.contains(&h));
    }

    #[test]
    fn prune_expired_removes_stale_entries() {
        let (path, _dir) = temp_cache_path();
        let mut cache = DigestCache::new(path);
        cache.insert_with_ttl(
            make_hash(b"stale"),
            1,
            Duration::ZERO,
        );
        cache.insert(make_hash(b"fresh"), 2);
        let pruned = cache.prune_expired();
        assert_eq!(pruned, 1);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn save_and_load_round_trip() {
        let (path, _dir) = temp_cache_path();
        let mut cache = DigestCache::new(path.clone());
        let h = make_hash(b"persist");
        cache.insert(h.clone(), 999);
        cache.save_to_disk().expect("save");

        let loaded = DigestCache::load_from_disk(&path).expect("load");
        assert!(loaded.contains(&h));
        assert_eq!(loaded.len(), 1);
    }

    #[test]
    fn stats_returns_correct_counts() {
        let (path, _dir) = temp_cache_path();
        let mut cache = DigestCache::new(path);
        cache.insert(make_hash(b"a"), 100);
        cache.insert(make_hash(b"b"), 200);
        cache.insert_with_ttl(
            make_hash(b"c"),
            50,
            Duration::ZERO,
        );

        let s = cache.stats();
        assert_eq!(s.total_entries, 3);
        assert_eq!(s.total_bytes, 350);
        assert_eq!(s.expired_entries, 1);
    }

    #[test]
    fn empty_cache_len_and_save_load() {
        let (path, _dir) = temp_cache_path();
        let cache = DigestCache::new(path.clone());
        assert_eq!(cache.len(), 0);
        assert!(cache.is_empty());

        cache.save_to_disk().expect("save empty");
        let loaded = DigestCache::load_from_disk(&path).expect("load empty");
        assert!(loaded.is_empty());
    }

    #[test]
    fn load_missing_file_returns_error() {
        let result =
            DigestCache::load_from_disk(Path::new("/nonexistent/path.bin"));
        assert!(result.is_err());
    }
}
