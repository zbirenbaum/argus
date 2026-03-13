# Data Pipeline Migration Spec

One-shot refactor. Replace the current broken data flow (tracer → cas.put() directly, CAS never uploaded, checkpoints never created) with a RecordBus/Sink architecture.

## Current State (broken)

```
Tracer → cas.put() directly (local only, never uploaded)
       → event_tx channel → event_writer thread → PipelineSink → EventLog → S3 ✓
TLS watcher → cas.put() directly (local only, never uploaded)
            → event_tx channel → same path ✓
StoragePipeline.store_content() exists but is never called by anyone.
Checkpoints: infrastructure exists, never invoked.
```

## Target State

```
Tracer ─────────────┐
TLS watcher ────────┤
                    ▼
               RecordBus
                    │
        ┌───────────┼───────────────┬──────────────┬─────────────┐
        ▼           ▼               ▼              ▼             ▼
  LocalCasSink  EventLogSink   IndexSink   MerkleTreeSink  BroadcastSink
  [blocking]    [blocking]     [blocking]   [blocking]      [async]
        │                                       │
        │                                       ▼
        │                              emits Record::Checkpoint
        │                              back into bus
        │
        ▼
  RemoteCasSink ← reads from LocalCas, uploads to S3
  [async, background]
```

## New Types

### Record

The universal unit of data in the pipeline. Everything the supervisor produces is a Record.

```rust
// crates/argus/src/pipeline/record.rs

#[derive(Clone, Debug)]
pub enum Record {
    /// Structured event (JSONL, indexes, broadcast)
    Event(Event),

    /// Content blob — file body, stdio, HTTP body, keylog entry
    /// Small content (<256KB): single record, data is the full content
    /// Large content (>=256KB): one record per chunk
    Content {
        hash: ContentHash,
        data: Vec<u8>,
    },

    /// Ordered list of chunk hashes that compose a large file
    /// The hash is the SHA-256 of the full content (not of the manifest itself)
    Manifest {
        hash: ContentHash,
        chunks: Vec<ContentHash>,
    },

    /// Serialized Merkle tree state
    Checkpoint {
        seq: u64,
        data: Vec<u8>,
    },
}
```

### SinkPriority

```rust
// crates/argus/src/pipeline/sink.rs

pub enum SinkPriority {
    /// Must complete before tracee resumes. Determines durability.
    Blocking,
    /// Best-effort. Enqueue and return immediately. Never adds latency to trace loop.
    Async,
}
```

### Sink trait

```rust
// crates/argus/src/pipeline/sink.rs

pub trait Sink: Send + Sync {
    /// Blocking or Async. Bus processes all Blocking sinks before any Async sinks.
    fn priority(&self) -> SinkPriority;

    /// Opt-out filter. Default true. Return false to skip this record.
    /// Used for: preventing feedback loops, keeping large blobs out of broadcast.
    fn accept(&self, record: &Record) -> bool { true }

    /// Process one record. For Blocking sinks, must complete before returning.
    /// For Async sinks, should enqueue internally and return fast.
    fn write(&self, record: Record) -> Result<()>;

    /// Flush any buffered state to durable storage.
    fn flush(&self) -> Result<()>;

    /// Clean shutdown. Flush + drain any async queues.
    fn shutdown(&self) -> Result<()> { self.flush() }

    /// Human-readable name for logging.
    fn name(&self) -> &str;
}
```

### RecordBus

```rust
// crates/argus/src/pipeline/bus.rs

pub struct RecordBus {
    blocking: Vec<Arc<dyn Sink>>,
    async_sinks: Vec<Arc<dyn Sink>>,
}

impl RecordBus {
    pub fn new(sinks: Vec<Arc<dyn Sink>>) -> Self {
        let (blocking, async_sinks) = sinks.into_iter().partition(|s| {
            matches!(s.priority(), SinkPriority::Blocking)
        });
        Self { blocking, async_sinks }
    }

    /// Emit a record. Blocking sinks run first, then async sinks.
    pub fn emit(&self, record: Record) {
        for sink in &self.blocking {
            if sink.accept(&record) {
                if let Err(e) = sink.write(record.clone()) {
                    tracing::error!(sink = sink.name(), error = %e, "blocking sink write failed");
                }
            }
        }
        for sink in &self.async_sinks {
            if sink.accept(&record) {
                if let Err(e) = sink.write(record.clone()) {
                    tracing::warn!(sink = sink.name(), error = %e, "async sink write failed");
                }
            }
        }
    }

    pub fn flush_all(&self) {
        for sink in self.blocking.iter().chain(self.async_sinks.iter()) {
            if let Err(e) = sink.flush() {
                tracing::error!(sink = sink.name(), error = %e, "flush failed");
            }
        }
    }

    pub fn shutdown_all(&self) {
        for sink in self.blocking.iter().chain(self.async_sinks.iter()) {
            if let Err(e) = sink.shutdown() {
                tracing::error!(sink = sink.name(), error = %e, "shutdown failed");
            }
        }
    }
}

impl Clone for RecordBus {
    fn clone(&self) -> Self {
        Self {
            blocking: self.blocking.clone(),
            async_sinks: self.async_sinks.clone(),
        }
    }
}
```

