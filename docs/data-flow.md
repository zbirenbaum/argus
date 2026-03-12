# Argus Data Flow Reference

## Summary of Broken Paths

| Data type | Local storage | S3 upload | Status |
|-|-|-|-|
| Event segments (JSONL) | `/data/events/{seq}.jsonl` | `events/{agent_id}/{seq}.jsonl` | WORKING |
| Digest cache snapshot | `/data/digest-cache.bin` | `meta/{agent_id}/digest-cache-latest.bin` | WORKING |
| CAS objects (file content) | `/data/cas/{hash[0:2]}/{hash[2:]}` | `cas/{prefix}/{suffix}` | BROKEN: written locally, never uploaded |
| Checkpoints | (never created) | `checkpoints/{agent_id}/{seq}.bin` | BROKEN: infrastructure exists, never called |
| HTTP flow bodies | stored in CAS | (via CAS path) | BROKEN: CAS not uploaded |
| TLS keylog content | stored in CAS | (via CAS path) | BROKEN: CAS not uploaded |

---

## 1. Proxy / TLS Capture

### Startup chain
```
main.rs:66  include_str!("scripts/argus_addon.py") → write to {data_dir}/argus_addon.py
main.rs:71  net::generate_ca(ca_dir) → ca_paths
main.rs:95  net::start_mitmdump_with_flow_capture(ca_paths, port, addon, upstream, mode)
            → net/mitmdump.rs:133 start_mitmdump() → spawns `mitmdump` child process
            → returns MitmdumpHandle { child, flow_output: Some("{data_dir}/flows.jsonl") }
```

### Flow data path
```
mitmdump process
  → addon script writes JSON lines to {data_dir}/flows.jsonl
  → tls_watcher.rs:30 spawn() creates thread polling every 200ms
  → tls_watcher.rs:127 poll_flows()
    → net/flow_watcher.rs:58 FlowWatcher::process_new_flows(cas, pid)
      → reads new lines from flows.jsonl (seeks to last offset)
      → parse_flow_line() → process_flow()
      → store_headers(cas, headers) → cas.put(json_bytes) → content_hash
      → store_body(cas, base64_body) → cas.put(decoded_bytes) → content_hash
      → returns Vec<FlowEvents> with HttpRequest/HttpResponse payloads
  → tls_watcher.rs emits events via event_tx channel
  → DESTINATION: event channel only. CAS content stays local.
```

### SSLKEYLOGFILE path
```
Agent process writes to SSLKEYLOGFILE={data_dir}/tls/sslkeylog.txt
  → tls_watcher.rs:71 poll_keylog()
    → net/keylog.rs:41 KeylogWatcher::process_new_lines(cas, pid, fd)
      → reads new lines, parses label+client_random+secret
      → cas.put(line_bytes) → keylog_line_hash
      → returns Vec<TlsKeys> events
  → tls_watcher.rs emits TlsKeys events via event_tx
  → DESTINATION: event channel only. CAS content stays local.
```

### Network event types emitted
| Event | Source | Content hashes |
|-|-|-|
| `HttpRequest` | flow_watcher | `headers_hash`, `body_hash` |
| `HttpResponse` | flow_watcher | `headers_hash`, `body_hash` |
| `TlsKeys` | keylog watcher | `keylog_line_hash` |
| `Connect` | net_ops handler | (none, just sockaddr) |
| `Socket` | net_ops handler | (none) |
| `Accept` | net_ops handler | (none) |

### Files
| File | Role |
|-|-|
| `scripts/argus_addon.py` | mitmdump addon, writes flows.jsonl |
| `crates/argus/src/net/mitmdump.rs` | `start_mitmdump()`, `MitmdumpHandle` |
| `crates/argus/src/net/flow_watcher.rs` | `FlowWatcher`, parses flows, stores in CAS |
| `crates/argus/src/net/keylog.rs` | `KeylogWatcher`, parses SSLKEYLOGFILE |
| `crates/argus/src/net/env.rs` | `agent_env_vars()` sets proxy/cert env vars |
| `crates/supervisor/src/tls_watcher.rs` | Background thread polling both watchers |
| `crates/argus/src/tracer/handlers/net_ops.rs` | ptrace handlers for socket/connect/accept |

