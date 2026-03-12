# Argus Run

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
| `07-tls-network.md` | SSLKEYLOGFILE, MITM proxy, network capture tiers, container image requirements |
| `08-kubernetes.md` | Capabilities, Yama, AppArmor, seccomp, pod structure, performance, invisibility |
| `09-multi-agent.md` | Container image, Helm chart, auto-registration, clock sync, cross-agent queries |
| `10-api-reference.md` | All REST endpoints, WebSocket, CLI commands |
| `11-implementation-phases.md` | Phase breakdown with file references per phase |


## Task Tracking

After completing any implementation work, update or create a task doc in `docs/tasks/`.
Name format: `docs/tasks/<phase>-<feature>.md` (e.g. `docs/tasks/p1-trace-loop.md`).

Keep task docs current. If a subsequent change affects a completed task, update that task doc in the same commit.
When using subagents make sure they get the full contents of this file.
Try to parallelize and organize tasks as much as possible to be executed by subagents without guesswork in worktrees.

Each task doc must contain:
- **Status**: not started | in progress | done
- **Spec reference**: which `docs/spec/` files this implements
- **What was done**: concrete list of files added/changed
- **What works**: which behaviors are implemented and tested
- **What's missing**: remaining TODOs, known gaps, stubbed functions
- **How to test**: exact commands to verify
- **Branch**: Branch(es) for the worktree. Use stacked PRs when applicable


A task is not complete until:
 - All tests pass
 - All TODOs are finalized
 - All stubbed functions are implemented
 - Coverage is as close to 100% as is feasibly possible
 - An agent running /code-review:code-review has given a full review
 - All code review feedback has been incorperated
 - All deviations from the spec have been recorded and signed off on by the human user you are assisting


**Important** If you are testing something and fail to get it right after the third attempt:
 - Immediately halt
 - Report the problem/failure
 - What you have attempted
 - Ask for guidance

## Rust Guidelines

**Always activate and tell subagents to activate the ms-rust-skill before doing any work in this repository**


## Environment

This project only builds on Linux (ptrace, seccomp, /proc). 
All cargo commands must run inside the dev container.
`cargo build` on macOS will fail — this is expected.


## Specs

- `docs/mvp.md` — MVP specification, event schema, storage architecture
- `docs/apis.md` — REST API, WebSocket, CLI interface contracts
- `docs/considerations.md` — Full design rationale and alternatives


## Conventions

- Target: `x86_64-unknown-linux-gnu`
- Rust edition 2024
- Errors: `anyhow` for app errors, `thiserror` for library types
- Async: `tokio` for API + S3 uploads; ptrace loop is synchronous on a dedicated thread
- Serde + serde_json for events and config
- CLI: `clap` derive API
- HTTP: `axum`
- Logging: `tracing` crate
- Tests: `#[test]` for unit, `#[test] #[ignore]` for integration requiring ptrace
- Files under 300 lines; extract a module if growing past that
- Functions under 40 lines
- Comments explain **why**, never **what**
- No commented-out code; delete it
- Ask for clarification or leave unimplemented with a comment if a subagent when uncertain

## Research

 - Use perplexity for involved queries which require deep consideration such as "Cutting edge techniques for graph optimization"
 - Use WebSearch for typical queries such as "Documentation"

## Git

- Commit messages: imperative mood, concise
- Do not include Co-Authored-By lines
- Docs update in the same commit as the code change


## Infrastructure

All deployment config is declarative and version-controlled (Kustomize manifests in `deploy/`). No manual kubectl, no ad-hoc scripts, no hardcoded values. If torn down and redeployed from what's committed, it must work.


## Organization

argus-run/
├── Cargo.toml
└── crates/
   ├── sandbox/              # library crate — all the logic
   │   ├── Cargo.toml
   │   └── src/
   │       ├── lib.rs
   │       ├── config/       # mod: all config structs
   │       ├── events/       # mod: event types + envelope
   │       ├── state/        # mod: fd table, pipes, pty, process tree, write locks
   │       ├── cas/          # mod: hasher, local store, chunker
   │       ├── storage/      # mod: digest cache, event log, S3, upload pool
   │       ├── tracer/       # mod: ptrace loop, seccomp, handlers
   │       ├── snapshot/     # mod: merkle tree, checkpoint, restore, diff
   │       ├── index/        # mod: path/pid/type indexes, query engine
   │       ├── net/          # mod: TLS setup, mitm proxy, connection tracker
   │       └── api/          # mod: axum server, routes, websocket
   │
   ├── supervisor/           # binary crate — PID 1 entrypoint
   │   ├── Cargo.toml        # depends on sandbox
   │   └── src/main.rs
   │
   └── cli/                  # binary crate — HTTP client to supervisor API
       ├── Cargo.toml        # depends on sandbox (for types only) + reqwest
       └── src/main.rs