### CapturePolicy

```rust
// crates/argus/src/pipeline/capture_policy.rs

pub enum CaptureLevel {
    /// Full content capture — hash, store in CAS, include in events
    Full,
    /// Events only — log path, pid, size, timestamp. No content stored.
    MetadataOnly,
    /// No events, no content. Completely invisible.
    Ignore,
}

pub struct CapturePolicy {
    /// Static rules from config, compiled glob patterns
    rules: Vec<CaptureRule>,
    /// Per-process write rate tracking
    rate: DashMap<u32, RateCounter>,
    /// Global content budget (bytes remaining in current window)
    budget: AtomicU64,
    /// Budget reset amount
    window_budget: u64,
}

pub struct CaptureRule {
    pub paths: Vec<glob::Pattern>,
    pub level: CaptureLevel,
}

impl CapturePolicy {
    pub fn new(rules: Vec<CaptureRule>, window_budget: u64) -> Self {
        Self {
            rules,
            rate: DashMap::new(),
            budget: AtomicU64::new(window_budget),
            window_budget,
        }
    }

    /// Determine capture level for a given write.
    /// Static rules take priority. Dynamic rate/budget limits degrade gracefully.
    pub fn level(&self, path: &Path, pid: u32, size: usize) -> CaptureLevel {
        // Static rules win
        for rule in &self.rules {
            for pattern in &rule.paths {
                if pattern.matches_path(path) {
                    return rule.level;
                }
            }
        }

        // Dynamic: per-process rate limit
        let mut rate = self.rate.entry(pid).or_insert_with(RateCounter::new);
        rate.record(size);
        if rate.bytes_per_sec() > CONTENT_RATE_LIMIT_BYTES {
            return CaptureLevel::MetadataOnly;
        }

        // Dynamic: global budget
        let remaining = self.budget.load(Ordering::Relaxed);
        if remaining < size as u64 {
            return CaptureLevel::MetadataOnly;
        }
        self.budget.fetch_sub(size as u64, Ordering::Relaxed);

        CaptureLevel::Full
    }

    /// Called periodically (e.g., every 10s) to reset budget window.
    pub fn reset_budget(&self) {
        self.budget.store(self.window_budget, Ordering::Relaxed);
    }
}

/// Sliding window byte counter.
pub struct RateCounter {
    bytes: u64,
    window_start: Instant,
}

const CONTENT_RATE_LIMIT_BYTES: u64 = 100 * 1024 * 1024; // 100MB/sec → MetadataOnly
const RATE_WINDOW_SECS: u64 = 5;

impl RateCounter {
    pub fn new() -> Self {
        Self { bytes: 0, window_start: Instant::now() }
    }

    pub fn record(&mut self, size: usize) {
        let elapsed = self.window_start.elapsed().as_secs();
        if elapsed >= RATE_WINDOW_SECS {
            self.bytes = 0;
            self.window_start = Instant::now();
        }
        self.bytes += size as u64;
    }

    pub fn bytes_per_sec(&self) -> u64 {
        let elapsed = self.window_start.elapsed().as_secs_f64().max(0.1);
        (self.bytes as f64 / elapsed) as u64
    }
}
```

### Content emission helper

