// Rust guideline compliant 2026-02-21

//! Persistent Merkle tree node with structural sharing.
//!
//! Uses `Arc`-based path-copy so mutations produce a new root while
//! sharing all unmodified subtrees. Hash computation is deferred via
//! `OnceLock` — dirty nodes stay unhashed until a reader forces
//! resolution, amortizing cost across burst mutations.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use crate::cas::ContentHash;

/// A node in the persistent Merkle tree.
///
/// Nodes are immutable once constructed. Mutations produce new nodes
/// via `path_copy`, sharing children via `Arc`.
///
/// NOTE: `Arc` usage here is approved — nodes are genuinely shared
/// across multiple tree snapshots (the entire point of structural
/// sharing). This is not a lazy substitute for ownership.
pub struct MerkleNode {
    hash: OnceLock<ContentHash>,
    kind: NodeKind,
}

enum NodeKind {
    Leaf { content_hash: ContentHash },
    Dir { children: Arc<BTreeMap<String, Arc<MerkleNode>>> },
}

impl std::fmt::Debug for MerkleNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            NodeKind::Leaf { content_hash } => {
                f.debug_struct("Leaf").field("hash", content_hash).finish()
            }
            NodeKind::Dir { children } => {
                f.debug_struct("Dir").field("children", &children.len()).finish()
            }
        }
    }
}

impl MerkleNode {
    /// Create a leaf node pre-seeded with its content hash.
    pub fn leaf(content_hash: ContentHash) -> Self {
        let lock = OnceLock::new();
        // Pre-set so callers never pay for re-computation on leaf nodes.
        lock.set(content_hash.clone())
            .expect("freshly constructed OnceLock must be empty");
        Self {
            hash: lock,
            kind: NodeKind::Leaf { content_hash },
        }
    }

    /// Create an empty directory node.
    pub fn empty_dir() -> Self {
        Self {
            hash: OnceLock::new(),
            kind: NodeKind::Dir {
                children: Arc::new(BTreeMap::new()),
            },
        }
    }

    /// Create a directory node from an existing children map.
    pub fn dir(children: BTreeMap<String, Arc<MerkleNode>>) -> Self {
        Self {
            hash: OnceLock::new(),
            kind: NodeKind::Dir {
                children: Arc::new(children),
            },
        }
    }

    /// Return a new directory node with `child` inserted under `name`.
    ///
    /// Path-copies this node's children map; unmodified siblings share their
    /// `Arc` allocations with the old node.
    ///
    /// # Panics
    ///
    /// Panics if called on a leaf node (leaf nodes have no children).
    pub fn with_child(&self, name: &str, child: Arc<MerkleNode>) -> Self {
        let NodeKind::Dir { children } = &self.kind else {
            panic!("with_child called on a leaf node");
        };
        let mut new_children: BTreeMap<String, Arc<MerkleNode>> = (**children).clone();
        new_children.insert(name.to_owned(), child);
        Self::dir(new_children)
    }

    /// Look up a direct child by name.
    pub fn get_child(&self, name: &str) -> Option<Arc<MerkleNode>> {
        let NodeKind::Dir { children } = &self.kind else {
            return None;
        };
        children.get(name).cloned()
    }

    /// Return a new directory node with `name` removed.
    ///
    /// # Panics
    ///
    /// Panics if called on a leaf node.
    pub fn without_child(&self, name: &str) -> Self {
        let NodeKind::Dir { children } = &self.kind else {
            panic!("without_child called on a leaf node");
        };
        let mut new_children: BTreeMap<String, Arc<MerkleNode>> = (**children).clone();
        new_children.remove(name);
        Self::dir(new_children)
    }

    /// Recursively path-copy down `components`, placing `new_leaf` at the end.
    ///
    /// Intermediate directories are created when missing. Returns a new root
    /// that shares all unaffected subtrees with the original.
    pub fn path_copy(&self, components: &[&str], new_leaf: Arc<MerkleNode>) -> Self {
        if let Some((&head, tail)) = components.split_first() {
            let existing_child = self.get_child(head);
            let updated_child = if tail.is_empty() {
                new_leaf
            } else {
                let placeholder;
                let subtree: &MerkleNode = match existing_child.as_deref() {
                    Some(n) => n,
                    None => {
                        placeholder = Self::empty_dir();
                        &placeholder
                    }
                };
                Arc::new(subtree.path_copy(tail, new_leaf))
            };
            self.with_child(head, updated_child)
        } else {
            // Empty path — replace this node wholesale.
            Arc::try_unwrap(new_leaf).unwrap_or_else(|arc| (*arc).clone_node())
        }
    }

