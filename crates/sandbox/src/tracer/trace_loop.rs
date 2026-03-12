// Rust guideline compliant 2026-02-21
//! Main ptrace event loop.
//!
//! Sits on a dedicated thread, calling `waitpid(-1)` in a loop and
//! dispatching to handlers based on the wait status. Automatically
//! follows forks, program replacements, and exits. Emits structured
//! events over a channel for downstream consumers.

use std::collections::HashMap;
use std::os::fd::{BorrowedFd, RawFd};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Sender;

use anyhow::{Context, Result};
use nix::sys::ptrace;
use nix::sys::signal::Signal;
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;
use tracing::event;
use tracing::Level;

use crate::cas::CasStore;
use crate::events::{Event, EventPayload, SequenceGenerator};
use crate::events::file as ef;
use crate::state::{FdTable, PipeRegistry, ProcessTree, PtyRegistry, WriteLocks};
use crate::tracer::{handlers, memory, process_events};

/// Ptrace options to set on every traced process.
///
/// `TRACESYSGOOD` makes syscall-exit stops report as `SIGTRAP | 0x80`
/// so we can distinguish them from signal-delivery stops. Required for
/// the entry/exit capture flow on file-mutating syscalls.
pub const PTRACE_OPTS: ptrace::Options = ptrace::Options::from_bits_truncate(
    ptrace::Options::PTRACE_O_TRACEFORK.bits()
        | ptrace::Options::PTRACE_O_TRACEVFORK.bits()
        | ptrace::Options::PTRACE_O_TRACECLONE.bits()
        | ptrace::Options::PTRACE_O_TRACEEXEC.bits()
        | ptrace::Options::PTRACE_O_TRACEEXIT.bits()
        | ptrace::Options::PTRACE_O_TRACESECCOMP.bits()
        | ptrace::Options::PTRACE_O_TRACESYSGOOD.bits(),
);

/// What kind of file mutation triggered the capture.
#[derive(Debug)]
pub enum CaptureKind {
    /// A write/pwrite/writev/pwritev syscall.
    Write { fd: i32, size: u64 },
    /// An open with O_TRUNC that truncates existing content.
    OpenTrunc,
}

/// Saved state between syscall entry and exit for content capture.
#[derive(Debug)]
pub struct PendingCapture {
    /// SHA-256 of the file before the syscall executed.
    pub before_hash: Option<String>,
    pub path: String,
    pub pid: u32,
    pub kind: CaptureKind,
}

/// Hashes a file's content via CAS, returning `None` on any error.
pub fn hash_file_content(cas: &CasStore, path: &str) -> Option<String> {
    let data = std::fs::read(path).ok()?;
    cas.store(&data).ok().map(|h| h.to_string())
}

/// Orchestrates the ptrace event loop.
///
/// Owns all in-memory state and the event channel. Runs synchronously
/// on a dedicated thread until all traced processes have exited.
#[derive(Debug)]
pub struct TracerLoop {
    pub process_tree: ProcessTree,
    pub pipe_registry: PipeRegistry,
    pub pty_registry: PtyRegistry,
    pub write_locks: WriteLocks,
    pub cas: Arc<CasStore>,
    /// Captures awaiting syscall-exit to hash the post-mutation content.
    pub pending_captures: HashMap<u32, PendingCapture>,
    /// Last known content hash per path, used as before_hash for the
    /// next mutation. Guarantees an unbroken hash chain across events.
    pub path_hashes: HashMap<String, String>,
    event_tx: Sender<Event>,
    seq_gen: Arc<SequenceGenerator>,
    agent_id: String,
    pub alive_count: u32,
}

impl TracerLoop {
    /// Creates a new tracer loop with a shared sequence generator.
    pub fn new(
        agent_id: String,
        event_tx: Sender<Event>,
        seq_gen: Arc<SequenceGenerator>,
        cas: Arc<CasStore>,
    ) -> Self {
        Self {
            process_tree: ProcessTree::new(),
            pipe_registry: PipeRegistry::new(),
            pty_registry: PtyRegistry::new(),
            write_locks: WriteLocks::new(),
            cas,
            pending_captures: HashMap::new(),
            path_hashes: HashMap::new(),
            event_tx,
            seq_gen,
            agent_id,
            alive_count: 0,
        }
    }

