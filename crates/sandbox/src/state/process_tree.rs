//! Process tree tracking for traced process hierarchies.
//!
//! Maintains parent-child relationships, per-process metadata (binary, argv,
//! cwd), and liveness state. Updated on fork, program replacement, and exit.

use std::collections::HashMap;
use std::os::fd::RawFd;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::fd_table::{FdTable, FdTarget};

/// State of a single traced process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessState {
    pub pid: u32,
    pub ppid: u32,
    pub binary: PathBuf,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    // Fd tables are rebuilt from /proc on checkpoint restore, not serialized
    #[serde(skip)]
    pub fds: FdTable,
    pub alive: bool,
}

/// Tracks all traced processes and their relationships.
#[derive(Debug, Clone, Default)]
pub struct ProcessTree {
    processes: HashMap<u32, ProcessState>,
}

impl ProcessTree {
    /// Creates an empty process tree.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a new process (on fork/clone).
    ///
    /// The child inherits its parent's fd table via `clone_for_fork`.
    pub fn add_process(
        &mut self,
        pid: u32,
        ppid: u32,
        binary: PathBuf,
        argv: Vec<String>,
        cwd: PathBuf,
        fds: FdTable,
    ) {
        let state = ProcessState {
            pid,
            ppid,
            binary,
            argv,
            cwd,
            fds,
            alive: true,
        };
        self.processes.insert(pid, state);
    }

    /// Updates a process after program replacement.
    ///
    /// Sets the new binary and argv, then drops all `FD_CLOEXEC` fds.
    /// Returns the list of closed (fd, target) pairs, or `None` if
    /// the process was not found.
    pub fn update_on_program_replace(
        &mut self,
        pid: u32,
        binary: PathBuf,
        argv: Vec<String>,
    ) -> Option<Vec<(RawFd, FdTarget)>> {
        let proc_state = self.processes.get_mut(&pid)?;
        proc_state.binary = binary;
        proc_state.argv = argv;
        Some(proc_state.fds.close_cloexec())
    }

    /// Marks a process as no longer alive.
    ///
    /// Returns the exited process's fd table so callers can clean up
    /// pipe and PTY registries.
    pub fn mark_exited(&mut self, pid: u32) -> Option<FdTable> {
        let proc_state = self.processes.get_mut(&pid)?;
        proc_state.alive = false;
        Some(std::mem::take(&mut proc_state.fds))
    }

    /// Returns process state by PID.
    pub fn get_process(&self, pid: u32) -> Option<&ProcessState> {
        self.processes.get(&pid)
    }

    /// Returns mutable process state by PID.
    pub fn get_process_mut(&mut self, pid: u32) -> Option<&mut ProcessState> {
        self.processes.get_mut(&pid)
    }

    /// Returns PIDs of all direct children of a process.
    pub fn get_children(&self, pid: u32) -> Vec<u32> {
        self.processes
            .values()
            .filter(|p| p.ppid == pid)
            .map(|p| p.pid)
            .collect()
    }

    /// Returns the number of tracked processes.
    pub fn len(&self) -> usize {
        self.processes.len()
    }

    /// Returns `true` if no processes are tracked.
    pub fn is_empty(&self) -> bool {
        self.processes.is_empty()
    }

