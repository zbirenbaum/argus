//! In-memory state tracking for the ptrace supervisor.
//!
//! Maintains file descriptor tables, pipe and PTY registries, the process
//! tree, and per-path write locks. Updated synchronously from the ptrace
//! loop on every intercepted syscall.

mod fd_serde;
pub(crate) mod fd_table;
pub(crate) mod pipe_registry;
pub(crate) mod process_tree;
pub(crate) mod pty_registry;
pub(crate) use fd_table::FdTable;
pub(crate) use pipe_registry::PipeRegistry;
pub(crate) use process_tree::ProcessTree;
pub(crate) use pty_registry::PtyRegistry;
