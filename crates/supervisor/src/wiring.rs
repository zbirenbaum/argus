// Rust guideline compliant 2026-02-21
//! Async startup wiring: storage, bus, pipeline stage construction.
//!
//! Called from `main` after CLI parsing and TLS setup. Extracted here
//! so `main.rs` stays under the 300-line guideline limit.

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use dashmap::DashMap;
use nix::unistd::Pid;
use tokio::sync::broadcast;
use tracing::{Level, event};

use argus::api;
use argus::api::state::{SharedState, new_shared_state};
use argus::approver::Approvers;
use argus::cas::LocalCas;
use argus::config::SupervisorConfig;
use argus::events::{Event, EventPayload, SequenceGenerator};
use argus::net;
use argus::pipeline::{PtraceStream, RawStopRecorder, RecordBus, Sink};
use argus::pipeline::runner::PipelineRunner;
use argus::pipeline::sinks::{
    BroadcastSink, EventLogSink, IndexSink, LocalCasSink, RemoteCasSink, StdoutSink,
};
use argus::pipeline::stages::{
    ApprovalStage, CaptureStage, CheckRulesStage, ClassifyStage, StampStage, TreeStage,
};
use argus::index::{PathIndex, PidIndex, TypeIndex};
use argus::pipeline::capture_policy::CapturePolicy;
use argus::snapshot::MerkleTree;
use argus::state::{FdTable, PipeRegistry, PtyRegistry};
use argus::storage::{DigestCache, DynObjectStore, EventLog, S3Client, UploadPool};

use crate::startup;
use crate::tls_watcher;

/// Top-level async entry point: initializes storage, pipeline, and API.
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
    let (bus, seq_gen, tls_handle, tls_stop) =
        init_storage_and_bus(&config, flow_path).await?;

    let (shared, api_shutdown_tx) = init_api_server(&config, bus.clone()).await?;

    let spawn = startup::spawn_agent(
        &config.agent_command,
        &agent_env,
        &config.workspace_dir,
        config.run_as.as_ref(),
    )?;
    let _stdout_drain = crate::spawn_drain_thread("stdout", spawn.stdout_r);
    let _stderr_drain = crate::spawn_drain_thread("stderr", spawn.stderr_r);

    crate::signals::install_handler();

    // Close the write end of the sync pipe — the ptrace thread signals the
    // tracee after attaching; we don't write to it from the async context.
    let _ = nix::unistd::close(spawn.sync_pipe_w);

    let (ptrace_stream, ptrace_thread) = PtraceStream::spawn(spawn.child_pid);

    emit_initial_state(&bus, &seq_gen, &config);

    let runner = build_runner(
        ptrace_stream,
        bus.clone(),
        shared,
        seq_gen,
        &config,
    );

    event!(Level::DEBUG, "wiring: entering pipeline.run()");
    runner.run().await;
    event!(Level::DEBUG, "wiring: pipeline.run() returned, beginning shutdown");

    shutdown(tls_stop, tls_handle, mitmdump.as_mut(), &bus, ptrace_thread, api_shutdown_tx)?;

    Ok(())
}

