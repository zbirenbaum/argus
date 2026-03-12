// Rust guideline compliant 2026-02-21
//! Syscall number constants for handler dispatch.
//!
//! On aarch64, legacy syscalls (open, mkdir, pipe, fork, etc.) do not
//! exist in the kernel. We define them as `u64::MAX` so match arms
//! compile but never fire — the `*at` variants handle everything.

#[cfg(target_arch = "x86_64")]
mod nums {
    pub const SYS_READ: u64 = 0;
    pub const SYS_WRITE: u64 = 1;
    pub const SYS_OPEN: u64 = 2;
    pub const SYS_CLOSE: u64 = 3;
    pub const SYS_LSEEK: u64 = 8;
    pub const SYS_IOCTL: u64 = 16;
    pub const SYS_PREAD64: u64 = 17;
    pub const SYS_PWRITE64: u64 = 18;
    pub const SYS_READV: u64 = 19;
    pub const SYS_WRITEV: u64 = 20;
    pub const SYS_PIPE: u64 = 22;
    pub const SYS_DUP: u64 = 32;
    pub const SYS_DUP2: u64 = 33;
    pub const SYS_SOCKET: u64 = 41;
    pub const SYS_CONNECT: u64 = 42;
    pub const SYS_ACCEPT: u64 = 43;
    pub const SYS_SENDTO: u64 = 44;
    pub const SYS_RECVFROM: u64 = 45;
    pub const SYS_SENDMSG: u64 = 46;
    pub const SYS_RECVMSG: u64 = 47;
    pub const SYS_BIND: u64 = 49;
    pub const SYS_LISTEN: u64 = 50;
    pub const SYS_CLONE: u64 = 56;
    pub const SYS_FORK: u64 = 57;
    pub const SYS_VFORK: u64 = 58;
    pub const SYS_EXECVE: u64 = 59;
    pub const SYS_EXIT: u64 = 60;
    pub const SYS_FCNTL: u64 = 72;
    pub const SYS_TRUNCATE: u64 = 76;
    pub const SYS_FTRUNCATE: u64 = 77;
    pub const SYS_RENAME: u64 = 82;
    pub const SYS_MKDIR: u64 = 83;
    pub const SYS_RMDIR: u64 = 84;
    pub const SYS_CREAT: u64 = 85;
    pub const SYS_LINK: u64 = 86;
    pub const SYS_UNLINK: u64 = 87;
    pub const SYS_SYMLINK: u64 = 88;
    pub const SYS_READLINK: u64 = 89;
    pub const SYS_CHMOD: u64 = 90;
    pub const SYS_FCHMOD: u64 = 91;
    pub const SYS_CHOWN: u64 = 92;
    pub const SYS_FCHOWN: u64 = 93;
    pub const SYS_EXIT_GROUP: u64 = 231;
    pub const SYS_OPENAT: u64 = 257;
    pub const SYS_MKDIRAT: u64 = 258;
    pub const SYS_FCHOWNAT: u64 = 260;
    pub const SYS_UNLINKAT: u64 = 263;
    pub const SYS_RENAMEAT: u64 = 264;
    pub const SYS_LINKAT: u64 = 265;
    pub const SYS_SYMLINKAT: u64 = 266;
    pub const SYS_READLINKAT: u64 = 267;
    pub const SYS_FCHMODAT: u64 = 268;
    pub const SYS_ACCEPT4: u64 = 288;
    pub const SYS_DUP3: u64 = 292;
    pub const SYS_PIPE2: u64 = 293;
    pub const SYS_PREADV: u64 = 295;
    pub const SYS_PWRITEV: u64 = 296;
    pub const SYS_RENAMEAT2: u64 = 316;
    pub const SYS_EXECVEAT: u64 = 322;
    pub const SYS_CLONE3: u64 = 435;
    pub const SYS_OPENAT2: u64 = 437;
}

