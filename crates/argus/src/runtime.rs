// Rust guideline compliant 2026-02-21
//! High-level facade for supervisor startup wiring.
//!
//! Constructs storage, sinks, bus, stages, and pipeline threads internally
//! so the supervisor binary only deals with config, process lifecycle,
//! and shutdown coordination.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result};
use compact_str::CompactString;
use dashmap::DashMap;
use nix::unistd::Pid;
use tokio::sync::broadcast;
use tracing::{Level, event};

use crate::api::state::{SharedState, new_shared_state};
use crate::approver::Approvers;
use crate::cas::{Cas, ContentHash, LocalCas};
use crate::config::{OutputConfig, SupervisorConfig};
use crate::events::{Event, EventPayload, SequenceGenerator};
use crate::events::snapshot::{InitialFile, InitialState};
use crate::index::{PathIndex, PidIndex, TypeIndex};
use crate::pipeline::bus::RecordBus;
use crate::pipeline::capture_policy::CapturePolicy;
use crate::pipeline::context::PipelineContext;
use crate::pipeline::durability::DurabilityLayer;
use crate::pipeline::outputs::{FileOutput, OutputList, StdoutOutput};
use crate::pipeline::record::Record;
use crate::pipeline::runner::PipelineRunner;
use crate::pipeline::sink::Sink;
use crate::pipeline::sinks::{
    BroadcastSink, EventLogSink, IndexSink, LocalCasSink, RemoteCasSink,
};
use crate::pipeline::stages::{
    ApprovalStage, CaptureStage, CheckRulesStage, ClassifyStage, StampStage, TreeStage,
};
use crate::pipeline::stages::redact::RedactStage;
use crate::pipeline::ptrace_thread::PtraceStream;
use crate::pipeline::replay::RawStopRecorder;
use crate::snapshot::MerkleTree;
use crate::state::{FdTable, PipeRegistry, PtyRegistry};
use crate::storage::{DigestCache, DynObjectStore, EventLog, S3Client, UploadPool};

// Chosen to be fast enough for SSLKEYLOGFILE changes to appear promptly
// while not burning CPU when TLS traffic is idle.
const TLS_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Facade that owns the pipeline context and shared state.
///
/// Constructs all internal pipeline components so the supervisor binary
/// never touches sinks, stages, or internal state types directly.
pub struct SupervisorRuntime {
    config: SupervisorConfig,
    ctx: PipelineContext,
    shared: SharedState,
    durability: DurabilityLayer,
    upload_pool: Option<Arc<UploadPool>>,
    outputs: OutputList,
    redact: RedactStage,
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

        // Single LocalCas instance shared by DurabilityLayer and LocalCasSink.
        // LocalCas is a cheap path wrapper with no open file handles.
        let shared_cas = LocalCas::new(cas_path.clone())
            .context("failed to initialize shared CAS")?;

        let event_log = EventLog::new(
            config.agent_id.clone(),
            data_dir.join("events"),
            config.durability.default,
        )
        .context("failed to initialize event log")?;
        let upload_pool = build_upload_pool(&config).await?;

        // Single DigestCache shared by DurabilityLayer and RemoteCasSink so
        // uploads confirmed by one path are visible to the other immediately.
        let shared_digest_cache = if upload_pool.is_some() {
            let cache_path = data_dir.join("digest-cache.bin");
            Some(Arc::new(DigestCache::new(cache_path)))
        } else {
            None
        };

        // DurabilityLayer for CaptureStage (and later TreeStage gets its own
        // lightweight handle to the same CAS directory).
        let durability_cas = LocalCas::new(cas_path.clone())
            .context("failed to initialize durability CAS")?;
        let durability = DurabilityLayer::new(
            durability_cas,
            upload_pool.clone(),
            shared_digest_cache.clone(),
        );

