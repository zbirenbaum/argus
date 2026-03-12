// Rust guideline compliant 2026-02-21
//! Restore filesystem state from a Merkle tree snapshot.
//!
//! Provides two restore modes:
//! - **Full restore**: writes all files from a tree snapshot to a target
//!   directory, producing a complete point-in-time copy of the workspace.
//! - **Selective restore**: writes only the specified paths from the tree,
//!   useful for recovering individual files without a full snapshot.
//!
//! Both modes pull file content from the CAS backend (local or tiered)
//! and write atomically via temp-file-then-rename to prevent partial
//! writes on failure.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::cas::{Cas, ContentHash};
use crate::snapshot::MerkleTree;

/// Statistics from a restore operation.
#[derive(Debug, Clone, Default)]
pub struct RestoreStats {
    /// Number of files successfully restored.
    pub files_restored: u64,
    /// Total bytes written across all restored files.
    pub bytes_restored: u64,
}

/// Restore all files from `tree` into `target_dir`.
///
/// Creates `target_dir` if it does not exist. Each file is written
/// atomically: content goes to a `.tmp` sibling, then renamed into
/// place. Directory structure mirrors the tree paths.
///
/// # Errors
///
/// Returns an error if CAS content is missing, directory creation
/// fails, or any file write/rename fails.
pub fn restore_full(
    tree: &MerkleTree,
    cas: &dyn Cas,
    target_dir: &Path,
) -> Result<RestoreStats> {
    fs::create_dir_all(target_dir)
        .with_context(|| format!("create target dir: {}", target_dir.display()))?;

    let mut stats = RestoreStats::default();

    for (path, hash) in tree.files() {
        let bytes = write_file(path, hash, cas, target_dir)?;
        stats.files_restored += 1;
        stats.bytes_restored += bytes;
    }

    Ok(stats)
}

/// Restore specific files from `tree` into `target_dir`.
///
/// Only paths present in both `paths` and the tree are restored.
/// Paths not found in the tree are silently skipped.
///
/// # Errors
///
/// Returns an error if CAS content is missing for a matched path,
/// or any filesystem operation fails.
pub fn restore_selective(
    tree: &MerkleTree,
    paths: &[PathBuf],
    cas: &dyn Cas,
    target_dir: &Path,
) -> Result<RestoreStats> {
    fs::create_dir_all(target_dir)
        .with_context(|| format!("create target dir: {}", target_dir.display()))?;

    let mut stats = RestoreStats::default();

    for path in paths {
        let Some(hash) = tree.get(path) else {
            continue;
        };
        let bytes = write_file(path, hash, cas, target_dir)?;
        stats.files_restored += 1;
        stats.bytes_restored += bytes;
    }

    Ok(stats)
}

/// Write a single file from CAS to the target directory.
///
/// Returns the number of bytes written.
fn write_file(
    path: &Path,
    hash: &ContentHash,
    cas: &dyn Cas,
    target_dir: &Path,
) -> Result<u64> {
    let content = cas.get(hash).with_context(|| {
        format!("fetch content for {}: hash {hash}", path.display())
    })?;

    let dest = target_dir.join(path);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!("create parent dirs for {}", dest.display())
        })?;
    }

    let tmp = dest.with_extension("tmp");
    fs::write(&tmp, &content)
        .with_context(|| format!("write temp file {}", tmp.display()))?;
    fs::rename(&tmp, &dest)
        .with_context(|| format!("rename {} -> {}", tmp.display(), dest.display()))?;

    Ok(content.len() as u64)
}

/// Load a tree from CAS by its root hash and restore to `target_dir`.
///
/// Convenience wrapper that combines `MerkleTree::load` with
/// `restore_full`. Used by the API when restoring to a past seq.
///
/// # Errors
///
/// Returns an error if the tree hash is not found in CAS, tree
/// deserialization fails, or any restore I/O fails.
pub fn restore_from_hash(
    tree_hash: &ContentHash,
    cas: &dyn Cas,
    target_dir: &Path,
) -> Result<RestoreStats> {
    let tree = MerkleTree::load(tree_hash, cas)
        .with_context(|| format!("load tree from CAS: {tree_hash}"))?;
    restore_full(&tree, cas, target_dir)
}

