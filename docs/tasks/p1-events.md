# P1: Event Schema & Envelope

**Status**: done

**Spec reference**: `docs/spec/02-event-schema.md`

## Dependencies
- **Blocked by**: nothing
- **Blocks**: P1-tracer-loop, P2-event-segments, P2-pause-resume-api, P3-indexes, P3-realtime-api

## Parallelizable with
- P1-config, P1-state, P1-seccomp, P1-net-env, P2-cas, P2-s3-upload, P2-digest-cache

## What was done
- `crates/sandbox/src/events/mod.rs` — module root with re-exports
- `crates/sandbox/src/events/envelope.rs` — `Event` struct, `EventPayload` tagged enum (35 variants), `SequenceGenerator` (AtomicU64), `timestamp_pair()` fn
- `crates/sandbox/src/events/process.rs` — `Exec`, `Fork`, `Exit`
- `crates/sandbox/src/events/file.rs` — `Read`, `Write`, `Rename`, `Unlink`, `Mkdir`, `Rmdir`, `Chmod`, `Truncate`, `Link`, `Symlink`
- `crates/sandbox/src/events/io.rs` — `Stdio`, `PipeCreate`, `PipeData`, `PipeClose`, `PtyCreate`, `PtyData`, `FdRedirect`, `FdTarget`, plus `StdioSubtype`, `PipeDirection`, `PtySubtype` enums
- `crates/sandbox/src/events/network.rs` — `Socket`, `Connect`, `Accept`, `TlsKeys`, `HttpRequest`, `HttpResponse`
- `crates/sandbox/src/events/control.rs` — `AgentStart`, `AgentPause`, `AgentResume`, `PendingApproval`, `ApprovalGranted`, `ApprovalDenied`
- `crates/sandbox/src/events/snapshot.rs` — `InitialState`, `Checkpoint`, `MmapWarning`

## What works
- All 35 event variants serialize/deserialize correctly with serde tagged enum (`"type": "variant_name"`)
- `SequenceGenerator` produces thread-safe monotonic sequence numbers via `AtomicU64`
- `timestamp_pair()` returns `(ts_monotonic_nanos, ts_wall_rfc3339)` with monotonic guarantees
- `Event::new()` auto-fills seq, both timestamps, and sets vclock to None
- Optional hash/tree_hash fields omitted from JSON when None
- vclock field omitted when None, serialized as map when Some
- 39 tests pass covering round-trips for all variants, seq increments, timestamp monotonicity, JSON format validation

## Spec deviations
- `AgentStart.agent_id` serializes as `start_agent_id` to avoid collision with the envelope's flattened `agent_id` field
- `Checkpoint.seq` serializes as `checkpoint_seq` to avoid collision with the envelope's flattened `seq` field

## What's missing
- Nothing — all event types from spec implemented, all tests pass, clippy clean

## How to test
```bash
cargo test -p sandbox --lib events
```

## Branch
- **Branch**: `p1-events`
- **Target**: `main`