        let (broadcast_tx, _) = broadcast::channel::<Event>(4096);
        let bus = build_bus(
            shared_cas,
            event_log,
            upload_pool.clone(),
            &config,
            broadcast_tx,
            shared_digest_cache,
        );

        let seq = Arc::new(SequenceGenerator::default());
        let agent_id = CompactString::from(config.agent_id.as_str());
        let ctx = PipelineContext::new(seq, bus.clone(), agent_id.clone());

        let api_cas: Arc<dyn Cas> = Arc::new(
            LocalCas::new(cas_path).context("failed to initialize API CAS handle")?,
        );
        let shared = new_shared_state(agent_id, api_cas, bus);
        shared.store_rules(config.build_ruleset());

        let outputs = build_outputs(&config);
        let redact = RedactStage::new(&config.redact);

        Ok(Self { config, ctx, shared, durability, upload_pool, outputs, redact })
    }

    /// Shared state handle for the API server.
    pub fn shared_state(&self) -> SharedState {
        self.shared.clone()
    }

    /// Emit the `AgentStart` control event through outputs and the bus.
    pub fn emit_agent_start(&mut self) {
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

        let mut evt = Event::new(&self.ctx.seq, self.ctx.agent_id.clone(), payload);
        self.redact.redact(&mut evt);
        self.outputs.emit(&evt);
        self.ctx.bus.emit(Record::Event(evt));
    }

    /// Walk the workspace and emit `InitialFile` + `InitialState` events.
    pub fn emit_initial_state(&mut self) {
        use std::os::unix::fs::MetadataExt;

        let workspace = &self.config.workspace_dir;
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
            let mut evt = Event::new(&self.ctx.seq, self.ctx.agent_id.clone(), payload);
            self.redact.redact(&mut evt);
            self.outputs.emit(&evt);
            self.ctx.bus.emit(Record::Event(evt));

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
        let mut evt = Event::new(&self.ctx.seq, self.ctx.agent_id.clone(), payload);
        self.redact.redact(&mut evt);
        self.outputs.emit(&evt);
        self.ctx.bus.emit(Record::Event(evt));
    }

    /// Spawn the keylog pipeline thread.
    ///
    /// Returns `(join_handle, stop_flag)`. Set `stop_flag` to `true` and
    /// join the handle during shutdown to drain final TLS key data.
    pub fn spawn_keylog_pipeline(&self) -> (JoinHandle<()>, Arc<AtomicBool>) {
        let keylog_path = self.config.tls.keylog_path.clone();
        let ctx = self.ctx.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();

        let handle = thread::Builder::new()
            .name("keylog-pipeline".into())
            .spawn(move || {
                crate::pipeline::keylog_pipeline::run(keylog_path, ctx, stop_clone, TLS_POLL_INTERVAL);
            })
            .expect("failed to spawn keylog pipeline thread");

        (handle, stop)
    }

    /// Spawn the proxy pipeline thread if a flow path is configured.
    ///
    /// Returns `None` when no flow path is provided (mitmdump not running).
    /// Otherwise returns `(join_handle, stop_flag)`.
    pub fn spawn_proxy_pipeline(
        &self,
        flow_path: Option<PathBuf>,
    ) -> Option<(JoinHandle<()>, Arc<AtomicBool>)> {
        let path = flow_path?;
        let ctx = self.ctx.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();

        let handle = thread::Builder::new()
            .name("proxy-pipeline".into())
            .spawn(move || {
                crate::pipeline::proxy_pipeline::run(Some(path), ctx, stop_clone, TLS_POLL_INTERVAL);
            })
            .expect("failed to spawn proxy pipeline thread");

        Some((handle, stop))
    }

    /// Construct all pipeline stages and return the runner.
    ///
    /// Consumes `self` — the context and config move into the runner.
    /// The returned `oneshot::Receiver<Result<()>>` fires once `PTRACE_SEIZE`
    /// completes. Await it before releasing the child's sync pipe so the
    /// child cannot execute ahead of the seize.
    pub fn into_pipeline(
        self,
        child_pid: Pid,
    ) -> (PipelineRunner, tokio::sync::oneshot::Receiver<Result<()>>, JoinHandle<()>) {
        let (ptrace_stream, seize_rx, ptrace_thread) = PtraceStream::spawn(child_pid);
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
            self.durability,
            policy,
            file_state,
            self.config.enrich.max_inline_bytes,
        );

        // Build a second DurabilityLayer for TreeStage. Both share the same
        // LocalCas root and upload pool, but each holds its own LocalCas handle
        // (which is cheap — it is just a path wrapper with no open file handles).
        let tree_cas = LocalCas::new(self.config.data_dir.join("cas"))
            .expect("failed to initialize tree-stage CAS");
        let tree_durability = DurabilityLayer::new(tree_cas, self.upload_pool, None);
        let tree_stage = TreeStage::new(MerkleTree::new(), tree_durability, 1000);
        let stamp_stage = StampStage::new(self.ctx.seq.clone(), self.ctx.agent_id.clone(), self.config.enrich.clone());

        let recorder: Option<RawStopRecorder> = None;

        let runner = PipelineRunner::new(
            ptrace_stream,
            classify,
            rules_stage,
            approvals,
            capture_stage,
            tree_stage,
            stamp_stage,
            self.ctx.bus,
            self.outputs,
            self.redact,
            recorder,
            self.shared.pause_flag(),
            self.shared,
        );

        (runner, seize_rx, ptrace_thread)
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

