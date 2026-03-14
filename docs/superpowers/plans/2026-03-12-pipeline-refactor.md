# Pipeline Architecture Refactor

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **IMPORTANT:** Activate the ms-rust skill before doing any Rust work. All commands MUST run inside `argus-arm64` container. Build: `docker exec argus-arm64 cargo build --target aarch64-unknown-linux-musl -p supervisor`. Test: `docker exec argus-arm64 cargo test --target aarch64-unknown-linux-musl -p argus -p supervisor`. Never use cargo without `--target aarch64-unknown-linux-musl`.

**Goal:** Migrate from monolithic ptrace pipeline + TLS watcher thread to three independent pipelines sharing a `PipelineContext`, with optimal locking, debug logging throughout.

**Architecture:** Three independent pipelines (Ptrace, Proxy, Keylog) share a `PipelineContext` holding read-only/atomic resources. The `Sink` trait changes from `&self` to `&mut self`, with `Mutex` pushed from individual sinks to the `RecordBus`. A single `Arc<SequenceGenerator>` provides total ordering across all pipelines. The `MerkleTree` stays exclusively in the ptrace pipeline.

**Tech Stack:** Rust 2024, tokio, tracing, std::sync::Mutex

---

## File Map

| Action | Path | Responsibility |
|-|-|-|
| Modify | `crates/argus/src/pipeline/sink.rs` | `&mut self` on write/flush/shutdown, drop `Sync` bound |
| Modify | `crates/argus/src/pipeline/bus.rs` | Wrap sinks in `Arc<Mutex<dyn Sink>>`, map poison to anyhow |
| Modify | `crates/argus/src/pipeline/sinks/event_log.rs` | Remove internal Mutex |
| Modify | `crates/argus/src/pipeline/sinks/memory.rs` | Remove internal Mutex |
| Modify | `crates/argus/src/pipeline/sinks/index.rs` | Remove 3 internal Mutexes |
| Modify | `crates/argus/src/pipeline/sinks/stdout.rs` | Remove internal Mutex |
| Modify | `crates/argus/src/pipeline/sinks/broadcast.rs` | Signature change only |
| Modify | `crates/argus/src/pipeline/sinks/local_cas.rs` | Signature change only |
| Modify | `crates/argus/src/pipeline/sinks/remote_cas.rs` | Signature change only |
| Create | `crates/argus/src/pipeline/context.rs` | `PipelineContext` struct |
| Create | `crates/argus/src/pipeline/keylog_pipeline.rs` | Keylog pipeline loop |
| Create | `crates/argus/src/pipeline/proxy_pipeline.rs` | Proxy/flow pipeline loop |
| Modify | `crates/argus/src/pipeline/mod.rs` | Add new modules, update re-exports |
| Modify | `crates/argus/src/pipeline/stages/stamp.rs` | Accept `Arc<SequenceGenerator>` |
| Modify | `crates/argus/src/pipeline/runner.rs` | Use `PipelineContext`, add debug logging |
| Modify | `crates/argus/src/runtime.rs` | Wire PipelineContext, spawn 3 pipelines |
| Modify | `crates/supervisor/src/wiring.rs` | Handle 3 pipeline handles in shutdown |

---

## Task 1: Sink Trait `&mut self` Refactor

**Files:**
- Modify: `crates/argus/src/pipeline/sink.rs`
- Modify: `crates/argus/src/pipeline/bus.rs`
- Modify: all 7 sink files in `crates/argus/src/pipeline/sinks/`

This task changes the Sink trait and RecordBus together, then updates all sink implementations. It must be done atomically — the crate won't compile with partial changes.

- [ ] **Step 1: Change Sink trait signatures**

In `crates/argus/src/pipeline/sink.rs`:

```rust
pub trait Sink: Send {
    fn priority(&self) -> SinkPriority;
    fn accept(&self, _record: &Record) -> bool { true }
    fn write(&mut self, record: Record) -> Result<()>;
    fn flush(&mut self) -> Result<()>;
    fn shutdown(&mut self) -> Result<()> { self.flush() }
    fn name(&self) -> &str;
}
```

Key changes:
- `Send + Sync` → `Send` (bus Mutex provides Sync)
- `write(&self)` → `write(&mut self)`
- `flush(&self)` → `flush(&mut self)`
- `shutdown(&self)` → `shutdown(&mut self)`
- `accept`, `priority`, `name` stay `&self` (read-only)

