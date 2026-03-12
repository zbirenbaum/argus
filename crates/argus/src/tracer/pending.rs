//! Unified pending syscall state for the entry/exit capture pattern.
//!
//! Every mutating or fd-producing syscall stores its entry-time args
//! here. At syscall exit, the return value is checked: negative means
//! the kernel rejected it, so the entry is dropped with no event.

/// What kind of file mutation triggered a write capture.
#[derive(Debug)]
pub enum CaptureKind {
    /// A write/pwrite/writev/pwritev syscall.
    Write { fd: i32, size: u64 },
    /// An open with O_TRUNC that truncates existing content.
    OpenTrunc { flags: i32 },
}

/// Captured arguments from syscall entry, awaiting exit confirmation.
#[derive(Debug)]
pub enum PendingSyscall {
    /// Syscall was cancelled; inject -EPERM at exit.
    Eperm,

    /// open/openat/openat2/creat — need the returned fd.
    Open { path: String, flags: i32 },

    /// read/pread64 — need to read buffer content from tracee.
    Read {
        pid: u32,
        fd: i32,
        path: String,
        buf_addr: u64,
        count: u64,
    },

    /// pipe/pipe2 — need to read the fd pair from tracee memory.
    Pipe { pid: u32, pipefd_addr: u64 },

    /// write/pwrite64 to a file — need after_hash for hash chain.
    WriteCapture {
        before_hash: Option<String>,
        path: String,
        pid: u32,
        kind: CaptureKind,
    },

    /// rename/renameat/renameat2.
    Rename {
        pid: u32,
        old_path: String,
        new_path: String,
    },

    /// unlink/unlinkat.
    Unlink { pid: u32, path: String },

    /// mkdir/mkdirat.
    Mkdir { pid: u32, path: String },

    /// rmdir.
    Rmdir { pid: u32, path: String },

    /// chmod/fchmod/fchmodat.
    Chmod {
        pid: u32,
        path: String,
        new_mode: u32,
    },

    /// truncate/ftruncate.
    Truncate {
        pid: u32,
        path: String,
        new_size: u64,
    },

    /// link/linkat.
    Link {
        pid: u32,
        target: String,
        link_path: String,
    },

    /// symlink/symlinkat.
    Symlink {
        pid: u32,
        target: String,
        link_path: String,
    },
}