/// Constructs the `RecordBus` from internal sinks only.
///
/// Enriched user-facing output is handled by `OutputList` (with redaction
/// applied) before the bus receives each event.
fn build_bus(
    local_cas: LocalCas,
    event_log: EventLog,
    upload_pool: Option<Arc<UploadPool>>,
    config: &SupervisorConfig,
    broadcast_tx: broadcast::Sender<Event>,
    shared_digest_cache: Option<Arc<DigestCache>>,
) -> RecordBus {
    let mut sinks: Vec<Arc<dyn Sink>> = vec![
        Arc::new(LocalCasSink::new(local_cas)),
        Arc::new(EventLogSink::new(event_log)),
        Arc::new(IndexSink::new(PathIndex::new(), PidIndex::new(), TypeIndex::new())),
        Arc::new(BroadcastSink::new(broadcast_tx)),
    ];

    if let Some(pool) = upload_pool {
        if let Some(digest_cache) = shared_digest_cache {
            sinks.push(Arc::new(RemoteCasSink::new(
                pool,
                digest_cache,
                config.agent_id.clone(),
            )));
        }
    }

    RecordBus::new(sinks)
}

/// Constructs an `OutputList` from the configured output destinations.
///
/// `UnixSocket` and `Http` outputs are not yet implemented; they are skipped
/// with a warning so the supervisor still starts with a partial config.
fn build_outputs(config: &SupervisorConfig) -> OutputList {
    let mut list = OutputList::new();
    for output_config in &config.outputs {
        match output_config {
            OutputConfig::Stdout { flush_every_event } => {
                list.push(Box::new(StdoutOutput::with_flush_policy(*flush_every_event)));
            }
            OutputConfig::File { path, max_size, max_files } => {
                match FileOutput::new(path.clone(), *max_size, *max_files) {
                    Ok(out) => list.push(Box::new(out)),
                    Err(e) => {
                        event!(
                            name: "runtime.output.file.error",
                            Level::WARN,
                            output.path = %path.display(),
                            error.message = %e,
                            "failed to open file output {{output.path}}: {{error.message}}; skipping",
                        );
                    }
                }
            }
            OutputConfig::UnixSocket { .. } | OutputConfig::Http { .. } => {
                event!(
                    name: "runtime.output.unimplemented",
                    Level::WARN,
                    "output type not yet implemented, skipping",
                );
            }
        }
    }
    list
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