- [ ] **Step 2: Update RecordBus to wrap sinks in Mutex**

In `crates/argus/src/pipeline/bus.rs`:

```rust
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct RecordBus {
    blocking: Vec<Arc<Mutex<dyn Sink>>>,
    async_sinks: Vec<Arc<Mutex<dyn Sink>>>,
}
```

Constructor:

```rust
pub fn new(sinks: Vec<Arc<Mutex<dyn Sink>>>) -> Self {
    let mut blocking = Vec::new();
    let mut async_sinks = Vec::new();
    for sink in sinks {
        let priority = sink.lock().unwrap().priority();
        match priority {
            SinkPriority::Blocking => blocking.push(sink),
            SinkPriority::Async => async_sinks.push(sink),
        }
    }
    Self { blocking, async_sinks }
}
```

`emit()` — lock each sink, check accept, write. Map poison to anyhow:

```rust
pub fn emit(&self, record: Record) {
    for sink in &self.blocking {
        let mut guard = match sink.lock() {
            Ok(g) => g,
            Err(e) => {
                event!(Level::ERROR, error.message = %e, "sink mutex poisoned, skipping");
                continue;
            }
        };
        if guard.accept(&record) {
            if let Err(e) = guard.write(record.clone()) {
                event!(
                    name: "bus.sink.write_error",
                    Level::WARN,
                    sink.name = guard.name(),
                    error.message = %e,
                    "blocking sink write failed",
                );
            }
        }
    }
    for sink in &self.async_sinks {
        let mut guard = match sink.lock() {
            Ok(g) => g,
            Err(e) => {
                event!(Level::ERROR, error.message = %e, "async sink mutex poisoned, skipping");
                continue;
            }
        };
        if guard.accept(&record) {
            if let Err(e) = guard.write(record.clone()) {
                event!(
                    name: "bus.sink.async_write_error",
                    Level::WARN,
                    sink.name = guard.name(),
                    error.message = %e,
                    "async sink write failed",
                );
            }
        }
    }
}
```

Same pattern for `flush_all()` and `shutdown_all()` — lock, map poison, call method.

Update `Debug` impl to match new field types.

- [ ] **Step 3: Remove internal Mutex from EventLogSink**

In `crates/argus/src/pipeline/sinks/event_log.rs`:

```rust
pub struct EventLogSink {
    log: EventLog,  // was Mutex<EventLog>
}

impl EventLogSink {
    pub fn new(log: EventLog) -> Self {
        Self { log }
    }

    pub fn with_log<F, T>(&mut self, f: F) -> T
    where
        F: FnOnce(&mut EventLog) -> T,
    {
        f(&mut self.log)
    }
}

impl Sink for EventLogSink {
    fn priority(&self) -> SinkPriority { SinkPriority::Blocking }
    fn accept(&self, record: &Record) -> bool { matches!(record, Record::Event(_)) }

    fn write(&mut self, record: Record) -> Result<()> {
        let Record::Event(event) = record else { return Ok(()) };
        self.log.append(&event, None)?;
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.log.flush()
    }

    fn name(&self) -> &str { "event-log" }
}
```

No `.expect("poisoned")` anywhere.

- [ ] **Step 4: Remove internal Mutex from MemorySink**

In `crates/argus/src/pipeline/sinks/memory.rs`:

```rust
pub struct MemorySink {
    records: Vec<Record>,  // was Mutex<Vec<Record>>
    priority: SinkPriority,
}

impl MemorySink {
    pub fn new(priority: SinkPriority) -> Self {
        Self { records: Vec::new(), priority }
    }

    pub fn drain(&mut self) -> Vec<Record> {
        std::mem::take(&mut self.records)
    }

    pub fn events(&self) -> Vec<Event> {
        self.records.iter().filter_map(|r| {
            if let Record::Event(e) = r { Some(e.clone()) } else { None }
        }).collect()
    }

    pub fn len(&self) -> usize { self.records.len() }
    pub fn is_empty(&self) -> bool { self.records.is_empty() }
}

impl Sink for MemorySink {
    fn priority(&self) -> SinkPriority { self.priority }
    fn accept(&self, _record: &Record) -> bool { true }
    fn write(&mut self, record: Record) -> Result<()> {
        self.records.push(record);
        Ok(())
    }
    fn flush(&mut self) -> Result<()> { Ok(()) }
    fn name(&self) -> &str { "memory" }
}
```

