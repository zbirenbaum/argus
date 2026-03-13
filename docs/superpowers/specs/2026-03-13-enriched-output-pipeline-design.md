# Enriched Output Pipeline

**Date:** 2026-03-13
**Status:** Draft

## Problem

The supervisor currently mixes internal durability infrastructure (CAS, digest cache, S3 uploads) with user-facing event outputs (stdout, event log) in a single RecordBus. Content records and event records share the same fan-out path. Raw bytes captured from tracee memory are discarded after hashing, making event enrichment impossible. The result: events contain only opaque content hashes — unreadable without CAS lookups.

## Design Principles

1. **Do what only we can do, do it incredibly well.** The supervisor owns ptrace, capture, enrichment, and durability. Routing events to N destinations is Vector's job.
2. **Capture everything, enrich everything.** Never discard data the supervisor has access to. Carry raw bytes through the pipeline. Future config controls opt-outs and size limits.
3. **Separate internal infrastructure from user-facing outputs.** CAS and durability are internal plumbing. Outputs are transports for the enriched event stream.
4. **Outputs are dumb pipes.** No transformation, no filtering, no fan-out logic. Just deliver JSONL.
5. **Recommend Vector for production routing.** Deploy examples ship `vector.yaml` alongside `supervisor.yaml`.

## Architecture

### Current (broken)

```
PtraceThread
         │ RawSyscallStop
         ▼
    Runner loop
         │
         ▼
    ClassifyStage ── resolve paths/args from ptrace regs
         │ ClassifiedEvent
         ▼
    CheckRulesStage ── block/pause evaluation
         │
         ▼
    ApprovalStage ── wait for operator if paused
         │
         ▼
    CaptureStage ── read tracee memory, hash bytes
         │    └── emits Content/Manifest records to RecordBus (mid-pipeline)
         │        CapturedContent only keeps hashes, raw bytes discarded
         ▼
    TreeStage ── update Merkle tree
         │
         ▼
    StampStage ── assign seq/timestamps, map to EventPayload
         │          emits Event record to RecordBus
         ▼
    Resume directive sent to ptrace thread
    (only after all Blocking sinks finished)

                    RecordBus (single bus, mixed data)
                    ┌────────────────┬─────────────────┐
                    ▼                ▼                  ▼
              Blocking sinks    Blocking sinks     Async sinks
              (Content)         (Events)           (both)
              ├─ LocalCasSink   ├─ StdoutSink      ├─ RemoteCasSink
              │                 ├─ EventLogSink     ├─ IndexSink
              │                 │                   ├─ BroadcastSink
```

Problems:
- Content records (raw bytes for CAS) and Event records (structured events) share one bus. Every sink filters by type.
- CaptureStage emits to the bus mid-pipeline, before stamp runs. Content and events arrive at sinks interleaved.
- Raw bytes discarded after hashing — enrichment impossible.
- CAS (internal durability) and stdout (user-facing output) are peers in the same fan-out.

### Optimal

```
PtraceThread
         │ RawSyscallStop
         ▼
    Classify ── resolve paths, args, addresses
         │
         ▼
    Rules ── block / pause
         │
         ▼
    Capture ── read tracee memory, keep raw bytes AND hash
         │         │
         │         ▼
         │    ┌─────────────────────────┐
         │    │  Durability layer       │  ← internal, not an "output"
         │    │  (blocking, pre-resume) │
         │    │                         │
         │    │  Local CAS: persist     │
         │    │  content by hash        │
         │    │                         │
         │    │  Remote CAS: async S3   │
         │    │  upload of blobs        │
         │    │                         │
         │    │  Digest cache: dedup    │
         │    └─────────────────────────┘
         │
         ▼
    Stamp ── build enriched Event from CapturedContent
         │     (inline bytes, paths, args per enrich config)
         │
         ▼
    Redact ── scrub sensitive data from inline content
         │     (path exclusions, field exclusions, pattern matching)
         │
         ▼
    Enriched Event (complete, self-contained, scrubbed JSON)
         │
         │  ← tracee resumed after this point
         │
         ▼
    ┌──────────────────────────┐
    │  Outputs                 │  ← user-configurable transports
    │  (list, each gets every  │
    │   event, same JSONL)     │
    │                          │
    │  - stdout                │
    │  - file (rotated)        │
    │  - unix socket           │
    │  - http POST             │
    └──────────────────────────┘
         │
         ▼  (optional, external)
    Vector / Fluent Bit / etc.
    routes to S3, ES, Datadog...
```

