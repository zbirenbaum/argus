//! Merkle tree, checkpoint serialization, and tree diffing.
//!
//! Provides an in-memory Merkle tree that tracks the filesystem state as
//! a content-addressed DAG of blob, tree, and commit objects. On every
//! mutating event the affected subtree is rehashed, producing a new root
//! that serves as a restore point.
//!
//! Checkpoints serialize the full tree state to binary for fast recovery.
//! The diff engine walks two trees and reports only subtrees whose hashes
//! differ, skipping identical branches entirely.

pub mod checkpoint;
pub mod diff;
pub mod tree;

#[doc(inline)]
pub use checkpoint::{deserialize_checkpoint, serialize_checkpoint};
#[doc(inline)]
pub use diff::{diff_trees, DiffEntry, DiffKind};
#[doc(inline)]
pub use tree::{Commit, MerkleTree, TreeObject};

// Rust guideline compliant 2026-02-21