Note: `drain`, `events`, `len`, `is_empty` access now require the bus Mutex to be held by the caller. Tests that use `MemorySink` directly (not through bus) will access fields via `Arc<Mutex<MemorySink>>` — use `.lock().unwrap()` in tests.

- [ ] **Step 5: Remove internal Mutexes from IndexSink**

In `crates/argus/src/pipeline/sinks/index.rs`:

```rust
pub struct IndexSink {
    path_index: PathIndex,     // was Mutex<PathIndex>
    pid_index: PidIndex,       // was Mutex<PidIndex>
    type_index: TypeIndex,     // was Mutex<TypeIndex>
}
```

`write(&mut self)` accesses `self.path_index`, `self.pid_index`, `self.type_index` directly — no lock calls, no `.expect("poisoned")`.

Tests access fields directly since they own the sink.

- [ ] **Step 6: Remove internal Mutex from StdoutSink**

In `crates/argus/src/pipeline/sinks/stdout.rs`:

```rust
pub struct StdoutSink {
    out: BufWriter<io::Stdout>,  // was Mutex<BufWriter<io::Stdout>>
}

impl StdoutSink {
    pub fn new() -> Self {
        Self { out: BufWriter::new(io::stdout()) }
    }
}

impl Sink for StdoutSink {
    fn write(&mut self, record: Record) -> Result<()> {
        let Record::Event(event) = record else { return Ok(()) };
        let json = serde_json::to_string(&event)
            .with_context(|| format!("serialize event seq={}", event.seq))?;
        writeln!(self.out, "{json}").context("write event to stdout")?;
        self.out.flush().context("flush stdout after event")
    }

    fn flush(&mut self) -> Result<()> {
        self.out.flush().context("flush stdout sink")
    }
    // ...
}
```

- [ ] **Step 7: Update BroadcastSink, LocalCasSink, RemoteCasSink signatures**

These three sinks have no internal Mutex. Just change method signatures from `&self` to `&mut self`:

- `broadcast.rs`: `write(&mut self, ...)`, `flush(&mut self)`, `shutdown(&mut self)`
- `local_cas.rs`: `write(&mut self, ...)`, `flush(&mut self)`
- `remote_cas.rs`: `write(&mut self, ...)`, `flush(&mut self)`

No other changes needed.

- [ ] **Step 8: Update bus.rs tests**

Bus tests now wrap sinks in `Arc::new(Mutex::new(...))`:

```rust
fn emit_reaches_blocking_sink() {
    let sink = Arc::new(Mutex::new(MemorySink::new(SinkPriority::Blocking)));
    let bus = RecordBus::new(vec![sink.clone() as Arc<Mutex<dyn Sink>>]);
    bus.emit(make_record(1));
    assert_eq!(sink.lock().unwrap().len(), 1);
}
```

`FailSink` test: update `write(&mut self, ...)` signature.

- [ ] **Step 9: Update all sink-specific tests**

Sink tests that call `sink.write()` directly still work because the test owns the sink. Just update method signatures. Tests for `MemorySink` that call `drain()` or `len()` work directly since the test has exclusive ownership.

For `IndexSink` tests, access `sink.path_index` directly instead of `sink.path_index.lock().expect("lock")`.

- [ ] **Step 10: Update runtime.rs sink construction**

In `build_bus()`:

```rust
fn build_bus(
    local_cas: LocalCas,
    event_log: EventLog,
    upload_pool: Option<Arc<UploadPool>>,
    config: &SupervisorConfig,
    broadcast_tx: broadcast::Sender<Event>,
) -> RecordBus {
    let mut sinks: Vec<Arc<Mutex<dyn Sink>>> = vec![
        Arc::new(Mutex::new(StdoutSink::new())),
        Arc::new(Mutex::new(LocalCasSink::new(local_cas))),
        Arc::new(Mutex::new(EventLogSink::new(event_log))),
        Arc::new(Mutex::new(IndexSink::new(PathIndex::new(), PidIndex::new(), TypeIndex::new()))),
        Arc::new(Mutex::new(BroadcastSink::new(broadcast_tx))),
    ];

    if let Some(pool) = upload_pool {
        let cache_path = config.data_dir.join("digest-cache.bin");
        let digest_cache = Arc::new(DigestCache::new(cache_path));
        sinks.push(Arc::new(Mutex::new(RemoteCasSink::new(pool, digest_cache, config.agent_id.clone()))));
    }

    RecordBus::new(sinks)
}
```

