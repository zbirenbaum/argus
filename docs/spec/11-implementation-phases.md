# Implementation Phases

## Phase 1: Tracer + Stdio + Event Envelope (Week 1-2)

**Read first:** `01-supervisor.md`, `02-event-schema.md`, `07-tls-network.md` (env setup only), `06-agent-controls.md` (stub hook only)

**Build:**
- Rust binary: nix (ptrace) + seccomp (BPF filter)
- PTRACE_TRACEME, auto-follow all descendants
- seccomp-bpf: SECCOMP_RET_TRACE for ~55 syscalls, block io_uring_setup
- Full startup sequence from `01-supervisor.md`: config → storage init → digest cache load → TLS CA → mitmdump → agent env → initial snapshot → fork → TRACEME → exec
- Event envelope from day one: seq, ts_monotonic, ts_wall, agent_id, vclock(None)
- Per-process fd table, pipe registry, PTY registry
- Classify write() by fd target: file, stdio, pipe, PTY, network, devnull
- Pause-before-action hook in main loop (stub returning "allow")
- TLS env set before exec; mitmdump started as traced child
- Emit agent_start event
- Output: JSON lines to stdout, full envelope format

**Deliverable:** `./supervisor --agent-id test -- /bin/bash` — complete event stream with dual timestamps, agent_id, stdout/stderr of subprocesses, pipe data flow, PTY traffic, exec events, TLS keylog active.

**Validate with:** Tests 1 (process tracing), 2 (stdio), 4 (pipe topology), 5 (subprocess tree), 6 (escape test), 12 (initial state).

**Not yet:** Content hashes, S3, write locking, Merkle tree, query API.

---

## Phase 2: Content + Storage + Pause/Resume (Week 3-4)

**Read first:** `03-storage.md`, `01-supervisor.md` (write locking section), `06-agent-controls.md` (pause API)

**Build:**
- CAS: SHA-256, local hot buffer
- S3 upload pipeline: tokio async pool, retry with backoff
- Digest cache: HashMap, disk persistence, S3 snapshot + incremental LIST on cold start, periodic snapshot upload
- Content capture: process_vm_readv for write/read buffers
- Per-path write locking from `01-supervisor.md`: lock → before_hash → resume → exit → after_hash → release
- Durability modes: memory / local (default) / remote, per-path config
- Initial state capture: walk watched paths, hash into CAS, commit zero
- Pause/resume API: POST /agent/pause, /resume, GET /status
- Pause-before-action: load rules from config, wire approval endpoints
- Read dedup: skip content if hash matches last captured version
- Stdio/pipe/PTY content stored in CAS by hash
- TLS content: watch keylog file, store in CAS, emit tls_keys; parse mitmdump JSON, emit http_request/http_response
- Event segments: 64MB JSONL, upload to S3 on completion
- Local buffer: bounded LRU, evict only uploaded content

**Deliverable:** Full content capture with before/after. Events streaming to S3. Digest cache working. Pause/resume + approval API functional. TLS bodies captured.

**Validate with:** Tests 3 (file write/read/delete), 7 (write locking), 8 (TLS capture), 9 (pause/resume), 10 (pause-before-action).

**Not yet:** Merkle tree, restore, indexes, query API, stdio reconstruction.

---

## Phase 3: Snapshots + Indexes + Queries (Week 5-6)

**Read first:** `04-snapshots-restore.md`, `05-indexing-queries.md`, `10-api-reference.md`

**Build:**
- Merkle tree: blob/tree/commit in CAS, update on mutating events, tree_hash per event
- Checkpoints: binary, every 1000 events, to S3. Deserialize via CLI.
- On restart: load checkpoint from S3, replay events
- Restore: full (new dir + in-place), selective, undo-last-N. Pull from local/S3.
- Pre-restore snapshot automatic
- Indexes: path, pid, type — append-only, rebuilt on restart
- Query API: GET /events (all filters), GET /file_history
- Stdio reconstruction: GET /stdio, GET /process_tree (with stdio), GET /pipeline
- Real-time: GET /stdio?follow=true (SSE), ws://…/ws/events, ws://…/ws/stdio/{pid}
- Content API: GET /content/{hash}, /text, GET /diff
- Tree API: GET /tree, GET /tree/diff
- Restore API: POST /restore, POST /restore/undo
- GET /connections, GET /storage/status, GET /health
- Cross-agent foundation: GET /agents (scan S3 for agent_start events)

**Deliverable:** Full query API. Point-in-time restore. Indexes for fast lookups. Stdio reconstruction. CLI tools.

**Validate with:** Tests 11 (snapshot and restore), 12 (initial state). Full integration test: trace coding agent building Argus.

**Not yet:** Multi-agent orchestration, Helm chart, Web UI.

---

## Phase 4: Multi-Agent + Orchestration (Week 7-8)

**Read first:** `09-multi-agent.md`, `08-kubernetes.md`

**Build:**
- Publish sandbox-base container image
- Helm chart: per-agent pods, shared bucket, service accounts, ConfigMaps
- Agent auto-registration: agent_start event to S3
- Cross-agent query layer: GET /timeline, GET /correlation
- Cross-agent CLI: sandbox agents, sandbox timeline, sandbox correlate
- Documentation: image usage, Helm values, SA setup per provider

**Deliverable:** Deploy N agents via Helm, all to same bucket. Cross-agent queries work. Image published.

---

## Phase 5: Polish (Week 9-10)

**Read first:** `06-agent-controls.md`, `10-api-reference.md`

**Build:**
- WebSocket: ws://…/ws/approvals (bidirectional)
- Webhook notifications for pending approvals
- CLI polish: consistent output, error handling, help text
- Config validation on startup with clear errors
- Graceful shutdown: flush events, persist digest cache, upload to S3, checkpoint
- GET /health for K8s liveness/readiness probes
- Web UI for timeline (stretch)

**Deliverable:** Production-ready supervisor, clean CLI, webhook integration, graceful lifecycle.
