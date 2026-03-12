// Rust guideline compliant 2026-02-21
//! Process lifecycle event payloads.

use serde::{Deserialize, Serialize};

/// A new process image was loaded via `execve`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exec {
    pub pid: u32,
    pub ppid: u32,
    pub binary: String,
    pub argv: Vec<String>,
    pub envp: Vec<String>,
    pub cwd: String,
}

/// A child process was created via `fork`/`clone`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fork {
    pub parent_pid: u32,
    pub child_pid: u32,
}

/// A process terminated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exit {
    pub pid: u32,
    pub exit_code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_round_trip() {
        let exec = Exec {
            pid: 1,
            ppid: 0,
            binary: "/bin/sh".into(),
            argv: vec!["/bin/sh".into(), "-c".into(), "echo hi".into()],
            envp: vec!["PATH=/usr/bin".into()],
            cwd: "/workspace".into(),
        };
        let json = serde_json::to_string(&exec).unwrap();
        let back: Exec = serde_json::from_str(&json).unwrap();
        assert_eq!(exec, back);
    }

    #[test]
    fn fork_round_trip() {
        let fork = Fork {
            parent_pid: 1,
            child_pid: 2,
        };
        let json = serde_json::to_string(&fork).unwrap();
        let back: Fork = serde_json::from_str(&json).unwrap();
        assert_eq!(fork, back);
    }

    #[test]
    fn exit_round_trip() {
        let exit = Exit {
            pid: 2,
            exit_code: 0,
            signal: None,
        };
        let json = serde_json::to_string(&exit).unwrap();
        assert!(!json.contains("signal"));
        let back: Exit = serde_json::from_str(&json).unwrap();
        assert_eq!(exit, back);

        let exit_sig = Exit {
            pid: 3,
            exit_code: -1,
            signal: Some(9),
        };
        let json = serde_json::to_string(&exit_sig).unwrap();
        assert!(json.contains("\"signal\":9"));
        let back: Exit = serde_json::from_str(&json).unwrap();
        assert_eq!(exit_sig, back);
    }
}