## Data Model Changes

### CapturedContent — carry raw bytes

Current: hashes only, bytes discarded after hashing.

New: raw bytes preserved alongside hashes. Both are available to stamp stage.

```rust
pub enum CapturedContent {
    None,
    FileWrite {
        before_hash: Option<ContentHash>,
        after_hash: Option<ContentHash>,
        data: Option<Vec<u8>>,          // NEW: the written bytes
        size: usize,
    },
    FileRead {
        content_hash: Option<ContentHash>,
        data: Option<Vec<u8>>,          // NEW: the read bytes
        size: usize,
    },
    StreamData {
        content_hash: Option<ContentHash>,
        data: Option<Vec<u8>>,          // NEW: stdio/pipe/pty bytes
        size: usize,
    },
    FileDelete {
        content_hash: Option<ContentHash>,
        data: Option<Vec<u8>>,          // NEW: file content before deletion
    },
    FileTruncate {                      // NEW variant
        before_hash: Option<ContentHash>,
        after_hash: Option<ContentHash>,
        before_data: Option<Vec<u8>>,
        after_data: Option<Vec<u8>>,
    },
}
```

### Event structs — inline content fields

Each event type gains optional inline content. When enrichment is on (MVP: always), stamp populates these from CapturedContent.

```rust
// io.rs — Stdio
pub struct Stdio {
    pub pid: u32,
    pub subtype: StdioSubtype,
    pub content_hash: Option<String>,
    pub size: u64,
    pub pipe_inode: Option<u64>,
    pub dest_pid: Option<u32>,
    pub source_pid: Option<u32>,
    pub text: Option<String>,              // NEW: inline message (lossy UTF-8)
}

// io.rs — PipeData
pub struct PipeData {
    pub pid: u32,
    pub inode: u64,
    pub direction: PipeDirection,
    pub content_hash: Option<String>,
    pub size: u64,
    pub dest_pids: Vec<u32>,
    pub text: Option<String>,              // NEW: inline data (lossy UTF-8)
}

// io.rs — PtyData
pub struct PtyData {
    pub pid: u32,
    pub subtype: PtySubtype,
    pub content_hash: Option<String>,
    pub size: u64,
    pub slave_path: String,
    pub text: Option<String>,              // NEW: inline data (lossy UTF-8)
}

// file.rs — Write
pub struct Write {
    pub pid: u32,
    pub path: String,
    pub fd: i32,
    pub offset: u64,
    pub size: u64,
    pub before_hash: Option<String>,
    pub after_hash: Option<String>,
    pub tree_hash: Option<String>,
    pub data: Option<String>,              // NEW: inline written content
}

// file.rs — Read
pub struct Read {
    pub pid: u32,
    pub path: String,
    pub fd: i32,
    pub offset: u64,
    pub size: u64,
    pub content_hash: Option<String>,
    pub data: Option<String>,              // NEW: inline read content
}

// file.rs — Unlink
pub struct Unlink {
    pub pid: u32,
    pub path: String,
    pub content_hash: Option<String>,
    pub tree_hash: Option<String>,
    pub data: Option<String>,              // NEW: file content before deletion
}

// file.rs — Truncate
pub struct Truncate {
    pub pid: u32,
    pub path: String,
    pub old_size: u64,
    pub new_size: u64,
    pub before_hash: Option<String>,
    pub after_hash: Option<String>,
    pub tree_hash: Option<String>,
    pub before_data: Option<String>,       // NEW: content before truncate
    pub after_data: Option<String>,        // NEW: content after truncate
}

// network.rs — HttpRequest
pub struct HttpRequest {
    pub pid: u32,
    pub method: String,
    pub url: String,
    pub headers_hash: Option<String>,
    pub body_hash: Option<String>,
    pub headers: Option<String>,           // NEW: inline headers
    pub body: Option<String>,              // NEW: inline request body
}

// network.rs — HttpResponse
pub struct HttpResponse {
    pub pid: u32,
    pub status: u16,
    pub headers_hash: Option<String>,
    pub body_hash: Option<String>,
    pub headers: Option<String>,           // NEW: inline headers
    pub body: Option<String>,              // NEW: inline response body
}
```

