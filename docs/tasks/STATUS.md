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
# Native ARM64 container (Apple Silicon — full seccomp + ptrace support)
docker exec argus-arm64 bash -c "cd /workspace && cargo test --workspace"

# Run ignored integration tests (require ptrace)
docker exec argus-arm64 bash -c "cd /workspace && cargo test --workspace -- --ignored"
```

Container: `argus-arm64` — **native aarch64, full seccomp-BPF + ptrace support**.

### Building the Supervisor Binary

Cross-compile on the host using zigbuild, or build natively in the container.

```bash
# aarch64 (native, for ARM64 container)
cargo zigbuild --target aarch64-unknown-linux-musl -p supervisor

# x86_64 (cross-compile, for x86_64 deployment)
cargo zigbuild --target x86_64-unknown-linux-musl -p supervisor
```

**Do not use `--release` while debugging** — debug builds are significantly faster.

### Running Validation Tests

Run in the ARM64 container (native seccomp works):

```bash
docker exec argus-arm64 bash -c "cd /workspace && ./target/aarch64-unknown-linux-musl/debug/supervisor --agent-id test -- bash -c 'echo hello'"
```

See `docs/spec/12-testing.md` for the full 12-test validation suite.

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
| 9: Pause/resume | not tested | |
| 10: Pause-before-action | not tested | |
| 11: Snapshot/restore | not tested | |
| 12: Initial state | not tested | |

## Ready to Dispatch (dependencies met)

| Task | Depends on | Status |
|-|-|-|
| P3: Restore | merkle-tree (merged), s3-upload (merged) | **READY** |
| P3: Query API | indexes (merged), merkle-tree (merged), pause-resume-api (merged) | **READY** |
| P4: Container Image | supervisor-main (merged), s3-upload (merged) | **READY** |

## Blocked (waiting on dependencies)

| Task | Depends on |
|-|-|
| P3: Realtime API | query-api |
| P4: Cross-Agent | container-image, query-api |
| P5: Polish | realtime-api, pause-resume-api (merged) |

## Running Agents Tracker

No agents currently running.

## Next Steps (in order)

1. **Dispatch P3 Restore, P3 Query API, P4 Container Image** — all three are unblocked, can run in parallel
2. **Run validation tests 7-12** — after restore + query API are merged (tests 7-11 need those features)
3. **Dispatch P3 Realtime API** — after query-api merged
4. **Dispatch P4 Cross-Agent** — after container-image + query-api merged
5. **Dispatch P5 Polish** — after realtime-api merged
6. **Clean up stale worktrees** — 15 old worktrees from merged branches

## Build Status

- Last full test run: 408 pass (391 argus + 17 supervisor), 0 fail, 5 ignored
- All P1 + P2 + P3 (indexes, merkle) merged to main
- Validation tests 1-7 pass on native aarch64
- Seccomp-BPF works natively on ARM64 (no more Rosetta/QEMU issues)
