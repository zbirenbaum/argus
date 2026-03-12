# P1: Supervisor Binary (Startup Sequence)

**Status**: in progress

**Spec reference**: `docs/spec/01-supervisor.md` (startup sequence)

## Dependencies
- **Blocked by**: P1-config, P1-tracer-loop, P1-net-env
- **Blocks**: P4-container-image

## Final integration point for Phase 1. Not parallelizable with its direct dependencies.

## What needs to be done
- `crates/supervisor/src/main.rs`:
  1. Parse CLI args via clap: `supervisor --agent-id <id> [--config <path>] -- <command> [args...]`
  2. Initialize tracing subscriber (JSON format to stderr)
  3. Parse + validate config (merge CLI + config file)
  4. Create data directories (`/data/cas`, `/data/events`, `/data/indexes`)
  5. Generate TLS CA (if not exists)
  6. Start mitmdump child process (traced)
  7. Build agent env vars (TLS proxy settings)
  8. Create event channel (crossbeam or tokio mpsc)
  9. Spawn event writer thread (Phase 1: JSON lines to stdout)
  10. Fork child process:
      - Child: install seccomp filter → PTRACE_TRACEME → set agent env vars → exec agent command
      - Parent: set ptrace options → enter tracer loop
  11. Emit `AgentStart` event
  12. Run tracer loop until agent exits
  13. Clean up: stop mitmdump, flush events

- Signal handling: SIGTERM → graceful shutdown (pause agent, flush, exit)

## Deliverable
```bash
./supervisor --agent-id test -- /bin/bash -c "echo hello; cat /etc/hostname"
```
Outputs JSON event stream to stdout with: AgentStart, Exec, StdioData (stdout), file reads, Exit events — all with dual timestamps and agent_id.

## How to test
```bash
cargo test -p supervisor -- --ignored
```
Integration test: run supervisor with simple command, parse JSON output, verify event sequence and envelope fields.

## Branch
- **Branch**: `p1-supervisor-main`
- **Target**: `main`
