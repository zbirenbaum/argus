# Pipeline Status

Last updated: 2026-03-11

## Process Rules

1. **Update this file** after every agent completion and every merge to main.
2. **Check running agents** before dispatching new work — avoid duplicate effort.
3. **Block merges** until review + fixes are complete.
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
| P2: S3 Upload | `p2-s3-upload` | 68 pass | done | 9dc9729 (merged a4f9d77) |

## Branches Pending Merge (review/fix cycle)

| Task | Branch | Tests | Review | Fixes | Merge blocked on |
|-|-|-|-|-|-|
| P1: Tracer Loop | `p1-tracer-loop` | 146 pass | done | fix agent running | fixes completing |

## Implementation In Progress

| Task | Branch | Agent status | Depends on |
|-|-|-|-|
| P2: TLS Content | `p2-tls-content` | agent running | net-env (merged), cas (merged) |
| P2: Event Segments | `p2-event-segments` | agent running | events (merged), s3-upload (merged) |

## Ready to Dispatch (after tracer loop merges)

| Task | Branch | Depends on |
|-|-|-|
| P1: Supervisor Main | `p1-supervisor-main` | tracer-loop, net-env, config |
| P2: Content Capture | `p2-content-capture` | tracer-loop, cas, digest-cache |
| P2: Write Locking | `p2-write-locking` | tracer-loop, cas |
| P2: Pause/Resume API | `p2-pause-resume-api` | tracer-loop, events, config |

## Blocked (waiting on dependencies)

| Task | Branch | Depends on |
|-|-|-|
| P3: Indexes | `p3-indexes` | events, event-segments |
| P3: Merkle Tree | `p3-merkle-tree` | content-capture, write-locking, cas |
| P3: Query API | `p3-query-api` | indexes, merkle-tree, pause-resume-api |
| P3: Restore | `p3-restore` | merkle-tree, s3-upload |
| P3: Realtime API | `p3-realtime-api` | query-api, events |
| P4: Container Image | `p4-container-image` | supervisor-main, s3-upload |
| P4: Cross-Agent | `p4-cross-agent` | container-image, query-api, s3-upload |
| P5: Polish | `p5-polish` | realtime-api, pause-resume-api |

## Running Agents Tracker

Check these before dispatching new work:

| Agent | Task | Type | Status |
|-|-|-|-|
| a3d9966e1978f5429 | P1 Tracer Loop fixes | fix | running |
| a5bc4ff9599348725 | P2 TLS Content | implementation | running |
| afaf96b9a57292537 | P2 Event Segments | implementation | running |
