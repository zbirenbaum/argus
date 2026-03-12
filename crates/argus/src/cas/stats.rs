//! Atomic counters for CAS store metrics.
//!
//! All fields use `AtomicU64` so they can be updated from any thread
//! without holding a lock. A [`CasStatsSnapshot`] provides a
//! non-atomic copy for reading and serialization.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

/// Atomic counters tracking CAS store activity.
///
/// Thread-safe via `AtomicU64`; no lock needed for updates.
#[derive(Debug)]
pub struct CasStats {
    total_objects: AtomicU64,
    total_bytes: AtomicU64,
    objects_added: AtomicU64,
    bytes_added: AtomicU64,
}

/// Point-in-time snapshot of [`CasStats`].
#[derive(Debug, Clone, Serialize)]
pub struct CasStatsSnapshot {
    /// Objects currently tracked.
    pub total_objects: u64,
    /// Total bytes of all tracked objects.
    pub total_bytes: u64,
    /// Objects added since store creation.
    pub objects_added: u64,
    /// Bytes added since store creation.
    pub bytes_added: u64,
}

impl CasStats {
    /// Create zeroed counters.
    pub fn new() -> Self {
        Self {
            total_objects: AtomicU64::new(0),
            total_bytes: AtomicU64::new(0),
            objects_added: AtomicU64::new(0),
            bytes_added: AtomicU64::new(0),
        }
    }

    /// Record a newly stored object.
    pub fn record_add(&self, size: u64) {
        self.total_objects.fetch_add(1, Ordering::Relaxed);
        self.total_bytes.fetch_add(size, Ordering::Relaxed);
        self.objects_added.fetch_add(1, Ordering::Relaxed);
        self.bytes_added.fetch_add(size, Ordering::Relaxed);
    }

    /// Record removal of an object.
    pub fn record_delete(&self, size: u64) {
        self.total_objects.fetch_sub(1, Ordering::Relaxed);
        self.total_bytes.fetch_sub(size, Ordering::Relaxed);
    }

    /// Take a consistent-enough snapshot for reporting.
    pub fn snapshot(&self) -> CasStatsSnapshot {
        CasStatsSnapshot {
            total_objects: self.total_objects.load(Ordering::Relaxed),
            total_bytes: self.total_bytes.load(Ordering::Relaxed),
            objects_added: self.objects_added.load(Ordering::Relaxed),
            bytes_added: self.bytes_added.load(Ordering::Relaxed),
        }
    }
}

/// Provider-agnostic storage statistics.
///
/// Returned by [`CasBackend::stats`](super::traits::CasBackend::stats)
/// so callers can monitor any backend without knowing its type.
#[derive(Debug, Clone, Default, Serialize)]
pub struct BackendStats {
    /// Number of objects currently stored.
    pub object_count: u64,
    /// Total bytes currently stored.
    pub total_bytes: u64,
}

impl Default for CasStats {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_stats_are_zero() {
        let s = CasStats::new();
        let snap = s.snapshot();
        assert_eq!(snap.total_objects, 0);
        assert_eq!(snap.total_bytes, 0);
        assert_eq!(snap.objects_added, 0);
        assert_eq!(snap.bytes_added, 0);
    }

    #[test]
    fn record_add_increments() {
        let s = CasStats::new();
        s.record_add(100);
        s.record_add(200);
        let snap = s.snapshot();
        assert_eq!(snap.total_objects, 2);
        assert_eq!(snap.total_bytes, 300);
        assert_eq!(snap.objects_added, 2);
        assert_eq!(snap.bytes_added, 300);
    }

    #[test]
    fn record_delete_decrements_totals_only() {
        let s = CasStats::new();
        s.record_add(100);
        s.record_add(200);
        s.record_delete(100);
        let snap = s.snapshot();
        assert_eq!(snap.total_objects, 1);
        assert_eq!(snap.total_bytes, 200);
        // Added counters are cumulative — never decrease.
        assert_eq!(snap.objects_added, 2);
        assert_eq!(snap.bytes_added, 300);
    }

    #[test]
    fn snapshot_serializes() {
        let s = CasStats::new();
        s.record_add(42);
        let snap = s.snapshot();
        let json = serde_json::to_string(&snap).expect("serialize");
        assert!(json.contains("\"total_objects\":1"));
    }
}
