# Data Pipeline Migration Spec

Full refactor. The supervisor becomes a pipeline of stream transformers with fan-out to composable sinks. Replaces the broken data flow in one shot.

## Architecture

```
Dedicated ptrace thread              Async pipeline (tokio)
┌─────────────────────┐             ┌──────────────────────────────────────────┐
│                     │  channel    │                                          │
│  waitpid loop       ├────────────►  PtraceStream (Stream<Item=RawStop>)     │
│                     │             │       │                                  │
│  executes           │  directives │       ▼                                  │
│  PipelineDirectives ◄────────────┤  .classify()     → ClassifiedEvent       │
│  (read memory,      │             │       │                                  │
│   inject errno,     │             │       ▼                                  │
│   resume)           │             │  .check_rules()  → filter/block/pause   │
│                     │             │       │                                  │
└─────────────────────┘             │       ▼                                  │
                                    │  .await_approvals() → approved/denied   │
                                    │       │                                  │
                                    │       ▼                                  │
                                    │  .capture_content() → attach hashes     │
                                    │       │        (sends ReadMemory         │
                                    │       │         directive to ptrace      │
                                    │       │         thread, awaits reply)    │
                                    │       ▼                                  │
                                    │  .update_tree()  → attach tree_hash     │
                                    │       │                                  │
                                    │       ▼                                  │
                                    │  .stamp()        → seq, timestamps      │
                                    │       │                                  │
                                    │       ▼                                  │
                                    │  RecordBus (fan-out)                     │
                                    │    ├─ LocalCasSink     [blocking]        │
                                    │    ├─ EventLogSink     [blocking]        │
                                    │    ├─ IndexSink        [blocking]        │
                                    │    ├─ RemoteCasSink    [async]           │
                                    │    ├─ BroadcastSink    [async]           │
                                    │    └─ (NatsSink, KafkaSink, ...)         │
                                    └──────────────────────────────────────────┘
```

Two threads. The ptrace thread is a dumb executor: waitpid, send stops, execute directives. All logic lives in the async pipeline stages. Each stage is independently testable. The full pipeline is replayable from recorded RawStops.

---

## Types

### RawSyscallStop

What the ptrace thread yields. Register data and /proc reads only. No classification, no content, no hashing.

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]  // Serialize for replay
pub struct RawSyscallStop {
    pub pid: Pid,
    pub stop_type: StopType,
}

pub enum StopType {
    SyscallEntry { syscall_nr: u64, args: SyscallArgs },
    SyscallExit { syscall_nr: u64, return_value: i64 },
    Fork { parent: Pid, child: Pid },
    Exec { pid: Pid },
    Exit { pid: Pid, exit_code: i32 },
    Signal { pid: Pid, signal: i32 },
}

pub struct SyscallArgs { pub arg0: u64, pub arg1: u64, pub arg2: u64, pub arg3: u64, pub arg4: u64, pub arg5: u64 }
```

### PipelineDirective

Commands the pipeline sends back to the ptrace thread.

```rust
pub enum PipelineDirective {
    Resume { pid: Pid },
    ReadMemory { pid: Pid, addr: usize, len: usize, reply: oneshot::Sender<Result<Vec<u8>>> },
    ReadString { pid: Pid, addr: usize, max_len: usize, reply: oneshot::Sender<Result<String>> },
    ReadFile { path: PathBuf, reply: oneshot::Sender<Result<Vec<u8>>> },
    InjectError { pid: Pid, errno: i32 },
    ResolveFd { pid: Pid, fd: i32, reply: oneshot::Sender<Result<PathBuf>> },
}
```

### ClassifiedEvent

After path/fd resolution. We know what operation this is.

```rust
pub struct ClassifiedEvent {
    pub pid: Pid,
    pub raw: RawSyscallStop,
    pub classification: Classification,
}