### Enrichment Catalog

Every piece of data the supervisor has access to, documented for future per-category opt-out and size-based handling.

| Category | Source | Data Available | MVP Behavior |
|-|-|-|-|
| Stdio text | ptrace read_memory(buf_addr, len) | Raw bytes from stdin/stdout/stderr | Inline as lossy UTF-8 |
| Pipe data | ptrace read_memory(buf_addr, len) | Raw bytes flowing through pipes | Inline as lossy UTF-8 |
| PTY data | ptrace read_memory(buf_addr, len) | Raw bytes through pseudo-terminals | Inline as lossy UTF-8 |
| File write content | ptrace read_memory(buf_addr, len) | Bytes being written to file | Inline as lossy UTF-8 |
| File read content | ptrace read_memory(buf_addr, len) | Bytes read from file | Inline as lossy UTF-8 |
| File delete content | read_file(path) before unlink | Full file content before deletion | Inline as lossy UTF-8 |
| Truncate before/after | read_file + read_memory | Content before and after truncation | Inline as lossy UTF-8 |
| Syscall args (paths) | ptrace regs → read_string | Filenames, dirnames, link targets | Already in event struct fields |
| Exec argv/envp | ptrace read_memory | Command line args and environment | Already in event struct fields |
| Network addresses | ptrace read_memory(sockaddr) | IP:port for connect/accept | Already in event struct fields |
| HTTP request body | MITM proxy capture | Full request payload | Inline as string |
| HTTP response body | MITM proxy capture | Full response payload | Inline as string |
| HTTP headers | MITM proxy capture | Request/response headers | Inline as string |
| TLS key material | SSLKEYLOGFILE | Key log lines | Already captured (hash only is correct) |

### Binary and large content handling

Inline content fields use `Option<String>`. Not all captured data is valid UTF-8 (binary files, compressed data, images). Rules:

1. **UTF-8 validity check**: if the captured bytes are valid UTF-8, inline as-is. If not, base64-encode and set `encoding: "base64"` on the event. Events carry an `encoding` field when the content is not plain UTF-8.
2. **Size cap**: inline content is capped at `max_inline_bytes` (default 256KB for MVP). Content larger than the cap is not inlined — the event retains only the content hash. This prevents a 100MB file write from living in memory through the pipeline.
3. **Affected structs**: every event type with inline content gains an optional `encoding` field:

```rust
pub struct Stdio {
    // ...
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,  // "base64" when binary, absent when UTF-8
}
```

The capture stage enforces the size cap when retaining bytes in CapturedContent. Bytes beyond the cap are hashed and emitted to CAS but not carried through to stamp.

### Memory pressure

CapturedContent carries `Option<Vec<u8>>` through the pipeline. To bound memory:

- The `max_inline_bytes` cap (256KB default) limits per-event memory. A 100MB file write hashes the full content for CAS but only retains up to 256KB for enrichment.
- Events are stamped and emitted to outputs immediately — bytes do not accumulate across events.
- The pipeline processes one event at a time per source (ptrace loop is serial). Peak memory for enrichment is bounded by `max_inline_bytes × active_pipelines` (3 pipelines × 256KB = 768KB worst case).

### Configuration defaults

**Every config section is optional.** A bare `supervisor.yaml` with no `enrich`, `redact`, or `outputs` keys works out of the box — the supervisor enriches everything, redacts common secrets, and emits JSONL to stdout.