Add `use std::sync::Mutex;` to runtime.rs imports.

- [ ] **Step 11: Build and test**

Run: `docker exec argus-arm64 cargo build --target aarch64-unknown-linux-musl -p supervisor`
Run: `docker exec argus-arm64 cargo test --target aarch64-unknown-linux-musl -p argus -p supervisor`

Expected: all compile, all tests pass.

- [ ] **Step 12: Commit**

```bash
git add crates/argus/src/pipeline/sink.rs crates/argus/src/pipeline/bus.rs \
  crates/argus/src/pipeline/sinks/ crates/argus/src/runtime.rs
git commit -m "refactor Sink trait to &mut self, push Mutex to RecordBus"
```

---

## Task 2: PipelineContext and StampStage Sharing

**Files:**
- Create: `crates/argus/src/pipeline/context.rs`
- Modify: `crates/argus/src/pipeline/mod.rs`
- Modify: `crates/argus/src/pipeline/stages/stamp.rs`
- Modify: `crates/argus/src/runtime.rs`

- [ ] **Step 1: Create PipelineContext**

Create `crates/argus/src/pipeline/context.rs`:

```rust
//! Shared context cloned into each independent pipeline.
//!
//! Contains only read-only or atomic resources — no mutable state.
//! Each pipeline gets a clone; the `Arc` handles ensure all pipelines
//! share the same underlying resources.

use std::sync::Arc;

use crate::cas::Cas;
use crate::events::SequenceGenerator;
use crate::pipeline::bus::RecordBus;

/// Read-only context shared across all pipelines.
///
/// Clone is cheap — all fields are `Arc` or `Clone`-friendly.
#[derive(Clone, Debug)]
pub struct PipelineContext {
    /// Single sequence generator shared across all pipelines.
    /// AtomicU64 internally — safe for concurrent use.
    pub seq: Arc<SequenceGenerator>,
    /// Content-addressed storage handle.
    pub cas: Arc<dyn Cas>,
    /// Fan-out bus to all sinks.
    pub bus: RecordBus,
    /// Agent identifier.
    pub agent_id: String,
}

impl PipelineContext {
    /// Create a new context.
    pub fn new(
        seq: Arc<SequenceGenerator>,
        cas: Arc<dyn Cas>,
        bus: RecordBus,
        agent_id: String,
    ) -> Self {
        Self { seq, cas, bus, agent_id }
    }
}
```

- [ ] **Step 2: Update StampStage to accept Arc<SequenceGenerator>**

In `crates/argus/src/pipeline/stages/stamp.rs`, change:

```rust
use std::sync::Arc;
use crate::events::envelope::SequenceGenerator;

pub struct StampStage {
    pub seq_gen: Arc<SequenceGenerator>,  // was SequenceGenerator (owned)
    pub agent_id: String,
}

impl StampStage {
    pub fn new(seq_gen: Arc<SequenceGenerator>, agent_id: String) -> Self {
        Self { seq_gen, agent_id }
    }
}
```

The `make_event` method calls `self.seq_gen.next_seq()` — this works unchanged because `SequenceGenerator::next_seq(&self)` uses `AtomicU64`. Each pipeline creates its own `StampStage` but they all share the same `Arc<SequenceGenerator>`, giving total ordering across pipelines.

Update tests to wrap in Arc:

```rust
fn stage() -> StampStage {
    StampStage::new(Arc::new(SequenceGenerator::new(0)), "test-agent".into())
}
```

- [ ] **Step 3: Register module in pipeline/mod.rs**

Add to `crates/argus/src/pipeline/mod.rs`:

```rust
pub(crate) mod context;
// ...
pub(crate) use context::PipelineContext;
```

- [ ] **Step 4: Build and test**