```rust
// crates/argus/src/pipeline/content.rs

const CHUNK_THRESHOLD: usize = 256 * 1024; // 256KB

/// Emit content to the bus. Small content goes as a single Record.
/// Large content is streamed as chunks directly from tracee memory
/// to keep supervisor memory flat.
pub fn emit_content_from_tracee(
    bus: &RecordBus,
    pid: Pid,
    addr: usize,
    len: usize,
) -> ContentHash {
    if len <= CHUNK_THRESHOLD {
        let data = process_vm_readv_all(pid, addr, len);
        let hash = ContentHash::from_bytes(&data);
        bus.emit(Record::Content { hash: hash.clone(), data });
        hash
    } else {
        emit_chunked_streaming(bus, pid, addr, len)
    }
}

/// Stream chunks from tracee memory one at a time.
/// Peak memory: one chunk (~4MB), not the full file.
fn emit_chunked_streaming(
    bus: &RecordBus,
    pid: Pid,
    addr: usize,
    len: usize,
) -> ContentHash {
    let mut full_hasher = Sha256::new();
    let mut chunk_hashes = Vec::new();
    let mut offset = 0;

    while offset < len {
        let chunk_len = next_chunk_boundary(len - offset);
        let chunk = process_vm_readv_all(pid, addr + offset, chunk_len);

        full_hasher.update(&chunk);
        let hash = ContentHash::from_bytes(&chunk);
        chunk_hashes.push(hash.clone());

        bus.emit(Record::Content { hash, data: chunk });
        // chunk Vec dropped here — memory freed before next read

        offset += chunk_len;
    }

    let full_hash = ContentHash::from(full_hasher.finalize());
    bus.emit(Record::Manifest {
        hash: full_hash.clone(),
        chunks: chunk_hashes,
    });
    full_hash
}

/// Emit content from an in-memory buffer (TLS watcher, flow parser).
/// No tracee memory involved.
pub fn emit_content_from_bytes(bus: &RecordBus, data: &[u8]) -> ContentHash {
    let hash = ContentHash::from_bytes(data);
    bus.emit(Record::Content { hash: hash.clone(), data: data.to_vec() });
    hash
}

/// Simple Rabin-like boundary: fixed 4MB chunks for now.
/// Replace with content-defined chunking later if dedup matters.
fn next_chunk_boundary(remaining: usize) -> usize {
    remaining.min(4 * 1024 * 1024)
}
```

## Sink Implementations

### LocalCasSink

```rust
// crates/argus/src/pipeline/sinks/local_cas.rs

pub struct LocalCasSink {
    cas: LocalCas,
}

impl Sink for LocalCasSink {
    fn priority(&self) -> SinkPriority { SinkPriority::Blocking }
    fn name(&self) -> &str { "local-cas" }

    fn accept(&self, record: &Record) -> bool {
        matches!(record, Record::Content { .. } | Record::Manifest { .. } | Record::Checkpoint { .. })
    }

    fn write(&self, record: Record) -> Result<()> {
        match record {
            Record::Content { hash, data } => {
                self.cas.put_with_hash(&hash, &data)?;
            }
            Record::Manifest { hash, chunks } => {
                let manifest_bytes = serde_json::to_vec(&chunks)?;
                self.cas.put_with_hash(&hash, &manifest_bytes)?;
            }
            Record::Checkpoint { seq, data } => {
                let hash = ContentHash::from_bytes(&data);
                self.cas.put_with_hash(&hash, &data)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn flush(&self) -> Result<()> { Ok(()) } // LocalCas writes are already fsync'd
}
```

### RemoteCasSink

```rust
// crates/argus/src/pipeline/sinks/remote_cas.rs

pub struct RemoteCasSink {
    upload_pool: UploadPool,
    digest_cache: Arc<DigestCache>,
    agent_id: String,
}

impl Sink for RemoteCasSink {
    fn priority(&self) -> SinkPriority { SinkPriority::Async }
    fn name(&self) -> &str { "remote-cas" }

    fn accept(&self, record: &Record) -> bool {
        matches!(record, Record::Content { .. } | Record::Manifest { .. } | Record::Checkpoint { .. })
    }

    fn write(&self, record: Record) -> Result<()> {
        match record {
            Record::Content { hash, data } => {
                if !self.digest_cache.contains(&hash) {
                    self.upload_pool.submit(UploadJob::CasObject { hash, data })?;
                }
            }
            Record::Manifest { hash, chunks } => {
                if !self.digest_cache.contains(&hash) {
                    let data = serde_json::to_vec(&chunks)?;
                    self.upload_pool.submit(UploadJob::CasObject { hash, data })?;
                }
            }
            Record::Checkpoint { seq, data } => {
                self.upload_pool.submit(UploadJob::Checkpoint {
                    agent_id: self.agent_id.clone(),
                    seq,
                    data,
                })?;
            }
            _ => {}
        }
        Ok(())
    }

    fn flush(&self) -> Result<()> {
        // Drain confirmations, update digest cache
        while let Ok(confirmation) = self.upload_pool.confirmations().try_recv() {
            self.digest_cache.insert(confirmation.hash, confirmation.size);
        }
        Ok(())
    }

    fn shutdown(&self) -> Result<()> {
        self.flush()?;
        // Upload digest cache snapshot
        let snapshot = self.digest_cache.serialize()?;
        self.upload_pool.submit(UploadJob::DigestCacheSnapshot {
            agent_id: self.agent_id.clone(),
            data: snapshot,
        })?;
        Ok(())
    }
}
```

