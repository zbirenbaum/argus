// Rust guideline compliant 2026-02-21
//! Merkle tree update stage.
//!
//! Applies mutating events to the in-memory [`MerkleTree`] and persists
//! the tree to CAS when the runner requests it. The runner controls
//! persist timing (batch boundaries, idle flush, shutdown).

use anyhow::Result;
use tracing::event;
use tracing::Level;

use crate::cas::{ContentHash, LocalCas};
use crate::pipeline::captured::{CapturedContent, CapturedEvent};
use crate::pipeline::classified::Classification;
use crate::snapshot::MerkleTree;

/// Stage that maintains the in-memory Merkle tree.
pub struct TreeStage {
    tree: MerkleTree,
    cas: LocalCas,
}

impl TreeStage {
    /// Create a new stage with an empty tree and a CAS backend.
    pub fn new(cas: LocalCas) -> Self {
        Self { tree: MerkleTree::new(), cas }
    }

    /// Apply a captured event to the tree; return the new root hash if mutated.
    pub fn update(&mut self, event: &CapturedEvent) -> Option<ContentHash> {
        // Renames need special handling: move entry from old_path to new_path.
        if let Classification::FileRename { ref old_path, ref new_path } = event.classification {
            let hash = self.tree.get(old_path).copied();
            self.tree.remove(old_path);
            if let Some(h) = hash {
                self.tree.update(new_path.clone(), h);
            }
            let root = self.tree.root_hash();
            event!(
                name: "pipeline.tree.rename",
                Level::DEBUG,
                old_path = %old_path.display(),
                new_path = %new_path.display(),
                root_hash = %root,
                "tree renamed",
            );
            return Some(root);
        }

        let path = mutated_path(&event.classification)?;
        let hash = content_hash(event);

        if let Some(h) = hash {
            self.tree.update(path.clone(), h);
        } else {
            self.tree.remove(&path);
        }

        let root = self.tree.root_hash();
        event!(
            name: "pipeline.tree.update",
            Level::DEBUG,
            path = %path.display(),
            root_hash = %root,
            "tree updated",
        );

        Some(root)
    }

    /// Stream-compatible tree update returning the event alongside the hash.
    pub fn process(&mut self, event: CapturedEvent) -> (CapturedEvent, Option<ContentHash>) {
        let tree_hash = self.update(&event);
        (event, tree_hash)
    }

    /// Persist the tree to CAS and return the CAS root hash.
    ///
    /// The returned hash is the one that `MerkleTree::load()` expects —
    /// use it for `insert_tree_hash` so `/restore` can find it.
    ///
    /// # Errors
    ///
    /// Returns an error if CAS writes fail.
    pub fn persist(&self) -> Result<ContentHash> {
        let cas_hash = self.tree.store(&self.cas)?;
        event!(
            name: "pipeline.tree.persisted",
            Level::DEBUG,
            cas_hash = %cas_hash,
            file_count = self.tree.file_count(),
            "tree persisted to CAS",
        );
        Ok(cas_hash)
    }

    /// Clone the current tree for SharedState storage.
    pub fn snapshot(&self) -> MerkleTree {
        self.tree.clone()
    }

    /// Number of tracked files.
    pub fn file_count(&self) -> usize {
        self.tree.file_count()
    }
}

/// Extract a mutated path from the classification for tree updates.
fn mutated_path(c: &Classification) -> Option<std::path::PathBuf> {
    match c {
        Classification::FileWrite { path, .. }
        | Classification::FileMkdir { path }
        | Classification::FileChmod { path, .. }
        | Classification::FileTruncate { path, .. } => Some(path.clone()),
        Classification::FileUnlink { path }
        | Classification::FileRmdir { path } => Some(path.clone()),
        _ => None,
    }
}

/// Extract the content hash from a captured event for tree storage.
fn content_hash(event: &CapturedEvent) -> Option<ContentHash> {
    match &event.content {
        CapturedContent::FileWrite { after_hash, .. } => *after_hash,
        CapturedContent::FileTruncate { after_hash, .. } => *after_hash,
        CapturedContent::FileRead { content_hash, .. } => *content_hash,
        CapturedContent::FileDelete { content_hash, .. } => *content_hash,
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cas::LocalCas;
    use nix::unistd::Pid;
    use std::path::PathBuf;

    fn make_stage() -> TreeStage {
        let dir = tempfile::TempDir::new().unwrap();
        let cas = LocalCas::new(dir.path().to_path_buf()).unwrap();
        TreeStage::new(cas)
    }

    fn dummy_hash() -> ContentHash {
        ContentHash::from_data(b"test content")
    }

    fn write_event(path: &str, hash: ContentHash) -> CapturedEvent {
        CapturedEvent {
            pid: Pid::from_raw(1),
            classification: Classification::FileWrite {
                path: PathBuf::from(path),
                fd: 3,
                buf_addr: 0,
                len: 10,
            },
            content: CapturedContent::FileWrite {
                before_hash: None,
                after_hash: Some(hash),
                data: None,
                size: 10,
            },
        }
    }

    fn rename_event(old: &str, new: &str) -> CapturedEvent {
        CapturedEvent {
            pid: Pid::from_raw(1),
            classification: Classification::FileRename {
                old_path: PathBuf::from(old),
                new_path: PathBuf::from(new),
            },
            content: CapturedContent::None,
        }
    }

    #[test]
    fn rename_moves_entry_in_tree() {
        let mut stage = make_stage();
        let hash = dummy_hash();

        // Write to .tmp file
        stage.update(&write_event("/workspace/foo.txt.tmp", hash));
        assert_eq!(stage.file_count(), 1);
        assert!(stage.tree.get(&PathBuf::from("/workspace/foo.txt.tmp")).is_some());

        // Rename .tmp → final
        let root = stage.update(&rename_event("/workspace/foo.txt.tmp", "/workspace/foo.txt"));
        assert!(root.is_some());
        assert_eq!(stage.file_count(), 1);
        assert!(stage.tree.get(&PathBuf::from("/workspace/foo.txt.tmp")).is_none());
        assert_eq!(
            stage.tree.get(&PathBuf::from("/workspace/foo.txt")),
            Some(&hash),
        );
    }

    #[test]
    fn rename_nonexistent_is_noop() {
        let mut stage = make_stage();
        let root = stage.update(&rename_event("/workspace/gone.tmp", "/workspace/gone.txt"));
        assert!(root.is_some());
        assert_eq!(stage.file_count(), 0);
    }

    #[test]
    fn rename_overwrites_existing_target() {
        let mut stage = make_stage();
        let hash_a = ContentHash::from_data(b"aaa");
        let hash_b = ContentHash::from_data(b"bbb");

        stage.update(&write_event("/workspace/old.txt", hash_a));
        stage.update(&write_event("/workspace/new.txt", hash_b));
        assert_eq!(stage.file_count(), 2);

        // Rename old → new should overwrite new with old's hash
        stage.update(&rename_event("/workspace/old.txt", "/workspace/new.txt"));
        assert_eq!(stage.file_count(), 1);
        assert_eq!(
            stage.tree.get(&PathBuf::from("/workspace/new.txt")),
            Some(&hash_a),
        );
    }
}
