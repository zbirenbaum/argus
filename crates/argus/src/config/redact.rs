// Rust guideline compliant 2026-02-21
//! Redaction configuration — three-tier PII scrubbing pipeline.
//!
//! Tier 1 (`exclude_paths`): entire files are dropped from capture.
//! Tier 2 (`drop_fields`): named event fields are removed before indexing.
//! Tier 3 (`scan_fields` + `patterns`): field values are regex-scanned and
//! matching substrings are replaced with a redaction token.

use std::collections::HashSet;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

use super::enrich::default_true;

const DEFAULT_EXCLUDE_PATHS: &[&str] = &[
    "*.env",
    "*.pem",
    "*.key",
    "credentials.json",
    ".ssh/**",
];

const DEFAULT_DROP_FIELDS: &[&str] = &[
    "http_request.headers.authorization",
    "http_request.headers.cookie",
    "http_request.headers.x-api-key",
];

const DEFAULT_SCAN_FIELDS: &[&str] = &[
    "http_request.headers",
    "http_request.body",
    "http_response.headers",
    "http_response.body",
    "stdio.text",
    "exec.envp",
];

/// Pre-built set of default drop fields for O(1) hot-path lookups.
static DEFAULT_DROP_SET: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    DEFAULT_DROP_FIELDS.iter().copied().collect()
});

/// Pre-built set of default scan fields for O(1) hot-path lookups.
static DEFAULT_SCAN_SET: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    DEFAULT_SCAN_FIELDS.iter().copied().collect()
});

/// Whether built-in redaction rule sets are active.
///
/// Each flag enables a curated set of patterns for a common credential
/// class. Disabling one is an explicit opt-out — the operator takes
/// responsibility for those secrets appearing in event data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuiltinRedactions {
    /// Generic API key patterns (`sk-`, `pk_`, `Bearer ...`).
    #[serde(default = "default_true")]
    pub api_keys: bool,

    /// Username/password patterns in URLs and config values.
    #[serde(default = "default_true")]
    pub credentials: bool,

    /// PEM-encoded private keys (`-----BEGIN ... PRIVATE KEY-----`).
    #[serde(default = "default_true")]
    pub private_keys: bool,

    /// AWS access key IDs and secret access keys.
    #[serde(default = "default_true")]
    pub aws_keys: bool,
}

impl Default for BuiltinRedactions {
    fn default() -> Self {
        Self {
            api_keys: true,
            credentials: true,
            private_keys: true,
            aws_keys: true,
        }
    }
}

/// A single user-defined regex redaction rule.
///
/// The `regex` is applied to every field listed in
/// [`RedactConfig::scan_fields`]. Any match is replaced by `replacement`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RedactPattern {
    /// Human-readable label used in audit logs.
    pub name: String,

    /// Regular expression to match sensitive substrings.
    pub regex: String,

    /// Token inserted in place of each match.
    #[serde(default = "default_redacted_token")]
    pub replacement: String,
}

/// Three-tier redaction configuration.
///
/// Applied in order: path exclusion, field dropping, then value scanning.
/// Each tier is independent — an event can be affected by all three.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RedactConfig {
    /// Tier 1 — glob patterns of file paths that must never be captured.
    ///
    /// Any file matching one of these patterns is silently excluded from
    /// all content capture. Metadata events (open/close) are still emitted.
    #[serde(default = "default_exclude_paths")]
    pub exclude_paths: Vec<String>,

    /// Tier 2 — dot-path field names that are dropped before indexing.
    ///
    /// Paths use dot notation matching event JSON structure, e.g.
    /// `http_request.headers.authorization`.
    #[serde(default = "default_drop_fields")]
    pub drop_fields: Vec<String>,

    /// Tier 3 — event fields whose values are scanned for secrets.
    ///
    /// Matched substrings are replaced with the pattern's `replacement`.
    #[serde(default = "default_scan_fields")]
    pub scan_fields: Vec<String>,

    /// Which built-in pattern sets are active for Tier 3 scanning.
    #[serde(default)]
    pub builtins: BuiltinRedactions,

    /// User-defined regex patterns added on top of built-ins.
    #[serde(default)]
    pub patterns: Vec<RedactPattern>,
}

impl Default for RedactConfig {
    fn default() -> Self {
        Self {
            exclude_paths: default_exclude_paths(),
            drop_fields: default_drop_fields(),
            scan_fields: default_scan_fields(),
            builtins: BuiltinRedactions::default(),
            patterns: Vec::new(),
        }
    }
}

