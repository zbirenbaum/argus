# Sink Resilience, Unwrap Audit & MerkleTree Path-Copy Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking. REQUIRED: Activate the ms-rust skill before writing any Rust code.

**Goal:** Make the pipeline crash-proof (no mutex poisoning, no unwraps in prod, required-sink retry with tracee freeze), add SQLite overflow for non-ptrace threads, expose stall status on the API, and replace the MerkleTree deep-clone with a persistent path-copy data structure with batched finalization.

**Architecture:** Three independent subsystems: (1) sink resilience — `parking_lot::Mutex`, `required` flag on sinks, `EmitResult` from bus, retry loop in runner that freezes tracee, SQLite overflow for non-ptrace paths, stall status on API; (2) unwrap audit — thread spawns return `Result`, all prod-path expects replaced with logged error handling; (3) MerkleTree redesign — persistent `MerkleNode` with `OnceLock` lazy hashing, `TreeBuilder` with configurable batch-size finalization, eliminates deep clone per event.

**Tech Stack:** `parking_lot` (non-poisoning mutex), `rusqlite` (overflow DB), `std::sync::OnceLock` (lazy node hashing, thread-safe), `bincode` (overflow serialization), existing `tracing` + `anyhow` + `compact_str`

**Conventions:**
- Build: `docker exec argus-arm64 cargo build --target aarch64-unknown-linux-musl -p argus -p supervisor`
- Test: `docker exec argus-arm64 cargo test --target aarch64-unknown-linux-musl -p argus -p supervisor`
- Files under 300 lines, functions under 40 lines
- No `unwrap()` or `expect()` in production paths
- Every `Arc` usage must be explicitly justified (user policy)

---

## File Structure

### New Files

| File | Responsibility |
|-|-|
| `crates/argus/src/pipeline/emit_result.rs` | `EmitResult` enum returned by `RecordBus::emit` |
| `crates/argus/src/pipeline/overflow.rs` | SQLite-backed overflow queue for failed records |
| `crates/argus/src/pipeline/stall.rs` | `StallState` struct written to Bridge during sink stalls |
| `crates/argus/src/snapshot/node.rs` | `MerkleNode` with `OnceLock<ContentHash>`, path-copy |
| `crates/argus/src/snapshot/builder.rs` | `TreeBuilder` — batched mutations, `finalize()` |
| `crates/argus/src/config/tree.rs` | `TreeConfig` — batch_size, checkpoint_interval |

### Modified Files

| File | Change |
|-|-|
| `Cargo.toml` (workspace) | Add `parking_lot`, `rusqlite` workspace deps |
| `crates/argus/Cargo.toml` | Add `parking_lot.workspace`, `rusqlite.workspace` |
| `crates/argus/src/pipeline/sink.rs` | Add `required()` method to `Sink` trait |
| `crates/argus/src/pipeline/bus.rs` | Return `EmitResult`, use overflow queue |
| `crates/argus/src/pipeline/runner.rs` | Retry loop on `EmitResult::RequiredFailed`, freeze tracee, set stall status |
| `crates/argus/src/pipeline/sinks/event_log.rs` | `parking_lot::Mutex`, recovery via `reopen()` on write failure |
| `crates/argus/src/pipeline/sinks/index.rs` | `parking_lot::Mutex` |
| `crates/argus/src/pipeline/sinks/memory.rs` | `parking_lot::Mutex` |
| `crates/argus/src/pipeline/sinks/broadcast.rs` | Add `required() -> false` |
| `crates/argus/src/api/state.rs` | Add stall status fields to Bridge, expose on status endpoint |
| `crates/argus/src/runtime.rs` | Thread spawns return `Result`, wire TreeConfig + OverflowConfig |
| `crates/argus/src/pipeline/ptrace_thread.rs` | `PtraceStream::spawn` returns `Result` |
| `crates/argus/src/snapshot/tree.rs` | Rewrite internals to use `MerkleNode` root + flat index |
| `crates/argus/src/snapshot/mod.rs` | Re-export `node`, `builder` |
| `crates/argus/src/snapshot/checkpoint.rs` | Bump version to 2, backward-compat deserialize for v1 |
| `crates/argus/src/pipeline/stages/tree.rs` | Own `TreeBuilder` instead of `Mutex<MerkleTree>` |
| `crates/argus/src/config/mod.rs` | Add `tree: TreeConfig`, `overflow: OverflowConfig` to `SupervisorConfig` |

---

## Chunk 1: Sink Resilience

### Task 1: Add `parking_lot` dependency and replace `std::sync::Mutex` in sinks

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/argus/Cargo.toml`
- Modify: `crates/argus/src/pipeline/sinks/event_log.rs`
- Modify: `crates/argus/src/pipeline/sinks/index.rs`
- Modify: `crates/argus/src/pipeline/sinks/memory.rs`
- Modify: `crates/argus/src/pipeline/stages/tree.rs`
- Modify: `crates/argus/src/snapshot/tree.rs`

- [ ] **Step 1: Add `parking_lot` to workspace Cargo.toml**

In workspace root `Cargo.toml`, add to `[workspace.dependencies]`:
```toml
parking_lot = "0.12"
```

In `crates/argus/Cargo.toml`, add:
```toml
parking_lot.workspace = true
```

- [ ] **Step 2: Replace `std::sync::Mutex` with `parking_lot::Mutex` in EventLogSink**

File: `crates/argus/src/pipeline/sinks/event_log.rs`

Replace:
```rust
use std::sync::Mutex;
```
With:
```rust
use parking_lot::Mutex;
```

Replace every `.lock().expect("EventLogSink mutex poisoned")` with `.lock()` (parking_lot returns `MutexGuard` directly, no `Result`).

There are 3 call sites: lines 50, 70, 76. Also update `with_log` method doc comment to remove the Panics section about poisoning.

- [ ] **Step 3: Add recovery logic to EventLogSink::write**

Replace the `write` method body:
```rust
fn write(&self, record: Record) -> Result<()> {
    let Record::Event(event) = record else {
        return Ok(());
    };
    let mut guard = self.log.lock();
    if let Err(e) = guard.append(&event, None) {
        event!(
            name: "sink.event_log.write_failed",
            tracing::Level::WARN,
            error.message = %e,
            "event log append failed, attempting recovery via reopen",
        );
        if let Err(reopen_err) = guard.reopen() {
            event!(
                name: "sink.event_log.reopen_failed",
                tracing::Level::ERROR,
                error.message = %reopen_err,
                "event log reopen failed, sink degraded until next successful write",
            );
        }
        return Err(e);
    }
    Ok(())
}
```

Add `use tracing::{event, Level};` to imports if not present.

- [ ] **Step 4: Replace `std::sync::Mutex` in IndexSink**

File: `crates/argus/src/pipeline/sinks/index.rs`

Replace `use std::sync::Mutex;` with `use parking_lot::Mutex;`.
Replace `.lock().expect("IndexSink mutex poisoned")` (line 65) with `.lock()`.
Update test that calls `.state.lock().unwrap()` to `.state.lock()` (parking_lot returns guard directly).

- [ ] **Step 5: Replace `std::sync::Mutex` in MemorySink**

File: `crates/argus/src/pipeline/sinks/memory.rs`

Replace `use std::sync::Mutex;` with `use parking_lot::Mutex;`.
Replace all `.lock().expect("MemorySink mutex poisoned")` with `.lock()`. There are 5 call sites (lines 47, 58, 76, 85, 99).

- [ ] **Step 6: Replace `std::sync::Mutex` in TreeStage**

File: `crates/argus/src/pipeline/stages/tree.rs`

Replace `use std::sync::Mutex;` with `use parking_lot::Mutex;`.
Replace the match on `self.tree.lock()` (lines 53-64) with:
```rust
let mut tree = self.tree.lock();
```
Remove the `Err(e)` poisoning branch entirely — parking_lot cannot poison.

- [ ] **Step 7: Replace `std::sync::Mutex` in MerkleTree::cached_root**

File: `crates/argus/src/snapshot/tree.rs`

Replace `use std::sync::Mutex;` with `use parking_lot::Mutex;`.
Replace all `.lock().unwrap()` on `cached_root` with `.lock()`. There are 6 call sites: lines 73, 115, 122, 134, 144, 150.

- [ ] **Step 8: Build and test**

```bash
docker exec argus-arm64 bash -c "cd /build/.claude/worktrees/enriched-output-pipeline && cargo build --target aarch64-unknown-linux-musl -p argus -p supervisor"
docker exec argus-arm64 bash -c "cd /build/.claude/worktrees/enriched-output-pipeline && cargo test --target aarch64-unknown-linux-musl -p argus -p supervisor"
```

Expected: all 621+ tests pass, zero `expect` on any mutex.

- [ ] **Step 9: Commit**

```bash
git add -A && git commit -m "replace std::sync::Mutex with parking_lot::Mutex in all sinks and tree

