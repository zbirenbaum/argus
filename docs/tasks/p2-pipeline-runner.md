# Task: Pipeline Runner and Main.rs Migration

**Status**: in progress

**Spec reference**: `docs/spec-r1/01-supervisor.md` — startup sequence and ptrace loop; pipeline stage wiring

## What was done

- Created `crates/argus/src/pipeline/` module with full scaffold:
  - `mod.rs` — `RecordBus`, `PtraceStream`, `RawStopRecorder`, `Record`, `Sink` trait (stubs)
  - `runner.rs` — `PipelineRunner` struct + `run()` async loop
  - `classified.rs` — `RawStop`, `ClassifiedStop`, `CapturedStop`, `Classification` (stubs)
  - `directive.rs` — `PipelineDirective` enum (stubs)
  - `stages.rs` — all six stage stubs: `ClassifyStage`, `CheckRulesStage`, `ApprovalStage`, `CaptureStage`, `TreeStage`, `StampStage`
  - `sinks.rs` — all five sink stubs: `LocalCasSink`, `EventLogSink`, `IndexSink`, `BroadcastSink`, `RemoteCasSink`
- Added `pub mod pipeline;` to `crates/argus/src/lib.rs`
- Added `futures = "0.3"` to `crates/argus/Cargo.toml` (for `StreamExt`)
- Rewrote `crates/supervisor/src/main.rs`:
  - Removed: `mod event_sink`, `mod event_writer`, `mod pipeline_sink`, `mod stdout_sink`
  - Removed: `TracerLoop`, channel-based event plumbing, `build_sinks()`, `shutdown_pipeline_sinks()`, `api_event_bridge` thread
  - Added: `async_main()`, `build_bus()`, `bus_sender()`, `PipelineRunner` construction and `.run().await`
  - Preserved: CLI parsing, `load_config()`, `init_tracing()`, all `tracing::event!` calls, TLS/proxy setup, `spawn_drain_thread()`, signals, API server
- Updated `crates/argus/src/tracer/mod.rs`: removed `trace_loop`, `content_capture`, `handlers`, `process_events` declarations and `pub use trace_loop::TracerLoop`
- Updated `crates/argus/src/storage/mod.rs`: removed `pub mod pipeline` and `pub use pipeline::StoragePipeline`
- Deleted files:
  - `crates/argus/src/tracer/trace_loop.rs`
  - `crates/argus/src/tracer/content_capture.rs`
  - `crates/argus/src/tracer/process_events.rs`
  - `crates/argus/src/tracer/handlers/` (entire directory)
  - `crates/argus/src/storage/pipeline.rs`
  - `crates/argus/src/storage/pipeline_tests.rs`
  - `crates/argus/src/storage/pipeline_integration_test.rs`
  - `crates/argus/src/tracer/content_capture_tests.rs`
  - `crates/supervisor/src/event_sink.rs`
  - `crates/supervisor/src/event_writer.rs`
  - `crates/supervisor/src/pipeline_sink.rs`
  - `crates/supervisor/src/stdout_sink.rs`

## What works

- `PipelineRunner` struct and `run()` loop compile with stage/sink stubs
- `main.rs` compiles with pipeline imports (pending build verification in container)
- Module structure is clean — no dangling references to deleted types

## What's missing

All `pipeline/` types are stubs — parallel agents must replace them:
- `PtraceStream::spawn()` — real ptrace loop integration
- `ClassifyStage::classify()` — fd table lookup, syscall classification
- `CheckRulesStage` — real rule evaluation
- `ApprovalStage::process()` — operator approval flow
- `CaptureStage::capture()` — memory reads, CAS writes
- `TreeStage::update()` — Merkle tree mutation
- `StampStage::stamp()` — real event construction
- All five `Sink::handle()` implementations
- `RecordBus::emit()` — real fan-out
- `RecordBus::legacy_sender()` — real mpsc adapter (TLS watcher migration)
- `RawStopRecorder` — disk serialization

## How to test

```bash
docker exec argus-arm64 cargo build --target aarch64-unknown-linux-musl -p supervisor
docker exec argus-arm64 cargo test --target aarch64-unknown-linux-musl -p argus -p supervisor
```

**Branch**: `spec-r1`
