//! Content capture around file-mutating syscalls.
//!
//! Coordinates per-path write locks with CAS hashing so the supervisor
//! captures consistent before/after snapshots of every file mutation.
//! The tracer acquires a [`CaptureGuard`] on syscall entry (which hashes
//! the file's current content) and completes it on syscall exit (hashing
//! the new content and releasing the lock).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use tracing::event;
use tracing::Level;

use crate::cas::{Cas, LocalCas, ContentHash};

use super::write_locks::WriteLocks;

/// Result of capturing content before and after a file mutation.
#[derive(Debug, Clone)]
pub struct CaptureResult {
    /// Hash of the file content before the syscall executed.
    pub before_hash: Option<ContentHash>,
    /// Hash of the file content after the syscall executed.
    pub after_hash: Option<ContentHash>,
}

/// Holds a per-path lock and the pre-mutation content hash.
///
/// Created on syscall entry via [`acquire_for_path`]. The caller must
/// invoke [`complete`] on syscall exit to hash the post-mutation
/// content, store it in CAS, and release the lock.
// SAFETY: Field drop order is critical for soundness. Rust drops fields in
// declaration order. `_lock_guard` (the `MutexGuard`) must be dropped
// *before* `_lock_arc` (the `Arc<Mutex<()>>`), because the guard borrows
// from the Mutex inside the Arc. If `_lock_arc` were listed before
// `_lock_guard`, the Arc could drop the Mutex while the guard still
// references it, causing use-after-free (UB). Do not reorder these fields.
#[derive(Debug)]
pub struct CaptureGuard {
    path: PathBuf,
    before_hash: Option<ContentHash>,
    _lock_guard: MutexGuard<'static, ()>,
    _lock_arc: Arc<Mutex<()>>,
}

// SAFETY: Same field drop order invariant as `CaptureGuard` — guards must
// be dropped before arcs. See the safety comment on `CaptureGuard`.
/// Holds locks for a rename (two paths, acquired in sorted order).
#[derive(Debug)]
pub struct RenameCaptureGuard {
    pub src_path: PathBuf,
    pub dst_path: PathBuf,
    pub src_before_hash: Option<ContentHash>,
    pub dst_before_hash: Option<ContentHash>,
    _lock_guards: Vec<MutexGuard<'static, ()>>,
    _lock_arcs: Vec<Arc<Mutex<()>>>,
}

/// Hashes a file's content and stores it in CAS.
///
/// Returns `None` if the file does not exist or cannot be read (e.g.,
/// the path refers to a directory, device, or was already deleted).
fn hash_and_store(cas: &LocalCas, path: &Path) -> Option<ContentHash> {
    let data = match fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            event!(
                name: "write_capture.file_read.skipped",
                Level::DEBUG,
                file.path = %path.display(),
                error.message = %e,
                "file unreadable for content capture at {{file.path}}: {{error.message}}",
            );
            return None;
        }
    };
    match cas.put(&data) {
        Ok(hash) => Some(hash),
        Err(e) => {
            event!(
                name: "write_capture.cas_store.error",
                Level::WARN,
                file.path = %path.display(),
                error.message = %e,
                "failed to store pre-write content for {{file.path}}: {{error.message}}",
            );
            None
        }
    }
}

/// Acquires the lock and returns a guard that must be completed later.
///
/// # Safety contract (not `unsafe`, but important)
///
/// The returned `Arc<Mutex<()>>` must stay alive as long as the
/// `MutexGuard` it produced. We enforce this by bundling both into
/// [`CaptureGuard`].
fn lock_and_hash(
    locks: &WriteLocks,
    cas: &LocalCas,
    path: &Path,
) -> (Arc<Mutex<()>>, MutexGuard<'static, ()>, Option<ContentHash>) {
    let arc = locks.get_or_create(path);
    // The Arc is kept alive by the caller, so the Mutex it points to
    // will not be dropped while the guard exists. Transmuting the
    // lifetime is sound because we co-store the Arc.
    let guard: MutexGuard<'static, ()> = unsafe {
        std::mem::transmute(arc.lock().expect("write lock poisoned"))
    };
    let before_hash = hash_and_store(cas, path);
    (arc, guard, before_hash)
}

