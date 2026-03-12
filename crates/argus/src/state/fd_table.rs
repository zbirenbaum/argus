//! File descriptor tracking for traced processes.
//!
//! Each traced process maintains a table mapping raw file descriptors to their
//! targets (files, pipes, sockets, PTYs, etc.). The table supports fork
//! (clone all entries) and exec (drop `FD_CLOEXEC` entries).

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::os::fd::RawFd;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::fd_serde;

/// Direction of a pipe endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PipeEnd {
    Read,
    Write,
}

/// Role of a PTY file descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PtyRole {
    Master,
    Slave,
}

/// Identifies what a file descriptor points to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FdTarget {
    File {
        path: PathBuf,
    },
    Pipe {
        inode: u64,
        direction: PipeEnd,
    },
    Socket {
        domain: i32,
        #[serde(
            serialize_with = "fd_serde::serialize_socket_addr",
            deserialize_with = "fd_serde::deserialize_socket_addr"
        )]
        addr: Option<SocketAddr>,
    },
    Pty {
        role: PtyRole,
        peer_path: PathBuf,
    },
    DevNull,
    Unknown,
}

/// Per-process file descriptor table.
#[derive(Debug, Clone, Default)]
pub struct FdTable {
    fds: HashMap<RawFd, FdTarget>,
    /// FDs marked with `FD_CLOEXEC`, dropped on program replacement.
    cloexec: HashSet<RawFd>,
}

