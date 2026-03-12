//! In-memory state tracking for the ptrace supervisor.
//!
//! Maintains file descriptor tables, pipe and PTY registries, the process
//! tree, and per-path write locks. Updated synchronously from the ptrace
//! loop on every intercepted syscall.

mod fd_serde;
pub mod fd_table;
pub mod pipe_registry;
pub mod process_tree;
pub mod pty_registry;
pub mod write_capture;
pub mod write_locks;

#[doc(inline)]
pub use fd_table::{FdTable, FdTarget, PipeEnd, PtyRole};
#[doc(inline)]
pub use pipe_registry::{PipeInfo, PipeRegistry};
#[doc(inline)]
pub use process_tree::{ProcessState, ProcessTree};
#[doc(inline)]
pub use pty_registry::{PtyInfo, PtyRegistry};
#[doc(inline)]
pub use write_locks::WriteLocks;
