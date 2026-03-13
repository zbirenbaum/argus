// Rust guideline compliant 2026-02-21
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

// Sequences for TLS events start far above the tracer generator to avoid
// collision between the two threads without any cross-thread coordination.
const TLS_SEQ_START: u64 = 1_000_000;

// Poll interval chosen to be fast enough for SSLKEYLOGFILE changes to appear
// promptly while not burning CPU when TLS is idle.
const TLS_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Facade that owns the bus, sequence generator, and shared state.
///
/// Constructs all internal pipeline components so the supervisor binary
/// never touches sinks, stages, or internal state types directly.
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
    /// Returns `(join_handle, stop_flag)`. Set `stop_flag` to `true` and
    /// join the handle during shutdown to drain final TLS data.
    pub fn spawn_tls_watcher(
        &self,
        flow_path: Option<PathBuf>,
    ) -> (JoinHandle<()>, Arc<AtomicBool>) {
        let keylog_path = self.config.tls.keylog_path.clone();
        let bus = self.bus.clone();
        let tls_seq = SequenceGenerator::new(TLS_SEQ_START);
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
    /// Consumes `self` — the bus, seq_gen, and config move into the runner.
    /// Call `emit_agent_start` and `emit_initial_state` before this.
    pub fn into_pipeline(self, child_pid: Pid) -> (PipelineRunner, JoinHandle<()>) {
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

    // Final drain ensures no TLS data is lost between last poll and shutdown.
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
