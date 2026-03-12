// Rust guideline compliant 2026-02-21
//! File content and metadata event payloads.

use serde::{Deserialize, Serialize};

/// File data was read from a tracked path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Read {
    pub pid: u32,
    pub path: String,
    pub fd: i32,
    pub offset: u64,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
}

/// File data was written to a tracked path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Write {
    pub pid: u32,
    pub path: String,
    pub fd: i32,
    pub offset: u64,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree_hash: Option<String>,
}

/// A file or directory was renamed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rename {
    pub pid: u32,
    pub old_path: String,
    pub new_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree_hash: Option<String>,
}

/// A file was removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unlink {
    pub pid: u32,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree_hash: Option<String>,
}

/// A directory was created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mkdir {
    pub pid: u32,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree_hash: Option<String>,
}

/// A directory was removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rmdir {
    pub pid: u32,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree_hash: Option<String>,
}

/// File permissions were changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chmod {
    pub pid: u32,
    pub path: String,
    pub old_mode: u32,
    pub new_mode: u32,
}

/// A file was truncated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Truncate {
    pub pid: u32,
    pub path: String,
    pub old_size: u64,
    pub new_size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree_hash: Option<String>,
}

/// A hard link was created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub pid: u32,
    pub target: String,
    pub link_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree_hash: Option<String>,
}

/// A symbolic link was created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Symlink {
    pub pid: u32,
    pub target: String,
    pub link_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree_hash: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_round_trip() {
        let w = Write {
            pid: 10,
            path: "/workspace/out.csv".into(),
            fd: 3,
            offset: 0,
            size: 4096,
            before_hash: Some("ab12".into()),
            after_hash: Some("cd34".into()),
            tree_hash: Some("ef56".into()),
        };
        let json = serde_json::to_string(&w).unwrap();
        let back: Write = serde_json::from_str(&json).unwrap();
        assert_eq!(w, back);
    }

    #[test]
    fn read_omits_none_hash() {
        let r = Read {
            pid: 5,
            path: "/workspace/data.bin".into(),
            fd: 4,
            offset: 0,
            size: 512,
            content_hash: None,
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("content_hash"));
    }

    #[test]
    fn rename_round_trip() {
        let r = Rename {
            pid: 7,
            old_path: "/workspace/a.txt".into(),
            new_path: "/workspace/b.txt".into(),
            tree_hash: None,
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: Rename = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn chmod_round_trip() {
        let c = Chmod {
            pid: 1,
            path: "/workspace/script.sh".into(),
            old_mode: 0o644,
            new_mode: 0o755,
        };
        let json = serde_json::to_string(&c).unwrap();
        let back: Chmod = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn truncate_round_trip() {
        let t = Truncate {
            pid: 2,
            path: "/workspace/log.txt".into(),
            old_size: 1024,
            new_size: 0,
            before_hash: Some("aa".into()),
            after_hash: Some("bb".into()),
            tree_hash: None,
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: Truncate = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn link_symlink_round_trip() {
        let l = Link {
            pid: 3,
            target: "/workspace/original".into(),
            link_path: "/workspace/hardlink".into(),
            tree_hash: None,
        };
        let json = serde_json::to_string(&l).unwrap();
        let back: Link = serde_json::from_str(&json).unwrap();
        assert_eq!(l, back);

        let s = Symlink {
            pid: 3,
            target: "/workspace/original".into(),
            link_path: "/workspace/symlink".into(),
            tree_hash: Some("ff00".into()),
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: Symlink = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn unlink_mkdir_rmdir_round_trip() {
        let u = Unlink {
            pid: 1,
            path: "/workspace/tmp.txt".into(),
            content_hash: Some("dead".into()),
            tree_hash: None,
        };
        let json = serde_json::to_string(&u).unwrap();
        let back: Unlink = serde_json::from_str(&json).unwrap();
        assert_eq!(u, back);

        let m = Mkdir {
            pid: 1,
            path: "/workspace/newdir".into(),
            tree_hash: None,
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: Mkdir = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);

        let r = Rmdir {
            pid: 1,
            path: "/workspace/newdir".into(),
            tree_hash: Some("ab".into()),
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: Rmdir = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }
}