Eliminates mutex poisoning risk across EventLogSink, IndexSink,
MemorySink, TreeStage, and MerkleTree. parking_lot::Mutex returns
MutexGuard directly — no unwrap/expect needed.

EventLogSink now attempts recovery via reopen() on write failure
so transient I/O errors don't permanently disable the sink."
```

---

### Task 2: Add `required` flag to Sink trait and `EmitResult` to RecordBus

**Files:**
- Create: `crates/argus/src/pipeline/emit_result.rs`
- Modify: `crates/argus/src/pipeline/sink.rs`
- Modify: `crates/argus/src/pipeline/bus.rs`
- Modify: `crates/argus/src/pipeline/mod.rs`
- Modify: `crates/argus/src/pipeline/sinks/broadcast.rs`
- Modify: `crates/argus/src/pipeline/sinks/index.rs`

- [ ] **Step 1: Create `emit_result.rs`**

File: `crates/argus/src/pipeline/emit_result.rs`
```rust
//! Result type for bus emission indicating required-sink failures.

/// Outcome of delivering a record to all sinks.
#[derive(Debug)]
pub enum EmitResult {
    /// All required sinks accepted the record.
    Ok,
    /// One or more required sinks failed. Contains (sink_name, error) pairs.
    RequiredFailed(Vec<(String, anyhow::Error)>),
}

impl EmitResult {
    /// Returns true if all required sinks succeeded.
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok)
    }
}
```

- [ ] **Step 2: Add `required()` to Sink trait**

File: `crates/argus/src/pipeline/sink.rs`

Add after the `accept` method:
```rust
    /// Whether this sink must succeed before the tracee is resumed.
    ///
    /// When a required sink fails, the pipeline runner holds the tracee
    /// frozen and retries with backoff until the sink recovers.
    /// Default is `true` — override to `false` for best-effort sinks.
    fn required(&self) -> bool {
        true
    }
```

- [ ] **Step 3: Mark BroadcastSink and IndexSink as non-required**

File: `crates/argus/src/pipeline/sinks/broadcast.rs` — add:
```rust
fn required(&self) -> bool {
    false
}
```

File: `crates/argus/src/pipeline/sinks/index.rs` — add:
```rust
fn required(&self) -> bool {
    false
}
```

Rationale: broadcast is fire-and-forget to websocket subscribers. Indexes can be rebuilt from the event log. EventLogSink, LocalCasSink, and RemoteCasSink remain required (default true).

- [ ] **Step 4: Update RecordBus::emit to return EmitResult**

File: `crates/argus/src/pipeline/bus.rs`

Add import:
```rust
use super::emit_result::EmitResult;
```

Change `emit` signature and body:
```rust
pub fn emit(&self, record: Record) -> EmitResult {
    let mut required_failures: Vec<(String, anyhow::Error)> = Vec::new();

    for sink in &self.blocking {
        if sink.accept(&record) {
            if let Err(e) = sink.write(record.clone()) {
                if sink.required() {
                    event!(
                        name: "bus.sink.required_write_error",
                        Level::ERROR,
                        sink.name = sink.name(),
                        error.message = %e,
                        "required sink write failed",
                    );
                    required_failures.push((sink.name().to_owned(), e));
                } else {
                    event!(
                        name: "bus.sink.write_error",
                        Level::WARN,
                        sink.name = sink.name(),
                        error.message = %e,
                        "optional sink write failed, continuing",
                    );
                }
            }
        }
    }
    for sink in &self.async_sinks {
        if sink.accept(&record) {
            if let Err(e) = sink.write(record.clone()) {
                if sink.required() {
                    event!(
                        name: "bus.sink.required_async_write_error",
                        Level::ERROR,
                        sink.name = sink.name(),
                        error.message = %e,
                        "required async sink write failed",
                    );
                    required_failures.push((sink.name().to_owned(), e));
                } else {
                    event!(
                        name: "bus.sink.async_write_error",
                        Level::WARN,
                        sink.name = sink.name(),
                        error.message = %e,
                        "optional async sink write failed, continuing",
                    );
                }
            }
        }
    }

    if required_failures.is_empty() {
        EmitResult::Ok
    } else {
        EmitResult::RequiredFailed(required_failures)
    }
}
```

- [ ] **Step 5: Add `emit_result` module to pipeline/mod.rs**

Add `pub mod emit_result;` and `pub use emit_result::EmitResult;`.

- [ ] **Step 6: Update all callers of `bus.emit()` to handle `EmitResult`**

Most callers (runtime.rs emit_agent_start, emit_initial_state, keylog_pipeline, proxy_pipeline, api/state.rs Bridge::emit) should log-and-continue since they don't have a tracee to freeze. Change:
```rust
self.ctx.bus.emit(Record::Event(evt));
```
To:
```rust
if let EmitResult::RequiredFailed(failures) = self.ctx.bus.emit(Record::Event(evt)) {
    for (name, err) in &failures {
        event!(
            name: "pipeline.emit.required_sink_failed",
            Level::ERROR,
            sink.name = name.as_str(),
            error.message = %err,
            "required sink failed on non-ptrace path, event may be lost",
        );
    }
}
```

The pipeline runner (runner.rs) gets the retry loop in Task 3.

- [ ] **Step 7: Update bus tests**

Tests that call `bus.emit(...)` without capturing the return value need updating. Add `assert!(bus.emit(record).is_ok())` or `let _ = bus.emit(record)` where the test doesn't care about the result.

- [ ] **Step 8: Build and test**

```bash
docker exec argus-arm64 bash -c "cd /build/.claude/worktrees/enriched-output-pipeline && cargo test --target aarch64-unknown-linux-musl -p argus"
```

- [ ] **Step 9: Commit**

```bash
git commit -m "add required flag to Sink trait, EmitResult from RecordBus

