// Rust guideline compliant 2026-02-21

//! Batched mutation accumulator for the persistent Merkle tree.
//!
//! [`TreeBuilder`] accepts file mutations and defers hash computation until
//! [`TreeBuilder::finalize`] is called. Between finalizations the flat
//! `files` index is the source of truth; the `MerkleNode` tree is
//! reconstructed only when needed to produce a root hash.
//!
//! # Design
//!
//! Hashing is O(n) in the number of files — deferred batching amortizes
//! that cost. [`TreeSnapshot`] is a cheap clone of the flat index plus a
//! single precomputed root hash, safe to hand off without holding the
//! builder lock.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::cas::ContentHash;
use crate::config::TreeConfig;
use crate::snapshot::node::MerkleNode;

/// Accumulates file mutations and produces [`TreeSnapshot`]s on demand.
///
/// Call [`TreeBuilder::update`], [`TreeBuilder::remove`], or
/// [`TreeBuilder::rename`] to record mutations. When
/// [`TreeBuilder::should_finalize`] returns `true` (or at any time),
/// call [`TreeBuilder::finalize`] to compute the root hash and obtain
/// an immutable snapshot.
#[derive(Debug)]
pub struct TreeBuilder {
    root: MerkleNode,
    files: BTreeMap<PathBuf, ContentHash>,
    dirty_count: u64,
    config: TreeConfig,
}

/// Immutable tree snapshot produced by [`TreeBuilder::finalize`].
///
/// Contains only the precomputed root hash and a flat file index.
/// The persistent [`MerkleNode`] tree stays internal to [`TreeBuilder`].
#[derive(Debug, Clone)]
pub struct TreeSnapshot {
    /// Precomputed root hash at the time of finalization.
    pub root_hash: ContentHash,
    /// Flat mapping from file paths to their content hashes.
    pub files: BTreeMap<PathBuf, ContentHash>,
}

impl TreeSnapshot {
    /// Number of files in this snapshot.
    #[must_use]
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// Returns `true` if `path` exists in this snapshot.
    #[must_use]
    pub fn contains(&self, path: &Path) -> bool {
        self.files.contains_key(path)
    }

    /// Returns the content hash for `path`, or `None` if absent.
    #[must_use]
    pub fn get(&self, path: &Path) -> Option<&ContentHash> {
        self.files.get(path)
    }

    /// Iterates over all `(path, hash)` pairs in this snapshot.
    pub fn files_iter(&self) -> impl Iterator<Item = (&Path, &ContentHash)> {
        self.files.iter().map(|(p, h)| (p.as_path(), h))
    }

    /// Returns the precomputed root hash.
    #[must_use]
    pub fn root_hash(&self) -> ContentHash {
        self.root_hash.clone()
    }
}

impl TreeBuilder {
    /// Create an empty builder with the given configuration.
    #[must_use]
    pub fn new(config: TreeConfig) -> Self {
        Self {
            root: MerkleNode::empty_dir(),
            files: BTreeMap::new(),
            dirty_count: 0,
            config,
        }
    }

    /// Record a file addition or update.
    ///
    /// Performs a path-copy on the internal tree and inserts `path` into
    /// the flat files index. Increments the dirty counter.
    pub fn update(&mut self, path: PathBuf, hash: ContentHash) {
        let comps = path_components(&path);
        let leaf = Arc::new(MerkleNode::leaf(hash.clone()));
        self.root = self.root.path_copy(&comps, leaf);
        self.files.insert(path, hash);
        self.dirty_count += 1;
    }

    /// Record a file removal.
    ///
    /// Returns `true` if the path was present. Rebuilds the tree root
    /// from the flat index after removal.
    pub fn remove(&mut self, path: &Path) -> bool {
        if self.files.remove(path).is_none() {
            return false;
        }
        self.rebuild_root();
        self.dirty_count += 1;
        true
    }

    /// Rename a file from `old` to `new`.
    ///
    /// Preserves the content hash. Rebuilds the tree root from the flat
    /// index after the rename.
    pub fn rename(&mut self, old: &Path, new: PathBuf) {
        if let Some(hash) = self.files.remove(old) {
            self.files.insert(new, hash);
            self.rebuild_root();
            self.dirty_count += 1;
        }
    }

    /// Returns `true` when accumulated mutations reach the configured threshold.
    #[must_use]
    pub fn should_finalize(&self) -> bool {
        self.dirty_count >= self.config.batch_size
    }

    /// Force hash computation and return an immutable snapshot.
    ///
    /// Resets the dirty counter to zero. The internal tree is retained for
    /// subsequent mutations via structural sharing.
    pub fn finalize(&mut self) -> TreeSnapshot {
        let root_hash = self.root.hash().clone();
        self.dirty_count = 0;
        TreeSnapshot {
            root_hash,
            files: self.files.clone(),
        }
    }

    /// Force and return the current root hash without resetting the dirty counter.
    #[must_use]
    pub fn root_hash(&self) -> ContentHash {
        self.root.hash().clone()
    }

    /// Number of tracked files.
    #[must_use]
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// Returns `true` if `path` is tracked.
    #[must_use]
    pub fn contains(&self, path: &Path) -> bool {
        self.files.contains_key(path)
    }

    /// Returns the content hash for `path`, or `None` if absent.
    #[must_use]
    pub fn get(&self, path: &Path) -> Option<&ContentHash> {
        self.files.get(path)
    }

    /// Iterates over all tracked `(path, hash)` pairs.
    pub fn files(&self) -> impl Iterator<Item = (&Path, &ContentHash)> {
        self.files.iter().map(|(p, h)| (p.as_path(), h))
    }

