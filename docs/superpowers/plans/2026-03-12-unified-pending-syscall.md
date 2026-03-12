# Unified PendingSyscall Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the five separate pending maps with a single `PendingSyscall` enum and one exit handler, adding return-value validation to every mutating syscall so failed operations never emit events or corrupt the Merkle tree.

**Architecture:** Every mutating syscall follows the same entry/exit shape: at seccomp entry, capture args and store a `PendingSyscall` variant, then resume with `ptrace::syscall`. At syscall exit, check the return value — if negative (error), drop the entry silently; if success, emit the event and update state. One `HashMap<u32, PendingSyscall>` replaces `pending_eperm`, `pending_opens`, `pending_reads`, `pending_pipes`, and `pending_captures`.

**Tech Stack:** Rust, ptrace, seccomp-bpf. Must build with `--target aarch64-unknown-linux-musl`.

**Build command:** `cargo build --target aarch64-unknown-linux-musl` (inside docker container `argus-arm64`)

**Test commands:**
- Unit: `cargo test --target aarch64-unknown-linux-musl -p argus`
- Integration: `timeout 120 bash tests/validate.sh 1 2 3 4 5 6 7 8 9 10 11 12`

---

## Current State

Five separate pending maps in `TracerLoop` (`trace_loop.rs:141-148`):

| Map | Type | Purpose |
|-|-|
| `pending_eperm` | `HashSet<u32>` | EPERM injection at exit |
| `pending_opens` | `HashMap<u32, PendingOpen>` | fd table insertion after open |
| `pending_reads` | `HashMap<u32, PendingRead>` | Buffer content capture after read |
| `pending_pipes` | `HashMap<u32, PendingPipe>` | fd pair capture after pipe |
| `pending_captures` | `HashMap<u32, PendingCapture>` | Before/after hash for writes |

**Bug:** Metadata handlers (`metadata_ops.rs`) emit events at entry without checking return value. A failed `unlink`, `rename`, `mkdir`, etc. still emits an event and updates the Merkle tree.

## File Structure

| File | Action | Responsibility |
|-|-|-|
| `crates/argus/src/tracer/pending.rs` | Create | `PendingSyscall` enum + `PendingArgs` structs |
| `crates/argus/src/tracer/trace_loop.rs` | Modify | Replace 5 maps with `pending: HashMap<u32, PendingSyscall>`, rewrite `handle_syscall_exit` |
| `crates/argus/src/tracer/handlers/mod.rs` | Modify | Update dispatch to use entry/exit for metadata ops |
| `crates/argus/src/tracer/handlers/metadata_ops.rs` | Modify | Convert all handlers to entry-only (return args, no events) |
| `crates/argus/src/tracer/handlers/file_ops.rs` | Modify | Use `PendingSyscall::Open` instead of `PendingOpen` |
| `crates/argus/src/tracer/handlers/io_ops.rs` | Modify | Use `PendingSyscall::Read/Pipe` instead of separate structs |
| `crates/argus/src/tracer/mod.rs` | Modify | Add `pub mod pending;` |

---

## Chunk 1: Define PendingSyscall enum

### Task 1: Create the PendingSyscall enum

**Files:**
- Create: `crates/argus/src/tracer/pending.rs`
- Modify: `crates/argus/src/tracer/mod.rs`

The enum must cover every syscall that currently uses a pending map, plus every metadata op that currently fires at entry. Each variant stores only the args captured at entry — no event payloads.

- [ ] **Step 1: Write the enum definition**

```rust
// crates/argus/src/tracer/pending.rs

//! Unified pending syscall state for the entry/exit capture pattern.
//!
//! Every mutating or fd-producing syscall stores its entry-time args
//! here. At syscall exit, the return value is checked: negative means
//! the kernel rejected it, so the entry is dropped with no event.

/// Captured arguments from syscall entry, awaiting exit confirmation.
#[derive(Debug)]
pub enum PendingSyscall {
    /// Syscall was cancelled; inject -EPERM at exit.
    Eperm,

    /// open/openat/openat2/creat — need the returned fd.
    Open {
        path: String,
        flags: i32,
    },

    /// read/pread64 — need to read buffer content from tracee.
    Read {
        pid: u32,
        fd: i32,
        path: String,
        buf_addr: u64,
        count: u64,
    },

    /// pipe/pipe2 — need to read the fd pair from tracee memory.
    Pipe {
        pid: u32,
        pipefd_addr: u64,
    },

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
    Unlink {
        pid: u32,
        path: String,
    },

    /// mkdir/mkdirat.
    Mkdir {
        pid: u32,
        path: String,
    },

    /// rmdir.
    Rmdir {
        pid: u32,
        path: String,
    },

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

/// What kind of file mutation triggered a write capture.
#[derive(Debug)]
pub enum CaptureKind {
    /// A write/pwrite/writev/pwritev syscall.
    Write { fd: i32, size: u64 },
    /// An open with O_TRUNC that truncates existing content.
    OpenTrunc,
}
```

