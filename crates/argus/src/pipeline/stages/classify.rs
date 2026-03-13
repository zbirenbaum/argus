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

use crate::state::{FdTable, PipeRegistry, PtyRegistry};
use crate::pipeline::classified::{ClassifiedEvent, Classification};
use crate::pipeline::ptrace_thread::PtraceHandle;
use crate::pipeline::raw_stop::{RawSyscallStop, StopType};

pub use super::sockaddr::{encode_sockaddr, is_tls_port, parse_sockaddr};
use super::syscall_handlers;

/// Stage that classifies raw ptrace stops into structured events.
pub struct ClassifyStage {
    pub handle: PtraceHandle,
    pub fd_tables: Arc<DashMap<Pid, FdTable>>,
    pub pipe_registry: Arc<std::sync::Mutex<PipeRegistry>>,
    pub pty_registry: Arc<std::sync::Mutex<PtyRegistry>>,
    /// Whether transparent connect() rewriting is active.
    pub transparent_mode: bool,
    /// Proxy address to rewrite HTTPS destinations to.
    pub proxy_addr: SocketAddr,
}

impl ClassifyStage {
    /// Create a new classification stage.
    pub fn new(
        handle: PtraceHandle,
        fd_tables: Arc<DashMap<Pid, FdTable>>,
        pipe_registry: Arc<std::sync::Mutex<PipeRegistry>>,
        pty_registry: Arc<std::sync::Mutex<PtyRegistry>>,
        transparent_mode: bool,
        proxy_addr: SocketAddr,
    ) -> Self {
        Self { handle, fd_tables, pipe_registry, pty_registry, transparent_mode, proxy_addr }
    }

    /// Classify one raw ptrace stop.
    pub async fn classify(&self, stop: RawSyscallStop) -> ClassifiedEvent {
        let pid = stop.pid;
        let classification = match &stop.stop_type {
            StopType::SyscallEntry { syscall_nr, args } => {
                syscall_handlers::handle_entry(self, pid, *syscall_nr, *args).await
            }
            StopType::Fork { parent, child } => {
                self.on_fork(*parent, *child);
                Classification::ProcessFork { parent: *parent, child: *child }
            }
            StopType::Exec { pid: p } => {
                self.on_exec(*p)
            }
            StopType::Exit { pid: p, exit_code } => {
                self.on_exit(*p);
                Classification::ProcessExit { exit_code: *exit_code }
            }
            StopType::SyscallExit { .. }
            | StopType::Signal { .. }
            | StopType::Unknown => Classification::Passthrough,
        };

        ClassifiedEvent { pid, raw: stop, classification }
    }

    /// Copy the parent's fd table to the new child on fork.
    pub fn on_fork(&self, parent: Pid, child: Pid) {
        let cloned = self.fd_tables.get(&parent).map(|t| t.clone_for_fork());
        if let Some(table) = cloned {
            self.fd_tables.insert(child, table);
        }
    }

    /// Drop cloexec fds after exec and classify the exec stop.
    ///
    /// Binary and argv are read from /proc directly since the old image
    /// has already been replaced by execve by the time we see this stop.
    pub fn on_exec(&self, pid: Pid) -> Classification {
        if let Some(mut table) = self.fd_tables.get_mut(&pid) {
            table.close_cloexec();
        }
        let binary = read_proc_exe(pid);
        let argv = read_cmdline(pid);
        Classification::ProcessExec { binary, argv, envp: Vec::new() }
    }

    /// Remove the fd table entry when a process exits.
    pub fn on_exit(&self, pid: Pid) {
        self.fd_tables.remove(&pid);
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