Run: `docker exec argus-arm64 cargo build --target aarch64-unknown-linux-musl -p supervisor`
Run: `docker exec argus-arm64 cargo test --target aarch64-unknown-linux-musl -p argus -p supervisor`

- [ ] **Step 5: Commit**

```bash
git add crates/argus/src/pipeline/context.rs crates/argus/src/pipeline/mod.rs \
  crates/argus/src/pipeline/stages/stamp.rs
git commit -m "add PipelineContext, share SequenceGenerator across pipelines"
```

---

## Task 3: Keylog Pipeline

**Files:**
- Create: `crates/argus/src/pipeline/keylog_pipeline.rs`
- Modify: `crates/argus/src/pipeline/mod.rs`

The keylog pipeline is the simplest: poll a file, parse lines, stamp, emit. Currently this logic lives inline in `runtime.rs::poll_keylog()`.

- [ ] **Step 1: Create keylog_pipeline.rs**

```rust
//! Keylog pipeline: SSLKEYLOGFILE → parse → stamp → emit.
//!
//! Runs on a dedicated thread, polling the keylog file at a fixed
//! interval. Each new key line becomes a TlsKeys event emitted
//! through the shared bus.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tracing::{event, Level};

use crate::events::{Event, EventPayload};
use crate::net::KeylogWatcher;
use crate::pipeline::context::PipelineContext;

/// Run the keylog pipeline until `stop` is set.
///
/// Designed to be spawned on a dedicated thread via `std::thread::spawn`.
pub fn run(
    keylog_path: PathBuf,
    ctx: PipelineContext,
    stop: Arc<AtomicBool>,
    poll_interval: Duration,
) {
    let mut watcher = KeylogWatcher::new(keylog_path.clone());

    event!(
        name: "pipeline.keylog.started",
        Level::INFO,
        keylog.path = %keylog_path.display(),
        "keylog pipeline started",
    );

    loop {
        if stop.load(Ordering::Acquire) {
            event!(Level::DEBUG, "keylog pipeline: stop flag set, draining");
            break;
        }

        poll_once(&mut watcher, &ctx);
        std::thread::sleep(poll_interval);
    }

    // Final drain so no TLS data is lost between last poll and shutdown.
    poll_once(&mut watcher, &ctx);

    event!(
        name: "pipeline.keylog.stopped",
        Level::INFO,
        "keylog pipeline stopped",
    );
}

fn poll_once(watcher: &mut KeylogWatcher, ctx: &PipelineContext) {
    match watcher.process_new_lines(&ctx.bus, 0, -1) {
        Ok(tls_events) => {
            for tls in tls_events {
                let evt = Event::new(&ctx.seq, ctx.agent_id.clone(), EventPayload::TlsKeys(tls));
                event!(
                    name: "pipeline.keylog.event",
                    Level::DEBUG,
                    event.seq = evt.seq,
                    "keylog pipeline emitting TlsKeys event",
                );
                ctx.bus.emit(crate::pipeline::Record::Event(evt));
            }
        }
        Err(e) => {
            event!(
                name: "pipeline.keylog.poll_error",
                Level::WARN,
                error.message = %e,
                "keylog poll failed",
            );
        }
    }
}
```

- [ ] **Step 2: Register in pipeline/mod.rs**

```rust
pub(crate) mod keylog_pipeline;
```

- [ ] **Step 3: Build and test**

Run: `docker exec argus-arm64 cargo build --target aarch64-unknown-linux-musl -p supervisor`

- [ ] **Step 4: Commit**

```bash
git add crates/argus/src/pipeline/keylog_pipeline.rs crates/argus/src/pipeline/mod.rs
git commit -m "add independent keylog pipeline"
```

---

## Task 4: Proxy Pipeline

**Files:**
- Create: `crates/argus/src/pipeline/proxy_pipeline.rs`
- Modify: `crates/argus/src/pipeline/mod.rs`

The proxy pipeline polls mitmdump's flow output file, parses HTTP flows, stamps, and emits. Currently in `runtime.rs::poll_flows()`.

- [ ] **Step 1: Create proxy_pipeline.rs**