impl FdTable {
    /// Creates an empty fd table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Populates an fd table from `/proc/{pid}/fd/`.
    ///
    /// Best-effort: unreadable symlinks are silently skipped.
    pub fn from_proc(pid: u32) -> Self {
        let mut table = Self::new();
        let dir = format!("/proc/{pid}/fd");
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => return table,
        };
        for entry in entries.flatten() {
            let fd: RawFd = match entry.file_name().to_string_lossy().parse() {
                Ok(n) => n,
                Err(_) => continue,
            };
            let target = match std::fs::read_link(entry.path()) {
                Ok(p) => Self::classify_link(&p),
                Err(_) => continue,
            };
            table.insert(fd, target);
        }
        table
    }

    /// Classifies a `/proc/pid/fd/N` symlink target.
    fn classify_link(path: &std::path::Path) -> FdTarget {
        let s = path.to_string_lossy();
        if s.starts_with("pipe:[") {
            let inode = s
                .trim_start_matches("pipe:[")
                .trim_end_matches(']')
                .parse::<u64>()
                .unwrap_or(0);
            FdTarget::Pipe {
                inode,
                direction: PipeEnd::Write,
            }
        } else if s == "/dev/null" {
            FdTarget::DevNull
        } else if s.starts_with("socket:[") {
            FdTarget::Socket {
                domain: 0,
                addr: None,
            }
        } else {
            FdTarget::File {
                path: path.to_path_buf(),
            }
        }
    }

    /// Inserts or replaces a file descriptor mapping.
    pub fn insert(&mut self, fd: RawFd, target: FdTarget) {
        self.fds.insert(fd, target);
    }

    /// Inserts a file descriptor and marks it `FD_CLOEXEC`.
    pub fn insert_cloexec(&mut self, fd: RawFd, target: FdTarget) {
        self.fds.insert(fd, target);
        self.cloexec.insert(fd);
    }

    /// Removes a file descriptor, returning its target if present.
    pub fn remove(&mut self, fd: RawFd) -> Option<FdTarget> {
        self.cloexec.remove(&fd);
        self.fds.remove(&fd)
    }

    /// Returns the target for a file descriptor.
    pub fn get(&self, fd: RawFd) -> Option<&FdTarget> {
        self.fds.get(&fd)
    }

    /// Returns `true` if the fd is marked `FD_CLOEXEC`.
    pub fn is_cloexec(&self, fd: RawFd) -> bool {
        self.cloexec.contains(&fd)
    }

    /// Marks a file descriptor as `FD_CLOEXEC`.
    pub fn set_cloexec(&mut self, fd: RawFd) {
        if self.fds.contains_key(&fd) {
            self.cloexec.insert(fd);
        }
    }

    /// Clears the `FD_CLOEXEC` flag on a file descriptor.
    pub fn clear_cloexec(&mut self, fd: RawFd) {
        self.cloexec.remove(&fd);
    }

    /// Clones the entire table for a forked child process.
    pub fn clone_for_fork(&self) -> Self {
        self.clone()
    }

    /// Copies the target from `old_fd` to `new_fd` (dup/dup2/dup3).
    ///
    /// If `new_fd` already exists it is silently replaced (matching kernel
    /// behavior for `dup2`). The new fd does not inherit `FD_CLOEXEC` unless
    /// explicitly set via `dup3` flags -- caller should use `set_cloexec`
    /// separately.
    pub fn dup(&mut self, old_fd: RawFd, new_fd: RawFd) -> bool {
        let Some(target) = self.fds.get(&old_fd).cloned() else {
            return false;
        };
        self.fds.insert(new_fd, target);
        self.cloexec.remove(&new_fd);
        true
    }

    /// Drops all file descriptors marked `FD_CLOEXEC`.
    ///
    /// Returns the list of closed (fd, target) pairs so callers can update
    /// pipe/pty registries.
    pub fn close_cloexec(&mut self) -> Vec<(RawFd, FdTarget)> {
        let to_close: Vec<RawFd> = self.cloexec.drain().collect();
        let mut closed = Vec::with_capacity(to_close.len());
        for fd in to_close {
            if let Some(target) = self.fds.remove(&fd) {
                closed.push((fd, target));
            }
        }
        closed
    }

    /// Returns an iterator over all (fd, target) pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&RawFd, &FdTarget)> {
        self.fds.iter()
    }

    /// Returns the number of tracked file descriptors.
    pub fn len(&self) -> usize {
        self.fds.len()
    }

    /// Returns `true` if no file descriptors are tracked.
    pub fn is_empty(&self) -> bool {
        self.fds.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_target(path: &str) -> FdTarget {
        FdTarget::File {
            path: PathBuf::from(path),
        }
    }

    #[test]
    fn insert_remove_get() {
        let mut table = FdTable::new();
        assert!(table.is_empty());

        table.insert(3, file_target("/tmp/a.txt"));
        assert_eq!(table.len(), 1);
        assert_eq!(table.get(3), Some(&file_target("/tmp/a.txt")));

        let removed = table.remove(3);
        assert_eq!(removed, Some(file_target("/tmp/a.txt")));
        assert!(table.is_empty());
        assert_eq!(table.get(3), None);
    }

    #[test]
    fn clone_for_fork_copies_all_entries() {
        let mut table = FdTable::new();
        table.insert(0, file_target("/dev/stdin"));
        table.insert(1, file_target("/dev/stdout"));
        table.insert(2, file_target("/dev/stderr"));
        table.insert_cloexec(5, file_target("/tmp/secret"));

        let forked = table.clone_for_fork();
        assert_eq!(forked.len(), 4);
        assert_eq!(forked.get(0), Some(&file_target("/dev/stdin")));
        assert_eq!(forked.get(5), Some(&file_target("/tmp/secret")));
        assert!(forked.is_cloexec(5));
    }

    #[test]
    fn close_cloexec_removes_flagged_fds() {
        let mut table = FdTable::new();
        table.insert(0, file_target("/dev/stdin"));
        table.insert_cloexec(3, file_target("/tmp/cloexec1"));
        table.insert_cloexec(4, file_target("/tmp/cloexec2"));
        table.insert(5, file_target("/tmp/keep"));

        let closed = table.close_cloexec();
        assert_eq!(closed.len(), 2);
        assert_eq!(table.len(), 2);
        assert!(table.get(0).is_some());
        assert!(table.get(5).is_some());
        assert!(table.get(3).is_none());
        assert!(table.get(4).is_none());
    }

    #[test]
    fn dup_copies_target() {
        let mut table = FdTable::new();
        table.insert(3, file_target("/tmp/orig"));

        assert!(table.dup(3, 7));
        assert_eq!(table.get(7), Some(&file_target("/tmp/orig")));
        assert!(!table.is_cloexec(7));
    }

    #[test]
    fn dup_nonexistent_returns_false() {
        let mut table = FdTable::new();
        assert!(!table.dup(99, 100));
    }

    #[test]
    fn dup2_replaces_existing() {
        let mut table = FdTable::new();
        table.insert(3, file_target("/tmp/a"));
        table.insert(4, file_target("/tmp/b"));

        assert!(table.dup(3, 4));
        assert_eq!(table.get(4), Some(&file_target("/tmp/a")));
    }

    #[test]
    fn set_clear_cloexec() {
        let mut table = FdTable::new();
        table.insert(3, file_target("/tmp/a"));

        assert!(!table.is_cloexec(3));
        table.set_cloexec(3);
        assert!(table.is_cloexec(3));
        table.clear_cloexec(3);
        assert!(!table.is_cloexec(3));
    }

    #[test]
    fn set_cloexec_on_nonexistent_fd_is_noop() {
        let mut table = FdTable::new();
        table.set_cloexec(99);
        assert!(!table.is_cloexec(99));
    }

    #[test]
    fn pipe_and_socket_targets() {
        let mut table = FdTable::new();
        table.insert(
            3,
            FdTarget::Pipe {
                inode: 12345,
                direction: PipeEnd::Read,
            },
        );
        table.insert(
            4,
            FdTarget::Socket {
                domain: 2,
                addr: Some("127.0.0.1:8080".parse().unwrap()),
            },
        );
        table.insert(5, FdTarget::DevNull);
        table.insert(6, FdTarget::Unknown);

        assert!(matches!(table.get(3), Some(FdTarget::Pipe { .. })));
        assert!(matches!(table.get(4), Some(FdTarget::Socket { .. })));
        assert!(matches!(table.get(5), Some(FdTarget::DevNull)));
        assert!(matches!(table.get(6), Some(FdTarget::Unknown)));
    }
}
