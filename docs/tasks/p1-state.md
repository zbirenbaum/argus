# P1: State Module (FD Table, Pipe/PTY Registry, Process Tree)

**Status**: done

**Spec reference**: `docs/spec/01-supervisor.md` (fd table, pipe registry, PTY registry)

## Dependencies
- **Blocked by**: nothing
- **Blocks**: P1-tracer-loop

## Parallelizable with
- P1-config, P1-events, P1-seccomp, P1-net-env, P2-cas, P2-s3-upload, P2-digest-cache, P2-event-segments, P3-indexes

## What was done
- `crates/argus/src/state/mod.rs` -- module root with re-exports
- `crates/argus/src/state/fd_table.rs` -- `FdTarget`, `PipeEnd`, `PtyRole`, `FdTable` with insert/remove/get/dup/clone_for_fork/close_cloexec/cloexec flag management
- `crates/argus/src/state/pipe_registry.rs` -- `PipeInfo`, `PipeRegistry` with create_pipe/on_fork/on_close/on_dup
- `crates/argus/src/state/pty_registry.rs` -- `PtyInfo`, `PtyRegistry` with register_master/register_slave/find_by_slave_path
- `crates/argus/src/state/process_tree.rs` -- `ProcessState`, `ProcessTree` with add_process/update_on_program_replace/mark_exited/get_children/alive_pids
- `crates/argus/src/state/write_locks.rs` -- `WriteLocks` with get_or_create per-path mutex management

## What works
- FD table: insert, remove, get, clone_for_fork, dup/dup2/dup3, cloexec flag management, close_cloexec on exec
- Pipe registry: create pipe, fork inheritance, close endpoint, dup alias, cleanup when all endpoints closed
- PTY registry: master/slave registration, lookup by path, removal
- Process tree: fork (add child), exec (update binary + drop cloexec), exit (mark dead), children query
- Write locks: per-path mutex creation, concurrent blocking on same path, independent paths don't block

## What's missing
- Write lock hashing integration (Phase 2+ per spec)
- No integration with tracer module yet (will be wired in P1 trace loop task)

## How to test
```bash
docker run --rm -v "$(pwd):/workspace" -w /workspace argus-dev cargo test -p argus --lib
docker run --rm -v "$(pwd):/workspace" -w /workspace argus-dev cargo clippy -p argus --lib
```

35 tests, 0 clippy warnings.

## Branch
- **Branch**: `p1-state`
- **Target**: `main`
