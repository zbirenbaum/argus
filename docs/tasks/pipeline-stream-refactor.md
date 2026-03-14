# Pipeline Stream Refactor

**Status**: in progress

**Spec reference**: `docs/superpowers/plans/2026-03-12-pipeline-refactor.md`, `~/.claude/plans/playful-mixing-stream.md`

## What was done

### Phase 1: Foundation
- Added `resume()` and `inject_error()` convenience methods to `PtraceHandle`
- Added `ClassifyStage::process()` — stream-compatible classification that resumes passthroughs internally
- Added `CaptureStage::process()` — captures content and resumes tracee internally
- Added `TreeStage::process()` — returns `(CapturedEvent, Option<ContentHash>)`
- Added `KeylogWatcher::poll_new_lines()` — bus-free method returning data for pipeline persistence
- Added `FlowWatcher::poll_new_flows()` and `flow_parser::process_flow_detached()` — bus-free flow processing

### Phase 2: PolicyGate
- Created `stages/policy_gate.rs` with unified `PolicyGate` stage
- `PolicyOutcome` enum: `Approved(ClassifiedEvent)` / `Blocked { pid, syscall, path, reason }`
- Evaluates hot-swappable `ArcSwap<RuleSet>` for block/pause-before-action rules
- Block: injects EPERM, returns `Blocked` for caller to record
- AskUser: emits `PendingApproval` event, waits for human decision via oneshot channel
- 6 unit tests covering no-rules, block, pause-approve, pause-deny, passthrough, hot-swap

### Phase 3: Stream Sources
- Created `pipeline/streams/` module with `KeylogStream` and `ProxyStream`
- Both implement `futures::Stream` with `tokio::time::Interval` polling
- Cancellation via `tokio_util::sync::CancellationToken`
- 4 unit tests covering data yield and cancellation

### Phase 4: Rewrite Ptrace Pipeline
- Rewrote `PipelineRunner` to use new stage methods
- Removed `CheckRulesStage` and `ApprovalStage` from runner struct
- Flow: `classify.process()` → `policy_gate.evaluate()` → `capture.process()` → `tree.process()` → `stamp` → `redact` → outputs → bus
- Passthrough resume, tracee resume, and policy enforcement are now internal to stages

### Phase 5: Rewrite Non-Ptrace Pipelines
- Rewrote `keylog_pipeline.rs` as async function using `KeylogStream`
- Rewrote `proxy_pipeline.rs` as async function using `ProxyStream`
- Changed `spawn_keylog_pipeline` / `spawn_proxy_pipeline` to use `tokio::spawn`
- Shutdown via `CancellationToken` instead of `AtomicBool` stop flag
- Updated supervisor `wiring.rs` for new return types

### Dependencies
- Added `tokio-util = "0.7"` to workspace and argus/supervisor crates
- Made `net::flow_parser` module `pub(crate)` (was private)
- Added `Clone` derive to `FlowContent`

## What works
- All 656 unit tests pass
- All 14 supervisor tests pass
- Validation tests 1-8 pass (basic tracing, stdio, file ops, pipes, subprocess, escape, write locking, TLS)

### Runner Stream Composition
- Rewrote `PipelineRunner::run()` as two composed stream stages:
  - `futures::stream::unfold(CoreState)` — threads all core pipeline state through each iteration, yields `Event` values
  - `StreamExt::fold(OutputState)` — consumes events, threads output state by value (redact → outputs → bus with retry)
- No `Arc<Mutex<>>`. `unfold` owns core state; `fold` owns output state. Natural backpressure.

### Validation Unit Tests (Phase 7)
- Created `pipeline/validation_tests.rs` with 9 tests covering behaviors of validation tests 9-13
- Test 9: pause flag semantics, `shared.emit()` bus routing (documents stdout gap)
- Test 10: PolicyGate pause-before-action approval flow (deny → Blocked/EPERM, approve → Approved)
- Test 11: TreeBuilder + snapshot storage via SharedState
- Test 12: Initial workspace walk emits InitialFile + InitialState events
- Test 13: Fork/exit sequence completeness, pid symmetry (zombie-free invariant)

### Test 8 strictness
- Updated `tests/validate.sh` test 8 to FAIL (not WARN) when http_request/http_response events are missing

## What's missing
- Tests 9 and 10 validation failures need root-cause fix (test 9: `shared.emit()` events bypass stdout OutputList; test 10: needs investigation)
- `approvals.rs`, `check_rules.rs` modules still exist but are dead code (kept for potential reuse)
- `RecordBus`, `Sink` trait, sinks infrastructure still used by bus output path
- `Analyze` variant in PolicyGate not yet wired to an HTTP analyzer service

## How to test
```bash
# Build
docker exec -w /build argus-arm64 cargo build --target aarch64-unknown-linux-musl -p supervisor

# Unit tests (665 pass)
docker exec -w /build argus-arm64 cargo test --target aarch64-unknown-linux-musl -p argus -p supervisor

# Validation tests 1-7 (all pass)
for i in 1 2 3 4 5 6 7; do docker exec -w /build argus-arm64 ./tests/validate.sh $i; done

# Test 8 now requires mitmdump for HTTP events
```
