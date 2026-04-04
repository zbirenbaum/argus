# Pipeline Status

Last updated: 2026-03-15

## Context Recovery

When resuming this conversation, starting a new one, or after context compaction:
1. Read `/Users/zach/Development/argus-run/CLAUDE.md` to refresh full project context.
2. Read this file (`docs/tasks/STATUS.md`) for current pipeline state.
3. You are dispatching agents for implementation, blocking merges until tests pass and reviews + fixes are complete.
4. Check the **Running Agents Tracker** below before doing anything — pick up where you left off.

## How to Run Tests

This project only builds on Linux. Use the dev container:

```bash
# Build
docker exec argus-arm64 cargo build --target aarch64-unknown-linux-musl -p supervisor -p argus-api

# Unit tests
docker exec argus-arm64 cargo test --target aarch64-unknown-linux-musl -p argus -p supervisor -p argus-api

# Validation tests (all 13)
docker exec argus-arm64 ./tests/validate.sh

# Single validation test
docker exec argus-arm64 ./tests/validate.sh 8
```

Container: `argus-arm64` — **native aarch64, full seccomp-BPF + ptrace support**.
Never use `cargo build` without `--target aarch64-unknown-linux-musl`.

## Process Rules

1. **Update this file** after every agent completion and every merge to main.
2. **Check running agents** before dispatching new work — avoid duplicate effort.
3. **Block merges** until tests are done and reviews + fixes are complete.
4. **Dispatch reviews** immediately after implementation completes.
5. **Dispatch fix agents** immediately after reviews complete.
6. **Dispatch next wave** as soon as dependencies are merged and reviewed.
7. **Run tests in the dev container** after merging — confirm they pass before moving on.

## Compiler Warning Audit (2026-03-15)

**144 warnings in `argus` crate.** Categorized below by root cause.

### Dead code (safe to delete)

| Module | What | Why dead |
|-|-|-|
| `tracer/syscall_nr.rs` | Entire module (65 constants) | Superseded by `libc::SYS_*`, used everywhere in `pipeline/stages/syscall_handlers.rs` |
| `tracer/memory.rs` | `read_path_at`, `read_proc_cwd`, `read_proc_exe`, `read_proc_cmdline`, `AT_FDCWD` | Pipeline resolves paths via direct `/proc` reads instead |
| `tracer/pending.rs` | `CaptureKind`, `PendingSyscall` | Superseded by `pipeline/classified.rs::PendingEntry` |

### Tested but not wired into runtime

These have passing unit tests but no methods are called from the actual pipeline/runtime:

| Module | What's tested | What's missing | Blocker |
|-|-|-|-|
| `index/` (PathIndex, TypeIndex, PidIndex, QueryEngine) | 338 tests pass | Zero index methods called at runtime. `QueryEngine` never constructed outside tests. | p3-query-api (not started) — would expose indexes through HTTP endpoints |
| `approver/` + `api/routes` (pause/resume/approvals) | 36 tests pass (API layer) | API endpoints work but `cancel_syscall`/`set_ret`/`set_regs` in `regs.rs` are never called — pause has no effect on traced process | ptrace-level enforcement not wired into pipeline stages |
| `pipeline/capture_policy.rs` | `should_capture()` tested | `should_capture()` never called — classify stage doesn't consult policy | Needs call site in classify stage |
| `net/dedup.rs` (NetworkDedup) | Unit tests pass | Never constructed outside tests | p2-tls-content integration incomplete |

### Partially wired (struct used, some methods dead)

