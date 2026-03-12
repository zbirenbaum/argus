# Task Graph — Worktree & Merge Strategy

## Branching Model

All work happens in **isolated git worktrees** branched from `main`.
Each task gets its own branch. Merges go back to `main` via PR once tests pass.

### Rules

1. **One worktree per task.** Never mix tasks in a single branch.
2. **Branch from `main`** unless the task depends on an unmerged branch — then branch from (or rebase onto) the dependency branch.
3. **Merge order follows the dependency graph.** A task's PR cannot merge until all its blockers are merged into `main`.
4. **Rebase before merge.** After a dependency merges, rebase the dependent branch onto `main` to pick it up.
5. **No long-lived integration branches.** Everything flows through `main`.

### Stacked PRs (when needed)

If two tasks are sequential and the first is still in review, the second can branch off the first and target it:

```
main ← p1-events ← p1-tracer-loop
```

When `p1-events` merges to `main`, retarget `p1-tracer-loop` PR to `main` and rebase.


## Worktree Commands

```bash
# Create a worktree for a task (targeting main)
git worktree add ../argus-<branch> -b <branch> main

# Create a stacked worktree (targeting a dependency branch)
git worktree add ../argus-<branch> -b <branch> <dependency-branch>

# After dependency merges, rebase onto main and retarget PR
cd ../argus-<branch>
git fetch origin main
git rebase origin/main
gh pr edit --base main

# Clean up after merge
git worktree remove ../argus-<branch>
```


## Dependency Graph

```
Wave 1 (no deps — start immediately, all parallel)
├── p1-config         → main
├── p1-events         → main
├── p1-state          → main
├── p1-seccomp        → main
└── p2-cas            → main

Wave 2 (each unblocked by specific Wave 1 tasks)
├── p1-net-env        → main   ← p1-config
├── p1-tracer-loop    → main   ← p1-events, p1-state, p1-seccomp
├── p2-s3-upload      → main   ← p2-cas
├── p2-digest-cache   → main   ← p2-cas
└── p2-event-segments → main   ← p1-events, p2-s3-upload

Wave 3 (after tracer loop and storage foundations)
├── p1-supervisor-main  → main ← p1-tracer-loop, p1-net-env, p1-config
├── p2-content-capture  → main ← p1-tracer-loop, p2-cas, p2-digest-cache
├── p2-write-locking    → main ← p1-tracer-loop, p2-cas
├── p2-pause-resume-api → main ← p1-tracer-loop, p1-events, p1-config
├── p2-tls-content      → main ← p1-net-env, p2-cas
└── p3-indexes          → main ← p1-events, p2-event-segments

Wave 4 (after content pipeline)
├── p3-merkle-tree    → main   ← p2-content-capture, p2-write-locking, p2-cas
└── p3-query-api      → main   ← p3-indexes, p3-merkle-tree, p2-pause-resume-api

Wave 5 (after query layer)
├── p3-restore        → main   ← p3-merkle-tree, p2-s3-upload
├── p3-realtime-api   → main   ← p3-query-api, p1-events
└── p4-container-image→ main   ← p1-supervisor-main, p2-s3-upload

Wave 6 (final)
├── p4-cross-agent    → main   ← p4-container-image, p3-query-api, p2-s3-upload
└── p5-polish         → main   ← p3-realtime-api, p2-pause-resume-api
```

**Critical path:** p1-events → p1-tracer-loop → p2-content-capture → p3-merkle-tree → p3-query-api → p3-realtime-api → p5-polish


## Full Task Assignments

### Wave 1 — No dependencies, start immediately

All five tasks can run as parallel worktrees.

| Task | Branch | Target | Depends on | Module |
|-|-|-|-|-|
| P1: Config | `p1-config` | `main` | — | `sandbox::config` |
| P1: Events | `p1-events` | `main` | — | `sandbox::events` |
| P1: State | `p1-state` | `main` | — | `sandbox::state` |
| P1: Seccomp | `p1-seccomp` | `main` | — | `sandbox::tracer::seccomp` |
| P2: CAS | `p2-cas` | `main` | — | `sandbox::cas` |

**Merge:** All five can merge to `main` in any order as soon as each passes review.

---

### Wave 2 — Unblocked once specific Wave 1 tasks merge

