# P2: Pause/Resume & Approval API

**Status**: done

**Spec reference**: `docs/spec/06-agent-controls.md`, `docs/spec/10-api-reference.md`

## Dependencies
- **Blocked by**: P1-tracer-loop, P1-events (Pause/Resume/PendingApproval events), P1-config (PauseRule)
- **Blocks**: P5-websocket-approvals

## Parallelizable with
- P2-cas, P2-s3-upload, P2-digest-cache, P2-write-locking, P2-tls-content

## What was done

- `crates/argus/src/api/` — axum server on 127.0.0.1:9090 with
  `mod.rs`, `routes.rs`, `state.rs`, `types.rs`, `errors.rs`:
  - `POST /agent/pause` — sets the paused flag *and* freezes every
    tracee, returning the list of processes it stopped
  - `POST /agent/resume` — clears the flag; queued interrupt-stops drain
    through the pipeline and resume normally
  - `GET /agent/status` — `running` / `paused` / `partially_paused`
    plus the live process list with per-process state
  - `GET /approvals/pending`, `POST /approvals/{id}/approve|deny`
- `api/state.rs` — `Bridge`, the lock-free bridge between the tokio API
  and the sync ptrace thread: atomics, `ArcSwap`, and `DashMap`, no
  `Mutex` on the hot path. Holds the tracee registry and the ptrace
  handle used to freeze.
- `pipeline/stages/policy_gate.rs` — the pause-before-action hook that
  replaced the Phase 1 stub: rule match → freeze → approver chain →
  resume / EPERM / human approval, with the decision delivered over a
  `oneshot` channel keyed by action id.

The freeze mechanics (`PTRACE_INTERRUPT`, the tracee registry, the wake
signal, errno injection) and the judge-chain wiring landed later and are
written up in [p2-verdict-freeze.md](p2-verdict-freeze.md).

## What works

- Pause stops every traced process, including ones busy computing, and
  returns only once they are stopped; resume releases them.
- Status reports the real process list and distinguishes `paused` from
  `partially_paused`.
- The approval lifecycle end to end: pending action queued, operator
  approves or denies, tracee resumed or given `EPERM`.
- Events emitted: `agent_pause`, `agent_resume`, `pending_approval`,
  `approval_granted`, `approval_denied`, `rules_updated`.

## What's missing

- WebSocket approvals (`ws://…/ws/approvals`) — REST only for now.
- No approval timeout: an escalated action holds the agent until an
  operator answers. This is deliberate (fail-closed), but there is no
  operator-facing warning that the agent is stuck.

## How to test

```bash
docker exec argus-arm64 cargo test --target aarch64-unknown-linux-musl -p argus --lib api
docker exec argus-arm64 ./tests/validate.sh 9 10 14
```

## Branch
- **Branch**: `p2-pause-resume-api`
- **Target**: `main`
