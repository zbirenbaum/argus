# P1: Seccomp BPF Filter

**Status**: done

**Spec reference**: `docs/spec/01-supervisor.md` (seccomp-bpf section)

## Dependencies
- **Blocked by**: nothing
- **Blocks**: P1-tracer-loop

## Parallelizable with
- P1-config, P1-events, P1-state, P1-net-env, P2-cas, P2-s3-upload, P2-digest-cache, P2-event-segments, P3-indexes

## What was done
- `crates/sandbox/src/tracer/seccomp/mod.rs` — public API: `install_seccomp_filter()`, `trapped_syscalls()`, `is_trapped()`
- `crates/sandbox/src/tracer/seccomp/syscalls.rs` — x86_64 syscall number constants and static lists (57 traced, 3 blocked)
- `crates/sandbox/src/tracer/seccomp/bpf.rs` — raw BPF program builder (`SyscallAction` enum, `build_filter_program()`)
- `crates/sandbox/src/tracer/mod.rs` — added `pub mod seccomp;`

## What works
- BPF program validates x86_64 arch, kills on mismatch
- 57 syscalls trapped with `SECCOMP_RET_TRACE` (file content, metadata, FD, process, PTY, network)
- 3 io_uring syscalls blocked with `SECCOMP_RET_ERRNO(ENOSYS)`
- All other syscalls allowed at native speed
- `install_seccomp_filter()` sets `PR_SET_NO_NEW_PRIVS` then loads filter via `prctl(PR_SET_SECCOMP)`
- Integration test verifies filter installs and child exits cleanly under ptrace

## Deviations from spec
- Spec says "~55 syscalls" but the enumerated list contains 57 traced syscalls. All 57 from the spec are included.

## What's missing
- Nothing

## How to test
```bash
# Unit tests (no special privileges needed)
cargo test -p sandbox --lib tracer::seccomp

# Integration test (requires Linux with SYS_PTRACE, seccomp=unconfined)
cargo test -p sandbox --lib tracer::seccomp -- --ignored
```

## Branch
- **Branch**: `p1-seccomp`
- **Target**: `main`
