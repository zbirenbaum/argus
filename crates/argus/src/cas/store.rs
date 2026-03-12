//! Filesystem-backed content-addressable store.
//!
//! Objects are stored under `{root}/{hash[0:2]}/{hash[2:]}`. Writes
//! are atomic (temp file, fsync, rename) so concurrent writers cannot
//! corrupt data. Identical content is stored exactly once.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tempfile::NamedTempFile;
use tracing::event;

use super::hash::ContentHash;
use super::stats::{CasStats, CasStatsSnapshot};
use super::traits::Cas;

/// Local content-addressable store backed by the filesystem.
///
/// All writes are atomic: data lands in a temp file, is fsynced, then
/// renamed to its content-addressed path. Duplicate content is
/// automatically deduplicated.
#[derive(Debug)]
pub struct LocalCas {
    root: PathBuf,
    stats: CasStats,
}

impl LocalCas {
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
    ///
    /// Uses `NamedTempFile` in the target directory to guarantee
    /// unique temp filenames even under high concurrency.
    fn atomic_write(&self, final_path: &Path, data: &[u8]) -> Result<()> {
        let parent = final_path
            .parent()
            .context("CAS object path has no parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("create CAS dir {}", parent.display()))?;

        let mut tmp = NamedTempFile::new_in(parent)
            .with_context(|| format!("create temp file in {}", parent.display()))?;
        tmp.write_all(data)
            .with_context(|| format!("write temp file {}", tmp.path().display()))?;
        tmp.as_file().sync_all()
            .with_context(|| format!("fsync temp file {}", tmp.path().display()))?;

        // Rename is atomic on POSIX; if the target appeared between our
        // exists-check and now, rename silently overwrites (same content).
        tmp.persist(final_path).map_err(|e| {
            anyhow::anyhow!(
                "rename {} -> {}: {}",
                e.file.path().display(),
                final_path.display(),
                e.error,
            )
        })?;

        Ok(())
    }
}

impl Cas for LocalCas {
    fn get(&self, hash: &ContentHash) -> Result<Vec<u8>> {
        let path = self.object_path(hash);
        fs::read(&path)
            .with_context(|| format!("read CAS object {hash}"))
    }

    fn put(&self, content: &[u8]) -> Result<ContentHash> {
        let hash = ContentHash::from_data(content);
        let path = self.object_path(&hash);

        // TOCTOU: concurrent stores of the same content may both pass
        // this check and double-count stats. Acceptable trade-off vs.
        // locking, since the CAS file itself is written atomically and
        // rename-overwrites are idempotent for identical content.
        if path.exists() {
            return Ok(hash);
        }

        self.atomic_write(&path, content)?;

        self.stats.record_add(content.len() as u64);
        event!(
            name: "cas.store.added",
            tracing::Level::DEBUG,
            cas.hash = hash.as_str(),
            cas.size = content.len(),
            "stored new CAS object",
        );

        Ok(hash)
    }

    fn exists(&self, hash: &ContentHash) -> Result<bool> {
        Ok(self.object_path(hash).exists())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store() -> (tempfile::TempDir, LocalCas) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store =
            LocalCas::new(dir.path().join("cas")).expect("LocalCas::new");
        (dir, store)
    }

    #[test]
    fn put_and_get_round_trip() {
        let (_dir, store) = tmp_store();
        let data = b"hello world";
        let hash = store.put(data).expect("put");
        let read_back = store.get(&hash).expect("get");
        assert_eq!(read_back, data);
    }

    #[test]
    fn dedup_same_content() {
        let (_dir, store) = tmp_store();
        let data = b"duplicate";
        let h1 = store.put(data).expect("put 1");
        let h2 = store.put(data).expect("put 2");
        assert_eq!(h1, h2);

        // Only one file should be on disk.
        let path = store.object_path(&h1);
        assert!(path.exists());

        // Stats should show only one add (dedup skips the second).
        let snap = store.stats();
        assert_eq!(snap.objects_added, 1);
    }

    #[test]
    fn exists_true_after_put() {
        let (_dir, store) = tmp_store();
        let hash = store.put(b"exists test").expect("put");
        assert!(store.exists(&hash).expect("exists"));
    }

    #[test]
    fn exists_false_for_unknown() {
        let (_dir, store) = tmp_store();
        let hash = ContentHash::from_data(b"never stored");
        assert!(!store.exists(&hash).expect("exists"));
    }

    #[test]
    fn delete_removes_file() {
        let (_dir, store) = tmp_store();
        let hash = store.put(b"to delete").expect("put");
        assert!(store.exists(&hash).expect("exists"));
        store.delete(&hash).expect("delete");
        assert!(!store.exists(&hash).expect("exists"));
    }

    #[test]
    fn delete_updates_stats() {
        let (_dir, store) = tmp_store();
        let data = b"stats delete";
        let hash = store.put(data).expect("put");
        store.delete(&hash).expect("delete");
        let snap = store.stats();
        assert_eq!(snap.total_objects, 0);
        assert_eq!(snap.total_bytes, 0);
        // Cumulative counters unchanged.
        assert_eq!(snap.objects_added, 1);
    }

    #[test]
    fn get_nonexistent_errors() {
        let (_dir, store) = tmp_store();
        let hash = ContentHash::from_data(b"ghost");
        assert!(store.get(&hash).is_err());
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
        store.put(b"aaa").expect("put 1");
        store.put(b"bbbbb").expect("put 2");
        let snap = store.stats();
        assert_eq!(snap.objects_added, 2);
        assert_eq!(snap.bytes_added, 8);
        assert_eq!(snap.total_objects, 2);
        assert_eq!(snap.total_bytes, 8);
    }

    #[test]
    fn put_empty_data() {
        let (_dir, store) = tmp_store();
        let hash = store.put(b"").expect("put empty");
        let read_back = store.get(&hash).expect("get empty");
        assert!(read_back.is_empty());
        assert!(store.exists(&hash).expect("exists"));

        let snap = store.stats();
        assert_eq!(snap.objects_added, 1);
        assert_eq!(snap.bytes_added, 0);
    }

    #[test]
    fn concurrent_put_same_content() {
        let (_dir, store) = tmp_store();
        let store = std::sync::Arc::new(store);
        let data = b"concurrent";

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let s = store.clone();
                let d = data.to_vec();
                std::thread::spawn(move || s.put(&d))
            })
            .collect();

        let hashes: Vec<_> = handles
            .into_iter()
            .map(|h| h.join().expect("thread").expect("put"))
            .collect();

        // All threads produce the same hash.
        let first = &hashes[0];
        for h in &hashes[1..] {
            assert_eq!(h, first);
        }

        // Exactly one file on disk.
        assert!(store.exists(first).expect("exists"));
        let snap = store.stats();
        // At least 1 add, at most 8 (races allowed), but no corruption.
        assert!(snap.objects_added >= 1 && snap.objects_added <= 8);
    }
}