```rust
//! Proxy pipeline: mitmdump flows → parse HTTP → extract bodies → stamp → emit.
//!
//! Runs on a dedicated thread, polling the flow output file at a fixed
//! interval. Each HTTP flow becomes one or two events (request +
//! optional response) emitted through the shared bus.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tracing::{event, Level};

use crate::events::Event;
use crate::net::FlowWatcher;
use crate::pipeline::context::PipelineContext;

/// Run the proxy pipeline until `stop` is set.
///
/// If `flow_path` is `None`, the pipeline exits immediately — no
/// mitmdump means no flows to process.
pub fn run(
    flow_path: Option<PathBuf>,
    ctx: PipelineContext,
    stop: Arc<AtomicBool>,
    poll_interval: Duration,
) {
    let Some(path) = flow_path else {
        event!(
            name: "pipeline.proxy.skipped",
            Level::INFO,
            "no flow output path, proxy pipeline not started",
        );
        return;
    };

    let mut watcher = FlowWatcher::new(path.clone());

    event!(
        name: "pipeline.proxy.started",
        Level::INFO,
        flow.path = %path.display(),
        "proxy pipeline started",
    );

    loop {
        if stop.load(Ordering::Acquire) {
            event!(Level::DEBUG, "proxy pipeline: stop flag set, draining");
            break;
        }

        poll_once(&mut watcher, &ctx);
        std::thread::sleep(poll_interval);
    }

    // Final drain.
    poll_once(&mut watcher, &ctx);

    event!(
        name: "pipeline.proxy.stopped",
        Level::INFO,
        "proxy pipeline stopped",
    );
}

fn poll_once(watcher: &mut FlowWatcher, ctx: &PipelineContext) {
    match watcher.process_new_flows(&ctx.bus, 0) {
        Ok(flows) => {
            for payload in FlowWatcher::into_event_payloads(flows) {
                let evt = Event::new(&ctx.seq, ctx.agent_id.clone(), payload);
                event!(
                    name: "pipeline.proxy.event",
                    Level::DEBUG,
                    event.seq = evt.seq,
                    event.type_ = evt.payload.event_type_tag(),
                    "proxy pipeline emitting event",
                );
                ctx.bus.emit(crate::pipeline::Record::Event(evt));
            }
        }
        Err(e) => {
            event!(
                name: "pipeline.proxy.poll_error",
                Level::WARN,
                error.message = %e,
                "flow poll failed",
            );
        }
    }
}
```

- [ ] **Step 2: Register in pipeline/mod.rs**

```rust
pub(crate) mod proxy_pipeline;
```

- [ ] **Step 3: Build and test**

Run: `docker exec argus-arm64 cargo build --target aarch64-unknown-linux-musl -p supervisor`

- [ ] **Step 4: Commit**

```bash
git add crates/argus/src/pipeline/proxy_pipeline.rs crates/argus/src/pipeline/mod.rs
git commit -m "add independent proxy pipeline"
```

---

## Task 5: Runtime Wiring and TLS Thread Removal

**Files:**
- Modify: `crates/argus/src/runtime.rs`
- Modify: `crates/supervisor/src/wiring.rs`

This task replaces the monolithic `tls_watcher_loop` with `PipelineContext`-based spawning of the two new pipelines, and threads `PipelineContext` into the ptrace pipeline.

- [ ] **Step 1: Update SupervisorRuntime to create PipelineContext**

In `runtime.rs`, change `SupervisorRuntime` fields:

```rust
pub struct SupervisorRuntime {
    config: SupervisorConfig,
    ctx: PipelineContext,
    shared: SharedState,
}
```

In `SupervisorRuntime::new()`:

```rust
let seq_gen = Arc::new(SequenceGenerator::default());
let ctx = PipelineContext::new(
    seq_gen,
    api_cas.clone(),
    bus,
    config.agent_id.clone(),
);
Ok(Self { config, ctx, shared })
```

Update `emit_agent_start` and `emit_initial_state` to use `self.ctx.bus` and `self.ctx.seq`.

- [ ] **Step 2: Replace spawn_tls_watcher with spawn_keylog and spawn_proxy**

Remove `spawn_tls_watcher`, `tls_watcher_loop`, `poll_keylog`, `poll_flows` entirely.

Add:

