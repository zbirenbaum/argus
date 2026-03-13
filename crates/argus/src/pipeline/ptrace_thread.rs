// Rust guideline compliant 2026-02-21
//! Ptrace thread and async handle for the pipeline architecture.
//!
//! The ptrace thread runs a blocking `waitpid` loop on a dedicated OS
//! thread and communicates with async pipeline stages through unbounded
//! channels. Stages send directives (memory reads, resumes) and receive
//! `RawSyscallStop` events in return.

use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};

use anyhow::Result;
use futures::Stream;
use nix::sys::ptrace;
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;
use tokio::sync::{mpsc, oneshot};
use tracing::event;
use tracing::Level;

use crate::tracer::memory::{read_bytes, read_c_string, write_bytes};
use crate::tracer::regs::{get_regs, set_regs, set_ret};

use super::directive::PipelineDirective;
use super::raw_stop::{RawSyscallStop, StopType, SyscallArgs};

/// Ptrace options applied to every seized process.
///
/// Mirrors the existing `PTRACE_OPTS` constant in `trace_loop.rs` plus
/// `TRACESECCOMP` for the pipeline's seccomp-driven entry stops.
const PTRACE_OPTS: ptrace::Options = ptrace::Options::from_bits_truncate(
    ptrace::Options::PTRACE_O_TRACEFORK.bits()
        | ptrace::Options::PTRACE_O_TRACEVFORK.bits()
        | ptrace::Options::PTRACE_O_TRACECLONE.bits()
        | ptrace::Options::PTRACE_O_TRACEEXEC.bits()
        | ptrace::Options::PTRACE_O_TRACEEXIT.bits()
        | ptrace::Options::PTRACE_O_TRACESECCOMP.bits()
        | ptrace::Options::PTRACE_O_TRACESYSGOOD.bits(),
);

/// Convert a nix `WaitStatus` into a `RawSyscallStop`.
fn translate_wait_status(status: WaitStatus) -> RawSyscallStop {
    match status {
        WaitStatus::PtraceSyscall(pid) => {
            // Syscall-exit stop (SIGTRAP|0x80). We don't have registers
            // here — the classify stage re-reads them via directive.
            RawSyscallStop {
                pid,
                stop_type: StopType::SyscallExit {
                    syscall_nr: 0,
                    return_value: 0,
                },
            }
        }
        WaitStatus::PtraceEvent(pid, _sig, evt) => {
            let fork_evt = ptrace::Event::PTRACE_EVENT_FORK as i32;
            let vfork_evt = ptrace::Event::PTRACE_EVENT_VFORK as i32;
            let clone_evt = ptrace::Event::PTRACE_EVENT_CLONE as i32;
            let exec_evt = ptrace::Event::PTRACE_EVENT_EXEC as i32;
            let exit_evt = ptrace::Event::PTRACE_EVENT_EXIT as i32;
            let seccomp_evt = ptrace::Event::PTRACE_EVENT_SECCOMP as i32;

            if evt == fork_evt || evt == vfork_evt || evt == clone_evt {
                // Child pid is embedded in the event message.
                let child_raw = ptrace::getevent(pid).unwrap_or(0) as i32;
                let child = Pid::from_raw(child_raw);
                RawSyscallStop {
                    pid,
                    stop_type: StopType::Fork { parent: pid, child },
                }
            } else if evt == exec_evt {
                RawSyscallStop {
                    pid,
                    stop_type: StopType::Exec { pid },
                }
            } else if evt == exit_evt {
                let code = ptrace::getevent(pid).unwrap_or(0) as i32;
                RawSyscallStop {
                    pid,
                    stop_type: StopType::Exit { pid, exit_code: code },
                }
            } else if evt == seccomp_evt {
                // Seccomp entry stop — read syscall args from registers.
                // The regs module names args 1-based (arg1 = first arg = x0/rdi).
                // We map them to 0-based SyscallArgs fields.
                let (nr, args) = {
                    use crate::tracer::regs::{arg1, arg2, arg3, arg4, arg5, syscall_nr};
                    let r = get_regs(pid).unwrap_or_default();
                    (syscall_nr(&r), SyscallArgs::from_array([
                        arg1(&r), arg2(&r), arg3(&r), arg4(&r), arg5(&r), 0,
                    ]))
                };
                RawSyscallStop {
                    pid,
                    stop_type: StopType::SyscallEntry { syscall_nr: nr, args },
                }
            } else {
                RawSyscallStop { pid, stop_type: StopType::Unknown }
            }
        }
        WaitStatus::Exited(pid, code) => RawSyscallStop {
            pid,
            stop_type: StopType::Exit { pid, exit_code: code },
        },
        WaitStatus::Signaled(pid, sig, _) => RawSyscallStop {
            pid,
            stop_type: StopType::Signal { pid, signal: sig as i32 },
        },
        WaitStatus::Stopped(pid, sig) => RawSyscallStop {
            pid,
            stop_type: StopType::Signal { pid, signal: sig as i32 },
        },
        _ => {
            // StillAlive and other non-actionable statuses.
            RawSyscallStop {
                pid: Pid::from_raw(0),
                stop_type: StopType::Unknown,
            }
        }
    }
}

