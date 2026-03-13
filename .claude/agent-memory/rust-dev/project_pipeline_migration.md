---
name: pipeline_migration_state
description: State of the pipeline migration from TracerLoop+EventSink to PipelineRunner+RecordBus — which types are stubs vs real
type: project
---

The argus supervisor codebase is mid-migration from a channel-based architecture (TracerLoop, EventSink, mpsc channel) to a pipeline architecture (PipelineRunner, RecordBus, Sink trait).

**Why:** Parallel agents are implementing individual pipeline stages and sinks in the `spec-r1` branch worktrees.

**How to apply:** When editing `crates/argus/src/pipeline/`, all types in `mod.rs`, `classified.rs`, `directive.rs`, `stages.rs`, `sinks.rs` are stubs marked with `// TODO: replace`. Do not treat them as final.

## What's a stub (pipeline/ module)
- `RecordBus`, `PtraceStream`, `RawStopRecorder`, `Record`, `Sink` in `pipeline/mod.rs`
- All stage types in `pipeline/stages.rs`: `ClassifyStage`, `CheckRulesStage`, `ApprovalStage`, `CaptureStage`, `TreeStage`, `StampStage`
- All sink types in `pipeline/sinks.rs`: `LocalCasSink`, `EventLogSink`, `IndexSink`, `BroadcastSink`, `RemoteCasSink`
- `RawStop`, `ClassifiedStop`, `CapturedStop`, `Classification` in `pipeline/classified.rs`
- `PipelineDirective` in `pipeline/directive.rs`

## What's real
- `PipelineRunner` in `pipeline/runner.rs` — the wiring loop is complete
- `supervisor/src/main.rs` and `supervisor/src/wiring.rs` — startup sequence is complete
- `tracer/` module now only has: `memory`, `pending`, `regs`, `seccomp`, `syscall_nr` — no trace_loop, no handlers, no process_events

## Deleted types
- `TracerLoop` (was in `tracer/trace_loop.rs`)
- `StoragePipeline` (was in `storage/pipeline.rs`)
- `EventSink` trait, `StdoutSink`, `PipelineSink`, event_writer thread (were in supervisor/)
- `ContentCapture` (was in `tracer/content_capture.rs`)
- `handlers/` module entirely

## Key wiring
- `wiring::run()` is the async entry point — `main()` calls `rt.block_on(wiring::run(...))`
- TLS watcher still uses `mpsc::Sender<Event>` via `bus.legacy_sender()` shim (migration pending)
- API shutdown channel is now properly threaded through `init_api_server()` → `shutdown()`
