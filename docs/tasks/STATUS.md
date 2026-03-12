# Pipeline Status

Last updated: 2026-03-11

## Merged to `main`

| Task | Branch | Tests | Review | Fixes |
|-|-|-|-|-|
| P0: Project Setup | main | n/a | n/a | n/a |
| P1: Config | `p1-config` | 34 pass | done | fix agent running |
| P1: Events | `p1-events` | 39 pass | done | fix agent running |
| P1: State | `p1-state` | 35 pass | done | fix agent running |
| P1: Seccomp | `p1-seccomp` | 12 pass | done | fixes done, follow-up running |
| P2: CAS | `p2-cas` | 23 pass | done | fix agent running |
| P2: Digest Cache | `p2-digest-cache` | 9 pass | not yet dispatched | — |

## Branches Pending Merge (implementation done, review/fix cycle)

| Task | Branch | Tests | Review | Fixes | Merge blocked on |
|-|-|-|-|-|-|
| P1: Net/TLS Env | `p1-net-env` | 9 pass | done | fix agent running | fixes completing |
| P2: S3 Upload | `p2-s3-upload` | 68 pass | done | fix agent running | fixes completing |

## Implementation In Progress

| Task | Branch | Status | Blocks |
|-|-|-|-|
| P1: Tracer Loop | `p1-tracer-loop` | agent running (CRITICAL PATH) | supervisor-main, content-capture, write-locking, pause-resume-api |

## Blocked (waiting on dependencies)

### Wave 3 — blocked on tracer loop merge
| Task | Branch | Depends on |
|-|-|-|
| P1: Supervisor Main | `p1-supervisor-main` | tracer-loop, net-env, config |
| P2: Content Capture | `p2-content-capture` | tracer-loop, cas, digest-cache |
| P2: Write Locking | `p2-write-locking` | tracer-loop, cas |
| P2: Pause/Resume API | `p2-pause-resume-api` | tracer-loop, events, config |
| P2: TLS Content | `p2-tls-content` | net-env, cas |
| P3: Indexes | `p3-indexes` | events, event-segments |

### Wave 3 — blocked on branch merges
| Task | Branch | Depends on |
|-|-|-|
| P2: Event Segments | `p2-event-segments` | events (merged), s3-upload (pending merge) |

### Wave 4+
| Task | Branch | Depends on |
|-|-|-|
| P3: Merkle Tree | `p3-merkle-tree` | content-capture, write-locking, cas |
| P3: Query API | `p3-query-api` | indexes, merkle-tree, pause-resume-api |
| P3: Restore | `p3-restore` | merkle-tree, s3-upload |
| P3: Realtime API | `p3-realtime-api` | query-api, events |
| P4: Container Image | `p4-container-image` | supervisor-main, s3-upload |
| P4: Cross-Agent | `p4-cross-agent` | container-image, query-api, s3-upload |
| P5: Polish | `p5-polish` | realtime-api, pause-resume-api |

## Review Findings Summary

### P1-Config (retro review)
- [x] Fix: rename `match_kind` → `type` via serde rename (spec compliance)
- [x] Fix: pre-compile glob patterns (performance)
- [x] Fix: remove compliance date comments
- [x] Fix: add PartialEq derives
- [x] Fix: log warnings for invalid glob patterns
- Agent: running

### P1-Events (retro review)
- [x] Fix: envelope.rs exceeds 300 lines — extract tests
- [x] Fix: serde flatten field collisions (start_agent_id, checkpoint_seq)
- [x] Fix: move vclock before payload
- [x] Fix: remove compliance date comments
- Agent: running

### P1-State (retro review)
- [x] Fix: dead binding in pipe_registry
- [x] Fix: write_locks needs remove() method
- [x] Fix: pty_registry needs multi-slave tracking after fork
- [x] Fix: document serde(skip) on fds
- [x] Fix: mark_exited should return fd table
- [x] Fix: fd_table.rs over 300 lines — extract serde helpers
- Agent: running

### P1-Seccomp (retro review + re-review)
- [x] Fix: add 4 missing syscalls (lseek, chown, fchown, fchownat) — done
- [x] Fix: remove compliance date comments — done
- [x] Fix: add SAFETY comment for const cast — done
- [x] Fix: u8 truncation guard in BPF builder — follow-up running
- [x] Fix: document 11 extra syscalls as intentional — follow-up running

### P2-CAS (retro review)
- [x] Fix: ContentHash validation on deserialization
- [x] Fix: use tempfile crate for atomic writes
- [x] Fix: remove compliance date comments
- [x] Fix: tracing format string bug
- [x] Fix: add empty data test
- [x] Fix: document stats TOCTOU trade-off
- Agent: running

### P1-Net-Env (pre-merge review)
- [x] Fix: remove `--mode transparent` (use regular proxy mode)
- [x] Fix: use SIGTERM not SIGKILL for mitmdump stop
- [x] Fix: unreliable missing_mitmdump test
- [x] Fix: partial file race in CA generation
- [x] Fix: remove compliance date comments
- [x] Fix: update task doc status
- [x] Fix: document cert path deviation
- Agent: running

### P2-S3-Upload (pre-merge review)
- [x] Fix: Arc data for retries instead of clone
- [x] Fix: add backoff jitter
- [x] Fix: log confirmation send failures
- [x] Fix: upload_pool.rs over 300 lines — extract tests
- [x] Fix: remove compliance date comments
- [x] Fix: clarify retry count naming
- Agent: running

### P2-Digest-Cache
- Review: not yet dispatched