Sinks now declare whether they must succeed via required().
BroadcastSink and IndexSink are best-effort (required=false).
RecordBus::emit returns EmitResult so callers can distinguish
required failures from optional ones."
```

---

### Task 3: Pipeline runner retry loop with tracee freeze and stall status

**Files:**
- Create: `crates/argus/src/pipeline/stall.rs`
- Modify: `crates/argus/src/pipeline/runner.rs`
- Modify: `crates/argus/src/pipeline/mod.rs`
- Modify: `crates/argus/src/api/state.rs`

- [ ] **Step 1: Create `stall.rs`**

File: `crates/argus/src/pipeline/stall.rs`
```rust
//! Sink stall status exposed to the API.

use std::time::Instant;

/// Describes a sink stall condition.
#[derive(Debug, Clone)]
pub struct StallState {
    /// Names of the failed required sinks.
    pub failed_sinks: Vec<String>,
    /// When the stall began.
    pub since: Instant,
    /// How many retry attempts have been made.
    pub retry_count: u32,
}
```

- [ ] **Step 2: Add stall status to Bridge**

File: `crates/argus/src/api/state.rs`

Add field to Bridge:
```rust
use parking_lot::Mutex as ParkingMutex;
use crate::pipeline::stall::StallState;

pub struct Bridge {
    // ... existing fields ...
    /// Current sink stall state, if any.
    stall: ParkingMutex<Option<StallState>>,
}
```

Add methods:
```rust
/// Set the stall state when required sinks fail.
pub fn set_stall(&self, state: StallState) {
    *self.stall.lock() = Some(state);
}

/// Clear the stall state when sinks recover.
pub fn clear_stall(&self) {
    *self.stall.lock() = None;
}

/// Snapshot of the current stall state for the API.
pub fn stall_state(&self) -> Option<StallState> {
    self.stall.lock().clone()
}
```

Initialize `stall: ParkingMutex::new(None)` in `Bridge::new`.

- [ ] **Step 3: Add supervisor status enum to API types**

File: `crates/argus/src/api/types.rs` — add:
```rust
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SupervisorStatus {
    Running,
    Paused { reason: String },
    SinkStall {
        failed_sinks: Vec<String>,
        stalled_since: String,
        retry_count: u32,
        message: String,
    },
}
```

Wire it into the existing `/status` route handler: check `bridge.stall_state()` first, then `bridge.is_paused()`, then `Running`.

- [ ] **Step 4: Add retry loop to pipeline runner**

File: `crates/argus/src/pipeline/runner.rs`

Add imports:
```rust
use crate::pipeline::EmitResult;
use crate::pipeline::stall::StallState;
use std::time::{Duration, Instant};
```

Replace the current emit-then-resume pattern. After stamp, before resume directive:
```rust
// Emit to sinks with retry on required failures.
// Tracee stays ptrace-frozen until all required sinks accept.
let record = crate::pipeline::Record::Event(evt);
let mut backoff = Duration::from_secs(1);
let max_backoff = Duration::from_secs(60);
let mut retry_count: u32 = 0;
let stall_start = Instant::now();

loop {
    match self.bus.emit(record.clone()) {
        EmitResult::Ok => {
            if retry_count > 0 {
                self.shared.clear_stall();
                event!(
                    name: "pipeline.ptrace.stall_recovered",
                    Level::INFO,
                    retry_count,
                    stall_duration_ms = stall_start.elapsed().as_millis() as u64,
                    "required sinks recovered, resuming tracee",
                );
            }
            break;
        }
        EmitResult::RequiredFailed(failures) => {
            retry_count += 1;
            let sink_names: Vec<String> = failures.iter().map(|(n, _)| n.clone()).collect();
            self.shared.set_stall(StallState {
                failed_sinks: sink_names.clone(),
                since: stall_start,
                retry_count,
            });
            event!(
                name: "pipeline.ptrace.sink_stall",
                Level::WARN,
                ?sink_names,
                retry_count,
                backoff_ms = backoff.as_millis() as u64,
                "required sinks failed, tracee frozen, retrying",
            );
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(max_backoff);
        }
    }
}
```

- [ ] **Step 5: Also handle the blocked-event emit path**

The runner also emits blocked events (line ~155). Apply the same retry pattern there, or extract a helper:
```rust
fn emit_with_retry(&self, record: Record) -> impl Future<Output = ()> + '_ {
    // ... retry loop from step 4 ...
}
```

Extract an async method on the runner:
```rust
async fn emit_required(&mut self, record: Record) {
    // retry loop body from step 4
}
```

Call it from both the main event path and the blocked-event path.

- [ ] **Step 6: Build and test**

- [ ] **Step 7: Commit**

```bash
git commit -m "add retry loop for required sinks with tracee freeze and stall status

Pipeline runner holds the tracee ptrace-frozen when required sinks
fail, retrying with exponential backoff (1s-60s). Bridge exposes
stall state to the API so operators see why the agent is frozen.
Stall clears automatically when sinks recover."
```

---

### Task 4: SQLite overflow for non-ptrace threads

**Files:**
- Create: `crates/argus/src/pipeline/overflow.rs`
- Modify: `Cargo.toml` (workspace)
- Modify: `crates/argus/Cargo.toml`
- Modify: `crates/argus/src/pipeline/mod.rs`
- Modify: `crates/argus/src/pipeline/keylog_pipeline.rs`
- Modify: `crates/argus/src/pipeline/proxy_pipeline.rs`
- Modify: `crates/argus/src/api/state.rs`
- Modify: `crates/argus/src/config/mod.rs`

- [ ] **Step 1: Add `rusqlite` dependency**

Workspace `Cargo.toml`:
```toml
rusqlite = { version = "0.32", features = ["bundled"] }
```

`crates/argus/Cargo.toml`:
```toml
rusqlite.workspace = true
```

The `bundled` feature statically links SQLite — no system dependency needed in the container.

- [ ] **Step 2: Add OverflowConfig**

File: `crates/argus/src/config/mod.rs`

```rust
/// SQLite overflow queue for non-ptrace pipeline threads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverflowConfig {
    /// Max records buffered in memory before spilling to SQLite.
    #[serde(default = "default_overflow_memory_limit")]
    pub memory_limit: usize,
    /// Initial retry interval in milliseconds.
    #[serde(default = "default_overflow_retry_ms")]
    pub retry_interval_ms: u64,
    /// Maximum retry interval in milliseconds.
    #[serde(default = "default_overflow_max_retry_ms")]
    pub max_retry_interval_ms: u64,
}

fn default_overflow_memory_limit() -> usize { 1024 }
fn default_overflow_retry_ms() -> u64 { 1000 }
fn default_overflow_max_retry_ms() -> u64 { 60_000 }

impl Default for OverflowConfig {
    fn default() -> Self {
        Self {
            memory_limit: default_overflow_memory_limit(),
            retry_interval_ms: default_overflow_retry_ms(),
            max_retry_interval_ms: default_overflow_max_retry_ms(),
        }
    }
}
```

Add `pub overflow: OverflowConfig` to `SupervisorConfig` with `#[serde(default)]`.

- [ ] **Step 3: Create overflow.rs**

File: `crates/argus/src/pipeline/overflow.rs`

