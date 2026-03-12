//! Pipe lifecycle tracking across traced processes.
//!
//! Tracks readers and writers for each pipe inode so the supervisor knows when
//! a pipe is fully closed and can attribute I/O to specific processes.

use std::collections::HashMap;
use std::os::fd::RawFd;

use serde::{Deserialize, Serialize};

use super::fd_table::{FdTable, FdTarget, PipeEnd};

/// Metadata for one pipe, keyed by inode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipeInfo {
    /// (pid, fd) pairs holding the write end.
    pub writers: Vec<(u32, RawFd)>,
    /// (pid, fd) pairs holding the read end.
    pub readers: Vec<(u32, RawFd)>,
    /// PID that created the pipe.
    pub created_by: u32,
}

/// Tracks all active pipes by inode.
#[derive(Debug, Clone, Default)]
pub struct PipeRegistry {
    pipes: HashMap<u64, PipeInfo>,
}

impl PipeRegistry {
    /// Creates an empty pipe registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a new pipe with read and write endpoints.
    pub fn create_pipe(&mut self, pid: u32, inode: u64, read_fd: RawFd, write_fd: RawFd) {
        let info = PipeInfo {
            writers: vec![(pid, write_fd)],
            readers: vec![(pid, read_fd)],
            created_by: pid,
        };
        self.pipes.insert(inode, info);
    }

    /// Duplicates pipe endpoints when a process forks.
    ///
    /// Scans the child's fd table for pipe targets and adds the child as
    /// an additional reader/writer.
    pub fn on_fork(&mut self, child_pid: u32, child_fds: &FdTable) {
        for (&fd, target) in child_fds.iter() {
            if let FdTarget::Pipe { inode, direction } = target
                && let Some(info) = self.pipes.get_mut(inode)
            {
                let endpoint = (child_pid, fd);
                match direction {
                    PipeEnd::Read => {
                        if !info.readers.contains(&endpoint) {
                            info.readers.push(endpoint);
                        }
                    }
                    PipeEnd::Write => {
                        if !info.writers.contains(&endpoint) {
                            info.writers.push(endpoint);
                        }
                    }
                }
            }
        }
    }

    /// Removes a single endpoint when a file descriptor is closed.
    ///
    /// Returns `true` if the pipe was fully cleaned up (no remaining
    /// readers or writers).
    pub fn on_close(&mut self, pid: u32, fd: RawFd, inode: u64) -> bool {
        let Some(info) = self.pipes.get_mut(&inode) else {
            return false;
        };
        info.readers.retain(|&e| e != (pid, fd));
        info.writers.retain(|&e| e != (pid, fd));

        if info.readers.is_empty() && info.writers.is_empty() {
            self.pipes.remove(&inode);
            return true;
        }
        false
    }

    /// Registers a duplicated pipe fd.
    pub fn on_dup(&mut self, pid: u32, _old_fd: RawFd, new_fd: RawFd, fd_table: &FdTable) {
        let Some(target) = fd_table.get(new_fd) else {
            return;
        };
        let FdTarget::Pipe { inode, direction } = target else {
            return;
        };
        let Some(info) = self.pipes.get_mut(inode) else {
            return;
        };
        let endpoint = (pid, new_fd);
        match direction {
            PipeEnd::Read => {
                if !info.readers.contains(&endpoint) {
                    info.readers.push(endpoint);
                }
            }
            PipeEnd::Write => {
                if !info.writers.contains(&endpoint) {
                    info.writers.push(endpoint);
                }
            }
        }
    }

    /// Returns pipe info by inode.
    pub fn get(&self, inode: u64) -> Option<&PipeInfo> {
        self.pipes.get(&inode)
    }

    /// Returns the number of tracked pipes.
    pub fn len(&self) -> usize {
        self.pipes.len()
    }

