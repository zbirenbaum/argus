// Rust guideline compliant 2026-02-21
//! Content-capture policy: per-path rules, per-pid rate limits, global budget.
//!
//! Rules are evaluated in order; the first match wins. If no rule matches,
//! per-pid rate and global budget are checked before falling through to Full.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;

/// How much content should be captured for a given event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureLevel {
    /// Read and hash the content.
    Full,
    /// Record size and metadata only; skip memory reads.
    MetadataOnly,
    /// Skip this event entirely.
    Ignore,
}

/// A single static capture rule matched by glob pattern.
#[derive(Debug, Clone)]
pub struct CaptureRule {
    /// Glob pattern matched against the absolute file path.
    pub pattern: String,
    pub level: CaptureLevel,
}

/// Per-pid byte counter for the current rate-limiting window.
#[derive(Debug, Default)]
pub struct RateCounter {
    pub bytes_this_window: u64,
}

/// Runtime capture policy combining static rules, rate limits, and budget.
///
/// Thread-safe via atomic operations and `DashMap`.
#[derive(Debug)]
pub struct CapturePolicy {
    pub rules: Vec<CaptureRule>,
    /// Per-pid byte counters for the current window.
    pub rate: DashMap<u32, RateCounter>,
    /// Remaining bytes in the global budget for the current window.
    pub budget: AtomicU64,
    /// Total global budget per window. Zero means unlimited.
    pub window_budget: u64,
    /// Per-pid byte cap before downgrading to MetadataOnly. Zero means unlimited.
    pub rate_limit_per_pid: u64,
    /// Pre-compiled glob patterns, parallel to `rules`.
    compiled: Vec<Option<glob::Pattern>>,
}

/// Configuration source for `CapturePolicy::new`.
///
/// Defined here to decouple from the config crate until it is finalized
/// by the config agent.
#[derive(Debug, Clone, Default)]
pub struct CaptureConfig {
    pub rules: Vec<CaptureRule>,
    /// Global per-window budget in bytes. Zero = unlimited.
    pub window_budget: u64,
    /// Per-pid per-window cap in bytes. Zero = unlimited.
    pub rate_limit_per_pid: u64,
}

impl CapturePolicy {
    /// Build a policy from config, pre-compiling glob patterns.
    pub fn new(config: &CaptureConfig) -> Self {
        let compiled = config.rules.iter().map(|r| glob::Pattern::new(&r.pattern).ok()).collect();
        Self {
            compiled,
            rules: config.rules.clone(),
            rate: DashMap::new(),
            budget: AtomicU64::new(config.window_budget),
            window_budget: config.window_budget,
            rate_limit_per_pid: config.rate_limit_per_pid,
        }
    }

    /// Full capture for all events with no limits — used in tests.
    pub fn default_full() -> Self {
        Self {
            rules: Vec::new(),
            compiled: Vec::new(),
            rate: DashMap::new(),
            budget: AtomicU64::new(u64::MAX),
            window_budget: 0,
            rate_limit_per_pid: 0,
        }
    }

    /// Determine the capture level for a given path, pid, and size.
    pub fn level(&self, path: &Path, pid: u32, size: usize) -> CaptureLevel {
        // Static rules take priority; first match wins.
        for (rule, compiled) in self.rules.iter().zip(self.compiled.iter()) {
            let matched = compiled.as_ref().map_or(false, |p| {
                p.matches_path(path)
            });
            if matched {
                return rule.level;
            }
        }

        // Per-pid rate limit degrades to MetadataOnly.
        if self.rate_limit_per_pid > 0 {
            let counter = self.rate.entry(pid).or_default();
            if counter.bytes_this_window.saturating_add(size as u64) > self.rate_limit_per_pid {
                return CaptureLevel::MetadataOnly;
            }
        }

        // Global budget degrades to MetadataOnly.
        if self.window_budget > 0 {
            let current = self.budget.load(Ordering::Relaxed);
            if (size as u64) > current {
                return CaptureLevel::MetadataOnly;
            }
        }

        CaptureLevel::Full
    }

    /// Account for bytes captured by a Full event.
    pub fn record_bytes(&self, pid: u32, size: usize) {
        let bytes = size as u64;
        if self.rate_limit_per_pid > 0 {
            let mut counter = self.rate.entry(pid).or_default();
            counter.bytes_this_window = counter.bytes_this_window.saturating_add(bytes);
        }
        if self.window_budget > 0 {
            // Saturate at zero; never wraps.
            self.budget.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
                Some(cur.saturating_sub(bytes))
            }).ok();
        }
    }

    /// Reset the global budget counter for a new window.
    pub fn reset_budget(&self) {
        if self.window_budget > 0 {
            self.budget.store(self.window_budget, Ordering::Relaxed);
        }
        // Per-pid counters are cleared by draining the map.
        self.rate.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_full_is_full() {
        let policy = CapturePolicy::default_full();
        let level = policy.level(Path::new("/workspace/file.txt"), 100, 1024);
        assert_eq!(level, CaptureLevel::Full);
    }

    #[test]
    fn rate_limit_degrades_after_threshold() {
        let config = CaptureConfig {
            rules: Vec::new(),
            window_budget: 0,
            rate_limit_per_pid: 1000,
        };
        let policy = CapturePolicy::new(&config);
        assert_eq!(policy.level(Path::new("/f"), 1, 500), CaptureLevel::Full);
        policy.record_bytes(1, 500);
        // 500 + 600 > 1000 → MetadataOnly
        assert_eq!(policy.level(Path::new("/f"), 1, 600), CaptureLevel::MetadataOnly);
    }

    #[test]
    fn budget_reset_restores_full() {
        let config = CaptureConfig {
            rules: Vec::new(),
            window_budget: 100,
            rate_limit_per_pid: 0,
        };
        let policy = CapturePolicy::new(&config);
        policy.record_bytes(1, 100);
        assert_eq!(policy.level(Path::new("/f"), 2, 50), CaptureLevel::MetadataOnly);
        policy.reset_budget();
        assert_eq!(policy.level(Path::new("/f"), 2, 50), CaptureLevel::Full);
    }
}
