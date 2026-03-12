# Pipeline Status

Last updated: 2026-03-12

## Context Recovery

When resuming this conversation, starting a new one, or after context compaction:
1. Read `/Users/zach/Development/argus-run/CLAUDE.md` to refresh full project context.
2. Read this file (`docs/tasks/STATUS.md`) for current pipeline state.
3. You are dispatching agents for implementation, blocking merges until tests pass and reviews + fixes are complete.
4. Check the **Running Agents Tracker** below before doing anything — pick up where you left off.

## How to Run Tests

This project only builds on Linux. Use the dev container:

```bash
# Find the container
docker ps | grep vsc-argus-run

# Run all tests (unit tests, no ptrace needed)
docker exec <container_id> bash -c "cd /workspaces/argus-run && cargo test --workspace"

# Run tests for a specific crate
docker exec <container_id> bash -c "cd /workspaces/argus-run && cargo test -p sandbox"

# Run ignored integration tests (require ptrace)
docker exec <container_id> bash -c "cd /workspaces/argus-run && cargo test --workspace -- --ignored"
```

Container ID: `04863da34598` (silly_snyder) — **aarch64, unit tests only**.
Always verify tests pass in the container before merging.

### Building for x86_64

Cross-compile on the host using zigbuild. No need to build inside a container.

```bash
# One-time setup
cargo install cargo-zigbuild

# Debug build (fast, use while iterating)
cargo zigbuild --target x86_64-unknown-linux-musl -p supervisor

# Release build (slow, use for final validation)
cargo zigbuild --release --target x86_64-unknown-linux-musl -p supervisor
```

**Do not use `--release` while debugging** — debug builds are significantly faster.

Debug binary: `target/x86_64-unknown-linux-musl/debug/supervisor`
Release binary: `target/x86_64-unknown-linux-musl/release/supervisor`

### x86_64 Container for Running Validation Tests

The seccomp BPF filter hardcodes `AUDIT_ARCH_X86_64` and kills with SIGSYS on arch mismatch.
**All validation tests from `12-testing.md` require an x86_64 container.**

After restarting Docker Desktop (to restore QEMU binfmt registration), use the project
devcontainer which already specifies `--platform=linux/amd64`:

```bash
# Rebuild devcontainer via VS Code:
# Cmd+Shift+P → "Dev Containers: Rebuild Container"

# Or run manually:
docker build --platform linux/amd64 -t argus-dev -f .devcontainer/Dockerfile.devcontainer .
docker run -d --platform linux/amd64 \
  --name argus-x86 \
  --cap-add SYS_PTRACE \
  --security-opt seccomp=unconfined \
  --init \
  -v /Users/zach/Development/argus-run:/workspace \
  argus-dev sleep infinity

# Verify architecture
docker exec argus-x86 uname -m  # must say x86_64

# Run validation test 1 using cross-compiled binary
docker exec argus-x86 bash -c "cd /workspace && ./target/x86_64-unknown-linux-musl/release/supervisor --agent-id test -- bash -c 'echo hello'"
```

## Process Rules

1. **Update this file** after every agent completion and every merge to main.
2. **Check running agents** before dispatching new work — avoid duplicate effort.
3. **Block merges** until tests are done and reviews + fixes are complete.
4. **Dispatch reviews** immediately after implementation completes.
5. **Dispatch fix agents** immediately after reviews complete.
6. **Dispatch next wave** as soon as dependencies are merged and reviewed.
7. **Run tests in the dev container** after merging — confirm they pass before moving on.

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

## Awaiting Container Test Verification (review + fixes done, need cargo test)

| Task | Branch | Worktree | Review | Fixes Applied |
|-|-|-|-|-|
| P3: Indexes | `p3-indexes` | agent-a37e3bc4 | done | chrono time parsing, malformed entry logging, no-filter fallback, IndexEntry moved to mod.rs, tests split, combined filter tests added |
| P3: Merkle Tree | `p3-merkle-tree` | agent-a74530a1 | done | diff_trees subtree-skipping, removed unused TreeEntry, root_hash takes &self via Cell, DiffKind Copy + DiffEntry Hash, checkpoint versioning |

**Verified before fix agents ran:** P3 Indexes 40 tests pass, P3 Merkle 39 tests pass (aarch64 container).
Fix agents applied changes after that verification. Need to re-verify after fixes.

