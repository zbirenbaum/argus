//! Binary checkpoint serialization for the Merkle tree.
//!
//! Checkpoints capture the full in-memory tree state as a compact
//! bincode blob. They are uploaded to S3 at
//! `checkpoints/{agent_id}/{seq}.bin` every N events (default 1000)
//! and loaded on restart to avoid replaying the entire event log.

use anyhow::{bail, Context, Result};

use super::tree::MerkleTree;

/// Default number of events between automatic checkpoints.
///
/// Chosen to balance checkpoint frequency (and thus restart speed)
/// against the storage and I/O cost of serializing the full tree.
pub const DEFAULT_CHECKPOINT_INTERVAL: u64 = 1000;

/// Wire format version for checkpoint blobs.
///
/// Incremented when the serialization format changes in an
/// incompatible way. Checked on deserialization so that old binaries
/// reject payloads they cannot decode.
const CHECKPOINT_VERSION: u8 = 1;

/// Serialize a `MerkleTree` to a compact binary representation.
///
/// Prepends a version byte before the bincode payload. The resulting
/// bytes are suitable for storage in S3 or on the local filesystem.
///
/// # Errors
///
/// Returns an error if bincode serialization fails (should not happen
/// for well-formed trees).
pub fn serialize_checkpoint(tree: &MerkleTree) -> Result<Vec<u8>> {
    let payload = bincode::serialize(tree).context("serialize checkpoint")?;
    let mut buf = Vec::with_capacity(1 + payload.len());
    buf.push(CHECKPOINT_VERSION);
    buf.extend_from_slice(&payload);
    Ok(buf)
}

/// Deserialize a `MerkleTree` from bytes produced by [`serialize_checkpoint`].
///
/// # Errors
///
/// Returns an error if the version byte is unsupported or the data is
/// corrupt.
pub fn deserialize_checkpoint(data: &[u8]) -> Result<MerkleTree> {
    let (&version, payload) = data
        .split_first()
        .context("checkpoint data is empty")?;
    if version != CHECKPOINT_VERSION {
        bail!(
            "unsupported checkpoint version {version}, expected {CHECKPOINT_VERSION}"
        );
    }
    bincode::deserialize(payload).context("deserialize checkpoint")
}

/// Build the S3 key for a checkpoint.
pub fn checkpoint_s3_key(agent_id: &str, seq: u64) -> String {
    format!("checkpoints/{agent_id}/{seq}.bin")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::cas::ContentHash;

    use super::*;

    fn hash(s: &str) -> ContentHash {
        ContentHash::from_data(s.as_bytes())
    }

    #[test]
    fn round_trip_empty() {
        let tree = MerkleTree::new();
        let data = serialize_checkpoint(&tree).unwrap();
        let restored = deserialize_checkpoint(&data).unwrap();
        assert_eq!(restored.file_count(), 0);
    }

    #[test]
    fn round_trip_with_files() {
        let mut tree = MerkleTree::new();
        tree.update(PathBuf::from("a.txt"), hash("content-a"));
        tree.update(
            PathBuf::from("dir/b.txt"),
            hash("content-b"),
        );
        tree.update(
            PathBuf::from("dir/sub/c.txt"),
            hash("content-c"),
        );

        let data = serialize_checkpoint(&tree).unwrap();
        let restored = deserialize_checkpoint(&data).unwrap();

        assert_eq!(restored.file_count(), 3);
        assert_eq!(restored.root_hash(), tree.root_hash());
    }

    #[test]
    fn corrupted_data_errors() {
        let result = deserialize_checkpoint(b"not valid bincode");
        assert!(result.is_err());
    }

    #[test]
    fn empty_data_errors() {
        let result = deserialize_checkpoint(b"");
        assert!(result.is_err());
    }

    #[test]
    fn wrong_version_errors() {
        let mut data = serialize_checkpoint(&MerkleTree::new()).unwrap();
        // Corrupt the version byte to an unsupported value.
        data[0] = 255;
        let result = deserialize_checkpoint(&data);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("unsupported checkpoint version"));
    }

    #[test]
    fn checkpoint_key_format() {
        assert_eq!(
            checkpoint_s3_key("agent-42", 1500),
            "checkpoints/agent-42/1500.bin"
        );
    }

    #[test]
    fn round_trip_preserves_paths() {
        let mut tree = MerkleTree::new();
        let paths = vec![
            PathBuf::from("workspace/src/main.rs"),
            PathBuf::from("workspace/Cargo.toml"),
            PathBuf::from("workspace/.gitignore"),
        ];
        for (i, p) in paths.iter().enumerate() {
            tree.update(p.clone(), hash(&format!("h{i}")));
        }

        let data = serialize_checkpoint(&tree).unwrap();
        let restored = deserialize_checkpoint(&data).unwrap();

        for p in &paths {
            assert!(restored.contains(p));
        }
    }

    #[test]
    fn default_interval_is_1000() {
        assert_eq!(DEFAULT_CHECKPOINT_INTERVAL, 1000);
    }

    #[test]
    fn large_tree_round_trip() {
        let mut tree = MerkleTree::new();
        for i in 0..500 {
            tree.update(
                PathBuf::from(format!("dir/{i}/file.txt")),
                hash(&format!("content-{i}")),
            );
        }
        let data = serialize_checkpoint(&tree).unwrap();
        let restored = deserialize_checkpoint(&data).unwrap();
        assert_eq!(restored.file_count(), 500);
        assert_eq!(restored.root_hash(), tree.root_hash());
    }
}

// Rust guideline compliant 2026-02-21
