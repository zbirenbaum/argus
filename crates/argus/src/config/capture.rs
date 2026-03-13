// Rust guideline compliant 2026-02-21
//! Adaptive content capture policy configuration.

use serde::{Deserialize, Serialize};

/// Per-path content capture configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapturePathConfig {
    /// Glob patterns for full content capture.
    #[serde(default = "default_capture_content_paths")]
    pub paths: Vec<String>,
}

/// Adaptive content capture policy configuration.
///
/// Controls which filesystem paths get full content capture, metadata-only
/// capture, or are ignored entirely. Rate limits prevent any single process
/// from saturating the CAS pipeline during heavy write workloads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureConfig {
    /// Paths that get full content capture (default: source code patterns).
    #[serde(default)]
    pub content: CapturePathConfig,
    /// Paths that get metadata-only capture (no content).
    #[serde(default)]
    pub metadata_only: CapturePathConfig,
    /// Paths that are completely ignored.
    #[serde(default)]
    pub ignore: CapturePathConfig,
    /// Per-process rate limit in bytes/second (default: 100 MiB/s).
    ///
    /// Prevents a single bursty process from monopolizing the upload pool.
    /// 100 MiB/s matches typical NVMe sequential write throughput, which is
    /// the expected upper bound for in-container workloads.
    #[serde(default = "default_rate_limit")]
    pub rate_limit_bytes_per_sec: u64,
    /// Global budget per window in bytes (default: 1 GiB).
    ///
    /// Caps total captured bytes per window across all processes so that
    /// runaway agent writes do not exhaust remote storage quota.
    #[serde(default = "default_budget_bytes")]
    pub budget_bytes_per_window: u64,
    /// Budget reset window in seconds (default: 60).
    #[serde(default = "default_budget_window_seconds")]
    pub budget_window_seconds: u64,
}

fn default_capture_content_paths() -> Vec<String> {
    vec![
        "/workspace/src/**".into(),
        "*.py".into(),
        "*.yaml".into(),
        "*.json".into(),
        "*.toml".into(),
        "*.rs".into(),
    ]
}

/// 100 MiB/s — matches typical NVMe sequential write ceiling for containers.
fn default_rate_limit() -> u64 {
    104_857_600
}

/// 1 GiB — sufficient for most agent sessions without unbounded growth.
fn default_budget_bytes() -> u64 {
    1_073_741_824
}

/// 60 seconds — aligns with typical minute-level metric granularity.
fn default_budget_window_seconds() -> u64 {
    60
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            content: CapturePathConfig { paths: default_capture_content_paths() },
            metadata_only: CapturePathConfig {
                paths: vec![
                    "/workspace/target/**".into(),
                    "**/node_modules/**".into(),
                    "**/*.o".into(),
                    "**/*.so".into(),
                ],
            },
            ignore: CapturePathConfig {
                paths: vec![
                    "**/__pycache__/**".into(),
                    "**/.git/objects/**".into(),
                ],
            },
            rate_limit_bytes_per_sec: default_rate_limit(),
            budget_bytes_per_window: default_budget_bytes(),
            budget_window_seconds: default_budget_window_seconds(),
        }
    }
}

impl Default for CapturePathConfig {
    fn default() -> Self {
        Self { paths: Vec::new() }
    }
}