```rust
//! SQLite-backed overflow queue for records that failed sink delivery.
//!
//! Non-ptrace threads (keylog, proxy, API) cannot freeze a tracee when
//! required sinks fail. Instead they push records into this queue.
//! A background flush thread drains the queue with exponential backoff
//! when sinks recover.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::Connection;
use tracing::{event, Level};

use crate::config::OverflowConfig;
use crate::pipeline::bus::RecordBus;
use crate::pipeline::emit_result::EmitResult;
use crate::pipeline::record::Record;

/// Overflow queue that buffers in memory then spills to SQLite.
pub struct OverflowQueue {
    memory: Mutex<VecDeque<Vec<u8>>>,
    db: Mutex<Connection>,
    config: OverflowConfig,
}

impl OverflowQueue {
    /// Open or create the overflow database at `db_path`.
    pub fn new(db_path: &Path, config: OverflowConfig) -> Result<Self> {
        let conn = Connection::open(db_path)
            .with_context(|| format!("open overflow db: {}", db_path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS overflow (
                 id INTEGER PRIMARY KEY,
                 record BLOB NOT NULL
             );"
        ).context("initialize overflow schema")?;
        Ok(Self {
            memory: Mutex::new(VecDeque::new()),
            db: Mutex::new(conn),
            config,
        })
    }

    /// Push a record that failed delivery.
    ///
    /// Buffers in memory up to `config.memory_limit`, then batch-inserts
    /// to SQLite when the threshold is reached.
    pub fn push(&self, record: &Record) {
        let bytes = match bincode::serialize(record) {
            Ok(b) => b,
            Err(e) => {
                event!(Level::ERROR, error = %e, "failed to serialize record for overflow");
                return;
            }
        };
        let mut mem = self.memory.lock();
        mem.push_back(bytes);
        if mem.len() >= self.config.memory_limit {
            self.spill_to_db(&mut mem);
        }
    }

    /// Drain pending records and attempt re-delivery via `bus`.
    ///
    /// Returns the number of records successfully delivered.
    pub fn flush_to_bus(&self, bus: &RecordBus) -> usize {
        let mut delivered = 0;

        // Drain SQLite first (oldest records)
        delivered += self.drain_db(bus);

        // Then drain memory queue
        let mut mem = self.memory.lock();
        let mut retry_queue = VecDeque::new();
        while let Some(bytes) = mem.pop_front() {
            match bincode::deserialize::<Record>(&bytes) {
                Ok(record) => {
                    if bus.emit(record).is_ok() {
                        delivered += 1;
                    } else {
                        retry_queue.push_back(bytes);
                    }
                }
                Err(e) => {
                    event!(Level::WARN, error = %e, "corrupt overflow record, dropping");
                }
            }
        }
        *mem = retry_queue;
        delivered
    }

    /// Number of pending records (memory + SQLite).
    pub fn pending_count(&self) -> usize {
        let mem_count = self.memory.lock().len();
        let db_count = self.db.lock()
            .query_row("SELECT COUNT(*) FROM overflow", [], |row| row.get::<_, usize>(0))
            .unwrap_or(0);
        mem_count + db_count
    }

    fn spill_to_db(&self, mem: &mut VecDeque<Vec<u8>>) {
        let mut db = self.db.lock();
        let tx = match db.transaction() {
            Ok(t) => t,
            Err(e) => {
                event!(Level::ERROR, error = %e, "failed to begin overflow transaction");
                return;
            }
        };
        {
            let mut stmt = match tx.prepare("INSERT INTO overflow (record) VALUES (?1)") {
                Ok(s) => s,
                Err(e) => {
                    event!(Level::ERROR, error = %e, "failed to prepare overflow insert");
                    return;
                }
            };
            for bytes in mem.drain(..) {
                if let Err(e) = stmt.execute([bytes]) {
                    event!(Level::WARN, error = %e, "overflow insert failed, record dropped");
                }
            }
        }
        if let Err(e) = tx.commit() {
            event!(Level::ERROR, error = %e, "overflow transaction commit failed");
        }
    }

    fn drain_db(&self, bus: &RecordBus) -> usize {
        let mut db = self.db.lock();
        let mut delivered = 0;
        let batch_size = 256;

        loop {
            let rows: Vec<(i64, Vec<u8>)> = {
                let mut stmt = match db.prepare(
                    "SELECT id, record FROM overflow ORDER BY id LIMIT ?1"
                ) {
                    Ok(s) => s,
                    Err(_) => break,
                };
                match stmt.query_map([batch_size], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
                }) {
                    Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
                    Err(_) => break,
                }
            };

            if rows.is_empty() {
                break;
            }

            let mut ids_to_delete = Vec::new();
            for (id, bytes) in &rows {
                match bincode::deserialize::<Record>(bytes) {
                    Ok(record) => {
                        if bus.emit(record).is_ok() {
                            ids_to_delete.push(*id);
                            delivered += 1;
                        } else {
                            // Sink still failing — stop draining, retry later
                            break;
                        }
                    }
                    Err(e) => {
                        event!(Level::WARN, error = %e, "corrupt overflow db record, deleting");
                        ids_to_delete.push(*id);
                    }
                }
            }

            if !ids_to_delete.is_empty() {
                let placeholders: String = ids_to_delete.iter()
                    .map(|_| "?".to_owned())
                    .collect::<Vec<_>>()
                    .join(",");
                let sql = format!("DELETE FROM overflow WHERE id IN ({placeholders})");
                if let Ok(mut stmt) = db.prepare(&sql) {
                    let params: Vec<&dyn rusqlite::types::ToSql> = ids_to_delete
                        .iter()
                        .map(|id| id as &dyn rusqlite::types::ToSql)
                        .collect();
                    let _ = stmt.execute(params.as_slice());
                }
            }

            if ids_to_delete.len() < rows.len() {
                break; // sink still failing
            }
        }

        delivered
    }
}
```

- [ ] **Step 4: Add overflow to pipeline/mod.rs**

```rust
pub mod overflow;
```

- [ ] **Step 5: Wire overflow into non-ptrace emit paths**

In `keylog_pipeline.rs`, `proxy_pipeline.rs`, and `api/state.rs` (Bridge::emit), when `EmitResult::RequiredFailed` is returned, push to the overflow queue instead of just logging.

The overflow queue is constructed in `runtime.rs` and passed via `PipelineContext`:

Add to `PipelineContext`:
```rust
pub(crate) overflow: Option<Arc<OverflowQueue>>,
```

The flush thread is spawned in `runtime.rs` alongside the keylog/proxy threads.

- [ ] **Step 6: Spawn background flush thread**

In `runtime.rs`, add a method:
```rust
pub fn spawn_overflow_flush_thread(
    &self,
) -> Option<(JoinHandle<()>, Arc<AtomicBool>)> {
    let overflow = self.ctx.overflow.clone()?;
    let bus = self.ctx.bus.clone();
    let config = self.config.overflow.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();

    let handle = thread::Builder::new()
        .name("overflow-flush".into())
        .spawn(move || {
            let mut interval = Duration::from_millis(config.retry_interval_ms);
            let max_interval = Duration::from_millis(config.max_retry_interval_ms);
            while !stop_clone.load(Ordering::Relaxed) {
                thread::sleep(interval);
                let flushed = overflow.flush_to_bus(&bus);
                if flushed > 0 {
                    interval = Duration::from_millis(config.retry_interval_ms);
                    event!(Level::INFO, flushed, "overflow flush delivered records");
                } else if overflow.pending_count() > 0 {
                    interval = (interval * 2).min(max_interval);
                }
            }
            // Final drain on shutdown
            let _ = overflow.flush_to_bus(&bus);
        })
        .ok()?;

    Some((handle, stop))
}
```

Note: thread spawn returns `Result` — `.ok()?` converts to `None` on failure (graceful degradation; overflow flush is best-effort).

- [ ] **Step 7: Write tests for overflow queue**

