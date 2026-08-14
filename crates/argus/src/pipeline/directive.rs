// Rust guideline compliant 2026-02-21
//! Commands sent from pipeline stages back to the ptrace thread.
//!
//! Stages are async; the ptrace thread is synchronous. Directives cross
//! that boundary. Reply channels use `oneshot` so the async caller can
//! await the result without blocking the ptrace thread more than needed.

use std::path::PathBuf;

use anyhow::Result;
use nix::unistd::Pid;
use tokio::sync::oneshot;

/// An instruction from a pipeline stage to the ptrace thread.
pub enum PipelineDirective {
    /// Resume the tracee.
    ///
    /// When `trace_exit` is true the tracee is resumed with
    /// `ptrace::syscall` so the next syscall-exit stop is delivered.
    /// When false, `ptrace::cont` is used and only SECCOMP / ptrace
    /// event stops fire — avoiding per-syscall overhead on threads
    /// that do not have a pending entry awaiting exit correlation.
    ///
    /// `signal` re-injects a pending signal into the tracee. Must be
    /// set when resuming from a signal-delivery stop so the tracee
    /// actually receives the signal (e.g. SIGCHLD).
    Resume {
        pid: Pid,
        trace_exit: bool,
        signal: Option<nix::sys::signal::Signal>,
    },

    /// Read raw bytes from tracee memory.
    ReadMemory {
        pid: Pid,
        addr: usize,
        len: usize,
        reply: oneshot::Sender<Result<Vec<u8>>>,
    },

    /// Read a null-terminated C string from tracee memory.
    ReadString {
        pid: Pid,
        addr: usize,
        max_len: usize,
        reply: oneshot::Sender<Result<String>>,
    },

    /// Read a file from the supervisor's own filesystem namespace.
    ReadFile {
        path: PathBuf,
        reply: oneshot::Sender<Result<Vec<u8>>>,
    },

    /// Inject an errno return value and resume.
    InjectError { pid: Pid, errno: i32 },

    /// Stop every live tracee and report those confirmed stopped.
    ///
    /// Used by `POST /agent/pause` and by the policy gate while a
    /// verdict is outstanding. Does not resume the in-flight tracee —
    /// the reply arrives while it is still held at its syscall stop.
    Freeze { reply: oneshot::Sender<Vec<Pid>> },

    /// Resolve a file descriptor number to its filesystem path.
    ResolveFd {
        pid: Pid,
        fd: i32,
        reply: oneshot::Sender<Result<PathBuf>>,
    },

    /// Write bytes into tracee memory (used for connect() rewrite).
    WriteMemory {
        pid: Pid,
        addr: usize,
        data: Vec<u8>,
        reply: oneshot::Sender<Result<()>>,
    },
}

impl std::fmt::Debug for PipelineDirective {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Resume { pid, trace_exit, signal } => {
                write!(f, "Resume({pid}, trace_exit={trace_exit}, signal={signal:?})")
            }
            Self::ReadMemory { pid, addr, len, .. } => {
                write!(f, "ReadMemory({pid}, {addr:#x}, {len})")
            }
            Self::ReadString { pid, addr, max_len, .. } => {
                write!(f, "ReadString({pid}, {addr:#x}, {max_len})")
            }
            Self::ReadFile { path, .. } => write!(f, "ReadFile({path:?})"),
            Self::InjectError { pid, errno } => write!(f, "InjectError({pid}, {errno})"),
            Self::Freeze { .. } => write!(f, "Freeze"),
            Self::ResolveFd { pid, fd, .. } => write!(f, "ResolveFd({pid}, {fd})"),
            Self::WriteMemory { pid, addr, .. } => {
                write!(f, "WriteMemory({pid}, {addr:#x})")
            }
        }
    }
}
