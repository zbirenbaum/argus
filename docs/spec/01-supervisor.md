# ptrace Supervisor

Runs as PID 1 in the container. Traces all descendant processes via ptrace with fork/clone/exec following.

## seccomp-bpf Filtering

Install SECCOMP_RET_TRACE filter that only traps target syscalls (~55). Everything else at native speed. Block `io_uring_setup` (syscall 425) to prevent bypass.

## Intercepted Syscalls

**File content:** open, openat, close, read, pread64, write, pwrite64, readv, writev, lseek

**File metadata:** rename, renameat, renameat2, unlink, unlinkat, mkdir, mkdirat, rmdir, chmod, fchmod, fchmodat, chown, fchown, fchownat, link, linkat, symlink, symlinkat, truncate, ftruncate

**Process:** execve, execveat, clone, fork, vfork

**Fd management:** dup, dup2, dup3, fcntl (F_DUPFD*), pipe, pipe2

**PTY:** ioctl (TIOCSPTLCK, TIOCGPTN), openat on /dev/ptmx and /dev/pts/*

**Network:** socket, connect, accept, accept4, sendto, recvfrom, sendmsg, recvmsg

**Blocked:** io_uring_setup, io_uring_enter, io_uring_register

**Pass-through:** Everything else via SECCOMP_RET_ALLOW.

## Argument Extraction

- PTRACE_GET_SYSCALL_INFO (Linux 5.3+) for syscall number, args, return value
- process_vm_readv for bulk buffer reads from tracee memory
- Resolve paths via /proc/\<pid\>/cwd and readlink(/proc/\<pid\>/fd/\<fd\>)

## Startup Sequence

Steps that can't be retrofitted are marked.

```
1.  Parse config (supervisor.yaml)
2.  Initialize local storage dirs (CAS, events, indexes)
3.  Load digest cache from disk; if missing, download S3 snapshot + incremental LIST
4.  Generate TLS CA keypair if not on data volume           ← CAN'T RETROFIT
5.  Start mitmdump child (traced, on 127.0.0.1:8080)       ← CAN'T RETROFIT
6.  Build agent environment:                                ← CAN'T RETROFIT
      SSLKEYLOGFILE=/data/tls/keylog.txt
      HTTPS_PROXY=http://127.0.0.1:8080
      HTTP_PROXY=http://127.0.0.1:8080
      SSL_CERT_FILE=/etc/ssl/certs/argus-ca.pem
      NODE_EXTRA_CA_CERTS=/etc/ssl/certs/argus-ca.pem
      REQUESTS_CA_BUNDLE=/etc/ssl/certs/argus-ca.pem
7.  Snapshot initial filesystem state:                      ← CAN'T RETROFIT
      Walk watched paths → hash every file into CAS → build Merkle tree
      This is commit zero. Emit initial_state event.
      S3 uploads enqueued (agent doesn't wait for S3, only for hashing)
8.  fork()
9.  Child: PTRACE_TRACEME → exec(agent with env from step 6)
10. Parent: enter ptrace loop
11. Emit agent_start event (agent_id, config, node, pod)
```

Steps 4-6 set environment the agent inherits on exec. Step 7 must complete before step 9 so commit zero captures pre-agent state.

## Per-Process State

### Fd Table

```rust
struct ProcessState {
    pid: u32, ppid: u32, binary: PathBuf, argv: Vec<String>,
    fds: HashMap<i32, FdTarget>,
}

enum FdTarget {
    File { path: PathBuf },
    Pipe { inode: u64, direction: PipeEnd },
    Socket { domain: i32, addr: Option<SocketAddr> },
    Pty { role: PtyRole, peer_path: PathBuf },
    DevNull, Unknown,
}
```

- Initialized from /proc/\<pid\>/fd/* on creation
- Updated on open/close/dup/dup2/dup3/fcntl/pipe/socket
- Copied on fork/clone, preserved on exec (minus FD_CLOEXEC)

### Pipe Registry

```rust
struct PipeRegistry { pipes: HashMap<u64, PipeInfo> }
struct PipeInfo { writers: Vec<(u32, i32)>, readers: Vec<(u32, i32)>, created_by: u32 }
```

Updated on pipe (create), fork (inherit), dup (alias), close (remove), exec (drop FD_CLOEXEC).

### PTY Registry

```rust
struct PtyRegistry { ptys: HashMap<i32, PtyInfo> }
struct PtyInfo { master_pid: u32, master_fd: i32, slave_path: PathBuf, slave_pid: Option<u32>, slave_fd: Option<i32> }
```

Updated on openat(/dev/ptmx), ioctl(TIOCGPTN), openat(/dev/pts/N), fork.

## Write Classification

**On write(fd, buf, count):**
1. Look up fd in process fd table
2. File → `write` event with before_hash/after_hash (see write locking below)
3. Pipe/Pty with fd=1 → `stdio` subtype `stdout`
4. Pipe/Pty with fd=2 → `stdio` subtype `stderr`
5. Pipe (other fd) → `pipe_data`
6. Socket → network event
7. DevNull → log byte count only

**On read(fd, buf, count) at syscall exit:**
1. File → `read` event with content_hash
2. Pipe with fd=0 → `stdio` subtype `stdin`
3. Pipe (other fd) → `pipe_data`
4. Socket → network event

## Per-Path Write Locking

Ensures correct before/after state when multiple processes target the same file. Core to capture correctness.

```rust
write_locks: HashMap<PathBuf, Mutex>
```

**On syscall entry for mutating ops (write, rename, unlink, truncate):**
1. Resolve target path
2. Acquire write_locks[path]
3. Read file content → before_hash (file is stable — competing tracees also stopped)
4. Resume tracee (PTRACE_SYSCALL)
5. Wait for syscall exit
6. Read file content → after_hash
7. Emit event with before_hash, after_hash, tree_hash
8. Release lock

Competing writes to same path: second tracee is stopped by ptrace, supervisor delays resuming until lock is free. Process sees a longer pause — no errors. Different paths: zero contention.

## Main Loop

```
loop {
    status = waitpid(-1, __WALL)

    PTRACE_EVENT_FORK/VFORK/CLONE:
        auto-trace child, copy parent fd table, update pipe/pty registries

    PTRACE_EVENT_EXEC:
        log exec event, drop FD_CLOEXEC fds

    SYSCALL_STOP (entry):
        extract syscall info via PTRACE_GET_SYSCALL_INFO
        resolve path/fd arguments

        >>> CHECK PAUSE-BEFORE-ACTION RULES <<<     // see 06-agent-controls.md
        if rule matches → emit pending_approval, wait for API decision
        if denied → inject EPERM, skip to resume

        if mutating file op → acquire write lock, capture before_hash

        PTRACE_SYSCALL to continue into kernel

    SYSCALL_STOP (exit):
        read return value

        write to file → capture after_hash, update Merkle tree, emit event, release lock
        write to pipe/pty/socket → read buffer via process_vm_readv, classify, emit
        read (any fd) → read buffer, classify, emit
        open/close/dup/pipe/socket → update fd table + registries, emit

        PTRACE_SYSCALL to resume

    WIFEXITED/WIFSIGNALED:
        emit exit event, clean up state
        if mmap tracked → capture final state of mapped files
```

## Known Gaps

**io_uring:** Blocked via seccomp. Transparent fallback. Low risk.

**mmap:** MAP_SHARED+PROT_WRITE on file-backed mappings — stores invisible to ptrace. Log mmap_warning event, capture at munmap/exit. Low risk for agent workloads.

## Tooling

**Language:** Rust. **Crates:** nix (ptrace), seccomp (BPF), aws-sdk-s3 + tokio (async uploads).
**Key APIs:** PTRACE_SEIZE, PTRACE_GET_SYSCALL_INFO, process_vm_readv.
