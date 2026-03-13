// Rust guideline compliant 2026-02-21
//! Classified event after syscall stops are decoded into semantic operations.

use std::net::SocketAddr;
use std::path::PathBuf;

use nix::unistd::Pid;
use serde::{Deserialize, Serialize};

use super::raw_stop::RawSyscallStop;

/// Direction of stdin/stdout/stderr flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StdioType {
    Stdin,
    Stdout,
    Stderr,
}

/// Which end of a pipe the data crosses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipeDirection {
    Read,
    Write,
}

/// Which side of a PTY pair produced the data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PtyDataType {
    Master,
    Slave,
}

/// Semantic label attached to a decoded syscall stop.
#[derive(Debug, Clone)]
pub enum Classification {
    FileWrite {
        path: PathBuf,
        fd: i32,
        buf_addr: usize,
        len: usize,
    },
    FileRead {
        path: PathBuf,
        fd: i32,
        buf_addr: usize,
        len: usize,
    },
    FileRename {
        old_path: PathBuf,
        new_path: PathBuf,
    },
    FileUnlink {
        path: PathBuf,
    },
    FileMkdir {
        path: PathBuf,
    },
    FileRmdir {
        path: PathBuf,
    },
    FileChmod {
        path: PathBuf,
        mode: u32,
    },
    FileTruncate {
        path: PathBuf,
        len: u64,
    },
    FileLink {
        target: PathBuf,
        link_path: PathBuf,
    },
    FileSymlink {
        target: PathBuf,
        link_path: PathBuf,
    },
    FileOpen {
        path: PathBuf,
        flags: i32,
        mode: u32,
    },
    FileClose {
        fd: i32,
    },
    Stdio {
        subtype: StdioType,
        pipe_inode: Option<u64>,
        buf_addr: usize,
        len: usize,
    },
    PipeCreate {
        read_fd: i32,
        write_fd: i32,
        inode: u64,
    },
    PipeData {
        inode: u64,
        direction: PipeDirection,
        buf_addr: usize,
        len: usize,
    },
    PtyCreate {
        master_fd: i32,
        slave_path: PathBuf,
    },
    PtyData {
        subtype: PtyDataType,
        buf_addr: usize,
        len: usize,
    },
    FdDup {
        old_fd: i32,
        new_fd: i32,
    },
    ProcessExec {
        binary: PathBuf,
        argv: Vec<String>,
        envp: Vec<String>,
    },
    ProcessFork {
        parent: Pid,
        child: Pid,
    },
    ProcessExit {
        exit_code: i32,
    },
    NetSocket {
        domain: i32,
        sock_type: i32,
        fd: i32,
    },
    NetConnect {
        fd: i32,
        addr: SocketAddr,
    },
    NetAccept {
        fd: i32,
        peer: SocketAddr,
    },
    /// Syscall that does not require further pipeline processing.
    Passthrough,
}

/// A stop that has been decoded into a semantic classification.
#[derive(Debug)]
pub struct ClassifiedEvent {
    pub pid: Pid,
    pub raw: RawSyscallStop,
    pub classification: Classification,
}

impl ClassifiedEvent {
    /// Returns the syscall number as a string, for use in blocked event records.
    pub fn syscall_name(&self) -> String {
        use super::raw_stop::StopType;
        match self.raw.stop_type {
            StopType::SyscallEntry { syscall_nr, .. }
            | StopType::SyscallExit { syscall_nr, .. } => format!("syscall_{syscall_nr}"),
            _ => "unknown".to_string(),
        }
    }

    /// Returns the primary path associated with this event, if any.
    pub fn primary_path(&self) -> Option<String> {
        match &self.classification {
            Classification::FileWrite { path, .. }
            | Classification::FileRead { path, .. }
            | Classification::FileUnlink { path }
            | Classification::FileMkdir { path }
            | Classification::FileRmdir { path }
            | Classification::FileChmod { path, .. }
            | Classification::FileTruncate { path, .. } => Some(path.display().to_string()),
            Classification::FileRename { old_path, .. } => Some(old_path.display().to_string()),
            Classification::FileLink { target, .. }
            | Classification::FileSymlink { target, .. } => Some(target.display().to_string()),
            _ => None,
        }
    }
}
