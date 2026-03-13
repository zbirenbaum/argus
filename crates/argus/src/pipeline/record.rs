// Rust guideline compliant 2026-02-21
//! The `Record` type — the unit of data flowing into sinks.

use crate::cas::ContentHash;
use crate::events::Event;

/// A unit written to the record bus and forwarded to all sinks.
#[derive(Debug, Clone)]
pub enum Record {
    /// A structured supervisor event.
    Event(Event),

    /// Raw content blob addressed by its hash.
    Content { hash: ContentHash, data: Vec<u8> },

    /// Manifest listing chunks for a large blob.
    Manifest {
        hash: ContentHash,
        chunks: Vec<ContentHash>,
    },

    /// Periodic Merkle-tree checkpoint.
    Checkpoint { seq: u64, data: Vec<u8> },
}
