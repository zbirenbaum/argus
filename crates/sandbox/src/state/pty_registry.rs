//! PTY master/slave pair tracking for traced processes.
//!
//! Records the relationship between PTY master and slave file descriptors
//! so the supervisor can reconstruct terminal I/O streams.

// Rust guideline compliant 2026-02-21

use std::collections::HashMap;
use std::os::fd::RawFd;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Metadata for one PTY pair, keyed by PTY number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PtyInfo {
    /// PID that opened the master.
    pub master_pid: u32,
    /// File descriptor of the master side.
    pub master_fd: RawFd,
    /// Path to the slave device (e.g., `/dev/pts/3`).
    pub slave_path: PathBuf,
    /// PID that opened the slave, if known.
    pub slave_pid: Option<u32>,
    /// File descriptor of the slave side, if known.
    pub slave_fd: Option<RawFd>,
}

/// Tracks all active PTY pairs by PTY number.
#[derive(Debug, Clone, Default)]
pub struct PtyRegistry {
    ptys: HashMap<i32, PtyInfo>,
}

impl PtyRegistry {
    /// Creates an empty PTY registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a new PTY master (from `openat(/dev/ptmx)`).
    pub fn register_master(&mut self, pty_num: i32, pid: u32, fd: RawFd) {
        let info = PtyInfo {
            master_pid: pid,
            master_fd: fd,
            slave_path: PathBuf::from(format!("/dev/pts/{pty_num}")),
            slave_pid: None,
            slave_fd: None,
        };
        self.ptys.insert(pty_num, info);
    }

    /// Records slave-side open (from `openat(/dev/pts/N)`).
    pub fn register_slave(&mut self, pty_num: i32, pid: u32, fd: RawFd) {
        if let Some(info) = self.ptys.get_mut(&pty_num) {
            info.slave_pid = Some(pid);
            info.slave_fd = Some(fd);
        }
    }

    /// Returns PTY info by number.
    pub fn get(&self, pty_num: i32) -> Option<&PtyInfo> {
        self.ptys.get(&pty_num)
    }

    /// Looks up a PTY by its slave path.
    pub fn find_by_slave_path(&self, path: &std::path::Path) -> Option<(&i32, &PtyInfo)> {
        self.ptys.iter().find(|(_, info)| info.slave_path == path)
    }

    /// Removes a PTY pair.
    pub fn remove(&mut self, pty_num: i32) -> Option<PtyInfo> {
        self.ptys.remove(&pty_num)
    }

    /// Returns the number of tracked PTY pairs.
    pub fn len(&self) -> usize {
        self.ptys.len()
    }

    /// Returns `true` if no PTY pairs are tracked.
    pub fn is_empty(&self) -> bool {
        self.ptys.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_master_and_slave() {
        let mut reg = PtyRegistry::new();
        reg.register_master(3, 100, 5);

        let info = reg.get(3).unwrap();
        assert_eq!(info.master_pid, 100);
        assert_eq!(info.master_fd, 5);
        assert_eq!(info.slave_path, PathBuf::from("/dev/pts/3"));
        assert!(info.slave_pid.is_none());

        reg.register_slave(3, 200, 0);
        let info = reg.get(3).unwrap();
        assert_eq!(info.slave_pid, Some(200));
        assert_eq!(info.slave_fd, Some(0));
    }

    #[test]
    fn find_by_slave_path() {
        let mut reg = PtyRegistry::new();
        reg.register_master(3, 100, 5);

        let result = reg.find_by_slave_path(std::path::Path::new("/dev/pts/3"));
        assert!(result.is_some());
        let (&num, info) = result.unwrap();
        assert_eq!(num, 3);
        assert_eq!(info.master_pid, 100);
    }

    #[test]
    fn find_by_slave_path_miss() {
        let reg = PtyRegistry::new();
        assert!(reg.find_by_slave_path(std::path::Path::new("/dev/pts/99")).is_none());
    }

    #[test]
    fn remove_pty() {
        let mut reg = PtyRegistry::new();
        reg.register_master(3, 100, 5);
        assert_eq!(reg.len(), 1);

        let removed = reg.remove(3);
        assert!(removed.is_some());
        assert!(reg.is_empty());
    }

    #[test]
    fn register_slave_without_master_is_noop() {
        let mut reg = PtyRegistry::new();
        reg.register_slave(99, 200, 0);
        assert!(reg.is_empty());
    }
}
