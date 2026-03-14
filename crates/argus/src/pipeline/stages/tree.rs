// Rust guideline compliant 2026-02-21
//! Merkle tree update stage.
//!
//! Applies mutating events to the in-memory [`MerkleTree`] and persists
//! the tree to CAS when the runner requests it. The runner controls
//! persist timing (batch boundaries, idle flush, shutdown).

use anyhow::Result;
use tracing::event;
use tracing::Level;

use crate::cas::{Cas, ContentHash, LocalCas};
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