```yaml
# This is the ENTIRE default config — you get all of this by writing NOTHING:
enrich:                               # ← section optional, all defaults below
  enabled: true
  max_inline_bytes: 262144            # 256KB
  stdio_text:    { enabled: true }
  pipe_data:     { enabled: true }
  pty_data:      { enabled: true }
  file_content:  { enabled: true }
  delete_content:{ enabled: true }
  truncate_content:{ enabled: true }
  http_headers:  { enabled: true }
  http_bodies:   { enabled: true }
  exec_envp:     { enabled: true }

redact:                               # ← section optional, all defaults below
  exclude_paths:                      # Tier 1: path deny-list
    - "**/*.env"
    - "**/*.pem"
    - "**/*.key"
    - "**/credentials.json"
    - "**/.ssh/**"
  drop_fields:                        # Tier 2: field-level drop
    - "http_request.headers.authorization"
    - "http_request.headers.cookie"
    - "http_request.headers.x-api-key"
  scan_fields:                        # Tier 3: regex-eligible fields
    - "http_request.headers"
    - "http_request.body"
    - "http_response.headers"
    - "http_response.body"
    - "stdio.text"
    - "exec.envp"
  builtins:
    api_keys: true
    credentials: true
    private_keys: true
    aws_keys: true
  patterns: []                        # no custom patterns by default

outputs:                              # ← section optional, default: stdout
  - type: stdout
```

**Override only what you need.** Unspecified fields keep their defaults:

```yaml
# Only change: disable file content enrichment, add a custom redaction pattern
enrich:
  file_content:
    enabled: false

redact:
  patterns:
    - name: internal_id
      regex: "INT-[A-Z0-9]{12}"
      replacement: "[REDACTED]"
```

All other `enrich` categories remain enabled, all default `redact` rules still apply, outputs still go to stdout.

### Enrichment config

Controls what captured data gets inlined into events. All categories default to enabled. Operators can disable categories or set per-category size limits.

```yaml
enrich:
  max_inline_bytes: 262144          # global default: 256KB

  stdio_text:
    enabled: true
    max_bytes: 262144               # override per category

  pipe_data:
    enabled: true

  pty_data:
    enabled: true

  file_content:
    enabled: true
    max_bytes: 1048576              # file content can be larger

  delete_content:
    enabled: true

  truncate_content:
    enabled: true

  http_headers:
    enabled: true

  http_bodies:
    enabled: true
    max_bytes: 4194304              # HTTP bodies up to 4MB

  exec_envp:
    enabled: true                   # environment variables on exec
```

When a category is disabled, the event retains the content hash but the inline field is `null`. When content exceeds `max_bytes` for its category (falling back to global `max_inline_bytes`), same behavior — hash only, no inline.

Shorthand for disabling all enrichment:

```yaml
enrich:
  enabled: false                    # hash-only mode, no inline content
```

### Redaction

The supervisor scrubs sensitive data from inline content before it reaches outputs. This is not optional — redaction runs regardless of whether Vector is downstream. Sensitive data should never leave the supervisor process.

Redaction is expensive (regex). The design uses a **three-tier pipeline** so that cheap operations eliminate work for expensive ones:

```
Tier 1: Path deny-list       O(1) glob match → strip all inline content (hash retained)
Tier 2: Field-level drop     O(1) set lookup → null entire fields
Tier 3: Value-level scrub    Regex scan → replace matches in remaining fields
```

Only fields that survive Tiers 1-2 reach the regex engine. Most events (file I/O on non-sensitive paths, stdio) skip regex entirely.

#### Which fields get regex-scanned

Not every field warrants regex. The `scan_fields` config controls which event fields are eligible for Tier 3 value scrubbing. Fields not in this list pass through untouched after Tiers 1-2.

Default scan targets (fields likely to contain PII/secrets):
- `http_request.headers`, `http_request.body` — auth tokens, API keys in headers/bodies
- `http_response.headers`, `http_response.body` — leaked credentials in responses
- `stdio.text` — agent terminal output may echo secrets
- `exec.envp` — environment variables often carry secrets

Fields that default to **no scan** (low PII probability, high volume):
- `file.data` — bulk file content; path deny-list handles `.env`/`.pem`/`.key` files
- `pipe.text`, `pty.text` — internal IPC, rarely contains secrets directly

