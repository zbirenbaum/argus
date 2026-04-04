// Rust guideline compliant 2026-02-21
//! Classification stage: decodes raw ptrace stops into semantic events.
//!
//! Maintains per-pid fd tables, pipe registry, and pty registry so that
//! fd numbers can be resolved to paths. Unknown or uninteresting syscalls
//! are classified as `Passthrough`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use dashmap::DashMap;
use nix::unistd::Pid;

use crate::cas::ContentHash;
use crate::state::fd_table::{FdTarget, PipeEnd};
use crate::state::{FdTable, PipeRegistry, ProcessTree, PtyRegistry};
use crate::pipeline::classified::{ClassifiedEvent, Classification};
use crate::pipeline::ptrace_thread::PtraceHandle;
use crate::pipeline::raw_stop::{RawSyscallStop, StopType};

use super::syscall_handlers;

/// Saved state from a syscall entry that requires exit correlation.
///
/// Syscalls like openat need the return value (fd number) from exit
/// to update the fd table. The entry handler stores args here and
/// the exit handler consumes them.
#[derive(Debug)]
pub enum PendingEntry {
    Openat { path: PathBuf, flags: i32, mode: u32 },
    Pipe { pipe_array_addr: usize },
    Socket { domain: i32, sock_type: i32 },
    Dup { old_fd: i32, new_fd: Option<i32> },
}

/// Stage that classifies raw ptrace stops into structured events.
pub struct ClassifyStage {
    pub handle: PtraceHandle,
    pub fd_tables: Arc<DashMap<Pid, FdTable>>,
    pub pipe_registry: Arc<parking_lot::Mutex<PipeRegistry>>,
    pub pty_registry: Arc<parking_lot::Mutex<PtyRegistry>>,
    pub process_tree: parking_lot::Mutex<ProcessTree>,
    /// Syscall entries awaiting their exit stop for fd/state updates.
    pub pending: DashMap<Pid, PendingEntry>,
    /// Whether transparent connect() rewriting is active.
    pub transparent_mode: bool,
    /// Proxy address to rewrite HTTPS destinations to.
    pub proxy_addr: SocketAddr,
    /// Tracked file content hashes for hash-chain correctness.
    ///
    /// Updated on O_TRUNC opens (set to empty hash) and shared with
    /// `CaptureStage` so writes use tracked state instead of racy
    /// filesystem reads.
    pub file_state: Arc<DashMap<PathBuf, ContentHash>>,
}

impl ClassifyStage {
    /// Create a new classification stage.
    pub fn new(
        handle: PtraceHandle,
        fd_tables: Arc<DashMap<Pid, FdTable>>,
        pipe_registry: Arc<parking_lot::Mutex<PipeRegistry>>,
        pty_registry: Arc<parking_lot::Mutex<PtyRegistry>>,
        transparent_mode: bool,
        proxy_addr: SocketAddr,
        file_state: Arc<DashMap<PathBuf, ContentHash>>,
    ) -> Self {
        Self {
            handle, fd_tables, pipe_registry, pty_registry,
            process_tree: parking_lot::Mutex::new(ProcessTree::new()),
            pending: DashMap::new(),
            transparent_mode, proxy_addr, file_state,
        }
    }

    /// Classify one raw ptrace stop.
    pub async fn classify(&self, stop: RawSyscallStop) -> ClassifiedEvent {
        let pid = stop.pid;
        let classification = match &stop.stop_type {
            StopType::SyscallEntry { syscall_nr, args } => {
                syscall_handlers::handle_entry(self, pid, *syscall_nr, *args).await
            }
            StopType::SyscallExit { return_value, .. } => {
                self.handle_exit(pid, *return_value).await
            }
            StopType::Fork { parent, child } => {
                self.on_fork(*parent, *child);
                Classification::ProcessFork { parent: *parent, child: *child }
            }
            StopType::Exec { pid: p } => {
                self.on_program_replace(*p)
            }
            StopType::Exit { pid: p, exit_code } => {
                self.on_exit(*p);
                Classification::ProcessExit { exit_code: *exit_code }
            }
            StopType::Signal { .. }
            | StopType::Unknown => Classification::Passthrough,
        };

        ClassifiedEvent { pid, raw: stop, classification }
    }

    /// Process a syscall exit by correlating with a stored pending entry.
    ///
    /// If no pending entry exists for this pid (non-seccomp syscall or
    /// entry-only syscall), returns Passthrough.
    async fn handle_exit(&self, pid: Pid, return_value: i64) -> Classification {
        let Some((_, pending)) = self.pending.remove(&pid) else {
            return Classification::Passthrough;
        };

        // Negative return value means the syscall failed.
        if return_value < 0 {
            return Classification::Passthrough;
        }

        match pending {
            PendingEntry::Openat { path, flags, .. } => {
                let fd = return_value as i32;
                // Seed the hash chain for newly-tracked paths so the first
                // write has a valid before_hash. Only insert if absent —
                // overwriting would break the chain between concurrent writes.
                if flags & libc::O_TRUNC != 0 {
                    self.file_state
                        .entry(path.clone())
                        .or_insert_with(|| ContentHash::from_data(b""));
                }
                let target = FdTarget::File { path };
                let cloexec = flags & libc::O_CLOEXEC != 0;
                {
                    let mut entry = self.fd_tables.entry(pid).or_default();
                    if cloexec {
                        entry.insert_cloexec(fd, target);
                    } else {
                        entry.insert(fd, target);
                    }
                }
                Classification::Passthrough
            }
            PendingEntry::Pipe { pipe_array_addr } => {
                self.handle_pipe_exit(pid, pipe_array_addr).await
            }
            PendingEntry::Socket { domain, sock_type } => {
                let fd = return_value as i32;
                let target = FdTarget::Socket { domain, addr: None };
                self.fd_tables.entry(pid).or_default().insert(fd, target);
                Classification::NetSocket { domain, sock_type, fd }
            }
            PendingEntry::Dup { old_fd, new_fd } => {
                let actual_new_fd = new_fd.unwrap_or(return_value as i32);
                if let Some(mut table) = self.fd_tables.get_mut(&pid) {
                    table.dup(old_fd, actual_new_fd);
                    self.pipe_registry.lock().on_dup(
                        pid.as_raw() as u32, old_fd, actual_new_fd, &table,
                    );
                }
                Classification::FdDup { old_fd, new_fd: actual_new_fd }
            }
        }
    }

