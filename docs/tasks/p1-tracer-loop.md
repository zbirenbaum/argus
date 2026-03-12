# P1: Ptrace Loop & Syscall Handlers

**Status**: done

**Spec reference**: `docs/spec/01-supervisor.md` (ptrace loop, syscall handling, auto-follow)

## Dependencies
- **Blocked by**: P1-events, P1-state, P1-seccomp
- **Blocks**: P1-supervisor-main, P2-content-capture, P2-write-locking, P2-pause-resume-api

## What was done

### Files added
- `crates/sandbox/src/tracer/trace_loop.rs` -- `TracerLoop` struct with main ptrace wait loop
- `crates/sandbox/src/tracer/process_events.rs` -- fork, program-replace, exit event handlers
- `crates/sandbox/src/tracer/handlers/mod.rs` -- seccomp stop dispatch by syscall number
- `crates/sandbox/src/tracer/handlers/file_ops.rs` -- open/close/dup/fcntl handlers
- `crates/sandbox/src/tracer/handlers/metadata_ops.rs` -- rename/unlink/mkdir/chmod/truncate/link/symlink handlers
- `crates/sandbox/src/tracer/handlers/io_ops.rs` -- read/write/pipe/ioctl handlers with fd target classification
- `crates/sandbox/src/tracer/handlers/net_ops.rs` -- socket/connect/accept handlers
- `crates/sandbox/src/tracer/memory.rs` -- `read_c_string`, `read_bytes`, `read_path_at`, `read_proc_exe`, `read_proc_cmdline`
- `crates/sandbox/src/tracer/regs.rs` -- architecture-abstracted register access (x86_64 + aarch64)
- `crates/sandbox/src/tracer/syscall_nr.rs` -- x86_64 syscall number constants

### Files modified
- `crates/sandbox/src/tracer/mod.rs` -- added module declarations and re-exports

## What works
- `TracerLoop::new()` / `TracerLoop::run()` -- full ptrace loop with `waitpid(-1, __WALL)`
- Auto-follow fork/vfork/clone via `PTRACE_O_TRACEFORK|TRACEVFORK|TRACECLONE`
- Program replacement detection via `PTRACE_EVENT_EXEC`
- Exit handling via `PTRACE_EVENT_EXIT` and `WaitStatus::Exited/Signaled`
- Seccomp stop dispatch for all 57 traced syscalls
- Fd target classification (file/pipe/pty/socket/devnull) via fd table + procfs fallback
- Stdio detection (fd 1 -> stdout, fd 2 -> stderr)
- Pause-before-action stub (`check_pause_rules` always returns `Allow`)
- Tracee memory access via `process_vm_readv`
- `AT_FDCWD` and dirfd resolution for `*at()` syscalls
- Structured logging with `tracing` crate using OpenTelemetry conventions
- 146 unit tests pass (3 new tests for TracerLoop)

## What's missing / Phase 1 limitations
- **No post-syscall handling**: seccomp stops on entry, so return values (new fd from open/dup/socket/accept/pipe) are not captured. Fd table not populated by open. Read/write handlers use procfs fallback.
- **No content capture**: all hashes are `None` (deferred to P2-content-capture)
- **No envp capture**: exec events have empty envp
- **No pipe fd capture**: pipe/pipe2 fds not read (need post-syscall)
- **chmod old_mode**: always 0 (need pre-call stat)
- **truncate old_size**: always 0 (need pre-call stat)
- **accept peer address**: always "unknown" (need post-call read)

## How to test
```bash
# Unit tests (no ptrace required)
cargo test -p sandbox --lib tracer

# Integration tests (require Linux + SYS_PTRACE capability)
cargo test -p sandbox --lib tracer -- --ignored
```

## Branch
- **Branch**: `p1-tracer-loop`
- **Target**: `main`