pub enum Classification {
    FileWrite { path: PathBuf, fd: i32, buf_addr: usize, len: usize },
    FileRead { path: PathBuf, fd: i32, buf_addr: usize, len: usize },
    FileRename { old_path: PathBuf, new_path: PathBuf },
    FileUnlink { path: PathBuf },
    FileMkdir { path: PathBuf },
    FileRmdir { path: PathBuf },
    FileChmod { path: PathBuf, mode: u32 },
    FileTruncate { path: PathBuf, len: u64 },
    FileLink { target: PathBuf, link_path: PathBuf },
    FileSymlink { target: PathBuf, link_path: PathBuf },
    FileOpen { path: PathBuf, flags: i32, mode: u32 },
    FileClose { fd: i32 },
    Stdio { subtype: StdioType, pipe_inode: Option<u64>, buf_addr: usize, len: usize },
    PipeCreate { read_fd: i32, write_fd: i32, inode: u64 },
    PipeData { inode: u64, direction: PipeDirection, buf_addr: usize, len: usize },
    PtyCreate { master_fd: i32, slave_path: PathBuf },
    PtyData { subtype: PtyDataType, buf_addr: usize, len: usize },
    FdDup { old_fd: i32, new_fd: i32 },
    ProcessExec { binary: PathBuf, argv: Vec<String>, envp: Vec<String> },
    ProcessFork { parent: Pid, child: Pid },
    ProcessExit { exit_code: i32 },
    NetSocket { domain: i32, sock_type: i32, fd: i32 },
    NetConnect { fd: i32, addr: SocketAddr },
    NetAccept { fd: i32, peer: SocketAddr },
    Passthrough,
}
```

### CapturedEvent

After content capture. Hashes attached.

```rust
pub struct CapturedEvent {
    pub pid: Pid,
    pub classification: Classification,
    pub content: CapturedContent,
}

pub enum CapturedContent {
    None,
    FileWrite { before_hash: Option<ContentHash>, after_hash: Option<ContentHash>, size: usize },
    FileRead { content_hash: Option<ContentHash>, size: usize },
    StreamData { content_hash: Option<ContentHash>, size: usize },
    FileDelete { content_hash: Option<ContentHash> },
}
```

### Record

What sinks receive.

```rust
pub enum Record {
    Event(Event),
    Content { hash: ContentHash, data: Vec<u8> },
    Manifest { hash: ContentHash, chunks: Vec<ContentHash> },
    Checkpoint { seq: u64, data: Vec<u8> },
}
```

---

## Ptrace Thread

Dumb executor. Waitpid, send stops, execute directives. No classification, no content capture, no event logic.

```rust
fn ptrace_thread_main(
    initial_pid: Pid,
    stop_tx: mpsc::UnboundedSender<RawSyscallStop>,
    directive_rx: mpsc::UnboundedReceiver<PipelineDirective>,
) {
    ptrace::seize(initial_pid, PTRACE_OPTIONS).expect("seize failed");

    loop {
        match waitpid(None, Some(WaitPidFlag::__WALL)) {
            Ok(status) => {
                let stop = translate_wait_status(status);
                if stop_tx.send(stop).is_err() { break; }

                // Block until pipeline tells us what to do
                match directive_rx.blocking_recv() {
                    Some(PipelineDirective::Resume { pid }) => {
                        let _ = ptrace::syscall(pid, None);
                    }
                    Some(PipelineDirective::ReadMemory { pid, addr, len, reply }) => {
                        let _ = reply.send(process_vm_readv(pid, addr, len));
                        // Don't resume — pipeline sends Resume after processing
                    }
                    Some(PipelineDirective::ReadString { pid, addr, max_len, reply }) => {
                        let _ = reply.send(read_null_terminated(pid, addr, max_len));
                    }
                    Some(PipelineDirective::ReadFile { path, reply }) => {
                        let _ = reply.send(std::fs::read(&path).map_err(Into::into));
                    }
                    Some(PipelineDirective::InjectError { pid, errno }) => {
                        set_return_value(pid, -errno as i64);
                        let _ = ptrace::syscall(pid, None);
                    }
                    Some(PipelineDirective::ResolveFd { pid, fd, reply }) => {
                        let _ = reply.send(read_fd_link(pid, fd));
                    }
                    None => break,
                }
            }
            Err(Errno::ECHILD) => break,
            Err(e) => { tracing::error!(%e, "waitpid failed"); break; }
        }
    }
}
```

---

## Pipeline Stages

Each is a struct with one method that transforms its input. Testable in isolation with synthetic data.

### Stage 1: PtraceStream

Bridges sync ptrace thread into async. Stream<Item=RawSyscallStop>.

```rust
pub struct PtraceStream {
    stop_rx: mpsc::UnboundedReceiver<RawSyscallStop>,
    directive_tx: mpsc::UnboundedSender<PipelineDirective>,
}

impl PtraceStream {
    pub fn spawn(child_pid: Pid) -> (Self, JoinHandle<()>) { /* ... */ }
    pub fn directive(&self, d: PipelineDirective) { let _ = self.directive_tx.send(d); }
}