```rust
const TLS_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Spawn the keylog pipeline on a dedicated thread.
pub fn spawn_keylog_pipeline(&self) -> (JoinHandle<()>, Arc<AtomicBool>) {
    let keylog_path = self.config.tls.keylog_path.clone();
    let ctx = self.ctx.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();

    let handle = thread::Builder::new()
        .name("keylog-pipeline".into())
        .spawn(move || {
            crate::pipeline::keylog_pipeline::run(
                keylog_path, ctx, stop_clone, TLS_POLL_INTERVAL,
            );
        })
        .expect("failed to spawn keylog pipeline thread");

    (handle, stop)
}

/// Spawn the proxy pipeline on a dedicated thread.
pub fn spawn_proxy_pipeline(
    &self,
    flow_path: Option<PathBuf>,
) -> (JoinHandle<()>, Arc<AtomicBool>) {
    let ctx = self.ctx.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();

    let handle = thread::Builder::new()
        .name("proxy-pipeline".into())
        .spawn(move || {
            crate::pipeline::proxy_pipeline::run(
                flow_path, ctx, stop_clone, TLS_POLL_INTERVAL,
            );
        })
        .expect("failed to spawn proxy pipeline thread");

    (handle, stop)
}
```

- [ ] **Step 3: Update into_pipeline to use PipelineContext**

In `into_pipeline`, create `StampStage` from the shared seq generator:

```rust
let stamp_stage = StampStage::new(self.ctx.seq.clone(), self.config.agent_id.clone());
```

Pass `self.ctx.bus` to `PipelineRunner::new()` as before (bus is cloneable).

- [ ] **Step 4: Update wiring.rs for new pipeline handles**

```rust
let (keylog_handle, keylog_stop) = runtime.spawn_keylog_pipeline();
let (proxy_handle, proxy_stop) = runtime.spawn_proxy_pipeline(flow_path);

// ... later in shutdown:
fn shutdown(
    keylog_stop: Arc<AtomicBool>,
    keylog_handle: JoinHandle<()>,
    proxy_stop: Arc<AtomicBool>,
    proxy_handle: JoinHandle<()>,
    mitmdump: Option<&mut net::MitmdumpHandle>,
    ptrace_thread: JoinHandle<()>,
    api_shutdown_tx: tokio::sync::watch::Sender<bool>,
) -> Result<()> {
    keylog_stop.store(true, Ordering::Release);
    proxy_stop.store(true, Ordering::Release);
    let _ = keylog_handle.join();
    let _ = proxy_handle.join();
    event!(Level::DEBUG, "shutdown: keylog and proxy pipelines stopped");

    if let Some(m) = mitmdump {
        let _ = m.stop();
    }

    let _ = api_shutdown_tx.send(true);
    ptrace_thread.join().ok();

    event!(Level::DEBUG, "shutdown: all subsystems stopped");
    Ok(())
}
```

- [ ] **Step 5: Build and test**

Run: `docker exec argus-arm64 cargo build --target aarch64-unknown-linux-musl -p supervisor`
Run: `docker exec argus-arm64 cargo test --target aarch64-unknown-linux-musl -p argus -p supervisor`

- [ ] **Step 6: Commit**

```bash
git add crates/argus/src/runtime.rs crates/supervisor/src/wiring.rs
git commit -m "wire PipelineContext, replace tls_watcher with keylog+proxy pipelines"
```

---

## Task 6: Debug Logging Throughout Pipeline

**Files:**
- Modify: `crates/argus/src/pipeline/runner.rs`
- Modify: `crates/argus/src/pipeline/bus.rs`
- Modify: `crates/argus/src/pipeline/stages/stamp.rs`
- Modify: `crates/argus/src/pipeline/stages/capture.rs`
- Modify: `crates/argus/src/pipeline/stages/tree.rs`
- Modify: `crates/argus/src/pipeline/stages/classify.rs`

Add `tracing::debug!` at key decision points. Use structured fields — no string interpolation.

- [ ] **Step 1: Add debug logging to PipelineRunner::run()**

In `runner.rs`, add tracing at each stage transition:

