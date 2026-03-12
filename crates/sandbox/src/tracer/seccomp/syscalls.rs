// Rust guideline compliant 2026-02-21
//! x86_64 syscall number tables for seccomp filtering.
//!
//! All numbers are from the Linux x86_64 ABI. Grouped by category for
//! readability but flattened into a single static slice for the BPF builder.

// --- File content ---
const SYS_OPEN: u64 = 2;
const SYS_OPENAT: u64 = 257;
const SYS_OPENAT2: u64 = 437;
const SYS_CREAT: u64 = 85;
const SYS_READ: u64 = 0;
const SYS_PREAD64: u64 = 17;
const SYS_READV: u64 = 19;
const SYS_PREADV: u64 = 295;
const SYS_WRITE: u64 = 1;
const SYS_PWRITE64: u64 = 18;
const SYS_WRITEV: u64 = 20;
const SYS_PWRITEV: u64 = 296;

// --- File metadata ---
const SYS_RENAME: u64 = 82;
const SYS_RENAMEAT: u64 = 264;
const SYS_RENAMEAT2: u64 = 316;
const SYS_UNLINK: u64 = 87;
const SYS_UNLINKAT: u64 = 263;
const SYS_MKDIR: u64 = 83;
const SYS_MKDIRAT: u64 = 258;
const SYS_RMDIR: u64 = 84;
const SYS_CHMOD: u64 = 90;
const SYS_FCHMOD: u64 = 91;
const SYS_FCHMODAT: u64 = 268;
const SYS_TRUNCATE: u64 = 76;
const SYS_FTRUNCATE: u64 = 77;
const SYS_LINK: u64 = 86;
const SYS_LINKAT: u64 = 265;
const SYS_SYMLINK: u64 = 88;
const SYS_SYMLINKAT: u64 = 266;
const SYS_READLINK: u64 = 89;
const SYS_READLINKAT: u64 = 267;

// --- FD management ---
const SYS_CLOSE: u64 = 3;
const SYS_DUP: u64 = 32;
const SYS_DUP2: u64 = 33;
const SYS_DUP3: u64 = 292;
const SYS_PIPE: u64 = 22;
const SYS_PIPE2: u64 = 293;
const SYS_FCNTL: u64 = 72;

// --- Process ---
const SYS_FORK: u64 = 57;
const SYS_VFORK: u64 = 58;
const SYS_CLONE: u64 = 56;
const SYS_CLONE3: u64 = 435;
const SYS_EXECVE: u64 = 59;
const SYS_EXECVEAT: u64 = 322;
const SYS_EXIT: u64 = 60;
const SYS_EXIT_GROUP: u64 = 231;

// --- PTY ---
const SYS_IOCTL: u64 = 16;

// --- Network ---
const SYS_SOCKET: u64 = 41;
const SYS_CONNECT: u64 = 42;
const SYS_ACCEPT: u64 = 43;
const SYS_ACCEPT4: u64 = 288;
const SYS_BIND: u64 = 49;
const SYS_LISTEN: u64 = 50;
const SYS_SENDTO: u64 = 44;
const SYS_SENDMSG: u64 = 46;
const SYS_RECVFROM: u64 = 45;
const SYS_RECVMSG: u64 = 47;

// --- Blocked (io_uring) ---
const SYS_IO_URING_SETUP: u64 = 425;
const SYS_IO_URING_ENTER: u64 = 426;
const SYS_IO_URING_REGISTER: u64 = 427;

/// Syscalls that generate `SECCOMP_RET_TRACE` ptrace stops.
pub static TRACED_SYSCALLS: &[u64] = &[
    // File content
    SYS_OPEN, SYS_OPENAT, SYS_OPENAT2, SYS_CREAT,
    SYS_READ, SYS_PREAD64, SYS_READV, SYS_PREADV,
    SYS_WRITE, SYS_PWRITE64, SYS_WRITEV, SYS_PWRITEV,
    // File metadata
    SYS_RENAME, SYS_RENAMEAT, SYS_RENAMEAT2,
    SYS_UNLINK, SYS_UNLINKAT,
    SYS_MKDIR, SYS_MKDIRAT, SYS_RMDIR,
    SYS_CHMOD, SYS_FCHMOD, SYS_FCHMODAT,
    SYS_TRUNCATE, SYS_FTRUNCATE,
    SYS_LINK, SYS_LINKAT,
    SYS_SYMLINK, SYS_SYMLINKAT,
    SYS_READLINK, SYS_READLINKAT,
    // FD management
    SYS_CLOSE, SYS_DUP, SYS_DUP2, SYS_DUP3,
    SYS_PIPE, SYS_PIPE2, SYS_FCNTL,
    // Process
    SYS_FORK, SYS_VFORK, SYS_CLONE, SYS_CLONE3,
    SYS_EXECVE, SYS_EXECVEAT,
    SYS_EXIT, SYS_EXIT_GROUP,
    // PTY
    SYS_IOCTL,
    // Network
    SYS_SOCKET, SYS_CONNECT, SYS_ACCEPT, SYS_ACCEPT4,
    SYS_BIND, SYS_LISTEN,
    SYS_SENDTO, SYS_SENDMSG, SYS_RECVFROM, SYS_RECVMSG,
];

/// Syscalls blocked with `SECCOMP_RET_ERRNO(ENOSYS)`.
///
/// io_uring bypasses ptrace entirely, so we block it to maintain
/// complete syscall visibility.
pub static BLOCKED_SYSCALLS: &[u64] = &[
    SYS_IO_URING_SETUP,
    SYS_IO_URING_ENTER,
    SYS_IO_URING_REGISTER,
];
