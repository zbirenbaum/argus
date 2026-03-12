# Pipeline Status

Last updated: 2026-03-11

## Merged to `main`

| Task | Branch | Tests | Review | Fixes |
|-|-|-|-|-|
| P0: Project Setup | main | n/a | n/a | n/a |
| P1: Config | `p1-config` | 34 pass | done | done (d9a36be) |
| P1: Events | `p1-events` | 39 pass | done | done (bdceea8) |
| P1: State | `p1-state` | 35 pass | done | done (dc11e91) |
| P1: Seccomp | `p1-seccomp` | 12 pass | done | done (7472229) |
| P2: CAS | `p2-cas` | 23 pass | done | done (2c4a343) |
| P2: Digest Cache | `p2-digest-cache` | 9 pass | review dispatched | — |
| P1: Net/TLS Env | `p1-net-env` | 9 pass | done | done (e22fda7) |
| P2: S3 Upload | `p2-s3-upload` | 68 pass | done | done (9dc9729, merged a4f9d77) |

## Branches Pending Merge (review/fix cycle)

| Task | Branch | Tests | Review | Fixes | Merge blocked on |
|-|-|-|-|-|-|
| P1: Tracer Loop | `p1-tracer-loop` | not yet run | review dispatched | — | review + fixes |

## Blocked (waiting on dependencies)

### Wave 3 — blocked on tracer loop merge
| Task | Branch | Depends on |
|-|-|-|
| P1: Supervisor Main | `p1-supervisor-main` | tracer-loop, net-env, config |
| P2: Content Capture | `p2-content-capture` | tracer-loop, cas, digest-cache |
| P2: Write Locking | `p2-write-locking` | tracer-loop, cas |
| P2: Pause/Resume API | `p2-pause-resume-api` | tracer-loop, events, config |
| P2: TLS Content | `p2-tls-content` | net-env, cas |
| P2: Event Segments | `p2-event-segments` | events (merged), s3-upload (merged) |

### Wave 4+
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

## Unblocked & Ready to Dispatch

| Task | Branch | Depends on | Status |
|-|-|-|-|
| P2: TLS Content | `p2-tls-content` | net-env (merged), cas (merged) | ready |
| P2: Event Segments | `p2-event-segments` | events (merged), s3-upload (merged) | ready |

## Review Findings Summary

### P1-Config — done
All 5 fixes applied and committed (d9a36be).

### P1-Events — done
All 4 fixes applied and committed (bdceea8).

### P1-State — done
All 6 fixes applied and committed (dc11e91).

### P1-Seccomp — done
All 5 fixes applied and committed (7472229).

### P2-CAS — done
All 6 fixes applied and committed (2c4a343).

### P1-Net-Env — done
All 7 fixes applied and committed (e22fda7).

### P2-S3-Upload — done
All 6 fixes applied and committed (9dc9729), merged to main (a4f9d77).

### P2-Digest-Cache — review dispatched
Awaiting review results.

### P1-Tracer-Loop — review dispatched
Awaiting review results. CRITICAL PATH — blocks Wave 3.
