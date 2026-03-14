# SupervisorRuntime Facade Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move internal pipeline/storage wiring out of the supervisor binary into an `argus::runtime` facade, shrinking the public API surface from ~130 items to ~30.

**Architecture:** A new `SupervisorRuntime` struct owns the bus, sequence generator, and config references. It provides `new()` (async, constructs storage + sinks + bus), `emit_agent_start()`, `emit_initial_state()`, `spawn_tls_watcher()`, and `into_pipeline()` (constructs stages, returns `PipelineRunner`). `PipelineRunner` fields become private; construction moves into `into_pipeline`. The supervisor binary becomes a thin shell: config + TLS + fork + runtime + API + shutdown.

**Tech Stack:** Rust, tokio, anyhow, dashmap, nix, arc-swap

**Constraints:**
- All builds/tests run inside `argus-arm64` container with `--target aarch64-unknown-linux-musl`
- `ContentHash` stays fully `pub` (CLI needs it, `Cas` trait exposes it)
- `emit_agent_start(&self)` and `emit_initial_state(&self)` borrow; `into_pipeline(self)` consumes

---

## File Structure

| Action | File | Responsibility |
|-|-|-|
| Create | `crates/argus/src/runtime.rs` | `SupervisorRuntime` facade: storage init, bus construction, stage wiring, initial event emission, TLS watcher spawning |
| Modify | `crates/argus/src/lib.rs` | Add `pub mod runtime` |
| Modify | `crates/argus/src/pipeline/runner.rs` | Make all fields private, remove struct-literal construction, add private `fn new()` called only from runtime |
| Modify | `crates/supervisor/src/wiring.rs` | Replace ~300 lines of internal construction with `SupervisorRuntime` calls |
| Modify | `crates/supervisor/src/tls_watcher.rs` | Delete — TLS watcher moves into `SupervisorRuntime::spawn_tls_watcher` |
| Modify | `crates/supervisor/src/main.rs` | Remove `mod tls_watcher` |
| Modify | `crates/argus/src/pipeline/mod.rs` | Downgrade internal submodules to `pub(crate) mod`, keep only cross-crate re-exports as `pub use` |
| Modify | `crates/argus/src/pipeline/stages/mod.rs` | Downgrade to `pub(crate) mod` + `pub(crate) use` |
| Modify | `crates/argus/src/pipeline/sinks/mod.rs` | Downgrade to `pub(crate) mod` + `pub(crate) use` |
| Modify | `crates/argus/src/state/mod.rs` | Downgrade internal re-exports to `pub(crate) use` where not cross-crate |
| Modify | `crates/argus/src/storage/mod.rs` | Downgrade internal re-exports |
| Modify | `crates/argus/src/index/mod.rs` | Downgrade internal re-exports |
| Modify | `crates/argus/src/config/mod.rs` | Downgrade `read_nspid_pair`, `read_host_pid` to `pub(crate)` |
| Modify | `crates/argus/src/net/mod.rs` | Keep cross-crate items `pub`, downgrade internals |
| Modify | `crates/argus/src/events/mod.rs` | Keep event types `pub`, downgrade `timestamp_pair` to `pub(crate)` |
| Modify | `crates/argus/src/tracer/mod.rs` | Keep `seccomp` `pub` (startup uses it), downgrade others |
| Modify | `crates/argus/src/api/mod.rs` | Keep `serve`, `state`, `types` `pub`; downgrade `routes`, `errors` |

---

## Chunk 1: Create SupervisorRuntime and move wiring into argus

### Task 1: Create `runtime.rs` with `SupervisorRuntime`

**Files:**
- Create: `crates/argus/src/runtime.rs`
- Modify: `crates/argus/src/lib.rs`

- [ ] **Step 1: Create the runtime module with SupervisorRuntime struct**

