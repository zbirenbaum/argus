// Rust guideline compliant 2026-02-21
//! Live tracee registry shared between the ptrace thread and the API.
//!
//! The ptrace thread owns tracee lifecycle: it learns new PIDs from
//! fork/clone events and drops them when they are reaped. The API needs
//! the same list to answer `GET /agent/status` and to freeze every
//! traced process on `POST /agent/pause`, so the set lives behind an
//! `Arc` shared by both.
//!
//! Reads are lock-free (`DashSet`), which matters because the ptrace
//! thread touches this on every fork and exit.

use dashmap::DashSet;
use nix::unistd::Pid;

/// Set of PIDs currently under ptrace control.
///
/// Cheap to clone behind an `Arc`; all methods take `&self`.
#[derive(Debug, Default)]
pub struct TraceeRegistry {
    pids: DashSet<i32>,
}

impl TraceeRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a PID as an active tracee.
    pub fn insert(&self, pid: Pid) {
        self.pids.insert(pid.as_raw());
    }

    /// Drop a PID that has been reaped.
    pub fn remove(&self, pid: Pid) {
        self.pids.remove(&pid.as_raw());
    }

    /// Whether `pid` is a known tracee.
    pub fn contains(&self, pid: Pid) -> bool {
        self.pids.contains(&pid.as_raw())
    }

    /// Snapshot of all live tracee PIDs, ascending.
    ///
    /// Sorted so freeze order and API output are deterministic.
    pub fn pids(&self) -> Vec<Pid> {
        let mut raw: Vec<i32> = self.pids.iter().map(|p| *p).collect();
        raw.sort_unstable();
        raw.into_iter().map(Pid::from_raw).collect()
    }

    /// Number of live tracees.
    pub fn len(&self) -> usize {
        self.pids.len()
    }

    /// Whether no tracees remain.
    pub fn is_empty(&self) -> bool {
        self.pids.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pid(n: i32) -> Pid {
        Pid::from_raw(n)
    }

    #[test]
    fn new_registry_is_empty() {
        let reg = TraceeRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        assert!(reg.pids().is_empty());
    }

    #[test]
    fn insert_and_contains() {
        let reg = TraceeRegistry::new();
        reg.insert(pid(10));
        assert!(reg.contains(pid(10)));
        assert!(!reg.contains(pid(11)));
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn remove_drops_pid() {
        let reg = TraceeRegistry::new();
        reg.insert(pid(10));
        reg.remove(pid(10));
        assert!(!reg.contains(pid(10)));
        assert!(reg.is_empty());
    }

    #[test]
    fn pids_are_sorted() {
        let reg = TraceeRegistry::new();
        reg.insert(pid(30));
        reg.insert(pid(10));
        reg.insert(pid(20));
        assert_eq!(reg.pids(), vec![pid(10), pid(20), pid(30)]);
    }

    #[test]
    fn duplicate_insert_is_idempotent() {
        let reg = TraceeRegistry::new();
        reg.insert(pid(10));
        reg.insert(pid(10));
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn shared_across_threads() {
        let reg = std::sync::Arc::new(TraceeRegistry::new());
        let writer = {
            let reg = std::sync::Arc::clone(&reg);
            std::thread::spawn(move || reg.insert(pid(42)))
        };
        writer.join().unwrap();
        assert!(reg.contains(pid(42)));
    }
}