---

## 2. CAS (Content-Addressable Storage)

### LocalCas API
```
crates/argus/src/cas/store.rs

LocalCas::new(root: PathBuf) → Result<Self>
  .put(data: &[u8]) → Result<ContentHash>        // SHA-256, write to disk
  .get(hash: &ContentHash) → Result<Vec<u8>>      // read from disk
  .exists(hash: &ContentHash) → bool
  .delete(hash: &ContentHash) → Result<()>
  .object_path(hash: &ContentHash) → PathBuf      // {root}/{hash[0:2]}/{hash[2:]}
  .stats() → &CasStats
  .detailed_stats() → CasDetailedStats
```

### LocalCas instances (production)
| Location | Variable | Purpose |
|-|-|-|
| `main.rs:116` | `cas` | Tracer content capture |
| `main.rs:121` | `api_cas` | API server (read-only, same dir) |
| `main.rs:137` | `tls_cas` | TLS watcher (keylog + flow bodies) |
| `pipeline.rs:52` | `self.cas` | Pipeline's own CAS (same dir, unused path) |

All four point to `/data/cas/` — safe because CAS is append-only + content-addressed.

### Content capture (tracer → CAS)
```
crates/argus/src/tracer/content_capture.rs

capture_write_buffer(cas, pid, addr, len)
  → reads tracee memory via process_vm_readv
  → cas.put(data) → ContentHash
  → returns hash as String

capture_iovec_buffer(cas, pid, iov_addr, iovcnt, total_len)
  → reads scatter-gather iovec from tracee
  → cas.put(concatenated) → ContentHash

try_capture_flat(cas, pid, addr, len) → Option<String>   // logs errors
try_capture_iovec(cas, pid, iov, cnt, len) → Option<String>
```

Called from `io_ops.rs` handlers for read/write/readv/writev on tracked files.

### CAS → S3 (BROKEN)
```
StoragePipeline::store_content(data: &[u8]) → Result<ContentHash>
  → cas.put(data) → hash
  → local_buffer.track(path, size)
  → if !digest_cache.contains(hash):
      upload_pool.submit(UploadJob::CasObject { hash, data })

WHO CALLS store_content(): NOBODY in production.
  - Tracer calls cas.put() directly, bypassing the pipeline entirely.
  - TLS watcher calls cas.put() directly.
  - store_content() only appears in test code.
```

---

## 3. Checkpoints / Snapshots

### Infrastructure (exists but unwired)
```
crates/argus/src/snapshot/
  checkpoint.rs  — serialize_checkpoint(tree, cas) → Vec<u8>
                   deserialize_checkpoint(data) → MerkleTree
                   checkpoint_s3_key(agent_id, seq) → String
  tree.rs        — MerkleTree: in-memory filesystem tree
  restore.rs     — restore_full(), restore_selective(), restore_from_hash()
  diff.rs        — DiffEntry, DiffKind, diff two trees
```

### Upload job (defined, never instantiated)
```
crates/argus/src/storage/upload_job.rs

UploadJob::Checkpoint { agent_id: String, seq: u64, data: Vec<u8> }
  → S3 key: checkpoints/{agent_id}/{seq}.bin

Created by: NOBODY in production. Only in upload_pool_tests.rs.
```

### Snapshot events (defined, some emitted)
| Event | Emitted by | Status |
|-|-|-|
| `InitialFile` | `trace_loop.rs` initial state capture | Working (emitted at startup) |
| `InitialState` | `trace_loop.rs` after initial scan | Working |
| `Checkpoint` | (nobody) | Never emitted |
| `MmapWarning` | (nobody currently) | Never emitted |

### What's missing
- No periodic checkpoint creation loop
- No code calls `serialize_checkpoint()`
- No code submits `UploadJob::Checkpoint`
- MerkleTree is built during initial state but never serialized or uploaded