/// Initializes the CAS, event log, upload pool, and bus.
///
/// Also spawns the TLS watcher thread.
async fn init_storage_and_bus(
    config: &SupervisorConfig,
    flow_path: Option<std::path::PathBuf>,
) -> Result<(
    RecordBus,
    SequenceGenerator,
    std::thread::JoinHandle<()>,
    Arc<std::sync::atomic::AtomicBool>,
)> {
    let data_dir = &config.data_dir;
    let cas_path = data_dir.join("cas");

    // The sink owns its own LocalCas — cheap to construct (just holds a path).
    let sink_cas = LocalCas::new(cas_path.clone()).context("failed to initialize sink CAS")?;
    let event_log = EventLog::new(
        config.agent_id.clone(),
        data_dir.join("events"),
        config.durability.default,
    )
    .context("failed to initialize event log")?;
    let upload_pool = build_upload_pool(config).await?;

    let (broadcast_tx, _) = broadcast::channel::<Event>(4096);
    let bus = build_bus(
        sink_cas,
        event_log,
        upload_pool,
        config,
        broadcast_tx,
    );

    let seq_gen = SequenceGenerator::default();
    // TLS sequences start at 1_000_000 to avoid collision with the tracer
    // generator without coordination between threads.
    let tls_seq = SequenceGenerator::new(1_000_000);
    emit_agent_start(&bus, &seq_gen, config);

    let tls_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let tls_handle = tls_watcher::spawn(
        config.tls.keylog_path.clone(),
        flow_path,
        bus.clone(),
        tls_seq,
        config.agent_id.clone(),
        tls_stop.clone(),
    );

    Ok((bus, seq_gen, tls_handle, tls_stop))
}

