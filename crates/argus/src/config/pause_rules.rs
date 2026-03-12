// Rust guideline compliant 2026-02-21
//! Rule configuration for syscall interception.
//!
//! Rules are evaluated on every intercepted syscall entry. Block rules
//! deny immediately with EPERM. Pause-before-action rules hold the
//! tracee and wait for operator approval.
//!
//! The [`RuleSet`] is hot-reloadable via `Arc<ArcSwap<RuleSet>>` —
//! the API handler swaps atomically and the ptrace loop loads on each
//! syscall stop.

use serde::{Deserialize, Serialize};

/// A single rule targeting a syscall category.
///
/// Each rule narrows the match with path globs, binary names, or
/// network destinations. Glob patterns are pre-compiled at load time
/// via [`Rule::validate_patterns`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    /// Which syscall category this rule applies to.
    #[serde(rename = "type")]
    pub match_kind: MatchKind,

    /// Glob patterns matched against the syscall's resolved path.
    #[serde(default)]
    pub paths: Vec<String>,

    /// Binary basenames that trigger this rule (for `exec` match kind).
    #[serde(default)]
    pub binaries: Vec<String>,

    /// Network destination patterns like `*:22` or `10.0.0.0/8:*`.
    #[serde(default)]
    pub destinations: Vec<String>,

    /// Pre-compiled glob patterns for `paths`. Built by `validate_patterns`.
    #[serde(skip)]
    compiled_paths: Vec<glob::Pattern>,

    /// Pre-compiled glob patterns for `destinations`. Built by `validate_patterns`.
    #[serde(skip)]
    compiled_destinations: Vec<glob::Pattern>,
}

impl PartialEq for Rule {
    fn eq(&self, other: &Self) -> bool {
        self.match_kind == other.match_kind
            && self.paths == other.paths
            && self.binaries == other.binaries
            && self.destinations == other.destinations
    }
}

/// Syscall category that a rule targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MatchKind {
    /// File reads (`read`, `pread64`, `readv`).
    Read,

    /// File deletion (`unlink`, `unlinkat`).
    Unlink,

    /// Process execution (`execve`, `execveat`).
    Exec,

    /// File writes (`write`, `pwrite64`, `writev`, `truncate`).
    Write,

    /// Outbound network connections (`connect`).
    Connect,

    /// File renames (`rename`, `renameat`, `renameat2`).
    Rename,

    /// Permission changes (`chmod`, `fchmod`, `fchmodat`).
    Chmod,
}

/// Combined block + pause-before-action rule set.
///
/// Block rules evaluate first (instant EPERM, no approval queue).
/// Pause-before-action rules evaluate second (hold + wait for approval).
/// First match wins within each category.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RuleSet {
    /// Rules that immediately deny with EPERM.
    #[serde(default)]
    pub block: Vec<Rule>,

    /// Rules that pause for operator approval.
    #[serde(default)]
    pub pause_before: Vec<Rule>,
}

impl RuleSet {
    /// Pre-compile all glob patterns in both rule lists.
    pub fn compile_patterns(&mut self) {
        for rule in &mut self.block {
            rule.validate_patterns();
        }
        for rule in &mut self.pause_before {
            rule.validate_patterns();
        }
    }

    /// Total number of rules across both lists.
    pub fn rule_count(&self) -> usize {
        self.block.len() + self.pause_before.len()
    }

    /// Evaluate all rules against a syscall context.
    ///
    /// Returns the action to take: `Block` if a block rule matched,
    /// `Pause` if a pause-before-action rule matched, or `Allow` if
    /// no rules matched.
    pub fn evaluate(
        &self,
        kind: MatchKind,
        path: Option<&str>,
        binary: Option<&str>,
        destination: Option<&str>,
    ) -> RuleDecision {
        for (i, rule) in self.block.iter().enumerate() {
            if rule.matches(kind, path, binary, destination) {
                return RuleDecision::Block { rule_index: i };
            }
        }
        for (i, rule) in self.pause_before.iter().enumerate() {
            if rule.matches(kind, path, binary, destination) {
                return RuleDecision::Pause { rule_index: i };
            }
        }
        RuleDecision::Allow
    }