impl Stream for PtraceStream {
    type Item = RawSyscallStop;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        self.stop_rx.poll_recv(cx)
    }
}
```

### Stage 2: ClassifyStage

RawSyscallStop → ClassifiedEvent. Sends ReadString/ResolveFd directives to read paths from tracee memory. Manages fd tables, pipe registry, PTY registry.

```rust
pub struct ClassifyStage {
    ptrace: PtraceStream,
    fd_tables: Arc<DashMap<Pid, FdTable>>,
    pipe_registry: Arc<PipeRegistry>,
    pty_registry: Arc<PtyRegistry>,
}

impl ClassifyStage {
    pub async fn classify(&self, stop: RawSyscallStop) -> ClassifiedEvent { /* ... */ }
}
```

Key: this is where fd table updates happen (open/close/dup/fork), pipe registry updates (pipe/fork/close), PTY registry updates (openat ptmx/ioctl/openat pts). Write classification (file vs pipe vs pty vs socket vs devnull) happens here by looking up the fd.

### Stage 3: CheckRulesStage

ClassifiedEvent → ClassifiedEvent | Blocked. Hot-reloadable rules via ArcSwap.

```rust
pub struct CheckRulesStage {
    rules: Arc<ArcSwap<RuleSet>>,
    ptrace: PtraceStream,
}

impl CheckRulesStage {
    pub fn check_block(&self, event: &ClassifiedEvent) -> Option<RuleMatch> { /* ... */ }
    pub fn needs_approval(&self, event: &ClassifiedEvent) -> bool { /* ... */ }
}
```

Block rules: immediate InjectError + Blocked event, skip rest of pipeline.
Pause rules: flag for approval stage.
No match: pass through.

### Stage 4: ApprovalStage

Flagged events wait for human/LLM verdict. Escalation chain.

```rust
pub struct ApprovalStage {
    approvers: Approvers,  // Vec<Box<dyn Approver>>, escalation chain
    ptrace: PtraceStream,
}

impl ApprovalStage {
    pub async fn process(&self, event: ClassifiedEvent, needs_approval: bool) -> Option<ClassifiedEvent> { /* ... */ }
}
```

Returns None if denied (InjectError already sent). Returns Some if approved.

### Stage 5: CaptureStage

ClassifiedEvent → CapturedEvent. Reads tracee memory, reads files for before_hash, emits Content/Manifest records to bus. Per-path write locks. Adaptive capture policy.

```rust
pub struct CaptureStage {
    ptrace: PtraceStream,
    bus: RecordBus,
    policy: CapturePolicy,
    write_locks: DashMap<PathBuf, tokio::sync::Mutex<()>>,
}

const CHUNK_THRESHOLD: usize = 256 * 1024;

impl CaptureStage {
    pub async fn capture(&self, event: ClassifiedEvent) -> CapturedEvent { /* ... */ }
}
```

Small content (<256KB): single ReadMemory directive, single Content record.
Large content (≥256KB): streaming chunks — one ReadMemory per 4MB, one Content record per chunk, one Manifest at the end. Peak memory: one chunk.

Per-path write lock: acquire before reading before_hash, hold through syscall execution, release after capturing after_hash. Ensures hash chain correctness for concurrent writes.

CapturePolicy: static path rules (full/metadata_only/ignore) + dynamic rate limit per process + global budget. Degrades to MetadataOnly under load, never to silence.

### Stage 6: TreeStage

Updates in-memory Merkle tree. Emits Checkpoint records periodically.

```rust
pub struct TreeStage {
    tree: Mutex<MerkleTree>,
    bus: RecordBus,
    checkpoint_interval: u64,
    events_since_checkpoint: AtomicU64,
}

impl TreeStage {
    pub fn update(&self, event: &CapturedEvent) -> Option<ContentHash> { /* ... */ }
}
```

Returns tree_hash for mutating events, None for non-mutating.

### Stage 7: StampStage

Attaches seq, ts_monotonic, ts_wall, agent_id. Produces final Event.

```rust
pub struct StampStage {
    seq_gen: SequenceGenerator,
    agent_id: String,
}