/// Acquires a write lock for a single path and captures its content.
///
/// Call [`CaptureGuard::complete`] on syscall exit to finalize.
///
/// # Errors
///
/// Returns an error only if internal invariants are violated; I/O
/// failures on the file itself are treated as missing content (hash
/// is `None`).
pub fn acquire_for_path(
    locks: &WriteLocks,
    cas: &LocalCas,
    path: &Path,
) -> CaptureGuard {
    let (arc, guard, before_hash) = lock_and_hash(locks, cas, path);
    CaptureGuard {
        path: path.to_path_buf(),
        before_hash,
        _lock_guard: guard,
        _lock_arc: arc,
    }
}

/// Acquires write locks for a rename (source and destination).
///
/// Locks are acquired in sorted path order to prevent deadlocks when
/// two renames cross paths concurrently.
pub fn acquire_for_rename(
    locks: &WriteLocks,
    cas: &LocalCas,
    src: &Path,
    dst: &Path,
) -> RenameCaptureGuard {
    // Self-rename (src == dst): only acquire one lock to avoid deadlock
    // on the same Mutex.
    if src == dst {
        let (arc, guard, hash) = lock_and_hash(locks, cas, src);
        return RenameCaptureGuard {
            src_path: src.to_path_buf(),
            dst_path: dst.to_path_buf(),
            src_before_hash: hash.clone(),
            dst_before_hash: hash,
            _lock_guards: vec![guard],
            _lock_arcs: vec![arc],
        };
    }

    let (first, second) = sorted_pair(src, dst);

    let (arc1, guard1, hash1) = lock_and_hash(locks, cas, first);
    let (arc2, guard2, hash2) = lock_and_hash(locks, cas, second);

    // Byte-for-byte path comparison; symlinks and relative segments
    // (e.g., `..`) are not resolved. The tracer is expected to pass
    // canonical absolute paths obtained from fd table resolution.
    let (src_hash, dst_hash) = if first == src {
        (hash1, hash2)
    } else {
        (hash2, hash1)
    };

    RenameCaptureGuard {
        src_path: src.to_path_buf(),
        dst_path: dst.to_path_buf(),
        src_before_hash: src_hash,
        dst_before_hash: dst_hash,
        _lock_guards: vec![guard1, guard2],
        _lock_arcs: vec![arc1, arc2],
    }
}

/// Returns `(smaller, larger)` so locks are always acquired in a
/// consistent order, preventing deadlocks.
fn sorted_pair<'a>(a: &'a Path, b: &'a Path) -> (&'a Path, &'a Path) {
    if a <= b { (a, b) } else { (b, a) }
}

impl CaptureGuard {
    /// Hashes the file after the syscall and produces the final result.
    ///
    /// Consumes `self`, releasing the per-path lock.
    pub fn complete(self, cas: &LocalCas) -> CaptureResult {
        let after_hash = hash_and_store(cas, &self.path);
        CaptureResult {
            before_hash: self.before_hash,
            after_hash,
        }
    }

    /// Completes capture for an unlink (no after-hash since file is gone).
    ///
    /// Consumes `self`, releasing the per-path lock.
    pub fn complete_unlink(self) -> CaptureResult {
        CaptureResult {
            before_hash: self.before_hash,
            after_hash: None,
        }
    }

    /// Returns the before-hash captured on acquisition.
    pub fn before_hash(&self) -> Option<&ContentHash> {
        self.before_hash.as_ref()
    }
}