    /// Human-readable description of a block rule by index.
    pub fn block_rule_description(&self, index: usize) -> String {
        self.block
            .get(index)
            .map(Rule::description)
            .unwrap_or_else(|| "unknown".into())
    }

    /// Human-readable description of a pause rule by index.
    pub fn pause_rule_description(&self, index: usize) -> String {
        self.pause_before
            .get(index)
            .map(Rule::description)
            .unwrap_or_else(|| "unknown".into())
    }
}

/// Outcome of evaluating the rule set against a syscall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleDecision {
    /// No rules matched — allow the syscall.
    Allow,
    /// A block rule matched — inject EPERM immediately.
    Block {
        /// Index into `RuleSet::block`.
        rule_index: usize,
    },
    /// A pause-before-action rule matched — hold for approval.
    Pause {
        /// Index into `RuleSet::pause_before`.
        rule_index: usize,
    },
}

// --- Backwards compatibility aliases for config deserialization ---

/// Alias for [`Rule`] used in config YAML `pause_before` section.
pub type PauseRule = Rule;
/// Alias for [`MatchKind`].
pub type PauseMatchKind = MatchKind;

/// Action taken when a pause rule matches (legacy config field).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PauseAction {
    /// Freeze the tracee and emit a `pending_approval` event.
    #[default]
    Pause,

    /// Immediately inject `EPERM` without waiting for approval.
    Deny,
}

impl Rule {
    /// Compile glob patterns in `paths` and `destinations`.
    ///
    /// Must be called after deserialization before using `matches`.
    pub fn validate_patterns(&mut self) {
        self.compiled_paths = compile_patterns(&self.paths);
        self.compiled_destinations = compile_patterns(&self.destinations);
    }

    /// Test whether this rule matches a syscall with the given context.
    pub fn matches(
        &self,
        kind: MatchKind,
        path: Option<&str>,
        binary: Option<&str>,
        destination: Option<&str>,
    ) -> bool {
        if self.match_kind != kind {
            return false;
        }

        match kind {
            MatchKind::Read
            | MatchKind::Unlink
            | MatchKind::Write
            | MatchKind::Rename
            | MatchKind::Chmod => self.matches_path(path),
            MatchKind::Exec => self.matches_binary(binary),
            MatchKind::Connect => self.matches_destination(destination),
        }
    }

    /// Human-readable summary of this rule.
    pub fn description(&self) -> String {
        let kind = serde_json::to_string(&self.match_kind)
            .unwrap_or_else(|_| format!("{:?}", self.match_kind));
        let kind = kind.trim_matches('"');

        if !self.paths.is_empty() {
            format!("{kind} {}", self.paths.join(", "))
        } else if !self.binaries.is_empty() {
            format!("{kind} {}", self.binaries.join(", "))
        } else if !self.destinations.is_empty() {
            format!("{kind} {}", self.destinations.join(", "))
        } else {
            format!("{kind} *")
        }
    }

    fn matches_path(&self, path: Option<&str>) -> bool {
        if self.compiled_paths.is_empty() {
            return true;
        }
        let Some(path) = path else {
            return false;
        };
        self.compiled_paths.iter().any(|p| p.matches(path))
    }

    fn matches_binary(&self, binary: Option<&str>) -> bool {
        if self.binaries.is_empty() {
            return true;
        }
        let Some(binary) = binary else {
            return false;
        };
        self.binaries.iter().any(|b| b == binary)
    }

    fn matches_destination(&self, destination: Option<&str>) -> bool {
        if self.compiled_destinations.is_empty() {
            return true;
        }
        let Some(dest) = destination else {
            return false;
        };
        self.compiled_destinations.iter().any(|p| p.matches(dest))
    }
}