## Uncommitted Changes on `main`

### Supervisor ptrace startup fix (DONE, needs commit)

**Root cause found and fixed:** `PTRACE_TRACEME` (child) and `PTRACE_SEIZE` (parent) are
mutually exclusive attachment mechanisms. The original code had the child call `PTRACE_TRACEME`
+ `SIGSTOP`, then the parent tried `PTRACE_SEIZE` which fails with EPERM.

**Fix:** Replaced `TRACEME`/`SIGSTOP` with pipe-based synchronization:
1. Parent creates a pipe before `fork()`
2. Child blocks on `read(pipe_r)` after fork
3. Parent calls `PTRACE_SEIZE` on child (succeeds — child is alive but not TRACEME'd)
4. Parent writes to pipe, unblocking child
5. Child installs seccomp filter (SECCOMP_RET_TRACE now works because tracer is attached)
6. Child calls `execvpe`

**Confirmed working via Python reproduction tests in container:**
- `PTRACE_SEIZE` after `PTRACE_TRACEME` → EPERM (reproduces bug)
- `PTRACE_SEIZE` + pipe sync without `TRACEME` → succeeds, exec events delivered

**Files changed on main (uncommitted):**
- `crates/supervisor/src/startup.rs` — pipe sync, removed TRACEME/SIGSTOP/wait_for_child_stop
- `crates/supervisor/src/main.rs` — `spawn_agent` returns `(Pid, i32)`, passes pipe fd to `tracer.run()`
- `crates/sandbox/src/tracer/trace_loop.rs` — `run()` takes `sync_pipe_w: i32`, signals child after seize
- `docs/spec/12-testing.md` — new: 12 validation tests, integration test, bug indicators
- `docs/spec/11-implementation-phases.md` — added "Validate with" lines per phase
- `CLAUDE.md` — documents table updated with 12-testing.md

**Could not run validation tests** because the dev container is aarch64 and the seccomp BPF
filter checks for x86_64 architecture, killing the process with SIGSYS on mismatch.
Need x86_64 container via QEMU emulation (requires Docker Desktop restart).

## New Spec Added

| File | Description |
|-|-|
| `docs/spec/12-testing.md` | 12 validation tests, integration test (trace coding agent), bug indicators table |
| `docs/spec/11-implementation-phases.md` | Updated with "Validate with" lines per phase |
| `CLAUDE.md` | Documents table updated with 12-testing.md |

## Ready to Dispatch (dependencies met after indexes + merkle merge)

| Task | Depends on |
|-|-|
| P3: Restore | merkle-tree, s3-upload (merged) |

## Blocked (waiting on dependencies)

| Task | Branch | Depends on |
|-|-|-|
| P3: Query API | `p3-query-api` | indexes, merkle-tree, pause-resume-api (merged) |
| P3: Realtime API | `p3-realtime-api` | query-api, events (merged) |
| P4: Container Image | `p4-container-image` | supervisor-main (merged), s3-upload (merged) — UNBLOCKED |
| P4: Cross-Agent | `p4-cross-agent` | container-image, query-api, s3-upload (merged) |
| P5: Polish | `p5-polish` | realtime-api, pause-resume-api (merged) |

## Running Agents Tracker

No agents currently running.

## Next Steps (in order)

1. **Restart Docker Desktop** — restores QEMU binfmt for x86_64 emulation
2. **Spin up x86_64 container** — `docker run --platform linux/amd64` with SYS_PTRACE (see commands above)
3. **Re-verify P3 branches** — `cargo test` for indexes and merkle in aarch64 container (unit tests work there)
4. **Merge P3 Indexes + P3 Merkle Tree** — once unit tests pass
5. **Commit ptrace startup fix** — the TRACEME→SEIZE pipe sync change on main
6. **Build supervisor in x86_64 container** — `cargo build -p supervisor`
7. **Run validation tests 1-12** — from `docs/spec/12-testing.md`, in the x86_64 container
8. **Dispatch P3 Restore, P3 Query API** — after indexes + merkle merged
9. **Dispatch P4 Container Image** — unblocked now

## Build Status

- Last full test run: 311 pass (294 sandbox + 17 supervisor), 0 fail, 2 ignored
- Commit: 1549470
- Supervisor binary builds, ptrace startup fix applied but untested on x86_64
- aarch64 container cannot run validation tests (seccomp arch check kills child)