impl StampStage {
    pub fn stamp(&self, captured: CapturedEvent, tree_hash: Option<ContentHash>) -> Event { /* ... */ }
}
```

---

## Pipeline Runner

Connects all stages. Drives the pipeline.

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
}

impl PipelineRunner {
    pub async fn run(mut self) {
        while let Some(stop) = self.ptrace.next().await {
            if let Some(ref mut rec) = self.recorder { rec.record(&stop); }

            let classified = self.classify.classify(stop).await;

            if matches!(classified.classification, Classification::Passthrough) {
                self.ptrace.directive(PipelineDirective::Resume { pid: classified.pid });
                continue;
            }

            if let Some(RuleMatch::Block { .. }) = self.rules.check_block(&classified) {
                self.ptrace.directive(PipelineDirective::InjectError { pid: classified.pid, errno: libc::EPERM });
                let blocked = self.stamp.stamp_blocked(&classified);
                self.bus.emit(Record::Event(blocked));
                continue;
            }

            let needs_approval = self.rules.needs_approval(&classified);
            if needs_approval {
                if self.approvals.process(classified.clone(), true).await.is_none() {
                    continue; // denied
                }
            }

            let captured = self.capture.capture(classified).await;
            self.ptrace.directive(PipelineDirective::Resume { pid: captured.pid });

            let tree_hash = self.tree.update(&captured);
            let event = self.stamp.stamp(captured, tree_hash);
            self.bus.emit(Record::Event(event));
        }
    }
}
```

---

## RecordBus and Sinks

### Sink trait

```rust
pub trait Sink: Send + Sync {
    fn priority(&self) -> SinkPriority;
    fn accept(&self, record: &Record) -> bool { true }
    fn write(&self, record: Record) -> Result<()>;
    fn flush(&self) -> Result<()>;
    fn shutdown(&self) -> Result<()> { self.flush() }
    fn name(&self) -> &str;
}

pub enum SinkPriority { Blocking, Async }
```

### RecordBus

```rust
pub struct RecordBus {
    blocking: Vec<Arc<dyn Sink>>,
    async_sinks: Vec<Arc<dyn Sink>>,
}

impl RecordBus {
    pub fn new(sinks: Vec<Arc<dyn Sink>>) -> Self { /* partition by priority */ }
    pub fn emit(&self, record: Record) {
        // Blocking sinks first, then async
        for sink in &self.blocking { if sink.accept(&record) { sink.write(record.clone()).ok(); } }
        for sink in &self.async_sinks { if sink.accept(&record) { sink.write(record.clone()).ok(); } }
    }
    pub fn flush_all(&self) { /* ... */ }
    pub fn shutdown_all(&self) { /* ... */ }
}
```

### Implementations

**LocalCasSink** [blocking] — Content, Manifest, Checkpoint → local disk
**EventLogSink** [blocking] — Event → JSONL segments, rotate + upload
**IndexSink** [blocking] — Event → path/pid/type indexes
**RemoteCasSink** [async] — Content, Manifest, Checkpoint → S3 via upload pool + digest cache
**BroadcastSink** [async] — Event → broadcast channel → WebSocket

BroadcastSink rejects Content (accept returns false). RemoteCasSink rejects Events.

---

## CapturePolicy

Static rules + dynamic degradation. Used by CaptureStage.

```rust
pub enum CaptureLevel { Full, MetadataOnly, Ignore }

pub struct CapturePolicy {
    rules: Vec<CaptureRule>,
    rate: DashMap<u32, RateCounter>,
    budget: AtomicU64,
    window_budget: u64,
}

impl CapturePolicy {
    pub fn level(&self, path: &Path, pid: u32, size: usize) -> CaptureLevel {
        // 1. Static rules win if matched
        // 2. Per-process rate limit → MetadataOnly if exceeded
        // 3. Global budget → MetadataOnly if exhausted
        // 4. Default: Full
    }
    pub fn reset_budget(&self) { /* called periodically */ }
}
```

---

## Replay

Record RawSyscallStops for offline replay through the pipeline with different rules/policies.

```rust
pub struct RawStopRecorder {
    writer: BufWriter<File>,
}

impl RawStopRecorder {
    pub fn record(&mut self, stop: &RawSyscallStop) { /* JSONL */ }
}

pub struct ReplayStream {
    reader: BufReader<File>,
}

impl Stream for ReplayStream {
    type Item = RawSyscallStop;
    fn poll_next(/* ... */) -> Poll<Option<Self::Item>> { /* read JSONL lines */ }
}
```

Replay connects to a MockPtrace that serves canned memory contents instead of real process_vm_readv.

---

## Startup Wiring