Operators can override both directions — add fields to scan or remove defaults.

```yaml
redact:
  # Tier 1: Path deny-list — strip all inline content for matching paths
  exclude_paths:
    - "**/*.env"
    - "**/*.pem"
    - "**/*.key"
    - "**/credentials.json"
    - "**/.ssh/**"

  # Tier 2: Field-level drop — null entire fields unconditionally
  drop_fields:
    - "http_request.headers.authorization"
    - "http_request.headers.cookie"
    - "http_request.headers.x-api-key"
    # - "exec.envp"                 # uncomment to drop all env vars

  # Tier 3: Value-level scrub — regex scan on eligible fields only
  scan_fields:                       # which fields get regex treatment
    - "http_request.headers"
    - "http_request.body"
    - "http_response.headers"
    - "http_response.body"
    - "stdio.text"
    - "exec.envp"
    # - "file.data"                  # uncomment to also scan file content

  # Built-in patterns (enabled by default, applied in Tier 3)
  builtins:
    api_keys: true                  # sk-ant-*, sk-*, Bearer *, x-api-key values
    credentials: true               # password=, secret=, token= in query strings/headers
    private_keys: true              # -----BEGIN * PRIVATE KEY-----
    aws_keys: true                  # AKIA* access key IDs

  # Custom patterns (applied in Tier 3 alongside builtins)
  patterns:
    - name: github_token
      regex: "ghp_[A-Za-z0-9_]{36}"
      replacement: "[REDACTED]"
    - name: ssn
      regex: "\\d{3}-\\d{2}-\\d{4}"
      replacement: "***-**-\\1"     # mask all but last 4
    - name: card_number
      regex: "\\b\\d{4}[- ]?\\d{4}[- ]?\\d{4}[- ]?(\\d{4})\\b"
      replacement: "****-****-****-\\1"
```

#### Redaction pipeline execution order

1. **Tier 1 — Path exclusion** (O(1) glob match): if the event's file path matches `exclude_paths`, all inline content fields are set to `None`. Hash retained. No further processing.

2. **Tier 2 — Field drop** (O(1) set lookup): fields listed in `drop_fields` are set to `None`. Supports dotted paths for sub-field targeting (e.g., `http_request.headers.authorization` drops only the authorization header, not all headers). Plain field names (e.g., `http_request.headers`) drop the entire field.

3. **Tier 3 — Value scrub** (regex): only fields listed in `scan_fields` that are still non-null after Tiers 1-2 are regex-scanned. Built-in patterns run first, then custom patterns. Each match is replaced with the pattern's `replacement` string (default: `[REDACTED]`).

#### Built-in patterns

| Name | Regex | Targets |
|-|-|-|
| `api_keys` | `sk-ant-[A-Za-z0-9_-]+`, `sk-[A-Za-z0-9_-]{20,}`, `Bearer\s+[A-Za-z0-9_.-]+` | API tokens |
| `credentials` | `(?i)(password\|secret\|token\|api_key)\s*[=:]\s*\S+` | Key-value credentials |
| `private_keys` | `-----BEGIN\s+\S+\s+PRIVATE KEY-----[\s\S]*?-----END\s+\S+\s+PRIVATE KEY-----` | PEM blocks |
| `aws_keys` | `AKIA[A-Z0-9]{16}` | AWS access key IDs |

#### Content hashes are never affected

Hashes reflect the original unredacted content (safely in CAS, access-controlled separately). Redaction only touches inline string fields.

### Redaction audit trail

Every redaction action is logged for auditability. The redaction stage emits structured `tracing` events recording **what was redacted** without logging the redacted value itself.

```rust
// Tier 3 value scrub
tracing::info!(
    name: "redact.scrubbed",
    event_seq = event.seq,
    field = "http_request.headers",
    rule = "api_keys",
    matches = 1,
    "value redaction applied",
);

// Tier 1 path exclusion
tracing::info!(
    name: "redact.path_excluded",
    event_seq = event.seq,
    path = %event_path,
    "inline content stripped by path exclusion",
);

// Tier 2 field drop
tracing::info!(
    name: "redact.field_dropped",
    event_seq = event.seq,
    field = "http_request.headers.authorization",
    "field dropped by deny list",
);
```