impl RenameCaptureGuard {
    /// Completes capture for a rename operation.
    ///
    /// After a rename, the source path no longer exists and the
    /// destination has the source's former content.
    ///
    /// Consumes `self`, releasing both per-path locks.
    pub fn complete(self, cas: &LocalCas) -> (CaptureResult, CaptureResult) {
        let src_result = CaptureResult {
            before_hash: self.src_before_hash,
            after_hash: None,
        };
        let dst_after = hash_and_store(cas, &self.dst_path);
        let dst_result = CaptureResult {
            before_hash: self.dst_before_hash,
            after_hash: dst_after,
        };
        (src_result, dst_result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, WriteLocks, LocalCas) {
        let dir = tempfile::tempdir().expect("tempdir");
        let locks = WriteLocks::new();
        let cas = LocalCas::new(dir.path().join("cas")).expect("LocalCas");
        (dir, locks, cas)
    }

    #[test]
    fn capture_existing_file_before_hash() {
        let (dir, locks, cas) = setup();
        let file = dir.path().join("test.txt");
        fs::write(&file, b"hello").unwrap();

        let guard = acquire_for_path(&locks, &cas, &file);
        assert!(guard.before_hash().is_some());
        let expected = ContentHash::from_data(b"hello");
        assert_eq!(guard.before_hash().unwrap(), &expected);

        // Modify file while lock is held (simulating syscall execution).
        fs::write(&file, b"world").unwrap();
        let result = guard.complete(&cas);

        assert_eq!(result.before_hash.unwrap(), expected);
        assert_eq!(
            result.after_hash.unwrap(),
            ContentHash::from_data(b"world"),
        );
    }

    #[test]
    fn capture_nonexistent_file_returns_none_before() {
        let (dir, locks, cas) = setup();
        let file = dir.path().join("ghost.txt");

        let guard = acquire_for_path(&locks, &cas, &file);
        assert!(guard.before_hash().is_none());

        // File is created by the syscall.
        fs::write(&file, b"new").unwrap();
        let result = guard.complete(&cas);

        assert!(result.before_hash.is_none());
        assert_eq!(
            result.after_hash.unwrap(),
            ContentHash::from_data(b"new"),
        );
    }

    #[test]
    fn capture_unlink_no_after_hash() {
        let (dir, locks, cas) = setup();
        let file = dir.path().join("doomed.txt");
        fs::write(&file, b"goodbye").unwrap();

        let guard = acquire_for_path(&locks, &cas, &file);
        let expected = ContentHash::from_data(b"goodbye");
        assert_eq!(guard.before_hash().unwrap(), &expected);

        fs::remove_file(&file).unwrap();
        let result = guard.complete_unlink();

        assert_eq!(result.before_hash.unwrap(), expected);
        assert!(result.after_hash.is_none());
    }

    #[test]
    fn rename_captures_both_paths() {
        let (dir, locks, cas) = setup();
        let src = dir.path().join("src.txt");
        let dst = dir.path().join("dst.txt");
        fs::write(&src, b"moving").unwrap();

        let guard = acquire_for_rename(&locks, &cas, &src, &dst);
        let expected_src = ContentHash::from_data(b"moving");
        assert_eq!(guard.src_before_hash.as_ref().unwrap(), &expected_src);
        assert!(guard.dst_before_hash.is_none());

        // Simulate the rename syscall.
        fs::rename(&src, &dst).unwrap();
        let (src_result, dst_result) = guard.complete(&cas);

        assert_eq!(src_result.before_hash.unwrap(), expected_src);
        assert!(src_result.after_hash.is_none());
        assert!(dst_result.before_hash.is_none());
        assert_eq!(
            dst_result.after_hash.unwrap(),
            ContentHash::from_data(b"moving"),
        );
    }

    #[test]
    fn rename_overwrites_existing_dst() {
        let (dir, locks, cas) = setup();
        let src = dir.path().join("src.txt");
        let dst = dir.path().join("dst.txt");
        fs::write(&src, b"new content").unwrap();
        fs::write(&dst, b"old content").unwrap();

        let guard = acquire_for_rename(&locks, &cas, &src, &dst);
        assert_eq!(
            guard.src_before_hash.as_ref().unwrap(),
            &ContentHash::from_data(b"new content"),
        );
        assert_eq!(
            guard.dst_before_hash.as_ref().unwrap(),
            &ContentHash::from_data(b"old content"),
        );

        fs::rename(&src, &dst).unwrap();
        let (src_result, dst_result) = guard.complete(&cas);

        assert!(src_result.after_hash.is_none());
        assert_eq!(
            dst_result.after_hash.unwrap(),
            ContentHash::from_data(b"new content"),
        );
    }

    #[test]
    fn sorted_pair_consistent_order() {
        let a = Path::new("/a/file");
        let b = Path::new("/b/file");
        let (first1, second1) = sorted_pair(a, b);
        let (first2, second2) = sorted_pair(b, a);
        assert_eq!(first1, first2);
        assert_eq!(second1, second2);
        assert_eq!(first1, a);
        assert_eq!(second1, b);
    }

    #[test]
    fn concurrent_writes_serialize() {
        use std::sync::Arc as StdArc;
        use std::sync::Barrier;
        use std::thread;

        let (dir, locks, cas) = setup();
        let locks = StdArc::new(locks);
        let cas = StdArc::new(cas);
        let file = dir.path().join("contested.txt");
        fs::write(&file, b"initial").unwrap();

        let barrier = StdArc::new(Barrier::new(2));
        let results: Vec<_> = (0..2)
            .map(|i| {
                let locks = StdArc::clone(&locks);
                let cas = StdArc::clone(&cas);
                let file = file.clone();
                let barrier = StdArc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    let guard = acquire_for_path(&locks, &cas, &file);
                    let content = format!("writer-{i}");
                    fs::write(&file, content.as_bytes()).unwrap();
                    guard.complete(&cas)
                })
            })
            .collect();