| Task | Branch | Target | Depends on | Module |
|-|-|-|-|-|
| P1: Net/TLS Env | `p1-net-env` | `main` | `p1-config` merged | `sandbox::net` |
| P1: Tracer Loop | `p1-tracer-loop` | `main` | `p1-events`, `p1-state`, `p1-seccomp` merged | `sandbox::tracer` |
| P2: S3 Upload | `p2-s3-upload` | `main` | `p2-cas` merged | `sandbox::storage::{s3,upload_pool}` |
| P2: Digest Cache | `p2-digest-cache` | `main` | `p2-cas` merged | `sandbox::storage::digest_cache` |
| P2: Event Segments | `p2-event-segments` | `main` | `p1-events`, `p2-s3-upload` merged | `sandbox::storage::{event_log,local_buffer}` |

**Stacking opportunity:** `p1-tracer-loop` can branch from `p1-events` (target `p1-events`) while `p1-state` and `p1-seccomp` are still in review, then retarget to `main` once all three merge. Same for `p2-s3-upload` branching from `p2-cas`.

---

### Wave 3 — After tracer loop and storage foundations

| Task | Branch | Target | Depends on | Module |
|-|-|-|-|-|
| P1: Supervisor Main | `p1-supervisor-main` | `main` | `p1-tracer-loop`, `p1-net-env`, `p1-config` merged | `supervisor::main` |
| P2: Content Capture | `p2-content-capture` | `main` | `p1-tracer-loop`, `p2-cas`, `p2-digest-cache` merged | `sandbox::tracer::handlers` (extend) |
| P2: Write Locking | `p2-write-locking` | `main` | `p1-tracer-loop`, `p2-cas` merged | `sandbox::state` (extend) |
| P2: Pause/Resume API | `p2-pause-resume-api` | `main` | `p1-tracer-loop`, `p1-events`, `p1-config` merged | `sandbox::api` |
| P2: TLS Content | `p2-tls-content` | `main` | `p1-net-env`, `p2-cas` merged | `sandbox::net` (extend) |
| P3: Indexes | `p3-indexes` | `main` | `p1-events`, `p2-event-segments` merged | `sandbox::index` |

**Note:** All six are independent of each other and can run in parallel once their deps merge.

---

### Wave 4 — After content pipeline complete

| Task | Branch | Target | Depends on | Module |
|-|-|-|-|-|
| P3: Merkle Tree | `p3-merkle-tree` | `main` | `p2-content-capture`, `p2-write-locking`, `p2-cas` merged | `sandbox::snapshot::{merkle,checkpoint}` |
| P3: Query API | `p3-query-api` | `main` | `p3-indexes`, `p3-merkle-tree`, `p2-pause-resume-api` merged | `sandbox::api` (extend) |

**Note:** Merkle tree and query API are sequential — query API needs merkle tree for tree endpoints.

---

### Wave 5 — After query layer

| Task | Branch | Target | Depends on | Module |
|-|-|-|-|-|
| P3: Restore | `p3-restore` | `main` | `p3-merkle-tree`, `p2-s3-upload` merged | `sandbox::snapshot::restore` |
| P3: Realtime API | `p3-realtime-api` | `main` | `p3-query-api`, `p1-events` merged | `sandbox::api` (extend) |
| P4: Container Image | `p4-container-image` | `main` | `p1-supervisor-main`, `p2-s3-upload` merged | `deploy/` |

**Note:** All three are independent of each other.

---

### Wave 6 — Final

| Task | Branch | Target | Depends on | Module |
|-|-|-|-|-|
| P4: Cross-Agent | `p4-cross-agent` | `main` | `p4-container-image`, `p3-query-api`, `p2-s3-upload` merged | `sandbox::api`, `cli` |
| P5: Polish | `p5-polish` | `main` | `p3-realtime-api`, `p2-pause-resume-api` merged | all crates |

---


## Maximum Parallelism Schedule

At peak, up to **6 worktrees** can be active simultaneously (Wave 3). Here's the maximum concurrency at each stage:

| Stage | Active worktrees | Tasks |
|-|-|-|
| Wave 1 | 5 | config, events, state, seccomp, cas |
| Wave 2 | 5 | net-env, tracer-loop, s3-upload, digest-cache, event-segments |
| Wave 3 | 6 | supervisor-main, content-capture, write-locking, pause-resume-api, tls-content, indexes |
| Wave 4 | 2 | merkle-tree, query-api (sequential) |
| Wave 5 | 3 | restore, realtime-api, container-image |
| Wave 6 | 2 | cross-agent, polish |


## Merge Checklist (per task)

Before merging any branch to `main`:

1. All dependency branches already merged to `main`
2. Branch rebased onto current `main`
3. `cargo check --workspace` passes
4. `cargo test -p sandbox` passes (unit tests)
5. Integration tests pass in dev container (if applicable)
6. Code review completed
7. Task doc updated with status: done