    /// Read pipe fds from tracee memory after pipe2() returns.
    async fn handle_pipe_exit(&self, pid: Pid, pipe_array_addr: usize) -> Classification {
        // pipe2() writes two ints (read_fd, write_fd) at the array address.
        let fds_bytes = match self.handle.read_memory(pid, pipe_array_addr, 8).await {
            Ok(b) if b.len() >= 8 => b,
            _ => return Classification::Passthrough,
        };
        let Some(read_bytes) = fds_bytes.get(0..4).and_then(|s| <[u8; 4]>::try_from(s).ok()) else {
            return Classification::Passthrough;
        };
        let Some(write_bytes) = fds_bytes.get(4..8).and_then(|s| <[u8; 4]>::try_from(s).ok()) else {
            return Classification::Passthrough;
        };
        let read_fd = i32::from_ne_bytes(read_bytes);
        let write_fd = i32::from_ne_bytes(write_bytes);

        // Resolve inode via /proc/pid/fd/read_fd symlink.
        let inode = match self.handle.resolve_fd(pid, read_fd).await {
            Ok(p) => p.to_string_lossy()
                .strip_prefix("pipe:[")
                .and_then(|s| s.strip_suffix(']'))
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0),
            Err(_) => 0,
        };

        // Update fd table with both pipe endpoints.
        {
            let mut entry = self.fd_tables.entry(pid).or_default();
            entry.insert(read_fd, FdTarget::Pipe { inode, direction: PipeEnd::Read });
            entry.insert(write_fd, FdTarget::Pipe { inode, direction: PipeEnd::Write });
        }

        // Update pipe registry.
        {
            let mut reg = self.pipe_registry.lock();
            reg.create_pipe(pid.as_raw() as u32, inode, read_fd, write_fd);
        }

        Classification::PipeCreate { read_fd, write_fd, inode }
    }

    /// Copy the parent's fd table to the new child on fork and update process tree.
    pub fn on_fork(&self, parent: Pid, child: Pid) {
        let cloned = self.fd_tables.get(&parent).map(|t| t.clone_for_fork());
        if let Some(table) = cloned {
            self.pipe_registry.lock().on_fork(child.as_raw() as u32, &table);
            self.fd_tables.insert(child, table);
        }

        let parent_pid = parent.as_raw() as u32;
        let child_pid = child.as_raw() as u32;
        let mut tree = self.process_tree.lock();
        let (binary, argv, cwd) = tree.get_process(parent_pid)
            .map(|p| (p.binary.clone(), p.argv.clone(), p.cwd.clone()))
            .unwrap_or_default();
        tree.add_process(child_pid, parent_pid, binary, argv, cwd, FdTable::new());
    }

    /// Drop cloexec fds after program replacement and classify the stop.
    ///
    /// Binary and argv are read from /proc directly since the old image
    /// has already been replaced by the time we see this stop.
    pub fn on_program_replace(&self, pid: Pid) -> Classification {
        if let Some(mut table) = self.fd_tables.get_mut(&pid) {
            table.close_cloexec();
        }
        let binary = read_proc_exe(pid);
        let argv = read_cmdline(pid);
        self.process_tree.lock().update_on_program_replace(
            pid.as_raw() as u32, binary.clone(), argv.clone(),
        );
        Classification::ProcessExec { binary, argv, envp: Vec::new() }
    }

    /// Remove the fd table entry when a process exits and update process tree.
    pub fn on_exit(&self, pid: Pid) {
        self.fd_tables.remove(&pid);
        self.process_tree.lock().mark_exited(pid.as_raw() as u32);
    }

    /// Stream-compatible classification.
    ///
    /// Resumes passthroughs internally and returns `None` for them.
    /// Returns `Some(ClassifiedEvent)` only for interesting events
    /// that need downstream processing.
    pub async fn process(&self, stop: RawSyscallStop) -> Option<ClassifiedEvent> {
        let classified = self.classify(stop).await;
        if matches!(classified.classification, Classification::Passthrough) {
            let trace_exit = self.pending.contains_key(&classified.pid);
            let signal = match &classified.raw.stop_type {
                StopType::Signal { signal, .. } => {
                    nix::sys::signal::Signal::try_from(*signal).ok()
                }
                _ => None,
            };
            self.handle.resume(classified.pid, trace_exit, signal);
            return None;
        }
        Some(classified)
    }
}

/// Read the executable path for a pid from `/proc/{pid}/exe`.
pub fn read_proc_exe(pid: Pid) -> PathBuf {
    let link = format!("/proc/{}/exe", pid.as_raw());
    std::fs::read_link(&link).unwrap_or_else(|_| PathBuf::from("<unknown>"))
}

/// Read argv from `/proc/{pid}/cmdline` (null-delimited args).
pub fn read_cmdline(pid: Pid) -> Vec<String> {
    let path = format!("/proc/{}/cmdline", pid.as_raw());
    let data = std::fs::read(path).unwrap_or_default();
    data.split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}