    /// Returns PIDs of all living processes.
    pub fn alive_pids(&self) -> Vec<u32> {
        self.processes
            .values()
            .filter(|p| p.alive)
            .map(|p| p.pid)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tree_with_parent() -> ProcessTree {
        let mut tree = ProcessTree::new();
        tree.add_process(
            1,
            0,
            PathBuf::from("/bin/bash"),
            vec!["bash".into()],
            PathBuf::from("/home"),
            FdTable::new(),
        );
        tree
    }

    #[test]
    fn add_and_get_process() {
        let tree = make_tree_with_parent();
        let proc_state = tree.get_process(1).unwrap();
        assert_eq!(proc_state.pid, 1);
        assert_eq!(proc_state.ppid, 0);
        assert_eq!(proc_state.binary, PathBuf::from("/bin/bash"));
        assert!(proc_state.alive);
    }

    #[test]
    fn fork_adds_child() {
        let mut tree = make_tree_with_parent();
        let parent_fds = tree.get_process(1).unwrap().fds.clone_for_fork();
        tree.add_process(
            2,
            1,
            PathBuf::from("/bin/bash"),
            vec!["bash".into()],
            PathBuf::from("/home"),
            parent_fds,
        );

        assert_eq!(tree.len(), 2);
        let children = tree.get_children(1);
        assert_eq!(children, vec![2]);
    }

    #[test]
    fn program_replace_updates_binary_and_drops_cloexec() {
        let mut tree = ProcessTree::new();
        let mut fds = FdTable::new();
        fds.insert(
            0,
            FdTarget::File {
                path: PathBuf::from("/dev/stdin"),
            },
        );
        fds.insert_cloexec(
            3,
            FdTarget::File {
                path: PathBuf::from("/tmp/secret"),
            },
        );

        tree.add_process(
            1,
            0,
            PathBuf::from("/bin/bash"),
            vec!["bash".into()],
            PathBuf::from("/"),
            fds,
        );

        let closed = tree
            .update_on_program_replace(
                1,
                PathBuf::from("/usr/bin/python"),
                vec!["python".into()],
            )
            .unwrap();

        assert_eq!(closed.len(), 1);
        let proc_state = tree.get_process(1).unwrap();
        assert_eq!(proc_state.binary, PathBuf::from("/usr/bin/python"));
        assert_eq!(proc_state.fds.len(), 1);
        assert!(proc_state.fds.get(0).is_some());
        assert!(proc_state.fds.get(3).is_none());
    }

    #[test]
    fn mark_exited_returns_fd_table() {
        let mut tree = make_tree_with_parent();
        let fds = tree.mark_exited(1);
        assert!(fds.is_some());

        let proc_state = tree.get_process(1).unwrap();
        assert!(!proc_state.alive);
    }

    #[test]
    fn get_children_returns_correct_list() {
        let mut tree = make_tree_with_parent();
        tree.add_process(
            2,
            1,
            PathBuf::from("/bin/ls"),
            vec!["ls".into()],
            PathBuf::from("/"),
            FdTable::new(),
        );
        tree.add_process(
            3,
            1,
            PathBuf::from("/bin/cat"),
            vec!["cat".into()],
            PathBuf::from("/"),
            FdTable::new(),
        );
        tree.add_process(
            4,
            2,
            PathBuf::from("/bin/grep"),
            vec!["grep".into()],
            PathBuf::from("/"),
            FdTable::new(),
        );

        let mut children = tree.get_children(1);
        children.sort();
        assert_eq!(children, vec![2, 3]);
        assert_eq!(tree.get_children(2), vec![4]);
        assert!(tree.get_children(99).is_empty());
    }

    #[test]
    fn alive_pids() {
        let mut tree = make_tree_with_parent();
        tree.add_process(
            2,
            1,
            PathBuf::from("/bin/ls"),
            vec!["ls".into()],
            PathBuf::from("/"),
            FdTable::new(),
        );
        let _ = tree.mark_exited(2);

        let alive = tree.alive_pids();
        assert_eq!(alive, vec![1]);
    }

    #[test]
    fn update_on_program_replace_nonexistent_returns_none() {
        let mut tree = ProcessTree::new();
        assert!(tree
            .update_on_program_replace(99, PathBuf::from("/bin/x"), vec![])
            .is_none());
    }

    #[test]
    fn mark_exited_nonexistent_returns_none() {
        let mut tree = ProcessTree::new();
        assert!(tree.mark_exited(99).is_none());
        assert!(tree.is_empty());
    }
}