```rust
// 1. Storage
let local_cas = LocalCas::new(data_dir.join("cas"))?;
let digest_cache = Arc::new(DigestCache::load_or_rebuild(&config.storage)?);
let upload_pool = UploadPool::new(object_store, config.storage.upload.clone())?;
let event_log = EventLog::new(data_dir.join("events"), &config.agent_id, upload_pool.clone())?;
let indexes = Indexes::new(data_dir.join("indexes"))?;
let (broadcast_tx, _) = tokio::sync::broadcast::channel(4096);

// 2. Sinks + bus
let bus = RecordBus::new(vec![
    Arc::new(LocalCasSink::new(local_cas.clone())),
    Arc::new(EventLogSink::new(event_log)),
    Arc::new(IndexSink::new(indexes)),
    Arc::new(RemoteCasSink::new(upload_pool.clone(), digest_cache.clone(), config.agent_id.clone())),
    Arc::new(BroadcastSink::new(broadcast_tx.clone())),
]);

// 3. Initial state
capture_initial_state(&bus, &config.watch_paths)?;

// 4. TLS
let ca_paths = net::generate_ca(&data_dir.join("tls"))?;
let mitmdump = net::start_mitmdump(ca_paths, config.proxy_port)?;
let agent_env = build_agent_env(&config, &ca_paths);

// 5. Spawn agent, get ptrace stream
let (ptrace_stream, ptrace_handle) = PtraceStream::spawn(child_pid);

// 6. Pipeline stages
let classify = ClassifyStage::new(ptrace_stream.clone(), fd_tables, pipe_reg, pty_reg);
let rules_stage = CheckRulesStage::new(rules_handle.clone(), ptrace_stream.clone());
let approvals = ApprovalStage::new(approver_chain, ptrace_stream.clone());
let capture = CaptureStage::new(ptrace_stream.clone(), bus.clone(), capture_policy);
let tree = TreeStage::new(MerkleTree::new(), bus.clone(), config.checkpoints.interval);
let stamp = StampStage::new(seq_gen, config.agent_id.clone());
let recorder = config.record_raw_stops.then(|| RawStopRecorder::new(&data_dir.join("raw_stops.jsonl")).unwrap());

// 7. TLS watcher on same bus
let tls_bus = bus.clone();
tokio::spawn(async move { tls_watcher::run(tls_bus, mitmdump).await });

// 8. API server
tokio::spawn(async move { api::serve(api_state, config.listen_addr).await });

// 9. Run pipeline (blocks until agent exits)
PipelineRunner { ptrace: ptrace_stream, classify, rules: rules_stage, approvals, capture, tree, stamp, bus: bus.clone(), recorder }.run().await;

// 10. Shutdown
bus.shutdown_all();
ptrace_handle.join().ok();
```

---

## Proxy Integration

The proxy is already built and working. Two things need to happen in the migration:

**1. TLS watcher → bus.** Already in the spec. Replace `event_tx` with `bus`. `FlowWatcher` and `KeylogWatcher` emit `Record::Content` (bodies, headers, keylog entries) and `Record::Event` (HttpRequest, HttpResponse, TlsKeys) directly to the bus. This is a parallel source alongside the pipeline, not part of it.

**2. Transparent mode connect() rewrite is NOT in the spec.** That's the gap. In transparent mode, the classify stage needs to rewrite the tracee's sockaddr before resuming the connect(). The ptrace thread needs a new directive:

```rust
WriteMemory {
    pid: Pid,
    addr: usize,
    data: Vec<u8>,
    reply: oneshot::Sender<Result<()>>,
},
```

And the classify stage's connect() handling in transparent mode becomes:

```rust
libc::SYS_connect => {
    let fd = args.arg0 as i32;
    let sockaddr_addr = args.arg1 as usize;
    let sockaddr_len = args.arg2 as usize;

    // Read original sockaddr from tracee
    let (reply_tx, reply_rx) = oneshot::channel();
    self.ptrace.directive(PipelineDirective::ReadMemory {
        pid, addr: sockaddr_addr, len: sockaddr_len, reply: reply_tx,
    });
    let sockaddr_bytes = reply_rx.await??;
    let original_dest = parse_sockaddr(&sockaddr_bytes);

    // Transparent mode: rewrite to proxy if TCP 443/8443 and not loopback
    if self.transparent_mode && is_tls_port(&original_dest) && !original_dest.ip().is_loopback() {
        let proxy_sockaddr = build_sockaddr(self.proxy_addr); // 127.0.0.1:8080
        let (reply_tx, reply_rx) = oneshot::channel();
        self.ptrace.directive(PipelineDirective::WriteMemory {
            pid, addr: sockaddr_addr, data: proxy_sockaddr, reply: reply_tx,
        });
        reply_rx.await??;
    }

    Classification::NetConnect { fd, addr: original_dest } // always record original
}
```

