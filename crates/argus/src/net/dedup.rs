//! Network event deduplication tracker.
//!
//! Prevents duplicate events when the same network data is captured
//! by both the ptrace write() interception and the mitmdump proxy.
//! Tracks `(fd, content_hash)` pairs with timestamps, expiring entries
//! older than the dedup window to bound memory usage.

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Window within which a repeated `(fd, hash)` is considered duplicate.
/// Chosen to exceed typical TCP retransmit delays while staying short
/// enough to reclaim memory quickly.
const DEDUP_WINDOW: Duration = Duration::from_secs(5);

/// Key for dedup lookup: file descriptor and content hash.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DedupKey {
    fd: i32,
    content_hash: String,
}

/// Tracks recently seen network events to suppress duplicates.
///
/// The tracker stores `(fd, content_hash)` pairs with the time they
/// were first seen. Entries older than [`DEDUP_WINDOW`] are lazily
/// evicted during `is_duplicate` calls.
#[derive(Debug)]
pub struct NetworkDedup {
    seen: HashMap<DedupKey, Instant>,
    window: Duration,
}

impl NetworkDedup {
    /// Create a tracker with the default dedup window.
    pub fn new() -> Self {
        Self {
            seen: HashMap::new(),
            window: DEDUP_WINDOW,
        }
    }

    /// Create a tracker with a custom dedup window (for testing).
    #[cfg(test)]
    fn with_window(window: Duration) -> Self {
        Self {
            seen: HashMap::new(),
            window,
        }
    }

    /// Check whether this `(fd, content_hash)` was already seen recently.
    ///
    /// Returns `true` if the event is a duplicate and should be
    /// suppressed. If not a duplicate, records it for future checks.
    pub fn is_duplicate(&mut self, fd: i32, content_hash: &str) -> bool {
        self.evict_expired();

        let key = DedupKey {
            fd,
            content_hash: content_hash.to_owned(),
        };

        match self.seen.entry(key) {
            Entry::Occupied(_) => true,
            Entry::Vacant(slot) => {
                slot.insert(Instant::now());
                false
            }
        }
    }

    /// Remove all tracked entries, resetting state for a new session.
    pub fn clear(&mut self) {
        self.seen.clear();
    }

    /// Number of entries currently tracked.
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// Whether the tracker has no entries.
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    /// Remove entries older than the dedup window.
    fn evict_expired(&mut self) {
        let cutoff = Instant::now() - self.window;
        self.seen.retain(|_, ts| *ts > cutoff);
    }
}

impl Default for NetworkDedup {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_event_is_not_duplicate() {
        let mut dedup = NetworkDedup::new();
        assert!(!dedup.is_duplicate(5, "abc123"));
    }

    #[test]
    fn same_fd_and_hash_is_duplicate() {
        let mut dedup = NetworkDedup::new();
        dedup.is_duplicate(5, "abc123");
        assert!(dedup.is_duplicate(5, "abc123"));
    }

    #[test]
    fn different_fd_is_not_duplicate() {
        let mut dedup = NetworkDedup::new();
        dedup.is_duplicate(5, "abc123");
        assert!(!dedup.is_duplicate(6, "abc123"));
    }

    #[test]
    fn different_hash_is_not_duplicate() {
        let mut dedup = NetworkDedup::new();
        dedup.is_duplicate(5, "abc123");
        assert!(!dedup.is_duplicate(5, "def456"));
    }

    #[test]
    fn expired_entries_are_evicted() {
        let mut dedup = NetworkDedup::with_window(Duration::from_millis(1));
        dedup.is_duplicate(5, "abc123");
        assert_eq!(dedup.len(), 1);

        // Sleep just past the window so the entry expires.
        std::thread::sleep(Duration::from_millis(5));

        // Eviction happens on next call.
        assert!(!dedup.is_duplicate(5, "abc123"));
    }

    #[test]
    fn len_and_is_empty() {
        let mut dedup = NetworkDedup::new();
        assert!(dedup.is_empty());
        assert_eq!(dedup.len(), 0);

        dedup.is_duplicate(1, "a");
        dedup.is_duplicate(2, "b");
        assert_eq!(dedup.len(), 2);
        assert!(!dedup.is_empty());
    }

    #[test]
    fn clear_resets_state() {
        let mut dedup = NetworkDedup::new();
        dedup.is_duplicate(1, "a");
        assert_eq!(dedup.len(), 1);

        dedup.clear();
        assert!(dedup.is_empty());
        assert!(!dedup.is_duplicate(1, "a"), "after clear, same key should not be duplicate");
    }

    #[test]
    fn multiple_entries_tracked_independently() {
        let mut dedup = NetworkDedup::new();
        assert!(!dedup.is_duplicate(1, "hash_a"));
        assert!(!dedup.is_duplicate(2, "hash_b"));
        assert!(!dedup.is_duplicate(1, "hash_b"));
        assert!(dedup.is_duplicate(1, "hash_a"));
        assert!(dedup.is_duplicate(2, "hash_b"));
    }
}