- [ ] **Step 2: Register the module**

Add `pub mod pending;` to `crates/argus/src/tracer/mod.rs`.

- [ ] **Step 3: Verify it compiles**

Run: `cargo build --target aarch64-unknown-linux-musl`

- [ ] **Step 4: Commit**

```
add PendingSyscall enum for unified entry/exit pattern
```

---

## Chunk 2: Replace the five maps with one

### Task 2: Swap TracerLoop fields

**Files:**
- Modify: `crates/argus/src/tracer/trace_loop.rs`

Replace:
```rust
pub pending_eperm: HashSet<u32>,
pub pending_opens: HashMap<u32, PendingOpen>,
pub pending_reads: HashMap<u32, PendingRead>,
pub pending_pipes: HashMap<u32, PendingPipe>,
pub pending_captures: HashMap<u32, PendingCapture>,
```

With:
```rust
pub pending: HashMap<u32, PendingSyscall>,
```

Also:
- Remove the old `PendingOpen`, `PendingRead`, `PendingPipe`, `PendingCapture`, `CaptureKind` struct/enum definitions from `trace_loop.rs` (they now live in `pending.rs`).
- Update `TracerLoop::new()` to initialize one map.
- Update `handle_signal_stop` to check `self.pending.contains_key(&pid_u32)`.
- Update `Exited`/`Signaled` handlers to remove from `self.pending`.