---

## 4. Syscall Events (ptrace)

### Main loop
```
crates/argus/src/tracer/trace_loop.rs

TracerLoop::run(initial_pid, sync_pipe_w)
  → ptrace::seize(pid, PTRACE_O_TRACEFORK|TRACEVFORK|TRACECLONE|TRACEEXEC|TRACEEXIT|TRACESECCOMP)
  → write(sync_pipe_w, [1]) — unblock child
  → capture_initial_state() — walks /proc/{pid}, emits InitialFile events
  → wait_loop()
    → loop { waitpid(-1) → handle_wait_status(pid, status) }
      → PTRACE_EVENT_SECCOMP → handle_seccomp_stop(pid)
      → PTRACE_EVENT_FORK/VFORK/CLONE → handle_fork()
      → PTRACE_EVENT_EXEC → handle_program_replace()
      → PTRACE_EVENT_EXIT → handle_exit_event()
      → WIFSTOPPED(SIGTRAP) → handle_seccomp_stop() (fallback)
      → WIFEXITED/WIFSIGNALED → handle_process_exit()
```

### Syscall handler dispatch
```
crates/argus/src/tracer/handlers/mod.rs

handle_seccomp_stop(pid)
  → reads syscall number from registers
  → dispatches to handler by syscall number:

  file_ops.rs:
    openat → handle_open()      // tracks fd in FdTable, no event
    close  → handle_close()     // removes fd, no event
    dup/dup2/dup3 → handle_dup() // clones fd, no event
    fcntl  → handle_fcntl()     // F_DUPFD tracking

  io_ops.rs:
    read/pread64/readv     → handle_read()   // emits Read/PipeData/PtyData/Stdio
    write/pwrite64/writev  → handle_write()  // emits Write/PipeData/PtyData/Stdio
    pipe/pipe2             → handle_pipe()   // emits PipeCreate
    lseek                  → handle_lseek()  // updates fd offset
    ioctl                  → handle_ioctl()  // TIOCSWINSZ tracking

  metadata_ops.rs:
    renameat2  → handle_rename()    // emits Rename
    unlinkat   → handle_unlink()    // emits Unlink
    mkdirat    → handle_mkdir()     // emits Mkdir
    unlinkat(AT_REMOVEDIR) → handle_rmdir() // emits Rmdir
    fchmodat   → handle_chmod()     // emits Chmod
    ftruncate  → handle_truncate()  // emits Truncate
    linkat     → handle_link()      // emits Link
    fchownat   → handle_chown()     // (no event, just tracks)
    symlinkat  → handle_symlink()   // emits Symlink

  net_ops.rs:
    socket   → handle_socket()    // emits Socket
    connect  → handle_connect()   // emits Connect (+ transparent proxy rewrite)
    accept4  → handle_accept()    // emits Accept
```

### Events with content hashes
| Event | Hash fields | Source |
|-|-|-|
| `Read` | `content_hash` | `content_capture::try_capture_flat/iovec` |
| `Write` | `before_hash`, `after_hash` | `content_capture::try_capture_flat/iovec` |
| `Truncate` | `before_hash`, `after_hash` | content capture |
| `Unlink` | `content_hash` | read before delete |
| `PipeData` | `content_hash` | content capture |
| `PtyData` | `content_hash` | content capture |
| `Stdio` | `content_hash` | content capture |
| `InitialFile` | `content_hash` | initial state scan |

### Event flow
```
Handler → tracer.emit(payload)
  → Event::new(seq_gen, agent_id, payload)  // envelope.rs
  → event_tx.send(event)                    // mpsc channel
  → event_writer thread receives
    → for each sink in sinks:
        sink.write(event)
    → on timeout (100ms):
        for each sink: sink.flush()
    → periodically:
        for each sink: sink.drain_confirmations()
```

---

## 5. S3 Upload Path

