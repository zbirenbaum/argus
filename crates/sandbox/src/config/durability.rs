//! Durability mode configuration with per-path overrides.
//!
//! Controls what must be persisted before a traced syscall is allowed to
//! complete.  Higher durability costs more latency but reduces data-loss
//! risk.

// Rust guideline compliant 2026-02-21

use serde::{Deserialize, Serialize};

/// Durability envelope: a default mode plus path-specific overrides.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DurabilityConfig {
    /// Mode applied when no override matches.
    #[serde(default)]
    pub default: DurabilityMode,

    /// Path-glob overrides evaluated in declaration order; first match wins.
    #[serde(default)]
    pub overrides: Vec<DurabilityOverride>,
}

impl DurabilityConfig {
    /// Resolve the effective durability mode for a given path.
    ///
    /// Overrides are checked in order; the first matching glob wins.
    /// Falls back to `self.default` if nothing matches.
    pub fn mode_for_path(&self, path: &str) -> DurabilityMode {
        for ov in &self.overrides {
            if ov.matches(path) {
                return ov.mode;
            }
        }
        self.default
    }
}

/// Controls what is persisted before the traced process resumes.
///
/// Ordered from fastest (least durable) to slowest (most durable).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DurabilityMode {
    /// Hash kept in supervisor heap only. Lost on supervisor crash.
    Memory,

    /// Written to local CAS file and event segment before resume.
    #[default]
    Local,

    /// Confirmed uploaded to S3 before resume.
    Remote,
}

/// A path-glob override that selects a non-default durability mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DurabilityOverride {
    /// Glob patterns matched against the absolute file path.
    pub paths: Vec<String>,

    /// Durability mode applied when any pattern matches.
    pub mode: DurabilityMode,
}

impl DurabilityOverride {
    /// Test whether `path` matches any of this override's glob patterns.
    fn matches(&self, path: &str) -> bool {
        self.paths.iter().any(|pattern| {
            glob::Pattern::new(pattern)
                .map(|p| p.matches(path))
                .unwrap_or(false)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_local() {
        assert_eq!(DurabilityMode::default(), DurabilityMode::Local);
    }

    #[test]
    fn serde_round_trip_modes() {
        for mode in [
            DurabilityMode::Memory,
            DurabilityMode::Local,
            DurabilityMode::Remote,
        ] {
            let json = serde_json::to_string(&mode).unwrap();
            let parsed: DurabilityMode = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, mode);
        }
    }

    #[test]
    fn serde_yaml_mode_values() {
        let yaml = "\"memory\"";
        let mode: DurabilityMode = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(mode, DurabilityMode::Memory);

        let yaml = "\"remote\"";
        let mode: DurabilityMode = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(mode, DurabilityMode::Remote);
    }

    #[test]
    fn mode_for_path_uses_default_when_no_overrides() {
        let cfg = DurabilityConfig::default();
        assert_eq!(cfg.mode_for_path("/workspace/foo.txt"), DurabilityMode::Local);
    }

    #[test]
    fn mode_for_path_matches_glob_override() {
        let cfg = DurabilityConfig {
            default: DurabilityMode::Local,
            overrides: vec![
                DurabilityOverride {
                    paths: vec!["*.key".into(), "*.pem".into()],
                    mode: DurabilityMode::Remote,
                },
                DurabilityOverride {
                    paths: vec!["/workspace/checkpoints/**".into()],
                    mode: DurabilityMode::Memory,
                },
            ],
        };
        assert_eq!(cfg.mode_for_path("server.key"), DurabilityMode::Remote);
        assert_eq!(cfg.mode_for_path("cert.pem"), DurabilityMode::Remote);
        assert_eq!(
            cfg.mode_for_path("/workspace/checkpoints/step-100.bin"),
            DurabilityMode::Memory,
        );
        assert_eq!(cfg.mode_for_path("/workspace/main.py"), DurabilityMode::Local);
    }

    #[test]
    fn first_matching_override_wins() {
        let cfg = DurabilityConfig {
            default: DurabilityMode::Local,
            overrides: vec![
                DurabilityOverride {
                    paths: vec!["*.key".into()],
                    mode: DurabilityMode::Remote,
                },
                DurabilityOverride {
                    paths: vec!["*.key".into()],
                    mode: DurabilityMode::Memory,
                },
            ],
        };
        assert_eq!(cfg.mode_for_path("secret.key"), DurabilityMode::Remote);
    }

    #[test]
    fn durability_config_yaml_round_trip() {
        let cfg = DurabilityConfig {
            default: DurabilityMode::Memory,
            overrides: vec![DurabilityOverride {
                paths: vec!["*.credentials".into()],
                mode: DurabilityMode::Remote,
            }],
        };
        let yaml = serde_yaml::to_string(&cfg).unwrap();
        let parsed: DurabilityConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.default, DurabilityMode::Memory);
        assert_eq!(parsed.overrides.len(), 1);
        assert_eq!(parsed.overrides[0].mode, DurabilityMode::Remote);
    }
}