    /// Lazily compute and return this node's hash.
    ///
    /// Leaf nodes return the stored content hash. Directory nodes hash
    /// sorted `name\0child_hash\n` pairs; an empty directory hashes
    /// the sentinel `b"empty-tree"`.
    pub fn hash(&self) -> &ContentHash {
        self.hash.get_or_init(|| self.compute_hash())
    }

    /// Returns `true` if this node is a directory.
    pub fn is_dir(&self) -> bool {
        matches!(self.kind, NodeKind::Dir { .. })
    }

    fn compute_hash(&self) -> ContentHash {
        match &self.kind {
            NodeKind::Leaf { content_hash } => content_hash.clone(),
            NodeKind::Dir { children } => {
                let mut hasher_input = Vec::new();
                for (name, child) in children.iter() {
                    let child_hash = child.hash();
                    hasher_input.extend_from_slice(name.as_bytes());
                    hasher_input.push(0);
                    hasher_input.extend_from_slice(child_hash.digest().as_bytes());
                    hasher_input.push(b'\n');
                }
                if hasher_input.is_empty() {
                    ContentHash::from_data(b"empty-tree")
                } else {
                    ContentHash::from_data(&hasher_input)
                }
            }
        }
    }

    /// Clone-by-value helper used only when `Arc::try_unwrap` cannot take ownership.
    fn clone_node(&self) -> Self {
        match &self.kind {
            NodeKind::Leaf { content_hash } => Self::leaf(content_hash.clone()),
            NodeKind::Dir { children } => Self {
                hash: OnceLock::new(),
                kind: NodeKind::Dir {
                    children: Arc::clone(children),
                },
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_hash(data: &[u8]) -> ContentHash {
        ContentHash::from_data(data)
    }

    #[test]
    fn empty_dir_has_deterministic_hash() {
        let a = MerkleNode::empty_dir();
        let b = MerkleNode::empty_dir();
        assert_eq!(a.hash().digest(), b.hash().digest());
    }

    #[test]
    fn leaf_hash_matches_content_hash() {
        let ch = make_hash(b"hello");
        let node = MerkleNode::leaf(ch.clone());
        assert_eq!(node.hash().digest(), ch.digest());
    }

    #[test]
    fn path_copy_creates_new_root() {
        let root = MerkleNode::empty_dir();
        let old_hash = root.hash().digest();

        let leaf = Arc::new(MerkleNode::leaf(make_hash(b"content")));
        let new_root = root.path_copy(&["file.txt"], leaf);

        assert_ne!(new_root.hash().digest(), old_hash);
    }

    #[test]
    fn path_copy_shares_siblings() {
        // Build a root with two children: "a" and "b".
        let child_a = Arc::new(MerkleNode::leaf(make_hash(b"aaa")));
        let child_b = Arc::new(MerkleNode::leaf(make_hash(b"bbb")));
        let root = MerkleNode::empty_dir()
            .with_child("a", Arc::clone(&child_a))
            .with_child("b", Arc::clone(&child_b));

        // Replace "a" with a new leaf.
        let new_leaf = Arc::new(MerkleNode::leaf(make_hash(b"new-a")));
        let new_root = root.path_copy(&["a"], new_leaf);

        // Sibling "b" must share its Arc allocation.
        let retained_b = new_root.get_child("b").expect("b must exist");
        assert!(Arc::ptr_eq(&retained_b, &child_b), "sibling b should be shared");
    }

    #[test]
    fn lazy_hash_computed_once() {
        let node = MerkleNode::empty_dir();
        let h1 = node.hash() as *const ContentHash;
        let h2 = node.hash() as *const ContentHash;
        // Same pointer means OnceLock returned the same stored value.
        assert_eq!(h1, h2);
    }

    #[test]
    fn nested_path_copy() {
        let root = MerkleNode::empty_dir();
        let leaf = Arc::new(MerkleNode::leaf(make_hash(b"nested content")));
        let new_root = root.path_copy(&["dir", "file.txt"], leaf);

        let dir_node = new_root.get_child("dir").expect("dir must exist");
        assert!(dir_node.is_dir());
        let file_node = dir_node.get_child("file.txt").expect("file.txt must exist");
        assert!(!file_node.is_dir());
        assert_eq!(file_node.hash().digest(), make_hash(b"nested content").digest());
    }
}
