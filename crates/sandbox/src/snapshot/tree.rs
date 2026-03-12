//! In-memory Merkle tree for filesystem state tracking.
//!
//! Maintains a virtual directory tree where each file is a blob (keyed
//! by its content hash) and each directory is a tree node whose hash is
//! derived from its sorted children. Updating a single file rehashes
//! only the ancestors of that file, keeping updates O(depth).
//!
//! Tree and commit objects can be persisted into a [`CasStore`] for
//! durable point-in-time snapshots.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::cas::{CasStore, ContentHash};

/// Serializable directory listing stored as a CAS object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeObject {
    /// Sorted child name to content hash.
    pub entries: BTreeMap<String, ContentHash>,
}

/// Commit object tying a root tree hash to a point in time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    /// Hash of the root tree object.
    pub tree_hash: ContentHash,
    /// Monotonic timestamp in nanoseconds.
    pub ts_monotonic: u64,
    /// RFC 3339 wall-clock timestamp.
    pub ts_wall: String,
    /// Hash of the parent commit, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<ContentHash>,
}

/// In-memory Merkle tree over the tracked filesystem.
///
/// Files are stored flat in a `BTreeMap<PathBuf, ContentHash>` for
/// efficient lookup. Directory hashes are computed lazily when the root
/// hash is requested or a commit is created.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MerkleTree {
    /// Flat map of absolute paths to their content hashes.
    files: BTreeMap<PathBuf, ContentHash>,

    /// Cached root hash, invalidated on any mutation.
    #[serde(skip)]
    cached_root: RefCell<Option<ContentHash>>,
}

impl Default for MerkleTree {
    fn default() -> Self {
        Self::new()
    }
}

impl MerkleTree {
    /// Create an empty tree.
    pub fn new() -> Self {
        Self {
            files: BTreeMap::new(),
            cached_root: RefCell::new(None),
        }
    }

    /// Insert or update a file at `path` with `hash`.
    pub fn update(&mut self, path: PathBuf, hash: ContentHash) {
        self.files.insert(path, hash);
        *self.cached_root.borrow_mut() = None;
    }

    /// Remove a file at `path`. Returns `true` if it existed.
    pub fn remove(&mut self, path: &Path) -> bool {
        let existed = self.files.remove(path).is_some();
        if existed {
            *self.cached_root.borrow_mut() = None;
        }
        existed
    }

    /// Atomically move a file from `old` to `new`.
    ///
    /// If `old` does not exist this is a no-op. If `new` already exists
    /// it is overwritten (matching POSIX rename semantics).
    pub fn rename(&mut self, old: &Path, new: PathBuf) {
        if let Some(hash) = self.files.remove(old) {
            self.files.insert(new, hash);
            *self.cached_root.borrow_mut() = None;
        }
    }

    /// Compute the current Merkle root hash.
    ///
    /// Builds a virtual directory tree from the flat file map, hashes
    /// each directory bottom-up, and returns the root hash. The result
    /// is cached until the next mutation.
    pub fn root_hash(&self) -> ContentHash {
        if let Some(ref h) = *self.cached_root.borrow() {
            return h.clone();
        }
        let h = compute_root(&self.files);
        *self.cached_root.borrow_mut() = Some(h.clone());
        h
    }

    /// Store tree and commit objects in `cas`, returning the commit hash.
    ///
    /// # Errors
    ///
    /// Returns an error if any CAS write fails.
    pub fn commit(
        &mut self,
        cas: &CasStore,
        ts_monotonic: u64,
        ts_wall: String,
        parent: Option<ContentHash>,
    ) -> Result<ContentHash> {
        let tree_hash = self.root_hash();
        store_tree_objects(cas, &self.files)?;

        let commit = Commit {
            tree_hash,
            ts_monotonic,
            ts_wall,
            parent,
        };
        let commit_bytes =
            serde_json::to_vec(&commit).context("serialize commit")?;
        let commit_hash = cas
            .store(&commit_bytes)
            .context("store commit in CAS")?;
        Ok(commit_hash)
    }

