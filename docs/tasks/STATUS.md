# Pipeline Status

Last updated: 2026-03-11

## Context Recovery

When resuming this conversation, starting a new one, or after context compaction:
1. Read `/Users/zach/Development/argus-run/CLAUDE.md` to refresh full project context.
2. Read this file (`docs/tasks/STATUS.md`) for current pipeline state.
3. You are dispatching agents for implementation, blocking merges until tests pass and reviews + fixes are complete.
4. Check the **Running Agents Tracker** below before doing anything — pick up where you left off.

## Process Rules

1. **Update this file** after every agent completion and every merge to main.
2. **Check running agents** before dispatching new work — avoid duplicate effort.
3. **Block merges** until tests are done and reviews + fixes are complete.
4. **Dispatch reviews** immediately after implementation completes.
5. **Dispatch fix agents** immediately after reviews complete.
6. **Dispatch next wave** as soon as dependencies are merged and reviewed.

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

## Implementation In Progress

| Task | Branch | Agent status | Depends on |
|-|-|-|-|
| P2: TLS Content | `p2-tls-content` | agent running | net-env (merged), cas (merged) |
| P2: Event Segments | `p2-event-segments` | agent running | events (merged), s3-upload (merged) |
| P1: Supervisor Main | `p1-supervisor-main` | agent dispatching | tracer-loop (merged), net-env (merged), config (merged) |
| P2: Content Capture | `p2-content-capture` | agent dispatching | tracer-loop (merged), cas (merged), digest-cache (merged) |
| P2: Write Locking | `p2-write-locking` | agent dispatching | tracer-loop (merged), cas (merged) |
| P2: Pause/Resume API | `p2-pause-resume-api` | agent dispatching | tracer-loop (merged), events (merged), config (merged) |

## Blocked (waiting on dependencies)

| Task | Branch | Depends on |
|-|-|-|
| P3: Indexes | `p3-indexes` | events (merged), event-segments (in progress) |
| P3: Merkle Tree | `p3-merkle-tree` | content-capture (dispatching), write-locking (dispatching), cas (merged) |
| P3: Query API | `p3-query-api` | indexes, merkle-tree, pause-resume-api |
| P3: Restore | `p3-restore` | merkle-tree, s3-upload (merged) |
| P3: Realtime API | `p3-realtime-api` | query-api, events (merged) |
| P4: Container Image | `p4-container-image` | supervisor-main (dispatching), s3-upload (merged) |
| P4: Cross-Agent | `p4-cross-agent` | container-image, query-api, s3-upload (merged) |
| P5: Polish | `p5-polish` | realtime-api, pause-resume-api |

## Running Agents Tracker

Check these before dispatching new work:

| Agent | Task | Type | Status |
|-|-|-|-|
| a5bc4ff9599348725 | P2 TLS Content | implementation | running |
| afaf96b9a57292537 | P2 Event Segments | implementation | running |
| (pending) | P1 Supervisor Main | implementation | dispatching now |
| (pending) | P2 Content Capture | implementation | dispatching now |
| (pending) | P2 Write Locking | implementation | dispatching now |
| (pending) | P2 Pause/Resume API | implementation | dispatching now |