Fields logged per audit entry:
- `event_seq`: sequence number of the event
- `field`: which field was affected
- `rule`: pattern name (Tier 3 only)
- `matches`: number of replacements (Tier 3 only)

This audit trail is invaluable for:
- **Debugging**: "Why is this field empty?" → check redaction logs
- **Compliance**: prove that sensitive data was caught and scrubbed
- **Tuning**: identify false positives from overly broad patterns

## Output Configuration

### Config schema

```yaml
outputs:
  - type: stdout                    # JSONL to stdout

  - type: file
    path: /data/events.jsonl        # JSONL to rotated file
    max_size: 64MB                  # rotate after this size
    max_files: 10                   # keep N rotated files

  - type: unix_socket
    path: /var/run/argus.sock       # JSONL to unix domain socket

  - type: http
    endpoint: http://vector:8080    # POST JSONL to HTTP endpoint
    timeout: 5s
    retry_max: 3
```

### Default (no outputs section in config)

When no `outputs` section is present, the supervisor defaults to:

```yaml
outputs:
  - type: stdout
```

This preserves backward compatibility with the validation test harness.

### Output trait

```rust
pub trait Output: Send {
    fn emit(&mut self, event: &Event) -> anyhow::Result<()>;
    fn flush(&mut self) -> anyhow::Result<()>;
    fn shutdown(&mut self) -> anyhow::Result<()> { self.flush() }
    fn name(&self) -> &str;
}
```

Outputs are simpler than the current Sink trait — they only handle Event records, never Content/Manifest/Checkpoint. They serialize to JSON and write to their transport.

### Output failure and backpressure

Outputs must never block the ptrace pipeline. Failure policy:

- **stdout / file**: synchronous write. If the write fails (broken pipe, disk full), log the error and continue. Events are best-effort — the durability guarantee comes from CAS, not outputs.
- **unix_socket**: non-blocking send. If the socket buffer is full (Vector is slow), drop the event and increment a counter. Log periodic warnings ("dropped N events in last 10s").
- **http**: async POST with bounded buffer (default 1000 events). If the buffer is full, drop oldest events. Retries: 3 attempts with 1s/2s/4s backoff, then drop.
- **All outputs**: a failed output never affects other outputs. Each output is independent.

The tracee is never held waiting for an output. If all outputs fail, the supervisor continues tracing — events are lost from outputs but content is safe in CAS.

## Durability Layer (internal)

CAS, digest cache, and remote upload remain internal infrastructure. They are NOT outputs. They are configured under the existing `storage:` and `durability:` config sections.

The capture stage emits content to the durability layer directly (not through a bus). Blocking durability (local CAS write) completes before the tracee resumes. Async durability (S3 upload) proceeds in the background.

```rust
pub struct DurabilityLayer {
    local_cas: LocalCas,
    remote_upload: Option<UploadPool>,
    digest_cache: DigestCache,
}

impl DurabilityLayer {
    /// Persist content, blocking. Returns hash.
    pub fn persist(&mut self, data: &[u8]) -> anyhow::Result<ContentHash> { ... }

    /// Enqueue async upload if remote is configured.
    pub fn upload_async(&self, hash: ContentHash, data: Vec<u8>) { ... }
}
```

## Production Deployment (Vector)

Recommended production setup: supervisor emits to unix socket or file, Vector consumes and routes.

### supervisor.yaml (production)

```yaml
outputs:
  - type: unix_socket
    path: /var/run/argus.sock
  - type: stdout                    # for docker logs
```

### vector.yaml (production, ships alongside supervisor.yaml)

