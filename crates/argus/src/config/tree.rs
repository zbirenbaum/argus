// Rust guideline compliant 2026-02-21

//! Configuration for Merkle tree batched finalization.

use serde::{Deserialize, Serialize};

/// Controls how often the tree finalizes and publishes snapshots.
///
/// Tuning `batch_size` trades latency for CPU cost: smaller values
/// produce more frequent snapshots at higher hash-computation overhead.
/// `checkpoint_interval` controls durability — lower means more frequent
/// persists to CAS/S3 at the cost of write amplification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TreeConfig {
    /// Mutations accumulated before a finalize pass.
    ///
    /// 64 is a reasonable default that amortizes hash computation
    /// across typical burst-write workloads without delaying snapshots
    /// by more than a few hundred milliseconds.
    #[serde(default = "default_batch_size")]
    pub batch_size: u64,

    /// Events between checkpoint persists to CAS/S3.
    ///
    /// 1000 balances write amplification against recovery window.
    /// At 100 events/s this checkpoints roughly every 10 seconds.
    #[serde(default = "default_checkpoint_interval")]
    pub checkpoint_interval: u64,
}

fn default_batch_size() -> u64 {
    64
}

fn default_checkpoint_interval() -> u64 {
    1000
}

impl Default for TreeConfig {
    fn default() -> Self {
        Self {
            batch_size: default_batch_size(),
            checkpoint_interval: default_checkpoint_interval(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_values_are_sensible() {
        let cfg = TreeConfig::default();
        assert_eq!(cfg.batch_size, 64);
        assert_eq!(cfg.checkpoint_interval, 1000);
    }

    #[test]
    fn serde_round_trip() {
        let cfg = TreeConfig { batch_size: 32, checkpoint_interval: 500 };
        let yaml = serde_yaml::to_string(&cfg).unwrap();
        let parsed: TreeConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed, cfg);
    }

    #[test]
    fn defaults_applied_on_empty_yaml() {
        let cfg: TreeConfig = serde_yaml::from_str("{}").unwrap();
        assert_eq!(cfg.batch_size, 64);
        assert_eq!(cfg.checkpoint_interval, 1000);
    }
}