/// Execute one `PipelineDirective` on behalf of the pipeline.
///
/// Called synchronously in the ptrace thread after each stop is delivered.
/// Returns `true` if the tracee was resumed by this function.
fn execute_directive(directive: PipelineDirective) -> bool {
    match directive {
        PipelineDirective::Resume { pid } => {
            let _ = ptrace::syscall(pid, None);
            true
        }
        PipelineDirective::ReadMemory { pid, addr, len, reply } => {
            let _ = reply.send(read_bytes(pid, addr as u64, len));
            false
        }
        PipelineDirective::ReadString { pid, addr, max_len, reply } => {
            let result = read_c_string(pid, addr as u64).map(|mut s| {
                s.truncate(max_len);
                s
            });
            let _ = reply.send(result);
            false
        }
        PipelineDirective::ReadFile { path, reply } => {
            let _ = reply.send(std::fs::read(&path).map_err(anyhow::Error::from));
            false
        }
        PipelineDirective::InjectError { pid, errno } => {
            if let Ok(mut r) = get_regs(pid) {
                set_ret(&mut r, (-(errno as i64)) as u64);
                let _ = set_regs(pid, &r);
            }
            let _ = ptrace::syscall(pid, None);
            true
        }
        PipelineDirective::ResolveFd { pid, fd, reply } => {
            let link = format!("/proc/{}/fd/{}", pid.as_raw(), fd);
            let _ = reply.send(std::fs::read_link(&link).map_err(anyhow::Error::from));
            false
        }
        PipelineDirective::WriteMemory { pid, addr, data, reply } => {
            let _ = reply.send(write_bytes(pid, addr as u64, &data));
            false
        }
    }
}

/// Entry point for the dedicated ptrace thread.
fn ptrace_thread_main(
    initial_pid: Pid,
    stop_tx: mpsc::UnboundedSender<RawSyscallStop>,
    mut directive_rx: mpsc::UnboundedReceiver<PipelineDirective>,
) {
    if let Err(e) = ptrace::seize(initial_pid, PTRACE_OPTS) {
        event!(
            name: "ptrace_thread.seize_failed",
            Level::ERROR,
            pid = initial_pid.as_raw(),
            error.message = %e,
            "ptrace seize of pid {{pid}} failed: {{error.message}}",
        );
        return;
    }

    loop {
        let status = match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::__WALL)) {
            Ok(s) => s,
            Err(nix::errno::Errno::ECHILD) => break,
            Err(e) => {
                event!(
                    name: "ptrace_thread.waitpid_error",
                    Level::ERROR,
                    error.message = %e,
                    "waitpid failed: {{error.message}}",
                );
                break;
            }
        };

        let stop = translate_wait_status(status);
        if stop_tx.send(stop).is_err() {
            // Receiver dropped — pipeline shutting down.
            break;
        }

        // Block until the pipeline sends back a directive.
        match directive_rx.blocking_recv() {
            Some(directive) => {
                execute_directive(directive);
            }
            None => break,
        }
    }
}

/// Cloneable handle for sending directives from async pipeline stages.
///
/// Holds only the sending side of the directive channel so stages can
/// request memory reads and resumes without owning the stream.
#[derive(Clone, Debug)]
pub struct PtraceHandle {
    tx: mpsc::UnboundedSender<PipelineDirective>,
}

