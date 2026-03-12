//! Generic two-tier CAS: fast front cache backed by slow store.
//!
//! `put` writes to the front tier only. Flushing to the back tier
//! is the caller's responsibility (e.g. an async upload pool).
//! `get` tries the front first, falls back to the back, and
//! backfills the front on a hit so subsequent reads are fast.

use std::sync::Arc;

use anyhow::Result;

use super::hash::ContentHash;
use super::stats::BackendStats;
use super::traits::{Cas, CasBackend};

/// Two-tier CAS with front-cache and back-store.
///
/// Both tiers are `Arc`-wrapped so the struct is cheap to clone.
/// The ptrace thread and upload workers can each hold a clone
/// without extra synchronization.
#[derive(Debug)]
pub struct TieredCas<Front: CasBackend, Back: CasBackend> {
    front: Arc<Front>,
    back: Arc<Back>,
}

// Manual Clone: the derive would add `Front: Clone, Back: Clone` bounds,
// but we only need `Arc<T>: Clone` which is always true.
impl<Front: CasBackend, Back: CasBackend> Clone for TieredCas<Front, Back> {
    fn clone(&self) -> Self {
        Self {
            front: Arc::clone(&self.front),
            back: Arc::clone(&self.back),
        }
    }
}

impl<Front: CasBackend, Back: CasBackend> TieredCas<Front, Back> {
    /// Compose two backends into a cached tier.
    pub fn new(front: Arc<Front>, back: Arc<Back>) -> Self {
        Self { front, back }
    }

    /// Direct access to the front tier.
    pub fn front(&self) -> &Front {
        &self.front
    }

    /// Direct access to the back tier.
    pub fn back(&self) -> &Back {
        &self.back
    }
}

impl<Front: CasBackend, Back: CasBackend> Cas for TieredCas<Front, Back> {
    fn get(&self, hash: &ContentHash) -> Result<Vec<u8>> {
        if let Ok(data) = self.front.get(hash) {
            return Ok(data);
        }
        let data = self.back.get(hash)?;
        let _ = self.front.put(&data);
        Ok(data)
    }

    fn put(&self, content: &[u8]) -> Result<ContentHash> {
        self.front.put(content)
    }

    fn exists(&self, hash: &ContentHash) -> Result<bool> {
        if self.front.exists(hash)? {
            return Ok(true);
        }
        self.back.exists(hash)
    }
}

impl<Front: CasBackend, Back: CasBackend> CasBackend for TieredCas<Front, Back> {
    fn delete(&self, hash: &ContentHash) -> Result<()> {
        self.front.delete(hash)
    }

