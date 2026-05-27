# Argus

ptrace-based filesystem and process sandbox for autonomous AI agents. Captures every file read, write, rename, and delete with content-addressable storage and process attribution — without the agent knowing it's being watched.

## Why

AI agents that write code, run commands, and modify filesystems need guardrails. Argus sits between the agent and the kernel, intercepting syscalls via seccomp-BPF + ptrace to build a complete, versioned audit trail of everything the agent touches. You get point-in-time restore, real-time event streaming, and the ability to pause an agent mid-syscall for human approval — all invisible to the agent process.

## Architecture

```
Container (SYS_PTRACE)
│
├── PID 1: Supervisor (Rust, static musl binary)
│   ├── seccomp-bpf filter (~55 syscalls trapped, rest native speed)
│   ├── ptrace loop (auto-follow fork/vfork/clone/exec)
│   ├── in-memory state (fd tables, pipe registry, Merkle tree, digest cache)
│   ├── event pipeline (classify → capture → stamp → redact → sink)
│   ├── content-addressable store (BLAKE3, local + S3)
│   ├── REST API + WebSocket (0.0.0.0:9090)
│   └── optional: mitmdump child (TLS MITM on :8080)
│
├── argus-api (query/control service, :8000)
│   ├── SQLite event store (ingests from supervisor's JSONL logs)
│   ├── SSE live event stream
│   ├── proxy to supervisor for pause/resume/restore
│   └── replay endpoint for historical events
│
├── Dashboard (Astro + SolidJS, :4321)
│   ├── live event viewer with filtering
│   ├── file tree browser with version history
│   └── network request viewer
│
└── Agent process (traced, all descendants auto-traced)
```

## What works

Validated on native aarch64 Linux (ARM64 container with full ptrace + seccomp-BPF):

| Validation Test | Status | What it proves |
|-----------------|--------|----------------|
| Process tracing | Pass | exec, fork, exit events with correct pid/ppid chains |
| Stdio capture | Pass | stdout/stderr separated with correct byte counts |
| File write/read/delete | Pass | write, read, unlink events with paths and BLAKE3 hashes |
| Pipe topology | Pass | pipe_create + pipe_data flow through shell pipelines (echo→grep→wc) |
| Subprocess tree | Pass | python3→ls with pipe_data flowing back to parent |
| Escape test | Pass | tool creation, exec, write attribution, unlink across processes |
| Write locking | Pass | 49 write events, unbroken hash chain across 3 concurrent writers |

408 unit tests pass across the workspace (391 argus + 17 supervisor).

### Not yet validated

- TLS/HTTPS capture (mitmdump integration wired but not end-to-end tested)
- Pause-before-action with ptrace enforcement (API layer works, syscall cancellation not wired)
- Full snapshot/restore cycle (restore endpoint works, checkpoint persistence not wired)
- Initial filesystem state scan
- Child reaping under PID 1

## Event types

Every syscall interception produces a typed JSON event with dual timestamps (monotonic + wall), agent ID, sequence number, and process attribution:

- `exec` — process started (path, args, env hash)
- `exit` — process exited (code, signal)
- `write` — file written (path, before/after BLAKE3 hash, byte count)
- `read` — file read (path, hash, byte count)
- `unlink` — file deleted
- `rename` — file moved (from, to)
- `stdio` — stdout/stderr data (subtype, byte count)
- `pipe_create` — pipe created (read/write fd pair)
- `pipe_data` — data through a pipe (direction, byte count)
- `socket` — network socket operation
- `connect` — outbound connection (addr, port)

## Quick start

Requires Docker with ARM64 support (OrbStack on macOS, or native Linux).

```bash
# Start the dev environment
docker compose up -d

# Build
docker exec argus-arm64 cargo build \
  --target aarch64-unknown-linux-musl \
  -p supervisor -p argus-api

# Run validation tests
docker exec argus-arm64 ./tests/validate.sh

# Trace a command
docker exec argus-arm64 \
  target/aarch64-unknown-linux-musl/debug/supervisor \
  --agent-id my-agent --config tests/test-config.yaml \
  -- bash -c 'echo hello > /tmp/test.txt && cat /tmp/test.txt'
```

### Run the full stack

```bash
# Start supervisor + API + dashboard
./scripts/run-demo.sh

# In another terminal, start the dashboard
cd dashboard && bun install && bun dev

# Services:
#   Supervisor API:  http://localhost:9090
#   Query API:       http://localhost:8000
#   Dashboard:       http://localhost:4321
```

## Project structure

```
crates/
├── argus/          # Library — all core logic
│   └── src/
│       ├── config/     # Configuration structs
│       ├── events/     # Event types + envelope
│       ├── state/      # fd table, pipes, process tree
│       ├── cas/        # Content-addressable store (BLAKE3)
│       ├── storage/    # Digest cache, event log, S3 upload pool
│       ├── tracer/     # ptrace loop, seccomp-BPF filter
│       ├── pipeline/   # Event processing pipeline
│       ├── snapshot/   # Merkle tree, checkpoint, restore
│       ├── index/      # Path/pid/type indexes, query engine
│       ├── net/        # TLS setup, MITM proxy, flow tracking
│       └── api/        # Axum HTTP server, WebSocket
├── supervisor/     # Binary — PID 1 entrypoint
├── argus-api/      # Binary — query + control service
└── cli/            # Binary — HTTP client for supervisor API

dashboard/          # Astro + SolidJS + Tailwind frontend
deploy/             # Dockerfiles + Kustomize manifests
tests/              # Validation test suite
```

## Design principles

- **Complete capture**: every file operation, with content and process attribution
- **Perfect versioning**: point-in-time restore to any fractional second
- **Real-time**: the agent waits for the log, never the other way around
- **Non-invasive**: the agent may be slowed, but never sees errors from the sandbox
- **Invisible**: the agent cannot detect it is being traced
- **Portable**: runs on managed Kubernetes (GKE, EKS, AKS) with no host-level access

## Tech stack

- **Language**: Rust (edition 2024), static musl binaries
- **Syscall interception**: seccomp-BPF + ptrace
- **Hashing**: BLAKE3 (content-addressable store)
- **Async runtime**: Tokio (API server, S3 uploads)
- **HTTP**: Axum
- **Storage**: Local CAS + S3-compatible backends
- **Dashboard**: Astro, SolidJS, Tailwind CSS v4
- **Deployment**: Docker, Kustomize

## License

[Business Source License 1.1](LICENSE) — free for non-production use. Production use permitted except offering Argus as a hosted/managed service. Converts to Apache 2.0 after four years.