### EventLogSink

```rust
// crates/argus/src/pipeline/sinks/event_log.rs

pub struct EventLogSink {
    log: Mutex<EventLog>,  // Mutex only because EventLog::append takes &mut self
}

impl Sink for EventLogSink {
    fn priority(&self) -> SinkPriority { SinkPriority::Blocking }
    fn name(&self) -> &str { "event-log" }

    fn accept(&self, record: &Record) -> bool {
        matches!(record, Record::Event(_))
    }

    fn write(&self, record: Record) -> Result<()> {
        if let Record::Event(event) = record {
            let mut log = self.log.lock().map_err(|_| anyhow!("event log lock poisoned"))?;
            log.append(&event)?;
        }
        Ok(())
    }

    fn flush(&self) -> Result<()> {
        let mut log = self.log.lock().map_err(|_| anyhow!("event log lock poisoned"))?;
        log.flush()?;
        // Rotate uploads handled by EventLog internally
        Ok(())
    }
}
```

Note: this Mutex is acceptable — only the ptrace thread writes events, and the lock is never contended because async sinks never touch it. It's only needed because EventLog::append takes `&mut self`. If EventLog is refactored to use interior mutability, remove it.

### IndexSink

```rust
// crates/argus/src/pipeline/sinks/index.rs

pub struct IndexSink {
    indexes: Indexes,  // assumed to use interior mutability (DashMap or similar)
}

impl Sink for IndexSink {
    fn priority(&self) -> SinkPriority { SinkPriority::Blocking }
    fn name(&self) -> &str { "index" }

    fn accept(&self, record: &Record) -> bool {
        matches!(record, Record::Event(_))
    }

    fn write(&self, record: Record) -> Result<()> {
        if let Record::Event(event) = record {
            self.indexes.index_event(&event)?;
        }
        Ok(())
    }

    fn flush(&self) -> Result<()> {
        self.indexes.flush()
    }
}
```

### MerkleTreeSink

```rust
// crates/argus/src/pipeline/sinks/merkle.rs

pub struct MerkleTreeSink {
    tree: Mutex<MerkleTree>,
    cas: Arc<dyn Cas>,
    checkpoint_interval: u64,
    events_since_checkpoint: AtomicU64,
    bus: RecordBus,  // to emit Checkpoint records back
    agent_id: String,
}

impl Sink for MerkleTreeSink {
    fn priority(&self) -> SinkPriority { SinkPriority::Blocking }
    fn name(&self) -> &str { "merkle-tree" }

    fn accept(&self, record: &Record) -> bool {
        // Accept mutating events. Reject Checkpoint to prevent feedback loop.
        match record {
            Record::Event(e) => e.payload.is_mutating(),
            Record::Checkpoint { .. } => false,
            _ => false,
        }
    }

    fn write(&self, record: Record) -> Result<()> {
        if let Record::Event(event) = record {
            let mut tree = self.tree.lock().map_err(|_| anyhow!("tree lock poisoned"))?;

            match &event.payload {
                EventPayload::Write { path, after_hash, .. } => {
                    if let Some(hash) = after_hash {
                        tree.insert(path, hash.clone());
                    }
                }
                EventPayload::Rename { old_path, new_path, .. } => {
                    tree.rename(old_path, new_path);
                }
                EventPayload::Unlink { path, .. } => {
                    tree.remove(path);
                }
                EventPayload::Mkdir { path, .. } => {
                    tree.mkdir(path);
                }
                EventPayload::Rmdir { path, .. } => {
                    tree.remove(path);
                }
                EventPayload::Truncate { path, after_hash, .. } => {
                    if let Some(hash) = after_hash {
                        tree.insert(path, hash.clone());
                    }
                }
                EventPayload::Link { link_path, target, .. } => {
                    if let Some(hash) = tree.get_hash(target) {
                        tree.insert(link_path, hash);
                    }
                }
                EventPayload::Symlink { link_path, .. } => {
                    // Symlinks tracked as metadata, not content
                }
                EventPayload::InitialFile { path, content_hash, .. } => {
                    tree.insert(path, content_hash.clone());
                }
                _ => {}
            }

            // Periodic checkpoint
            let count = self.events_since_checkpoint.fetch_add(1, Ordering::Relaxed);
            if count > 0 && count % self.checkpoint_interval == 0 {
                let data = tree.serialize()?;
                self.bus.emit(Record::Checkpoint { seq: event.seq, data });
            }
        }
        Ok(())
    }

    fn flush(&self) -> Result<()> { Ok(()) }

    fn shutdown(&self) -> Result<()> {
        // Final checkpoint
        let tree = self.tree.lock().map_err(|_| anyhow!("tree lock poisoned"))?;
        let data = tree.serialize()?;
        let seq = self.events_since_checkpoint.load(Ordering::Relaxed);
        self.bus.emit(Record::Checkpoint { seq, data });
        Ok(())
    }
}
```

