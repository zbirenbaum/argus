# P1: State Module (FD Table, Pipe/PTY Registry, Process Tree)

**Status**: not started

**Spec reference**: `docs/spec/01-supervisor.md` (fd table, pipe registry, PTY registry)

## Dependencies
- **Blocked by**: nothing
- **Blocks**: P1-tracer-loop

## Parallelizable with
- P1-config, P1-events, P1-seccomp, P1-net-env, P2-cas, P2-s3-upload, P2-digest-cache, P2-event-segments, P3-indexes

## What needs to be done
- `crates/sandbox/src/state/mod.rs` — in-memory state tracking:

### FD Table
- `FdTarget` enum: `File { path, flags }`, `Pipe { pipe_id, end: Read|Write }`, `Pty { pty_id, end: Master|Slave }`, `Socket { domain, sock_type, addr: Option }`, `DevNull`, `Other`
- `FdTable`: per-process `HashMap<RawFd, FdTarget>`
- Operations: insert, remove, clone_for_fork (dup all entries), update on dup/dup2/dup3

### Pipe Registry
- `PipeEntry`: pipe_id (u64), readers (HashSet<(pid, fd)>), writers (HashSet<(pid, fd)>)
- `PipeRegistry`: `HashMap<u64, PipeEntry>` keyed by inode
- Track pipe2() creation, register read/write ends, remove on close

### PTY Registry
- `PtyEntry`: pty_id (u64), master_pid, master_fd, slave_pid, slave_fd, slave_path
- `PtyRegistry`: `HashMap<u64, PtyEntry>`
- Track openpty/posix_openpt, pts path resolution, close

### Process Tree
- `ProcessState`: pid, ppid, binary (PathBuf), args (Vec<String>), cwd (PathBuf), fds (FdTable), alive (bool)
- `ProcessTree`: `HashMap<Pid, ProcessState>`
- Operations: add on fork/clone, update on exec, mark dead on exit, get_children

### Write Locks
- `WriteLockManager`: `HashMap<PathBuf, Mutex<()>>`
- Per-path mutex; stub implementation for Phase 1 (acquire/release without hashing)

## How to test
```bash
cargo test -p sandbox --lib state
```
Unit tests: fd table clone on fork, pipe registry tracks endpoints correctly, PTY pairing, process tree parent-child relationships, write lock mutual exclusion.

## Branch
- **Branch**: `p1-state`
- **Target**: `main`