#[cfg(target_arch = "aarch64")]
mod nums {
    // Sentinel for legacy syscalls that don't exist on aarch64.
    const ABSENT: u64 = u64::MAX;

    pub const SYS_DUP: u64 = 23;
    pub const SYS_DUP3: u64 = 24;
    pub const SYS_FCNTL: u64 = 25;
    pub const SYS_IOCTL: u64 = 29;
    pub const SYS_MKDIRAT: u64 = 34;
    pub const SYS_UNLINKAT: u64 = 35;
    pub const SYS_SYMLINKAT: u64 = 36;
    pub const SYS_LINKAT: u64 = 37;
    pub const SYS_RENAMEAT: u64 = 38;
    pub const SYS_TRUNCATE: u64 = 45;
    pub const SYS_FTRUNCATE: u64 = 46;
    pub const SYS_FCHMOD: u64 = 52;
    pub const SYS_FCHMODAT: u64 = 53;
    pub const SYS_FCHOWNAT: u64 = 54;
    pub const SYS_FCHOWN: u64 = 55;
    pub const SYS_OPENAT: u64 = 56;
    pub const SYS_CLOSE: u64 = 57;
    pub const SYS_PIPE2: u64 = 59;
    pub const SYS_LSEEK: u64 = 62;
    pub const SYS_READ: u64 = 63;
    pub const SYS_WRITE: u64 = 64;
    pub const SYS_READV: u64 = 65;
    pub const SYS_WRITEV: u64 = 66;
    pub const SYS_PREAD64: u64 = 67;
    pub const SYS_PWRITE64: u64 = 68;
    pub const SYS_PREADV: u64 = 69;
    pub const SYS_PWRITEV: u64 = 70;
    pub const SYS_READLINKAT: u64 = 78;
    pub const SYS_EXIT: u64 = 93;
    pub const SYS_EXIT_GROUP: u64 = 94;
    pub const SYS_SOCKET: u64 = 198;
    pub const SYS_BIND: u64 = 200;
    pub const SYS_LISTEN: u64 = 201;
    pub const SYS_ACCEPT: u64 = 202;
    pub const SYS_CONNECT: u64 = 203;
    pub const SYS_SENDTO: u64 = 206;
    pub const SYS_RECVFROM: u64 = 207;
    pub const SYS_SENDMSG: u64 = 211;
    pub const SYS_RECVMSG: u64 = 212;
    pub const SYS_CLONE: u64 = 220;
    pub const SYS_EXECVE: u64 = 221;
    pub const SYS_ACCEPT4: u64 = 242;
    pub const SYS_RENAMEAT2: u64 = 276;
    pub const SYS_EXECVEAT: u64 = 281;
    pub const SYS_CLONE3: u64 = 435;
    pub const SYS_OPENAT2: u64 = 437;

    // Legacy syscalls absent on aarch64 — sentinel value, never matched.
    pub const SYS_OPEN: u64 = ABSENT;
    pub const SYS_CREAT: u64 = ABSENT - 1;
    pub const SYS_PIPE: u64 = ABSENT - 2;
    pub const SYS_DUP2: u64 = ABSENT - 3;
    pub const SYS_FORK: u64 = ABSENT - 4;
    pub const SYS_VFORK: u64 = ABSENT - 5;
    pub const SYS_RENAME: u64 = ABSENT - 6;
    pub const SYS_MKDIR: u64 = ABSENT - 7;
    pub const SYS_RMDIR: u64 = ABSENT - 8;
    pub const SYS_UNLINK: u64 = ABSENT - 9;
    pub const SYS_SYMLINK: u64 = ABSENT - 10;
    pub const SYS_READLINK: u64 = ABSENT - 11;
    pub const SYS_LINK: u64 = ABSENT - 12;
    pub const SYS_CHMOD: u64 = ABSENT - 13;
    pub const SYS_CHOWN: u64 = ABSENT - 14;
}

pub use nums::*;