/// Load a tree from CAS and restore only specific paths.
///
/// # Errors
///
/// Returns an error if tree loading or file restoration fails.
pub fn restore_selective_from_hash(
    tree_hash: &ContentHash,
    paths: &[PathBuf],
    cas: &dyn Cas,
    target_dir: &Path,
) -> Result<RestoreStats> {
    let tree = MerkleTree::load(tree_hash, cas)
        .with_context(|| format!("load tree from CAS: {tree_hash}"))?;
    restore_selective(&tree, paths, cas, target_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cas::MemoryCas;

    fn setup_tree_with_files(cas: &MemoryCas) -> MerkleTree {
        let mut tree = MerkleTree::new();

        let h1 = cas.put(b"hello world").unwrap();
        tree.update(PathBuf::from("file1.txt"), h1);

        let h2 = cas.put(b"nested content").unwrap();
        tree.update(PathBuf::from("dir/file2.txt"), h2);

        let h3 = cas.put(b"deep file").unwrap();
        tree.update(PathBuf::from("a/b/c/file3.txt"), h3);

        tree
    }

    #[test]
    fn full_restore_creates_all_files() {
        let cas = MemoryCas::new();
        let tree = setup_tree_with_files(&cas);
        let dir = tempfile::tempdir().unwrap();

        let stats = restore_full(&tree, &cas, dir.path()).unwrap();

        assert_eq!(stats.files_restored, 3);
        assert_eq!(
            fs::read_to_string(dir.path().join("file1.txt")).unwrap(),
            "hello world"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("dir/file2.txt")).unwrap(),
            "nested content"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("a/b/c/file3.txt")).unwrap(),
            "deep file"
        );
    }

    #[test]
    fn full_restore_byte_count() {
        let cas = MemoryCas::new();
        let tree = setup_tree_with_files(&cas);
        let dir = tempfile::tempdir().unwrap();

        let stats = restore_full(&tree, &cas, dir.path()).unwrap();

        let expected_bytes = b"hello world".len()
            + b"nested content".len()
            + b"deep file".len();
        assert_eq!(stats.bytes_restored, expected_bytes as u64);
    }

    #[test]
    fn selective_restore_picks_only_matching_paths() {
        let cas = MemoryCas::new();
        let tree = setup_tree_with_files(&cas);
        let dir = tempfile::tempdir().unwrap();

        let paths = vec![PathBuf::from("file1.txt")];
        let stats = restore_selective(&tree, &paths, &cas, dir.path()).unwrap();

        assert_eq!(stats.files_restored, 1);
        assert!(dir.path().join("file1.txt").exists());
        assert!(!dir.path().join("dir/file2.txt").exists());
    }

    #[test]
    fn selective_restore_skips_missing_paths() {
        let cas = MemoryCas::new();
        let tree = setup_tree_with_files(&cas);
        let dir = tempfile::tempdir().unwrap();

        let paths = vec![
            PathBuf::from("file1.txt"),
            PathBuf::from("nonexistent.txt"),
        ];
        let stats = restore_selective(&tree, &paths, &cas, dir.path()).unwrap();

        assert_eq!(stats.files_restored, 1);
    }

    #[test]
    fn restore_creates_target_dir_if_missing() {
        let cas = MemoryCas::new();
        let tree = setup_tree_with_files(&cas);
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("does/not/exist");

        let stats = restore_full(&tree, &cas, &nested).unwrap();
        assert_eq!(stats.files_restored, 3);
    }

    #[test]
    fn empty_tree_restores_nothing() {
        let cas = MemoryCas::new();
        let tree = MerkleTree::new();
        let dir = tempfile::tempdir().unwrap();

        let stats = restore_full(&tree, &cas, dir.path()).unwrap();
        assert_eq!(stats.files_restored, 0);
        assert_eq!(stats.bytes_restored, 0);
    }

    #[test]
    fn restore_from_cas_hash() {
        let cas = MemoryCas::new();
        let mut tree = MerkleTree::new();
        let h = cas.put(b"content").unwrap();
        tree.update(PathBuf::from("f.txt"), h);

        let root_hash = tree.store(&cas).unwrap();
        let dir = tempfile::tempdir().unwrap();

        let stats = restore_from_hash(&root_hash, &cas, dir.path()).unwrap();
        assert_eq!(stats.files_restored, 1);
        assert_eq!(
            fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "content"
        );
    }

    #[test]
    fn selective_restore_from_cas_hash() {
        let cas = MemoryCas::new();
        let mut tree = MerkleTree::new();
        let h1 = cas.put(b"alpha").unwrap();
        let h2 = cas.put(b"beta").unwrap();
        tree.update(PathBuf::from("a.txt"), h1);
        tree.update(PathBuf::from("b.txt"), h2);

        let root_hash = tree.store(&cas).unwrap();
        let dir = tempfile::tempdir().unwrap();

        let paths = vec![PathBuf::from("b.txt")];
        let stats =
            restore_selective_from_hash(&root_hash, &paths, &cas, dir.path())
                .unwrap();
        assert_eq!(stats.files_restored, 1);
        assert!(!dir.path().join("a.txt").exists());
        assert_eq!(
            fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            "beta"
        );
    }
}
