# Pipeline Foundation

**Status**: done
**Spec reference**: migration spec provided in task prompt
**Branch**: spec-r1

## What was done

Created the full `crates/argus/src/pipeline/` module hierarchy:

- `pipeline/mod.rs` — module declarations and re-exports
- `pipeline/raw_stop.rs` — `RawSyscallStop`, `StopType`, `SyscallArgs` (serde-capable for replay)
- `pipeline/directive.rs` — `PipelineDirective` (ptrace thread command enum)
- `pipeline/classified.rs` — `ClassifiedEvent`, `Classification`, `StdioType`, `PipeDirection`, `PtyDataType`
- `pipeline/captured.rs` — `CapturedEvent`, `CapturedContent`
- `pipeline/record.rs` — `Record` enum (Event/Content/Manifest/Checkpoint)
- `pipeline/sink.rs` — `Sink` trait, `SinkPriority`
- `pipeline/bus.rs` — `RecordBus` with blocking/async priority fanout
- `pipeline/capture_policy.rs` — `CapturePolicy`, `CaptureLevel`, `CaptureRule`, `CaptureConfig`
- `pipeline/replay.rs` — `RawStopRecorder` (JSONL writer), `ReplayStream` (futures::Stream)
- `pipeline/ptrace_thread.rs` — `PtraceStream`, `PtraceHandle`, `ptrace_thread_main`
- `pipeline/stages/mod.rs` — stage module declarations
- `pipeline/stages/classify.rs` — `ClassifyStage` (fd table management, stop classification)
- `pipeline/stages/sockaddr.rs` — sockaddr parsing/encoding helpers
- `pipeline/stages/syscall_handlers.rs` — per-syscall handlers (aarch64 + x86_64)
- `pipeline/stages/check_rules.rs` — `CheckRulesStage` (block/pause rule evaluation)
- `pipeline/stages/approvals.rs` — `ApprovalStage` (approver chain integration)
- `pipeline/stages/capture.rs` — `CaptureStage` (content read, chunking, CAS emission)
- `pipeline/stages/stamp.rs` — `StampStage` (Classification → EventPayload mapping)
- `pipeline/stages/tree.rs` — `TreeStage` (Merkle tree updates, checkpoint emission)

Modified:
- `crates/argus/src/lib.rs` — added `pub mod pipeline;`
- `crates/argus/Cargo.toml` — added `futures = "0.3"`

## What works

- All 563 existing unit tests pass
- Pipeline module compiles cleanly for aarch64-unknown-linux-musl
- `RawSyscallStop` serializes/deserializes for replay testing
- `RecordBus` fans out to blocking and async sinks in priority order
- `CapturePolicy` rate limiting and budget tracking with tests
- `ReplayStream` implements `futures::Stream` for test replay
- `PtraceStream` implements `futures::Stream` backed by the ptrace thread
- `PtraceHandle` provides async wrappers for all memory directives
- `ClassifyStage` handles all major file, pipe, pty, and network syscalls
- Transparent connect() rewrite for TLS ports
- `StampStage` maps all Classification variants to EventPayload
- Sockaddr round-trip tests pass

## What's missing

- `CaptureConfig` needs to be wired to the actual config crate once the config agent adds the field (currently defined inline in `capture_policy.rs`)
- `CaptureStage` write-lock serialization uses per-path `DashMap<PathBuf, Mutex<()>>` — the lock is created on demand but never expires; a production implementation should evict stale locks
- `StampStage::stamp` returns `None` for `FileOpen`/`FileClose`/`Passthrough` — callers should handle this
- Transparent proxy rewrite only rewrites the sockaddr bytes; the original destination is not saved for event attribution (would need a per-(pid,fd) map similar to `connect_originals` in old tracer)
- `on_exec` reads envp as empty — production code should parse `/proc/pid/environ`
- The `TreeStage` uses `bincode::serialize` for checkpoint data — requires `bincode` dep (already present)
- No runner/orchestrator connecting the stages together (out of scope for this task)

## How to test

```bash
# Build
docker exec argus-arm64 bash -c "cd /build/.claude/worktrees/agent-abb4379c && cargo build --target aarch64-unknown-linux-musl -p argus"

# Unit tests (all 563 must pass)
docker exec argus-arm64 bash -c "cd /build/.claude/worktrees/agent-abb4379c && cargo test --target aarch64-unknown-linux-musl -p argus"
```
