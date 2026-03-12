# P1: Ptrace Loop & Syscall Handlers

**Status**: not started

**Spec reference**: `docs/spec/01-supervisor.md` (ptrace loop, syscall handling, auto-follow)

## Dependencies
- **Blocked by**: P1-events, P1-state, P1-seccomp
- **Blocks**: P1-supervisor-main, P2-content-capture, P2-write-locking, P2-pause-resume-api

## Critical path — depends on events, state, and seccomp. NOT parallelizable with those three.

## What needs to be done
- `crates/sandbox/src/tracer/mod.rs` and submodules:

### Ptrace Loop (`tracer/loop.rs`)
- `TracerLoop` struct: holds `ProcessTree`, `PipeRegistry`, `PtyRegistry`, event sender (channel), config
- `run(agent_pid: Pid) -> Result<()>`: main loop
  - `waitpid(-1, WALL)` — wait for any traced child
  - Match on wait status:
    - `PTRACE_EVENT_FORK/VFORK/CLONE`: auto-add child to process tree, set ptrace options on child
    - `PTRACE_EVENT_EXEC`: update process state (binary, args from /proc)
    - `PTRACE_EVENT_EXIT`: mark process dead, emit Exit event
    - `PTRACE_EVENT_SECCOMP`: dispatch to syscall handler
  - Ptrace options on every new process: `PTRACE_O_TRACESYSGOOD | TRACEFORK | TRACEVFORK | TRACECLONE | TRACEEXEC | TRACEEXIT | TRACESECCOMP`
  - Continue with `PTRACE_CONT` after handling

### Syscall Handlers (`tracer/handlers.rs`)
- Read syscall number and args from registers (`PTRACE_GETREGS` / `PTRACE_GETREGSET`)
- Dispatch by syscall number to handler functions:
  - **open/openat**: read path from tracee memory (process_vm_readv), record fd→path mapping
  - **read/write**: classify fd target (file/stdio/pipe/pty/socket), emit appropriate event. For Phase 1: emit event with size but no content hash (content capture is Phase 2)
  - **close**: remove fd from table
  - **dup/dup2/dup3**: clone fd entry
  - **pipe/pipe2**: create pipe registry entry, record both fds
  - **fork/vfork/clone**: handled by PTRACE_EVENT, but also update fd table (clone fds)
  - **execve**: read new binary path from /proc/{pid}/exe, emit Exec event
  - **rename/unlink/mkdir/etc**: read paths from tracee, emit corresponding event
  - **socket/connect/accept**: update fd table with socket info, emit events
  - **ioctl**: check for PTY operations (TIOCGPTN, TIOCSPTLCK)
- Pause-before-action check: call stub hook (always returns Allow for Phase 1)
- Helper: `read_string_from_tracee(pid, addr) -> Result<String>` using process_vm_readv

### Tracee Memory Access (`tracer/memory.rs`)
- `read_bytes(pid, addr, len) -> Result<Vec<u8>>`: process_vm_readv wrapper
- `read_c_string(pid, addr) -> Result<String>`: read until null terminator
- `read_string_array(pid, addr) -> Result<Vec<String>>`: for argv/envp

## How to test
```bash
cargo test -p sandbox --lib tracer -- --ignored
```
Integration tests (require Linux + SYS_PTRACE):
1. Trace `echo hello` — verify Exec and Exit events emitted
2. Trace `cat /etc/hostname` — verify open, read, close events with correct paths
3. Trace `bash -c "echo x | cat"` — verify pipe creation and data events
4. Trace a process that forks — verify child auto-traced
5. Verify stdio classification (fd 0/1/2 → StdioData events)

## Branch
- **Branch**: `p1-tracer-loop`
- **Target**: `main`