### UploadJob variants
```
crates/argus/src/storage/upload_job.rs

CasObject { hash: ContentHash, data: Vec<u8> }
  → key: cas/{hash[0:2]}/{hash[2:]}
  → BROKEN: never submitted in production

EventSegment { agent_id: String, seq: u64, data: Vec<u8> }
  → key: events/{agent_id}/{seq}.jsonl
  → WORKING: submitted by EventLog::rotate() and EventLog::finalize()

Checkpoint { agent_id: String, seq: u64, data: Vec<u8> }
  → key: checkpoints/{agent_id}/{seq}.bin
  → BROKEN: never submitted in production

DigestCacheSnapshot { agent_id: String, data: Vec<u8> }
  → key: meta/{agent_id}/digest-cache-latest.bin
  → WORKING: submitted by StoragePipeline::save_digest_cache()
```

### UploadPool
```
crates/argus/src/storage/upload_pool.rs

UploadPool::new(store, config, capacity) — spawns tokio worker tasks
  .submit(job: UploadJob) → Result<()>   — enqueues to channel
  .confirmations() → &Receiver<UploadConfirmation>
  .stats() → &UploadStats
  .shutdown() → async, drains queue, returns stats
```

### StoragePipeline — all public methods
```
crates/argus/src/storage/pipeline.rs

::new(config, agent_id, store, durability)  — wires CAS+EventLog+UploadPool+DigestCache+LocalBuffer
.store_content(data) → ContentHash          — CAS put + enqueue upload (UNUSED)
.append_event(event)                        — EventLog append
.rotate_now()                               — force segment rotation + upload
.process_confirmations() → usize            — drain upload confirmations
.save_digest_cache()                        — serialize + enqueue snapshot upload
.flush()                                    — fsync event log
.shutdown() → async UploadStatsSnapshot     — finalize all + drain pool
.upload_stats() → UploadStatsSnapshot
.digest_cache_len() → usize
.local_buffer_bytes() → u64
.read_content(hash) → Vec<u8>
.current_segment_seq() → u64
```

### What calls what (production)
| Method | Called by | Location |
|-|-|-|
| `store_content()` | **NOBODY** | — |
| `append_event()` | `PipelineSink::write()` | `pipeline_sink.rs` |
| `rotate_now()` | `PipelineSink::flush()` | `pipeline_sink.rs` |
| `process_confirmations()` | `PipelineSink::flush/drain` | `pipeline_sink.rs` |
| `flush()` | (not called directly) | — |
| `shutdown()` | `shutdown_pipeline_sinks()` | `main.rs` |
| `save_digest_cache()` | `shutdown()` internally | `pipeline.rs` |

---

## 6. Event Envelope

### Event struct
```
crates/argus/src/events/envelope.rs

pub struct Event {
    pub seq: u64,                              // monotonic from SequenceGenerator
    pub ts_monotonic: u64,                     // nanos, CLOCK_MONOTONIC
    pub ts_wall: String,                       // RFC 3339 with nanoseconds
    pub agent_id: String,
    pub vclock: Option<HashMap<String, u64>>,  // optional vector clock
    #[serde(flatten)]
    pub payload: EventPayload,                 // type tag + fields inline
}
```

### SequenceGenerator
```
SequenceGenerator::default()      — starts at 0 (tracer)
SequenceGenerator::new(1_000_000) — starts at 1M (TLS watcher)
.next_seq() → u64                 — AtomicU64 fetch_add, Relaxed ordering
```

### EventPayload — all 34 variants
```
Process:    Exec, Fork, Exit
File I/O:   Read, Write
Metadata:   Rename, Unlink, Mkdir, Rmdir, Chmod, Truncate, Link, Symlink
Pipes:      PipeCreate, PipeData, PipeClose
PTY:        PtyCreate, PtyData
Stdio:      Stdio, FdRedirect
Network:    Socket, Connect, Accept, TlsKeys, HttpRequest, HttpResponse
Control:    AgentStart, AgentPause, AgentResume,
            PendingApproval, ApprovalGranted, ApprovalDenied,
            Blocked, RulesUpdated
Snapshot:   InitialFile, InitialState, Checkpoint, MmapWarning
```
