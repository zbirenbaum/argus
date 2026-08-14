# Agent Controls

## Pause/Resume

Supervisor controls when every traced process resumes. A paused agent consumes zero CPU: the kernel holds each process stopped. Withholding PTRACE_CONT is only half of it — see [Freeze mechanics](#freeze-mechanics).

```
POST /agent/pause   → Freeze all. Returns when all stopped.
POST /agent/resume  → Resume all. Returns immediately.
GET  /agent/status  → running | paused | partially_paused + process list
```

### Freeze mechanics

Withholding PTRACE_CONT only holds a process that is already stopped at a syscall. A tracee doing arithmetic, or blocked in a syscall the filter does not trap, keeps running — so pause cannot wait for it to trap.

Freezing is therefore active: the supervisor sends `PTRACE_INTERRUPT` to every live tracee and confirms each one reached a stopped state (`/proc/<pid>/stat` state `t` or `T`) before the pause call returns. The tracee currently held by the pipeline at a syscall stop counts as already stopped.

The resulting interrupt-stops are deliberately **not reaped**. The kernel keeps each process stopped at zero CPU until the ptrace thread collects the notification, which it cannot do while the pause flag is set. There is no separate "thaw" step: on resume the queued stops flow through the normal pipeline path and are resumed like any other passthrough.

Freeze requests are executed on the ptrace thread — ptrace requests are only valid from the thread that attached. When no stop is in flight that thread is blocked in `waitpid`, so a freeze request is accompanied by a signal that interrupts the wait (`EINTR`) and makes it drain pending requests.

**Status semantics.** `running` = no pause requested. `paused` = pause requested and no traced process is running. `partially_paused` = pause requested but at least one tracee is still running — a process in an uninterruptible wait, for example. The process list carries `{pid, binary, state}` per tracee.

### Tracee lifetime

Tracees outlive the supervisor: ptrace detaches on tracer exit, it does not kill. Killing the supervisor therefore leaves the agent's processes running, which matters for cleanup in tests and orchestration.

## Rules

Rules control what traced processes can do. Two types share the same hook point (syscall entry in the ptrace loop) and the same matching logic. Evaluated on every syscall stop, sequentially — the ptrace loop is single-threaded, so no concurrent access.

### Rule Types

**Block rules** — instant deny, no approval prompt. The syscall gets EPERM injected immediately and a `blocked` event is emitted. Use for hard security boundaries.

**Pause-before-action rules** — freeze the agent, ask the approver chain for a verdict, and act on it. An escalation reaches the operator as a `pending_approval` on the approvals API. Approve resumes normally; deny injects EPERM.

A match freezes **every** traced process, not only the one that made the syscall (same mechanism as `POST /agent/pause`). The calling tracee is already held at its syscall-entry stop; its siblings are not, and a sibling left running could complete the very action under review while the verdict is outstanding.

### Evaluation Order

1. Block rules evaluate first (highest priority)
2. Pause-before-action rules evaluate second
3. No match → allow (syscall proceeds normally)

First matching rule wins within each category.

### Configuration

```yaml
block:
  - type: read
    paths: ["*.env", "*.key", "*.pem", "*.credentials"]
    action: deny
  - type: write
    paths: ["/etc/passwd", "/etc/shadow"]
    action: deny
  - type: exec
    binaries: ["rm -rf /"]
    action: deny

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

### Hot Reload

Rules are hot-reloadable via API. No restart required, no downtime.

**Implementation:** Rules live in `Arc<ArcSwap<RuleSet>>`. The API handler builds and validates a new `RuleSet`, then calls `rules.store(Arc::new(new_rules))`. The ptrace loop calls `rules.load()` on each syscall stop — it sees either the old set or the new set, never a partial update. Zero locking, zero contention.

A `rules_updated` event is emitted on every change so the log shows when rules changed relative to other events.

### Rules API

```
GET  /rules                → Current active ruleset
POST /rules                → Replace entire ruleset (validates, swaps atomically)
  Body: {block: [...], pause_before: [...]}
  → {applied: true, rule_count: 5}
DELETE /rules/{index}      → Remove single rule, swap
  → {applied: true, rule_count: 4}
```

### Decision Flow (block)

1. Tracee stops at syscall entry
2. Check block rules against syscall + path/binary
3. If match: inject EPERM, emit `blocked` event, resume tracee
4. Process sees "permission denied" — no approval queue involved

### Decision Flow (pause-before-action)

1. Tracee stops at syscall entry (no block rule matched)
2. Check pause-before-action rules against syscall + path/binary
3. If match: freeze every traced process (see [Freeze mechanics](#freeze-mechanics)) and mint an `action_id` for the decision
4. Walk the approver chain with an `ApprovalRequest` carrying that `action_id`
5. `Allow` → emit `approval_granted`, resume normally
6. `Deny` → emit `approval_denied`, inject EPERM
7. `Escalate` (including "no approvers configured") → emit `pending_approval` under the same `action_id`, publish it on `/approvals/pending`, and wait for `POST /approvals/{action_id}/approve` or `/deny`. The agent stays frozen for the whole wait.

The chain is consulted on a blocking thread, not on the ptrace loop. Nothing resumes the tracee until a verdict lands, so a slow judge costs latency, never correctness.

### Approvals API

```
GET  /approvals/pending                  → List pending actions
POST /approvals/{action_id}/approve      → Allow syscall
POST /approvals/{action_id}/deny         → Inject EPERM
```

### Events

Block events:
```json
{"type": "blocked", "pid": 42, "syscall": "read", "path": "/workspace/.env", "rule": "*.env"}
```

Rules change events:
```json
{"type": "rules_updated", "block_count": 3, "pause_before_count": 4, "source": "api"}
```

### WebSocket (planned)

Not implemented. The supervisor serves a single `ws://127.0.0.1:9090/ws` event stream; there is no approvals-specific socket and no client→server decision channel yet. Approve/deny is REST-only.

```
ws://127.0.0.1:9090/ws/approvals
  Server→Client: {"action_id":"a1b2","pid":42,"syscall":"unlink","path":"/workspace/important.txt"}
  Client→Server: {"action_id":"a1b2","decision":"approve"}
```

## Approver Interface

The approval mechanism is pluggable via the `Approver` trait. When a pause-before-action rule matches, the supervisor walks an ordered escalation chain of approvers until one returns a terminal verdict.

### Trait

```rust
pub trait Approver: Send + Sync {
    fn judge(&self, request: &ApprovalRequest) -> anyhow::Result<Verdict>;
    fn name(&self) -> &str;
}
```

Sync by design — the ptrace loop holds a tracee frozen at syscall entry. Implementations that need async I/O (webhooks, LLM APIs) block internally.

### Verdict

```rust
pub enum Verdict {
    Allow  { reason: Option<String>, approver: String },
    Deny   { reason: Option<String>, approver: String },
    Escalate { reason: Option<String>, approver: String },
}
```

### Escalation Chain

Approvers are evaluated in config order. First non-`Escalate` verdict wins.

1. If an approver returns `Allow` or `Deny` → that's the final decision, chain stops.
2. If an approver returns `Escalate` → log it, move to the next approver.
3. If an approver returns an error → treat as implicit escalation, move to next.
4. If the chain is **exhausted** → escalate to the human approval API.

**Exhausted** means the chain produced no terminal verdict: every configured approver returned `Escalate` or failed with an error, or no approvers are configured at all. It is not a decision — it means nobody in the chain was willing to decide.

The human approval API (`/approvals/pending` + approve/deny) is the supervisor's terminal backstop and sits **outside** the configured chain. Exhaustion routes there, the agent stays frozen, and the wait is unbounded — the backstop blocks until a human decides. Denying on exhaustion instead would mean that configuring any judge silently disables human approval; escalating keeps the backstop reachable no matter what the chain does.

Two entry points express this:

- `Approvers::judge` — returns `Deny { approver: "system:chain-exhausted" }` on exhaustion. For callers whose chain carries its own terminal backstop.
- `Approvers::judge_or_escalate` — returns `Escalate { approver: "system:chain-exhausted" }` on exhaustion. This is what the policy gate calls, so exhaustion reaches the operator.

Both are fail-closed: neither can turn an exhausted chain into an `Allow`.

### Planned Approver Implementations

None of these exist yet. The chain is injectable — `PolicyGate::with_approvers` — and is exercised in tests with stub judges, but nothing constructs a non-empty chain at runtime, so today every pause-before-action match escalates to the human backstop. The human path itself is built into the gate rather than being a chain member.

1. **LLM Judge** — HTTP call to an LLM API with the `ApprovalRequest` as context. Returns `Allow`/`Deny` if confident, `Escalate` if confidence is below threshold.
2. **Webhook** — POST to a configured URL, wait for response. Supports push notifications, Slack bots, PagerDuty, etc.
3. **Email/SMS** — Notification with approve/deny links. Polls for response or uses callback URL.

### Configuration (planned)

Not implemented — `SupervisorConfig` has no `approvers` field yet, so this block is a design target, not something the supervisor reads. Approver order in config is the escalation chain order, automated judges first. The human backstop needs no entry: it is always last.

```yaml
approvers:
  - type: llm
    endpoint: https://api.anthropic.com/v1/messages
    model: claude-sonnet-4-20250514
    confidence_threshold: 0.8
    timeout: 30s
  - type: webhook
    url: https://hooks.example.com/approvals
    timeout: 60s
```
