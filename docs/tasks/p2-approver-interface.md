# Approver Interface

**Status**: done
**Spec reference**: `docs/spec/06-agent-controls.md` (Approver Interface section)

## What was done

- **`approver/mod.rs`**: `Approver` trait (sync `judge` + `name`), `DynApprover` wrapper (hides `Arc<dyn Approver>` per M-AVOID-WRAPPERS), `Approvers` collection with policy-based fan-out.
- **`approver/request.rs`**: `ApprovalRequest` struct with all syscall context fields, serde support.
- **`approver/verdict.rs`**: `Verdict` struct (allow/deny + optional reason + approver identity), `Decision` enum.
- **`approver/policy.rs`**: `ApprovalPolicy` enum (FirstResponse, Unanimous, AnyAllow), `evaluate()` dispatcher, per-policy combinators.
- **`lib.rs`**: Registered `approver` module.
- **Spec**: Updated `06-agent-controls.md` with approver interface documentation.

## What works

- Sync `Approver` trait — ptrace loop calls directly, no async runtime needed
- `DynApprover` for runtime-polymorphic collections
- `Approvers::judge()` with three fan-out policies
- Error handling: failing approvers skipped (first_response/any_allow) or treated as deny (unanimous)
- Empty approvers → allow (prevents indefinite blocking)
- 21 new tests (453 total, all passing)

## What's missing

- Concrete `Approver` implementations (ApiApprover, LlmApprover, WebhookApprover)
- Wiring `Approvers` into `TracerLoop` and `SupervisorState`
- Config deserialization for `approvers:` YAML section
- Timeout wrapper for slow approvers

## How to test

```bash
docker exec argus-x86 cargo test -p argus approver
```

## Branch

main (direct)
