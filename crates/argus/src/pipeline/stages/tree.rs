// Rust guideline compliant 2026-02-21
//! Merkle tree update stage.
//!
//! Applies mutating events to the in-memory [`TreeBuilder`] and periodically
//! emits checkpoint records so downstream sinks can persist durable snapshots.
//!
//! The pipeline runner is single-threaded (one event at a time), so no mutex
//! is needed — [`TreeBuilder`] is owned directly.

use tracing::event;
use tracing::Level;

use crate::cas::ContentHash;
use crate::config::TreeConfig;
use crate::pipeline::captured::{CapturedContent, CapturedEvent};
use crate::pipeline::classified::Classification;
use crate::pipeline::durability::DurabilityLayer;
use crate::snapshot::builder::{TreeBuilder, TreeSnapshot};

/// Stage that maintains the in-memory Merkle tree and persists checkpoints.
pub struct TreeStage {
    builder: TreeBuilder,
    durability: DurabilityLayer,
    events_since_checkpoint: u64,
    checkpoint_interval: u64,
}

impl TreeStage {
    /// Create a new stage from config and a durability layer.
    pub fn new(config: TreeConfig, durability: DurabilityLayer) -> Self {
        let checkpoint_interval = config.checkpoint_interval;
        Self {
            builder: TreeBuilder::new(config),
            durability,
            events_since_checkpoint: 0,
            checkpoint_interval,
        }
    }

    /// Apply a captured event to the tree; return the new root hash if mutated.
    ///
    /// Emits a checkpoint record every `checkpoint_interval` mutations.
    pub fn update(&mut self, event: &CapturedEvent) -> Option<ContentHash> {
        let path = mutated_path(&event.classification)?;
        let hash = content_hash(event);

        let path_display = path.display().to_string();

        if let Some(h) = hash {
            self.builder.update(path, h);
        } else {
            // Deletions and unlink events remove the path from the tree.
            self.builder.remove(&path);
        }

        let root = self.builder.root_hash();
        event!(
            name: "pipeline.tree.update",
            Level::DEBUG,
            path = path_display,
            root_hash = %root,
            "tree updated",
        );

        // Persist checkpoint after every N mutations.
        self.events_since_checkpoint += 1;
        if self.events_since_checkpoint >= self.checkpoint_interval {
            self.events_since_checkpoint = 0;
            let seq = self.events_since_checkpoint;
            let snapshot = self.builder.finalize();
            persist_checkpoint(seq, &snapshot, &self.durability);
        }

        Some(root)
    }

    /// Returns `true` when accumulated mutations reach the configured threshold.
    #[must_use]
    pub fn should_finalize(&self) -> bool {
        self.builder.should_finalize()
    }

    /// Force hash computation and return an immutable snapshot.
    pub fn finalize(&mut self) -> TreeSnapshot {
        self.builder.finalize()
    }
}

/// Serialize and persist a checkpoint to the durability layer.
fn persist_checkpoint(seq: u64, snapshot: &TreeSnapshot, durability: &DurabilityLayer) {
    if let Ok(data) = bincode::serialize(snapshot) {
        let hash = ContentHash::from_data(&data);
        let _ = durability.persist_with_hash(hash.clone(), &data);
        durability.upload_async(hash, data);
        event!(
            name: "tree_stage.checkpoint",
            Level::DEBUG,
            seq,
            "persisted checkpoint at event {{seq}}",
        );
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
///
/// Returns `None` for deletions — callers use `None` to remove the path.
fn content_hash(event: &CapturedEvent) -> Option<ContentHash> {
    match &event.content {
        CapturedContent::FileWrite { after_hash, .. } => *after_hash,
        CapturedContent::FileTruncate { after_hash, .. } => *after_hash,
        CapturedContent::FileRead { content_hash, .. } => *content_hash,
        CapturedContent::FileDelete { content_hash, .. } => *content_hash,
        _ => None,
    }
}
