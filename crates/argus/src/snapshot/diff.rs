//! Tree diffing between two Merkle roots.
//!
//! Given two [`MerkleTree`] snapshots, [`diff_trees`] builds virtual
//! directory trees and recursively compares them. Subtrees with
//! identical hashes are skipped entirely, making the diff proportional
//! to the number of changed files rather than the total tree size.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::cas::ContentHash;

use super::tree::{build_dir_tree, hash_dir_node, DirNode, MerkleTree};

/// Kind of change detected between two tree snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiffKind {
    /// File exists only in the second tree.
    Added,
    /// File exists only in the first tree.
    Deleted,
    /// File exists in both trees with different content hashes.
    Modified,
}

/// A single file-level difference between two trees.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
/// Builds virtual directory trees and walks them recursively,
/// skipping entire subtrees whose hashes match. The result is
/// sorted by path.
pub fn diff_trees(tree_a: &MerkleTree, tree_b: &MerkleTree) -> Vec<DiffEntry> {
    let files_a: BTreeMap<PathBuf, ContentHash> =
        tree_a.files().map(|(p, h)| (p.to_path_buf(), *h)).collect();
    let files_b: BTreeMap<PathBuf, ContentHash> =
        tree_b.files().map(|(p, h)| (p.to_path_buf(), *h)).collect();

    let dir_a = build_dir_tree(&files_a);
    let dir_b = build_dir_tree(&files_b);

    let mut diffs = Vec::new();
    let prefix = PathBuf::new();
    diff_children(&dir_a, &dir_b, &prefix, &mut diffs);
    diffs.sort_by(|a, b| a.path.cmp(&b.path));
    diffs
}

/// Recursively diff two directory-level children maps.
///
/// When both sides have a directory with the same hash, the entire
/// subtree is skipped (Merkle subtree-skipping optimization).
fn diff_children(
    a: &BTreeMap<String, DirNode>,
    b: &BTreeMap<String, DirNode>,
    prefix: &Path,
    diffs: &mut Vec<DiffEntry>,
) {
    for (name, node_a) in a {
        let child_path = prefix.join(name);
        match b.get(name) {
            Some(node_b) => diff_nodes(node_a, node_b, &child_path, diffs),
            None => collect_all(node_a, &child_path, DiffKind::Deleted, true, diffs),
        }
    }

    for (name, node_b) in b {
        if !a.contains_key(name) {
            let child_path = prefix.join(name);
            collect_all(node_b, &child_path, DiffKind::Added, false, diffs);
        }
    }
}

/// Compare two nodes at the same path, skipping equal subtrees.
fn diff_nodes(
    a: &DirNode,
    b: &DirNode,
    path: &Path,
    diffs: &mut Vec<DiffEntry>,
) {
    match (a, b) {
        (DirNode::File(ha), DirNode::File(hb)) => {
            if ha != hb {
                diffs.push(DiffEntry {
                    path: path.to_path_buf(),
                    kind: DiffKind::Modified,
                    old_hash: Some(*ha),
                    new_hash: Some(*hb),
                });
            }
        }
        (DirNode::Dir(ca), DirNode::Dir(cb)) => {
            // Merkle subtree-skipping: only recurse when hashes differ
            if hash_dir_node(ca) != hash_dir_node(cb) {
                diff_children(ca, cb, path, diffs);
            }
        }
        (DirNode::File(_), DirNode::Dir(_))
        | (DirNode::Dir(_), DirNode::File(_)) => {
            // Type changed: treat old as deleted, new as added.
            collect_all(a, path, DiffKind::Deleted, true, diffs);
            collect_all(b, path, DiffKind::Added, false, diffs);
        }
    }
}

/// Recursively collect all files under a node as a single diff kind.
fn collect_all(
    node: &DirNode,
    path: &Path,
    kind: DiffKind,
    is_old: bool,
    diffs: &mut Vec<DiffEntry>,
) {
    match node {
        DirNode::File(h) => {
            diffs.push(DiffEntry {
                path: path.to_path_buf(),
                kind,
                old_hash: if is_old { Some(*h) } else { None },
                new_hash: if is_old { None } else { Some(*h) },
            });
        }
        DirNode::Dir(children) => {
            for (name, child) in children {
                collect_all(child, &path.join(name), kind, is_old, diffs);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use crate::cas::ContentHash;

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
        b.rename(
            Path::new("old_name.txt"),
            PathBuf::from("new_name.txt"),
        );

        let d = diff_trees(&a, &b);
        assert_eq!(d.len(), 2);

        let deleted = d.iter().find(|e| e.kind == DiffKind::Deleted).unwrap();
        assert_eq!(deleted.path, PathBuf::from("old_name.txt"));

        let added = d.iter().find(|e| e.kind == DiffKind::Added).unwrap();
        assert_eq!(added.path, PathBuf::from("new_name.txt"));
    }

    #[test]
    fn subtree_skipping_skips_identical_dirs() {
        // Two trees sharing an identical subdirectory should skip it.
        let mut a = MerkleTree::new();
        a.update(PathBuf::from("shared/x.txt"), hash("x"));
        a.update(PathBuf::from("shared/y.txt"), hash("y"));
        a.update(PathBuf::from("changed/a.txt"), hash("v1"));

        let mut b = MerkleTree::new();
        b.update(PathBuf::from("shared/x.txt"), hash("x"));
        b.update(PathBuf::from("shared/y.txt"), hash("y"));
        b.update(PathBuf::from("changed/a.txt"), hash("v2"));

        let d = diff_trees(&a, &b);
        // Only the changed file should appear, not the shared ones.
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].path, PathBuf::from("changed/a.txt"));
        assert_eq!(d[0].kind, DiffKind::Modified);
    }

    #[test]
    fn diff_kind_is_copy() {
        let k = DiffKind::Added;
        let k2 = k;
        assert_eq!(k, k2);
    }

    #[test]
    fn diff_entry_is_hashable() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(DiffEntry {
            path: PathBuf::from("a.txt"),
            kind: DiffKind::Added,
            old_hash: None,
            new_hash: Some(hash("x")),
        });
        assert_eq!(set.len(), 1);
    }
}

// Rust guideline compliant 2026-02-21
