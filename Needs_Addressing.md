# Needs Addressing

Issues discovered during the event consumer work. All marked with `FIXME(event-consumer)` in source.

## 1. Disk polling is wasteful — replace with file watching

**Files:** `crates/argus-api/src/ingest.rs`, `crates/argus-api/src/replay.rs`

`replay::load_from_disk` re-reads every JSONL segment file from byte 0 on every poll. INSERT OR IGNORE makes this correct but wasteful. The poll loop runs every 500ms.

**Fix:** Track per-file byte offsets so each poll only reads new lines. Better yet, replace polling entirely with inotify (Linux) / kqueue (macOS) file watching for near-zero latency.

## 2. SSE latency is bounded by poll interval

**File:** `crates/argus-api/src/routes.rs`

The SSE `/events/stream` endpoint gets events from the broadcast channel, which is fed by the disk polling loop. Dashboard latency is up to 500ms behind reality. This goes away if file watching replaces polling.

## 3. WS drain task may be unnecessary

**File:** `crates/argus-api/src/ingest.rs`

The `drain_ws` task keeps the supervisor WebSocket consumed to prevent backpressure on the ptrace pipeline. However, `BroadcastSink` uses `tokio::sync::broadcast::Sender::send`, which silently drops events when there are zero subscribers — it never blocks. If that's the case, the drain task is pure overhead and can be deleted.

**Action:** Confirm `BroadcastSink` behavior with zero subscribers, then remove `drain_ws` if safe.

## 4. Proxy pipeline drain race (not yet fixed on develop)

**Files:** `crates/argus/src/pipeline/runner.rs`, `crates/supervisor/src/wiring.rs`

`PipelineRunner::run()` calls `bus.shutdown_all()` internally before control returns to `wiring.rs`. By the time wiring cancels the proxy pipeline, the bus sinks are already torn down. Late HTTP flow events are silently dropped.

**Fix:** Return `RecordBus` from `run()`, let `wiring::shutdown()` call `bus.shutdown_all()` after the proxy pipeline has drained. Implementation exists on the `claude-cannot-debug-right` branch but was not brought to develop because it touches the argus binary.

## 5. No proof that WS events were ever being dropped

The original hypothesis — that `BroadcastSink` drops events when argus-api hasn't connected yet — was never validated with data. The supervisor writes 1582 events to disk; it's unknown how many the WS delivered. The disk-as-source-of-truth approach sidesteps the question but doesn't answer it.

**Action:** Compare disk event count vs WS-ingested count in a test run to determine if the WS path actually loses events. If it doesn't, the disk polling architecture may be unnecessary complexity.
