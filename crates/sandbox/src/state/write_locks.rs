//! Per-path write locking for serializing file mutations.
//!
//! Ensures that concurrent writes to the same path are serialized so the
//! supervisor can capture consistent content snapshots. Phase 1 stub: provides
//! the locking mechanism without hashing integration.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Manages per-path mutexes for write serialization.
///
/// Callers obtain an `Arc<Mutex<()>>` for a given path and lock it
/// themselves. This avoids lifetime issues with returning a `MutexGuard`
/// from an interior `Arc`.
#[derive(Debug, Default)]
pub struct WriteLocks {
    locks: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
}

impl WriteLocks {
    /// Creates an empty write lock set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the lock for a path, creating it if needed.
    ///
    /// # Panics
    ///
    /// Panics if the internal registry mutex is poisoned.
    pub fn get_or_create(&self, path: &Path) -> Arc<Mutex<()>> {
        let mut map = self.locks.lock().expect("write lock registry poisoned");
        Arc::clone(
            map.entry(path.to_path_buf())
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        )
    }

    /// Evicts the lock entry for a path.
    ///
    /// Callers (the tracer loop) are responsible for calling this when files
    /// are unlinked or renamed away, so stale entries do not accumulate.
    ///
    /// # Panics
    ///
    /// Panics if the internal registry mutex is poisoned.
    pub fn remove(&self, path: &Path) {
        let mut map = self.locks.lock().expect("write lock registry poisoned");
        map.remove(path);
    }

    /// Returns the number of tracked path locks.
    pub fn len(&self) -> usize {
        self.locks
            .lock()
            .expect("write lock registry poisoned")
            .len()
    }

    /// Returns `true` if no path locks are tracked.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc as StdArc;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn get_or_create_returns_same_lock() {
        let wl = WriteLocks::new();
        let a = wl.get_or_create(Path::new("/tmp/a"));
        let b = wl.get_or_create(Path::new("/tmp/a"));
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn different_paths_get_different_locks() {
        let wl = WriteLocks::new();
        let a = wl.get_or_create(Path::new("/tmp/a"));
        let b = wl.get_or_create(Path::new("/tmp/b"));
        assert!(!Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn concurrent_acquire_blocks() {
        let wl = StdArc::new(WriteLocks::new());
        let path = Path::new("/tmp/contested");

        let lock = wl.get_or_create(path);
        let guard = lock.lock().unwrap();

        let wl2 = StdArc::clone(&wl);
        let start = Instant::now();
        let handle = thread::spawn(move || {
            let lock2 = wl2.get_or_create(Path::new("/tmp/contested"));
            let _guard2 = lock2.lock().unwrap();
            start.elapsed()
        });

        // Hold for 50ms so the other thread has to wait
        thread::sleep(Duration::from_millis(50));
        drop(guard);

        let elapsed = handle.join().unwrap();
        assert!(elapsed >= Duration::from_millis(40));
    }

    #[test]
    fn different_paths_dont_block() {
        let wl = StdArc::new(WriteLocks::new());

        let lock_a = wl.get_or_create(Path::new("/tmp/a"));
        let _guard = lock_a.lock().unwrap();

        let wl2 = StdArc::clone(&wl);
        let handle = thread::spawn(move || {
            let start = Instant::now();
            let lock_b = wl2.get_or_create(Path::new("/tmp/b"));
            let _guard = lock_b.lock().unwrap();
            start.elapsed()
        });

        let elapsed = handle.join().unwrap();
        assert!(elapsed < Duration::from_millis(10));
    }

    #[test]
    fn len_tracks_created_locks() {
        let wl = WriteLocks::new();
        assert!(wl.is_empty());

        wl.get_or_create(Path::new("/a"));
        wl.get_or_create(Path::new("/b"));
        assert_eq!(wl.len(), 2);

        wl.get_or_create(Path::new("/a"));
        assert_eq!(wl.len(), 2);
    }
}