    /// Returns `true` if no pipes are tracked.
    pub fn is_empty(&self) -> bool {
        self.pipes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn make_fd_table_with_pipe(inode: u64, read_fd: RawFd, write_fd: RawFd) -> FdTable {
        let mut table = FdTable::new();
        table.insert(
            read_fd,
            FdTarget::Pipe {
                inode,
                direction: PipeEnd::Read,
            },
        );
        table.insert(
            write_fd,
            FdTarget::Pipe {
                inode,
                direction: PipeEnd::Write,
            },
        );
        table
    }

    #[test]
    fn create_pipe_tracks_both_ends() {
        let mut reg = PipeRegistry::new();
        reg.create_pipe(100, 42, 3, 4);

        let info = reg.get(42).unwrap();
        assert_eq!(info.readers, vec![(100, 3)]);
        assert_eq!(info.writers, vec![(100, 4)]);
        assert_eq!(info.created_by, 100);
    }

    #[test]
    fn fork_duplicates_endpoints() {
        let mut reg = PipeRegistry::new();
        reg.create_pipe(100, 42, 3, 4);

        let child_fds = make_fd_table_with_pipe(42, 3, 4);
        reg.on_fork(200, &child_fds);

        let info = reg.get(42).unwrap();
        assert_eq!(info.readers.len(), 2);
        assert!(info.readers.contains(&(100, 3)));
        assert!(info.readers.contains(&(200, 3)));
        assert_eq!(info.writers.len(), 2);
        assert!(info.writers.contains(&(100, 4)));
        assert!(info.writers.contains(&(200, 4)));
    }

    #[test]
    fn close_removes_endpoint() {
        let mut reg = PipeRegistry::new();
        reg.create_pipe(100, 42, 3, 4);

        let cleaned = reg.on_close(100, 3, 42);
        assert!(!cleaned);
        let info = reg.get(42).unwrap();
        assert!(info.readers.is_empty());
        assert_eq!(info.writers.len(), 1);
    }

    #[test]
    fn pipe_cleaned_up_when_all_endpoints_closed() {
        let mut reg = PipeRegistry::new();
        reg.create_pipe(100, 42, 3, 4);

        assert!(!reg.on_close(100, 3, 42));
        assert!(reg.on_close(100, 4, 42));
        assert!(reg.is_empty());
    }

    #[test]
    fn on_dup_adds_new_endpoint() {
        let mut reg = PipeRegistry::new();
        reg.create_pipe(100, 42, 3, 4);

        let mut fd_table = FdTable::new();
        fd_table.insert(
            3,
            FdTarget::Pipe {
                inode: 42,
                direction: PipeEnd::Read,
            },
        );
        fd_table.insert(
            7,
            FdTarget::Pipe {
                inode: 42,
                direction: PipeEnd::Read,
            },
        );

        reg.on_dup(100, 3, 7, &fd_table);

        let info = reg.get(42).unwrap();
        assert_eq!(info.readers.len(), 2);
        assert!(info.readers.contains(&(100, 7)));
    }

    #[test]
    fn close_nonexistent_inode_returns_false() {
        let mut reg = PipeRegistry::new();
        assert!(!reg.on_close(100, 3, 999));
    }

    #[test]
    fn fork_does_not_duplicate_existing_endpoints() {
        let mut reg = PipeRegistry::new();
        reg.create_pipe(100, 42, 3, 4);

        // Fork with same pid (edge case)
        let child_fds = make_fd_table_with_pipe(42, 3, 4);
        reg.on_fork(100, &child_fds);

        let info = reg.get(42).unwrap();
        // Should not duplicate
        assert_eq!(info.readers.len(), 1);
        assert_eq!(info.writers.len(), 1);
    }

    #[test]
    fn on_dup_with_non_pipe_fd_is_noop() {
        let mut reg = PipeRegistry::new();
        reg.create_pipe(100, 42, 3, 4);

        let mut fd_table = FdTable::new();
        fd_table.insert(
            7,
            FdTarget::File {
                path: PathBuf::from("/tmp/x"),
            },
        );

        reg.on_dup(100, 3, 7, &fd_table);
        let info = reg.get(42).unwrap();
        assert_eq!(info.readers.len(), 1);
    }
}