### BroadcastSink

```rust
// crates/argus/src/pipeline/sinks/broadcast.rs

pub struct BroadcastSink {
    tx: tokio::sync::broadcast::Sender<Event>,
}

impl Sink for BroadcastSink {
    fn priority(&self) -> SinkPriority { SinkPriority::Async }
    fn name(&self) -> &str { "broadcast" }

    fn accept(&self, record: &Record) -> bool {
        // Events only. Never send content blobs to WebSocket clients.
        matches!(record, Record::Event(_))
    }

    fn write(&self, record: Record) -> Result<()> {
        if let Record::Event(event) = record {
            let _ = self.tx.send(event); // drop if no receivers
        }
        Ok(())
    }

    fn flush(&self) -> Result<()> { Ok(()) }
}
```

## Tracer Changes

### TracerLoop fields

Remove:
```rust
// DELETE these fields
cas: LocalCas,              // direct CAS access — gone
event_tx: Sender<Event>,    // old event channel — gone
```

Add:
```rust
// ADD these fields
bus: RecordBus,
capture_policy: CapturePolicy,
```

### Content capture callsites

Every place that currently calls `cas.put()` changes to emit through the bus.

**crates/argus/src/tracer/content_capture.rs:**

Replace all functions. The module becomes a thin wrapper around `emit_content_from_tracee` and `emit_content_from_bytes`.

```rust
/// Capture a write buffer from tracee memory. Returns content hash or None.
pub fn try_capture_write(
    bus: &RecordBus,
    policy: &CapturePolicy,
    pid: Pid,
    path: &Path,
    addr: usize,
    len: usize,
) -> Option<ContentHash> {
    match policy.level(path, pid.as_raw() as u32, len) {
        CaptureLevel::Full => {
            Some(emit_content_from_tracee(bus, pid, addr, len))
        }
        CaptureLevel::MetadataOnly | CaptureLevel::Ignore => None,
    }
}

/// Capture file content by reading the file directly (for before_hash).
pub fn try_capture_file(
    bus: &RecordBus,
    policy: &CapturePolicy,
    path: &Path,
) -> Option<ContentHash> {
    match policy.level(path, 0, 0) {
        CaptureLevel::Full => {
            let data = std::fs::read(path).ok()?;
            Some(emit_content_from_bytes(bus, &data))
        }
        _ => None,
    }
}
```

**crates/argus/src/tracer/handlers/io_ops.rs — handle_write():**

Before:
```rust
let after_hash = try_capture_flat(&self.cas, pid, buf_addr, count);
self.emit(Write { path, before_hash, after_hash, ... });
```

After:
```rust
let after_hash = try_capture_write(&self.bus, &self.capture_policy, pid, &path, buf_addr, count);
self.bus.emit(Record::Event(Event::new(&self.seq_gen, &self.agent_id, Write {
    path, before_hash, after_hash, size: count, ...
})));
```

Same pattern for: `handle_read`, `handle_write`, `handle_readv`, `handle_writev` in io_ops.rs. And `handle_unlink`, `handle_truncate` in metadata_ops.rs.

### Event emission

Before:
```rust
fn emit(&self, payload: EventPayload) {
    let event = Event::new(&self.seq_gen, &self.agent_id, payload);
    let _ = self.event_tx.send(event);
}
```

