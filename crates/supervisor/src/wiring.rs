// Rust guideline compliant 2026-02-21
//! Async startup wiring: storage, bus, pipeline stage construction.
//!
//! Called from `main` after CLI parsing and TLS setup. Extracted here
//! so `main.rs` stays under the 300-line guideline limit.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use anyhow::{Context, Result};
use tokio::sync::broadcast;
use tracing::{Level, event};

use argus::api;
use argus::api::state::{SharedState, new_shared_state};
use argus::cas::LocalCas;
use argus::config::SupervisorConfig;
use argus::events::{Event, EventPayload, SequenceGenerator};
use argus::net;
use argus::pipeline::{PipelineRunner, PtraceStream, RawStopRecorder, RecordBus, Sink};
use argus::pipeline::sinks::{
    BroadcastSink, EventLogSink, IndexSink, LocalCasSink, RemoteCasSink,
};
use argus::pipeline::stages::{
    ApprovalStage, CaptureStage, CheckRulesStage, ClassifyStage, StampStage, TreeStage,
};
use argus::storage::{DynObjectStore, EventLog, S3Client, UploadPool};

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
    let (local_cas, bus, seq_gen, tls_handle, tls_stop) =
        init_storage_and_bus(&config, flow_path).await?;

    let (shared, api_shutdown_tx) = init_api_server(&config).await?;

    let spawn = startup::spawn_agent(
        &config.agent_command,
        &agent_env,
        &config.workspace_dir,
        config.run_as.as_ref(),
    )?;
    let _stdout_drain = crate::spawn_drain_thread("stdout", spawn.stdout_r);
    let _stderr_drain = crate::spawn_drain_thread("stderr", spawn.stderr_r);

    crate::signals::install_handler();

    let (ptrace_stream, ptrace_thread) =
        PtraceStream::spawn(spawn.child_pid, spawn.sync_pipe_w);

    let runner = build_runner(
        ptrace_stream,
        local_cas,
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
    Arc<LocalCas>,
    RecordBus,
    SequenceGenerator,
    std::thread::JoinHandle<()>,
    Arc<std::sync::atomic::AtomicBool>,
)> {
    let data_dir = &config.data_dir;
    let cas_path = data_dir.join("cas");

    let local_cas = Arc::new(
        LocalCas::new(cas_path.clone()).context("failed to initialize CAS store")?,
    );
    let event_log = EventLog::new(
        config.agent_id.clone(),
        data_dir.join("events"),
        config.durability.default,
    )
    .context("failed to initialize event log")?;
    let upload_pool = build_upload_pool(config).await?;

    let (broadcast_tx, _) = broadcast::channel::<Event>(4096);
    let bus = build_bus(
        local_cas.clone(),
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
        LocalCas::new(cas_path).context("failed to initialize TLS watcher CAS handle")?,
        bus.legacy_sender(),
        tls_seq,
        config.agent_id.clone(),
        tls_stop.clone(),
    );

    Ok((local_cas, bus, seq_gen, tls_handle, tls_stop))
}

/// Initializes shared API state and spawns the API server task.
///
/// Returns `(shared_state, api_shutdown_tx)`. The caller must send
/// `true` on `api_shutdown_tx` during shutdown to stop the server.
async fn init_api_server(
    config: &SupervisorConfig,
) -> Result<(SharedState, tokio::sync::watch::Sender<bool>)> {
    let cas_path = config.data_dir.join("cas");
    let api_cas: Arc<dyn argus::cas::Cas> = Arc::new(
        LocalCas::new(cas_path).context("failed to initialize API CAS handle")?,
    );
    let shared = new_shared_state(config.agent_id.clone(), api_cas);
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
    local_cas: Arc<LocalCas>,
    bus: RecordBus,
    shared: SharedState,
    seq_gen: SequenceGenerator,
    config: &SupervisorConfig,
) -> PipelineRunner {
    let proxy_mode = config.tls.proxy_mode;
    let classify = ClassifyStage::new(shared.clone(), proxy_mode, config.tls.mitm_proxy_port);
    let rules_stage = CheckRulesStage::new(shared.rules_handle());
    let approvals = ApprovalStage::new(shared.clone());
    let capture_stage = CaptureStage::new(local_cas, bus.clone());
    let tree_stage = TreeStage::new(shared.clone());
    let stamp_stage = StampStage::new(seq_gen, config.agent_id.clone());
    // Raw stop recording is opt-in; no config field exposes it yet.
    // TODO: wire to a config field when RawStopRecorder is fully implemented.
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

    // Channel capacity: enough to buffer bursts without back-pressure on the
    // pipeline. 4096 matches the broadcast channel capacity.
    const UPLOAD_CHANNEL_CAPACITY: usize = 4096;
    let pool = UploadPool::new(dyn_store, &config.storage.upload, UPLOAD_CHANNEL_CAPACITY);

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
    local_cas: Arc<LocalCas>,
    event_log: EventLog,
    upload_pool: Option<Arc<UploadPool>>,
    config: &SupervisorConfig,
    broadcast_tx: broadcast::Sender<Event>,
) -> RecordBus {
    let mut sinks: Vec<Arc<dyn Sink>> = vec![
        Arc::new(LocalCasSink::new(local_cas)),
        Arc::new(EventLogSink::new(event_log)),
        Arc::new(IndexSink::new(config.data_dir.join("indexes"))),
        Arc::new(BroadcastSink::new(broadcast_tx)),
    ];

    if let Some(pool) = upload_pool {
        sinks.push(Arc::new(RemoteCasSink::new(pool, config.agent_id.clone())));
    }

    RecordBus::new(sinks)
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
