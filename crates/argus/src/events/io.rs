//! Stdio, pipe, PTY, and fd redirect event payloads.

use serde::{Deserialize, Serialize};

/// Direction of stdio stream data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StdioSubtype {
    Stdout,
    Stderr,
    Stdin,
}

/// Data passed through a standard I/O stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stdio {
    pub pid: u32,
    pub subtype: StdioSubtype,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pipe_inode: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dest_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_pid: Option<u32>,
    /// Inline content; absent when content was not inlined.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Encoding of `text`; absent means UTF-8, `"base64"` means binary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
}

/// A pipe was created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipeCreate {
    pub pid: u32,
    pub inode: u64,
    pub read_fd: i32,
    pub write_fd: i32,
}

/// Direction of data flow in a pipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipeDirection {
    Read,
    Write,
}

/// Data was transferred through a pipe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipeData {
    pub pid: u32,
    pub inode: u64,
    pub direction: PipeDirection,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dest_pids: Vec<u32>,
    /// Inline content; absent when content was not inlined.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Encoding of `text`; absent means UTF-8, `"base64"` means binary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
}

/// A pipe endpoint was closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipeClose {
    pub pid: u32,
    pub inode: u64,
    pub direction: PipeDirection,
}

/// A pseudo-terminal pair was created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PtyCreate {
    pub pid: u32,
    pub master_fd: i32,
    pub slave_path: String,
}

/// Direction of PTY data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PtySubtype {
    SlaveWrite,
    MasterRead,
}

/// Data was transferred through a PTY.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PtyData {
    pub pid: u32,
    pub subtype: PtySubtype,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    pub size: u64,
    pub slave_path: String,
    /// Inline content; absent when content was not inlined.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Encoding of `text`; absent means UTF-8, `"base64"` means binary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
}

/// Describes the target of a file descriptor redirect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FdTarget {
    #[serde(rename = "type")]
    pub target_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inode: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<PipeDirection>,
}

/// A file descriptor was redirected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FdRedirect {
    pub pid: u32,
    pub fd: i32,
    pub target: FdTarget,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdio_round_trip() {
        let s = Stdio {
            pid: 5,
            subtype: StdioSubtype::Stdout,
            content_hash: Some("abc123".into()),
            size: 256,
            pipe_inode: Some(12345),
            dest_pid: Some(6),
            source_pid: None,
            text: None,
            encoding: None,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("source_pid"));
        let back: Stdio = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn pipe_create_round_trip() {
        let p = PipeCreate {
            pid: 1,
            inode: 99,
            read_fd: 3,
            write_fd: 4,
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: PipeCreate = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn pipe_data_round_trip() {
        let d = PipeData {
            pid: 1,
            inode: 99,
            direction: PipeDirection::Write,
            content_hash: Some("ff".into()),
            size: 128,
            dest_pids: vec![2, 3],
            text: None,
            encoding: None,
        };
        let json = serde_json::to_string(&d).unwrap();
        let back: PipeData = serde_json::from_str(&json).unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn pipe_close_round_trip() {
        let c = PipeClose {
            pid: 1,
            inode: 99,
            direction: PipeDirection::Read,
        };
        let json = serde_json::to_string(&c).unwrap();
        let back: PipeClose = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn pty_create_round_trip() {
        let p = PtyCreate {
            pid: 1,
            master_fd: 5,
            slave_path: "/dev/pts/0".into(),
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: PtyCreate = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn pty_data_round_trip() {
        let d = PtyData {
            pid: 1,
            subtype: PtySubtype::SlaveWrite,
            content_hash: Some("aa".into()),
            size: 64,
            slave_path: "/dev/pts/0".into(),
            text: None,
            encoding: None,
        };
        let json = serde_json::to_string(&d).unwrap();
        let back: PtyData = serde_json::from_str(&json).unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn stdio_inline_text_round_trip() {
        let s = Stdio {
            pid: 1,
            subtype: StdioSubtype::Stderr,
            content_hash: None,
            size: 5,
            pipe_inode: None,
            dest_pid: None,
            source_pid: None,
            text: Some("hello".into()),
            encoding: None,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"text\""));
        assert!(!json.contains("\"encoding\""));
        let back: Stdio = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn pipe_data_inline_binary_round_trip() {
        let d = PipeData {
            pid: 1,
            inode: 1,
            direction: PipeDirection::Read,
            content_hash: None,
            size: 4,
            dest_pids: vec![],
            text: Some("AAAA".into()),
            encoding: Some("base64".into()),
        };
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains("\"base64\""));
        let back: PipeData = serde_json::from_str(&json).unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn pty_data_inline_text_round_trip() {
        let d = PtyData {
            pid: 2,
            subtype: PtySubtype::MasterRead,
            content_hash: None,
            size: 3,
            slave_path: "/dev/pts/1".into(),
            text: Some("ls\n".into()),
            encoding: None,
        };
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains("\"text\""));
        let back: PtyData = serde_json::from_str(&json).unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn fd_redirect_round_trip() {
        let r = FdRedirect {
            pid: 1,
            fd: 1,
            target: FdTarget {
                target_type: "pipe".into(),
                inode: Some(42),
                path: None,
                direction: Some(PipeDirection::Write),
            },
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("\"path\""));
        let back: FdRedirect = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }
}