After:
```rust
fn emit(&self, payload: EventPayload) {
    let event = Event::new(&self.seq_gen, &self.agent_id, payload);
    self.bus.emit(Record::Event(event));
}
```

### Write locking integration

No change to the write lock mechanism. The lock guards when content is captured and when the tracee resumes. The only difference is that `cas.put()` inside the lock is replaced by `bus.emit(Record::Content)`. The LocalCasSink (blocking) writes to disk before `bus.emit()` returns, so the durability guarantee is preserved.

## TLS Watcher Changes

### crates/supervisor/src/tls_watcher.rs

Replace the `cas` and `event_tx` fields with `bus`:

Before:
```rust
struct TlsWatcher {
    cas: LocalCas,
    event_tx: Sender<Event>,
    // ...
}
```

After:
```rust
struct TlsWatcher {
    bus: RecordBus,
    // ...
}
```

### crates/argus/src/net/flow_watcher.rs

Before:
```rust
fn store_headers(cas: &dyn Cas, headers: &HashMap<String, String>) -> Option<ContentHash> {
    let data = serde_json::to_vec(headers).ok()?;
    cas.put(&data).ok()
}

fn store_body(cas: &dyn Cas, body_b64: &str) -> Option<ContentHash> {
    let data = base64::decode(body_b64).ok()?;
    cas.put(&data).ok()
}
```

After:
```rust
fn store_headers(bus: &RecordBus, headers: &HashMap<String, String>) -> Option<ContentHash> {
    let data = serde_json::to_vec(headers).ok()?;
    Some(emit_content_from_bytes(bus, &data))
}

fn store_body(bus: &RecordBus, body_b64: &str) -> Option<ContentHash> {
    let data = base64::decode(body_b64).ok()?;
    Some(emit_content_from_bytes(bus, &data))
}
```

### crates/argus/src/net/keylog.rs

Same pattern — replace `cas.put()` with `emit_content_from_bytes(bus, &data)`.

## Startup Wiring

### crates/supervisor/src/main.rs

Remove:
```rust
// DELETE
let cas = LocalCas::new(data_dir.join("cas"))?;
let api_cas = LocalCas::new(data_dir.join("cas"))?;
let tls_cas = LocalCas::new(data_dir.join("cas"))?;
let (event_tx, event_rx) = std::sync::mpsc::channel();
// DELETE the event_writer thread
// DELETE PipelineSink
// DELETE StoragePipeline (replaced by sinks)
```

Add:
```rust
// CREATE sinks
let local_cas = LocalCas::new(data_dir.join("cas"))?;
let digest_cache = Arc::new(DigestCache::load_or_rebuild(&config.storage)?);
let upload_pool = UploadPool::new(object_store, config.storage.upload.clone())?;
let event_log = EventLog::new(data_dir.join("events"), config.agent_id.clone(), upload_pool.clone())?;
let indexes = Indexes::new(data_dir.join("indexes"))?;
let (broadcast_tx, _) = tokio::sync::broadcast::channel(4096);
let tree = MerkleTree::new();

let local_cas_sink = Arc::new(LocalCasSink::new(local_cas.clone()));
let remote_cas_sink = Arc::new(RemoteCasSink::new(upload_pool.clone(), digest_cache.clone(), config.agent_id.clone()));
let event_log_sink = Arc::new(EventLogSink::new(event_log));
let index_sink = Arc::new(IndexSink::new(indexes));
let broadcast_sink = Arc::new(BroadcastSink::new(broadcast_tx.clone()));

// Bus must exist before MerkleTreeSink (it needs a clone for checkpoint emission)
let bus = RecordBus::new(vec![
    local_cas_sink.clone(),
    remote_cas_sink.clone(),
    event_log_sink.clone(),
    index_sink.clone(),
    broadcast_sink.clone(),
]);

let merkle_sink = Arc::new(MerkleTreeSink::new(
    tree,
    Box::new(local_cas.clone()),
    config.checkpoints.interval,
    bus.clone(),
    config.agent_id.clone(),
));

// Rebuild bus with merkle sink included
let bus = RecordBus::new(vec![
    local_cas_sink,
    remote_cas_sink,
    event_log_sink,
    index_sink,
    broadcast_sink,
    merkle_sink,
]);

// Capture policy from config
let capture_policy = CapturePolicy::new(
    config.build_capture_rules(),
    config.capture.budget_bytes_per_window,
);

// TLS watcher gets bus clone
let tls_bus = bus.clone();

// API server gets broadcast receiver + shared state
let api_broadcast_rx = broadcast_tx.clone();

// Tracer gets bus + policy
let tracer = TracerLoop::new(bus, capture_policy, seq_gen, agent_id, ...);
```