| Module | Methods called | Methods never called |
|-|-|-|
| `state/fd_table.rs` | `insert`, `get_mut`, `dup`, `close_cloexec`, `insert_cloexec`, `clone_for_fork`, `remove` | `new`, `from_proc`, `is_cloexec`, `set_cloexec`, `clear_cloexec`, `iter`, `len`, `is_empty` |
| `state/pipe_registry.rs` | `create_pipe` | `on_fork`, `on_close`, `on_dup`, `get`, `len`, `is_empty` — pipe lifecycle not tracked on fork/close/dup |
| `storage/upload_pool.rs` | `submit` | `stats`, `workers`, `confirmation_rx`, `job_sender()`, `snapshot()`, `shutdown()` — operational monitoring |
| `storage/digest_cache.rs` | `new`, `contains` | `DigestCacheStats`, `SerializedCache` — checkpoint serialization |
| `storage/event_log.rs` | `new`, `append` (via sink) | `current_segment_size`, `current_segment_seq`, `finalize` |
| `snapshot/tree.rs` | `load`, `insert`, `commit` (in tests) | `write_meta`, `load_entries`, `load_meta` — checkpoint persistence helpers |
| `pipeline/classified.rs` | Most variants | `FileRmdir`, `FileOpen`, `PtyCreate`, `PtyData` variants never constructed |
| `pipeline/outputs/mod.rs` | `push`, event dispatch | `flush`, `len`, `is_empty` |
| `pipeline/sinks/broadcast.rs` | Construction, send | `subscribe` |
| `pipeline/sinks/event_log.rs` | Construction, sink trait | `with_log` |

### Constructed but entirely unused

| Module | What | Notes |
|-|-|-|
| `state/process_tree.rs` | `ProcessTree`, `ProcessState` | Constructed with `::new()` in runtime, zero methods called. Process tracking done implicitly via FdTable's pid-keyed map |
| `state/pty_registry.rs` | `PtyRegistry` | Constructed, zero methods called, `pty_registry` field in ClassifyStage never read. PTY tracking unimplemented |
| `pipeline/replay.rs` | `RawStopRecorder`, `ReplayStream` | Debug replay infrastructure, never called |
| `pipeline/sinks/memory.rs` | `MemorySink` | Only used in test code |
| `storage/local_buffer.rs` | `LocalBuffer`, `BufferEntry` | Phase 3 operational feature, never constructed |
| `cas/durability.rs` | `DurabilityLayer.persist()` | Layer constructed but `.persist()` never called |
| `pipeline/stages/classify.rs` | `pty_registry` field | Field stored but never read |
| `runtime.rs` | `upload_pool` field | Field stored but never read at struct level |

### Planned but not started

| Module | Feature | Spec |
|-|-|-|
| `tracer/regs.rs` partials | `cancel_syscall`, `ret_val`, `set_ret`, `set_regs` — ptrace-level syscall modification | §06 agent controls |
| `net/` (MitmProcessor, flow functions) | `process_flow`, `store_headers`, `store_body`, `parse_flow_lines` | §07 TLS network, p2-tls-content in progress |

## Merged to `main` (reviewed + fixed)

| Task | Branch | Tests | Review | Fix commit |
|-|-|-|-|-|
| P0: Project Setup | main | n/a | n/a | n/a |
| P1: Config | `p1-config` | 34 pass | done | d9a36be |
| P1: Events | `p1-events` | 39 pass | done | bdceea8 |
| P1: State | `p1-state` | 35 pass | done | dc11e91 |
| P1: Seccomp | `p1-seccomp` | 12 pass | done | 7472229 |
| P2: CAS | `p2-cas` | 23 pass | done | 2c4a343 |
| P2: Digest Cache | `p2-digest-cache` | 9 pass | done | 630db19 |
| P1: Net/TLS Env | `p1-net-env` | 9 pass | done | e22fda7 |
| P2: S3 Upload | `p2-s3-upload` | 68 pass | done | 9dc9729 |
| P1: Tracer Loop | `p1-tracer-loop` | 146 pass | done | 6d973bc |
| P2: TLS Content | `p2-tls-content` | 29 pass | done | b4acce9 |
| P2: Event Segments | `p2-event-segments` | 17 pass | done | 7f9b893 |
| P2: Content Capture | `p2-content-capture` | 13 pass | done | 0cb7f9a |
| P2: Write Locking | `p2-write-locking` | 8 pass | done | 0cb7f9a |
| P2: Pause/Resume API | `p2-pause-resume-api` | 36 pass | done | 0cb7f9a |
| P1: Supervisor Main | `p1-supervisor-main` | 17 pass | done | c80c2e5 |
| aarch64 + libc→nix | main | 395 pass | — | 7347a69 |
| P3: Indexes | `p3-indexes` | 338 pass | done | merged |
| P3: Merkle Tree | `p3-merkle-tree` | 335 pass | done | merged |