```rust
// After classify:
tracing::debug!(
    name: "pipeline.ptrace.classified",
    pid = classified.pid.as_raw(),
    classification = ?classified.classification,
    "classified syscall stop",
);

// After passthrough skip:
tracing::trace!(
    name: "pipeline.ptrace.passthrough",
    pid = classified.pid.as_raw(),
    "passthrough, resuming immediately",
);

// After capture:
tracing::debug!(
    name: "pipeline.ptrace.captured",
    pid = captured.pid.as_raw(),
    has_content = !matches!(captured.content, CapturedContent::None),
    "content capture complete",
);

// After tree update:
tracing::debug!(
    name: "pipeline.ptrace.tree_updated",
    pid = captured.pid.as_raw(),
    has_tree_hash = tree_hash.is_some(),
    "tree stage complete",
);

// After stamp + emit:
tracing::debug!(
    name: "pipeline.ptrace.emitted",
    event.seq = event.seq,
    event.type_ = event.payload.event_type_tag(),
    "event emitted to bus",
);
```

Add at pipeline start and end:

```rust
tracing::info!(name: "pipeline.ptrace.started", "ptrace pipeline running");
// ... after loop ...
tracing::info!(name: "pipeline.ptrace.stopped", "ptrace pipeline finished, shutting down bus");
```

- [ ] **Step 2: Add debug logging to bus.rs**

Log total emit count and sink delivery:

```rust
// In emit():
tracing::trace!(
    name: "bus.emit",
    blocking_count = self.blocking.len(),
    async_count = self.async_sinks.len(),
    "delivering record to sinks",
);
```

- [ ] **Step 3: Add debug logging to stamp.rs**

```rust
// In stamp():
tracing::debug!(
    name: "pipeline.stamp",
    event.seq = evt.seq,
    event.type_ = evt.payload.event_type_tag(),
    pid,
    "stamped event",
);
```

- [ ] **Step 4: Add debug logging to capture.rs**

```rust
// At start of capture():
tracing::debug!(
    name: "pipeline.capture.start",
    pid = pid.as_raw(),
    classification = ?event.classification,
    "starting content capture",
);
```

In `capture_write`:

```rust
tracing::debug!(
    name: "pipeline.capture.write",
    pid = pid.as_raw(),
    path = %path.display(),
    len,
    level = ?level,
    "capturing write content",
);
```

- [ ] **Step 5: Add debug logging to tree.rs**

```rust
// In update():
tracing::debug!(
    name: "pipeline.tree.update",
    path = %path.display(),
    root_hash = %root,
    "tree updated",
);
```

- [ ] **Step 6: Build and test**

Run: `docker exec argus-arm64 cargo build --target aarch64-unknown-linux-musl -p supervisor`
Run: `docker exec argus-arm64 cargo test --target aarch64-unknown-linux-musl -p argus -p supervisor`

- [ ] **Step 7: Commit**

```bash
git add crates/argus/src/pipeline/
git commit -m "add debug logging throughout pipeline stages"
```

---

## Task 7: Validation and Fix Test 1

**Files:**
- Potentially: `crates/argus/src/pipeline/stages/classify.rs` or `runner.rs`

- [ ] **Step 1: Run validation test 1 with debug output**

```bash
docker exec argus-arm64 bash -c 'RUST_LOG=debug ./tests/validate.sh 1 2>test1-debug.log'
```

Examine `test1-debug.log` for what stops are classified and whether exec events flow through to stamp/emit.

- [ ] **Step 2: Diagnose and fix**

With the debug logging from Task 6, the trace should show where exec events are dropped. Common causes:
- `Classification::ProcessExec` might be filtered as `Passthrough` somewhere
- The classify stage might not be producing `ProcessExec` at all
- The capture stage might swallow it (though `_ => CapturedContent::None` should pass it through)

Fix the root cause. This step depends on diagnosis.

- [ ] **Step 3: Run full validation suite**

```bash
docker exec argus-arm64 ./tests/validate.sh
```

All 13 tests must pass.

- [ ] **Step 4: Commit**

```bash
git add <fixed files>
git commit -m "fix test 1: <root cause description>"
```

---

## Task 8: Final Review

- [ ] **Step 1: Run full test suite**

```bash
docker exec argus-arm64 cargo test --target aarch64-unknown-linux-musl -p argus -p supervisor
docker exec argus-arm64 ./tests/validate.sh
```

- [ ] **Step 2: Code review**

Run `/code-review:code-review` against all changed files.

- [ ] **Step 3: Update task doc**

Create or update `docs/tasks/pipeline-refactor.md` with status, what was done, what works, how to test.