Test file embedded in `overflow.rs` as `#[cfg(test)] mod tests`:
- `push_and_flush_happy_path`: push records, flush to a MemorySink bus, verify delivery
- `memory_spills_to_db`: push > memory_limit records, verify SQLite has rows
- `drain_order_is_fifo`: push A, B, C, flush, verify order
- `corrupt_record_is_skipped`: insert bad bytes into SQLite, verify flush skips without panic
- `pending_count_accurate`: verify memory + db count

- [ ] **Step 8: Build and test**

- [ ] **Step 9: Commit**

```bash
git commit -m "add SQLite overflow queue for non-ptrace pipeline threads

Non-ptrace threads (keylog, proxy, API) push failed records to an
in-memory queue that spills to SQLite when memory_limit is reached.
A background thread flushes overflow to sinks with exponential
backoff. Ordering preserved: SQLite (oldest) drains before memory."
```

---

### Task 5: Unwrap audit — thread spawns return Result

**Files:**
- Modify: `crates/argus/src/runtime.rs`
- Modify: `crates/argus/src/pipeline/ptrace_thread.rs`

- [ ] **Step 1: Change `spawn_keylog_pipeline` to return `Result`**

```rust
pub fn spawn_keylog_pipeline(&self) -> Result<(JoinHandle<()>, Arc<AtomicBool>)> {
    // ... same body ...
    let handle = thread::Builder::new()
        .name("keylog-pipeline".into())
        .spawn(move || {
            crate::pipeline::keylog_pipeline::run(keylog_path, ctx, stop_clone, TLS_POLL_INTERVAL);
        })
        .context("failed to spawn keylog pipeline thread")?;
    Ok((handle, stop))
}
```

- [ ] **Step 2: Change `spawn_proxy_pipeline` to return `Result<Option<...>>`**

```rust
pub fn spawn_proxy_pipeline(
    &self,
    flow_path: Option<PathBuf>,
) -> Result<Option<(JoinHandle<()>, Arc<AtomicBool>)>> {
    let Some(path) = flow_path else { return Ok(None) };
    // ...
    let handle = thread::Builder::new()
        .name("proxy-pipeline".into())
        .spawn(move || { /* ... */ })
        .context("failed to spawn proxy pipeline thread")?;
    Ok(Some((handle, stop)))
}
```

- [ ] **Step 3: Change `into_pipeline` to return `Result`**

The tree-stage CAS init on line 320 uses `.expect()`. Change:
```rust
pub fn into_pipeline(
    self,
    child_pid: Pid,
) -> Result<(PipelineRunner, tokio::sync::oneshot::Receiver<Result<()>>, JoinHandle<()>)> {
    // ...
    let tree_cas = LocalCas::new(self.config.data_dir.join("cas"))
        .context("failed to initialize tree-stage CAS")?;
    // ...
    Ok((runner, seize_rx, ptrace_thread))
}
```

- [ ] **Step 4: Change `PtraceStream::spawn` to return `Result`**

File: `crates/argus/src/pipeline/ptrace_thread.rs`

Change the `.expect("failed to spawn ptrace thread")` to return `Result`. The caller in `into_pipeline` propagates it.

- [ ] **Step 5: Update callers in `crates/supervisor/src/main.rs`**

Startup code calls these methods. Add `?` or `.context("...")` at the callsite. Startup failures are still fatal — `main` returns `Result<()>` so they propagate to process exit.

- [ ] **Step 6: Build and test**

- [ ] **Step 7: Commit**

```bash
git commit -m "thread spawns and into_pipeline return Result instead of panicking

spawn_keylog_pipeline, spawn_proxy_pipeline, into_pipeline, and
PtraceStream::spawn now return Result. Startup code propagates
errors to main. Mid-runtime callers can handle spawn failures
gracefully without crashing the supervisor."
```

---

## Chunk 2: MerkleTree Path-Copy Redesign

### Task 6: MerkleNode with OnceLock lazy hashing and path-copy

**Files:**
- Create: `crates/argus/src/snapshot/node.rs`
- Modify: `crates/argus/src/snapshot/mod.rs`

- [ ] **Step 1: Write failing test for MerkleNode**

Create `crates/argus/src/snapshot/node.rs` with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cas::ContentHash;

    #[test]
    fn empty_dir_has_deterministic_hash() {
        let a = MerkleNode::empty_dir();
        let b = MerkleNode::empty_dir();
        assert_eq!(a.hash(), b.hash());
    }

    #[test]
    fn leaf_hash_matches_content_hash() {
        let h = ContentHash::from_data(b"hello");
        let leaf = MerkleNode::leaf(h);
        assert_eq!(*leaf.hash(), h);
    }

    #[test]
    fn path_copy_creates_new_root() {
        let h1 = ContentHash::from_data(b"v1");
        let h2 = ContentHash::from_data(b"v2");
        let mut root = MerkleNode::empty_dir();
        root = root.with_child("a.txt", Arc::new(MerkleNode::leaf(h1)));
        let new_root = root.path_copy(&["a.txt"], Arc::new(MerkleNode::leaf(h2)));
        // Old root unchanged
        assert_ne!(root.hash(), new_root.hash());
    }

    #[test]
    fn path_copy_shares_siblings() {
        let ha = ContentHash::from_data(b"a");
        let hb = ContentHash::from_data(b"b");
        let hb2 = ContentHash::from_data(b"b2");
        let mut root = MerkleNode::empty_dir();
        root = root.with_child("a.txt", Arc::new(MerkleNode::leaf(ha)));
        root = root.with_child("b.txt", Arc::new(MerkleNode::leaf(hb)));
        let new_root = root.path_copy(&["b.txt"], Arc::new(MerkleNode::leaf(hb2)));
        // a.txt child should be the exact same Arc (pointer equality)
        let old_a = root.get_child("a.txt").unwrap();
        let new_a = new_root.get_child("a.txt").unwrap();
        assert!(Arc::ptr_eq(&old_a, &new_a));
    }

    #[test]
    fn lazy_hash_computed_once() {
        let h = ContentHash::from_data(b"x");
        let mut root = MerkleNode::empty_dir();
        root = root.with_child("f", Arc::new(MerkleNode::leaf(h)));
        let hash1 = *root.hash();
        let hash2 = *root.hash();
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn nested_path_copy() {
        let h1 = ContentHash::from_data(b"deep");
        let h2 = ContentHash::from_data(b"deeper");
        let leaf1 = Arc::new(MerkleNode::leaf(h1));
        let mut inner = MerkleNode::empty_dir();
        inner = inner.with_child("file.txt", leaf1);
        let mut root = MerkleNode::empty_dir();
        root = root.with_child("dir", Arc::new(inner));
        let new_root = root.path_copy(
            &["dir", "file.txt"],
            Arc::new(MerkleNode::leaf(h2)),
        );
        assert_ne!(root.hash(), new_root.hash());
    }
}
```

- [ ] **Step 2: Implement MerkleNode**

```rust
//! Persistent Merkle tree node with structural sharing.
//!
//! Uses `Arc`-based path-copy so mutations produce a new root while
//! sharing all unmodified subtrees. Hash computation is deferred via
//! `OnceLock` — dirty nodes stay unhashed until a reader forces
//! resolution, amortizing cost across burst mutations.

use std::sync::OnceLock;
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::cas::ContentHash;

/// A node in the persistent Merkle tree.
///
/// Nodes are immutable once constructed. Mutations produce new nodes
/// via `path_copy`, sharing children via `Arc`.
///
/// NOTE: `Arc` usage here is approved — nodes are genuinely shared
/// across multiple tree snapshots (the entire point of structural
/// sharing). This is not a lazy substitute for ownership.
pub struct MerkleNode {
    hash: OnceLock<ContentHash>,
    kind: NodeKind,
}