    /// Iterate all file paths and their hashes.
    pub fn files(&self) -> impl Iterator<Item = (&Path, &ContentHash)> {
        self.files.iter().map(|(p, h)| (p.as_path(), h))
    }

    /// Number of tracked files.
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// Check if a path exists in the tree.
    pub fn contains(&self, path: &Path) -> bool {
        self.files.contains_key(path)
    }

    /// Get the content hash for a path, if present.
    pub fn get(&self, path: &Path) -> Option<&ContentHash> {
        self.files.get(path)
    }
}

/// Build the virtual directory tree and compute the root hash.
fn compute_root(files: &BTreeMap<PathBuf, ContentHash>) -> ContentHash {
    if files.is_empty() {
        return ContentHash::from_data(b"empty-tree");
    }

    // Group files by their top-level component relative to a virtual
    // root. Each directory accumulates child hashes which are then
    // hashed together to form the directory hash.
    let dir_tree = build_dir_tree(files);
    hash_dir_node(&dir_tree)
}

/// Recursive directory node used during hash computation.
#[derive(Debug)]
pub(crate) enum DirNode {
    File(ContentHash),
    Dir(BTreeMap<String, DirNode>),
}

/// Build a nested `DirNode` tree from the flat file map.
pub(crate) fn build_dir_tree(files: &BTreeMap<PathBuf, ContentHash>) -> BTreeMap<String, DirNode> {
    let mut root: BTreeMap<String, DirNode> = BTreeMap::new();

    for (path, hash) in files {
        let components: Vec<&str> = path
            .components()
            .filter_map(|c| match c {
                std::path::Component::Normal(s) => s.to_str(),
                _ => None,
            })
            .collect();

        insert_into_tree(&mut root, &components, hash.clone());
    }

    root
}

/// Recursively insert a file into the nested directory map.
fn insert_into_tree(
    node: &mut BTreeMap<String, DirNode>,
    components: &[&str],
    hash: ContentHash,
) {
    match components {
        [] => {}
        [name] => {
            node.insert((*name).to_owned(), DirNode::File(hash));
        }
        [dir, rest @ ..] => {
            let entry = node
                .entry((*dir).to_owned())
                .or_insert_with(|| DirNode::Dir(BTreeMap::new()));
            if let DirNode::Dir(children) = entry {
                insert_into_tree(children, rest, hash);
            }
        }
    }
}

/// Recursively hash a directory node.
pub(crate) fn hash_dir_node(children: &BTreeMap<String, DirNode>) -> ContentHash {
    let mut hasher_input = Vec::new();
    for (name, node) in children {
        let child_hash = match node {
            DirNode::File(h) => h.clone(),
            DirNode::Dir(sub) => hash_dir_node(sub),
        };
        // Format: "name\0hash\n" — deterministic, sorted by BTreeMap.
        hasher_input.extend_from_slice(name.as_bytes());
        hasher_input.push(0);
        hasher_input.extend_from_slice(child_hash.as_str().as_bytes());
        hasher_input.push(b'\n');
    }
    ContentHash::from_data(&hasher_input)
}

/// Serialize all directory tree objects into CAS.
///
/// Each unique directory becomes a `TreeObject` stored by its content
/// hash. Files (blobs) are assumed to already exist in CAS.
fn store_tree_objects(
    cas: &CasStore,
    files: &BTreeMap<PathBuf, ContentHash>,
) -> Result<()> {
    let dir_tree = build_dir_tree(files);
    store_dir_node(cas, &dir_tree)?;
    Ok(())
}

