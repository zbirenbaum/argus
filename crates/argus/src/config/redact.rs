// Rust guideline compliant 2026-02-21
//! Redaction configuration — three-tier PII scrubbing pipeline.
//!
//! Tier 1 (`exclude_paths`): entire files are dropped from capture.
//! Tier 2 (`drop_fields`): named event fields are removed before indexing.
//! Tier 3 (`scan_fields` + `patterns`): field values are regex-scanned and
//! matching substrings are replaced with a redaction token.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

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
    #[must_use]
    pub fn scan_field_set(&self) -> HashSet<String> {
        self.scan_fields.iter().cloned().collect()
    }
}

fn default_exclude_paths() -> Vec<String> {
    vec![
        "*.env".into(),
        "*.pem".into(),
        "*.key".into(),
        "credentials.json".into(),
        ".ssh/**".into(),
    ]
}

fn default_drop_fields() -> Vec<String> {
    vec![
        "http_request.headers.authorization".into(),
        "http_request.headers.cookie".into(),
        "http_request.headers.x-api-key".into(),
    ]
}

fn default_scan_fields() -> Vec<String> {
    vec![
        "http_request.headers".into(),
        "http_request.body".into(),
        "http_response.headers".into(),
        "http_response.body".into(),
        "stdio.text".into(),
        "exec.envp".into(),
    ]
}

fn default_true() -> bool {
    true
}

fn default_redacted_token() -> String {
    "[REDACTED]".into()
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
}