/// Compile a list of glob pattern strings, warning on invalid ones.
fn compile_patterns(patterns: &[String]) -> Vec<glob::Pattern> {
    patterns
        .iter()
        .filter_map(|s| match glob::Pattern::new(s) {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::warn!(
                    pattern = %s,
                    error = %e,
                    "invalid glob pattern, skipping"
                );
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_rule(
        kind: MatchKind,
        paths: Vec<String>,
        binaries: Vec<String>,
        destinations: Vec<String>,
    ) -> Rule {
        let mut rule = Rule {
            match_kind: kind,
            paths,
            binaries,
            destinations,
            compiled_paths: Vec::new(),
            compiled_destinations: Vec::new(),
        };
        rule.validate_patterns();
        rule
    }

    fn unlink_rule() -> Rule {
        make_rule(
            MatchKind::Unlink,
            vec!["/workspace/**".into()],
            Vec::new(),
            Vec::new(),
        )
    }

    fn exec_rule() -> Rule {
        make_rule(
            MatchKind::Exec,
            Vec::new(),
            vec!["rm".into(), "curl".into(), "wget".into()],
            Vec::new(),
        )
    }

    fn connect_rule() -> Rule {
        make_rule(
            MatchKind::Connect,
            Vec::new(),
            Vec::new(),
            vec!["*:22".into(), "*:25".into()],
        )
    }

    fn read_block_rule() -> Rule {
        make_rule(
            MatchKind::Read,
            vec!["*.env".into(), "*.key".into(), "*.pem".into()],
            Vec::new(),
            Vec::new(),
        )
    }

    #[test]
    fn unlink_matches_workspace_path() {
        let rule = unlink_rule();
        assert!(rule.matches(
            MatchKind::Unlink,
            Some("/workspace/important.txt"),
            None,
            None,
        ));
    }

    #[test]
    fn unlink_ignores_outside_workspace() {
        let rule = unlink_rule();
        assert!(!rule.matches(
            MatchKind::Unlink,
            Some("/tmp/scratch.txt"),
            None,
            None,
        ));
    }

    #[test]
    fn unlink_rule_ignores_wrong_kind() {
        let rule = unlink_rule();
        assert!(!rule.matches(
            MatchKind::Write,
            Some("/workspace/foo.txt"),
            None,
            None,
        ));
    }

    #[test]
    fn exec_matches_listed_binary() {
        let rule = exec_rule();
        assert!(rule.matches(MatchKind::Exec, None, Some("rm"), None));
        assert!(rule.matches(MatchKind::Exec, None, Some("curl"), None));
    }

    #[test]
    fn exec_ignores_unlisted_binary() {
        let rule = exec_rule();
        assert!(!rule.matches(MatchKind::Exec, None, Some("ls"), None));
    }

    #[test]
    fn connect_matches_port_glob() {
        let rule = connect_rule();
        assert!(rule.matches(
            MatchKind::Connect,
            None,
            None,
            Some("10.0.0.1:22"),
        ));
        assert!(rule.matches(
            MatchKind::Connect,
            None,
            None,
            Some("mail.example.com:25"),
        ));
    }

    #[test]
    fn connect_ignores_unmatched_port() {
        let rule = connect_rule();
        assert!(!rule.matches(
            MatchKind::Connect,
            None,
            None,
            Some("api.example.com:443"),
        ));
    }

    #[test]
    fn read_rule_matches_sensitive_files() {
        let rule = read_block_rule();
        assert!(rule.matches(MatchKind::Read, Some(".env"), None, None));
        assert!(rule.matches(MatchKind::Read, Some("server.key"), None, None));
        assert!(rule.matches(MatchKind::Read, Some("cert.pem"), None, None));
        assert!(!rule.matches(MatchKind::Read, Some("main.py"), None, None));
    }

    #[test]
    fn default_action_is_pause() {
        assert_eq!(PauseAction::default(), PauseAction::Pause);
    }

    #[test]
    fn match_kind_round_trip() {
        for kind in [
            MatchKind::Read,
            MatchKind::Unlink,
            MatchKind::Exec,
            MatchKind::Write,
            MatchKind::Connect,
            MatchKind::Rename,
            MatchKind::Chmod,
        ] {
            let json = serde_json::to_string(&kind).unwrap();
            let parsed: MatchKind = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, kind);
        }
    }

    #[test]
    fn empty_paths_matches_any_path() {
        let rule = make_rule(
            MatchKind::Write,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        assert!(rule.matches(
            MatchKind::Write,
            Some("/any/path/file.txt"),
            None,
            None,
        ));
    }

    #[test]
    fn write_rule_with_extension_globs() {
        let rule = make_rule(
            MatchKind::Write,
            vec!["*.env".into(), "*.key".into(), "*.pem".into()],
            Vec::new(),
            Vec::new(),
        );
        assert!(rule.matches(MatchKind::Write, Some(".env"), None, None));
        assert!(rule.matches(MatchKind::Write, Some("server.key"), None, None));
        assert!(!rule.matches(MatchKind::Write, Some("main.py"), None, None));
    }

    #[test]
    fn serde_uses_type_key() {
        let yaml = "type: unlink\npaths: [\"/workspace/**\"]\n";
        let mut rule: Rule = serde_yaml::from_str(yaml).unwrap();
        rule.validate_patterns();
        assert_eq!(rule.match_kind, MatchKind::Unlink);
        assert!(rule.matches(
            MatchKind::Unlink,
            Some("/workspace/foo.txt"),
            None,
            None,
        ));
    }

    #[test]
    fn partial_eq_compares_by_value() {
        let a = unlink_rule();
        let b = unlink_rule();
        assert_eq!(a, b);
    }

    #[test]
    fn rule_description_includes_kind_and_targets() {
        let rule = exec_rule();
        let desc = rule.description();
        assert!(desc.contains("exec"), "got: {desc}");
        assert!(desc.contains("rm"), "got: {desc}");
    }

    // --- RuleSet tests ---

    fn sample_ruleset() -> RuleSet {
        let mut rs = RuleSet {
            block: vec![read_block_rule()],
            pause_before: vec![unlink_rule(), exec_rule()],
        };
        rs.compile_patterns();
        rs
    }

    #[test]
    fn ruleset_block_takes_priority() {
        let rs = sample_ruleset();
        let decision = rs.evaluate(MatchKind::Read, Some(".env"), None, None);
        assert!(matches!(decision, RuleDecision::Block { rule_index: 0 }));
    }

    #[test]
    fn ruleset_pause_when_no_block() {
        let rs = sample_ruleset();
        let decision = rs.evaluate(
            MatchKind::Unlink,
            Some("/workspace/foo.txt"),
            None,
            None,
        );
        assert!(matches!(decision, RuleDecision::Pause { rule_index: 0 }));
    }

    #[test]
    fn ruleset_allow_when_no_match() {
        let rs = sample_ruleset();
        let decision = rs.evaluate(MatchKind::Read, Some("main.py"), None, None);
        assert_eq!(decision, RuleDecision::Allow);
    }

    #[test]
    fn ruleset_rule_count() {
        let rs = sample_ruleset();
        assert_eq!(rs.rule_count(), 3);
    }

    #[test]
    fn ruleset_serde_round_trip() {
        let rs = sample_ruleset();
        let json = serde_json::to_string(&rs).unwrap();
        let mut parsed: RuleSet = serde_json::from_str(&json).unwrap();
        parsed.compile_patterns();
        assert_eq!(parsed, rs);
    }

    #[test]
    fn empty_ruleset_allows_everything() {
        let rs = RuleSet::default();
        assert_eq!(
            rs.evaluate(MatchKind::Write, Some("/anything"), None, None),
            RuleDecision::Allow,
        );
    }
}
