# Pipeline Architecture Refactor

**Status**: done
**Branch**: spec-r1

## Spec reference

- `docs/superpowers/plans/2026-03-12-pipeline-refactor.md`
- `docs/mvp.md` (event pipeline architecture)

## What was done

- **Sink trait refactor**: Changed `Sink` methods (`write`, `flush`, `shutdown`) from `&self` to `&mut self`. Dropped `Sync` bound (bus Mutex provides it). Pushed `Mutex` from individual sinks to `RecordBus`.
- **PipelineContext**: New shared context struct (`seq: Arc<SequenceGenerator>`, `bus: RecordBus`, `agent_id: String`) cloned into each pipeline.
- **Keylog pipeline**: Extracted from monolithic `tls_watcher_loop` into independent `keylog_pipeline::run()` with stop flag and final drain.
- **Proxy pipeline**: Extracted from monolithic `tls_watcher_loop` into independent `proxy_pipeline::run()` with stop flag and final drain.
- **Runtime wiring**: `SupervisorRuntime` uses `PipelineContext`. Two independent spawn methods replace `spawn_tls_watcher`. Eliminated `TLS_SEQ_START` hack — single shared `Arc<SequenceGenerator>` provides total ordering.
- **Debug logging**: Structured tracing throughout classify, capture, tree, stamp, bus, and runner stages.
- **Race fix**: ptrace seize now completes before sync pipe is closed, preventing child from executing ahead of tracing.
- **Proxy thread optimization**: No thread spawned when mitmdump is not running.

## Files added/changed

| File | Change |
|-|-|
| `crates/argus/src/pipeline/context.rs` | New: PipelineContext |
| `crates/argus/src/pipeline/keylog_pipeline.rs` | New: independent keylog pipeline |
| `crates/argus/src/pipeline/proxy_pipeline.rs` | New: independent proxy pipeline |
| `crates/argus/src/pipeline/mod.rs` | Register new modules |
| `crates/argus/src/pipeline/sink.rs` | &mut self, drop Sync |
| `crates/argus/src/pipeline/bus.rs` | Arc<Mutex<dyn Sink>>, poison handling, trace logging |
| `crates/argus/src/pipeline/sinks/*` | Remove internal Mutexes, update signatures |
| `crates/argus/src/pipeline/runner.rs` | Debug logging |
| `crates/argus/src/pipeline/stages/stamp.rs` | Arc<SequenceGenerator>, debug logging |
| `crates/argus/src/pipeline/stages/capture.rs` | Debug logging |
| `crates/argus/src/pipeline/stages/tree.rs` | Debug logging |
| `crates/argus/src/pipeline/ptrace_thread.rs` | Seize-ready oneshot channel |
| `crates/argus/src/runtime.rs` | PipelineContext, spawn methods, removed TLS watcher code |
| `crates/supervisor/src/wiring.rs` | Two pipeline handles, seize-before-release |

## What works

- All 550 unit tests pass
- All 13 validation tests pass
- Three independent pipelines (ptrace, keylog, proxy) share a single SequenceGenerator
- Orderly shutdown: TLS pipelines → mitmdump → API → ptrace
- No thread spawned when mitmdump is not running

## What's missing

Nothing. All planned tasks completed.

## Plan deviations

- `PipelineContext` omits `cas: Arc<dyn Cas>` field — CAS access goes through bus sinks, no pipeline needs direct access. Signed off by user.

## How to test

```bash
# Unit tests
docker exec argus-arm64 cargo test --target aarch64-unknown-linux-musl -p argus -p supervisor

# All 13 validation tests
docker exec argus-arm64 ./tests/validate.sh
```
