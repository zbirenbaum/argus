# P1: Seccomp BPF Filter

**Status**: not started

**Spec reference**: `docs/spec/01-supervisor.md` (seccomp-bpf section)

## Dependencies
- **Blocked by**: nothing
- **Blocks**: P1-tracer-loop

## Parallelizable with
- P1-config, P1-events, P1-state, P1-net-env, P2-cas, P2-s3-upload, P2-digest-cache, P2-event-segments, P3-indexes

## What needs to be done
- `crates/sandbox/src/tracer/seccomp.rs` (submodule of tracer):
  - Build BPF program that returns `SECCOMP_RET_TRACE` for ~55 syscalls:
    - **File**: open, openat, openat2, creat, read, pread64, readv, preadv, write, pwrite64, writev, pwritev, rename, renameat, renameat2, unlink, unlinkat, mkdir, mkdirat, rmdir, chmod, fchmod, fchmodat, truncate, ftruncate, link, linkat, symlink, symlinkat, readlink, readlinkat
    - **FD**: close, dup, dup2, dup3, pipe, pipe2, fcntl
    - **Process**: fork, vfork, clone, clone3, execve, execveat, exit, exit_group
    - **PTY**: ioctl (for TIOCGPTN, TIOCSPTLCK)
    - **Network**: socket, connect, accept, accept4, bind, listen, sendto, sendmsg, recvfrom, recvmsg
    - **Blocked**: io_uring_setup (return ENOSYS)
  - All other syscalls: `SECCOMP_RET_ALLOW` (native speed)
  - Function: `install_seccomp_filter() -> Result<()>` — called in child after fork, before exec
  - Use raw BPF (libc::sock_filter / sock_fprog) or the `seccompiler` crate

## How to test
```bash
cargo test -p sandbox --lib tracer::seccomp -- --ignored
```
Integration test (requires Linux + SYS_PTRACE): install filter, exec a simple program, verify ptrace stops on trapped syscalls only.

## Branch
- **Branch**: `p1-seccomp`
- **Target**: `main`
