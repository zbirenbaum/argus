//! Pause-before-action rule configuration.
//!
//! Rules are evaluated on every intercepted syscall entry. When a rule
//! matches, the supervisor either pauses the tracee (awaiting approval)
//! or denies the syscall outright by injecting `EPERM`.

// Rust guideline compliant 2026-02-21

use serde::{Deserialize, Serialize};

/// A single pause-before-action rule.
///
/// Each rule targets a specific syscall category and narrows the match
/// with path globs, binary names, or network destinations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PauseRule {
    /// Which syscall category this rule applies to.
    pub match_kind: PauseMatchKind,

    /// Glob patterns matched against the syscall's resolved path.
    /// Only relevant for file-oriented match kinds.
    #[serde(default)]
    pub paths: Vec<String>,

    /// Binary basenames that trigger this rule (for `exec` match kind).
    #[serde(default)]
    pub binaries: Vec<String>,

    /// Network destination patterns like `*:22` or `10.0.0.0/8:*`.
    /// Only relevant for `connect` match kind.
    #[serde(default)]
    pub destinations: Vec<String>,

    /// What to do when the rule matches. Defaults to pause.
    #[serde(default)]
    pub action: PauseAction,
}

/// Syscall category that a pause rule targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PauseMatchKind {
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

/// Action taken when a pause rule matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PauseAction {
    /// Freeze the tracee and emit a `pending_approval` event.
    #[default]
    Pause,

    /// Immediately inject `EPERM` without waiting for approval.
    Deny,
}

impl PauseRule {
    /// Test whether this rule matches a syscall with the given context.
    ///
    /// `kind` is the syscall category, `path` is the resolved filesystem
    /// path (if any), `binary` is the executable basename (for exec),
    /// and `destination` is the network target (for connect).
    pub fn matches(
        &self,
        kind: PauseMatchKind,
        path: Option<&str>,
        binary: Option<&str>,
        destination: Option<&str>,
    ) -> bool {
        if self.match_kind != kind {
            return false;
        }

        match kind {
            PauseMatchKind::Unlink
            | PauseMatchKind::Write
            | PauseMatchKind::Rename
            | PauseMatchKind::Chmod => self.matches_path(path),
            PauseMatchKind::Exec => self.matches_binary(binary),
            PauseMatchKind::Connect => self.matches_destination(destination),
        }
    }

    fn matches_path(&self, path: Option<&str>) -> bool {
        if self.paths.is_empty() {
            return true;
        }
        let Some(path) = path else {
            return false;
        };
        self.paths.iter().any(|pattern| {
            glob::Pattern::new(pattern)
                .map(|p| p.matches(path))
                .unwrap_or(false)
        })
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
        if self.destinations.is_empty() {
            return true;
        }
        let Some(dest) = destination else {
            return false;
        };
        self.destinations.iter().any(|pattern| {
            glob::Pattern::new(pattern)
                .map(|p| p.matches(dest))
                .unwrap_or(false)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unlink_rule() -> PauseRule {
        PauseRule {
            match_kind: PauseMatchKind::Unlink,
            paths: vec!["/workspace/**".into()],
            binaries: Vec::new(),
            destinations: Vec::new(),
            action: PauseAction::Pause,
        }
    }

    fn exec_rule() -> PauseRule {
        PauseRule {
            match_kind: PauseMatchKind::Exec,
            paths: Vec::new(),
            binaries: vec!["rm".into(), "curl".into(), "wget".into()],
            destinations: Vec::new(),
            action: PauseAction::Pause,
        }
    }

    fn connect_rule() -> PauseRule {
        PauseRule {
            match_kind: PauseMatchKind::Connect,
            paths: Vec::new(),
            binaries: Vec::new(),
            destinations: vec!["*:22".into(), "*:25".into()],
            action: PauseAction::Deny,
        }
    }

    #[test]
    fn unlink_matches_workspace_path() {
        let rule = unlink_rule();
        assert!(rule.matches(
            PauseMatchKind::Unlink,
            Some("/workspace/important.txt"),
            None,
            None,
        ));
    }

    #[test]
    fn unlink_ignores_outside_workspace() {
        let rule = unlink_rule();
        assert!(!rule.matches(
            PauseMatchKind::Unlink,
            Some("/tmp/scratch.txt"),
            None,
            None,
        ));
    }

    #[test]
    fn unlink_rule_ignores_wrong_kind() {
        let rule = unlink_rule();
        assert!(!rule.matches(
            PauseMatchKind::Write,
            Some("/workspace/foo.txt"),
            None,
            None,
        ));
    }

    #[test]
    fn exec_matches_listed_binary() {
        let rule = exec_rule();
        assert!(rule.matches(PauseMatchKind::Exec, None, Some("rm"), None));
        assert!(rule.matches(PauseMatchKind::Exec, None, Some("curl"), None));
    }

    #[test]
    fn exec_ignores_unlisted_binary() {
        let rule = exec_rule();
        assert!(!rule.matches(PauseMatchKind::Exec, None, Some("ls"), None));
    }

    #[test]
    fn connect_matches_port_glob() {
        let rule = connect_rule();
        assert!(rule.matches(
            PauseMatchKind::Connect,
            None,
            None,
            Some("10.0.0.1:22"),
        ));
        assert!(rule.matches(
            PauseMatchKind::Connect,
            None,
            None,
            Some("mail.example.com:25"),
        ));
    }

    #[test]
    fn connect_ignores_unmatched_port() {
        let rule = connect_rule();
        assert!(!rule.matches(
            PauseMatchKind::Connect,
            None,
            None,
            Some("api.example.com:443"),
        ));
    }

    #[test]
    fn deny_action_serde() {
        let rule = connect_rule();
        assert_eq!(rule.action, PauseAction::Deny);
        let yaml = serde_yaml::to_string(&rule).unwrap();
        let parsed: PauseRule = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.action, PauseAction::Deny);
    }

    #[test]
    fn default_action_is_pause() {
        assert_eq!(PauseAction::default(), PauseAction::Pause);
    }

    #[test]
    fn match_kind_round_trip() {
        for kind in [
            PauseMatchKind::Unlink,
            PauseMatchKind::Exec,
            PauseMatchKind::Write,
            PauseMatchKind::Connect,
            PauseMatchKind::Rename,
            PauseMatchKind::Chmod,
        ] {
            let json = serde_json::to_string(&kind).unwrap();
            let parsed: PauseMatchKind = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, kind);
        }
    }

    #[test]
    fn empty_paths_matches_any_path() {
        let rule = PauseRule {
            match_kind: PauseMatchKind::Write,
            paths: Vec::new(),
            binaries: Vec::new(),
            destinations: Vec::new(),
            action: PauseAction::Pause,
        };
        assert!(rule.matches(
            PauseMatchKind::Write,
            Some("/any/path/file.txt"),
            None,
            None,
        ));
    }

    #[test]
    fn write_rule_with_extension_globs() {
        let rule = PauseRule {
            match_kind: PauseMatchKind::Write,
            paths: vec!["*.env".into(), "*.key".into(), "*.pem".into()],
            binaries: Vec::new(),
            destinations: Vec::new(),
            action: PauseAction::Pause,
        };
        assert!(rule.matches(PauseMatchKind::Write, Some(".env"), None, None));
        assert!(rule.matches(PauseMatchKind::Write, Some("server.key"), None, None));
        assert!(!rule.matches(PauseMatchKind::Write, Some("main.py"), None, None));
    }
}