```rust
// crates/argus/src/runtime.rs
//! High-level facade for supervisor startup wiring.
//!
//! Constructs storage, sinks, bus, stages, and TLS watcher internally
//! so the supervisor binary only deals with config, process lifecycle,
//! and shutdown coordination.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result};
use dashmap::DashMap;
use nix::unistd::Pid;
use tokio::sync::broadcast;
use tracing::{Level, event};

use crate::api::state::{SharedState, new_shared_state};
use crate::approver::Approvers;
use crate::cas::{Cas, ContentHash, LocalCas};
use crate::config::SupervisorConfig;
use crate::events::{Event, EventPayload, SequenceGenerator};
use crate::events::snapshot::{InitialFile, InitialState};
use crate::index::{PathIndex, PidIndex, TypeIndex};
use crate::net::{FlowWatcher, KeylogWatcher};
use crate::pipeline::bus::RecordBus;
use crate::pipeline::capture_policy::CapturePolicy;
use crate::pipeline::record::Record;
use crate::pipeline::runner::PipelineRunner;
use crate::pipeline::sink::{Sink, SinkPriority};
use crate::pipeline::sinks::{
    BroadcastSink, EventLogSink, IndexSink, LocalCasSink, RemoteCasSink, StdoutSink,
};
use crate::pipeline::stages::{
    ApprovalStage, CaptureStage, CheckRulesStage, ClassifyStage, StampStage, TreeStage,
};
use crate::pipeline::ptrace_thread::PtraceStream;
use crate::pipeline::replay::RawStopRecorder;
use crate::snapshot::MerkleTree;
use crate::state::{FdTable, PipeRegistry, PtyRegistry};
use crate::storage::{DigestCache, DynObjectStore, EventLog, S3Client, UploadPool};

/// High-level facade that owns the bus, sequence generator, and shared
/// state. Constructs all internal pipeline components so the supervisor
/// binary never touches sinks, stages, or internal state types.
pub struct SupervisorRuntime {
    config: SupervisorConfig,
    bus: RecordBus,
    seq_gen: SequenceGenerator,
    shared: SharedState,
}

impl SupervisorRuntime {
    /// Initialize storage (CAS, EventLog, UploadPool), sinks, and bus.
    ///
    /// # Errors
    ///
    /// Returns an error if CAS directory creation, event log init, or
    /// S3 client setup fails.
    pub async fn new(config: SupervisorConfig) -> Result<Self> {
        let data_dir = &config.data_dir;
        let cas_path = data_dir.join("cas");

        let sink_cas = LocalCas::new(cas_path.clone())
            .context("failed to initialize sink CAS")?;
        let event_log = EventLog::new(
            config.agent_id.clone(),
            data_dir.join("events"),
            config.durability.default,
        )
        .context("failed to initialize event log")?;
        let upload_pool = build_upload_pool(&config).await?;

        let (broadcast_tx, _) = broadcast::channel::<Event>(4096);
        let bus = build_bus(sink_cas, event_log, upload_pool, &config, broadcast_tx);

        let seq_gen = SequenceGenerator::default();

        let api_cas: Arc<dyn Cas> = Arc::new(
            LocalCas::new(cas_path).context("failed to initialize API CAS handle")?,
        );
        let shared = new_shared_state(config.agent_id.clone(), api_cas, bus.clone());
        shared.store_rules(config.build_ruleset());

        Ok(Self { config, bus, seq_gen, shared })
    }

    /// Shared state handle for the API server.
    pub fn shared_state(&self) -> SharedState {
        self.shared.clone()
    }

    /// Emit the `AgentStart` control event through the bus.
    pub fn emit_agent_start(&self) {
        let nspid = crate::config::read_nspid_pair();

        let payload = EventPayload::AgentStart(crate::events::control::AgentStart {
            agent_id: self.config.agent_id.clone(),
            supervisor_pid_host: nspid.map(|(h, _)| h),
            supervisor_pid_ns: nspid.map(|(_, n)| n),
            config_summary: format!(
                "data_dir={}, workspace={}",
                self.config.data_dir.display(),
                self.config.workspace_dir.display(),
            ),
            node: std::env::var("NODE_NAME").ok(),
            pod: std::env::var("POD_NAME").ok(),
            container: std::env::var("CONTAINER_NAME").ok(),
        });

        let evt = Event::new(&self.seq_gen, self.config.agent_id.clone(), payload);
        self.bus.emit(Record::Event(evt));
    }

    /// Walk the workspace and emit `InitialFile` + `InitialState` events.
    pub fn emit_initial_state(&self) {
        let workspace = &self.config.workspace_dir;
        let mut file_count: u64 = 0;
        let mut total_size: u64 = 0;
        let mut tree = MerkleTree::new();

        walk_dir_recursive(workspace, &mut |path: &std::path::Path| {
            let meta = match path.metadata() {
                Ok(m) if m.is_file() => m,
                _ => return,
            };

            use std::os::unix::fs::MetadataExt;
            let size = meta.len();
            let mode = meta.mode();

            let hash = match std::fs::read(path) {
                Ok(data) => ContentHash::from_data(&data),
                Err(_) => return,
            };

            let content_hash = hash.to_string();
            tree.update(path.to_path_buf(), hash);

            let payload = EventPayload::InitialFile(InitialFile {
                pid: 0,
                path: path.to_string_lossy().into(),
                content_hash,
                size,
                mode,
            });
            let evt = Event::new(&self.seq_gen, self.config.agent_id.clone(), payload);
            self.bus.emit(Record::Event(evt));

            file_count += 1;
            total_size += size;
        });

        let tree_hash = if file_count > 0 {
            Some(tree.root_hash().to_string())
        } else {
            None
        };

        let payload = EventPayload::InitialState(InitialState {
            tree_hash,
            file_count,
            total_size,
        });
        let evt = Event::new(&self.seq_gen, self.config.agent_id.clone(), payload);
        self.bus.emit(Record::Event(evt));
    }

    /// Spawn the TLS watcher thread.
    ///
    /// Returns `(join_handle, stop_flag)`. Set stop_flag to `true` and
    /// join the handle during shutdown.
    pub fn spawn_tls_watcher(
        &self,
        flow_path: Option<PathBuf>,
    ) -> (JoinHandle<()>, Arc<AtomicBool>) {
        let keylog_path = self.config.tls.keylog_path.clone();
        let bus = self.bus.clone();
        // TLS sequences start at 1_000_000 to avoid collision with the
        // tracer generator without coordination between threads.
        let tls_seq = SequenceGenerator::new(1_000_000);
        let agent_id = self.config.agent_id.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();

        let handle = thread::Builder::new()
            .name("tls-watcher".into())
            .spawn(move || {
                tls_watcher_loop(keylog_path, flow_path, bus, tls_seq, agent_id, stop_clone);
            })
            .expect("failed to spawn tls-watcher thread");

        (handle, stop)
    }

    /// Construct all pipeline stages and return the runner.
    ///
    /// Consumes `self` — the bus, seq_gen, and config move into the
    /// runner. Call `emit_agent_start` and `emit_initial_state` before
    /// this.
    pub fn into_pipeline(self, child_pid: Pid) -> (PipelineRunner, std::thread::JoinHandle<()>) {
        let (ptrace_stream, ptrace_thread) = PtraceStream::spawn(child_pid);
        let handle = ptrace_stream.handle();

        let fd_tables: Arc<DashMap<Pid, FdTable>> = Arc::new(DashMap::new());
        let pipe_registry = Arc::new(Mutex::new(PipeRegistry::new()));
        let pty_registry = Arc::new(Mutex::new(PtyRegistry::new()));

        let transparent_mode = matches!(
            self.config.tls.proxy_mode,
            crate::config::ProxyMode::Transparent
        );
        let proxy_addr = std::net::SocketAddr::new(
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            self.config.tls.mitm_proxy_port,
        );

        let file_state = Arc::new(DashMap::new());

        let classify = ClassifyStage::new(
            handle.clone(),
            fd_tables,
            pipe_registry,
            pty_registry,
            transparent_mode,
            proxy_addr,
            file_state.clone(),
        );
        let rules_stage = CheckRulesStage::new(self.shared.rules_handle());
        let approvals = ApprovalStage::new(Approvers::new());

        let policy = CapturePolicy::default_full();
        let capture_stage = CaptureStage::new(
            handle.clone(),
            self.bus.clone(),
            policy,
            file_state,
        );

        let tree_stage = TreeStage::new(MerkleTree::new(), self.bus.clone(), 1000);
        let stamp_stage = StampStage::new(self.seq_gen, self.config.agent_id.clone());

        let recorder: Option<RawStopRecorder> = None;

        let runner = PipelineRunner::new(
            ptrace_stream,
            classify,
            rules_stage,
            approvals,
            capture_stage,
            tree_stage,
            stamp_stage,
            self.bus,
            recorder,
            self.shared.pause_flag(),
            self.shared,
        );

        (runner, ptrace_thread)
    }

}

/// Constructs the upload pool if S3 is configured.
async fn build_upload_pool(config: &SupervisorConfig) -> Result<Option<Arc<UploadPool>>> {
    let Some(ref s3_config) = config.storage.s3 else {
        event!(
            name: "runtime.storage.local_only",
            Level::INFO,
            "no S3 config, running in local-only mode",
        );
        return Ok(None);
    };

    let s3_client = S3Client::new(s3_config)
        .await
        .context("failed to create S3 client")?;
    let dyn_store = DynObjectStore::new(s3_client);
    let pool = UploadPool::new(dyn_store, &config.storage.upload);

    event!(
        name: "runtime.storage.s3",
        Level::INFO,
        s3.bucket = %s3_config.bucket,
        s3.endpoint = s3_config.endpoint.as_deref().unwrap_or("default"),
        "storage pipeline initialized with S3 backend",
    );

    Ok(Some(Arc::new(pool)))
}

/// Constructs the `RecordBus` from all configured sinks.
fn build_bus(
    local_cas: LocalCas,
    event_log: EventLog,
    upload_pool: Option<Arc<UploadPool>>,
    config: &SupervisorConfig,
    broadcast_tx: broadcast::Sender<Event>,
) -> RecordBus {
    let mut sinks: Vec<Arc<dyn Sink>> = vec![
        Arc::new(StdoutSink::new()),
        Arc::new(LocalCasSink::new(local_cas)),
        Arc::new(EventLogSink::new(event_log)),
        Arc::new(IndexSink::new(PathIndex::new(), PidIndex::new(), TypeIndex::new())),
        Arc::new(BroadcastSink::new(broadcast_tx)),
    ];

    if let Some(pool) = upload_pool {
        let cache_path = config.data_dir.join("digest-cache.bin");
        let digest_cache = Arc::new(DigestCache::new(cache_path));
        sinks.push(Arc::new(RemoteCasSink::new(
            pool,
            digest_cache,
            config.agent_id.clone(),
        )));
    }

    RecordBus::new(sinks)
}

const TLS_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Polling loop for TLS keylog and flow data.
fn tls_watcher_loop(
    keylog_path: PathBuf,
    flow_output: Option<PathBuf>,
    bus: RecordBus,
    seq_gen: SequenceGenerator,
    agent_id: String,
    stop: Arc<AtomicBool>,
) {
    let mut keylog = KeylogWatcher::new(keylog_path);
    let mut flow = flow_output.map(FlowWatcher::new);

    event!(
        name: "tls_watcher.started",
        Level::INFO,
        "TLS watcher thread started",
    );

    loop {
        if stop.load(Ordering::Acquire) {
            break;
        }

        poll_keylog(&mut keylog, &bus, &seq_gen, &agent_id);

        if let Some(ref mut fw) = flow {
            poll_flows(fw, &bus, &seq_gen, &agent_id);
        }

        thread::sleep(TLS_POLL_INTERVAL);
    }

    // Final drain.
    poll_keylog(&mut keylog, &bus, &seq_gen, &agent_id);
    if let Some(ref mut fw) = flow {
        poll_flows(fw, &bus, &seq_gen, &agent_id);
    }

    event!(
        name: "tls_watcher.stopped",
        Level::INFO,
        "TLS watcher thread stopped",
    );
}

fn poll_keylog(
    watcher: &mut KeylogWatcher,
    bus: &RecordBus,
    seq_gen: &SequenceGenerator,
    agent_id: &str,
) {
    match watcher.process_new_lines(bus, 0, -1) {
        Ok(tls_events) => {
            for tls in tls_events {
                let evt = Event::new(seq_gen, agent_id.to_owned(), EventPayload::TlsKeys(tls));
                bus.emit(Record::Event(evt));
            }
        }
        Err(e) => {
            event!(
                name: "tls_watcher.keylog.error",
                Level::WARN,
                error.message = %e,
                "keylog poll failed: {{error.message}}",
            );
        }
    }
}

fn poll_flows(
    watcher: &mut FlowWatcher,
    bus: &RecordBus,
    seq_gen: &SequenceGenerator,
    agent_id: &str,
) {
    match watcher.process_new_flows(bus, 0) {
        Ok(flows) => {
            for payload in FlowWatcher::into_event_payloads(flows) {
                let evt = Event::new(seq_gen, agent_id.to_owned(), payload);
                bus.emit(Record::Event(evt));
            }
        }
        Err(e) => {
            event!(
                name: "tls_watcher.flow.error",
                Level::WARN,
                error.message = %e,
                "flow poll failed: {{error.message}}",
            );
        }
    }
}

fn walk_dir_recursive(dir: &std::path::Path, cb: &mut dyn FnMut(&std::path::Path)) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_dir_recursive(&path, cb);
        } else {
            cb(&path);
        }
    }
}
```

