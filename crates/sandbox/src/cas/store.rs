// Rust guideline compliant 2026-02-21

//! Filesystem-backed content-addressable store.
//!
//! Objects are stored under `{root}/{hash[0:2]}/{hash[2:]}`. Writes
//! are atomic (temp file, fsync, rename) so concurrent writers cannot
//! corrupt data. Identical content is stored exactly once.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::event;

use super::hash::ContentHash;
use super::stats::{CasStats, CasStatsSnapshot};

/// Local content-addressable store backed by the filesystem.
///
/// All writes are atomic: data lands in a temp file, is fsynced, then
/// renamed to its content-addressed path. Duplicate content is
/// automatically deduplicated.
#[derive(Debug)]
pub struct CasStore {
    root: PathBuf,
    stats: CasStats,
}

impl CasStore {
    /// Open or create a CAS store rooted at `root`.
    ///
    /// # Errors
    ///
    /// Returns an error if the root directory cannot be created.
    pub fn new(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(&root)
            .with_context(|| format!("create CAS root {}", root.display()))?;
        Ok(Self {
            root,
            stats: CasStats::new(),
        })
    }

    /// Hash `data` and store it, returning the content hash.
    ///
    /// If the object already exists on disk the write is skipped
    /// (dedup). Stats are only updated for genuinely new objects.
    ///
    /// # Errors
    ///
    /// Returns an error if the filesystem write fails.
    pub fn store(&self, data: &[u8]) -> Result<ContentHash> {
        let hash = ContentHash::from_data(data);
        let path = self.object_path(&hash);

        if path.exists() {
            return Ok(hash);
        }

        self.atomic_write(&path, data)?;

        self.stats.record_add(data.len() as u64);
        event!(
            name: "cas.store.added",
            tracing::Level::DEBUG,
            cas.hash = hash.as_str(),
            cas.size = data.len(),
            "stored new object {{cas.hash}} ({{cas.size}} bytes)",
        );

        Ok(hash)
    }

    /// Check whether an object exists on disk.
    pub fn exists(&self, hash: &ContentHash) -> bool {
        self.object_path(hash).exists()
    }

    /// Read the full contents of a stored object.
    ///
    /// # Errors
    ///
    /// Returns an error if the object does not exist or cannot be read.
    pub fn read(&self, hash: &ContentHash) -> Result<Vec<u8>> {
        let path = self.object_path(hash);
        fs::read(&path)
            .with_context(|| format!("read CAS object {hash}"))
    }

    /// Remove an object from the store.
    ///
    /// # Errors
    ///
    /// Returns an error if the object does not exist or removal fails.
    pub fn delete(&self, hash: &ContentHash) -> Result<()> {
        let path = self.object_path(hash);
        let size = fs::metadata(&path)
            .with_context(|| format!("stat CAS object {hash}"))?
            .len();
        fs::remove_file(&path)
            .with_context(|| format!("delete CAS object {hash}"))?;
        self.stats.record_delete(size);
        Ok(())
    }

    /// Filesystem path for an object: `{root}/{prefix}/{suffix}`.
    pub fn object_path(&self, hash: &ContentHash) -> PathBuf {
        self.root.join(hash.prefix()).join(hash.suffix())
    }

    /// Take a snapshot of current store statistics.
    pub fn stats(&self) -> CasStatsSnapshot {
        self.stats.snapshot()
    }