enum NodeKind {
    Leaf { content_hash: ContentHash },
    Dir { children: Arc<BTreeMap<String, Arc<MerkleNode>>> },
}

impl std::fmt::Debug for MerkleNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            NodeKind::Leaf { content_hash } => {
                f.debug_struct("Leaf").field("hash", content_hash).finish()
            }
            NodeKind::Dir { children } => {
                f.debug_struct("Dir").field("children", &children.len()).finish()
            }
        }
    }
}

impl MerkleNode {
    /// Create a leaf node referencing a content blob in CAS.
    pub fn leaf(content_hash: ContentHash) -> Self {
        let hash = OnceLock::new();
        let _ = hash.set(content_hash);
        Self { hash, kind: NodeKind::Leaf { content_hash } }
    }

    /// Create an empty directory node.
    pub fn empty_dir() -> Self {
        Self {
            hash: OnceLock::new(),
            kind: NodeKind::Dir { children: Arc::new(BTreeMap::new()) },
        }
    }

    /// Create a directory node with the given children.
    pub fn dir(children: BTreeMap<String, Arc<MerkleNode>>) -> Self {
        Self {
            hash: OnceLock::new(),
            kind: NodeKind::Dir { children: Arc::new(children) },
        }
    }

    /// Add or replace a child, returning a new node (path-copy).
    pub fn with_child(&self, name: &str, child: Arc<MerkleNode>) -> Self {
        let children = match &self.kind {
            NodeKind::Dir { children } => children,
            NodeKind::Leaf { .. } => {
                // Replacing a leaf with a directory that contains the child
                let mut new_children = BTreeMap::new();
                new_children.insert(name.to_owned(), child);
                return Self {
                    hash: OnceLock::new(),
                    kind: NodeKind::Dir { children: Arc::new(new_children) },
                };
            }
        };
        let mut new_children = (**children).clone();
        new_children.insert(name.to_owned(), child);
        Self {
            hash: OnceLock::new(),
            kind: NodeKind::Dir { children: Arc::new(new_children) },
        }
    }

    /// Get a child by name.
    pub fn get_child(&self, name: &str) -> Option<Arc<MerkleNode>> {
        match &self.kind {
            NodeKind::Dir { children } => children.get(name).cloned(),
            NodeKind::Leaf { .. } => None,
        }
    }

    /// Remove a child by name, returning a new node.
    pub fn without_child(&self, name: &str) -> Self {
        let children = match &self.kind {
            NodeKind::Dir { children } => children,
            NodeKind::Leaf { .. } => return Self::empty_dir(),
        };
        let mut new_children = (**children).clone();
        new_children.remove(name);
        Self {
            hash: OnceLock::new(),
            kind: NodeKind::Dir { children: Arc::new(new_children) },
        }
    }

    /// Path-copy: walk down `components`, replace the leaf, return new root.
    ///
    /// Intermediate directories are created if they don't exist.
    /// All sibling subtrees are shared via Arc — only the modified
    /// path allocates new nodes.
    pub fn path_copy(&self, components: &[&str], new_leaf: Arc<MerkleNode>) -> Self {
        match components {
            [] => {
                // Unwrap the Arc — caller wants this node replaced
                // Return the inner node by cloning (OnceLock is not Clone,
                // so we reconstruct)
                match &new_leaf.kind {
                    NodeKind::Leaf { content_hash } => Self::leaf(*content_hash),
                    NodeKind::Dir { children } => Self {
                        hash: OnceLock::new(),
                        kind: NodeKind::Dir { children: Arc::clone(children) },
                    },
                }
            }
            [name, rest @ ..] => {
                let child = self.get_child(name)
                    .unwrap_or_else(|| Arc::new(Self::empty_dir()));
                let new_child = child.path_copy(rest, new_leaf);
                self.with_child(name, Arc::new(new_child))
            }
        }
    }

    /// Lazily compute and cache the hash for this node.
    ///
    /// Leaf nodes return their content hash directly. Directory nodes
    /// hash their sorted children deterministically. The result is
    /// cached in `OnceLock` — subsequent calls are O(1).
    pub fn hash(&self) -> &ContentHash {
        self.hash.get_or_init(|| {
            match &self.kind {
                NodeKind::Leaf { content_hash } => *content_hash,
                NodeKind::Dir { children } => {
                    let mut hasher_input = Vec::new();
                    for (name, child) in children.iter() {
                        let child_hash = child.hash();
                        hasher_input.extend_from_slice(name.as_bytes());
                        hasher_input.push(0);
                        hasher_input.extend_from_slice(child_hash.digest().as_bytes());
                        hasher_input.push(b'\n');
                    }
                    if hasher_input.is_empty() {
                        ContentHash::from_data(b"empty-tree")
                    } else {
                        ContentHash::from_data(&hasher_input)
                    }
                }
            }
        })
    }

    /// Returns true if this is a directory node.
    pub fn is_dir(&self) -> bool {
        matches!(self.kind, NodeKind::Dir { .. })
    }
}
```

- [ ] **Step 3: Add to snapshot/mod.rs**

```rust
pub mod node;
```

- [ ] **Step 4: Build and test**

- [ ] **Step 5: Commit**

```bash
git commit -m "add MerkleNode with Arc-based path-copy and OnceLock lazy hashing

Persistent tree node that shares unmodified subtrees via Arc.
path_copy produces a new root in O(depth) with O(1) sibling sharing.
Hash computation deferred via OnceLock — burst mutations only pay
for hashing once when a reader forces resolution.

Arc usage is intentional: nodes are genuinely shared across multiple
tree snapshots (structural sharing, not lazy ownership)."
```

---

### Task 7: TreeBuilder with batched finalization

**Files:**
- Create: `crates/argus/src/snapshot/builder.rs`
- Create: `crates/argus/src/config/tree.rs`
- Modify: `crates/argus/src/config/mod.rs`
- Modify: `crates/argus/src/snapshot/mod.rs`

- [ ] **Step 1: Create TreeConfig**

File: `crates/argus/src/config/tree.rs`
```rust
//! Configuration for the Merkle tree batched finalization.

use serde::{Deserialize, Serialize};

/// Controls how often the tree finalizes and publishes snapshots.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeConfig {
    /// Mutations accumulated before a finalize pass. Default 64.
    #[serde(default = "default_batch_size")]
    pub batch_size: u64,
    /// Events between checkpoint persists to CAS/S3. Default 1000.
    #[serde(default = "default_checkpoint_interval")]
    pub checkpoint_interval: u64,
}

fn default_batch_size() -> u64 { 64 }
fn default_checkpoint_interval() -> u64 { 1000 }

impl Default for TreeConfig {
    fn default() -> Self {
        Self {
            batch_size: default_batch_size(),
            checkpoint_interval: default_checkpoint_interval(),
        }
    }
}
```

Add `pub mod tree;` to `config/mod.rs`, `pub use tree::TreeConfig;`, and add `#[serde(default)] pub tree: TreeConfig` to `SupervisorConfig`.

- [ ] **Step 2: Create TreeBuilder**

