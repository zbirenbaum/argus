# P2: Verdict Freeze — stopping the agent on a judge decision

**Status**: done

**Spec reference**: `docs/spec/06-agent-controls.md` (Pause/Resume, Pause-before-action, Approver Interface), `docs/spec/10-api-reference.md`

## Problem

Freezing was never wired up. A verdict — from a judge or an operator —
did not stop the supervised agent:

1. **Only the offending tracee was held.** The pipeline stops one tracee
   at the syscall it is deciding on. Its siblings kept running at full
   CPU for the whole time a verdict was outstanding. Measured: a sibling
   burned 200 jiffies in the 2 s while an unlink sat pending approval.
2. **`POST /agent/pause` stopped nothing.** It set an in-process flag,
   returned `{"stopped_processes": []}` immediately, and left every
   tracee in state `R`. The spec says "Freeze all. Returns when all
   stopped."
3. **`GET /agent/status` reported no processes** and never
   `partially_paused`, though both are in the spec.
4. **A rejected syscall did not return `EPERM`.** `InjectError` ignored
   its `errno` argument: it cancelled the syscall and left whatever was
   in the return register, so `rm` reported `ENOTTY` ("Inappropriate
   ioctl for device"). On x86_64 `set_syscall_nr` is a no-op, so the
   syscall was not even cancelled — a denial would have executed.
5. **The judge chain was unreachable.** `crates/argus/src/approver/`
   (trait, `Verdict`, escalation chain, parallel policies) had no caller
   anywhere in the tree. `PolicyGate` went straight to the human API, so
   a judge verdict of "rejected" or "needs approval" could not reach the
   ptrace loop at all. This was the known gap recorded in
   `p2-approver-interface.md`.

## What was done

**New files**

- `crates/argus/src/pipeline/tracee_registry.rs` — `TraceeRegistry`, the
  lock-free live-PID set shared by the ptrace thread and the API.
- `crates/argus/src/pipeline/freeze.rs` — `freeze_all()`
  (PTRACE_INTERRUPT + `/proc` state confirmation), `is_stopped()`,
  `proc_state()`, and the SIGUSR1 wake used to interrupt a blocking
  `waitpid`.

**Changed files**

- `pipeline/directive.rs` — added `PipelineDirective::Freeze`.
- `pipeline/ptrace_thread.rs` — `TraceeSet` is now backed by the shared
  registry; the wait loop drains directives before blocking and treats
  `EINTR` as a wake instead of a fatal error; `PtraceHandle::freeze()`;
  `inject_errno()` writes the negated errno into the return register
  (and cancels the syscall on both architectures).
- `pipeline/stages/policy_gate.rs` — `with_approvers()`; every
  pause-before-action match now freezes the agent, consults the judge
  chain off-runtime via `spawn_blocking`, and maps
  `Allow`/`Deny`/`Escalate` onto resume / EPERM / human backstop.
- `approver/mod.rs` — `Approvers::judge_or_escalate()`, which reports an
  exhausted chain as an escalation so the human API stays the backstop.
- `api/state.rs` — registry + ptrace handle on `Bridge`;
  `freeze_tracees()`, `process_list()`, `set_ptrace_handle()`.
- `api/routes.rs` — pause freezes and reports stopped processes; resume
  reports resumed PIDs; status lists processes and can report
  `partially_paused`.
- `runtime.rs` — passes the registry to the ptrace thread and publishes
  the handle to shared state.
- `tests/validate.sh` — new test 14; fixed test 10, which aborted the
  whole script on its first `curl` because `set -e -o pipefail` treats a
  not-yet-listening API as fatal (test 10 had never run to completion).

**How the freeze works.** `PTRACE_INTERRUPT` stops each tracee and the
resulting stop notification is deliberately left unreaped: the kernel
keeps the process stopped at zero CPU until the ptrace thread collects
it, and when it does the stop flows through the normal pipeline path and
is resumed like any other passthrough. There is therefore no "thaw"
step — clearing the pause flag (or finishing the verdict) is enough.

## What works

- A judge verdict of `Deny` stops the action: EPERM is injected, the
  file survives, `approval_denied` is emitted with the judge's name.
- A judge verdict of `Escalate` (or no judges configured) holds every
  traced process, queues the action on `/approvals/pending`, and keeps
  the agent stopped until an operator answers.
- A judge verdict of `Allow` resumes without asking a human.
- While any verdict is outstanding, every traced process is in state `t`
  and consumes zero CPU — verified against `/proc`.
- `POST /agent/pause` returns once all tracees are stopped and lists
  them; `GET /agent/status` lists processes with per-process state.
- A denied syscall returns `EPERM` ("Operation not permitted").
- 695 unit tests pass (681 argus + 14 supervisor); validation tests 1-14
  pass, as does `tests/repro-verdict-freeze.sh` (15 checks).

## What's missing

- **No concrete `Approver` implementations.** The chain is injectable
  (`PolicyGate::with_approvers`) and covered by tests with stub judges,
  but there is no LLM, webhook, or email approver yet and no
  `approvers:` section in the config, so a deployed supervisor still
  escalates every pause-before-action match to the human API. Adding
  those is the follow-up to this task.
- `partially_paused` is reachable but not exercised by a test — it needs
  a tracee that ignores `PTRACE_INTERRUPT` (uninterruptible sleep).
- The freeze wait polls `/proc` at 200 µs for up to 500 ms per tracee.
  Fine at current process counts; would need `waitid` batching for
  hundreds of tracees.
- The errno injection is only verified on aarch64. The x86_64 path now
  cancels the syscall via `orig_rax` and writes `rax`, which previously
  did nothing at all, but CLAUDE.md rules out the x86 container (Rosetta
  breaks ptrace) so it is unverified.

## Deviations from spec — signed off 2026-08-14, spec updated

Both were reviewed and accepted by the human user; the spec was vague on
each and now states the accepted behaviour:

1. **Pause-before-action freezes the whole agent, not just the calling
   process.** The old wording ("hold the process") did not say what
   happens to siblings, and holding only the caller leaves them free to
   complete the very action under review. `06-agent-controls.md` now
   specifies whole-agent freeze for a pause-before-action match, plus
   the freeze mechanics (`PTRACE_INTERRUPT`, unreaped interrupt-stops,
   the wake signal) under "Freeze mechanics".
2. **An exhausted judge chain escalates instead of denying.**
   `06-agent-controls.md` now defines *exhausted* — no approver reached
   a terminal verdict, whether by escalating, erroring, or not being
   configured at all — and documents the two entry points:
   `Approvers::judge` (deny on exhaustion, for chains carrying their own
   backstop) and `Approvers::judge_or_escalate` (escalate on exhaustion,
   what the policy gate calls, because the human API backstop sits
   outside the chain). Both are fail-closed: neither turns exhaustion
   into an `Allow`.

## Docs updated alongside the code

- `docs/spec/06-agent-controls.md` — freeze mechanics, status semantics,
  tracee lifetime, whole-agent freeze scope, exhaustion definition,
  decision flow with the approver chain, approver-implementation status.
- `docs/spec/10-api-reference.md` — real pause/resume/status response
  shapes, approval queue semantics, EPERM guarantee.
- `docs/spec/01-supervisor.md` — main-loop pseudocode for the rule hook.
- `docs/spec/12-testing.md` — tests 7b/13/14 added to the table and
  documented; tests 9/10 expectations updated; how to run the suite.
- `CLAUDE.md` — 14 validation tests, repro command, stray-tracee note.
- `README.md` — interception section replacing "ptrace enforcement not
  yet connected".
- `docs/tasks/STATUS.md` — refreshed validation results and test counts.

## How to test

```bash
# Unit tests (judge verdict paths, freeze primitives, registry)
docker exec argus-arm64 cargo test --target aarch64-unknown-linux-musl -p argus -p supervisor

# Reproduction — standalone, exits non-zero on any spec violation
docker exec argus-arm64 ./tests/repro-verdict-freeze.sh

# Same checks as validation test 14
docker exec argus-arm64 ./tests/validate.sh 14

# Full suite
docker exec argus-arm64 ./tests/validate.sh
```

To see the original failure, check out `main` before this change, build,
and run `tests/repro-verdict-freeze.sh`: it reports 9 spec violations.

## Branch

`p2-verdict-freeze` → `main`
