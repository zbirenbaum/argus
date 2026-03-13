# Task: Rewire Runtime to DurabilityLayer + OutputList

**Status**: done

**Spec reference**: `docs/spec-r1/` (enriched output pipeline refactor)

## What was done

- `crates/argus/src/pipeline/stages/capture.rs` — replaced `bus: RecordBus` field with `durability: DurabilityLayer`; updated `emit_content` and `hash_and_emit` to call `durability.persist_with_hash` + `durability.upload_async` instead of `bus.emit(Record::Content/Manifest)`
- `crates/argus/src/pipeline/stages/tree.rs` — replaced `bus: RecordBus` with `durability: DurabilityLayer`; checkpoints now persist directly via `DurabilityLayer` instead of emitting `Record::Checkpoint` to the bus
- `crates/argus/src/pipeline/runner.rs` — added `outputs: OutputList` and `redact: RedactStage` fields; after `stamp`, events are redacted then delivered to `outputs` (user-facing) before the internal bus receives them; same pattern for `stamp_blocked`; `outputs.shutdown()` called before `bus.shutdown_all()`
- `crates/argus/src/runtime.rs` — added `durability`, `outputs`, `redact` fields to `SupervisorRuntime`; added `build_outputs()` function that constructs `OutputList` from `config.outputs`; removed `StdoutSink` from `build_bus` (replaced by `StdoutOutput` in `OutputList`); `emit_agent_start` and `emit_initial_state` changed to `&mut self` so they can emit to `outputs` before the bus; `into_pipeline` passes `DurabilityLayer` to `CaptureStage` and a tree-specific `DurabilityLayer` to `TreeStage`
- `crates/supervisor/src/wiring.rs` — changed `runtime` binding to `mut` to accommodate new `&mut self` methods

## What works

- CAS content writes from CaptureStage go through DurabilityLayer (local persist + async upload)
- Checkpoints from TreeStage go through DurabilityLayer
- All events (AgentStart, InitialFile, InitialState, file events, blocked events) are redacted then delivered to OutputList before the internal bus
- StdoutSink removed from bus; StdoutOutput in OutputList receives enriched/redacted events
- FileOutput and fallback warnings for UnixSocket/Http
- RecordBus retained for EventLogSink, IndexSink, BroadcastSink, LocalCasSink, RemoteCasSink

## What's missing

- UnixSocket and Http outputs not implemented (warn and skip)
- Tree-stage DurabilityLayer does not share upload pool (local-only for checkpoints); a future task could wire in the pool

## How to test

```
docker exec -w /build/.claude/worktrees/enriched-output-pipeline argus-arm64 cargo build --target aarch64-unknown-linux-musl -p supervisor
docker exec -w /build/.claude/worktrees/enriched-output-pipeline argus-arm64 cargo test --target aarch64-unknown-linux-musl -p argus
docker exec -w /build/.claude/worktrees/enriched-output-pipeline argus-arm64 ./tests/validate.sh
```

Tests 1–7b pass. Tests 8/9/10 are pre-existing failures unrelated to this task.

**Branch**: `worktree-enriched-output-pipeline`