## Validation Tests (docs/spec/12-testing.md)

| Test | Status | Notes |
|-|-|-|
| 1: Process tracing | PASS | exec, fork, exit, stdio, read, socket events |
| 2: Stdio capture | PASS | stdout/stderr separated with correct sizes |
| 3: File write/read/delete | PASS | write, read, unlink with paths and sizes |
| 4: Pipe topology | PASS | pipe_data flow through echo→grep→wc |
| 5: Subprocess tree | PASS | python3→ls with pipe_data back to parent |
| 6: Escape test | PASS | Tool creation, exec, write attribution, unlink |
| 7: Write locking | PASS | 49 write events, unbroken hash chain across 3 concurrent threads |
| 8: TLS capture | not tested | |
| 9: Pause/resume | not tested | API layer works (36 tests) but ptrace enforcement not wired |
| 10: Pause-before-action | not tested | Approver chain tested (22 tests) but `cancel_syscall` not wired |
| 11: Snapshot/restore | not tested | Restore endpoint works (8 tests) but full checkpoint flow not wired |
| 12: Initial state | not tested | |
| 13: Child reaping | not tested | |

## Ready to Dispatch (dependencies met)

| Task | Depends on | Status |
|-|-|-|
| P3: Query API | indexes (merged), merkle-tree (merged), pause-resume-api (merged) | **READY** — would wire QueryEngine to HTTP, eliminating index dead code |
| P4: Container Image | supervisor-main (merged), s3-upload (merged) | **READY** |
| Wire ptrace enforcement | pause-resume-api (merged) | **READY** — connect `cancel_syscall`/`set_ret` to pipeline, make pause actually stop syscalls |
| Wire CapturePolicy | content-capture (merged) | **READY** — add `should_capture()` call in classify stage |
| Wire ProcessTree | state (merged), tracer-loop (merged) | **READY** — call `add_process`/`mark_exited`/`get_children` from classify stage fork/exit handlers |
| Wire PipeRegistry lifecycle | state (merged) | **READY** — call `on_fork`/`on_close`/`on_dup` from classify stage |

## Blocked (waiting on dependencies)

| Task | Depends on |
|-|-|
| P3: Realtime API | query-api |
| P4: Cross-Agent | container-image, query-api |
| P5: Polish | realtime-api, pause-resume-api (merged) |

## Running Agents Tracker

No agents currently running.

## Next Steps (in order)

1. **Delete dead code** — `tracer/syscall_nr.rs`, dead functions in `memory.rs`, `pending.rs`
2. **Wire disconnected pieces** — ProcessTree, PipeRegistry lifecycle, CapturePolicy, ptrace enforcement for pause
3. **Dispatch P3 Query API** — unblocked, would wire indexes to HTTP
4. **Dispatch P4 Container Image** — unblocked, can run in parallel
5. **Run validation tests 8-13** — after wiring + query API are merged
6. **Dispatch P3 Realtime API** — after query-api merged
7. **Clean up stale worktrees** — old worktrees from merged branches

## Build Status

- Last full test run: 408 pass (391 argus + 17 supervisor + 9 argus-api), 0 fail, 5 ignored
- **144 compiler warnings** — see audit above
- All P1 + P2 + P3 (indexes, merkle) merged to main
- Validation tests 1-7 pass on native aarch64