impl RedactConfig {
    /// Build a `HashSet` from `scan_fields` for O(1) membership checks.
    ///
    /// Call once at startup and pass the result into the redaction engine.
    #[must_use]
    pub fn scan_field_set(&self) -> HashSet<String> {
        self.scan_fields.iter().cloned().collect()
    }

    /// Build a `HashSet` from `drop_fields` for O(1) membership checks.
    ///
    /// Call once at startup and pass the result into the redaction engine.
    #[must_use]
    pub fn drop_field_set(&self) -> HashSet<String> {
        self.drop_fields.iter().cloned().collect()
    }

    /// Static default drop fields for hot-path lookups without allocation.
    #[must_use]
    pub fn default_drop_set() -> &'static HashSet<&'static str> {
        &DEFAULT_DROP_SET
    }

    /// Static default scan fields for hot-path lookups without allocation.
    #[must_use]
    pub fn default_scan_set() -> &'static HashSet<&'static str> {
        &DEFAULT_SCAN_SET
    }
}

fn default_exclude_paths() -> Vec<String> {
    DEFAULT_EXCLUDE_PATHS.iter().map(|s| String::from(*s)).collect()
}

fn default_drop_fields() -> Vec<String> {
    DEFAULT_DROP_FIELDS.iter().map(|s| String::from(*s)).collect()
}

fn default_scan_fields() -> Vec<String> {
    DEFAULT_SCAN_FIELDS.iter().map(|s| String::from(*s)).collect()
}

fn default_redacted_token() -> String {
    String::from("[REDACTED]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_have_builtins_enabled() {
        let cfg = RedactConfig::default();
        assert!(cfg.builtins.api_keys);
        assert!(cfg.builtins.credentials);
        assert!(cfg.builtins.private_keys);
        assert!(cfg.builtins.aws_keys);
    }

    #[test]
    fn defaults_have_drop_fields() {
        let cfg = RedactConfig::default();
        assert!(cfg.drop_fields.iter().any(|f| f.contains("authorization")));
        assert!(cfg.drop_fields.iter().any(|f| f.contains("cookie")));
    }

    #[test]
    fn defaults_scan_fields_targets_pii_likely() {
        let cfg = RedactConfig::default();
        let set = cfg.scan_field_set();
        assert!(set.contains("stdio.text"));
        assert!(set.contains("exec.envp"));
        assert!(set.contains("http_request.body"));
    }

    #[test]
    fn parse_yaml_with_custom_pattern() {
        let yaml = r#"
patterns:
  - name: github_pat
    regex: "ghp_[A-Za-z0-9]{36}"
    replacement: "[GH-TOKEN]"
"#;
        let cfg: RedactConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.patterns.len(), 1);
        assert_eq!(cfg.patterns[0].name, "github_pat");
        assert_eq!(cfg.patterns[0].replacement, "[GH-TOKEN]");
        // Defaults are still present.
        assert!(cfg.builtins.api_keys);
    }

    #[test]
    fn parse_yaml_adds_scan_field() {
        let yaml = r#"
scan_fields:
  - stdio.text
  - exec.envp
  - custom.field
"#;
        let cfg: RedactConfig = serde_yaml::from_str(yaml).unwrap();
        let set = cfg.scan_field_set();
        assert!(set.contains("custom.field"));
    }

    #[test]
    fn round_trip() {
        let cfg = RedactConfig::default();
        let yaml = serde_yaml::to_string(&cfg).unwrap();
        let parsed: RedactConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed, cfg);
    }

    #[test]
    fn drop_field_set_from_config() {
        let cfg = RedactConfig::default();
        let set = cfg.drop_field_set();
        assert!(set.contains("http_request.headers.authorization"));
        assert!(set.contains("http_request.headers.cookie"));
        assert!(set.contains("http_request.headers.x-api-key"));
        assert_eq!(set.len(), 3);
    }

    #[test]
    fn static_default_sets_match_config() {
        let drop_set = RedactConfig::default_drop_set();
        assert!(drop_set.contains("http_request.headers.authorization"));
        let scan_set = RedactConfig::default_scan_set();
        assert!(scan_set.contains("stdio.text"));
        assert!(scan_set.contains("exec.envp"));
    }
}