impl PtraceHandle {
    /// Construct a handle directly from a sender — test helper only.
    #[cfg(test)]
    pub fn from_sender(tx: mpsc::UnboundedSender<PipelineDirective>) -> Self {
        Self { tx }
    }

    /// Send a raw directive without awaiting a reply.
    pub fn directive(&self, d: PipelineDirective) {
        let _ = self.tx.send(d);
    }

    /// Read `len` bytes from `addr` in the tracee's address space.
    ///
    /// # Errors
    ///
    /// Returns an error if the ptrace thread fails to read the memory.
    pub async fn read_memory(&self, pid: Pid, addr: usize, len: usize) -> Result<Vec<u8>> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.directive(PipelineDirective::ReadMemory { pid, addr, len, reply: reply_tx });
        reply_rx.await?
    }

    /// Read a null-terminated string from `addr` in the tracee.
    ///
    /// # Errors
    ///
    /// Returns an error if the read fails.
    pub async fn read_string(&self, pid: Pid, addr: usize, max_len: usize) -> Result<String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.directive(PipelineDirective::ReadString { pid, addr, max_len, reply: reply_tx });
        reply_rx.await?
    }

    /// Read a file through the supervisor's filesystem namespace.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read.
    pub async fn read_file(&self, path: PathBuf) -> Result<Vec<u8>> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.directive(PipelineDirective::ReadFile { path, reply: reply_tx });
        reply_rx.await?
    }

    /// Resolve an open file descriptor to its filesystem path.
    ///
    /// # Errors
    ///
    /// Returns an error if the fd cannot be resolved.
    pub async fn resolve_fd(&self, pid: Pid, fd: i32) -> Result<PathBuf> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.directive(PipelineDirective::ResolveFd { pid, fd, reply: reply_tx });
        reply_rx.await?
    }

    /// Write `data` into the tracee's address space at `addr`.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    pub async fn write_memory(&self, pid: Pid, addr: usize, data: Vec<u8>) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.directive(PipelineDirective::WriteMemory { pid, addr, data, reply: reply_tx });
        reply_rx.await?
    }
}

/// Async stream of `RawSyscallStop` events from the ptrace thread.
///
/// Constructed via [`PtraceStream::spawn`]. Each item yielded represents
/// one ptrace stop; the caller must send a directive back through the
/// attached [`PtraceHandle`] to unblock the ptrace thread.
pub struct PtraceStream {
    stop_rx: mpsc::UnboundedReceiver<RawSyscallStop>,
    directive_tx: mpsc::UnboundedSender<PipelineDirective>,
}

impl PtraceStream {
    /// Construct a stream from raw channel halves — test helper only.
    ///
    /// Allows mock implementations to inject stops without spawning a real
    /// ptrace thread.
    #[cfg(test)]
    pub fn from_channels(
        stop_rx: mpsc::UnboundedReceiver<RawSyscallStop>,
        directive_tx: mpsc::UnboundedSender<PipelineDirective>,
    ) -> Self {
        Self { stop_rx, directive_tx }
    }

    /// Spawn the ptrace thread for `child_pid` and return the stream.
    pub fn spawn(child_pid: Pid) -> (Self, std::thread::JoinHandle<()>) {
        let (stop_tx, stop_rx) = mpsc::unbounded_channel();
        let (directive_tx, directive_rx) = mpsc::unbounded_channel();

        let handle = std::thread::Builder::new()
            .name("ptrace-loop".into())
            .spawn(move || ptrace_thread_main(child_pid, stop_tx, directive_rx))
            .expect("failed to spawn ptrace thread");

        let stream = Self { stop_rx, directive_tx };
        (stream, handle)
    }

    /// Send a directive to the ptrace thread without awaiting a reply.
    pub fn directive(&self, d: PipelineDirective) {
        let _ = self.directive_tx.send(d);
    }

    /// Clone the directive sender for use by pipeline stages.
    pub fn handle(&self) -> PtraceHandle {
        PtraceHandle { tx: self.directive_tx.clone() }
    }
}

impl Stream for PtraceStream {
    type Item = RawSyscallStop;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        self.stop_rx.poll_recv(cx)
    }
}