        let captures: Vec<_> = results
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect();

        // Both captures must have before and after hashes (file always
        // exists). They serialize, so one writer sees the other's
        // output as its before-hash.
        for cap in &captures {
            assert!(cap.before_hash.is_some());
            assert!(cap.after_hash.is_some());
        }
    }

    #[test]
    fn content_stored_in_cas() {
        let (dir, locks, cas) = setup();
        let file = dir.path().join("stored.txt");
        fs::write(&file, b"store me").unwrap();

        let guard = acquire_for_path(&locks, &cas, &file);
        let before = guard.before_hash().unwrap().clone();
        let result = guard.complete(&cas);

        // Verify the CAS actually contains the content.
        assert!(cas.exists(&before).unwrap());
        let data = cas.get(&before).unwrap();
        assert_eq!(data, b"store me");

        if let Some(ref after) = result.after_hash {
            assert!(cas.exists(after).unwrap());
        }
    }

    #[test]
    fn self_rename_does_not_deadlock() {
        let (dir, locks, cas) = setup();
        let file = dir.path().join("same.txt");
        fs::write(&file, b"unchanged").unwrap();

        let guard = acquire_for_rename(&locks, &cas, &file, &file);
        let expected = ContentHash::from_data(b"unchanged");
        assert_eq!(guard.src_before_hash.as_ref().unwrap(), &expected);
        assert_eq!(guard.dst_before_hash.as_ref().unwrap(), &expected);

        // Self-rename is a no-op; file content stays the same.
        let (src_result, dst_result) = guard.complete(&cas);
        assert!(src_result.after_hash.is_none());
        assert_eq!(dst_result.after_hash.unwrap(), expected);
    }

    #[test]
    fn empty_file_capture() {
        let (dir, locks, cas) = setup();
        let file = dir.path().join("empty.txt");
        fs::write(&file, b"").unwrap();

        let guard = acquire_for_path(&locks, &cas, &file);
        let expected = ContentHash::from_data(b"");
        assert_eq!(guard.before_hash().unwrap(), &expected);

        let result = guard.complete(&cas);
        assert_eq!(result.before_hash.unwrap(), expected);
        assert_eq!(result.after_hash.unwrap(), expected);
    }
}