    /// Runs the main ptrace loop until all traced processes exit.
    ///
    /// Attaches to `initial_pid` via `PTRACE_SEIZE`, then signals
    /// the child via `sync_pipe_w` to install seccomp and exec.
    ///
    /// # Errors
    ///
    /// Returns an error if ptrace operations fail or the wait loop
    /// encounters an unrecoverable error.
    pub fn run(&mut self, initial_pid: Pid, sync_pipe_w: RawFd) -> Result<()> {
        ptrace::seize(initial_pid, PTRACE_OPTS)
            .with_context(|| format!("ptrace seize pid {initial_pid}"))?;

        // Child is blocked on pipe read — unblock it now that seize
        // has established the trace relationship.
        // SAFETY: sync_pipe_w is a valid open fd from pipe().
        let pipe_fd = unsafe { BorrowedFd::borrow_raw(sync_pipe_w) };
        nix::unistd::write(pipe_fd, &[1u8])
            .context("write to sync pipe")?;
        nix::unistd::close(sync_pipe_w)
            .context("close sync pipe")?;

        self.register_initial_process(initial_pid)?;
        self.alive_count = 1;

        event!(
            name: "tracer.loop.start",
            Level::INFO,
            pid = initial_pid.as_raw(),
            "ptrace loop started, tracing pid {{pid}}",
        );

        self.wait_loop()
    }

    /// The core wait loop. Blocks on `waitpid(-1)` and dispatches.
    fn wait_loop(&mut self) -> Result<()> {
        loop {
            if self.alive_count == 0 {
                event!(
                    name: "tracer.loop.exit",
                    Level::INFO,
                    "all traced processes exited, stopping ptrace loop",
                );
                return Ok(());
            }

            let wall = WaitPidFlag::__WALL;
            let status = match waitpid(Pid::from_raw(-1), Some(wall)) {
                Ok(s) => s,
                Err(nix::errno::Errno::ECHILD) => {
                    event!(
                        name: "tracer.loop.no_children",
                        Level::INFO,
                        "no more children to wait for",
                    );
                    return Ok(());
                }
                Err(e) => return Err(e).context("waitpid failed"),
            };

            self.handle_wait_status(status)?;
        }
    }

    /// Dispatches a single wait status to the appropriate handler.
    fn handle_wait_status(&mut self, status: WaitStatus) -> Result<()> {
        match status {
            WaitStatus::PtraceEvent(pid, _sig, evt) => {
                let already_resumed = self.handle_ptrace_event(pid, evt)?;
                if !already_resumed {
                    ptrace::cont(pid, None)?;
                }
            }
            WaitStatus::PtraceSyscall(pid) => {
                self.handle_syscall_exit(pid)?;
            }
            WaitStatus::Stopped(pid, sig) => {
                self.handle_signal_stop(pid, sig)?;
            }
            WaitStatus::Exited(pid, code) => {
                self.pending_captures.remove(&(pid.as_raw() as u32));
                process_events::handle_process_exit(self, pid, code, None);
            }
            WaitStatus::Signaled(pid, sig, _core) => {
                self.pending_captures.remove(&(pid.as_raw() as u32));
                process_events::handle_process_exit(
                    self,
                    pid,
                    128 + sig as i32,
                    Some(sig as i32),
                );
            }
            _ => {}
        }
        Ok(())
    }

    /// Handles ptrace events (fork, clone, seccomp, etc.).
    ///
    /// Returns `true` if the tracee was already resumed (via
    /// `ptrace::syscall`) and the caller should NOT call `ptrace::cont`.
    fn handle_ptrace_event(&mut self, pid: Pid, evt: i32) -> Result<bool> {
        let fork = ptrace::Event::PTRACE_EVENT_FORK as i32;
        let vfork = ptrace::Event::PTRACE_EVENT_VFORK as i32;
        let clone = ptrace::Event::PTRACE_EVENT_CLONE as i32;
        let exec = ptrace::Event::PTRACE_EVENT_EXEC as i32;
        let exit = ptrace::Event::PTRACE_EVENT_EXIT as i32;
        let seccomp = ptrace::Event::PTRACE_EVENT_SECCOMP as i32;

        if evt == fork || evt == vfork || evt == clone {
            process_events::handle_fork(self, pid)?;
        } else if evt == exec {
            process_events::handle_program_replace(self, pid)?;
        } else if evt == exit {
            process_events::handle_exit_event(self, pid)?;
        } else if evt == seccomp {
            match handlers::handle_seccomp_stop(self, pid) {
                Ok(true) => return Ok(true),
                Ok(false) => {}
                Err(e) => {
                    event!(
                        name: "tracer.seccomp.error",
                        Level::WARN,
                        pid = pid.as_raw(),
                        error.message = %e,
                        "seccomp handler error for pid {{pid}}: {{error.message}}",
                    );
                }
            }
        }
        Ok(false)
    }

