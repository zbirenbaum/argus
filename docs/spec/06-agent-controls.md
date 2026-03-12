# Agent Controls

## Pause/Resume

Supervisor controls when every traced process resumes. Pausing = stop calling PTRACE_CONT. Kernel keeps processes stopped, zero CPU.

```
POST /agent/pause   → Freeze all. Returns when all stopped.
POST /agent/resume  → Resume all. Returns immediately.
GET  /agent/status  → running | paused | partially_paused + process list
```

## Pause-Before-Action Rules

Hook point is in the supervisor main loop (see `01-supervisor.md`). Rule check runs on every syscall entry. Phase 1: stub that returns "allow." The hook shape must exist from day one.

### Configuration

```yaml
pause_before:
  - type: unlink
    paths: ["/workspace/**"]
  - type: exec
    binaries: ["rm", "curl", "wget"]
  - type: write
    paths: ["*.env", "*.key", "*.pem"]
  - type: connect
    destinations: ["*:22", "*:25"]
```

### Decision Flow

1. Tracee stops at syscall entry (already stopped — free)
2. Check rules against syscall + path/binary
3. If match: emit `pending_approval` event, notify via API
4. Wait for `POST /approvals/{action_id}/approve` or `/deny`
5. Approve → resume normally. Deny → inject EPERM (process sees "permission denied")

### API

```
GET  /approvals/pending                  → List pending actions
POST /approvals/{action_id}/approve      → Allow syscall
POST /approvals/{action_id}/deny         → Inject EPERM
```

### WebSocket

```
ws://127.0.0.1:9090/ws/approvals
  Server→Client: {"action_id":"a1b2","pid":42,"syscall":"unlink","path":"/workspace/important.txt"}
  Client→Server: {"action_id":"a1b2","decision":"approve"}
```