- [ ] **Step 1: Remove old structs, add `pending` field**
- [ ] **Step 2: Update `new()`, `handle_signal_stop`, exit cleanup**
- [ ] **Step 3: Verify it compiles (it won't yet — callers still reference old maps)**

---

### Task 3: Rewrite handle_syscall_exit as a single dispatcher

**Files:**
- Modify: `crates/argus/src/tracer/trace_loop.rs`

The new `handle_syscall_exit` removes the entry from `self.pending` and matches on the variant:

```rust
fn handle_syscall_exit(&mut self, pid: Pid) -> Result<()> {
    let pid_u32 = pid.as_raw() as u32;

    let Some(pending) = self.pending.remove(&pid_u32) else {
        // No pending entry — stale syscall exit, just resume.
        ptrace::cont(pid, None)?;
        return Ok(());
    };

    match pending {
        PendingSyscall::Eperm => {
            inject_eperm(pid)?;
            ptrace::cont(pid, None)?;
        }
        PendingSyscall::Open { path, flags } => {
            self.complete_open(pid, &path, flags)?;
            ptrace::cont(pid, None)?;
        }
        PendingSyscall::Read { pid: p, fd, path, buf_addr, count } => {
            self.complete_read(pid, p, fd, &path, buf_addr, count)?;
            ptrace::cont(pid, None)?;
        }
        PendingSyscall::Pipe { pid: p, pipefd_addr } => {
            self.complete_pipe(pid, p, pipefd_addr)?;
            ptrace::cont(pid, None)?;
        }
        PendingSyscall::WriteCapture { before_hash, path, pid: p, kind } => {
            self.complete_write_capture(pid, p, path, before_hash, kind)?;
            // ptrace::cont called inside (handles write queue)
        }
        PendingSyscall::Rename { pid: p, old_path, new_path } => {
            self.complete_rename(pid, p, &old_path, &new_path)?;
            ptrace::cont(pid, None)?;
        }
        PendingSyscall::Unlink { pid: p, path } => {
            self.complete_unlink(pid, p, &path)?;
            ptrace::cont(pid, None)?;
        }
        PendingSyscall::Mkdir { pid: p, path } => {
            self.complete_mkdir(pid, p, &path)?;
            ptrace::cont(pid, None)?;
        }
        PendingSyscall::Rmdir { pid: p, path } => {
            self.complete_rmdir(pid, p, &path)?;
            ptrace::cont(pid, None)?;
        }
        PendingSyscall::Chmod { pid: p, path, new_mode } => {
            self.complete_chmod(pid, p, &path, new_mode)?;
            ptrace::cont(pid, None)?;
        }
        PendingSyscall::Truncate { pid: p, path, new_size } => {
            self.complete_truncate(pid, p, &path, new_size)?;
            ptrace::cont(pid, None)?;
        }
        PendingSyscall::Link { pid: p, target, link_path } => {
            self.complete_link(pid, p, &target, &link_path)?;
            ptrace::cont(pid, None)?;
        }
        PendingSyscall::Symlink { pid: p, target, link_path } => {
            self.complete_symlink(pid, p, &target, &link_path)?;
            ptrace::cont(pid, None)?;
        }
    }
    Ok(())
}
```

Every `complete_*` method follows the same pattern:
1. `let r = regs::get_regs(pid)?;`
2. `let ret = regs::ret_val(&r) as i64;`
3. `if ret < 0 { return Ok(()); }` — failed syscall, no event
4. Emit event + update tree

- [ ] **Step 1: Implement `complete_rename`, `complete_unlink`, `complete_mkdir`, `complete_rmdir`, `complete_chmod`, `complete_truncate`, `complete_link`, `complete_symlink`**

Each is simple — check return value, emit event if success. Example for rename:

```rust
fn complete_rename(&mut self, pid: Pid, pid_u32: u32, old_path: &str, new_path: &str) -> Result<()> {
    let r = regs::get_regs(pid)?;
    let ret = regs::ret_val(&r) as i64;
    if ret < 0 { return Ok(()); }

    let tree_hash = self.tree_rename(old_path, new_path);
    self.emit(EventPayload::Rename(ef::Rename {
        pid: pid_u32,
        old_path: old_path.to_owned(),
        new_path: new_path.to_owned(),
        tree_hash,
    }));
    Ok(())
}
```

- [ ] **Step 2: Refactor existing `complete_pending_open`, `complete_pending_read`, `complete_pending_pipe` to take inline args instead of struct references**
- [ ] **Step 3: Refactor write capture completion to use `PendingSyscall::WriteCapture` fields**
- [ ] **Step 4: Delete old `PendingOpen`, `PendingRead`, `PendingPipe`, `PendingCapture` struct definitions**
- [ ] **Step 5: Verify it compiles (callers still need updating)**

---

## Chunk 3: Update all callers

### Task 4: Update handlers to use `self.pending`

**Files:**
- Modify: `crates/argus/src/tracer/handlers/file_ops.rs`
- Modify: `crates/argus/src/tracer/handlers/io_ops.rs`
- Modify: `crates/argus/src/tracer/handlers/mod.rs`
- Modify: `crates/argus/src/tracer/handlers/metadata_ops.rs`

**file_ops.rs:**
- `handle_open`: change `tracer.pending_opens.insert(...)` → `tracer.pending.insert(pid_u32, PendingSyscall::Open { ... })`

**io_ops.rs:**
- `handle_read` (fd==0 stdin path): change `tracer.pending_reads.insert(...)` → `tracer.pending.insert(pid_u32, PendingSyscall::Read { ... })`
- `handle_read` (file path): same change
- `handle_pipe`: change `tracer.pending_pipes.insert(...)` → `tracer.pending.insert(pid_u32, PendingSyscall::Pipe { ... })`

**mod.rs (handlers dispatch):**
- `cancel_syscall_with_eperm`: change `tracer.pending_eperm.insert(...)` → `tracer.pending.insert(pid_u32, PendingSyscall::Eperm)`
- `try_start_write_capture`: change `tracer.pending_captures.insert(...)` → `tracer.pending.insert(pid_u32, PendingSyscall::WriteCapture { ... })`
- `try_start_open_trunc_capture`: same pattern
- Write queue (`write_wait_queue`): entries store `PendingSyscall::WriteCapture` instead of `PendingCapture`

**metadata_ops.rs — convert to entry-only:**
Each handler becomes: parse args → return them. The dispatch in `mod.rs` stores a `PendingSyscall` variant and calls `ptrace::syscall`.

Change every metadata handler signature from:
```rust
pub fn handle_rename(tracer: &mut TracerLoop, pid: Pid, nr: u64, r: &UserRegs) -> Result<()>
```
to:
```rust
pub fn parse_rename_args(pid: Pid, nr: u64, r: &UserRegs) -> Result<(String, String)>
```

The dispatch in `mod.rs` becomes:
```rust
SYS_RENAME | SYS_RENAMEAT | SYS_RENAMEAT2 => {
    let (old_path, new_path) = metadata_ops::parse_rename_args(pid, nr, &r)?;
    let pid_u32 = pid.as_raw() as u32;
    tracer.pending.insert(pid_u32, PendingSyscall::Rename {
        pid: pid_u32, old_path, new_path,
    });
    ptrace::syscall(pid, None)?;
    return Ok(true);
}
```

- [ ] **Step 1: Update `file_ops.rs` callers**
- [ ] **Step 2: Update `io_ops.rs` callers**
- [ ] **Step 3: Convert metadata_ops handlers to arg-parsing functions**
- [ ] **Step 4: Update dispatch in `mod.rs` for metadata ops**
- [ ] **Step 5: Update `cancel_syscall_with_eperm` and write capture paths**
- [ ] **Step 6: Update `write_wait_queue` to store `PendingSyscall::WriteCapture`**
- [ ] **Step 7: Update `cleanup_dead_writer` to work with new types**
- [ ] **Step 8: Verify it compiles**
- [ ] **Step 9: Run unit tests** — `cargo test --target aarch64-unknown-linux-musl -p argus`
- [ ] **Step 10: Run integration tests** — `timeout 120 bash tests/validate.sh 1 2 3 4 5 6 7 8 9 10 11 12`
- [ ] **Step 11: Commit**

```
unify pending maps into PendingSyscall enum with return-value validation
```

---

## Chunk 4: Update tests

### Task 5: Update existing unit tests

**Files:**
- Modify: `crates/argus/src/tracer/trace_loop.rs` (test module)
- Modify: `crates/argus/src/tracer/handlers/mod.rs` (test module)

Tests that reference old field names need updating:
- `tracer_loop_new_initializes_empty_state`: check `tracer.pending.is_empty()` instead of individual maps
- `active_writes_blocks_concurrent_path`: `write_wait_queue` entries become `PendingSyscall::WriteCapture`
- `resume_next_queued_writer_drains_queue`: same
- `cleanup_dead_writer_*`: same
- `pending_eperm_insert_and_remove`: use `PendingSyscall::Eperm`

- [ ] **Step 1: Update all test references to old map names**
- [ ] **Step 2: Run unit tests**
- [ ] **Step 3: Run integration tests**
- [ ] **Step 4: Commit**

```
update tests for unified PendingSyscall
```

---

## Chunk 5: Cleanup

### Task 6: Remove dead code, update task doc

**Files:**
- Modify: `crates/argus/src/tracer/trace_loop.rs` — remove any leftover `use` imports for deleted types
- Modify: `crates/argus/src/tracer/pending.rs` — add compliance comment
- Create/Modify: `docs/tasks/p1-unified-pending.md`

- [ ] **Step 1: `cargo clippy --target aarch64-unknown-linux-musl -p argus` — fix any warnings**
- [ ] **Step 2: Final integration test run**
- [ ] **Step 3: Update task doc**
- [ ] **Step 4: Commit**

```
cleanup: remove dead pending types, add task doc
```

---

## Notes from Review

1. **Exit cleanup improvement:** Currently `Exited`/`Signaled` only cleans up `pending_captures`, and `pending_eperm` is never cleaned on exit. After unification, `self.pending.remove(&pid_u32)` covers all variants — a silent correctness fix.

2. **`complete_truncate` fields:** Must populate `old_size`, `before_hash`, `after_hash` to match the `ef::Truncate` event shape. Phase 1 uses dummy values (`old_size: 0`, hashes `None`) — same as today, but the complete handler must still include them.

3. **Truncate vs write-capture:** `SYS_TRUNCATE`/`SYS_FTRUNCATE` go through `metadata_ops::handle_truncate`, not `try_start_write_capture` (which only handles write/pwrite/writev/pwritev). No conflict — standalone truncate events keep their current shape with dummy hashes.

## Key Design Decisions

1. **`chown` stays log-only:** No event type exists for it. It doesn't go through the pending map — it stays as a debug log at entry. If we want chown events later, add the event type first.

2. **Write serialization unchanged:** The `active_writes` / `write_wait_queue` mechanism stays. `write_wait_queue` stores `PendingSyscall::WriteCapture` variants instead of `PendingCapture` structs. The queue's purpose (preventing kernel-level write interleaving) is orthogonal to the unification.

3. **close/dup/fcntl stay at entry:** These don't need return-value validation because:
   - `close` removing an fd from the table on a failed close is harmless (the fd is likely invalid anyway)
   - `dup2/dup3` specify the target fd explicitly (no kernel-chosen return)
   - `fcntl` for F_SETFD is idempotent

   If we wanted perfect correctness, these could move to entry/exit too, but the cost/benefit doesn't justify it now.

4. **Read events stay dual-path:** File reads use entry/exit (for buffer capture). Pipe/PTY reads emit immediately at entry (no content capture in Phase 1). This split stays because pipe reads don't need post-exit processing.
