//! Main ptrace event loop.
//!
//! Sits on a dedicated thread, calling `waitpid(-1)` in a loop and
//! dispatching to handlers based on the wait status. Automatically
//! follows forks, program replacements, and exits. Emits structured
//! events over a channel for downstream consumers.

use std::path::PathBuf;
use std::sync::mpsc::Sender;

use anyhow::{Context, Result};
use nix::sys::ptrace;
use nix::sys::signal::Signal;
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;
use tracing::event;
use tracing::Level;

use crate::events::{Event, EventPayload, SequenceGenerator};
use crate::state::{FdTable, PipeRegistry, ProcessTree, PtyRegistry, WriteLocks};
use crate::tracer::{handlers, memory, process_events};

/// Ptrace options to set on every traced process.
pub const PTRACE_OPTS: ptrace::Options = ptrace::Options::from_bits_truncate(
    ptrace::Options::PTRACE_O_TRACEFORK.bits()
        | ptrace::Options::PTRACE_O_TRACEVFORK.bits()
        | ptrace::Options::PTRACE_O_TRACECLONE.bits()
        | ptrace::Options::PTRACE_O_TRACEEXEC.bits()
        | ptrace::Options::PTRACE_O_TRACEEXIT.bits()
        | ptrace::Options::PTRACE_O_TRACESECCOMP.bits(),
);

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
    event_tx: Sender<Event>,
    seq_gen: SequenceGenerator,
    agent_id: String,
    pub alive_count: u32,
}

impl TracerLoop {
    /// Creates a new tracer loop.
    pub fn new(agent_id: String, event_tx: Sender<Event>) -> Self {
        Self {
            process_tree: ProcessTree::new(),
            pipe_registry: PipeRegistry::new(),
            pty_registry: PtyRegistry::new(),
            write_locks: WriteLocks::new(),
            event_tx,
            seq_gen: SequenceGenerator::default(),
            agent_id,
            alive_count: 0,
        }
    }

    /// Runs the main ptrace loop until all traced processes exit.
    ///
    /// Attaches to `initial_pid` via `PTRACE_SEIZE`, sets ptrace
    /// options, and enters the wait loop.
    ///
    /// # Errors
    ///
    /// Returns an error if ptrace operations fail or the wait loop
    /// encounters an unrecoverable error.
    pub fn run(&mut self, initial_pid: Pid) -> Result<()> {
        ptrace::seize(initial_pid, PTRACE_OPTS)
            .with_context(|| format!("ptrace seize pid {initial_pid}"))?;

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

            let status = match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::__WALL)) {
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
                // Seccomp stops are continued inside handle_seccomp_stop
                // after handler processing. Other ptrace events (fork,
                // exec, exit) are continued here. Signal stops forward the
                // signal to the tracee via handle_signal_stop.
                self.handle_ptrace_event(pid, evt)?;
                ptrace::cont(pid, None)?;
            }
            WaitStatus::Stopped(pid, sig) => {
                self.handle_signal_stop(pid, sig)?;
            }
            WaitStatus::Exited(pid, code) => {
                process_events::handle_process_exit(self, pid, code, None);
            }
            WaitStatus::Signaled(pid, sig, _core) => {
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
    fn handle_ptrace_event(&mut self, pid: Pid, evt: i32) -> Result<()> {
        match evt {
            libc::PTRACE_EVENT_FORK
            | libc::PTRACE_EVENT_VFORK
            | libc::PTRACE_EVENT_CLONE => {
                process_events::handle_fork(self, pid)?;
            }
            libc::PTRACE_EVENT_EXEC => {
                process_events::handle_program_replace(self, pid)?;
            }
            libc::PTRACE_EVENT_EXIT => {
                process_events::handle_exit_event(self, pid)?;
            }
            libc::PTRACE_EVENT_SECCOMP => {
                if let Err(e) = handlers::handle_seccomp_stop(self, pid) {
                    event!(
                        name: "tracer.seccomp.error",
                        Level::WARN,
                        pid = pid.as_raw(),
                        error.message = %e,
                        "seccomp handler error for pid {{pid}}: {{error.message}}",
                    );
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Forwards non-ptrace signals to the tracee.
    fn handle_signal_stop(&mut self, pid: Pid, sig: Signal) -> Result<()> {
        let forward = match sig {
            Signal::SIGSTOP | Signal::SIGTRAP => None,
            other => Some(other),
        };
        ptrace::cont(pid, forward)?;
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

    #[test]
    fn tracer_loop_new_initializes_empty_state() {
        let (tx, _rx) = mpsc::channel();
        let tracer = TracerLoop::new("test-agent".into(), tx);
        assert!(tracer.process_tree.is_empty());
        assert!(tracer.pipe_registry.is_empty());
        assert!(tracer.pty_registry.is_empty());
        assert!(tracer.write_locks.is_empty());
        assert_eq!(tracer.alive_count, 0);
    }

    #[test]
    fn emit_sends_event_with_correct_agent_id() {
        let (tx, rx) = mpsc::channel();
        let tracer = TracerLoop::new("agent-42".into(), tx);
        tracer.emit(EventPayload::Fork(ep::Fork {
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
        let tracer = TracerLoop::new("a".into(), tx);
        tracer.emit(EventPayload::Exit(ep::Exit {
            pid: 1,
            exit_code: 0,
            signal: None,
        }));
        tracer.emit(EventPayload::Exit(ep::Exit {
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