    /// Completes a pending content capture at syscall exit.
    ///
    /// Uses the per-path `path_hashes` cache for before_hash and
    /// updates it with the new after_hash, ensuring an unbroken chain.
    fn handle_syscall_exit(&mut self, pid: Pid) -> Result<()> {
        let pid_u32 = pid.as_raw() as u32;

        if let Some(cap) = self.pending_captures.remove(&pid_u32) {
            let after_hash = hash_file_content(&self.cas, &cap.path);

            // Use cached hash as before_hash; fall back to the one
            // computed at entry (for the first event on this path).
            let before_hash = self
                .path_hashes
                .get(&cap.path)
                .cloned()
                .or(cap.before_hash);

            // Update cache so the next event's before_hash chains.
            if let Some(ref h) = after_hash {
                self.path_hashes.insert(cap.path.clone(), h.clone());
            }

            match cap.kind {
                CaptureKind::Write { fd, size } => {
                    self.emit(EventPayload::Write(ef::Write {
                        pid: cap.pid,
                        path: cap.path,
                        fd,
                        offset: 0,
                        size,
                        before_hash,
                        after_hash,
                        tree_hash: None,
                    }));
                }
                CaptureKind::OpenTrunc => {
                    if before_hash != after_hash {
                        self.emit(EventPayload::Write(ef::Write {
                            pid: cap.pid,
                            path: cap.path,
                            fd: -1,
                            offset: 0,
                            size: 0,
                            before_hash,
                            after_hash,
                            tree_hash: None,
                        }));
                    }
                }
            }
        }

        ptrace::cont(pid, None)?;
        Ok(())
    }

    /// Forwards non-ptrace signals to the tracee.
    ///
    /// If the pid has a pending capture, resumes with `ptrace::syscall`
    /// to preserve syscall-exit tracking across signal delivery.
    fn handle_signal_stop(&mut self, pid: Pid, sig: Signal) -> Result<()> {
        let forward = match sig {
            Signal::SIGSTOP | Signal::SIGTRAP => None,
            other => Some(other),
        };

        let pid_u32 = pid.as_raw() as u32;
        if self.pending_captures.contains_key(&pid_u32) {
            ptrace::syscall(pid, forward)?;
        } else {
            ptrace::cont(pid, forward)?;
        }
        Ok(())
    }

    /// Registers the initial process in the tree.
    fn register_initial_process(&mut self, pid: Pid) -> Result<()> {
        let pid_u32 = pid.as_raw() as u32;
        let binary = memory::read_proc_exe(pid)
            .unwrap_or_else(|_| PathBuf::from("unknown"));
        let argv = memory::read_proc_cmdline(pid).unwrap_or_default();
        let cwd = std::fs::read_link(format!("/proc/{}/cwd", pid.as_raw()))
            .unwrap_or_else(|_| PathBuf::from("/"));

        let fds = FdTable::new();
        self.process_tree
            .add_process(pid_u32, 0, binary, argv, cwd, fds);

        Ok(())
    }

    /// Emits an event through the channel.
    pub fn emit(&self, payload: EventPayload) {
        let evt = Event::new(&self.seq_gen, self.agent_id.clone(), payload);
        if let Err(e) = self.event_tx.send(evt) {
            event!(
                name: "tracer.event.send_error",
                Level::ERROR,
                error.message = %e,
                "failed to send event: {{error.message}}",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    fn test_cas() -> Arc<CasStore> {
        let dir = tempfile::tempdir().expect("tempdir");
        Arc::new(CasStore::new(dir.path().join("cas")).expect("CasStore"))
    }

    #[test]
    fn tracer_loop_new_initializes_empty_state() {
        let (tx, _rx) = mpsc::channel();
        let seq = Arc::new(SequenceGenerator::default());
        let tracer = TracerLoop::new("test-agent".into(), tx, seq, test_cas());
        assert!(tracer.process_tree.is_empty());
        assert!(tracer.pipe_registry.is_empty());
        assert!(tracer.pty_registry.is_empty());
        assert!(tracer.write_locks.is_empty());
        assert!(tracer.pending_captures.is_empty());
        assert_eq!(tracer.alive_count, 0);
    }

    #[test]
    fn emit_sends_event_with_correct_agent_id() {
        let (tx, rx) = mpsc::channel();
        let seq = Arc::new(SequenceGenerator::default());
        let tracer = TracerLoop::new("agent-42".into(), tx, seq, test_cas());
        tracer.emit(EventPayload::Fork(crate::events::process::Fork {
            parent_pid: 1,
            child_pid: 2,
        }));
        let evt = rx.recv().unwrap();
        assert_eq!(evt.agent_id, "agent-42");
        assert_eq!(evt.seq, 0);
    }

    #[test]
    fn emit_increments_sequence() {
        let (tx, rx) = mpsc::channel();
        let seq = Arc::new(SequenceGenerator::default());
        let tracer = TracerLoop::new("a".into(), tx, seq, test_cas());
        tracer.emit(EventPayload::Exit(crate::events::process::Exit {
            pid: 1,
            exit_code: 0,
            signal: None,
        }));
        tracer.emit(EventPayload::Exit(crate::events::process::Exit {
            pid: 2,
            exit_code: 0,
            signal: None,
        }));
        let e1 = rx.recv().unwrap();
        let e2 = rx.recv().unwrap();
        assert_eq!(e1.seq, 0);
        assert_eq!(e2.seq, 1);
    }
}
