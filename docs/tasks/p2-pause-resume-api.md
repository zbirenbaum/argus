# P2: Pause/Resume & Approval API

**Status**: not started

**Spec reference**: `docs/spec/06-agent-controls.md`, `docs/spec/10-api-reference.md`

## Dependencies
- **Blocked by**: P1-tracer-loop (pause = suppress PTRACE_CONT), P1-events (emit Pause/Resume/PendingApproval events), P1-config (PauseRule)
- **Blocks**: P5-websocket-approvals

## Parallelizable with
- P2-cas, P2-s3-upload, P2-digest-cache, P2-write-locking, P2-tls-content

## What needs to be done
- `crates/sandbox/src/api/mod.rs` + submodules:
  - Axum server on 127.0.0.1:9090
  - `POST /agent/pause` — set paused flag, all traced processes stop receiving PTRACE_CONT
  - `POST /agent/resume` — clear paused flag, resume all
  - `GET /agent/status` — return { paused, pid_count, event_seq }
  - `GET /approvals/pending` — list pending approval requests
  - `POST /approvals/{action_id}/approve` — resume blocked syscall
  - `POST /approvals/{action_id}/deny` — inject EPERM, resume

- Replace stub pause-before-action hook in tracer loop:
  - On syscall entry, match against PauseRule list
  - If matched: emit PendingApproval event, block tracee until approved/denied
  - Approval state: `HashMap<ActionId, oneshot::Sender<Decision>>`

- Shared state between API server (tokio) and tracer loop (sync thread):
  - Use `Arc<Mutex<TracerState>>` or crossbeam channels

## How to test
```bash
cargo test -p sandbox --lib api
```
Unit tests: pause/resume state machine, approval lifecycle.
Integration test (ignored): start supervisor, POST /agent/pause, verify processes stop, POST /agent/resume, verify continue.

## Branch
- **Branch**: `p2-pause-resume-api`
- **Target**: `main`