### API server

The API server no longer needs access to LocalCas for reads. Content reads go through the CAS trait (local with S3 fallback). Provide `local_cas` for the content read endpoints. The broadcast receiver provides the WebSocket event stream.

## Files to Delete

```
crates/argus/src/storage/pipeline.rs         — replaced by RecordBus + sinks
crates/argus/src/storage/pipeline_sink.rs    — replaced by EventLogSink
```

Keep:
```
crates/argus/src/storage/upload_pool.rs      — used by RemoteCasSink
crates/argus/src/storage/upload_job.rs       — used by RemoteCasSink
crates/argus/src/storage/event_log.rs        — used by EventLogSink
crates/argus/src/storage/digest_cache.rs     — used by RemoteCasSink
crates/argus/src/storage/local_buffer.rs     — used by LocalCasSink (if eviction exists)
```

## New File Structure

```
crates/argus/src/pipeline/
    mod.rs
    record.rs           — Record enum
    sink.rs             — Sink trait, SinkPriority
    bus.rs              — RecordBus
    content.rs          — emit_content_from_tracee, emit_content_from_bytes, chunking
    capture_policy.rs   — CapturePolicy, CaptureLevel, AdaptivePolicy, RateCounter

crates/argus/src/pipeline/sinks/
    mod.rs
    local_cas.rs        — LocalCasSink
    remote_cas.rs       — RemoteCasSink
    event_log.rs        — EventLogSink
    index.rs            — IndexSink
    merkle.rs           — MerkleTreeSink
    broadcast.rs        — BroadcastSink
    memory.rs           — MemorySink (tests only)
```

## Config Changes

Add to supervisor.yaml:

```yaml
capture:
  # Static path rules
  content:
    paths: ["/workspace/src/**", "*.py", "*.yaml", "*.json", "*.toml", "*.rs"]
  metadata_only:
    paths: ["/workspace/target/**", "**/node_modules/**", "**/*.o", "**/*.so"]
  ignore:
    paths: ["**/__pycache__/**", "**/.git/objects/**"]

  # Dynamic limits
  rate_limit_bytes_per_sec: 104857600   # 100MB/s per process → degrade to metadata_only
  budget_bytes_per_window: 1073741824   # 1GB per window
  budget_window_seconds: 60
```

## Tests

### RecordBus

```rust
#[test]
fn blocking_sinks_run_before_async() {
    let order = Arc::new(Mutex::new(Vec::new()));

    struct OrderSink { name: &'static str, order: Arc<Mutex<Vec<&'static str>>>, priority: SinkPriority }
    impl Sink for OrderSink {
        fn priority(&self) -> SinkPriority { self.priority }
        fn name(&self) -> &str { self.name }
        fn write(&self, _: Record) -> Result<()> {
            self.order.lock().unwrap().push(self.name);
            Ok(())
        }
        fn flush(&self) -> Result<()> { Ok(()) }
    }

    let bus = RecordBus::new(vec![
        Arc::new(OrderSink { name: "async1", order: order.clone(), priority: SinkPriority::Async }),
        Arc::new(OrderSink { name: "blocking1", order: order.clone(), priority: SinkPriority::Blocking }),
        Arc::new(OrderSink { name: "async2", order: order.clone(), priority: SinkPriority::Async }),
        Arc::new(OrderSink { name: "blocking2", order: order.clone(), priority: SinkPriority::Blocking }),
    ]);

    bus.emit(Record::Event(dummy_event()));

    let order = order.lock().unwrap();
    // Both blocking sinks ran before any async sink
    assert_eq!(order[0], "blocking1");
    assert_eq!(order[1], "blocking2");
    assert_eq!(order[2], "async1");
    assert_eq!(order[3], "async2");
}
```

### Accept filtering

```rust
#[test]
fn merkle_sink_rejects_checkpoint_records() {
    let sink = MerkleTreeSink::new(/* ... */);
    assert!(!sink.accept(&Record::Checkpoint { seq: 0, data: vec![] }));
    assert!(sink.accept(&Record::Event(write_event())));
}

#[test]
fn broadcast_sink_rejects_content() {
    let sink = BroadcastSink::new(/* ... */);
    assert!(!sink.accept(&Record::Content { hash: hash(), data: vec![1,2,3] }));
    assert!(sink.accept(&Record::Event(write_event())));
}
```