/// Initializes shared API state and spawns the API server task.
///
/// Returns `(shared_state, api_shutdown_tx)`. The caller must send
/// `true` on `api_shutdown_tx` during shutdown to stop the server.
async fn init_api_server(
    config: &SupervisorConfig,
    bus: RecordBus,
) -> Result<(SharedState, tokio::sync::watch::Sender<bool>)> {
    let cas_path = config.data_dir.join("cas");
    let api_cas: Arc<dyn argus::cas::Cas> = Arc::new(
        LocalCas::new(cas_path).context("failed to initialize API CAS handle")?,
    );
    let shared = new_shared_state(config.agent_id.clone(), api_cas, bus);
    shared.store_rules(config.build_ruleset());

    let listen_addr = config.listen_addr;
    let api_shared = shared.clone();
    let (api_shutdown_tx, api_shutdown_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        if let Err(e) = api::serve(api_shared, listen_addr, api_shutdown_rx).await {
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

    Ok((shared, api_shutdown_tx))
}

/// Constructs all pipeline stages and returns the assembled [`PipelineRunner`].
fn build_runner(
    ptrace_stream: PtraceStream,
    bus: RecordBus,
    shared: SharedState,
    seq_gen: SequenceGenerator,
    config: &SupervisorConfig,
) -> PipelineRunner {
    let handle = ptrace_stream.handle();

    // fd/pipe/pty state shared between ClassifyStage and the ptrace loop.
    let fd_tables: Arc<DashMap<Pid, FdTable>> = Arc::new(DashMap::new());
    let pipe_registry = Arc::new(Mutex::new(PipeRegistry::new()));
    let pty_registry = Arc::new(Mutex::new(PtyRegistry::new()));

    let transparent_mode = matches!(
        config.tls.proxy_mode,
        argus::config::ProxyMode::Transparent
    );
    let proxy_addr = std::net::SocketAddr::new(
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        config.tls.mitm_proxy_port,
    );

    // Shared file content hashes: ClassifyStage sets empty hash on O_TRUNC,
    // CaptureStage reads before_hash and updates after_hash. Avoids racy
    // filesystem reads between concurrent writes.
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
    let rules_stage = CheckRulesStage::new(shared.rules_handle());
    let approvals = ApprovalStage::new(Approvers::new());

    let policy = CapturePolicy::default_full();
    let capture_stage = CaptureStage::new(handle.clone(), bus.clone(), policy, file_state);

    let tree_stage = TreeStage::new(MerkleTree::new(), bus.clone(), 1000);
    let stamp_stage = StampStage::new(seq_gen, config.agent_id.clone());
    // Raw stop recording is opt-in; no config field exposes it yet.
    let recorder: Option<RawStopRecorder> = None;

    PipelineRunner {
        ptrace: ptrace_stream,
        classify,
        rules: rules_stage,
        approvals,
        capture: capture_stage,
        tree: tree_stage,
        stamp: stamp_stage,
        bus,
        recorder,
        paused: shared.pause_flag(),
        shared,
    }
}

/// Shuts down all subsystems in dependency order.
fn shutdown(
    tls_stop: Arc<std::sync::atomic::AtomicBool>,
    tls_handle: std::thread::JoinHandle<()>,
    mitmdump: Option<&mut net::MitmdumpHandle>,
    bus: &RecordBus,
    ptrace_thread: std::thread::JoinHandle<()>,
    api_shutdown_tx: tokio::sync::watch::Sender<bool>,
) -> Result<()> {
    use std::sync::atomic::Ordering;

    // Stop TLS watcher before mitmdump exits so it can drain final data.
    tls_stop.store(true, Ordering::Release);
    let _ = tls_handle.join();
    event!(Level::DEBUG, "shutdown: tls-watcher stopped");

    if let Some(m) = mitmdump {
        event!(Level::DEBUG, "shutdown: stopping mitmdump");
        let _ = m.stop();
        event!(Level::DEBUG, "shutdown: mitmdump stopped");
    }

    bus.shutdown_all();
    let _ = api_shutdown_tx.send(true);
    ptrace_thread.join().ok();

    event!(Level::DEBUG, "shutdown: all subsystems stopped");
    Ok(())
}

/// Constructs the upload pool if S3 is configured.
async fn build_upload_pool(config: &SupervisorConfig) -> Result<Option<Arc<UploadPool>>> {
    let Some(ref s3_config) = config.storage.s3 else {
        event!(
            name: "supervisor.storage.local_only",
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
        name: "supervisor.storage.s3",
        Level::INFO,
        s3.bucket = %s3_config.bucket,
        s3.endpoint = s3_config.endpoint.as_deref().unwrap_or("default"),
        "storage pipeline initialized with S3 backend",
    );

    Ok(Some(Arc::new(pool)))
}

/// Constructs the [`RecordBus`] from all configured sinks.
pub fn build_bus(
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

/// Walks the workspace directory and emits `InitialFile` + `InitialState` events.
fn emit_initial_state(
    bus: &RecordBus,
    seq_gen: &SequenceGenerator,
    config: &SupervisorConfig,
) {
    use argus::cas::ContentHash;
    use argus::events::snapshot::{InitialFile, InitialState};
    use std::os::unix::fs::MetadataExt;

    let workspace = &config.workspace_dir;
    let mut file_count: u64 = 0;
    let mut total_size: u64 = 0;
    let mut tree = MerkleTree::new();

    walk_dir_recursive(workspace, &mut |path: &std::path::Path| {
        let meta = match path.metadata() {
            Ok(m) if m.is_file() => m,
            _ => return,
        };

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
        let evt = Event::new(seq_gen, config.agent_id.clone(), payload);
        bus.emit(argus::pipeline::Record::Event(evt));

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
    let evt = Event::new(seq_gen, config.agent_id.clone(), payload);
    bus.emit(argus::pipeline::Record::Event(evt));
}

/// Recursively visit all files under `dir`.
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

/// Emits the `AgentStart` control event through the bus.
pub fn emit_agent_start(
    bus: &RecordBus,
    seq_gen: &SequenceGenerator,
    config: &SupervisorConfig,
) {
    let nspid = argus::config::read_nspid_pair();

    let payload = EventPayload::AgentStart(argus::events::control::AgentStart {
        agent_id: config.agent_id.clone(),
        supervisor_pid_host: nspid.map(|(h, _)| h),
        supervisor_pid_ns: nspid.map(|(_, n)| n),
        config_summary: format!(
            "data_dir={}, workspace={}",
            config.data_dir.display(),
            config.workspace_dir.display(),
        ),
        node: std::env::var("NODE_NAME").ok(),
        pod: std::env::var("POD_NAME").ok(),
        container: std::env::var("CONTAINER_NAME").ok(),
    });

    let evt = Event::new(seq_gen, config.agent_id.clone(), payload);
    bus.emit(argus::pipeline::Record::Event(evt));
}