- [ ] **Step 2: Add `pub mod runtime` to lib.rs**

Add after the last existing `pub mod` line in `crates/argus/src/lib.rs`:
```rust
pub mod runtime;
```

- [ ] **Step 3: Build argus crate to verify it compiles**

Run: `docker exec argus-arm64 cargo check --target aarch64-unknown-linux-musl -p argus`
Expected: PASS (possibly with unused warnings — that's fine at this stage)

- [ ] **Step 4: Commit**

```bash
git add crates/argus/src/runtime.rs crates/argus/src/lib.rs
git commit -m "add SupervisorRuntime facade for internal wiring"
```

---

### Task 2: Make PipelineRunner fields private, add constructor

**Files:**
- Modify: `crates/argus/src/pipeline/runner.rs`

- [ ] **Step 1: Replace pub fields with private fields + constructor**

In `crates/argus/src/pipeline/runner.rs`, change the `PipelineRunner` struct from pub fields to private fields:

```rust
pub struct PipelineRunner {
    ptrace: PtraceStream,
    classify: ClassifyStage,
    rules: CheckRulesStage,
    approvals: ApprovalStage,
    capture: CaptureStage,
    tree: TreeStage,
    stamp: StampStage,
    bus: RecordBus,
    recorder: Option<RawStopRecorder>,
    paused: Arc<AtomicBool>,
    shared: SharedState,
}
```

Add a `pub(crate) fn new(...)` constructor right after the struct definition inside the existing `impl PipelineRunner` block (before `pub async fn run`):

```rust
impl PipelineRunner {
    /// Construct a new pipeline runner.
    ///
    /// Called by `SupervisorRuntime::into_pipeline`; not part of the
    /// public API.
    pub(crate) fn new(
        ptrace: PtraceStream,
        classify: ClassifyStage,
        rules: CheckRulesStage,
        approvals: ApprovalStage,
        capture: CaptureStage,
        tree: TreeStage,
        stamp: StampStage,
        bus: RecordBus,
        recorder: Option<RawStopRecorder>,
        paused: Arc<AtomicBool>,
        shared: SharedState,
    ) -> Self {
        Self {
            ptrace, classify, rules, approvals, capture,
            tree, stamp, bus, recorder, paused, shared,
        }
    }

    // ... existing wait_if_paused() method unchanged
}
```

Also modify `run()` to flush sinks after the pipeline loop. At the end of `pub async fn run(mut self)`, after the `while let Some(stop)` loop exits, add:

```rust
        // (end of while loop)
        self.bus.shutdown_all();
    }
```

This ensures sinks are flushed before the runner is dropped, since `into_pipeline` consumed the runtime's bus.

- [ ] **Step 2: Build argus crate**

Run: `docker exec argus-arm64 cargo check --target aarch64-unknown-linux-musl -p argus`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add crates/argus/src/pipeline/runner.rs
git commit -m "make PipelineRunner fields private, add pub(crate) constructor"
```

---

### Task 3: Rewrite supervisor wiring to use SupervisorRuntime

**Files:**
- Modify: `crates/supervisor/src/wiring.rs`
- Modify: `crates/supervisor/src/main.rs`
- Delete: `crates/supervisor/src/tls_watcher.rs`

- [ ] **Step 1: Rewrite wiring.rs**

Replace the entire contents of `crates/supervisor/src/wiring.rs` with:

```rust
//! Async startup wiring using the argus runtime facade.
//!
//! Called from `main` after CLI parsing and TLS setup.

use anyhow::{Result};
use tracing::{Level, event};

use argus::api;
use argus::api::state::SharedState;
use argus::net;
use argus::runtime::SupervisorRuntime;
use argus::config::SupervisorConfig;

/// Top-level async entry point: initializes runtime, API, and pipeline.
///
/// # Errors
///
/// Returns an error if any subsystem fails to initialize.
pub async fn run(
    config: SupervisorConfig,
    agent_env: std::collections::HashMap<String, String>,
    mut mitmdump: Option<net::MitmdumpHandle>,
) -> Result<()> {
    let flow_path = mitmdump.as_ref().and_then(|m| m.flow_output_path().cloned());

    let runtime = SupervisorRuntime::new(config.clone()).await?;
    let shared = runtime.shared_state();

    let (api_shutdown_tx, api_shutdown_rx) = tokio::sync::watch::channel(false);
    spawn_api_server(shared.clone(), &config, api_shutdown_rx);

    // emit_agent_start before spawn_agent — matches the original ordering
    // so the event log records supervisor readiness before the tracee starts.
    runtime.emit_agent_start();

    let spawn = crate::startup::spawn_agent(
        &config.agent_command,
        &agent_env,
        &config.workspace_dir,
        config.run_as.as_ref(),
    )?;
    let _stdout_drain = crate::spawn_drain_thread("stdout", spawn.stdout_r);
    let _stderr_drain = crate::spawn_drain_thread("stderr", spawn.stderr_r);

    crate::signals::install_handler();

    let _ = nix::unistd::close(spawn.sync_pipe_w);

    let (tls_handle, tls_stop) = runtime.spawn_tls_watcher(flow_path);

    runtime.emit_initial_state();

    let (runner, ptrace_thread) = runtime.into_pipeline(spawn.child_pid);

    event!(Level::DEBUG, "wiring: entering pipeline.run()");
    runner.run().await;
    event!(Level::DEBUG, "wiring: pipeline.run() returned, beginning shutdown");

    shutdown(tls_stop, tls_handle, mitmdump.as_mut(), &shared, ptrace_thread, api_shutdown_tx)?;

    Ok(())
}

fn spawn_api_server(
    shared: SharedState,
    config: &SupervisorConfig,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
) {
    let listen_addr = config.listen_addr;
    tokio::spawn(async move {
        if let Err(e) = api::serve(shared, listen_addr, shutdown_rx).await {
            event!(
                name: "supervisor.api.error",
                Level::ERROR,
                error.message = %e,
                "API server failed: {{error.message}}",
            );
        }
    });

    event!(
        name: "supervisor.api.started",
        Level::INFO,
        listen.addr = %listen_addr,
        "API server listening on {{listen.addr}}",
    );
}

fn shutdown(
    tls_stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    tls_handle: std::thread::JoinHandle<()>,
    mitmdump: Option<&mut net::MitmdumpHandle>,
    shared: &SharedState,
    ptrace_thread: std::thread::JoinHandle<()>,
    api_shutdown_tx: tokio::sync::watch::Sender<bool>,
) -> Result<()> {
    use std::sync::atomic::Ordering;

    tls_stop.store(true, Ordering::Release);
    let _ = tls_handle.join();
    event!(Level::DEBUG, "shutdown: tls-watcher stopped");

    if let Some(m) = mitmdump {
        event!(Level::DEBUG, "shutdown: stopping mitmdump");
        let _ = m.stop();
        event!(Level::DEBUG, "shutdown: mitmdump stopped");
    }

    // Bus is shut down inside PipelineRunner::run() after the pipeline
    // loop exits, before the runner is dropped.
    let _ = api_shutdown_tx.send(true);
    ptrace_thread.join().ok();

    event!(Level::DEBUG, "shutdown: all subsystems stopped");
    Ok(())
}
```

- [ ] **Step 2: Remove `mod tls_watcher` from main.rs and delete the file**

In `crates/supervisor/src/main.rs`, remove the line:
```rust
mod tls_watcher;
```

Delete `crates/supervisor/src/tls_watcher.rs`.

- [ ] **Step 3: Build the whole workspace**

Run: `docker exec argus-arm64 cargo check --target aarch64-unknown-linux-musl --workspace`
Expected: PASS (fix any compilation errors iteratively)

Note: `config.clone()` works because `SupervisorConfig` derives `Clone`. Bus shutdown is handled by `PipelineRunner::run()` (added in Task 2).

- [ ] **Step 4: Commit**

```bash
git add crates/supervisor/src/wiring.rs crates/supervisor/src/main.rs
git rm crates/supervisor/src/tls_watcher.rs
git commit -m "rewrite supervisor wiring to use SupervisorRuntime facade"
```

---

## Chunk 2: Tighten visibility across argus crate

### Task 4: Downgrade pipeline module visibility

**Files:**
- Modify: `crates/argus/src/pipeline/mod.rs`
- Modify: `crates/argus/src/pipeline/stages/mod.rs`
- Modify: `crates/argus/src/pipeline/sinks/mod.rs`

- [ ] **Step 1: Downgrade pipeline/mod.rs submodules and re-exports**

Only `PipelineRunner` (via `runner` module) needs to be accessible from the supervisor. Everything else is internal. In `crates/argus/src/pipeline/mod.rs`:

```rust
// Submodules — all internal except runner
pub(crate) mod bus;
pub(crate) mod capture_policy;
#[cfg(test)]
pub(crate) mod mock_ptrace;
pub(crate) mod captured;
pub(crate) mod classified;
pub(crate) mod directive;
pub(crate) mod ptrace_thread;
pub(crate) mod raw_stop;
pub(crate) mod record;
pub(crate) mod replay;
pub mod runner;
pub(crate) mod sink;
pub(crate) mod sinks;
pub(crate) mod stages;

// Only re-export what external crates actually use
pub use runner::PipelineRunner;
```

Remove all other `pub use` re-exports (RecordBus, CapturePolicy, etc.) — they are no longer needed externally.

- [ ] **Step 2: Downgrade pipeline/stages/mod.rs**

```rust
pub(crate) mod approvals;
pub(crate) mod capture;
pub(crate) mod check_rules;
pub(crate) mod classify;
pub(crate) mod sockaddr;
pub(crate) mod stamp;
pub(crate) mod syscall_handlers;
pub(crate) mod tree;

pub(crate) use approvals::ApprovalStage;
pub(crate) use capture::CaptureStage;
pub(crate) use check_rules::CheckRulesStage;
pub(crate) use classify::ClassifyStage;
pub(crate) use stamp::StampStage;
pub(crate) use tree::TreeStage;
```

- [ ] **Step 3: Downgrade pipeline/sinks/mod.rs**

```rust
pub(crate) mod broadcast;
pub(crate) mod event_log;
pub(crate) mod index;
pub(crate) mod local_cas;
pub(crate) mod memory;
pub(crate) mod remote_cas;
pub(crate) mod stdout;

pub(crate) use broadcast::BroadcastSink;
pub(crate) use event_log::EventLogSink;
pub(crate) use index::IndexSink;
pub(crate) use local_cas::LocalCasSink;
pub(crate) use memory::MemorySink;
pub(crate) use remote_cas::RemoteCasSink;
pub(crate) use stdout::StdoutSink;
```

- [ ] **Step 4: Build workspace**

Run: `docker exec argus-arm64 cargo check --target aarch64-unknown-linux-musl --workspace`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/argus/src/pipeline/mod.rs crates/argus/src/pipeline/stages/mod.rs crates/argus/src/pipeline/sinks/mod.rs
git commit -m "downgrade pipeline internals to pub(crate)"
```

---

### Task 5: Downgrade remaining module visibility

**Files:**
- Modify: `crates/argus/src/state/mod.rs`
- Modify: `crates/argus/src/storage/mod.rs`
- Modify: `crates/argus/src/index/mod.rs`
- Modify: `crates/argus/src/config/mod.rs`
- Modify: `crates/argus/src/events/mod.rs`
- Modify: `crates/argus/src/net/mod.rs`
- Modify: `crates/argus/src/tracer/mod.rs`
- Modify: `crates/argus/src/api/mod.rs`
- Modify: `crates/argus/src/cas/mod.rs`

- [ ] **Step 1: state/mod.rs — downgrade internal re-exports**

State types are only used within argus (by runtime and stages). Change all `pub` to `pub(crate)`:

```rust
mod fd_serde;
pub(crate) mod fd_table;
pub(crate) mod pipe_registry;
pub(crate) mod process_tree;
pub(crate) mod pty_registry;
pub(crate) mod write_capture;
pub(crate) mod write_locks;

pub(crate) use fd_table::{FdTable, FdTarget, PipeEnd, PtyRole};
pub(crate) use pipe_registry::{PipeInfo, PipeRegistry};
pub(crate) use process_tree::{ProcessState, ProcessTree};
pub(crate) use pty_registry::{PtyInfo, PtyRegistry};
pub(crate) use write_locks::WriteLocks;
```

Also change `pub mod state` to `pub(crate) mod state` in `lib.rs`.

- [ ] **Step 2: storage/mod.rs — downgrade internal re-exports**

Storage is only used within argus (by runtime and sinks):

```rust
pub(crate) mod digest_cache;
pub(crate) mod event_log;
pub(crate) mod local_buffer;
pub(crate) mod object_store_dyn;
pub(crate) mod s3;
pub(crate) mod upload_job;
pub(crate) mod upload_pool;

pub(crate) use digest_cache::{DigestCache, DigestCacheStats, DigestEntry};
pub(crate) use event_log::EventLog;
pub(crate) use local_buffer::LocalBuffer;
pub(crate) use object_store_dyn::DynObjectStore;
pub(crate) use s3::{ObjectStore, S3Client};
pub(crate) use upload_job::UploadJob;
pub(crate) use upload_pool::{UploadConfirmation, UploadPool, UploadStats, UploadStatsSnapshot};
```

Also change `pub mod storage` to `pub(crate) mod storage` in `lib.rs`.

- [ ] **Step 3: index/mod.rs — downgrade internal re-exports**

Indexes are only used within argus:

```rust
pub(crate) mod path_index;
pub(crate) mod pid_index;
pub(crate) mod query;
pub(crate) mod type_index;

pub(crate) struct IndexEntry { /* unchanged fields */ }

pub(crate) use path_index::PathIndex;
pub(crate) use pid_index::{PidIndex, ProcessInfo};
pub(crate) use query::{QueryEngine, QueryFilter, QueryResult};
pub(crate) use type_index::TypeIndex;
```

Also change `pub mod index` to `pub(crate) mod index` in `lib.rs`.

- [ ] **Step 4: config/mod.rs — downgrade internal helpers**

Keep `SupervisorConfig`, `ProxyMode`, `RunAs`, `TlsConfig`, `RuleSet` etc. as `pub` (supervisor needs them). But downgrade internal helpers:

Change `read_host_pid` and `read_nspid_pair` from `pub fn` to `pub(crate) fn`.

- [ ] **Step 5: events/mod.rs — keep types pub, downgrade timestamp_pair**

Event types must stay `pub` (CLI uses them indirectly through API). But `timestamp_pair` is internal:

Change its re-export from `pub use envelope::{Event, EventPayload, SequenceGenerator, timestamp_pair}` to:
```rust
pub use envelope::{Event, EventPayload, SequenceGenerator};
pub(crate) use envelope::timestamp_pair;
```

Keep `pub mod control` and `pub mod snapshot` since the supervisor no longer accesses them directly — but the API types module references them. Check: if only crate-internal code uses these submodules, change to `pub(crate) mod`. If `api::types` or `api::state` reference `crate::events::control::*`, then `pub(crate) mod` is sufficient.

- [ ] **Step 6: net/mod.rs — keep cross-crate items, downgrade internals**

The supervisor still uses `generate_ca`, `start_mitmdump*`, `agent_env_vars`, `AddonConfig`, `MitmdumpHandle`, `CaPaths`. Keep those `pub`. But `FlowWatcher`, `KeylogWatcher`, flow parser functions, `NetworkDedup`, `KeylogLine`, `parse_keylog_line`, `parse_flow_line*`, `process_flow`, `ProcessedFlow`, `MitmdumpFlow` are now only used within argus (by `runtime.rs`). Downgrade:

```rust
mod ca;
mod dedup;
mod env;
mod flow_parser;
mod flow_watcher;
mod keylog;
mod mitmdump;

// Cross-crate: supervisor startup
pub use ca::{CaPaths, generate_ca};
pub use env::agent_env_vars;
pub use mitmdump::{AddonConfig, MitmdumpHandle, start_mitmdump, start_mitmdump_with_flow_capture};

// Crate-internal: runtime TLS watcher
pub(crate) use dedup::NetworkDedup;
pub(crate) use flow_parser::{MitmdumpFlow, ProcessedFlow, parse_flow_line, parse_flow_lines, process_flow};
pub(crate) use flow_watcher::{FlowEvents, FlowWatcher};
pub(crate) use keylog::{KeylogLine, KeylogWatcher, parse_keylog_line};
```

- [ ] **Step 7: tracer/mod.rs — keep seccomp pub, downgrade rest**

`install_seccomp_filter` is called from `supervisor/src/startup.rs` (child_setup). Keep `seccomp` as `pub mod`. Everything else is internal:

```rust
pub(crate) mod memory;
pub(crate) mod pending;
pub(crate) mod regs;
pub mod seccomp;
pub(crate) mod syscall_nr;
```

- [ ] **Step 8: api/mod.rs — keep serve/state/types pub, downgrade routes/errors**

```rust
pub(crate) mod errors;
pub(crate) mod routes;
pub mod state;
pub mod types;
```

Keep `serve` and `build_router` as `pub fn` (supervisor calls `api::serve`).

- [ ] **Step 9: cas/mod.rs — keep Cas trait and ContentHash pub**

`ContentHash` stays fully `pub`. `Cas`, `CasBackend`, `LocalCas`, `MemoryCas`, `RemoteCas`, `TieredCas` all stay `pub` (CLI reads CAS, supervisor wires CAS via runtime, custom backends need the traits). The `cas` module visibility is correct as-is. No changes needed here.

- [ ] **Step 10: Update lib.rs module visibility**

Change the module declarations in `lib.rs` to:

```rust
pub mod approver;
pub mod config;
pub mod events;
pub(crate) mod state;
pub mod cas;
pub(crate) mod storage;
pub mod tracer;
pub mod snapshot;
pub(crate) mod index;
pub mod net;
pub mod api;
pub mod pipeline;
pub mod runtime;
```

Rationale for items that stay `pub mod`:
- `tracer` — supervisor's `startup.rs` calls `argus::tracer::seccomp::install_seccomp_filter()`. Submodules inside `tracer/mod.rs` are already `pub(crate)` except `seccomp`.
- `snapshot` — tree types (`MerkleTree`, `Commit`, `TreeObject`) are part of the public API contract.

- [ ] **Step 11: Build the full workspace**

Run: `docker exec argus-arm64 cargo check --target aarch64-unknown-linux-musl --workspace`
Expected: PASS. Fix any remaining errors iteratively. Common issues:
- Test files in separate modules (e.g., `event_log_tests.rs`) may need `pub(super)` access
- `#[cfg(test)]` modules within files access private items via `super::*` — this works fine

- [ ] **Step 12: Run all tests**

Run: `docker exec argus-arm64 cargo test --target aarch64-unknown-linux-musl -p argus -p supervisor`
Expected: PASS

- [ ] **Step 13: Commit**

```bash
git add crates/argus/src/
git commit -m "tighten visibility: downgrade internal modules to pub(crate)"
```

---

## Chunk 3: Validation and cleanup

### Task 6: Run full validation suite

- [ ] **Step 1: Run validation tests**

Run: `docker exec argus-arm64 ./tests/validate.sh`
Expected: All 13 tests pass

- [ ] **Step 2: Verify the public API surface**

Run a quick check — count remaining `pub` items accessible from outside the crate:
```bash
docker exec argus-arm64 grep -rn '^pub ' crates/argus/src/ | grep -v 'pub(crate)\|pub(super)\|pub(self)\|#\[cfg(test)\]' | wc -l
```
Expected: Roughly 80-100 items (down from ~400+)

- [ ] **Step 3: Commit any final fixes**

```bash
git add -A
git commit -m "fix visibility issues found during validation"
```

### Task 7: Update task doc

**Files:**
- Create or update: `docs/tasks/p1-runtime-facade.md`

- [ ] **Step 1: Write task doc**

```markdown
# SupervisorRuntime Facade

**Status:** done
**Spec reference:** docs/superpowers/specs/2026-03-12-supervisor-runtime-design.md

## What was done
- Created `argus::runtime::SupervisorRuntime` facade
- Moved bus construction, stage wiring, initial state emission, TLS watcher into argus
- Made PipelineRunner fields private
- Rewrote supervisor wiring.rs (~435 lines → ~100 lines)
- Deleted supervisor tls_watcher.rs (moved into runtime)
- Downgraded ~100 internal types from pub to pub(crate)

## What works
- Full pipeline startup through SupervisorRuntime facade
- All 13 validation tests pass
- Public API surface reduced to ~30 cross-crate types

## What's missing
- None

## How to test
docker exec argus-arm64 cargo test --target aarch64-unknown-linux-musl -p argus -p supervisor
docker exec argus-arm64 ./tests/validate.sh
```

- [ ] **Step 2: Commit**

```bash
git add docs/
git commit -m "add runtime facade task doc"
```
