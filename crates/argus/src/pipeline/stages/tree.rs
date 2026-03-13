// Rust guideline compliant 2026-02-21
//! Merkle tree update stage.
//!
//! Applies mutating events to the in-memory Merkle tree and periodically
//! emits checkpoint records so downstream sinks can persist durable
//! snapshots.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use tracing::event;
use tracing::Level;

use crate::cas::ContentHash;
use crate::pipeline::captured::{CapturedContent, CapturedEvent};
use crate::pipeline::classified::Classification;
use crate::pipeline::durability::DurabilityLayer;
use crate::snapshot::MerkleTree;

/// Stage that maintains the in-memory Merkle tree and persists checkpoints.
pub struct TreeStage {
    tree: Mutex<MerkleTree>,
    durability: DurabilityLayer,
    /// How many mutating events between checkpoint persists.
    checkpoint_interval: u64,
    events_since_checkpoint: AtomicU64,
}

impl TreeStage {
    /// Access the inner Merkle tree for snapshot operations.
    pub(crate) fn tree(&self) -> &Mutex<MerkleTree> {
        &self.tree
    }

    /// Create a new stage from an existing tree and durability layer.
    pub fn new(tree: MerkleTree, durability: DurabilityLayer, checkpoint_interval: u64) -> Self {
        Self {
            tree: Mutex::new(tree),
            durability,
            checkpoint_interval,
            events_since_checkpoint: AtomicU64::new(0),
        }
    }

    /// Apply a captured event to the tree; return the new root hash if mutated.
    ///
    /// Emits a checkpoint record every `checkpoint_interval` mutations.
    pub fn update(&self, event: &CapturedEvent) -> Option<ContentHash> {
        let path = mutated_path(&event.classification)?;
        let hash = content_hash(event)?;

        let path_display = path.display().to_string();
        let mut tree = match self.tree.lock() {
            Ok(g) => g,
            Err(e) => {
                event!(
                    name: "pipeline.tree.lock_poisoned",
                    Level::ERROR,
                    error.message = %e,
                    "tree mutex poisoned, skipping update",
                );
                return None;
            }
        };
        tree.update(path, hash);
        let root = tree.root_hash();
        event!(
            name: "pipeline.tree.update",
            Level::DEBUG,
            path = path_display,
            root_hash = %root,
            "tree updated",
        );

        // Persist checkpoint after every N mutations.
        let count = self.events_since_checkpoint.fetch_add(1, Ordering::Relaxed) + 1;
        if count >= self.checkpoint_interval {
            self.events_since_checkpoint.store(0, Ordering::Relaxed);
            let seq = count;
            if let Ok(data) = bincode::serialize(&*tree) {
                let hash = ContentHash::from_data(&data);
                let _ = self.durability.persist_with_hash(hash.clone(), &data);
                self.durability.upload_async(hash, data);
                event!(
                    name: "tree_stage.checkpoint",
                    Level::DEBUG,
                    seq,
                    "persisted checkpoint at event {{seq}}",
                );
            }
        }

        Some(root)
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
        | Classification::FileRmdir { path } => {
            // Deletions are still tracked as mutations.
            Some(path.clone())
        }
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
