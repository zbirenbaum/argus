//! Durability mode configuration with per-path overrides.
//!
//! Controls what must be persisted before a traced syscall is allowed to
//! complete.  Higher durability costs more latency but reduces data-loss
//! risk.

use serde::{Deserialize, Serialize};

/// Durability envelope: a default mode plus path-specific overrides.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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

    /// Compile all glob patterns in overrides, logging warnings for
    /// any invalid patterns.
    ///
    /// Must be called after deserialization before using `mode_for_path`.
    pub fn validate_patterns(&mut self) {
        for ov in &mut self.overrides {
            ov.compile_patterns();
        }
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

    /// Pre-compiled glob patterns. Built by `compile_patterns`.
    #[serde(skip)]
    compiled: Vec<glob::Pattern>,
}

impl PartialEq for DurabilityOverride {
    fn eq(&self, other: &Self) -> bool {
        self.paths == other.paths && self.mode == other.mode
    }
}

impl DurabilityOverride {
    /// Test whether `path` matches any of this override's compiled patterns.
    fn matches(&self, path: &str) -> bool {
        self.compiled.iter().any(|p| p.matches(path))
    }

    /// Compile glob patterns from string list, warning on invalid ones.
    fn compile_patterns(&mut self) {
        self.compiled = self
            .paths
            .iter()
            .filter_map(|s| match glob::Pattern::new(s) {
                Ok(p) => Some(p),
                Err(e) => {
                    tracing::warn!(
                        pattern = %s,
                        error = %e,
                        "invalid glob pattern in durability override, skipping"
                    );
                    None
                }
            })
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_override(paths: Vec<String>, mode: DurabilityMode) -> DurabilityOverride {
        let mut ov = DurabilityOverride {
            paths,
            mode,
            compiled: Vec::new(),
        };
        ov.compile_patterns();
        ov
    }

    fn make_config(
        default: DurabilityMode,
        overrides: Vec<DurabilityOverride>,
    ) -> DurabilityConfig {
        let mut cfg = DurabilityConfig { default, overrides };
        cfg.validate_patterns();
        cfg
    }

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
        let cfg = make_config(
            DurabilityMode::Local,
            vec![
                make_override(
                    vec!["*.key".into(), "*.pem".into()],
                    DurabilityMode::Remote,
                ),
                make_override(
                    vec!["/workspace/checkpoints/**".into()],
                    DurabilityMode::Memory,
                ),
            ],
        );
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
        let cfg = make_config(
            DurabilityMode::Local,
            vec![
                make_override(vec!["*.key".into()], DurabilityMode::Remote),
                make_override(vec!["*.key".into()], DurabilityMode::Memory),
            ],
        );
        assert_eq!(cfg.mode_for_path("secret.key"), DurabilityMode::Remote);
    }

    #[test]
    fn durability_config_yaml_round_trip() {
        let cfg = make_config(
            DurabilityMode::Memory,
            vec![make_override(
                vec!["*.credentials".into()],
                DurabilityMode::Remote,
            )],
        );
        let yaml = serde_yaml::to_string(&cfg).unwrap();
        let mut parsed: DurabilityConfig = serde_yaml::from_str(&yaml).unwrap();
        parsed.validate_patterns();
        assert_eq!(parsed, cfg);
    }

    #[test]
    fn partial_eq_ignores_compiled() {
        let a = make_override(vec!["*.key".into()], DurabilityMode::Remote);
        let b = DurabilityOverride {
            paths: vec!["*.key".into()],
            mode: DurabilityMode::Remote,
            compiled: Vec::new(),
        };
        assert_eq!(a, b);
    }
}