File: `crates/argus/src/snapshot/builder.rs`
```rust
//! Batched mutation builder for the persistent Merkle tree.
//!
//! Accumulates mutations via path-copy without computing hashes.
//! `finalize()` forces hash resolution and produces an immutable
//! snapshot suitable for sharing via Arc.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::cas::ContentHash;
use crate::config::TreeConfig;
use crate::snapshot::node::MerkleNode;

/// Accumulates tree mutations and produces snapshots at cadence.
pub struct TreeBuilder {
    /// Root of the persistent node tree.
    root: MerkleNode,
    /// Flat index for O(1) path lookups (used by diff, restore, API).
    files: BTreeMap<PathBuf, ContentHash>,
    /// Mutations since last finalize.
    dirty_count: u64,
    /// Configuration for finalize cadence.
    config: TreeConfig,
}

impl TreeBuilder {
    /// Create a new builder with an empty tree.
    pub fn new(config: TreeConfig) -> Self {
        Self {
            root: MerkleNode::empty_dir(),
            files: BTreeMap::new(),
            dirty_count: 0,
            config,
        }
    }

    /// Insert or update a file.
    pub fn update(&mut self, path: PathBuf, hash: ContentHash) {
        let components: Vec<&str> = path
            .components()
            .filter_map(|c| match c {
                std::path::Component::Normal(s) => s.to_str(),
                _ => None,
            })
            .collect();
        let new_leaf = Arc::new(MerkleNode::leaf(hash));
        self.root = self.root.path_copy(&components, new_leaf);
        self.files.insert(path, hash);
        self.dirty_count += 1;
    }

    /// Remove a file. Returns true if it existed.
    pub fn remove(&mut self, path: &Path) -> bool {
        let existed = self.files.remove(path).is_some();
        if existed {
            // Rebuild root without this path
            // For simplicity, reconstruct from files index
            self.rebuild_root();
            self.dirty_count += 1;
        }
        existed
    }

    /// Rename a file atomically.
    pub fn rename(&mut self, old: &Path, new: PathBuf) {
        if let Some(hash) = self.files.remove(old) {
            self.files.insert(new.clone(), hash);
            self.rebuild_root();
            self.dirty_count += 1;
        }
    }

    /// Whether enough mutations have accumulated to warrant finalization.
    pub fn should_finalize(&self) -> bool {
        self.dirty_count >= self.config.batch_size
    }

    /// Force hash resolution and produce an immutable snapshot.
    ///
    /// The returned `TreeSnapshot` can be shared via Arc cheaply.
    /// Resets the dirty counter.
    pub fn finalize(&mut self) -> TreeSnapshot {
        // Force hash computation by reading root hash — O(dirty nodes)
        let root_hash = *self.root.hash();
        self.dirty_count = 0;
        TreeSnapshot {
            root_hash,
            files: self.files.clone(),
        }
    }

    /// Current root hash (forces computation if dirty).
    pub fn root_hash(&self) -> ContentHash {
        *self.root.hash()
    }

    /// Number of tracked files.
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// Check if a path exists.
    pub fn contains(&self, path: &Path) -> bool {
        self.files.contains_key(path)
    }

    /// Get content hash for a path.
    pub fn get(&self, path: &Path) -> Option<&ContentHash> {
        self.files.get(path)
    }

    /// Iterate all files.
    pub fn files(&self) -> impl Iterator<Item = (&Path, &ContentHash)> {
        self.files.iter().map(|(p, h)| (p.as_path(), h))
    }

    /// Mutations since last finalize.
    pub fn dirty_count(&self) -> u64 {
        self.dirty_count
    }

    fn rebuild_root(&mut self) {
        let mut root = MerkleNode::empty_dir();
        for (path, hash) in &self.files {
            let components: Vec<&str> = path
                .components()
                .filter_map(|c| match c {
                    std::path::Component::Normal(s) => s.to_str(),
                    _ => None,
                })
                .collect();
            let leaf = Arc::new(MerkleNode::leaf(*hash));
            root = root.path_copy(&components, leaf);
        }
        self.root = root;
    }
}

/// Immutable tree snapshot produced by `TreeBuilder::finalize`.
///
/// Contains only the precomputed root hash and flat file index.
/// Consumers (diff, restore, API) use these two fields exclusively.
/// The persistent MerkleNode tree stays internal to TreeBuilder.
#[derive(Debug, Clone)]
pub struct TreeSnapshot {
    /// Precomputed root hash.
    pub root_hash: ContentHash,
    /// Flat file index for lookups, diff, restore.
    pub files: BTreeMap<PathBuf, ContentHash>,
}

impl TreeSnapshot {
    /// Number of tracked files.
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// Check if a path exists.
    pub fn contains(&self, path: &Path) -> bool {
        self.files.contains_key(path)
    }

    /// Get content hash for a path.
    pub fn get(&self, path: &Path) -> Option<&ContentHash> {
        self.files.get(path)
    }

    /// Iterate all files.
    pub fn files_iter(&self) -> impl Iterator<Item = (&Path, &ContentHash)> {
        self.files.iter().map(|(p, h)| (p.as_path(), h))
    }

    /// The precomputed root hash.
    pub fn root_hash(&self) -> ContentHash {
        self.root_hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(s: &str) -> ContentHash {
        ContentHash::from_data(s.as_bytes())
    }

    #[test]
    fn update_and_finalize() {
        let mut b = TreeBuilder::new(TreeConfig { batch_size: 10, checkpoint_interval: 100 });
        b.update(PathBuf::from("a.txt"), hash("a"));
        b.update(PathBuf::from("b.txt"), hash("b"));
        let snap = b.finalize();
        assert_eq!(snap.file_count(), 2);
        assert!(snap.contains(Path::new("a.txt")));
    }

    #[test]
    fn should_finalize_at_threshold() {
        let mut b = TreeBuilder::new(TreeConfig { batch_size: 3, checkpoint_interval: 100 });
        assert!(!b.should_finalize());
        b.update(PathBuf::from("1"), hash("1"));
        b.update(PathBuf::from("2"), hash("2"));
        assert!(!b.should_finalize());
        b.update(PathBuf::from("3"), hash("3"));
        assert!(b.should_finalize());
    }

    #[test]
    fn finalize_resets_dirty_count() {
        let mut b = TreeBuilder::new(TreeConfig { batch_size: 2, checkpoint_interval: 100 });
        b.update(PathBuf::from("x"), hash("x"));
        b.update(PathBuf::from("y"), hash("y"));
        assert!(b.should_finalize());
        let _ = b.finalize();
        assert!(!b.should_finalize());
        assert_eq!(b.dirty_count(), 0);
    }

    #[test]
    fn remove_file() {
        let mut b = TreeBuilder::new(TreeConfig::default());
        b.update(PathBuf::from("a"), hash("a"));
        assert!(b.contains(Path::new("a")));
        assert!(b.remove(Path::new("a")));
        assert!(!b.contains(Path::new("a")));
    }

    #[test]
    fn rename_file() {
        let mut b = TreeBuilder::new(TreeConfig::default());
        let h = hash("data");
        b.update(PathBuf::from("old"), h);
        b.rename(Path::new("old"), PathBuf::from("new"));
        assert!(!b.contains(Path::new("old")));
        assert_eq!(b.get(Path::new("new")), Some(&h));
    }

    #[test]
    fn snapshot_is_independent() {
        let mut b = TreeBuilder::new(TreeConfig::default());
        b.update(PathBuf::from("a"), hash("v1"));
        let snap1 = b.finalize();
        b.update(PathBuf::from("a"), hash("v2"));
        let snap2 = b.finalize();
        assert_ne!(snap1.root_hash(), snap2.root_hash());
        // snap1 still has v1
        assert_eq!(snap1.get(Path::new("a")), Some(&hash("v1")));
    }

    #[test]
    fn nested_paths() {
        let mut b = TreeBuilder::new(TreeConfig::default());
        b.update(PathBuf::from("src/lib.rs"), hash("lib"));
        b.update(PathBuf::from("src/main.rs"), hash("main"));
        let snap = b.finalize();
        assert_eq!(snap.file_count(), 2);
        assert!(snap.contains(Path::new("src/lib.rs")));
    }
}
```

