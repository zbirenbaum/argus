# Approver Interface

**Status**: done
**Spec reference**: `docs/spec/06-agent-controls.md` (Approver Interface section)

## What was done

- **`approver/mod.rs`**: `Approver` trait (sync `judge` + `name`), `DynApprover` wrapper (hides `Arc<dyn Approver>` per M-AVOID-WRAPPERS), `Approvers` escalation chain.
- **`approver/request.rs`**: `ApprovalRequest` struct with all syscall context fields, serde support.
- **`approver/verdict.rs`**: `Verdict` enum with `Allow`, `Deny`, `Escalate` variants. Each carries optional reason + approver identity.
- **`approver/policy.rs`**: `walk_chain()` — walks approvers in order, first non-Escalate verdict wins, errors treated as implicit escalations.
- **`lib.rs`**: Registered `approver` module.
- **Spec**: Updated `06-agent-controls.md` with escalation chain documentation.

## Design

Escalation chain, not fan-out. Approvers evaluated in config order:
1. `Allow`/`Deny` → terminal, chain stops
2. `Escalate` → log, continue to next approver
3. Error → implicit escalation, continue
4. All escalate → deny with `system:chain-exhausted`

Last approver should be terminal backstop (human API endpoint).

## What works

- Sync `Approver` trait — ptrace loop calls directly, no async runtime needed
- `Verdict` enum with `Allow`/`Deny`/`Escalate` + reason + approver identity
- `DynApprover` for runtime-polymorphic collections
- `Approvers::judge()` walks escalation chain
- Error handling: errors treated as implicit escalations
- Empty chain → allow (prevents indefinite blocking)
- 28 tests covering chain behavior, verdict serde, edge cases

## What's missing

- Concrete `Approver` implementations (ApiApprover, LlmApprover, WebhookApprover)
- Config deserialization for `approvers:` YAML section
- Timeout wrapper for slow approvers

Chain wiring is done: `PolicyGate::with_approvers` consults the chain on
every pause-before-action match and maps verdicts onto resume / EPERM /
human backstop. See [p2-verdict-freeze.md](p2-verdict-freeze.md). Until
a concrete approver exists there is nothing to configure, so a deployed
supervisor still escalates every match to the human API.

## How to test

```bash
docker exec argus-x86 cargo test -p argus approver
```

## Branch

main (direct)