### CapturePolicy

```rust
#[test]
fn static_rules_override_dynamic() {
    let policy = CapturePolicy::new(vec![
        CaptureRule { paths: vec![glob("**/target/**")], level: CaptureLevel::MetadataOnly },
    ], u64::MAX);

    assert!(matches!(
        policy.level(Path::new("/workspace/target/debug/foo.o"), 1, 100),
        CaptureLevel::MetadataOnly
    ));
}

#[test]
fn rate_limit_degrades_to_metadata() {
    let policy = CapturePolicy::new(vec![], u64::MAX);

    // Write 200MB in burst — should trigger rate limit
    for _ in 0..200 {
        policy.level(Path::new("/workspace/big.bin"), 42, 1024 * 1024);
    }

    assert!(matches!(
        policy.level(Path::new("/workspace/another.bin"), 42, 1024),
        CaptureLevel::MetadataOnly
    ));
}

#[test]
fn budget_degrades_to_metadata() {
    let policy = CapturePolicy::new(vec![], 1000); // 1000 byte budget

    policy.level(Path::new("/workspace/a.txt"), 1, 900);  // 100 remaining
    assert!(matches!(
        policy.level(Path::new("/workspace/b.txt"), 1, 200), // over budget
        CaptureLevel::MetadataOnly
    ));
}
```

### End-to-end with MemorySink

```rust
#[test]
fn content_and_event_both_emitted() {
    let memory = Arc::new(MemorySink::new());
    let bus = RecordBus::new(vec![memory.clone()]);

    let hash = emit_content_from_bytes(&bus, b"hello world");
    bus.emit(Record::Event(Event::new_write("/workspace/test.txt", None, Some(hash.clone()), 11)));

    let records = memory.drain();
    assert_eq!(records.len(), 2);
    assert!(matches!(&records[0], Record::Content { data, .. } if data == b"hello world"));
    assert!(matches!(&records[1], Record::Event(e) if e.payload.as_write().unwrap().after_hash == Some(hash)));
}
```

## Migration Checklist

1. [ ] Create `crates/argus/src/pipeline/` module with `record.rs`, `sink.rs`, `bus.rs`, `content.rs`, `capture_policy.rs`
2. [ ] Create `crates/argus/src/pipeline/sinks/` with all six sink implementations + MemorySink
3. [ ] Add `put_with_hash(&self, hash: &ContentHash, data: &[u8])` to LocalCas (write without re-hashing — we already computed it)
4. [ ] Add `is_mutating(&self) -> bool` to EventPayload (match on Write/Rename/Unlink/Mkdir/Rmdir/Truncate/Link/Symlink/InitialFile)
5. [ ] Update TracerLoop: remove `cas` and `event_tx` fields, add `bus` and `capture_policy`
6. [ ] Update `content_capture.rs`: replace all `cas.put()` calls with `emit_content_from_tracee` / `try_capture_write` / `try_capture_file`
7. [ ] Update all handlers in `io_ops.rs`, `metadata_ops.rs`: use new content capture functions, emit events via `self.bus.emit(Record::Event(...))`
8. [ ] Update `trace_loop.rs`: emit via bus, remove direct event_tx usage
9. [ ] Update TLS watcher: replace `cas` and `event_tx` with `bus`, update `flow_watcher.rs` and `keylog.rs`
10. [ ] Update `main.rs`: wire up all sinks, create RecordBus, pass to tracer and TLS watcher
11. [ ] Delete `storage/pipeline.rs` and `storage/pipeline_sink.rs`
12. [ ] Remove old `event_writer` thread from main.rs
13. [ ] Update API server: use broadcast receiver for WebSocket, LocalCas for content reads
14. [ ] Add `capture` section to supervisor.yaml config parsing
15. [ ] Write tests: bus ordering, accept filtering, capture policy, end-to-end with MemorySink
16. [ ] Run existing integration tests — verify event output unchanged (same JSONL format, same content hashes)
17. [ ] Verify: CAS objects now appear in S3 (the original bug)
18. [ ] Verify: checkpoints are created periodically and uploaded
19. [ ] Verify: large file writes use chunked streaming (check peak memory)
