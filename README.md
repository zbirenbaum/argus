# Argus

A sandbox for AI agents. Argus intercepts every syscall an agent makes — file reads, writes, deletes, network connections, subprocesses — and builds a versioned, content-addressed audit trail. The agent can't tell it's being watched.

It works by running as PID 1 in a container, using seccomp-BPF to trap ~55 syscalls and ptrace to inspect them. Everything else runs at native speed. The supervisor automatically follows forks, clones, and execs, so the entire process tree is traced without configuration.

You get a structured event stream of everything the agent did, BLAKE3 hashes of every file version, and the ability to restore the workspace to any point in time.

## Architecture

```
Container (SYS_PTRACE)
│
├── PID 1: Supervisor
│   ├── seccomp-bpf (~55 syscalls trapped, rest native)
│   ├── ptrace loop (auto-follows fork/clone/exec)
│   ├── event pipeline (classify → capture → stamp → redact → sink)
│   ├── content-addressable store (BLAKE3, local + S3)
│   ├── REST API + WebSocket (:9090)
│   └── optional: mitmdump for TLS interception (:8080)
│
├── argus-api — query + control service (:8000)
│   ├── SQLite event store
│   ├── SSE live stream
│   └── proxy for pause/resume/restore
│
├── Dashboard (:4321)
│   ├── live event viewer
│   ├── file tree with version history
│   └── network request viewer
│
└── Agent process (traced, all descendants auto-traced)
```

### Not yet implemented:
 - Pause-before-action (API works but ptrace enforcement not yet connected)
 - Initial filesystem state scan

## Events

Every intercepted syscall produces a JSON event with timestamps, agent ID, sequence number, and process attribution:

`exec` `exit` `write` `read` `unlink` `rename` `stdio` `pipe_create` `pipe_data` `socket` `connect`

Writes include before/after BLAKE3 hashes. Stdio is split by stdout/stderr. Pipes track the full fd topology.

## Quick start

Needs Docker with ARM64 support (OrbStack on macOS, or native Linux).

```bash
docker compose up -d

docker exec argus-arm64 cargo build \
  --target aarch64-unknown-linux-musl \
  -p supervisor -p argus-api

# Run the validation suite
docker exec argus-arm64 ./tests/validate.sh

# Trace a command
docker exec argus-arm64 \
  target/aarch64-unknown-linux-musl/debug/supervisor \
  --agent-id my-agent --config tests/test-config.yaml \
  -- bash -c 'echo hello > /tmp/test.txt && cat /tmp/test.txt'
```

### Full stack

```bash
./scripts/run-demo.sh

# Dashboard (separate terminal)
cd dashboard && bun install && bun dev
```

Supervisor at localhost:9090, query API at localhost:8000, dashboard at localhost:4321.

## Project layout

```
crates/
├── argus/          # Core library
├── supervisor/     # PID 1 binary
├── argus-api/      # Query + control service
└── cli/            # CLI client

dashboard/          # Web frontend
deploy/             # Dockerfiles + Kustomize
tests/              # Validation suite
```

## License

[BSL 1.1](LICENSE) — use it however you want, except running it as a hosted service. Converts to Apache 2.0 after four years.