```yaml
sources:
  argus:
    type: unix
    path: /var/run/argus.sock

transforms:
  parsed:
    inputs: ["argus"]
    type: remap
    source: '. = parse_json!(.message)'

sinks:
  s3_events:
    inputs: ["parsed"]
    type: aws_s3
    bucket: my-argus-bucket
    key_prefix: "agents/{{ agent_id }}/events/"
    encoding:
      codec: json
    compression: gzip
    batch:
      timeout_secs: 60
      max_bytes: 10000000

  elasticsearch:
    inputs: ["parsed"]
    type: elasticsearch
    endpoints: ["http://elasticsearch:9200"]
    bulk:
      index: "argus-events-%Y-%m-%d"

  datadog:
    inputs: ["parsed"]
    type: datadog_logs
    default_api_key: "${DATADOG_API_KEY}"
```

## Migration Path

1. Add `data: Option<Vec<u8>>` to CapturedContent variants — capture stage stops discarding bytes
2. Add inline content fields to event structs (Stdio.text, Write.data, HttpRequest.body, etc.)
3. Add `encoding` field to event structs for binary content signaling
4. Add `enrich:` config section with per-category toggles and size limits
5. Stamp stage populates inline fields from CapturedContent per enrich config
6. Add `redact:` config section with builtins, custom patterns, path/field exclusions
7. Implement redaction stage (pattern scrubbing, path exclusion, field exclusion)
8. Add FileTruncate capture (before/after content)
9. Extract durability layer from RecordBus into standalone struct
10. Replace Sink-based output (StdoutSink, EventLogSink) with Output trait
11. Add file, unix_socket, http output implementations
12. Add `outputs:` config section with parsing and validation
13. Remove RecordBus fan-out for events (outputs handle it directly)
14. Ship example `vector.yaml` in deploy/

### What stays the same
- CAS storage (local + remote) — now encapsulated in DurabilityLayer
- Digest cache — unchanged
- Capture policy (ignore/metadata-only/full per path) — unchanged
- Block/pause rules — unchanged
- Validation test harness — stdout output is the default

### What's removed
- RecordBus (replaced by direct durability layer + output list)
- Sink trait (replaced by simpler Output trait)
- EventLogSink (replaced by file output)
- RemoteCasSink as a "sink" (moved into DurabilityLayer)

### What's deferred
- **IndexSink**: secondary indexes (path, pid, type) are a query concern. Deferred to a future design — can be built by indexing the JSONL output externally or via Vector transform + Elasticsearch.
- **BroadcastSink**: WebSocket live tailing. Deferred — can be reimplemented as a standalone output type or as a separate service consuming from the unix socket.
- **Sensitive data scrubbing**: handled by the built-in redaction stage (see Redaction section). The supervisor scrubs before output — sensitive data never leaves the process. Vector can apply additional transforms but the baseline is enforced internally.

## Example Enriched Event Output

```json
{
  "seq": 42,
  "ts_monotonic": 1710323123456789,
  "ts_wall": "2026-03-13T10:05:23.456Z",
  "agent_id": "gke-pool-1-abc-14523",
  "type": "stdio",
  "payload": {
    "pid": 100,
    "subtype": "stdout",
    "text": "Creating /workspace/todo-app/index.js...\n",
    "content_hash": "af3b1c...",
    "size": 42
  }
}

{
  "seq": 87,
  "ts_wall": "2026-03-13T10:05:24.789Z",
  "agent_id": "gke-pool-1-abc-14523",
  "type": "write",
  "payload": {
    "pid": 100,
    "path": "/workspace/todo-app/index.js",
    "fd": 3,
    "offset": 0,
    "size": 1234,
    "after_hash": "c4d2e5...",
    "data": "const express = require('express');\nconst app = express();\n...",
    "tree_hash": "9f8e7d..."
  }
}

{
  "seq": 112,
  "ts_wall": "2026-03-13T10:05:25.012Z",
  "agent_id": "gke-pool-1-abc-14523",
  "type": "http_request",
  "payload": {
    "pid": 100,
    "method": "POST",
    "url": "https://api.anthropic.com/v1/messages",
    "headers": "content-type: application/json\nx-api-key: [REDACTED]",
    "body": "{\"model\":\"claude-haiku-4-5-20251001\",\"messages\":[{\"role\":\"user\",\"content\":\"Build a todo app\"}]}",
    "headers_hash": "a1b2c3...",
    "body_hash": "d4e5f6..."
  }
}
```