    fn stats(&self) -> BackendStats {
        self.front.stats()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cas::MemoryCas;

    #[test]
    fn put_writes_to_front_only() {
        let front = Arc::new(MemoryCas::new());
        let back = Arc::new(MemoryCas::new());
        let cached = TieredCas::new(front.clone(), back.clone());

        let hash = cached.put(b"hello").unwrap();
        assert!(front.exists(&hash).unwrap());
        assert!(!back.exists(&hash).unwrap());
    }

    #[test]
    fn get_from_front() {
        let front = Arc::new(MemoryCas::new());
        let back = Arc::new(MemoryCas::new());
        let cached = TieredCas::new(front.clone(), back.clone());

        let hash = cached.put(b"local data").unwrap();
        let data = cached.get(&hash).unwrap();
        assert_eq!(data, b"local data");
    }

    #[test]
    fn get_falls_back_to_back() {
        let front = Arc::new(MemoryCas::new());
        let back = Arc::new(MemoryCas::new());

        let hash = back.put(b"remote only").unwrap();
        let cached = TieredCas::new(front.clone(), back.clone());

        assert!(!front.exists(&hash).unwrap());
        let data = cached.get(&hash).unwrap();
        assert_eq!(data, b"remote only");
    }

    #[test]
    fn get_backfills_front_on_miss() {
        let front = Arc::new(MemoryCas::new());
        let back = Arc::new(MemoryCas::new());

        let hash = back.put(b"hello").unwrap();
        let cached = TieredCas::new(front.clone(), back.clone());

        assert!(!front.exists(&hash).unwrap());
        let data = cached.get(&hash).unwrap();
        assert_eq!(data, b"hello");
        assert!(front.exists(&hash).unwrap());
    }

    #[test]
    fn get_missing_from_both_errors() {
        let front = Arc::new(MemoryCas::new());
        let back = Arc::new(MemoryCas::new());
        let cached = TieredCas::new(front, back);

        let hash = ContentHash::from_data(b"ghost");
        assert!(cached.get(&hash).is_err());
    }

    #[test]
    fn exists_checks_front_first() {
        let front = Arc::new(MemoryCas::new());
        let back = Arc::new(MemoryCas::new());
        let cached = TieredCas::new(front.clone(), back.clone());

        let hash = cached.put(b"exists local").unwrap();
        assert!(cached.exists(&hash).unwrap());
    }

    #[test]
    fn exists_falls_back_to_back() {
        let front = Arc::new(MemoryCas::new());
        let back = Arc::new(MemoryCas::new());

        let hash = back.put(b"exists remote").unwrap();
        let cached = TieredCas::new(front, back);
        assert!(cached.exists(&hash).unwrap());
    }

    #[test]
    fn exists_false_when_missing() {
        let front = Arc::new(MemoryCas::new());
        let back = Arc::new(MemoryCas::new());
        let cached = TieredCas::new(front, back);

        let hash = ContentHash::from_data(b"nope");
        assert!(!cached.exists(&hash).unwrap());
    }

    #[test]
    fn delete_removes_from_front() {
        let front = Arc::new(MemoryCas::new());
        let back = Arc::new(MemoryCas::new());
        let cached = TieredCas::new(front.clone(), back.clone());

        let hash = cached.put(b"evict me").unwrap();
        assert!(front.exists(&hash).unwrap());

        cached.delete(&hash).unwrap();
        assert!(!front.exists(&hash).unwrap());
    }

    #[test]
    fn stats_reflects_front() {
        let front = Arc::new(MemoryCas::new());
        let back = Arc::new(MemoryCas::new());
        let cached = TieredCas::new(front.clone(), back.clone());

        cached.put(b"aaa").unwrap();
        cached.put(b"bbbbb").unwrap();

        let s = cached.stats();
        assert_eq!(s.object_count, 2);
        assert_eq!(s.total_bytes, 8);
    }

    #[test]
    fn nested_composition() {
        let inner_front = Arc::new(MemoryCas::new());
        let inner_back = Arc::new(MemoryCas::new());
        let inner = Arc::new(TieredCas::new(inner_front.clone(), inner_back.clone()));

        let outer_front = Arc::new(MemoryCas::new());
        let outer = TieredCas::new(outer_front.clone(), inner.clone());

        // Put into inner back (simulating S3 content)
        let hash = inner_back.put(b"deep").unwrap();

        // Outer miss → inner miss on front → inner hit on back → backfill both
        assert!(!outer_front.exists(&hash).unwrap());
        assert!(!inner_front.exists(&hash).unwrap());

        let data = outer.get(&hash).unwrap();
        assert_eq!(data, b"deep");

        // Inner front got backfilled by inner TieredCas
        assert!(inner_front.exists(&hash).unwrap());
        // Outer front got backfilled by outer TieredCas
        assert!(outer_front.exists(&hash).unwrap());
    }

    #[test]
    fn clone_shares_state() {
        let front = Arc::new(MemoryCas::new());
        let back = Arc::new(MemoryCas::new());
        let cached = TieredCas::new(front.clone(), back.clone());

        let cloned = cached.clone();
        let hash = cached.put(b"shared").unwrap();
        assert!(cloned.exists(&hash).unwrap());
    }
}

// Rust guideline compliant 2026-02-21
