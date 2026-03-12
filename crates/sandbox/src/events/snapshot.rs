// Rust guideline compliant 2026-02-21
//! Snapshot and checkpoint event payloads.

use serde::{Deserialize, Serialize};

/// The initial filesystem state at agent start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitialState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree_hash: Option<String>,
    pub file_count: u64,
    pub total_size: u64,
}

/// A point-in-time checkpoint was created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// The event sequence number at which the checkpoint was taken.
    #[serde(rename = "checkpoint_seq")]
    pub seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree_hash: Option<String>,
    pub checkpoint_s3_key: String,
}

/// A memory-mapped file was detected (untrackable writes).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MmapWarning {
    pub pid: u32,
    pub path: String,
    pub fd: i32,
    pub prot: u32,
    pub flags: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_state_round_trip() {
        let s = InitialState {
            tree_hash: Some("root_hash".into()),
            file_count: 1500,
            total_size: 1_048_576,
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: InitialState = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn checkpoint_round_trip() {
        let c = Checkpoint {
            seq: 100,
            tree_hash: Some("abcdef".into()),
            checkpoint_s3_key: "checkpoints/agent-1/100.bin".into(),
        };
        let json = serde_json::to_string(&c).unwrap();
        let back: Checkpoint = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn mmap_warning_round_trip() {
        let m = MmapWarning {
            pid: 42,
            path: "/workspace/data.db".into(),
            fd: 7,
            prot: 3,
            flags: 1,
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: MmapWarning = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }
}
