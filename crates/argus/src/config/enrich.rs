// Rust guideline compliant 2026-02-21
//! Enrichment configuration — controls what event data gets inlined vs referenced.
//!
//! Each event category can be individually enabled and capped at a maximum byte
//! size. The global `max_inline_bytes` acts as a fallback ceiling when a
//! category does not set its own limit.

use serde::{Deserialize, Serialize};

/// Per-category enrichment settings.
///
/// Controls whether a data category is captured at all and how many bytes
/// are inlined into the event record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CategoryConfig {
    /// Whether this category is captured.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Maximum bytes to inline for this category.
    ///
    /// Overrides [`EnrichConfig::max_inline_bytes`] when set. Data beyond
    /// this limit is stored in CAS and referenced by digest instead.
    #[serde(default)]
    pub max_bytes: Option<usize>,
}

impl Default for CategoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_bytes: None,
        }
    }
}

/// Inline enrichment configuration for captured event data.
///
/// Controls which categories of raw data are embedded directly into event
/// records and the byte ceiling for each. Anything exceeding a limit is
/// written to CAS and replaced with a content-addressed reference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnrichConfig {
    /// Master switch — when false, no data is inlined in any category.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Default maximum bytes to inline when a category has no explicit limit.
    ///
    /// 256 KiB balances query convenience against event record size. Larger
    /// payloads are better served via CAS digest lookup.
    #[serde(default = "default_max_inline_bytes")]
    pub max_inline_bytes: usize,

    /// Text written to stdout/stderr.
    #[serde(default)]
    pub stdio_text: CategoryConfig,

    /// Data flowing through anonymous pipes.
    #[serde(default)]
    pub pipe_data: CategoryConfig,

    /// Data flowing through PTY masters.
    #[serde(default)]
    pub pty_data: CategoryConfig,

    /// Full file contents on open/write/close.
    #[serde(default)]
    pub file_content: CategoryConfig,

    /// Content of files at the moment they are unlinked.
    #[serde(default)]
    pub delete_content: CategoryConfig,

    /// Content of files at the moment they are truncated.
    #[serde(default)]
    pub truncate_content: CategoryConfig,

    /// HTTP request and response headers.
    #[serde(default)]
    pub http_headers: CategoryConfig,

    /// HTTP request and response bodies.
    #[serde(default)]
    pub http_bodies: CategoryConfig,

    /// Environment variables passed to exec calls.
    #[serde(default)]
    pub exec_envp: CategoryConfig,
}

impl Default for EnrichConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_inline_bytes: default_max_inline_bytes(),
            stdio_text: CategoryConfig::default(),
            pipe_data: CategoryConfig::default(),
            pty_data: CategoryConfig::default(),
            file_content: CategoryConfig::default(),
            delete_content: CategoryConfig::default(),
            truncate_content: CategoryConfig::default(),
            http_headers: CategoryConfig::default(),
            http_bodies: CategoryConfig::default(),
            exec_envp: CategoryConfig::default(),
        }
    }
}

/// Data categories that can be individually configured for enrichment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    StdioText,
    PipeData,
    PtyData,
    FileContent,
    DeleteContent,
    TruncateContent,
    HttpHeaders,
    HttpBodies,
    ExecEnvp,
}

impl EnrichConfig {
    /// Effective byte ceiling for a category.
    ///
    /// Returns the category's own `max_bytes` if set, otherwise falls back to
    /// [`EnrichConfig::max_inline_bytes`].
    #[must_use]
    pub fn max_bytes_for(&self, category: Category) -> usize {
        let cat = self.category(category);
        cat.max_bytes.unwrap_or(self.max_inline_bytes)
    }

    /// Whether a given category should be inlined at all.
    ///
    /// Returns false when the global switch is off or the category is
    /// individually disabled.
    #[must_use]
    pub fn should_inline(&self, category: Category) -> bool {
        self.enabled && self.category(category).enabled
    }

    fn category(&self, category: Category) -> &CategoryConfig {
        match category {
            Category::StdioText => &self.stdio_text,
            Category::PipeData => &self.pipe_data,
            Category::PtyData => &self.pty_data,
            Category::FileContent => &self.file_content,
            Category::DeleteContent => &self.delete_content,
            Category::TruncateContent => &self.truncate_content,
            Category::HttpHeaders => &self.http_headers,
            Category::HttpBodies => &self.http_bodies,
            Category::ExecEnvp => &self.exec_envp,
        }
    }
}

/// 256 KiB — large enough for typical files, small enough to keep events scannable.
const fn default_max_inline_bytes() -> usize {
    256 * 1024
}

pub(crate) const fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_all_enabled() {
        let cfg = EnrichConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.max_inline_bytes, 256 * 1024);
        for cat in [
            Category::StdioText,
            Category::PipeData,
            Category::PtyData,
            Category::FileContent,
            Category::DeleteContent,
            Category::TruncateContent,
            Category::HttpHeaders,
            Category::HttpBodies,
            Category::ExecEnvp,
        ] {
            assert!(cfg.should_inline(cat), "{cat:?} should be enabled by default");
        }
    }

    #[test]
    fn disabled_globally() {
        let cfg = EnrichConfig {
            enabled: false,
            ..EnrichConfig::default()
        };
        for cat in [Category::StdioText, Category::FileContent] {
            assert!(!cfg.should_inline(cat));
        }
    }

    #[test]
    fn category_max_bytes_override() {
        let cfg = EnrichConfig {
            file_content: CategoryConfig {
                enabled: true,
                max_bytes: Some(1024),
            },
            ..EnrichConfig::default()
        };
        assert_eq!(cfg.max_bytes_for(Category::FileContent), 1024);
        // Other categories fall back to the global limit.
        assert_eq!(cfg.max_bytes_for(Category::StdioText), 256 * 1024);
    }

    #[test]
    fn parse_yaml_minimal() {
        let yaml = "{}";
        let cfg: EnrichConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.max_inline_bytes, 256 * 1024);
    }

    #[test]
    fn parse_yaml_with_category() {
        let yaml = r#"
enabled: true
max_inline_bytes: 512
file_content:
  enabled: false
"#;
        let cfg: EnrichConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.max_inline_bytes, 512);
        assert!(!cfg.file_content.enabled);
        assert!(cfg.stdio_text.enabled);
    }
}
