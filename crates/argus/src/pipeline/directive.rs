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
    /// Resume the tracee via `ptrace::syscall`.
    Resume { pid: Pid },

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
            Self::Resume { pid } => write!(f, "Resume({pid})"),
            Self::ReadMemory { pid, addr, len, .. } => {
                write!(f, "ReadMemory({pid}, {addr:#x}, {len})")
            }
            Self::ReadString { pid, addr, max_len, .. } => {
                write!(f, "ReadString({pid}, {addr:#x}, {max_len})")
            }
            Self::ReadFile { path, .. } => write!(f, "ReadFile({path:?})"),
            Self::InjectError { pid, errno } => write!(f, "InjectError({pid}, {errno})"),
            Self::ResolveFd { pid, fd, .. } => write!(f, "ResolveFd({pid}, {fd})"),
            Self::WriteMemory { pid, addr, .. } => {
                write!(f, "WriteMemory({pid}, {addr:#x})")
            }
        }
    }
}
