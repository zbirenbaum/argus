# Agent Controls

> `docs/spec/06-agent-controls.md` is the fuller document for this area —
> block rules, hot reload, the approver interface, and the freeze
> mechanics in detail. This file carries the same normative behaviour in
> summary form; when the two disagree, `docs/spec/` wins.

## Pause/Resume

Supervisor controls when every traced process resumes. Pausing = stop calling PTRACE_CONT. Kernel keeps processes stopped, zero CPU.

```
POST /agent/pause   → Freeze all. Returns when all stopped.
POST /agent/resume  → Resume all. Returns immediately.
GET  /agent/status  → running | paused | partially_paused + process list
```

Withholding PTRACE_CONT only holds a process already stopped at a syscall — a tracee doing pure computation keeps running. Freezing is therefore active: `PTRACE_INTERRUPT` to every live tracee, each confirmed stopped (`/proc/<pid>/stat` state `t` or `T`) before pause returns. The interrupt-stops are left unreaped, so the kernel holds each process at zero CPU; on resume they flow through the pipeline and resume normally, so there is no separate thaw step.

Freeze runs on the ptrace thread (ptrace requests are only valid from the attaching thread). When no stop is in flight that thread is blocked in `waitpid`, so a freeze request is accompanied by a signal that interrupts the wait.

`partially_paused` means a pause was requested but some tracee is still running — an uninterruptible wait, for example.

## Pause-Before-Action Rules

Hook point is in the supervisor main loop (see `01-supervisor.md`). Rule check runs on every syscall entry.

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
3. If match: freeze **every** traced process, not just the caller — otherwise a sibling could complete the action under review while the verdict is outstanding
4. Walk the approver chain for a verdict
5. `Allow` → emit `approval_granted`, resume normally
6. `Deny` → emit `approval_denied`, inject EPERM (process sees "permission denied")
7. `Escalate`, or an **exhausted** chain — no approver reached a terminal verdict, whether by escalating, erroring, or not being configured at all — → emit `pending_approval`, notify via API, and wait for `POST /approvals/{action_id}/approve` or `/deny`. The agent stays frozen for the whole wait; there is no timeout.

The human approval API is the terminal backstop and sits outside the configured chain, which is why exhaustion escalates rather than denying: denying would mean that configuring any judge silently disables human approval. Nothing turns exhaustion into an `Allow`.

### API

```
GET  /approvals/pending                  → List pending actions
POST /approvals/{action_id}/approve      → Allow syscall
POST /approvals/{action_id}/deny         → Inject EPERM
```

### WebSocket (planned)

Not implemented — the supervisor serves one `ws://127.0.0.1:9090/ws` event stream and approve/deny is REST-only.

```
ws://127.0.0.1:9090/ws/approvals
  Server→Client: {"action_id":"a1b2","pid":42,"syscall":"unlink","path":"/workspace/important.txt"}
  Client→Server: {"action_id":"a1b2","decision":"approve"}
```
