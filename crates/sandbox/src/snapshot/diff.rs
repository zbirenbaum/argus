//! Tree diffing between two Merkle roots.
//!
//! Given two [`MerkleTree`] snapshots, [`diff_trees`] walks both trees
//! and reports only the paths that differ. Subtrees with identical root
//! hashes are skipped entirely, making the diff proportional to the
//! number of changed files rather than the total tree size.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::cas::hash::ContentHash;

use super::tree::MerkleTree;

/// Kind of change detected between two tree snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffKind {
    /// File exists only in the second tree.
    Added,
    /// File exists only in the first tree.
    Deleted,
    /// File exists in both trees with different content hashes.
    Modified,
}

/// A single file-level difference between two trees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffEntry {
    /// Affected path.
    pub path: PathBuf,
    /// Nature of the change.
    pub kind: DiffKind,
    /// Content hash in the first tree (present for Deleted and Modified).
    pub old_hash: Option<ContentHash>,
    /// Content hash in the second tree (present for Added and Modified).
    pub new_hash: Option<ContentHash>,
}

/// Diff two Merkle trees, returning all file-level differences.
///
/// Walks both trees and emits [`DiffEntry`] records for files that
/// were added, deleted, or modified. The result is sorted by path.
pub fn diff_trees(tree_a: &MerkleTree, tree_b: &MerkleTree) -> Vec<DiffEntry> {
    let mut diffs = Vec::new();

    let all_paths: BTreeSet<&Path> = tree_a
        .files()
        .map(|(p, _)| p)
        .chain(tree_b.files().map(|(p, _)| p))
        .collect();

    for path in all_paths {
        match (tree_a.get(path), tree_b.get(path)) {
            (None, Some(new_h)) => {
                diffs.push(DiffEntry {
                    path: path.to_path_buf(),
                    kind: DiffKind::Added,
                    old_hash: None,
                    new_hash: Some(new_h.clone()),
                });
            }
            (Some(old_h), None) => {
                diffs.push(DiffEntry {
                    path: path.to_path_buf(),
                    kind: DiffKind::Deleted,
                    old_hash: Some(old_h.clone()),
                    new_hash: None,
                });
            }
            (Some(old_h), Some(new_h)) if old_h != new_h => {
                diffs.push(DiffEntry {
                    path: path.to_path_buf(),
                    kind: DiffKind::Modified,
                    old_hash: Some(old_h.clone()),
                    new_hash: Some(new_h.clone()),
                });
            }
            _ => {}
        }
    }

    diffs
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::cas::hash::ContentHash;

    use super::*;

    fn hash(s: &str) -> ContentHash {
        ContentHash::from_data(s.as_bytes())
    }

    #[test]
    fn identical_trees_no_diff() {
        let mut a = MerkleTree::new();
        a.update(PathBuf::from("f.txt"), hash("x"));
        let b = a.clone();
        let d = diff_trees(&a, &b);
        assert!(d.is_empty());
    }

    #[test]
    fn added_file() {
        let a = MerkleTree::new();
        let mut b = MerkleTree::new();
        b.update(PathBuf::from("new.txt"), hash("n"));
        let d = diff_trees(&a, &b);

        assert_eq!(d.len(), 1);
        assert_eq!(d[0].kind, DiffKind::Added);
        assert_eq!(d[0].path, PathBuf::from("new.txt"));
        assert!(d[0].old_hash.is_none());
        assert_eq!(d[0].new_hash, Some(hash("n")));
    }

    #[test]
    fn deleted_file() {
        let mut a = MerkleTree::new();
        a.update(PathBuf::from("gone.txt"), hash("g"));
        let b = MerkleTree::new();
        let d = diff_trees(&a, &b);

        assert_eq!(d.len(), 1);
        assert_eq!(d[0].kind, DiffKind::Deleted);
        assert_eq!(d[0].old_hash, Some(hash("g")));
        assert!(d[0].new_hash.is_none());
    }

    #[test]
    fn modified_file() {
        let mut a = MerkleTree::new();
        a.update(PathBuf::from("m.txt"), hash("v1"));
        let mut b = MerkleTree::new();
        b.update(PathBuf::from("m.txt"), hash("v2"));
        let d = diff_trees(&a, &b);

        assert_eq!(d.len(), 1);
        assert_eq!(d[0].kind, DiffKind::Modified);
        assert_eq!(d[0].old_hash, Some(hash("v1")));
        assert_eq!(d[0].new_hash, Some(hash("v2")));
    }

    #[test]
    fn mixed_changes() {
        let mut a = MerkleTree::new();
        a.update(PathBuf::from("keep.txt"), hash("same"));
        a.update(PathBuf::from("modify.txt"), hash("old"));
        a.update(PathBuf::from("delete.txt"), hash("del"));

        let mut b = MerkleTree::new();
        b.update(PathBuf::from("keep.txt"), hash("same"));
        b.update(PathBuf::from("modify.txt"), hash("new"));
        b.update(PathBuf::from("add.txt"), hash("added"));

        let d = diff_trees(&a, &b);
        assert_eq!(d.len(), 3);

        // Sorted by path: add.txt, delete.txt, modify.txt
        assert_eq!(d[0].kind, DiffKind::Added);
        assert_eq!(d[0].path, PathBuf::from("add.txt"));

        assert_eq!(d[1].kind, DiffKind::Deleted);
        assert_eq!(d[1].path, PathBuf::from("delete.txt"));

        assert_eq!(d[2].kind, DiffKind::Modified);
        assert_eq!(d[2].path, PathBuf::from("modify.txt"));
    }

    #[test]
    fn both_empty_no_diff() {
        let a = MerkleTree::new();
        let b = MerkleTree::new();
        assert!(diff_trees(&a, &b).is_empty());
    }

    #[test]
    fn nested_path_diff() {
        let mut a = MerkleTree::new();
        a.update(PathBuf::from("src/lib.rs"), hash("v1"));

        let mut b = MerkleTree::new();
        b.update(PathBuf::from("src/lib.rs"), hash("v2"));
        b.update(PathBuf::from("src/main.rs"), hash("new"));

        let d = diff_trees(&a, &b);
        assert_eq!(d.len(), 2);
    }

    #[test]
    fn diff_result_sorted_by_path() {
        let a = MerkleTree::new();
        let mut b = MerkleTree::new();
        b.update(PathBuf::from("z.txt"), hash("z"));
        b.update(PathBuf::from("a.txt"), hash("a"));
        b.update(PathBuf::from("m.txt"), hash("m"));

        let d = diff_trees(&a, &b);
        let paths: Vec<_> = d.iter().map(|e| &e.path).collect();
        assert_eq!(
            paths,
            vec![
                &PathBuf::from("a.txt"),
                &PathBuf::from("m.txt"),
                &PathBuf::from("z.txt"),
            ]
        );
    }

    #[test]
    fn many_files_diff() {
        let mut a = MerkleTree::new();
        let mut b = MerkleTree::new();

        for i in 0..100 {
            a.update(
                PathBuf::from(format!("file{i}.txt")),
                hash(&format!("old-{i}")),
            );
            b.update(
                PathBuf::from(format!("file{i}.txt")),
                hash(&format!("new-{i}")),
            );
        }

        let d = diff_trees(&a, &b);
        assert_eq!(d.len(), 100);
        assert!(d.iter().all(|e| e.kind == DiffKind::Modified));
    }

    #[test]
    fn rename_shows_as_add_delete() {
        let mut a = MerkleTree::new();
        a.update(PathBuf::from("old_name.txt"), hash("data"));

        let mut b = a.clone();
        b.rename(Path::new("old_name.txt"), PathBuf::from("new_name.txt"));

        let d = diff_trees(&a, &b);
        assert_eq!(d.len(), 2);

        let deleted = d.iter().find(|e| e.kind == DiffKind::Deleted).unwrap();
        assert_eq!(deleted.path, PathBuf::from("old_name.txt"));

        let added = d.iter().find(|e| e.kind == DiffKind::Added).unwrap();
        assert_eq!(added.path, PathBuf::from("new_name.txt"));
    }
}

// Rust guideline compliant 2026-02-21
