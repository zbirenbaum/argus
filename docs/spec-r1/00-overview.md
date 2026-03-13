# Argus Run — Design Specification

ptrace-based filesystem versioning sandbox for autonomous AI agents.

## Requirements

- **Complete capture:** Every file read, write, rename, delete — with content and process attribution
- **Perfect versioning:** Point-in-time restore to any fractional second
- **Real-time:** Slowing the agent is acceptable; letting it get ahead of the log is not
- **Non-invasive:** Making the agent wait is fine; causing errors is not
- **Invisible:** Agent cannot detect it is being sandboxed
- **Portable:** Managed Kubernetes (GKE, EKS, AKS) with no host access
- **General-purpose:** OpenClaw, research agents, fine-tuning pipelines, future frameworks

## Architecture

```
Container (SYS_PTRACE)
│
├── PID 1: Supervisor (Rust, static binary)
│   ├── seccomp-bpf filter (~55 syscalls trapped, rest native speed)
│   ├── ptrace loop (auto-follow fork/vfork/clone/exec)
│   ├── in-memory state (fd tables, pipe registry, PTY registry, Merkle tree, digest cache, write locks)
│   ├── local buffer (/data/ — CAS, events, indexes)
│   ├── async S3 upload pool (tokio, aws-sdk-s3)
│   ├── REST API (127.0.0.1:9090)
│   └── mitmdump child process (TLS MITM, 127.0.0.1:8080)
│
├── Agent process (traced, all descendants auto-traced)
│
└── Volumes:
    ├── /data — local buffer, digest cache, indexes
    └── /workspace — agent working directory

External:
└── S3/GCS bucket (source of truth)
    ├── cas/{hash[0:2]}/{hash[2:]}
    ├── events/{agent_id}/{segment}.jsonl
    ├── checkpoints/{agent_id}/{seq}.bin
    └── meta/{agent_id}/digest-cache-latest.bin
```

## Documents

| File | Contents |
|------|----------|
| `01-supervisor.md` | ptrace loop, syscall interception, fd/pipe/PTY tracking, write locking, startup sequence |
| `02-event-schema.md` | Event envelope, dual timestamps, agent_id, vclock, all event types |
| `03-storage.md` | CAS, event log, digest cache, S3 integration, durability modes, local buffer |
| `04-snapshots-restore.md` | Merkle tree, checkpoints, full/selective/in-place restore, undo |
| `05-indexing-queries.md` | Secondary indexes, query engine, stdio reconstruction, pipeline data flow |
| `06-agent-controls.md` | Pause/resume, pause-before-action rules, approval API |
| `07-tls-network.md` | SSLKEYLOGFILE, MITM proxy, network capture tiers, database protocol capture |
| `08-kubernetes.md` | Capabilities, Yama, AppArmor, seccomp, pod structure, performance, invisibility |
| `09-multi-agent.md` | Container image, Helm chart, auto-registration, clock sync, cross-agent queries |
| `10-api-reference.md` | All REST endpoints, WebSocket, CLI commands |
| `11-implementation-phases.md` | Phase breakdown with file references per phase |
| `12-testing.md` | Validation test plan, devcontainer setup, integration test, bug indicators |
| `13-pipeline-migration.md` | RecordBus/Sink refactor — one-shot migration from broken data flow to streaming pipeline |