    /// Number of mutations since the last finalization.
    #[must_use]
    pub fn dirty_count(&self) -> u64 {
        self.dirty_count
    }

    /// Reconstruct the root `MerkleNode` entirely from the flat files index.
    ///
    /// Used after removal and rename operations where the old path-copy
    /// approach would leave ghost nodes in the tree.
    fn rebuild_root(&mut self) {
        let mut root = MerkleNode::empty_dir();
        for (path, hash) in &self.files {
            let comps = path_components(path);
            let leaf = Arc::new(MerkleNode::leaf(hash.clone()));
            root = root.path_copy(&comps, leaf);
        }
        self.root = root;
    }
}

/// Extract the normal path components of `path` as string slices.
///
/// Strips root, prefix, and CurDir/ParentDir components so that only
/// meaningful directory and file name segments remain.
fn path_components(path: &Path) -> Vec<&str> {
    path.components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(data: &[u8]) -> ContentHash {
        ContentHash::from_data(data)
    }

    fn config_with_batch(batch_size: u64) -> TreeConfig {
        TreeConfig { batch_size, checkpoint_interval: 1000 }
    }

    #[test]
    fn update_and_finalize() {
        let mut b = TreeBuilder::new(TreeConfig::default());
        b.update(PathBuf::from("a.txt"), hash(b"a"));
        b.update(PathBuf::from("b.txt"), hash(b"b"));
        let snap = b.finalize();
        assert_eq!(snap.file_count(), 2);
        assert!(snap.contains(Path::new("a.txt")));
        assert!(snap.contains(Path::new("b.txt")));
    }

    #[test]
    fn should_finalize_at_threshold() {
        let mut b = TreeBuilder::new(config_with_batch(3));
        assert!(!b.should_finalize());
        b.update(PathBuf::from("a"), hash(b"a"));
        b.update(PathBuf::from("b"), hash(b"b"));
        assert!(!b.should_finalize());
        b.update(PathBuf::from("c"), hash(b"c"));
        assert!(b.should_finalize());
    }

    #[test]
    fn finalize_resets_dirty_count() {
        let mut b = TreeBuilder::new(config_with_batch(3));
        b.update(PathBuf::from("a"), hash(b"a"));
        b.update(PathBuf::from("b"), hash(b"b"));
        b.update(PathBuf::from("c"), hash(b"c"));
        assert_eq!(b.dirty_count(), 3);
        let _ = b.finalize();
        assert_eq!(b.dirty_count(), 0);
    }

    #[test]
    fn remove_file() {
        let mut b = TreeBuilder::new(TreeConfig::default());
        b.update(PathBuf::from("a.txt"), hash(b"a"));
        assert!(b.contains(Path::new("a.txt")));
        let removed = b.remove(Path::new("a.txt"));
        assert!(removed);
        assert!(!b.contains(Path::new("a.txt")));
        assert_eq!(b.file_count(), 0);
    }

    #[test]
    fn remove_absent_file_returns_false() {
        let mut b = TreeBuilder::new(TreeConfig::default());
        assert!(!b.remove(Path::new("ghost.txt")));
        assert_eq!(b.dirty_count(), 0);
    }

    #[test]
    fn rename_file() {
        let mut b = TreeBuilder::new(TreeConfig::default());
        let h = hash(b"content");
        b.update(PathBuf::from("old.txt"), h.clone());
        b.rename(Path::new("old.txt"), PathBuf::from("new.txt"));
        assert!(!b.contains(Path::new("old.txt")));
        assert_eq!(b.get(Path::new("new.txt")), Some(&h));
    }

    #[test]
    fn snapshot_is_independent() {
        let mut b = TreeBuilder::new(TreeConfig::default());
        b.update(PathBuf::from("a.txt"), hash(b"v1"));
        let snap1 = b.finalize();

        b.update(PathBuf::from("a.txt"), hash(b"v2"));
        let snap2 = b.finalize();

        assert_ne!(
            snap1.root_hash().digest(),
            snap2.root_hash().digest(),
            "snapshots must differ after mutation"
        );
        assert_eq!(snap1.get(Path::new("a.txt")), Some(&hash(b"v1")));
        assert_eq!(snap2.get(Path::new("a.txt")), Some(&hash(b"v2")));
    }

    #[test]
    fn nested_paths() {
        let mut b = TreeBuilder::new(TreeConfig::default());
        b.update(PathBuf::from("src/lib.rs"), hash(b"lib"));
        b.update(PathBuf::from("src/main.rs"), hash(b"main"));
        b.update(PathBuf::from("tests/foo.rs"), hash(b"test"));
        let snap = b.finalize();
        assert_eq!(snap.file_count(), 3);
        assert!(snap.contains(Path::new("src/lib.rs")));
        assert!(snap.contains(Path::new("tests/foo.rs")));
    }

    #[test]
    fn root_hash_changes_on_update() {
        let mut b = TreeBuilder::new(TreeConfig::default());
        let h1 = b.root_hash();
        b.update(PathBuf::from("a.txt"), hash(b"x"));
        let h2 = b.root_hash();
        assert_ne!(h1.digest(), h2.digest());
    }

    #[test]
    fn root_hash_stable_after_remove_then_readd() {
        let mut b = TreeBuilder::new(TreeConfig::default());
        b.update(PathBuf::from("a.txt"), hash(b"x"));
        let h_before = b.root_hash();
        b.remove(Path::new("a.txt"));
        b.update(PathBuf::from("a.txt"), hash(b"x"));
        let h_after = b.root_hash();
        assert_eq!(h_before.digest(), h_after.digest());
    }
}