And the ptrace thread handles it:

```rust
Some(PipelineDirective::WriteMemory { pid, addr, data, reply }) => {
    let result = write_tracee_memory(pid, addr, &data);
    let _ = reply.send(result);
    // Don't resume — pipeline sends Resume after classification completes
}
```

That's the only addition. The rest of the proxy wiring (mitmdump startup, addon script, CA generation, env vars) stays exactly where it is — in the supervisor startup sequence. The TLS watcher thread polls flows.jsonl and keylog.txt the same way, just emits to bus instead of event_tx.

## Files to Delete

```
crates/argus/src/storage/pipeline.rs
crates/argus/src/storage/pipeline_sink.rs
crates/argus/src/tracer/trace_loop.rs
crates/argus/src/tracer/content_capture.rs
crates/argus/src/tracer/handlers/mod.rs
crates/argus/src/tracer/handlers/io_ops.rs
crates/argus/src/tracer/handlers/metadata_ops.rs
crates/argus/src/tracer/handlers/file_ops.rs
crates/argus/src/tracer/handlers/net_ops.rs
```

## New File Structure

```
crates/argus/src/pipeline/
    mod.rs
    record.rs
    raw_stop.rs
    directive.rs
    classified.rs
    captured.rs
    sink.rs
    bus.rs
    runner.rs
    capture_policy.rs
    replay.rs
    ptrace_thread.rs
    stages/
        mod.rs
        classify.rs
        check_rules.rs
        approvals.rs
        capture.rs
        tree.rs
        stamp.rs
    sinks/
        mod.rs
        local_cas.rs
        remote_cas.rs
        event_log.rs
        index.rs
        broadcast.rs
        memory.rs
```

## Config Additions

```yaml
capture:
  content:
    paths: ["/workspace/src/**", "*.py", "*.yaml", "*.json", "*.toml", "*.rs"]
  metadata_only:
    paths: ["/workspace/target/**", "**/node_modules/**", "**/*.o", "**/*.so"]
  ignore:
    paths: ["**/__pycache__/**", "**/.git/objects/**"]
  rate_limit_bytes_per_sec: 104857600
  budget_bytes_per_window: 1073741824
  budget_window_seconds: 60

record_raw_stops: false
```

## Migration Checklist

1. [ ] Create `pipeline/` module: record.rs, raw_stop.rs, directive.rs, classified.rs, captured.rs, sink.rs, bus.rs, capture_policy.rs, replay.rs
2. [ ] Create `pipeline/ptrace_thread.rs` — extract waitpid loop, make dumb directive executor
3. [ ] Create `pipeline/stages/` — classify.rs, check_rules.rs, approvals.rs, capture.rs, tree.rs, stamp.rs
4. [ ] Create `pipeline/sinks/` — local_cas.rs, remote_cas.rs, event_log.rs, index.rs, broadcast.rs, memory.rs
5. [ ] Create `pipeline/runner.rs` — PipelineRunner
6. [ ] Add `put_with_hash()` to LocalCas
7. [ ] Add `is_mutating()` to EventPayload
8. [ ] Create MockPtrace test helper
9. [ ] Update TLS watcher: replace cas + event_tx with bus
10. [ ] Update main.rs: new startup wiring
11. [ ] Delete old tracer files (trace_loop.rs, content_capture.rs, handlers/*)
12. [ ] Delete old pipeline files (pipeline.rs, pipeline_sink.rs)
13. [ ] Remove event_writer thread from main.rs
14. [ ] Update API server: broadcast receiver + LocalCas for reads
15. [ ] Add capture config parsing
16. [ ] Add record_raw_stops config option
17. [ ] Tests: stage isolation, end-to-end with MockPtrace, replay, bus ordering, capture policy
18. [ ] Verify: JSONL format unchanged
19. [ ] Verify: CAS objects in S3
20. [ ] Verify: checkpoints created + uploaded
21. [ ] Verify: chunked streaming for large files
22. [ ] Verify: replay produces same payloads as live
