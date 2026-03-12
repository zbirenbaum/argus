# P1: Event Schema & Envelope

**Status**: not started

**Spec reference**: `docs/spec/02-event-schema.md`

## Dependencies
- **Blocked by**: nothing
- **Blocks**: P1-tracer-loop, P2-event-segments, P2-pause-resume-api, P3-indexes, P3-realtime-api

## Parallelizable with
- P1-config, P1-state, P1-seccomp, P1-net-env, P2-cas, P2-s3-upload, P2-digest-cache

## What needs to be done
- `crates/sandbox/src/events/mod.rs` — event types and envelope:
  - `EventEnvelope`: seq (u64, monotonic), ts_monotonic (f64, CLOCK_MONOTONIC_RAW), ts_wall (chrono DateTime<Utc>, RFC3339), agent_id (String), vclock (Option<HashMap<String, u64>>), event (Event enum)
  - `Event` enum with all variants from spec:
    - Process: `Exec { pid, ppid, binary, args, env, cwd }`, `Fork { parent_pid, child_pid }`, `Exit { pid, code, signal }`
    - File: `Read { pid, path, hash, size }`, `Write { pid, path, before_hash, after_hash, size }`, `Rename { pid, from, to }`, `Unlink { pid, path, hash }`, `Mkdir { pid, path, mode }`, `Chmod { pid, path, mode }`, `Truncate { pid, path, before_hash, after_hash }`, `Link { pid, from, to }`, `Symlink { pid, target, link_path }`
    - IO: `StdioData { pid, fd, stream (stdout/stderr/stdin), hash, size }`, `PipeData { pid, fd, pipe_id, hash, size }`, `PtyData { pid, fd, pty_id, hash, size }`
    - Network: `SocketCreate { pid, fd, domain, sock_type }`, `Connect { pid, fd, addr }`, `Accept { pid, fd, new_fd, peer_addr }`, `TlsKeys { pid, fd, hash }`, `HttpRequest { pid, method, url, status, req_hash, resp_hash }`
    - Control: `AgentStart { agent_id, command, config_hash }`, `Pause { reason }`, `Resume`, `PendingApproval { action_id, rule, syscall_info }`
    - System: `InitialState { path, hash, mode, size }`, `Checkpoint { seq, tree_hash }`, `MmapWarning { pid, path }`
  - `SequenceGenerator`: atomic u64 counter
  - `timestamp_pair()` fn returning (ts_monotonic, ts_wall)
  - Serde serialization: snake_case, tagged enum
  - `EventEnvelope::new(agent_id, event) -> Self` auto-fills seq, timestamps

## How to test
```bash
cargo test -p sandbox --lib events
```
Unit tests: envelope creation with auto-incrementing seq, serialization round-trip for every event variant, timestamp monotonicity across calls.

## Branch
- **Branch**: `p1-events`
- **Target**: `main`