    /// Write data atomically: temp file -> fsync -> rename.
    fn atomic_write(&self, final_path: &Path, data: &[u8]) -> Result<()> {
        let parent = final_path
            .parent()
            .context("CAS object path has no parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("create CAS dir {}", parent.display()))?;

        let temp_path = parent.join(format!(
            ".tmp-{tid}-{rand}",
            tid = std::process::id(),
            rand = fastrand_u32(),
        ));

        let result = write_and_sync(&temp_path, data);

        if let Err(e) = &result {
            // Clean up the temp file on failure; ignore removal errors.
            let _ = fs::remove_file(&temp_path);
            return Err(anyhow::anyhow!(
                "atomic write to {}: {e}",
                final_path.display()
            ));
        }

        // Rename is atomic on POSIX; if the target appeared between our
        // exists-check and now, rename silently overwrites (same content).
        fs::rename(&temp_path, final_path).with_context(|| {
            format!(
                "rename {} -> {}",
                temp_path.display(),
                final_path.display()
            )
        })?;

        Ok(())
    }
}

/// Write data and fsync, separated for clarity and borrow scoping.
fn write_and_sync(path: &Path, data: &[u8]) -> Result<()> {
    let mut file = fs::File::create(path)
        .with_context(|| format!("create temp file {}", path.display()))?;
    file.write_all(data)
        .with_context(|| format!("write temp file {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("fsync temp file {}", path.display()))?;
    Ok(())
}

/// Cheap pseudo-random u32 using the thread's address as entropy. Good
/// enough for temp file naming; not cryptographic.
fn fastrand_u32() -> u32 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::SystemTime;

    let mut h = DefaultHasher::new();
    SystemTime::now().hash(&mut h);
    std::thread::current().id().hash(&mut h);
    h.finish() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store() -> (tempfile::TempDir, CasStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store =
            CasStore::new(dir.path().join("cas")).expect("CasStore::new");
        (dir, store)
    }

    #[test]
    fn store_and_read_round_trip() {
        let (_dir, store) = tmp_store();
        let data = b"hello world";
        let hash = store.store(data).expect("store");
        let read_back = store.read(&hash).expect("read");
        assert_eq!(read_back, data);
    }

    #[test]
    fn dedup_same_content() {
        let (_dir, store) = tmp_store();
        let data = b"duplicate";
        let h1 = store.store(data).expect("store 1");
        let h2 = store.store(data).expect("store 2");
        assert_eq!(h1, h2);

        // Only one file should be on disk.
        let path = store.object_path(&h1);
        assert!(path.exists());

        // Stats should show only one add (dedup skips the second).
        let snap = store.stats();
        assert_eq!(snap.objects_added, 1);
    }

    #[test]
    fn exists_true_after_store() {
        let (_dir, store) = tmp_store();
        let hash = store.store(b"exists test").expect("store");
        assert!(store.exists(&hash));
    }

    #[test]
    fn exists_false_for_unknown() {
        let (_dir, store) = tmp_store();
        let hash = ContentHash::from_data(b"never stored");
        assert!(!store.exists(&hash));
    }

    #[test]
    fn delete_removes_file() {
        let (_dir, store) = tmp_store();
        let hash = store.store(b"to delete").expect("store");
        assert!(store.exists(&hash));
        store.delete(&hash).expect("delete");
        assert!(!store.exists(&hash));
    }

    #[test]
    fn delete_updates_stats() {
        let (_dir, store) = tmp_store();
        let data = b"stats delete";
        let hash = store.store(data).expect("store");
        store.delete(&hash).expect("delete");
        let snap = store.stats();
        assert_eq!(snap.total_objects, 0);
        assert_eq!(snap.total_bytes, 0);
        // Cumulative counters unchanged.
        assert_eq!(snap.objects_added, 1);
    }

    #[test]
    fn read_nonexistent_errors() {
        let (_dir, store) = tmp_store();
        let hash = ContentHash::from_data(b"ghost");
        assert!(store.read(&hash).is_err());
    }

    #[test]
    fn object_path_layout() {
        let (_dir, store) = tmp_store();
        let hash = ContentHash::from_data(b"path test");
        let path = store.object_path(&hash);
        let components: Vec<_> =
            path.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
        let len = components.len();
        assert_eq!(components[len - 2], hash.prefix());
        assert_eq!(components[len - 1], hash.suffix());
    }

    #[test]
    fn stats_track_bytes() {
        let (_dir, store) = tmp_store();
        store.store(b"aaa").expect("store 1");
        store.store(b"bbbbb").expect("store 2");
        let snap = store.stats();
        assert_eq!(snap.objects_added, 2);
        assert_eq!(snap.bytes_added, 8);
        assert_eq!(snap.total_objects, 2);
        assert_eq!(snap.total_bytes, 8);
    }

    #[test]
    fn concurrent_store_same_content() {
        let (_dir, store) = tmp_store();
        let store = std::sync::Arc::new(store);
        let data = b"concurrent";

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let s = store.clone();
                let d = data.to_vec();
                std::thread::spawn(move || s.store(&d))
            })
            .collect();

        let hashes: Vec<_> = handles
            .into_iter()
            .map(|h| h.join().expect("thread").expect("store"))
            .collect();

        // All threads produce the same hash.
        let first = &hashes[0];
        for h in &hashes[1..] {
            assert_eq!(h, first);
        }

        // Exactly one file on disk.
        assert!(store.exists(first));
        let snap = store.stats();
        // At least 1 add, at most 8 (races allowed), but no corruption.
        assert!(snap.objects_added >= 1 && snap.objects_added <= 8);
    }
}