- [ ] **Step 3: Add to snapshot/mod.rs**

```rust
pub mod builder;
pub use builder::{TreeBuilder, TreeSnapshot};
```

- [ ] **Step 4: Build and test**

- [ ] **Step 5: Commit**

```bash
git commit -m "add TreeBuilder with batched finalization and TreeConfig

TreeBuilder accumulates mutations via path-copy without computing
hashes. finalize() forces hash resolution and produces an immutable
TreeSnapshot. Configurable batch_size (default 64) controls
finalize cadence for many-small-edit workloads."
```

---

### Task 8: Wire TreeBuilder into pipeline stages and runner

**Files:**
- Modify: `crates/argus/src/pipeline/stages/tree.rs`
- Modify: `crates/argus/src/pipeline/runner.rs`
- Modify: `crates/argus/src/runtime.rs`
- Modify: `crates/argus/src/api/state.rs`

- [ ] **Step 1: Rewrite TreeStage to use TreeBuilder**

File: `crates/argus/src/pipeline/stages/tree.rs`

Replace the `Mutex<MerkleTree>` with a direct `TreeBuilder`. The pipeline runner is single-threaded (one event at a time), so no mutex is needed.

```rust
use crate::config::TreeConfig;
use crate::snapshot::builder::{TreeBuilder, TreeSnapshot};
use crate::pipeline::durability::DurabilityLayer;
// ... keep existing captured/classified imports ...

pub struct TreeStage {
    builder: TreeBuilder,
    durability: DurabilityLayer,
    events_since_checkpoint: u64,
    checkpoint_interval: u64,
}

impl TreeStage {
    pub fn new(config: TreeConfig, durability: DurabilityLayer) -> Self {
        let checkpoint_interval = config.checkpoint_interval;
        Self {
            builder: TreeBuilder::new(config),
            durability,
            events_since_checkpoint: 0,
            checkpoint_interval,
        }
    }

    /// Apply a mutation. Returns the current root hash if the tree was mutated.
    pub fn update(&mut self, event: &CapturedEvent) -> Option<ContentHash> {
        let path = mutated_path(&event.classification)?;
        let hash = content_hash(event)?;
        self.builder.update(path, hash);
        self.events_since_checkpoint += 1;

        // Checkpoint at interval
        if self.events_since_checkpoint >= self.checkpoint_interval {
            self.events_since_checkpoint = 0;
            self.persist_checkpoint();
        }

        Some(self.builder.root_hash())
    }

    /// Check if enough mutations accumulated for a snapshot finalize.
    pub fn should_finalize(&self) -> bool {
        self.builder.should_finalize()
    }

    /// Produce an immutable snapshot. Cheap: Arc-based structural sharing.
    pub fn finalize(&mut self) -> TreeSnapshot {
        self.builder.finalize()
    }

    fn persist_checkpoint(&self) {
        // Serialize the flat files map for checkpoint
        let files: std::collections::BTreeMap<_, _> = self.builder.files()
            .map(|(p, h)| (p.to_path_buf(), *h))
            .collect();
        if let Ok(data) = bincode::serialize(&files) {
            let hash = ContentHash::from_data(&data);
            let _ = self.durability.persist_with_hash(hash.clone(), &data);
            self.durability.upload_async(hash, data);
        }
    }
}
```

- [ ] **Step 2: Update pipeline runner to use finalize-at-cadence**

File: `crates/argus/src/pipeline/runner.rs`

Replace the per-event `Arc::new(snapshot.clone())` with finalize-at-cadence:

```rust
let tree_hash = self.tree.update(&captured);

// Finalize and publish snapshot only at batch-size cadence
if self.tree.should_finalize() {
    let snapshot = self.tree.finalize();
    // Store snapshot for API — Arc::new on TreeSnapshot is O(1)
    self.shared.store_tree_snapshot(snapshot);
}
```

Remove the `match self.tree.tree().lock()` block entirely.

- [ ] **Step 3: Update Bridge/SharedState for TreeSnapshot**

File: `crates/argus/src/api/state.rs`

Replace `ArcSwap<MerkleTree>` with `ArcSwap<TreeSnapshot>`:

```rust
use crate::snapshot::builder::TreeSnapshot;

// In Bridge:
tree: ArcSwap<TreeSnapshot>,

// Methods:
pub fn store_tree_snapshot(&self, snapshot: TreeSnapshot) {
    self.tree.store(Arc::new(snapshot));
}

pub fn load_tree_snapshot(&self) -> arc_swap::Guard<Arc<TreeSnapshot>> {
    self.tree.load()
}
```

Update the existing `store_tree` and `load_tree` callers (runtime.rs emit_initial_state, API routes) to use the new snapshot type.

- [ ] **Step 4: Update runtime.rs**

Pass `TreeConfig` to `TreeStage::new`:
```rust
let tree_stage = TreeStage::new(self.config.tree.clone(), tree_durability);
```

Remove the `MerkleTree::new()` argument and the checkpoint_interval argument.

- [ ] **Step 5: Update diff.rs to work with TreeSnapshot**

The `diff_trees` function currently takes `&MerkleTree`. Add an overload or change it to accept the flat files iterator:
```rust
pub fn diff_snapshots(a: &TreeSnapshot, b: &TreeSnapshot) -> Vec<DiffEntry> {
    // Use files maps from snapshots
    let dir_a = build_dir_tree(&a.files);
    let dir_b = build_dir_tree(&b.files);
    // ... same diff logic ...
}
```

- [ ] **Step 6: Update checkpoint.rs for new format**

Bump `CHECKPOINT_VERSION` to 2. Version 2 serializes `BTreeMap<PathBuf, ContentHash>` (the flat index). Version 1 backward compat: deserialize old MerkleTree format, extract files map.

- [ ] **Step 7: Build and test**

All 621+ existing tests must pass. The MerkleTree tests in `snapshot/tree.rs` still work (the old MerkleTree struct stays for backward compat with restore and existing tests). The new path goes through TreeBuilder.

- [ ] **Step 8: Commit**

```bash
git commit -m "wire TreeBuilder into pipeline, eliminate per-event deep clone

TreeStage owns a TreeBuilder directly (no mutex needed, pipeline
is single-threaded). Snapshots are finalized at batch_size cadence
(default 64 events) instead of per-event. finalize() produces a
TreeSnapshot with Arc-based structural sharing — O(1) to publish
to the API via ArcSwap."
```

---

## Dependency Notes

- `parking_lot 0.12` — already a transitive dep of `dashmap`, adds ~0 build cost
- `rusqlite 0.32` with `bundled` feature — statically links SQLite, ~2MB binary size increase, no runtime dependency
- `Arc` usage in `MerkleNode` and `TreeSnapshot` — explicitly approved by design (genuine structural sharing across snapshots, not lazy ownership avoidance)

## Execution Order

Tasks 1-5 (Chunk 1: Sink Resilience) are independent of Tasks 6-8 (Chunk 2: MerkleTree). Within each chunk, tasks must be sequential. The two chunks can be parallelized if using separate worktrees.
