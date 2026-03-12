# Agent Controls

## Pause/Resume

Supervisor controls when every traced process resumes. Pausing = stop calling PTRACE_CONT. Kernel keeps processes stopped, zero CPU.

```
POST /agent/pause   → Freeze all. Returns when all stopped.
POST /agent/resume  → Resume all. Returns immediately.
GET  /agent/status  → running | paused | partially_paused + process list
```

## Rules

Rules control what traced processes can do. Two types share the same hook point (syscall entry in the ptrace loop) and the same matching logic. Evaluated on every syscall stop, sequentially — the ptrace loop is single-threaded, so no concurrent access.

### Rule Types

**Block rules** — instant deny, no approval prompt. The syscall gets EPERM injected immediately and a `blocked` event is emitted. Use for hard security boundaries.

**Pause-before-action rules** — hold the process, emit `pending_approval`, wait for operator decision via API/WebSocket. Approve resumes normally; deny injects EPERM.

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
3. If match: emit `pending_approval` event, notify via API/WebSocket
4. Wait for `POST /approvals/{action_id}/approve` or `/deny`
5. Approve → resume normally. Deny → inject EPERM

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

### WebSocket

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
4. If every approver escalates (or errors) → deny with `system:chain-exhausted`.

The last approver should be a terminal backstop that never escalates (e.g. the human API endpoint, which blocks until a human decides).

### Planned Approver Implementations

1. **API Approver** — existing REST/WebSocket endpoint (`POST /approvals/{id}/approve|deny`). Human operator via CLI or dashboard. Always terminal — blocks until a human decides.
2. **LLM Judge** — HTTP call to an LLM API with the `ApprovalRequest` as context. Returns `Allow`/`Deny` if confident, `Escalate` if confidence is below threshold.
3. **Webhook** — POST to a configured URL, wait for response. Supports push notifications, Slack bots, PagerDuty, etc.
4. **Email/SMS** — Notification with approve/deny links. Polls for response or uses callback URL.

### Configuration

Approver order in config is the escalation chain order. Automated judges first, human backstop last.

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
  - type: api
```