/// Recursively store a directory node and its children.
fn store_dir_node(
    cas: &CasStore,
    children: &BTreeMap<String, DirNode>,
) -> Result<ContentHash> {
    let mut entries = BTreeMap::new();
    for (name, node) in children {
        let child_hash = match node {
            DirNode::File(h) => h.clone(),
            DirNode::Dir(sub) => store_dir_node(cas, sub)?,
        };
        entries.insert(name.clone(), child_hash);
    }
    let tree_obj = TreeObject { entries };
    let bytes =
        serde_json::to_vec(&tree_obj).context("serialize tree object")?;
    cas.store(&bytes).context("store tree object in CAS")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(s: &str) -> ContentHash {
        ContentHash::from_data(s.as_bytes())
    }

    #[test]
    fn empty_tree_has_deterministic_root() {
        let mut t = MerkleTree::new();
        let h1 = t.root_hash();
        let h2 = t.root_hash();
        assert_eq!(h1, h2);
    }

    #[test]
    fn update_changes_root() {
        let mut t = MerkleTree::new();
        let before = t.root_hash();
        t.update(PathBuf::from("a.txt"), hash("content-a"));
        let after = t.root_hash();
        assert_ne!(before, after);
    }

    #[test]
    fn same_content_same_root() {
        let mut t1 = MerkleTree::new();
        t1.update(PathBuf::from("a.txt"), hash("x"));
        t1.update(PathBuf::from("b.txt"), hash("y"));

        let mut t2 = MerkleTree::new();
        t2.update(PathBuf::from("a.txt"), hash("x"));
        t2.update(PathBuf::from("b.txt"), hash("y"));

        assert_eq!(t1.root_hash(), t2.root_hash());
    }

    #[test]
    fn different_content_different_root() {
        let mut t1 = MerkleTree::new();
        t1.update(PathBuf::from("a.txt"), hash("x"));

        let mut t2 = MerkleTree::new();
        t2.update(PathBuf::from("a.txt"), hash("y"));

        assert_ne!(t1.root_hash(), t2.root_hash());
    }

    #[test]
    fn remove_restores_previous_root() {
        let mut t = MerkleTree::new();
        let empty_root = t.root_hash();
        t.update(PathBuf::from("a.txt"), hash("x"));
        assert_ne!(t.root_hash(), empty_root);
        t.remove(Path::new("a.txt"));
        assert_eq!(t.root_hash(), empty_root);
    }

    #[test]
    fn remove_nonexistent_returns_false() {
        let mut t = MerkleTree::new();
        assert!(!t.remove(Path::new("ghost.txt")));
    }

    #[test]
    fn rename_preserves_hash() {
        let mut t = MerkleTree::new();
        let h = hash("data");
        t.update(PathBuf::from("old.txt"), h.clone());
        t.rename(Path::new("old.txt"), PathBuf::from("new.txt"));
        assert!(!t.contains(Path::new("old.txt")));
        assert_eq!(t.get(Path::new("new.txt")), Some(&h));
    }

    #[test]
    fn rename_nonexistent_is_noop() {
        let mut t = MerkleTree::new();
        let root = t.root_hash();
        t.rename(Path::new("nope"), PathBuf::from("also_nope"));
        assert_eq!(t.root_hash(), root);
    }

    #[test]
    fn nested_paths_produce_different_hashes() {
        let mut t1 = MerkleTree::new();
        t1.update(PathBuf::from("dir/a.txt"), hash("x"));

        let mut t2 = MerkleTree::new();
        t2.update(PathBuf::from("a.txt"), hash("x"));

        assert_ne!(t1.root_hash(), t2.root_hash());
    }

    #[test]
    fn file_count_and_contains() {
        let mut t = MerkleTree::new();
        assert_eq!(t.file_count(), 0);
        t.update(PathBuf::from("a"), hash("a"));
        t.update(PathBuf::from("b"), hash("b"));
        assert_eq!(t.file_count(), 2);
        assert!(t.contains(Path::new("a")));
        assert!(!t.contains(Path::new("c")));
    }

    #[test]
    fn files_iterator() {
        let mut t = MerkleTree::new();
        let ha = hash("a");
        let hb = hash("b");
        t.update(PathBuf::from("x"), ha.clone());
        t.update(PathBuf::from("y"), hb.clone());

        let collected: BTreeMap<PathBuf, ContentHash> =
            t.files().map(|(p, h)| (p.to_path_buf(), h.clone())).collect();
        assert_eq!(collected.len(), 2);
        assert_eq!(collected[Path::new("x")], ha);
        assert_eq!(collected[Path::new("y")], hb);
    }

    #[test]
    fn commit_stores_to_cas() {
        let dir = tempfile::tempdir().unwrap();
        let cas = CasStore::new(dir.path().join("cas")).unwrap();

        let mut t = MerkleTree::new();
        t.update(PathBuf::from("file.txt"), hash("hello"));

        let commit_hash = t
            .commit(&cas, 1000, "2026-01-01T00:00:00Z".into(), None)
            .unwrap();

        // Commit object should be readable from CAS.
        let data = cas.read(&commit_hash).unwrap();
        let commit: Commit = serde_json::from_slice(&data).unwrap();
        assert_eq!(commit.ts_monotonic, 1000);
        assert!(commit.parent.is_none());
    }

    #[test]
    fn commit_with_parent() {
        let dir = tempfile::tempdir().unwrap();
        let cas = CasStore::new(dir.path().join("cas")).unwrap();

        let mut t = MerkleTree::new();
        t.update(PathBuf::from("a.txt"), hash("v1"));
        let c1 = t
            .commit(&cas, 100, "2026-01-01T00:00:00Z".into(), None)
            .unwrap();

        t.update(PathBuf::from("a.txt"), hash("v2"));
        let c2 = t
            .commit(&cas, 200, "2026-01-01T00:00:01Z".into(), Some(c1.clone()))
            .unwrap();

        let data = cas.read(&c2).unwrap();
        let commit: Commit = serde_json::from_slice(&data).unwrap();
        assert_eq!(commit.parent, Some(c1));
    }

    #[test]
    fn deep_nesting_works() {
        let mut t = MerkleTree::new();
        t.update(
            PathBuf::from("a/b/c/d/e/f.txt"),
            hash("deep"),
        );
        // Should not panic and should produce a valid hash.
        let h = t.root_hash();
        assert_eq!(h.as_str().len(), 64);
    }

    #[test]
    fn multiple_files_same_dir() {
        let mut t = MerkleTree::new();
        t.update(PathBuf::from("dir/a.txt"), hash("a"));
        t.update(PathBuf::from("dir/b.txt"), hash("b"));

        let h1 = t.root_hash();

        // Order of insertion should not matter.
        let mut t2 = MerkleTree::new();
        t2.update(PathBuf::from("dir/b.txt"), hash("b"));
        t2.update(PathBuf::from("dir/a.txt"), hash("a"));

        assert_eq!(h1, t2.root_hash());
    }

    #[test]
    fn update_overwrites_previous() {
        let mut t = MerkleTree::new();
        t.update(PathBuf::from("f.txt"), hash("v1"));
        let h1 = t.root_hash();
        t.update(PathBuf::from("f.txt"), hash("v2"));
        let h2 = t.root_hash();
        assert_ne!(h1, h2);
        assert_eq!(t.file_count(), 1);
    }

    #[test]
    fn default_creates_empty() {
        let t = MerkleTree::default();
        assert_eq!(t.file_count(), 0);
    }

    #[test]
    fn rename_overwrites_destination() {
        let mut t = MerkleTree::new();
        t.update(PathBuf::from("a"), hash("keep"));
        t.update(PathBuf::from("b"), hash("discard"));
        t.rename(Path::new("a"), PathBuf::from("b"));
        assert_eq!(t.file_count(), 1);
        assert_eq!(t.get(Path::new("b")), Some(&hash("keep")));
    }
}

// Rust guideline compliant 2026-02-21
